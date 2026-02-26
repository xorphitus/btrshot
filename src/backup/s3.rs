use anyhow::Context;
use tracing::{info, warn};

use crate::cmd;
use crate::config::Config;
use crate::state::{Operation, State};

/// The compiled pattern for valid snapshot names: `(full|incr)_YYYYMMDD_HHMMSS`.
///
/// Validated at function entry in [`run_s3_upload`] to prevent path traversal
/// through `tar` arguments.
fn is_valid_snapshot_name(name: &str) -> bool {
    // Accept only the exact pattern produced by `snapshot.rs`:
    // (full|incr)_YYYYMMDD_HHMMSS  →  e.g. full_20240101_120000
    let bytes = name.as_bytes();
    // Minimum length: "full_YYYYMMDD_HHMMSS" = 20 chars, "incr_..." = 20 chars
    if bytes.len() != 20 {
        return false;
    }
    let (prefix, rest) = name.split_once('_').unwrap_or(("", ""));
    if prefix != "full" && prefix != "incr" {
        return false;
    }
    // rest must be "YYYYMMDD_HHMMSS" — 15 chars: 8 digits, underscore, 6 digits
    if rest.len() != 15 {
        return false;
    }
    let date_part = &rest[..8];
    let sep = rest.as_bytes().get(8).copied();
    let time_part = &rest[9..];
    sep == Some(b'_')
        && date_part.bytes().all(|b| b.is_ascii_digit())
        && time_part.bytes().all(|b| b.is_ascii_digit())
}

/// Returns `true` if `name` is a valid S3 object name produced by btrshot,
/// i.e. matches `(full|incr)_YYYYMMDD_HHMMSS.tar.gpg`.
fn is_valid_s3_object_name(name: &str) -> bool {
    name.strip_suffix(".tar.gpg")
        .map(is_valid_snapshot_name)
        .unwrap_or(false)
}

/// Parses the output of `aws s3 ls s3://<bucket>/` and returns the list of
/// object keys found in that output.
///
/// Each line of `aws s3 ls` has the format:
/// ```text
/// 2024-01-01 12:00:00      12345 full_20240101_120000.tar.gpg
/// ```
/// This function extracts the fourth whitespace-separated token (the object
/// name) from each non-empty line. Lines that do not contain at least four
/// tokens are silently ignored (e.g. blank lines, header lines).
fn parse_s3_ls_output(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let mut tokens = line.split_whitespace();
            // Skip: date, time, size — the fourth token is the object name.
            tokens.next()?; // date
            tokens.next()?; // time
            tokens.next()?; // size
            tokens.next().map(str::to_owned)
        })
        .collect()
}

/// Given a list of S3 object names (e.g. `full_20240101_120000.tar.gpg`),
/// returns the subset that should be deleted to enforce `retention_count`.
///
/// Objects are sorted lexicographically by name (ascending); because the
/// timestamp is embedded in the prefix (`YYYYMMDD_HHMMSS`), a lexicographic
/// sort equals a chronological sort. The *oldest* objects (lowest sort keys)
/// beyond `retention_count` are returned as candidates for deletion.
///
/// If `objects.len() <= retention_count`, the returned list is empty.
fn objects_to_delete(objects: &[String], retention_count: usize) -> Vec<String> {
    if objects.len() <= retention_count {
        return Vec::new();
    }

    let mut sorted = objects.to_vec();
    sorted.sort_unstable();

    let delete_count = sorted.len() - retention_count;
    sorted.into_iter().take(delete_count).collect()
}

/// Builds the list of environment variables to inject into `aws` subprocesses.
///
/// When `aws_profile` is `Some`, `AWS_PROFILE` is included so that the correct
/// credential profile is used regardless of the inherited environment.
fn aws_env_vars(aws_profile: &Option<String>) -> Vec<(String, String)> {
    match aws_profile {
        Some(profile) => vec![("AWS_PROFILE".to_owned(), profile.clone())],
        None => Vec::new(),
    }
}

/// Uploads a snapshot to S3 and enforces the retention policy.
///
/// Steps:
/// 1. Validates `snapshot_name` against the expected pattern.
/// 2. Verifies the GPG public key file exists.
/// 3. Writes state `in_progress:s3_upload`.
/// 4. Streams the snapshot via `tar | gpg | aws s3 cp`.
/// 5. Lists current S3 objects, sorts by embedded timestamp, and deletes the
///    oldest objects that exceed `retention_count`.
/// 6. Writes state `idle`.
pub fn run_s3_upload(config: &Config, snapshot_name: &str) -> anyhow::Result<()> {
    // Validate snapshot_name to prevent path traversal.
    if !is_valid_snapshot_name(snapshot_name) {
        anyhow::bail!(
            "invalid snapshot name {:?}: must match (full|incr)_YYYYMMDD_HHMMSS",
            snapshot_name
        );
    }

    // Verify the GPG public key file exists before launching the pipeline.
    let public_key_file = &config.gpg.public_key_file;
    if !public_key_file.exists() || !public_key_file.is_file() {
        anyhow::bail!(
            "GPG public key file does not exist or is not a regular file: {}",
            public_key_file.display()
        );
    }

    info!("Starting S3 upload for snapshot: {}", snapshot_name);

    let state_dir = &config.state.state_dir;
    let backup_path = &config.paths.backup_path;
    let bucket = &config.s3.bucket;
    let retention_count = config.s3.retention_count;

    // Build env vars for aws subprocesses.
    let env_pairs = aws_env_vars(&config.s3.aws_profile);
    let env_refs: Vec<(&str, &str)> = env_pairs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    // Step 1: write state in_progress:s3_upload
    State::InProgress(Operation::S3Upload)
        .write(state_dir)
        .context("failed to write in_progress:s3_upload state")?;

    // Step 2: stream-upload the snapshot via tar | gpg | aws s3 cp
    let snapshots_dir = backup_path.join("snapshots");
    let snapshots_dir_str = snapshots_dir.to_string_lossy().into_owned();
    let snapshot_dir_arg = format!("{}/", snapshot_name);
    let public_key_str = public_key_file.to_string_lossy().into_owned();
    let s3_dest = format!("s3://{}/{}.tar.gpg", bucket, snapshot_name);

    cmd::pipe_with_env(
        &[
            (
                "tar",
                &["-cf", "-", "-C", &snapshots_dir_str, &snapshot_dir_arg] as &[&str],
            ),
            (
                "gpg",
                &[
                    "--encrypt",
                    "--recipient-file",
                    &public_key_str,
                    "--output",
                    "-",
                ],
            ),
            ("aws", &["s3", "cp", "-", &s3_dest]),
        ],
        &env_refs,
    )
    .context("failed to stream-upload snapshot to S3")?;

    // Step 3: enforce S3 retention
    let s3_prefix = format!("s3://{}/", bucket);
    let ls_output = cmd::run_with_output_env("aws", &["s3", "ls", &s3_prefix], &env_refs)
        .context("failed to list S3 objects")?;
    let all_objects = parse_s3_ls_output(&ls_output);

    // Filter out any object names that don't match the expected pattern.
    let objects: Vec<String> = all_objects
        .into_iter()
        .filter(|name| {
            if is_valid_s3_object_name(name) {
                true
            } else {
                warn!(
                    object = %name,
                    "skipping S3 object with unexpected name during retention enforcement"
                );
                false
            }
        })
        .collect();

    let to_delete = objects_to_delete(&objects, retention_count);

    for object in &to_delete {
        let s3_uri = format!("s3://{}/{}", bucket, object);
        cmd::run_with_env("aws", &["s3", "rm", &s3_uri], &env_refs)
            .with_context(|| format!("failed to delete S3 object: {}", s3_uri))?;
    }

    if !to_delete.is_empty() {
        info!(
            "Deleted {} old S3 object(s) to enforce retention count of {}",
            to_delete.len(),
            retention_count
        );
    }

    // Step 4: write state idle
    State::Idle
        .write(state_dir)
        .context("failed to write idle state")?;

    info!("S3 upload complete for snapshot: {}", snapshot_name);
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// A process-wide mutex that must be held whenever a test mutates the `PATH`
    /// environment variable. Rust test threads share the same process, so any
    /// test that calls `std::env::set_var("PATH", …)` must hold this lock for
    /// the duration of the mutation to prevent races with other tests that spawn
    /// subprocesses and depend on the current `PATH`.
    static PATH_MUTEX: Mutex<()> = Mutex::new(());

    // -----------------------------------------------------------------------
    // parse_s3_ls_output — pure function, fully deterministic
    // -----------------------------------------------------------------------

    /// Standard `aws s3 ls` output produces the correct object names.
    #[test]
    fn test_parse_s3_ls_output_standard_lines() {
        let output = "\
2024-01-01 12:00:00      12345 full_20240101_120000.tar.gpg\n\
2024-01-02 08:00:00      12345 incr_20240102_080000.tar.gpg\n\
2024-01-03 09:00:00      12345 full_20240103_090000.tar.gpg\n";

        let names = parse_s3_ls_output(output);
        assert_eq!(
            names,
            vec![
                "full_20240101_120000.tar.gpg",
                "incr_20240102_080000.tar.gpg",
                "full_20240103_090000.tar.gpg",
            ]
        );
    }

    /// Empty output produces an empty list.
    #[test]
    fn test_parse_s3_ls_output_empty_string() {
        let names = parse_s3_ls_output("");
        assert!(names.is_empty(), "empty input should yield empty list");
    }

    /// Lines with fewer than four tokens (e.g. blank lines) are skipped.
    #[test]
    fn test_parse_s3_ls_output_skips_short_lines() {
        let output = "\n\n2024-01-01 12:00:00      12345 full_20240101_120000.tar.gpg\n\n";
        let names = parse_s3_ls_output(output);
        assert_eq!(names, vec!["full_20240101_120000.tar.gpg"]);
    }

    /// A single object line is parsed correctly.
    #[test]
    fn test_parse_s3_ls_output_single_line() {
        let output = "2024-01-01 12:00:00      99999 full_20240101_120000.tar.gpg\n";
        let names = parse_s3_ls_output(output);
        assert_eq!(names, vec!["full_20240101_120000.tar.gpg"]);
    }

    /// Extra whitespace between tokens is handled correctly (split_whitespace).
    #[test]
    fn test_parse_s3_ls_output_extra_whitespace() {
        let output = "2024-01-01  12:00:00   99999  full_20240101_120000.tar.gpg\n";
        let names = parse_s3_ls_output(output);
        assert_eq!(names, vec!["full_20240101_120000.tar.gpg"]);
    }

    // -----------------------------------------------------------------------
    // objects_to_delete — pure function, fully deterministic
    // -----------------------------------------------------------------------

    /// When the object count is within retention, nothing is deleted.
    #[test]
    fn test_objects_to_delete_within_retention_returns_empty() {
        let objects: Vec<String> = vec![
            "full_20240101_120000.tar.gpg".to_owned(),
            "incr_20240102_080000.tar.gpg".to_owned(),
        ];
        let to_delete = objects_to_delete(&objects, 10);
        assert!(
            to_delete.is_empty(),
            "within retention: nothing should be deleted"
        );
    }

    /// When the object count exactly equals retention, nothing is deleted.
    #[test]
    fn test_objects_to_delete_exactly_at_retention_returns_empty() {
        let objects: Vec<String> = (0..3)
            .map(|i| format!("full_2024010{}_120000.tar.gpg", i + 1))
            .collect();
        let to_delete = objects_to_delete(&objects, 3);
        assert!(
            to_delete.is_empty(),
            "at exact retention count: nothing should be deleted"
        );
    }

    /// The oldest object (lowest sort key) is returned when one exceeds retention.
    #[test]
    fn test_objects_to_delete_one_excess_returns_oldest() {
        let objects: Vec<String> = vec![
            "full_20240103_090000.tar.gpg".to_owned(),
            "full_20240101_120000.tar.gpg".to_owned(),
            "incr_20240102_080000.tar.gpg".to_owned(),
        ];
        let to_delete = objects_to_delete(&objects, 2);
        assert_eq!(to_delete.len(), 1);
        assert_eq!(to_delete[0], "full_20240101_120000.tar.gpg");
    }

    /// Multiple excess objects are returned in oldest-first order.
    #[test]
    fn test_objects_to_delete_multiple_excess() {
        let objects: Vec<String> = vec![
            "full_20240104_120000.tar.gpg".to_owned(),
            "full_20240101_120000.tar.gpg".to_owned(),
            "full_20240102_120000.tar.gpg".to_owned(),
            "full_20240103_120000.tar.gpg".to_owned(),
        ];
        let to_delete = objects_to_delete(&objects, 2);
        assert_eq!(to_delete.len(), 2);
        assert_eq!(to_delete[0], "full_20240101_120000.tar.gpg");
        assert_eq!(to_delete[1], "full_20240102_120000.tar.gpg");
    }

    /// Zero retention count returns all objects.
    #[test]
    fn test_objects_to_delete_zero_retention_returns_all() {
        let objects: Vec<String> = vec![
            "full_20240102_120000.tar.gpg".to_owned(),
            "full_20240101_120000.tar.gpg".to_owned(),
        ];
        let mut to_delete = objects_to_delete(&objects, 0);
        to_delete.sort_unstable();
        assert_eq!(
            to_delete,
            vec![
                "full_20240101_120000.tar.gpg",
                "full_20240102_120000.tar.gpg",
            ]
        );
    }

    /// Empty object list with any retention returns empty.
    #[test]
    fn test_objects_to_delete_empty_list() {
        let to_delete = objects_to_delete(&[], 5);
        assert!(
            to_delete.is_empty(),
            "empty list should yield empty delete list"
        );
    }

    /// Mixed full and incr objects are sorted correctly (lexicographic = chronological
    /// because both prefixes start with the same timestamp format).
    #[test]
    fn test_objects_to_delete_mixed_full_and_incr() {
        let objects: Vec<String> = vec![
            "incr_20240103_090000.tar.gpg".to_owned(),
            "full_20240101_120000.tar.gpg".to_owned(),
            "incr_20240102_080000.tar.gpg".to_owned(),
        ];
        let to_delete = objects_to_delete(&objects, 2);
        assert_eq!(to_delete.len(), 1);
        // "full_20240101_..." sorts before "incr_20240102_..." and "incr_20240103_..."
        assert_eq!(to_delete[0], "full_20240101_120000.tar.gpg");
    }

    // -----------------------------------------------------------------------
    // run_s3_upload — error propagation without aws/gpg/tar
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // is_valid_snapshot_name — pure function
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_valid_snapshot_name_valid_full() {
        assert!(is_valid_snapshot_name("full_20240101_120000"));
    }

    #[test]
    fn test_is_valid_snapshot_name_valid_incr() {
        assert!(is_valid_snapshot_name("incr_20241231_235959"));
    }

    #[test]
    fn test_is_valid_snapshot_name_wrong_prefix() {
        assert!(!is_valid_snapshot_name("snap_20240101_120000"));
    }

    #[test]
    fn test_is_valid_snapshot_name_path_traversal() {
        assert!(!is_valid_snapshot_name("../etc/passwd"));
        assert!(!is_valid_snapshot_name("full_20240101_120000/../../etc"));
    }

    #[test]
    fn test_is_valid_snapshot_name_with_tar_gpg_suffix_is_invalid() {
        // snapshot_name should NOT include the .tar.gpg suffix
        assert!(!is_valid_snapshot_name("full_20240101_120000.tar.gpg"));
    }

    #[test]
    fn test_is_valid_snapshot_name_empty_string() {
        assert!(!is_valid_snapshot_name(""));
    }

    #[test]
    fn test_is_valid_snapshot_name_non_digit_date() {
        assert!(!is_valid_snapshot_name("full_2024010x_120000"));
    }

    // -----------------------------------------------------------------------
    // is_valid_s3_object_name — pure function
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_valid_s3_object_name_valid() {
        assert!(is_valid_s3_object_name("full_20240101_120000.tar.gpg"));
        assert!(is_valid_s3_object_name("incr_20241231_235959.tar.gpg"));
    }

    #[test]
    fn test_is_valid_s3_object_name_missing_suffix() {
        assert!(!is_valid_s3_object_name("full_20240101_120000"));
    }

    #[test]
    fn test_is_valid_s3_object_name_unexpected_name() {
        assert!(!is_valid_s3_object_name("README.md"));
        assert!(!is_valid_s3_object_name("../../etc/passwd.tar.gpg"));
    }

    // -----------------------------------------------------------------------
    // aws_env_vars — pure function
    // -----------------------------------------------------------------------

    #[test]
    fn test_aws_env_vars_none_returns_empty() {
        let vars = aws_env_vars(&None);
        assert!(vars.is_empty());
    }

    #[test]
    fn test_aws_env_vars_some_returns_aws_profile() {
        let vars = aws_env_vars(&Some("my-profile".to_owned()));
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].0, "AWS_PROFILE");
        assert_eq!(vars[0].1, "my-profile");
    }

    // -----------------------------------------------------------------------
    // run_s3_upload — validation errors (before state write)
    // -----------------------------------------------------------------------

    /// `run_s3_upload` should reject an invalid snapshot name before anything else.
    #[test]
    fn test_run_s3_upload_rejects_invalid_snapshot_name() {
        use crate::config::{GpgConfig, PathsConfig, S3Config, ScheduleConfig, StateConfig};

        let config = Config {
            paths: PathsConfig {
                source_path: std::path::PathBuf::from("/nonexistent/source"),
                source_subvolume: "data".to_owned(),
                backup_path: std::path::PathBuf::from("/nonexistent/backup"),
            },
            s3: S3Config {
                bucket: "test-bucket".to_owned(),
                retention_count: 10,
                aws_profile: None,
            },
            gpg: GpgConfig {
                public_key_file: std::path::PathBuf::from("/etc/key.pub"),
            },
            schedule: ScheduleConfig {
                check_interval: 7200,
                full_backup_interval: 604_800,
                incremental_interval: 86400,
            },
            state: StateConfig {
                state_dir: std::path::PathBuf::from("/nonexistent/state"),
            },
        };

        let result = run_s3_upload(&config, "../../etc/passwd");
        assert!(result.is_err(), "should reject invalid snapshot name");
        let msg = format!("{:#}", result.expect_err("already Err"));
        assert!(
            msg.contains("invalid snapshot name"),
            "error should mention invalid snapshot name; got: {msg}"
        );
    }

    /// `run_s3_upload` should reject a missing GPG key file before writing state.
    #[test]
    fn test_run_s3_upload_rejects_missing_gpg_key() {
        use crate::config::{GpgConfig, PathsConfig, S3Config, ScheduleConfig, StateConfig};

        let state_dir = TempDir::new().expect("temp dir");

        let config = Config {
            paths: PathsConfig {
                source_path: std::path::PathBuf::from("/nonexistent/source"),
                source_subvolume: "data".to_owned(),
                backup_path: std::path::PathBuf::from("/nonexistent/backup"),
            },
            s3: S3Config {
                bucket: "test-bucket".to_owned(),
                retention_count: 10,
                aws_profile: None,
            },
            gpg: GpgConfig {
                // This file does not exist.
                public_key_file: std::path::PathBuf::from("/nonexistent/key.pub"),
            },
            schedule: ScheduleConfig {
                check_interval: 7200,
                full_backup_interval: 604_800,
                incremental_interval: 86400,
            },
            state: StateConfig {
                state_dir: state_dir.path().to_path_buf(),
            },
        };

        let result = run_s3_upload(&config, "full_20240101_120000");
        assert!(result.is_err(), "should reject missing GPG key file");
        let msg = format!("{:#}", result.expect_err("already Err"));
        assert!(
            msg.contains("GPG") || msg.contains("gpg") || msg.contains("key"),
            "error should mention GPG/key; got: {msg}"
        );

        // State should NOT have been written (validation happens before state write).
        let state =
            crate::state::State::read(state_dir.path()).expect("state file should be readable");
        assert_eq!(
            state,
            crate::state::State::Idle,
            "state should remain Idle when validation fails before state write"
        );
    }

    /// `run_s3_upload` should return `Err` immediately when the state directory
    /// does not exist (the very first state write fails).
    #[test]
    fn test_run_s3_upload_fails_on_nonexistent_state_dir() {
        use crate::config::{GpgConfig, PathsConfig, S3Config, ScheduleConfig, StateConfig};
        use tempfile::NamedTempFile;

        // Create a real key file so we get past GPG validation.
        let key_file = NamedTempFile::new().expect("temp key file");

        let config = Config {
            paths: PathsConfig {
                source_path: std::path::PathBuf::from("/nonexistent/source"),
                source_subvolume: "data".to_owned(),
                backup_path: std::path::PathBuf::from("/nonexistent/backup"),
            },
            s3: S3Config {
                bucket: "test-bucket".to_owned(),
                retention_count: 10,
                aws_profile: None,
            },
            gpg: GpgConfig {
                public_key_file: key_file.path().to_path_buf(),
            },
            schedule: ScheduleConfig {
                check_interval: 7200,
                full_backup_interval: 604_800,
                incremental_interval: 86400,
            },
            state: StateConfig {
                state_dir: std::path::PathBuf::from("/nonexistent/state"),
            },
        };

        let result = run_s3_upload(&config, "full_20240101_120000");
        assert!(
            result.is_err(),
            "run_s3_upload should fail when state_dir does not exist"
        );
    }

    /// After a successful state write but a failed pipeline (tar/gpg/aws not
    /// available or backup_path missing), the state file should be
    /// `in_progress:s3_upload` — proving the state was written before the
    /// pipeline ran.
    #[test]
    fn test_run_s3_upload_writes_state_before_pipeline() {
        use crate::config::{GpgConfig, PathsConfig, S3Config, ScheduleConfig, StateConfig};
        use crate::state::State;
        use tempfile::NamedTempFile;

        let state_dir = TempDir::new().expect("temp dir");
        // Create a real key file so we get past GPG validation.
        let key_file = NamedTempFile::new().expect("temp key file");

        let config = Config {
            paths: PathsConfig {
                source_path: std::path::PathBuf::from("/nonexistent/source"),
                source_subvolume: "data".to_owned(),
                backup_path: std::path::PathBuf::from("/nonexistent/backup"),
            },
            s3: S3Config {
                bucket: "test-bucket".to_owned(),
                retention_count: 10,
                aws_profile: None,
            },
            gpg: GpgConfig {
                public_key_file: key_file.path().to_path_buf(),
            },
            schedule: ScheduleConfig {
                check_interval: 7200,
                full_backup_interval: 604_800,
                incremental_interval: 86400,
            },
            state: StateConfig {
                state_dir: state_dir.path().to_path_buf(),
            },
        };

        // Hold the PATH mutex to ensure another test is not mid-mutation of PATH
        // while this test spawns `tar` (which must be the real binary to fail).
        let _guard = PATH_MUTEX.lock().expect("PATH_MUTEX poisoned");

        // The pipeline (tar | gpg | aws) will fail because paths don't exist.
        let result = run_s3_upload(&config, "full_20240101_120000");

        drop(_guard);

        assert!(
            result.is_err(),
            "run_s3_upload should fail when pipeline cannot execute"
        );

        // The state should be in_progress:s3_upload (written before the pipeline).
        let state = State::read(state_dir.path()).expect("state should be readable");
        assert_eq!(
            state,
            State::InProgress(Operation::S3Upload),
            "state should be in_progress:s3_upload after the first step"
        );
    }

    /// Integration test: use PATH-substituted fake binaries to drive a full
    /// successful `run_s3_upload` and verify that `State::Idle` is written and
    /// that `aws s3 rm` is invoked for objects beyond the retention limit.
    #[test]
    fn test_run_s3_upload_writes_idle_state_on_success() {
        use crate::config::{GpgConfig, PathsConfig, S3Config, ScheduleConfig, StateConfig};
        use crate::state::State;
        use std::os::unix::fs::PermissionsExt as _;
        use tempfile::NamedTempFile;

        let state_dir = TempDir::new().expect("state temp dir");
        let fake_bin_dir = TempDir::new().expect("fake bin temp dir");
        let key_file = NamedTempFile::new().expect("temp key file");

        // Create a fake `tar` that just exits 0, producing no output.
        let fake_tar = fake_bin_dir.path().join("tar");
        std::fs::write(&fake_tar, "#!/bin/sh\nexit 0\n").expect("write fake tar");
        std::fs::set_permissions(&fake_tar, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake tar");

        // Create a fake `gpg` that just exits 0.
        let fake_gpg = fake_bin_dir.path().join("gpg");
        std::fs::write(&fake_gpg, "#!/bin/sh\nexit 0\n").expect("write fake gpg");
        std::fs::set_permissions(&fake_gpg, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake gpg");

        // Create a fake `aws` that:
        //   - For `aws s3 cp ...`: exits 0.
        //   - For `aws s3 ls ...`: prints two objects (retention_count=1 → one deleted).
        //   - For `aws s3 rm ...`: exits 0 and logs to a file.
        let rm_log = fake_bin_dir.path().join("rm_calls.txt");
        let rm_log_str = rm_log.to_string_lossy().into_owned();
        let aws_script = format!(
            "#!/bin/sh\ncase \"$2\" in\n  cp) exit 0;;\n  ls) echo '2024-01-01 12:00:00 1 full_20240101_120000.tar.gpg'; echo '2024-01-02 12:00:00 1 full_20240102_120000.tar.gpg'; exit 0;;\n  rm) echo \"$3\" >> {rm_log_str}; exit 0;;\nesac\nexit 1\n",
        );
        let fake_aws = fake_bin_dir.path().join("aws");
        std::fs::write(&fake_aws, &aws_script).expect("write fake aws");
        std::fs::set_permissions(&fake_aws, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake aws");

        let config = Config {
            paths: PathsConfig {
                source_path: std::path::PathBuf::from("/nonexistent/source"),
                source_subvolume: "data".to_owned(),
                backup_path: std::path::PathBuf::from("/nonexistent/backup"),
            },
            s3: S3Config {
                bucket: "test-bucket".to_owned(),
                retention_count: 1, // keep 1, delete 1 oldest
                aws_profile: None,
            },
            gpg: GpgConfig {
                public_key_file: key_file.path().to_path_buf(),
            },
            schedule: ScheduleConfig {
                check_interval: 7200,
                full_backup_interval: 604_800,
                incremental_interval: 86400,
            },
            state: StateConfig {
                state_dir: state_dir.path().to_path_buf(),
            },
        };

        // Hold the PATH mutex for the duration of the PATH mutation so no other
        // test that spawns processes observes the modified PATH concurrently.
        let _guard = PATH_MUTEX.lock().expect("PATH_MUTEX poisoned");

        // Prepend fake_bin_dir to PATH so our fakes shadow the real binaries.
        let original_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{}", fake_bin_dir.path().display(), original_path);
        std::env::set_var("PATH", &new_path);

        let result = run_s3_upload(&config, "full_20240102_120000");

        // Restore PATH regardless of outcome, before releasing the lock.
        std::env::set_var("PATH", &original_path);
        drop(_guard);

        assert!(
            result.is_ok(),
            "run_s3_upload should succeed with fake binaries; got: {result:?}"
        );

        // State should be Idle after a successful upload.
        let state = State::read(state_dir.path()).expect("state should be readable");
        assert_eq!(state, State::Idle, "state should be Idle after success");

        // The retention enforcement should have triggered one `aws s3 rm`.
        let rm_calls = std::fs::read_to_string(&rm_log).unwrap_or_default();
        assert!(
            rm_calls.contains("full_20240101_120000.tar.gpg"),
            "aws s3 rm should have been called for the oldest object; rm_log: {rm_calls:?}"
        );
    }

    /// Integration test placeholder — skipped unless run with `--ignored`
    /// on a machine with aws, gpg, and real btrfs snapshots.
    #[test]
    #[ignore]
    fn test_run_s3_upload_integration() {
        // Requires real AWS credentials, GPG key, and btrfs snapshots.
        // See DESIGN.md § Integration smoke test.
    }
}

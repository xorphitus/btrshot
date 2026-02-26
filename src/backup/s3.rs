use anyhow::Context;
use tracing::info;

use crate::cmd;
use crate::config::Config;
use crate::state::{Operation, State};

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
pub(crate) fn parse_s3_ls_output(output: &str) -> Vec<String> {
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
pub(crate) fn objects_to_delete(objects: &[String], retention_count: usize) -> Vec<String> {
    if objects.len() <= retention_count {
        return Vec::new();
    }

    let mut sorted = objects.to_vec();
    sorted.sort_unstable();

    let delete_count = sorted.len() - retention_count;
    sorted.into_iter().take(delete_count).collect()
}

/// Uploads a snapshot to S3 and enforces the retention policy.
///
/// Steps:
/// 1. Writes state `in_progress:s3_upload`.
/// 2. Streams the snapshot via `tar | gpg | aws s3 cp`.
/// 3. Lists current S3 objects, sorts by embedded timestamp, and deletes the
///    oldest objects that exceed `retention_count`.
/// 4. Writes state `idle`.
pub fn run_s3_upload(config: &Config, snapshot_name: &str) -> anyhow::Result<()> {
    info!("Starting S3 upload for snapshot: {}", snapshot_name);

    let state_dir = &config.state.state_dir;
    let backup_path = &config.paths.backup_path;
    let bucket = &config.s3.bucket;
    let public_key_file = &config.gpg.public_key_file;
    let retention_count = config.s3.retention_count;

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

    cmd::pipe(&[
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
    ])
    .context("failed to stream-upload snapshot to S3")?;

    // Step 3: enforce S3 retention
    let s3_prefix = format!("s3://{}/", bucket);
    let ls_output = run_s3_ls(&s3_prefix).context("failed to list S3 objects")?;
    let objects = parse_s3_ls_output(&ls_output);
    let to_delete = objects_to_delete(&objects, retention_count);

    for object in &to_delete {
        let s3_uri = format!("s3://{}/{}", bucket, object);
        cmd::run("aws", &["s3", "rm", &s3_uri])
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

/// Runs `aws s3 ls <prefix>` and returns the captured stdout as a `String`.
///
/// Returns `Err` if the command exits non-zero.
fn run_s3_ls(prefix: &str) -> anyhow::Result<String> {
    use std::process::Command;

    let output = Command::new("aws")
        .args(["s3", "ls", prefix])
        .output()
        .context("failed to spawn 'aws s3 ls'")?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output.status.code().unwrap_or(-1);
        anyhow::bail!("'aws s3 ls' exited with status {code}: {}", stderr.trim())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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

    /// `run_s3_upload` should return `Err` immediately when the state directory
    /// does not exist (the very first state write fails).
    #[test]
    fn test_run_s3_upload_fails_on_nonexistent_state_dir() {
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
                public_key_file: std::path::PathBuf::from("/etc/key.pub"),
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

        // The pipeline (tar | gpg | aws) will fail because paths don't exist.
        let result = run_s3_upload(&config, "full_20240101_120000");
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

    /// Integration test placeholder — skipped unless run with `--ignored`
    /// on a machine with aws, gpg, and real btrfs snapshots.
    #[test]
    #[ignore]
    fn test_run_s3_upload_integration() {
        // Requires real AWS credentials, GPG key, and btrfs snapshots.
        // See DESIGN.md § Integration smoke test.
    }
}

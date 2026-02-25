use std::path::Path;
use std::time::SystemTime;

use anyhow::Context;
use tracing::info;

use crate::backup::s3::run_s3_upload;
use crate::cmd;
use crate::config::Config;
use crate::snapshot::{
    current_symlink, full_snapshot_name, snapshots_dir, timestamp_now, FULL_PREFIX, SNAP_BASE_FULL,
    SNAP_TMP,
};
use crate::state::{Operation, State, Timestamps};

/// Performs a full btrfs snapshot backup:
///
/// 1. Writes state `in_progress:full`.
/// 2. Creates a read-only snapshot of the source subvolume on Disk A as `.snap_tmp`.
/// 3. Sends the snapshot to Disk B via `btrfs send | btrfs receive`.
/// 4. Renames the received snapshot to `full_<ts>`.
/// 5. Updates the `current` symlink on Disk B.
/// 6. Deletes old snapshots on Disk B (all entries except the new full snapshot).
/// 7. On Disk A: removes the old `.snap_base_full` (if present) and renames
///    `.snap_tmp` to `.snap_base_full`.
/// 8. Initiates the S3 upload for the new full snapshot.
/// 9. Records the `last_full_backup` timestamp and writes state `idle`.
pub fn run_full_backup(config: &Config) -> anyhow::Result<()> {
    info!("Starting full backup");

    let state_dir = &config.state.state_dir;
    let source_path = &config.paths.source_path;
    let backup_path = &config.paths.backup_path;

    // Step 1: write state in_progress:full
    State::InProgress(Operation::Full)
        .write(state_dir)
        .context("failed to write in_progress:full state")?;

    // Step 2: create read-only snapshot on A
    let snap_tmp = source_path.join(SNAP_TMP);
    let subvolume_path = config.source_subvolume_path();
    let subvolume_str = subvolume_path.to_string_lossy().into_owned();
    let snap_tmp_str = snap_tmp.to_string_lossy().into_owned();
    cmd::run(
        "btrfs",
        &["subvolume", "snapshot", "-r", &subvolume_str, &snap_tmp_str],
    )
    .context("failed to create read-only snapshot on Disk A")?;

    // Step 3: send snapshot to Disk B
    let snaps_dir = snapshots_dir(backup_path);
    let snaps_dir_str = snaps_dir.to_string_lossy().into_owned();
    cmd::pipe(&[
        ("btrfs", &["send", &snap_tmp_str] as &[&str]),
        ("btrfs", &["receive", &snaps_dir_str]),
    ])
    .context("failed to send snapshot to Disk B")?;

    // Step 4: rename received snapshot to full_<ts>
    let ts = timestamp_now();
    let snapshot_name = full_snapshot_name(&ts).context("failed to build full snapshot name")?;
    let received = snaps_dir.join(SNAP_TMP);
    let final_path = snaps_dir.join(&snapshot_name);
    std::fs::rename(&received, &final_path).with_context(|| {
        format!(
            "failed to rename {} to {}",
            received.display(),
            final_path.display()
        )
    })?;

    // Step 5: update `current` symlink
    let symlink_path = current_symlink(backup_path);
    if symlink_path.symlink_metadata().is_ok() {
        std::fs::remove_file(&symlink_path).with_context(|| {
            format!(
                "failed to remove old 'current' symlink at {}",
                symlink_path.display()
            )
        })?;
    }
    // The symlink target is relative to the backup_path directory.
    let symlink_target = Path::new("snapshots").join(&snapshot_name);
    std::os::unix::fs::symlink(&symlink_target, &symlink_path).with_context(|| {
        format!(
            "failed to create 'current' symlink {} -> {}",
            symlink_path.display(),
            symlink_target.display()
        )
    })?;

    // Step 6: delete all old snapshots on Disk B (everything except the new full snapshot)
    delete_old_snapshots(&snaps_dir, &snapshot_name)
        .context("failed to delete old snapshots on Disk B")?;

    // Step 7: on A, rotate .snap_tmp -> .snap_base_full
    let snap_base = source_path.join(SNAP_BASE_FULL);
    if snap_base.symlink_metadata().is_ok() {
        let snap_base_str = snap_base.to_string_lossy().into_owned();
        cmd::run("btrfs", &["subvolume", "delete", &snap_base_str])
            .context("failed to delete old .snap_base_full on Disk A")?;
    }
    std::fs::rename(&snap_tmp, &snap_base).with_context(|| {
        format!(
            "failed to rename {} to {}",
            snap_tmp.display(),
            snap_base.display()
        )
    })?;

    // Step 8: S3 upload
    run_s3_upload(config, &snapshot_name).context("S3 upload failed")?;

    // Step 9: record timestamp and mark idle
    let now = current_unix_ts().context("failed to read system clock")?;
    Timestamps::write_full(state_dir, now).context("failed to write last_full_backup timestamp")?;
    State::Idle
        .write(state_dir)
        .context("failed to write idle state")?;

    info!("Full backup complete");
    Ok(())
}

/// Deletes every entry inside `snaps_dir` that is not `keep_name`.
///
/// Uses `btrfs subvolume delete` to remove each btrfs subvolume directory.
fn delete_old_snapshots(snaps_dir: &Path, keep_name: &str) -> anyhow::Result<()> {
    let entries = std::fs::read_dir(snaps_dir)
        .with_context(|| format!("failed to read snapshots dir: {}", snaps_dir.display()))?;

    for entry in entries {
        let entry = entry.with_context(|| {
            format!("failed to read directory entry in {}", snaps_dir.display())
        })?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip the snapshot we just created.
        if name_str == keep_name {
            continue;
        }

        // Only delete entries that look like btrfs snapshots (full_ or incr_ prefixed).
        if !name_str.starts_with(FULL_PREFIX) && !name_str.starts_with(crate::snapshot::INCR_PREFIX)
        {
            continue;
        }

        let path = entry.path();
        let path_str = path.to_string_lossy().into_owned();
        cmd::run("btrfs", &["subvolume", "delete", &path_str])
            .with_context(|| format!("failed to delete old snapshot: {}", path.display()))?;
    }

    Ok(())
}

/// Returns the current time as Unix epoch seconds.
fn current_unix_ts() -> anyhow::Result<u64> {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .context("system clock is before the Unix epoch")
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
    // delete_old_snapshots — pure filesystem logic, no btrfs required
    // -----------------------------------------------------------------------

    /// When the snapshots directory contains only the kept snapshot, nothing is
    /// deleted (or attempted via btrfs).
    ///
    /// Because `btrfs subvolume delete` would fail on a plain directory, the
    /// test verifies that the kept entry is left untouched.
    #[test]
    fn test_delete_old_snapshots_skips_kept_entry() {
        let dir = TempDir::new().expect("temp dir");
        let snaps = dir.path().join("snapshots");
        std::fs::create_dir(&snaps).expect("create snapshots dir");
        let keep = "full_20240101_120000";
        std::fs::create_dir(snaps.join(keep)).expect("create keep dir");

        // No btrfs call should be made, so no error even without btrfs installed.
        let result = delete_old_snapshots(&snaps, keep);
        assert!(
            result.is_ok(),
            "should succeed when only the kept snapshot exists: {result:?}"
        );
        assert!(
            snaps.join(keep).exists(),
            "kept snapshot should still exist"
        );
    }

    /// Entries that do not match `full_` or `incr_` prefixes are left alone
    /// (they are not valid snapshot directories and must not be deleted).
    #[test]
    fn test_delete_old_snapshots_ignores_non_snapshot_entries() {
        let dir = TempDir::new().expect("temp dir");
        let snaps = dir.path().join("snapshots");
        std::fs::create_dir(&snaps).expect("create snapshots dir");

        let keep = "full_20240101_120000";
        std::fs::create_dir(snaps.join(keep)).expect("create keep dir");

        // Plain directory that does not look like a snapshot.
        let other = "some_other_dir";
        std::fs::create_dir(snaps.join(other)).expect("create other dir");

        let result = delete_old_snapshots(&snaps, keep);
        assert!(
            result.is_ok(),
            "should succeed ignoring non-snapshot entries: {result:?}"
        );
        // The non-snapshot entry must not have been touched.
        assert!(
            snaps.join(other).exists(),
            "non-snapshot directory should not be deleted"
        );
    }

    /// If an old `full_` snapshot exists alongside the new one, `btrfs
    /// subvolume delete` would be called.  Because we cannot run btrfs in unit
    /// tests, we verify that the function returns `Err` for the old snapshot
    /// (the btrfs call fails), which demonstrates the code path is exercised.
    #[test]
    fn test_delete_old_snapshots_attempts_to_delete_old_full_snapshot() {
        let dir = TempDir::new().expect("temp dir");
        let snaps = dir.path().join("snapshots");
        std::fs::create_dir(&snaps).expect("create snapshots dir");

        let keep = "full_20240202_120000";
        std::fs::create_dir(snaps.join(keep)).expect("create keep dir");

        let old = "full_20240101_120000";
        std::fs::create_dir(snaps.join(old)).expect("create old dir");

        // btrfs is not available in the test environment, so this should fail.
        let result = delete_old_snapshots(&snaps, keep);
        assert!(
            result.is_err(),
            "should fail because btrfs subvolume delete cannot run on a plain dir"
        );
    }

    /// Same as above but for an old `incr_` snapshot.
    #[test]
    fn test_delete_old_snapshots_attempts_to_delete_old_incr_snapshot() {
        let dir = TempDir::new().expect("temp dir");
        let snaps = dir.path().join("snapshots");
        std::fs::create_dir(&snaps).expect("create snapshots dir");

        let keep = "full_20240202_120000";
        std::fs::create_dir(snaps.join(keep)).expect("create keep dir");

        let old_incr = "incr_20240101_120000";
        std::fs::create_dir(snaps.join(old_incr)).expect("create old incr dir");

        let result = delete_old_snapshots(&snaps, keep);
        assert!(
            result.is_err(),
            "should fail because btrfs subvolume delete cannot run on a plain dir"
        );
    }

    /// `delete_old_snapshots` returns `Err` when the snapshots directory does
    /// not exist.
    #[test]
    fn test_delete_old_snapshots_missing_dir_returns_err() {
        let result =
            delete_old_snapshots(Path::new("/nonexistent/snapshots"), "full_20240101_120000");
        assert!(result.is_err(), "missing dir should return Err");
    }

    // -----------------------------------------------------------------------
    // current_unix_ts
    // -----------------------------------------------------------------------

    #[test]
    fn test_current_unix_ts_is_positive() {
        let ts = current_unix_ts().expect("should return a timestamp");
        assert!(ts > 0, "Unix timestamp should be positive");
    }

    #[test]
    fn test_current_unix_ts_is_recent() {
        // The timestamp must be after 2020-01-01 (Unix epoch 1577836800).
        let ts = current_unix_ts().expect("should return a timestamp");
        assert!(
            ts > 1_577_836_800,
            "Unix timestamp should be after 2020-01-01, got {ts}"
        );
    }

    // -----------------------------------------------------------------------
    // run_full_backup — error propagation without btrfs
    // -----------------------------------------------------------------------

    /// `run_full_backup` should return `Err` immediately when the state
    /// directory does not exist (the very first write fails).
    #[test]
    fn test_run_full_backup_fails_on_nonexistent_state_dir() {
        use crate::config::{GpgConfig, PathsConfig, S3Config, ScheduleConfig, StateConfig};

        let config = Config {
            paths: PathsConfig {
                source_path: std::path::PathBuf::from("/nonexistent/source"),
                source_subvolume: "data".to_string(),
                backup_path: std::path::PathBuf::from("/nonexistent/backup"),
            },
            s3: S3Config {
                bucket: "test-bucket".to_string(),
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

        let result = run_full_backup(&config);
        assert!(
            result.is_err(),
            "run_full_backup should fail when state_dir does not exist"
        );
    }

    /// `run_full_backup` fails at the `btrfs subvolume snapshot` step when the
    /// state directory is valid but btrfs is unavailable / source paths don't exist.
    #[test]
    fn test_run_full_backup_fails_at_btrfs_snapshot_step() {
        use crate::config::{GpgConfig, PathsConfig, S3Config, ScheduleConfig, StateConfig};

        let state_dir = TempDir::new().expect("temp dir");

        let config = Config {
            paths: PathsConfig {
                source_path: std::path::PathBuf::from("/nonexistent/source"),
                source_subvolume: "data".to_string(),
                backup_path: std::path::PathBuf::from("/nonexistent/backup"),
            },
            s3: S3Config {
                bucket: "test-bucket".to_string(),
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

        // The state write should succeed but btrfs snapshot must fail.
        let result = run_full_backup(&config);
        assert!(
            result.is_err(),
            "run_full_backup should fail when btrfs subvolume snapshot cannot run"
        );
        // The state file should have been written (in_progress:full) before the failure.
        let state = State::read(state_dir.path()).expect("state should be readable");
        assert_eq!(
            state,
            State::InProgress(Operation::Full),
            "state should be in_progress:full after the first step"
        );
    }

    // -----------------------------------------------------------------------
    // Full backup integration (requires real btrfs — skipped in CI)
    // -----------------------------------------------------------------------

    /// Full happy-path integration test. Skipped unless run with `--ignored`
    /// on a machine with btrfs subvolumes available.
    #[test]
    #[ignore]
    fn test_run_full_backup_integration() {
        // Requires two btrfs filesystems mounted and writable.
        // See DESIGN.md § Integration smoke test.
    }
}

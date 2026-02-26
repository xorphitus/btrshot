use std::time::SystemTime;

use anyhow::Context;
use tracing::info;

use crate::cmd;
use crate::config::Config;
use crate::snapshot::{incr_snapshot_name, snapshots_dir, timestamp_now, SNAP_BASE_FULL, SNAP_TMP};
use crate::state::{Operation, State, Timestamps};

/// Performs an incremental btrfs snapshot backup:
///
/// 1. Writes state `in_progress:incremental`.
/// 2. Creates a read-only snapshot of the source subvolume on Disk A as `.snap_tmp`.
/// 3. Sends the incremental snapshot to Disk B via
///    `btrfs send -p .snap_base_full .snap_tmp | btrfs receive`.
/// 4. Renames the received snapshot to `incr_<ts>`.
/// 5. On Disk A: removes the old `.snap_base_full` and renames `.snap_tmp` to
///    `.snap_base_full`.
/// 6. Records the `last_incremental_backup` timestamp.
/// 7. Writes state `idle`.
pub fn run_incremental_backup(config: &Config) -> anyhow::Result<()> {
    info!("Starting incremental backup");

    let state_dir = &config.state.state_dir;
    let source_path = &config.paths.source_path;
    let backup_path = &config.paths.backup_path;

    // Capture the timestamp before creating the snapshot so the name reflects
    // when the backup was initiated, not when the rename happens.
    let ts = timestamp_now();

    // Step 1: write state in_progress:incremental
    State::InProgress(Operation::Incremental)
        .write(state_dir)
        .context("failed to write in_progress:incremental state")?;

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

    // Pre-condition check: .snap_base_full must exist before we attempt an
    // incremental send. If it is absent we delete the temporary snapshot we
    // just created so we don't leave orphaned state on Disk A.
    let snap_base = source_path.join(SNAP_BASE_FULL);
    let snap_base_str = snap_base.to_string_lossy().into_owned();
    if !snap_base.exists() {
        // Best-effort cleanup of the snapshot created in Step 2.
        let _ = cmd::run("btrfs", &["subvolume", "delete", &snap_tmp_str]);
        anyhow::bail!(
            "Cannot run incremental backup: base snapshot `.snap_base_full` does not exist \
             at {}. A full backup must run first.",
            snap_base.display()
        );
    }

    // Steps 3-5 are wrapped so that a failure at any point triggers best-effort
    // cleanup of the `.snap_tmp` snapshot on Disk A before the error propagates.
    let snaps_dir = snapshots_dir(backup_path);
    let snaps_dir_str = snaps_dir.to_string_lossy().into_owned();
    let snapshot_name =
        incr_snapshot_name(&ts).context("failed to build incremental snapshot name")?;

    let result = (|| -> anyhow::Result<()> {
        // Step 3: send incremental snapshot to Disk B, using .snap_base_full as parent
        cmd::pipe(&[
            (
                "btrfs",
                &["send", "-p", &snap_base_str, &snap_tmp_str] as &[&str],
            ),
            ("btrfs", &["receive", &snaps_dir_str]),
        ])
        .context("failed to send incremental snapshot to Disk B")?;

        // Step 4: rename received snapshot from .snap_tmp to incr_<ts>
        // Both `received` and `final_path` are on the same filesystem (Disk B),
        // so rename(2) is guaranteed to be atomic.
        let received = snaps_dir.join(SNAP_TMP);
        let final_path = snaps_dir.join(&snapshot_name);
        std::fs::rename(&received, &final_path).with_context(|| {
            format!(
                "failed to rename {} to {}",
                received.display(),
                final_path.display()
            )
        })?;

        // Step 5: on A, rotate .snap_tmp -> .snap_base_full
        // Delete old .snap_base_full first (it must exist for the incremental to work,
        // but we replace it with the new snapshot as the base for the next incremental).
        cmd::run("btrfs", &["subvolume", "delete", &snap_base_str])
            .context("failed to delete old .snap_base_full on Disk A")?;
        // Both paths are on Disk A (the same filesystem), so rename(2) is atomic.
        std::fs::rename(&snap_tmp, &snap_base).with_context(|| {
            format!(
                "failed to rename {} to {}",
                snap_tmp.display(),
                snap_base.display()
            )
        })?;

        Ok(())
    })();

    if result.is_err() {
        // Best-effort cleanup: remove the temporary snapshot to avoid orphaned state.
        let _ = cmd::run("btrfs", &["subvolume", "delete", &snap_tmp_str]);
    }
    result?;

    // Step 6: record timestamp
    let now = current_unix_ts().context("failed to read system clock")?;
    Timestamps::write_incremental(state_dir, now)
        .context("failed to write last_incremental_backup timestamp")?;

    // Step 7: write state idle
    State::Idle
        .write(state_dir)
        .context("failed to write idle state")?;

    info!("Incremental backup complete");
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
    // run_incremental_backup — error propagation without btrfs
    // -----------------------------------------------------------------------

    /// `run_incremental_backup` should return `Err` immediately when the state
    /// directory does not exist (the very first write fails).
    #[test]
    fn test_run_incremental_backup_fails_on_nonexistent_state_dir() {
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

        let result = run_incremental_backup(&config);
        assert!(
            result.is_err(),
            "run_incremental_backup should fail when state_dir does not exist"
        );
    }

    /// `run_incremental_backup` should write `in_progress:incremental` state
    /// before attempting any btrfs operations. When the btrfs call fails
    /// (unavailable in test environment), the state file should already
    /// reflect the in-progress state.
    #[test]
    fn test_run_incremental_backup_writes_state_before_btrfs() {
        use crate::config::{GpgConfig, PathsConfig, S3Config, ScheduleConfig, StateConfig};
        use crate::state::State;

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

        // The state write should succeed, but btrfs snapshot must fail because
        // the source path does not exist.
        let result = run_incremental_backup(&config);
        assert!(
            result.is_err(),
            "run_incremental_backup should fail when btrfs subvolume snapshot cannot run"
        );
        // The state file should have been written (in_progress:incremental) before the failure.
        let state = State::read(state_dir.path()).expect("state should be readable");
        assert_eq!(
            state,
            State::InProgress(Operation::Incremental),
            "state should be in_progress:incremental after the first step"
        );
    }

    /// Integration test placeholder — skipped unless run with `--ignored`
    /// on a machine with btrfs subvolumes available.
    #[test]
    #[ignore]
    fn test_run_incremental_backup_integration() {
        // Requires two btrfs filesystems mounted and writable.
        // See DESIGN.md § Integration smoke test.
    }
}

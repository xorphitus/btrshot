use std::path::Path;

use anyhow::Context;
use tracing::{debug, error};

use crate::config::Config;

/// Validates that `subvolume_path` is a btrfs subvolume by running
/// `btrfs subvolume show <subvolume_path>`.
///
/// Returns `Ok(())` if the command exits with status 0, otherwise returns `Err`
/// with a descriptive message that includes the path and captured stderr.
pub fn validate_source(subvolume_path: &Path) -> anyhow::Result<()> {
    debug!(path = %subvolume_path.display(), "validating btrfs subvolume");

    let output = std::process::Command::new("btrfs")
        .args(["subvolume", "show"])
        .arg(subvolume_path)
        .output()
        .with_context(|| {
            format!(
                "failed to spawn 'btrfs subvolume show' for path: {}",
                subvolume_path.display()
            )
        })?;

    if output.status.success() {
        debug!(path = %subvolume_path.display(), "source subvolume validated");
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output.status.code().unwrap_or(-1);
        error!(
            path = %subvolume_path.display(),
            exit_code = code,
            stderr = %stderr.trim(),
            "source path is not a valid btrfs subvolume"
        );
        anyhow::bail!(
            "path '{}' is not a btrfs subvolume (exit code {}): {}",
            subvolume_path.display(),
            code,
            stderr.trim()
        )
    }
}

/// Validates that `backup_path` is mounted on a btrfs filesystem.
///
/// Reads `/proc/mounts` to check whether `backup_path` (or any of its ancestor
/// directories) is listed with filesystem type `btrfs`.
///
/// Returns `Ok(())` if a matching btrfs mount is found, otherwise `Err`.
pub fn validate_backup_fs(backup_path: &Path) -> anyhow::Result<()> {
    debug!(path = %backup_path.display(), "validating backup filesystem type");

    let mounts =
        read_proc_mounts().context("failed to read /proc/mounts for filesystem validation")?;

    if path_is_on_btrfs(backup_path, &mounts) {
        debug!(path = %backup_path.display(), "backup filesystem is btrfs");
        Ok(())
    } else {
        error!(
            path = %backup_path.display(),
            "backup path is not on a btrfs filesystem"
        );
        anyhow::bail!(
            "backup path '{}' is not on a btrfs filesystem; \
             Disk B must be formatted as btrfs to receive snapshots",
            backup_path.display()
        )
    }
}

/// Runs all startup validations required before entering the scheduler loop.
///
/// Calls `validate_source` with the configured source subvolume path and
/// `validate_backup_fs` with the configured backup path.
pub fn validate_all(config: &Config) -> anyhow::Result<()> {
    validate_source(&config.source_subvolume_path())
        .context("source subvolume validation failed")?;
    validate_backup_fs(&config.paths.backup_path).context("backup filesystem validation failed")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Reads `/proc/mounts` and returns all entries as a `Vec<(device, mount_point, fs_type)>`.
///
/// Each line in `/proc/mounts` has the format:
/// `<device> <mount_point> <fs_type> <options> <dump> <pass>`
///
/// Lines that do not have at least three whitespace-separated fields are silently skipped.
fn read_proc_mounts() -> anyhow::Result<Vec<(String, String, String)>> {
    let contents =
        std::fs::read_to_string("/proc/mounts").context("failed to read /proc/mounts")?;
    Ok(parse_proc_mounts(&contents))
}

/// Parses the text content of `/proc/mounts` (or a compatible format) into
/// `(device, mount_point, fs_type)` triples.
///
/// This function is pure and separated from the I/O in `read_proc_mounts` so it
/// can be tested without touching the real filesystem.
fn parse_proc_mounts(contents: &str) -> Vec<(String, String, String)> {
    contents
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let device = fields.next()?.to_string();
            let mount_point = fields.next()?.to_string();
            let fs_type = fields.next()?.to_string();
            Some((device, mount_point, fs_type))
        })
        .collect()
}

/// Returns `true` if `path` is covered by any btrfs mount in `mounts`.
///
/// A mount covers `path` if `path` starts with the mount point and the
/// filesystem type is `btrfs`. When multiple mounts match (e.g. `/` and
/// `/mnt/backup`), the one with the longest matching prefix wins; it is
/// considered btrfs only if that best match has type `btrfs`.
fn path_is_on_btrfs(path: &Path, mounts: &[(String, String, String)]) -> bool {
    // Find the longest mount-point prefix that covers `path`.
    mounts
        .iter()
        .filter(|(_, mount_point, _)| {
            let mp = Path::new(mount_point);
            path.starts_with(mp)
        })
        .max_by_key(|(_, mount_point, _)| mount_point.len())
        .is_some_and(|(_, _, fs_type)| fs_type == "btrfs")
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
    // validate_source — error paths (no real btrfs needed)
    // -----------------------------------------------------------------------

    #[test]
    fn test_validate_source_nonexistent_path_returns_err() {
        let result = validate_source(Path::new("/nonexistent/path/that/does/not/exist"));
        assert!(
            result.is_err(),
            "a nonexistent path should fail validate_source"
        );
    }

    #[test]
    fn test_validate_source_regular_directory_returns_err() {
        let dir = TempDir::new().expect("temp dir");
        let result = validate_source(dir.path());
        assert!(
            result.is_err(),
            "a plain directory should not be a valid btrfs subvolume"
        );
    }

    #[test]
    fn test_validate_source_error_contains_path() {
        let path = Path::new("/nonexistent/subvolume");
        let err = validate_source(path).expect_err("should be Err");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("/nonexistent/subvolume"),
            "error message should mention the path; got: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // parse_proc_mounts (pure helper)
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_proc_mounts_basic() {
        let contents = "sysfs /sys sysfs rw,nosuid 0 0\n\
                        /dev/sdb1 /mnt/backup btrfs rw,relatime 0 0\n";
        let mounts = parse_proc_mounts(contents);
        assert_eq!(mounts.len(), 2);
        assert_eq!(
            mounts[1],
            (
                "/dev/sdb1".to_string(),
                "/mnt/backup".to_string(),
                "btrfs".to_string()
            )
        );
    }

    #[test]
    fn test_parse_proc_mounts_skips_short_lines() {
        let contents = "incomplete_line\n\
                        /dev/sdb1 /mnt/backup btrfs rw 0 0\n";
        let mounts = parse_proc_mounts(contents);
        // "incomplete_line" has no second or third field, should be skipped
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].1, "/mnt/backup");
    }

    #[test]
    fn test_parse_proc_mounts_empty_input() {
        let mounts = parse_proc_mounts("");
        assert!(mounts.is_empty());
    }

    // -----------------------------------------------------------------------
    // path_is_on_btrfs (pure helper — testable without real mounts)
    // -----------------------------------------------------------------------

    #[test]
    fn test_path_is_on_btrfs_exact_mount_match() {
        let mounts = vec![(
            "dev/sdb".to_string(),
            "/mnt/backup".to_string(),
            "btrfs".to_string(),
        )];
        assert!(path_is_on_btrfs(Path::new("/mnt/backup"), &mounts));
    }

    #[test]
    fn test_path_is_on_btrfs_child_path_match() {
        let mounts = vec![(
            "dev/sdb".to_string(),
            "/mnt/backup".to_string(),
            "btrfs".to_string(),
        )];
        assert!(path_is_on_btrfs(
            Path::new("/mnt/backup/snapshots"),
            &mounts
        ));
    }

    #[test]
    fn test_path_is_on_btrfs_wrong_fs_type_returns_false() {
        let mounts = vec![(
            "dev/sdc".to_string(),
            "/mnt/backup".to_string(),
            "ext4".to_string(),
        )];
        assert!(!path_is_on_btrfs(Path::new("/mnt/backup"), &mounts));
    }

    #[test]
    fn test_path_is_on_btrfs_no_matching_mount_returns_false() {
        let mounts = vec![(
            "dev/sdb".to_string(),
            "/mnt/other".to_string(),
            "btrfs".to_string(),
        )];
        assert!(!path_is_on_btrfs(Path::new("/mnt/backup"), &mounts));
    }

    #[test]
    fn test_path_is_on_btrfs_prefix_overlap_does_not_match() {
        // "/mnt/back" should NOT match "/mnt/backup" as a child
        let mounts = vec![(
            "dev/sdb".to_string(),
            "/mnt/backup".to_string(),
            "btrfs".to_string(),
        )];
        assert!(!path_is_on_btrfs(Path::new("/mnt/back"), &mounts));
    }

    #[test]
    fn test_path_is_on_btrfs_multiple_mounts_picks_btrfs() {
        let mounts = vec![
            ("dev/sda".to_string(), "/".to_string(), "ext4".to_string()),
            (
                "dev/sdb".to_string(),
                "/mnt/backup".to_string(),
                "btrfs".to_string(),
            ),
        ];
        assert!(path_is_on_btrfs(Path::new("/mnt/backup"), &mounts));
    }

    #[test]
    fn test_path_is_on_btrfs_nested_mounts_uses_longest_prefix() {
        // /mnt/backup is ext4, /mnt/backup/btrfs_sub is btrfs
        let mounts = vec![
            (
                "dev/sdb".to_string(),
                "/mnt/backup".to_string(),
                "ext4".to_string(),
            ),
            (
                "dev/sdc".to_string(),
                "/mnt/backup/btrfs_sub".to_string(),
                "btrfs".to_string(),
            ),
        ];
        // /mnt/backup/btrfs_sub/data is under the btrfs sub-mount
        assert!(path_is_on_btrfs(
            Path::new("/mnt/backup/btrfs_sub/data"),
            &mounts
        ));
        // /mnt/backup/other is only under the ext4 mount
        assert!(!path_is_on_btrfs(Path::new("/mnt/backup/other"), &mounts));
    }

    // -----------------------------------------------------------------------
    // validate_backup_fs — error path (non-btrfs temp dir)
    // -----------------------------------------------------------------------

    #[test]
    fn test_validate_backup_fs_non_btrfs_path_returns_err() {
        // tempfile directories are on the host filesystem (typically ext4/tmpfs),
        // which is not btrfs, so this should return Err.
        let dir = TempDir::new().expect("temp dir");
        let result = validate_backup_fs(dir.path());
        assert!(
            result.is_err(),
            "a non-btrfs path should fail validate_backup_fs"
        );
    }

    #[test]
    fn test_validate_backup_fs_error_contains_path() {
        let dir = TempDir::new().expect("temp dir");
        let err = validate_backup_fs(dir.path()).expect_err("should be Err");
        let msg = format!("{err:#}");
        let path_str = dir.path().to_string_lossy();
        assert!(
            msg.contains(path_str.as_ref()),
            "error message should mention the path; got: {msg}"
        );
    }
}

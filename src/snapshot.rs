use std::path::{Path, PathBuf};

use chrono::Utc;

/// Temporary snapshot name used on Disk A while a backup is in progress.
pub const SNAP_TMP: &str = ".snap_tmp";

/// Base snapshot name kept on Disk A as the parent for the next incremental.
pub const SNAP_BASE_FULL: &str = ".snap_base_full";

/// Prefix for full-backup snapshot names, used when constructing and parsing names.
pub const FULL_PREFIX: &str = "full_";

/// Prefix for incremental snapshot names, used when constructing and parsing names.
pub const INCR_PREFIX: &str = "incr_";

/// Returns the current UTC time formatted as `YYYYMMDD_HHMMSS`.
pub fn timestamp_now() -> String {
    Utc::now().format("%Y%m%d_%H%M%S").to_string()
}

/// Validates that `ts` matches the `YYYYMMDD_HHMMSS` pattern exactly.
///
/// The check is regex-free: length must be 15, characters 0–7 and 9–14 must be
/// ASCII digits, and character 8 must be `_`.
fn validate_timestamp(ts: &str) -> anyhow::Result<()> {
    let bytes = ts.as_bytes();
    if bytes.len() != 15 {
        anyhow::bail!(
            "invalid timestamp {:?}: expected 15 characters (YYYYMMDD_HHMMSS), got {}",
            ts,
            bytes.len()
        );
    }
    // Positions 0..=7 are the date digits, 9..=14 are the time digits.
    for i in (0..=7usize).chain(9..=14) {
        if !bytes[i].is_ascii_digit() {
            anyhow::bail!(
                "invalid timestamp {:?}: character at position {i} is not an ASCII digit",
                ts
            );
        }
    }
    if bytes[8] != b'_' {
        anyhow::bail!(
            "invalid timestamp {:?}: character at position 8 must be '_', got '{}'",
            ts,
            bytes[8] as char
        );
    }
    Ok(())
}

/// Returns the name for a full-backup snapshot: `full_<ts>`.
///
/// Returns `Err` if `ts` does not match the `YYYYMMDD_HHMMSS` pattern.
pub fn full_snapshot_name(ts: &str) -> anyhow::Result<String> {
    validate_timestamp(ts)?;
    Ok(format!("{FULL_PREFIX}{ts}"))
}

/// Returns the name for an incremental snapshot: `incr_<ts>`.
///
/// Returns `Err` if `ts` does not match the `YYYYMMDD_HHMMSS` pattern.
pub fn incr_snapshot_name(ts: &str) -> anyhow::Result<String> {
    validate_timestamp(ts)?;
    Ok(format!("{INCR_PREFIX}{ts}"))
}

/// Returns the path to the snapshots directory on Disk B: `<backup_path>/snapshots`.
pub fn snapshots_dir(backup_path: &Path) -> PathBuf {
    backup_path.join("snapshots")
}

/// Returns the path to the `current` symlink on Disk B: `<backup_path>/current`.
pub fn current_symlink(backup_path: &Path) -> PathBuf {
    backup_path.join("current")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // timestamp_now
    // -----------------------------------------------------------------------

    #[test]
    fn test_timestamp_now_has_correct_length() {
        // YYYYMMDD_HHMMSS is exactly 15 characters.
        let ts = timestamp_now();
        assert_eq!(
            ts.len(),
            15,
            "expected 15 chars (YYYYMMDD_HHMMSS), got {:?}",
            ts
        );
    }

    #[test]
    fn test_timestamp_now_format_matches_pattern() {
        let ts = timestamp_now();
        // Characters 0-7 are digits, 8 is '_', 9-14 are digits.
        let chars: Vec<char> = ts.chars().collect();
        assert_eq!(chars.len(), 15);
        // Positions 0..=7 are the date digits, 9..=14 are the time digits.
        for i in (0..=7usize).chain(9..=14) {
            assert!(
                chars[i].is_ascii_digit(),
                "position {i} should be a digit in {:?}",
                ts
            );
        }
        assert_eq!(chars[8], '_', "position 8 should be '_' in {:?}", ts);
    }

    #[test]
    fn test_timestamp_now_is_within_before_after_bracket() {
        // Bracket the call with before/after timestamps so the test is deterministic.
        let before = chrono::Utc::now();
        let ts = timestamp_now();
        let after = chrono::Utc::now();

        let parsed = chrono::NaiveDateTime::parse_from_str(&ts, "%Y%m%d_%H%M%S")
            .expect("timestamp_now should produce a parseable string");
        let parsed_utc: chrono::DateTime<chrono::Utc> =
            chrono::DateTime::from_naive_utc_and_offset(parsed, chrono::Utc);

        // The format truncates sub-second precision, so allow up to 1 s of rounding
        // downward on the lower bound.
        let lower = before - chrono::TimeDelta::seconds(1);
        assert!(
            parsed_utc >= lower,
            "timestamp_now ({:?}) is before lower bound ({:?})",
            parsed_utc,
            lower
        );
        assert!(
            parsed_utc <= after,
            "timestamp_now ({:?}) is after upper bound ({:?})",
            parsed_utc,
            after
        );
    }

    // -----------------------------------------------------------------------
    // validate_timestamp
    // -----------------------------------------------------------------------

    #[test]
    fn test_validate_timestamp_accepts_valid_timestamp() {
        assert!(
            validate_timestamp("20240101_120000").is_ok(),
            "valid timestamp should be accepted"
        );
    }

    #[test]
    fn test_validate_timestamp_rejects_empty_string() {
        assert!(
            validate_timestamp("").is_err(),
            "empty string should be rejected"
        );
    }

    #[test]
    fn test_validate_timestamp_rejects_path_traversal() {
        assert!(
            validate_timestamp("../evil").is_err(),
            "path traversal string should be rejected"
        );
    }

    #[test]
    fn test_validate_timestamp_rejects_wrong_length() {
        assert!(
            validate_timestamp("20240101_1200").is_err(),
            "timestamp with wrong length should be rejected"
        );
    }

    #[test]
    fn test_validate_timestamp_rejects_non_digit_in_date_part() {
        assert!(
            validate_timestamp("2024010a_120000").is_err(),
            "non-digit in date part should be rejected"
        );
    }

    #[test]
    fn test_validate_timestamp_rejects_wrong_separator() {
        assert!(
            validate_timestamp("20240101-120000").is_err(),
            "wrong separator character should be rejected"
        );
    }

    // -----------------------------------------------------------------------
    // full_snapshot_name
    // -----------------------------------------------------------------------

    #[test]
    fn test_full_snapshot_name_has_full_prefix() {
        let name = full_snapshot_name("20240101_120000").expect("valid timestamp");
        assert!(
            name.starts_with("full_"),
            "expected 'full_' prefix, got {:?}",
            name
        );
    }

    #[test]
    fn test_full_snapshot_name_contains_timestamp() {
        let ts = "20240101_120000";
        let name = full_snapshot_name(ts).expect("valid timestamp");
        assert!(
            name.contains(ts),
            "expected name to contain timestamp {:?}, got {:?}",
            ts,
            name
        );
    }

    #[test]
    fn test_full_snapshot_name_exact_format() {
        assert_eq!(
            full_snapshot_name("20240101_120000").expect("valid timestamp"),
            "full_20240101_120000"
        );
    }

    #[test]
    fn test_full_snapshot_name_with_empty_ts() {
        assert!(
            full_snapshot_name("").is_err(),
            "empty timestamp should return Err"
        );
    }

    #[test]
    fn test_full_snapshot_name_rejects_invalid_ts() {
        assert!(
            full_snapshot_name("../evil").is_err(),
            "path traversal string should return Err"
        );
        assert!(
            full_snapshot_name("not-a-timestamp").is_err(),
            "non-timestamp string should return Err"
        );
    }

    // -----------------------------------------------------------------------
    // incr_snapshot_name
    // -----------------------------------------------------------------------

    #[test]
    fn test_incr_snapshot_name_has_incr_prefix() {
        let name = incr_snapshot_name("20240101_120000").expect("valid timestamp");
        assert!(
            name.starts_with("incr_"),
            "expected 'incr_' prefix, got {:?}",
            name
        );
    }

    #[test]
    fn test_incr_snapshot_name_contains_timestamp() {
        let ts = "20240101_120000";
        let name = incr_snapshot_name(ts).expect("valid timestamp");
        assert!(
            name.contains(ts),
            "expected name to contain timestamp {:?}, got {:?}",
            ts,
            name
        );
    }

    #[test]
    fn test_incr_snapshot_name_exact_format() {
        assert_eq!(
            incr_snapshot_name("20240101_120000").expect("valid timestamp"),
            "incr_20240101_120000"
        );
    }

    #[test]
    fn test_incr_snapshot_name_with_empty_ts() {
        assert!(
            incr_snapshot_name("").is_err(),
            "empty timestamp should return Err"
        );
    }

    #[test]
    fn test_incr_snapshot_name_rejects_invalid_ts() {
        assert!(
            incr_snapshot_name("../evil").is_err(),
            "path traversal string should return Err"
        );
        assert!(
            incr_snapshot_name("not-a-timestamp").is_err(),
            "non-timestamp string should return Err"
        );
    }

    // -----------------------------------------------------------------------
    // snapshots_dir
    // -----------------------------------------------------------------------

    #[test]
    fn test_snapshots_dir_appends_snapshots_segment() {
        let backup = Path::new("/mnt/b");
        let dir = snapshots_dir(backup);
        assert_eq!(dir, PathBuf::from("/mnt/b/snapshots"));
    }

    #[test]
    fn test_snapshots_dir_with_trailing_slash_in_backup_path() {
        // PathBuf normalises trailing slashes, so the result is the same.
        let backup = Path::new("/mnt/b/");
        let dir = snapshots_dir(backup);
        assert_eq!(dir, PathBuf::from("/mnt/b/snapshots"));
    }

    #[test]
    fn test_snapshots_dir_with_relative_path() {
        let backup = Path::new("mnt/b");
        let dir = snapshots_dir(backup);
        assert_eq!(dir, PathBuf::from("mnt/b/snapshots"));
    }

    // -----------------------------------------------------------------------
    // current_symlink
    // -----------------------------------------------------------------------

    #[test]
    fn test_current_symlink_appends_current_segment() {
        let backup = Path::new("/mnt/b");
        let link = current_symlink(backup);
        assert_eq!(link, PathBuf::from("/mnt/b/current"));
    }

    #[test]
    fn test_current_symlink_with_trailing_slash() {
        // PathBuf normalises trailing slashes, so the result is the same.
        let backup = Path::new("/mnt/b/");
        let link = current_symlink(backup);
        assert_eq!(link, PathBuf::from("/mnt/b/current"));
    }

    #[test]
    fn test_current_symlink_with_relative_path() {
        let backup = Path::new("mnt/b");
        let link = current_symlink(backup);
        assert_eq!(link, PathBuf::from("mnt/b/current"));
    }
}

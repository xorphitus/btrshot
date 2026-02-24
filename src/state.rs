use std::fs::File;
use std::io::Write as IoWrite;
use std::path::Path;
use std::time::SystemTime;

use anyhow::Context;

/// Identifies which backup operation is currently in progress.
#[derive(Debug, PartialEq, Eq)]
pub enum Operation {
    Full,
    Incremental,
    S3Upload,
}

/// Tracks whether the daemon is idle or mid-operation.
///
/// Persisted to `<state_dir>/state` so the daemon can detect an interrupted
/// run on the next startup and clean up before proceeding.
///
/// The on-disk format is `<status>:<operation>:<timestamp>` where timestamp is
/// Unix epoch seconds. For `idle`, the operation field is empty:
/// `idle::1706000000`. The timestamp is written on every update and discarded
/// on read — the `State` enum variants do not carry it.
#[derive(Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    InProgress(Operation),
}

impl State {
    /// Reads the state file at `state_dir/state`.
    ///
    /// A missing file is treated as `Idle` (first run or clean shutdown).
    /// Any other I/O error or unrecognised content is returned as `Err`.
    pub fn read(state_dir: &Path) -> anyhow::Result<State> {
        let path = state_dir.join("state");
        match std::fs::read_to_string(&path) {
            Ok(raw) => State::parse(raw.trim()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::Idle),
            Err(e) => {
                Err(e).with_context(|| format!("failed to read state file: {}", path.display()))
            }
        }
    }

    /// Writes the canonical string representation to `state_dir/state`.
    ///
    /// The current Unix timestamp is embedded in the written string as the
    /// third colon-separated field (e.g. `idle::1706000000`).
    pub fn write(&self, state_dir: &Path) -> anyhow::Result<()> {
        let path = state_dir.join("state");
        let ts = current_unix_ts()?;
        atomic_write(&path, &self.to_file_string(ts))
    }

    /// Serialises `self` into the three-field on-disk format with the given
    /// Unix timestamp.
    fn to_file_string(&self, ts: u64) -> String {
        match self {
            State::Idle => format!("idle::{ts}"),
            State::InProgress(Operation::Full) => format!("in_progress:full:{ts}"),
            State::InProgress(Operation::Incremental) => format!("in_progress:incremental:{ts}"),
            State::InProgress(Operation::S3Upload) => format!("in_progress:s3_upload:{ts}"),
        }
    }

    /// Parses the on-disk three-field format `<status>:<operation>:<timestamp>`.
    ///
    /// The timestamp field is discarded; only the status and operation fields
    /// are used to construct the enum variant.
    fn parse(s: &str) -> anyhow::Result<State> {
        // Expected format: "<status>:<operation>:<timestamp>"
        // For idle: "idle::<ts>"  →  parts[0]="idle", parts[1]="", parts[2]=<ts>
        // For in_progress: "in_progress:full:<ts>"  →  parts[0]="in_progress", etc.
        //
        // We require at least two colon-separated parts (status[:operation]).
        let mut parts = s.splitn(3, ':');
        let status = parts.next().unwrap_or("");
        let operation = parts.next().unwrap_or("");
        // timestamp (third field) is intentionally ignored
        match (status, operation) {
            ("idle", _) => Ok(State::Idle),
            ("in_progress", "full") => Ok(State::InProgress(Operation::Full)),
            ("in_progress", "incremental") => Ok(State::InProgress(Operation::Incremental)),
            ("in_progress", "s3_upload") => Ok(State::InProgress(Operation::S3Upload)),
            _ => anyhow::bail!("unrecognised state value: {:?}", s),
        }
    }
}

/// Last-run timestamps (Unix epoch seconds) for full and incremental backups.
///
/// Persisted as two plain-text files inside `state_dir`:
/// - `last_full_backup`
/// - `last_incremental_backup`
#[derive(Debug, PartialEq, Eq)]
pub struct Timestamps {
    pub(crate) last_full: Option<u64>,
    pub(crate) last_incremental: Option<u64>,
}

impl Timestamps {
    /// Reads both timestamp files from `state_dir`.
    ///
    /// A missing file yields `None` for that field; other errors are propagated.
    pub fn read(state_dir: &Path) -> anyhow::Result<Timestamps> {
        let last_full = read_timestamp(state_dir, "last_full_backup")?;
        let last_incremental = read_timestamp(state_dir, "last_incremental_backup")?;
        Ok(Timestamps {
            last_full,
            last_incremental,
        })
    }

    /// Overwrites `state_dir/last_full_backup` with `ts`.
    pub fn write_full(state_dir: &Path, ts: u64) -> anyhow::Result<()> {
        write_timestamp(state_dir, "last_full_backup", ts)
    }

    /// Overwrites `state_dir/last_incremental_backup` with `ts`.
    pub fn write_incremental(state_dir: &Path, ts: u64) -> anyhow::Result<()> {
        write_timestamp(state_dir, "last_incremental_backup", ts)
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Returns the current time as Unix epoch seconds.
fn current_unix_ts() -> anyhow::Result<u64> {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .context("system clock is before the Unix epoch")
}

/// Writes `content` to `path` atomically via a `.tmp` sibling and `rename(2)`.
///
/// The write is fsynced before the rename to ensure the data reaches disk
/// before the directory entry is updated.
fn atomic_write(path: &Path, content: &str) -> anyhow::Result<()> {
    let tmp_path = path.with_extension("tmp");
    {
        let mut file = File::create(&tmp_path)
            .with_context(|| format!("failed to create tmp file: {}", tmp_path.display()))?;
        file.write_all(content.as_bytes())
            .with_context(|| format!("failed to write tmp file: {}", tmp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to fsync tmp file: {}", tmp_path.display()))?;
    }
    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "failed to rename {} to {}",
            tmp_path.display(),
            path.display()
        )
    })
}

fn read_timestamp(state_dir: &Path, filename: &str) -> anyhow::Result<Option<u64>> {
    let path = state_dir.join(filename);
    match std::fs::read_to_string(&path) {
        Ok(raw) => {
            let trimmed = raw.trim();
            let preview = if trimmed.len() > 64 {
                &trimmed[..64]
            } else {
                trimmed
            };
            let ts: u64 = trimmed.parse().with_context(|| {
                format!(
                    "failed to parse timestamp in {}: {:?}",
                    path.display(),
                    preview
                )
            })?;
            Ok(Some(ts))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => {
            Err(e).with_context(|| format!("failed to read timestamp file: {}", path.display()))
        }
    }
}

fn write_timestamp(state_dir: &Path, filename: &str, ts: u64) -> anyhow::Result<()> {
    let path = state_dir.join(filename);
    atomic_write(&path, &ts.to_string())
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
    // State serialisation / deserialisation
    // -----------------------------------------------------------------------

    #[test]
    fn test_state_parse_idle() {
        assert_eq!(
            State::parse("idle::1706000000").expect("valid"),
            State::Idle
        );
    }

    #[test]
    fn test_state_parse_in_progress_full() {
        assert_eq!(
            State::parse("in_progress:full:1706000000").expect("valid"),
            State::InProgress(Operation::Full)
        );
    }

    #[test]
    fn test_state_parse_in_progress_incremental() {
        assert_eq!(
            State::parse("in_progress:incremental:1706000000").expect("valid"),
            State::InProgress(Operation::Incremental)
        );
    }

    #[test]
    fn test_state_parse_in_progress_s3_upload() {
        assert_eq!(
            State::parse("in_progress:s3_upload:1706000000").expect("valid"),
            State::InProgress(Operation::S3Upload)
        );
    }

    #[test]
    fn test_state_parse_unknown_returns_err() {
        assert!(State::parse("unknown_value::1706000000").is_err());
    }

    #[test]
    fn test_state_to_file_string_round_trip() {
        let ts = 1_706_000_000_u64;
        let cases = [
            State::Idle,
            State::InProgress(Operation::Full),
            State::InProgress(Operation::Incremental),
            State::InProgress(Operation::S3Upload),
        ];
        for state in cases {
            let s = state.to_file_string(ts);
            let parsed = State::parse(&s).expect("round-trip parse");
            assert_eq!(parsed, state, "round-trip failed for {:?}", state);
        }
    }

    /// Verify that the file string for idle embeds an empty operation field.
    #[test]
    fn test_state_to_file_string_idle_format() {
        let s = State::Idle.to_file_string(1_706_000_000);
        assert_eq!(s, "idle::1706000000");
    }

    /// Verify that the file string for in_progress:full matches the spec.
    #[test]
    fn test_state_to_file_string_in_progress_full_format() {
        let s = State::InProgress(Operation::Full).to_file_string(1_706_000_000);
        assert_eq!(s, "in_progress:full:1706000000");
    }

    // -----------------------------------------------------------------------
    // State file I/O
    // -----------------------------------------------------------------------

    #[test]
    fn test_state_read_missing_file_returns_idle() {
        let dir = TempDir::new().expect("temp dir");
        let state = State::read(dir.path()).expect("read should succeed");
        assert_eq!(state, State::Idle);
    }

    #[test]
    fn test_state_write_and_read_idle() {
        let dir = TempDir::new().expect("temp dir");
        State::Idle.write(dir.path()).expect("write");
        let read_back = State::read(dir.path()).expect("read");
        assert_eq!(read_back, State::Idle);
    }

    #[test]
    fn test_state_write_and_read_in_progress_full() {
        let dir = TempDir::new().expect("temp dir");
        State::InProgress(Operation::Full)
            .write(dir.path())
            .expect("write");
        let read_back = State::read(dir.path()).expect("read");
        assert_eq!(read_back, State::InProgress(Operation::Full));
    }

    #[test]
    fn test_state_write_and_read_in_progress_incremental() {
        let dir = TempDir::new().expect("temp dir");
        State::InProgress(Operation::Incremental)
            .write(dir.path())
            .expect("write");
        let read_back = State::read(dir.path()).expect("read");
        assert_eq!(read_back, State::InProgress(Operation::Incremental));
    }

    #[test]
    fn test_state_write_and_read_in_progress_s3_upload() {
        let dir = TempDir::new().expect("temp dir");
        State::InProgress(Operation::S3Upload)
            .write(dir.path())
            .expect("write");
        let read_back = State::read(dir.path()).expect("read");
        assert_eq!(read_back, State::InProgress(Operation::S3Upload));
    }

    #[test]
    fn test_state_read_error_on_invalid_content() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("state");
        std::fs::write(&path, "garbage_value").expect("write garbage");
        assert!(State::read(dir.path()).is_err());
    }

    #[test]
    fn test_state_write_error_on_nonexistent_dir() {
        let result = State::Idle.write(Path::new("/nonexistent/directory"));
        assert!(result.is_err());
    }

    /// Fix 5a: whitespace-padded state file must parse correctly.
    #[test]
    fn test_state_read_whitespace_padded_file() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("state");
        std::fs::write(&path, "idle::1706000000\n").expect("write");
        let state = State::read(dir.path()).expect("read");
        assert_eq!(state, State::Idle);
    }

    /// Fix 5c: sequential state overwrites — last write wins.
    #[test]
    fn test_state_sequential_overwrite() {
        let dir = TempDir::new().expect("temp dir");
        State::InProgress(Operation::Full)
            .write(dir.path())
            .expect("write in_progress:full");
        State::Idle.write(dir.path()).expect("write idle");
        let read_back = State::read(dir.path()).expect("read");
        assert_eq!(read_back, State::Idle);
    }

    // -----------------------------------------------------------------------
    // Timestamps I/O
    // -----------------------------------------------------------------------

    #[test]
    fn test_timestamps_read_both_missing_returns_none() {
        let dir = TempDir::new().expect("temp dir");
        let ts = Timestamps::read(dir.path()).expect("read");
        assert_eq!(
            ts,
            Timestamps {
                last_full: None,
                last_incremental: None
            }
        );
    }

    #[test]
    fn test_timestamps_write_full_and_read() {
        let dir = TempDir::new().expect("temp dir");
        Timestamps::write_full(dir.path(), 1_700_000_000).expect("write");
        let ts = Timestamps::read(dir.path()).expect("read");
        assert_eq!(ts.last_full, Some(1_700_000_000));
        assert_eq!(ts.last_incremental, None);
    }

    #[test]
    fn test_timestamps_write_incremental_and_read() {
        let dir = TempDir::new().expect("temp dir");
        Timestamps::write_incremental(dir.path(), 1_700_001_000).expect("write");
        let ts = Timestamps::read(dir.path()).expect("read");
        assert_eq!(ts.last_full, None);
        assert_eq!(ts.last_incremental, Some(1_700_001_000));
    }

    #[test]
    fn test_timestamps_write_both_and_read() {
        let dir = TempDir::new().expect("temp dir");
        Timestamps::write_full(dir.path(), 1_700_000_000).expect("write full");
        Timestamps::write_incremental(dir.path(), 1_700_001_000).expect("write incr");
        let ts = Timestamps::read(dir.path()).expect("read");
        assert_eq!(
            ts,
            Timestamps {
                last_full: Some(1_700_000_000),
                last_incremental: Some(1_700_001_000),
            }
        );
    }

    #[test]
    fn test_timestamps_write_full_overwrites_previous() {
        let dir = TempDir::new().expect("temp dir");
        Timestamps::write_full(dir.path(), 1_000).expect("write first");
        Timestamps::write_full(dir.path(), 2_000).expect("write second");
        let ts = Timestamps::read(dir.path()).expect("read");
        assert_eq!(ts.last_full, Some(2_000));
    }

    #[test]
    fn test_timestamps_read_invalid_content_returns_err() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("last_full_backup");
        std::fs::write(&path, "not_a_number").expect("write garbage");
        assert!(Timestamps::read(dir.path()).is_err());
    }

    #[test]
    fn test_timestamps_write_error_on_nonexistent_dir() {
        let result = Timestamps::write_full(Path::new("/nonexistent/directory"), 42);
        assert!(result.is_err());
    }

    /// Fix 5b: whitespace-padded timestamp file must parse correctly.
    #[test]
    fn test_timestamps_read_whitespace_padded_file() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("last_full_backup");
        std::fs::write(&path, "1700000000\n").expect("write");
        let ts = Timestamps::read(dir.path()).expect("read");
        assert_eq!(ts.last_full, Some(1_700_000_000));
    }
}

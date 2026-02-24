# Implementation Tasks

## Checklist

- [ ] 1. Scaffold the Cargo project
- [ ] 2. Config parsing (`src/config.rs`)
- [ ] 3. State management (`src/state.rs`)
- [ ] 4. Startup validation (`src/validation.rs`)
- [ ] 5. External command helpers (`src/cmd.rs`)
- [ ] 6. Snapshot naming utilities (`src/snapshot.rs`)
- [ ] 7. Full backup (`src/backup/full.rs`)
- [ ] 8. Incremental backup (`src/backup/incremental.rs`)
- [ ] 9. S3 upload (`src/backup/s3.rs`)
- [ ] 10. Interruption recovery (`src/recovery.rs`)
- [ ] 11. Backup decision logic (`src/scheduler.rs`)
- [ ] 12. Main entry point and scheduler loop (`src/main.rs`)
- [ ] 13. Logging setup
- [ ] 14. systemd unit file
- [ ] 15. Example config file
- [ ] 16. Integration smoke test (optional, manual)

---

Tasks are ordered by dependency. Complete them top-to-bottom. Each task references the relevant section of `DESIGN.md`.

---

## 1. Scaffold the Cargo project

- Run `cargo init --name btrshot` to create `Cargo.toml` and `src/main.rs`.
- Add all required dependencies to `Cargo.toml`:
  - `tokio` (features: `rt-multi-thread`, `macros`, `time`, `signal`, `process`)
  - `serde` (features: `derive`)
  - `toml`
  - `tracing`
  - `tracing-subscriber` (features: `env-filter`)
  - `sd-notify` (for `Type=notify` systemd integration)
  - `anyhow` (for ergonomic error propagation)
- Verify `cargo build` succeeds before proceeding.

---

## 2. Config parsing (`src/config.rs`)

**Reference**: DESIGN.md § Configuration

- Define `Config` struct (and nested `PathsConfig`, `S3Config`, `GpgConfig`, `ScheduleConfig`, `StateConfig`) with `#[derive(Deserialize)]`.
- Fields:
  - `paths.source_path: PathBuf`
  - `paths.source_subvolume: String`
  - `paths.backup_path: PathBuf`
  - `s3.bucket: String`
  - `s3.retention_count: usize`
  - `s3.aws_profile: Option<String>`
  - `gpg.public_key_file: PathBuf`
  - `schedule.check_interval: u64` (seconds)
  - `schedule.full_backup_interval: u64` (seconds)
  - `schedule.incremental_interval: u64` (seconds)
  - `state.state_dir: PathBuf`
- Implement `Config::load(path: &Path) -> anyhow::Result<Config>` that reads and parses the TOML file.
- Add a `Config::source_subvolume_path(&self) -> PathBuf` helper returning `source_path/source_subvolume`.

---

## 3. State management (`src/state.rs`)

**Reference**: DESIGN.md § State File Format

- Define enum `Operation { Full, Incremental, S3Upload }` and enum `State { Idle, InProgress(Operation) }`.
- Implement:
  - `State::read(state_dir: &Path) -> anyhow::Result<State>` — reads `state_dir/state`; treats missing file as `Idle`.
  - `State::write(state_dir: &Path) -> anyhow::Result<()>` — writes canonical string to `state_dir/state`.
- Define `Timestamps` with fields `last_full: Option<u64>` and `last_incremental: Option<u64>` (Unix epoch seconds).
- Implement:
  - `Timestamps::read(state_dir: &Path) -> anyhow::Result<Timestamps>` — reads `last_full_backup` and `last_incremental_backup` files; missing file → `None`.
  - `Timestamps::write_full(state_dir: &Path, ts: u64) -> anyhow::Result<()>`
  - `Timestamps::write_incremental(state_dir: &Path, ts: u64) -> anyhow::Result<()>`

---

## 4. Startup validation (`src/validation.rs`)

**Reference**: DESIGN.md § Validation, § Mount validation

- Implement `validate_source(subvolume_path: &Path) -> anyhow::Result<()>`:
  - Run `btrfs subvolume show <subvolume_path>`.
  - Return `Err` with a descriptive message if the command fails or the path is not a subvolume.
- Implement `validate_backup_fs(backup_path: &Path) -> anyhow::Result<()>`:
  - Run `btrfs filesystem show <backup_path>` (or check `/proc/mounts` for `btrfs` type).
  - Return `Err` with a descriptive message if Disk B is not btrfs.
- Implement `validate_all(config: &Config) -> anyhow::Result<()>` that calls both validators.

---

## 5. External command helpers (`src/cmd.rs`)

**Reference**: DESIGN.md § External commands

- Implement `run(program: &str, args: &[&str]) -> anyhow::Result<()>`:
  - Uses `std::process::Command` (blocking; called from a `tokio::task::spawn_blocking` wrapper where needed).
  - Captures stdout/stderr; logs them at `DEBUG`/`WARN` level.
  - Returns `Err` if exit status is non-zero, including captured stderr in the error message.
- Implement `pipe(steps: &[(&str, &[&str])]) -> anyhow::Result<()>` for chaining two or three commands with piped stdout→stdin (used for `btrfs send | btrfs receive` and `tar | gpg | aws`).
  - Each step is `(program, args)`.
  - All processes are spawned; stdout of step N is connected to stdin of step N+1.
  - Wait for all processes; return `Err` if any exits non-zero.

---

## 6. Snapshot naming utilities (`src/snapshot.rs`)

**Reference**: DESIGN.md § Directory Structure, § Snapshot naming

- Implement `timestamp_now() -> String` returning `YYYYMMDD_HHMMSS` from the current UTC time.
- Implement `full_snapshot_name(ts: &str) -> String` → `"full_{ts}"`.
- Implement `incr_snapshot_name(ts: &str) -> String` → `"incr_{ts}"`.
- Constants / helpers for:
  - Temp snapshot name on A: `.snap_tmp`
  - Base snapshot name on A: `.snap_base_full`
  - Snapshots directory on B: `{backup_path}/snapshots/`
  - `current` symlink path: `{backup_path}/current`

---

## 7. Full backup (`src/backup/full.rs`)

**Reference**: DESIGN.md § Full Backup Process

Implement `run_full_backup(config: &Config) -> anyhow::Result<()>`:

1. Write state `in_progress:full`.
2. Create read-only snapshot:
   `btrfs subvolume snapshot -r <source_subvolume> <source_path>/.snap_tmp`
3. Send to Disk B:
   pipe `btrfs send <source_path>/.snap_tmp` → `btrfs receive <backup_path>/snapshots/`
4. Rename received snapshot (btrfs names it after `.snap_tmp`; rename to `full_<ts>/`).
5. Update `current` symlink to point to the new full snapshot.
6. Delete old snapshots on B (everything except the new full snapshot and its incrementals — at this point after a fresh full, only the new full exists, so delete all others).
7. On A: remove previous `.snap_base_full` (if exists); rename `.snap_tmp` → `.snap_base_full`.
8. Call `run_s3_upload(config, snapshot_name)` for the new full snapshot.
9. On success: update `last_full_backup` timestamp; write state `idle`.

---

## 8. Incremental backup (`src/backup/incremental.rs`)

**Reference**: DESIGN.md § Incremental Backup Process

Implement `run_incremental_backup(config: &Config) -> anyhow::Result<()>`:

1. Write state `in_progress:incremental`.
2. Create read-only snapshot:
   `btrfs subvolume snapshot -r <source_subvolume> <source_path>/.snap_tmp`
3. Send incremental:
   pipe `btrfs send -p <source_path>/.snap_base_full <source_path>/.snap_tmp` → `btrfs receive <backup_path>/snapshots/`
4. Rename received snapshot to `incr_<ts>/`.
5. On A: delete `.snap_base_full`; rename `.snap_tmp` → `.snap_base_full`.
6. Update `last_incremental_backup` timestamp.
7. Write state `idle`.

---

## 9. S3 upload (`src/backup/s3.rs`)

**Reference**: DESIGN.md § S3 Upload Process

Implement `run_s3_upload(config: &Config, snapshot_name: &str) -> anyhow::Result<()>`:

1. Write state `in_progress:s3_upload`.
2. Stream-upload the snapshot:
   pipe `tar -cf - -C <backup_path>/snapshots <snapshot_name>/`
   → `gpg --encrypt --recipient-file <public_key_file>`
   → `aws s3 cp - s3://<bucket>/<snapshot_name>.tar.gpg`
3. Enforce S3 retention:
   - List objects in the bucket prefix with `aws s3 ls s3://<bucket>/`.
   - Parse names, sort by timestamp (embedded in name), delete oldest beyond `retention_count`.
   - Delete each excess object: `aws s3 rm s3://<bucket>/<object>`.
4. Write state `idle`.

---

## 10. Interruption recovery (`src/recovery.rs`)

**Reference**: DESIGN.md § Cleanup on Interruption Detection

Implement `recover_if_needed(config: &Config) -> anyhow::Result<()>`:

- Read state file.
- If `in_progress:full` or `in_progress:incremental`:
  - Delete `<source_path>/.snap_tmp` if it exists (`btrfs subvolume delete`).
  - Delete any snapshot in `<backup_path>/snapshots/` whose name matches `.snap_tmp` (i.e. a partially received snapshot).
  - Write state `idle`.
- If `in_progress:s3_upload`:
  - Abort incomplete multipart uploads:
    `aws s3api list-multipart-uploads --bucket <bucket>` then `aws s3api abort-multipart-upload` for each.
  - Write state `idle`.
- If `idle` or file missing: no-op.

---

## 11. Backup decision logic (`src/scheduler.rs`)

**Reference**: DESIGN.md § Decision Flow

Implement `async fn run_check(config: &Config) -> anyhow::Result<()>`:

1. Call `recover_if_needed(config)`.
2. Read `Timestamps`.
3. Current Unix time → `now`.
4. If `last_full` is `None` or `now - last_full >= full_backup_interval`:
   - Call `run_full_backup(config)`.
   - Return (full backup also handles any pending incremental).
5. Else if `last_incremental` is `None` or `now - last_incremental >= incremental_interval`:
   - Call `run_incremental_backup(config)`.
6. Else: log `INFO` "Nothing to do".

---

## 12. Main entry point and scheduler loop (`src/main.rs`)

**Reference**: DESIGN.md § Daemon Scheduler Loop, § Graceful Shutdown

- Parse CLI argument `--config <path>` (use `std::env::args()` or the `clap` crate).
- Initialize `tracing_subscriber` (plain-text lines, level from `RUST_LOG` env var, default `INFO`).
- Load config via `Config::load`.
- Create state directory if it doesn't exist (`std::fs::create_dir_all`).
- Call `validate_all(config)` — exit with non-zero code and error message on failure.
- Notify systemd readiness: `sd_notify::notify(false, &[sd_notify::NotifyState::Ready])`.
- Enter `tokio::runtime::Runtime` (or `#[tokio::main]`):
  ```
  let mut interval = tokio::time::interval(Duration::from_secs(config.schedule.check_interval));
  interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
  loop {
      tokio::select! {
          _ = interval.tick() => { run_check(&config).await }
          _ = shutdown_signal() => { break }
      }
  }
  ```
- `shutdown_signal()` listens for SIGTERM and SIGINT (`tokio::signal`).
- Log `INFO "Shutting down"` on exit.

---

## 13. Logging setup

**Reference**: DESIGN.md § Logging

- In `main.rs`, configure `tracing_subscriber::fmt` with:
  - No ANSI colors (journald strips them anyway).
  - Level filter from `RUST_LOG` environment variable, defaulting to `INFO`.
- Use `tracing::{info!, warn!, error!, debug!}` macros throughout all modules.
- Key log events to instrument:
  - Scheduler tick: `INFO "Scheduler tick"`
  - Backup start/end: `INFO "Starting full backup"`, `INFO "Full backup complete"`
  - S3 upload start/end.
  - Recovery actions: `WARN "Interruption detected: {state}; cleaning up"`
  - Validation failure: `ERROR "Validation failed: {reason}"`
  - Shutdown: `INFO "Received shutdown signal"`

---

## 14. systemd unit file

**Reference**: DESIGN.md § systemd Unit

- Create `btrshot.service` at the repository root (for packaging/installation reference):

```ini
[Unit]
Description=btrshot backup daemon (btrfs snapshot to local + S3)
After=network-online.target
Wants=network-online.target
ConditionPathIsMountPoint=/path/to/A
ConditionPathIsMountPoint=/path/to/B

[Service]
Type=notify
ExecStart=/usr/local/bin/btrshot --config /etc/btrshot/config.toml
Restart=on-failure
RestartSec=60
EnvironmentFile=-/etc/btrshot/aws.env

[Install]
WantedBy=multi-user.target
```

- Note: `ConditionPathIsMountPoint` lines must be customised to actual mount paths during installation.

---

## 15. Example config file

**Reference**: DESIGN.md § Configuration

- Create `config.example.toml` at the repository root with all fields documented via inline comments.

---

## 16. Integration smoke test (optional, manual)

Not automated (requires real btrfs disks and AWS credentials), but document the manual test procedure in `README.md`:

1. Prepare two btrfs filesystems (can use loopback devices).
2. Create a subvolume on the source.
3. Place a test file in the subvolume.
4. Run `btrshot --config test.toml` with `check_interval = 5`.
5. Verify snapshot appears on Disk B and in S3.
6. Modify the test file; wait for incremental.
7. Send SIGTERM; verify clean exit and `state` file reads `idle`.

---

## Dependency order summary

```
1 (scaffold)
└─ 2 (config)
   └─ 3 (state)
      ├─ 4 (validation)
      ├─ 5 (cmd helpers)
      │  └─ 6 (snapshot naming)
      │     ├─ 7 (full backup)   ─┐
      │     ├─ 8 (incremental)   ─┤─ 9 (S3 upload) ─┐
      │     └─ 10 (recovery)     ─┘                  │
      │                                               │
      └─ 11 (scheduler) ◄─────────────────────────────┘
         └─ 12 (main / loop)
            └─ 13 (logging, wired throughout)
               └─ 14 (service file)
                  └─ 15 (example config)
```

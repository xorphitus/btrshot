# Backup System Design

## Overview

A btrfs-based backup system that performs incremental backups from disk A to disk B, with encrypted offsite storage to Amazon S3. Disk A retains a read-only parent snapshot to support incrementals; Disk B only stores received backups. Offsite artifacts are encrypted tar archives uploaded per snapshot (not bundled), optimized for straightforward file recovery.

## Requirements Summary

| Item | Specification |
|------|---------------|
| Source | btrfs disk A (mounted at configurable path) |
| Local backup destination | btrfs disk B (mounted at configurable path) |
| Remote backup destination | Amazon S3 (encrypted) |
| Full backup interval | Every 7 days |
| Incremental backup interval | Every 24 hours |
| Local retention | Latest full backup + incremental snapshots since then |
| S3 retention | 10 most recent offsite snapshot objects (uploaded separately) |
| Encryption | GPG (asymmetric, public key) |
| Implementation | Rust (long-running daemon) |

## Prerequisites

### btrfs Subvolume Requirement

The source data on Disk A **must be organized as a btrfs subvolume**, not a regular directory. This is a fundamental btrfs constraint:

- **Snapshots**: btrfs can only create snapshots of subvolumes, not arbitrary directories
- **Send/Receive**: The `btrfs send` command only operates on read-only snapshots of subvolumes
- **Atomic consistency**: Subvolume snapshots provide point-in-time consistency guarantees

#### Initial Setup

If your data is not already in a subvolume, create one and move your data:

```bash
# Create subvolume
btrfs subvolume create /path/to/A/data

# Move existing data into the subvolume (includes hidden files)
mv /path/to/A/existing_data/. /path/to/A/data/
```

#### Verify Subvolume

```bash
# List subvolumes on the filesystem
btrfs subvolume list /path/to/A

# Check if a path is a subvolume
btrfs subvolume show /path/to/A/data
```

### Disk B Requirements

Disk B must also be a btrfs filesystem to receive snapshots via `btrfs receive`.

### Validation

The daemon validates these prerequisites at startup before entering the scheduler loop:

1. Source data path is a btrfs subvolume (via `btrfs subvolume show`)
2. Disk B is a btrfs filesystem

If validation fails, the daemon exits with an error message explaining the issue.

### Source Snapshot Retention

Disk A keeps a single read-only base snapshot (`.snap_base_full`) after each full backup. Each incremental uses this as its parent; upon success the new snapshot replaces it. The base is rotated only when the next full backup is taken.

## Architecture

### Components

```
┌─────────────────────────────────────────────────────────────────┐
│                   systemd service (daemon)                      │
└─────────────────────────────────────────────────────────────────┘
                                 │
                                 ▼
┌─────────────────────────────────────────────────────────────────┐
│                    btrshot (Rust daemon)                        │
│  - Internal scheduler loop (tokio async runtime)                │
│  - Check last backup timestamps every CHECK_INTERVAL            │
│  - Determine action (full/incremental/none)                     │
│  - Execute backup via external commands                         │
│  - Handle interruption recovery                                 │
│  - Graceful shutdown on SIGTERM/SIGINT                          │
└─────────────────────────────────────────────────────────────────┘
                                 │
              ┌──────────────────┼──────────────────┐
              ▼                  ▼                  ▼
        ┌──────────┐      ┌──────────┐      ┌──────────┐
        │  Disk A  │      │  Disk B  │      │   S3     │
        │ (source) │ ───► │ (local)  │ ───► │ (remote) │
        └──────────┘      └──────────┘      └──────────┘
              btrfs send/receive      GPG + aws s3 cp
```

The daemon replaces both the systemd timer and the oneshot script. It manages its own schedule using `tokio::time` and runs indefinitely until signalled to stop.

### Rust Crate Overview

| Crate | Purpose |
|-------|---------|
| `tokio` | Async runtime, internal timer (`tokio::time::interval`) |
| `serde` + `toml` | Config file parsing |
| `tracing` + `tracing-subscriber` | Structured logging (journald-compatible) |
| `tokio::signal` | SIGTERM/SIGINT handling for graceful shutdown |
| `std::process::Command` | Spawn external processes (`btrfs`, `gpg`, `aws`) |

### Directory Structure

```
/etc/btrshot/
└── config.toml                 # Configuration file

/var/lib/btrshot/               # State directory
├── state                       # Current operation state (for interruption detection)
├── last_full_backup            # Timestamp of last full backup
└── last_incremental_backup     # Timestamp of last incremental backup

Disk A (source):
/path/to/A/
└── data/                       # Actual data (subvolume)

Disk B (backup destination):
/path/to/B/
├── snapshots/
│   ├── full_YYYYMMDD_HHMMSS/   # Full backup snapshot
│   ├── incr_YYYYMMDD_HHMMSS/   # Incremental snapshot
│   └── ...
└── current -> snapshots/full_YYYYMMDD_HHMMSS/  # Symlink to latest full
```

## Configuration

Configuration file: `/etc/btrshot/config.toml`

```toml
[paths]
source_path = "/path/to/A"
source_subvolume = "data"
backup_path = "/path/to/B"

[s3]
bucket = "s3://your-bucket-name/backups"
retention_count = 10
# aws_profile = "backup-profile"   # optional; omit to use instance role or env vars

[gpg]
public_key_file = "/path/to/backup-key.pub"

[schedule]
# How often the daemon wakes up to check if a backup is due (in seconds)
check_interval = 7200          # 2 hours
full_backup_interval = 604800  # 7 days
incremental_interval = 86400   # 24 hours

[state]
state_dir = "/var/lib/btrshot"
```

AWS credentials are read from environment variables (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`) or `AWS_PROFILE` at runtime, consistent with standard AWS SDK conventions.

## Daemon Scheduler Loop

The daemon runs a single async loop on a `tokio` runtime:

```
Startup
  │
  ▼
Validate prerequisites (btrfs subvolumes, mounts)
  │
  ▼
┌─────────────────────────────────────────────────────┐
│  tokio::select! {                                   │
│    _ = check_interval.tick() => { run_check() }     │
│    _ = shutdown_signal => { graceful_shutdown() }   │
│  }                                                  │
└─────────────────────────────────────────────────────┘
```

`run_check()` is called every `check_interval` (default 2 h) and performs the backup decision logic. The interval fires immediately on the first tick so a backup is evaluated at startup.

## Backup Process

### Decision Flow

```
run_check()
  │
  ▼
Check state file
  │
  ├─ "in_progress" ──► Cleanup incomplete backup ──┐
  │                                                │
  ▼                                                │
Check last_full_backup timestamp ◄─────────────────┘
  │
  ├─ >= 7 days ago (or not exists) ──► Execute FULL backup
  │
  ▼
Check last_incremental_backup timestamp
  │
  ├─ >= 24 hours ago ──► Execute INCREMENTAL backup
  │
  ▼
Nothing to do, sleep until next tick
```

### Full Backup Process

1. Write state: `in_progress:full`
2. Create read-only snapshot of source subvolume
   ```bash
   btrfs subvolume snapshot -r /path/to/A/data /path/to/A/.snapshot_temp
   ```
3. Send snapshot to disk B
   ```bash
   btrfs send /path/to/A/.snapshot_temp | btrfs receive /path/to/B/snapshots/
   ```
4. Rename received snapshot with timestamp
5. Update `current` symlink
6. Delete old snapshots from B (keep only current full + its incrementals)
7. On Disk A, keep the read-only snapshot as the new incremental base (rename to `/path/to/A/.snap_base_full`)
8. Trigger S3 upload process for the new full snapshot
9. Only after S3 upload succeeds, update `last_full_backup` timestamp
10. Write state: `idle`

### Incremental Backup Process

1. Write state: `in_progress:incremental`
2. Create read-only snapshot of source subvolume (`/path/to/A/.snap_tmp`)
3. Send incremental snapshot using the retained base snapshot on A as parent
   ```bash
   btrfs send -p /path/to/A/.snap_base_full /path/to/A/.snap_tmp | \
       btrfs receive /path/to/B/snapshots/
   ```
4. Rename received snapshot with timestamp
5. Delete old base snapshot on A, rename `.snap_tmp` to become the new base parent for the next incremental
6. Update `last_incremental_backup` timestamp
7. Write state: `idle`

### S3 Upload Process

1. Write state: `in_progress:s3_upload`
2. Create tar stream for one snapshot at a time (full or incremental; no bundling)
   ```bash
   tar -cf - -C /path/to/B/snapshots full_YYYYMMDD_HHMMSS/
   ```
3. Encrypt with GPG
   ```bash
   gpg --encrypt --recipient-file /path/to/key.pub
   ```
4. Upload to S3
   ```bash
   tar ... | gpg ... | aws s3 cp - s3://bucket/snapshots/full_YYYYMMDD_HHMMSS.tar.gpg
   ```
5. Upload incremental snapshots as separate objects:
   ```bash
   tar -cf - -C /path/to/B/snapshots incr_YYYYMMDD_HHMMSS/ | \
       gpg --encrypt --recipient-file /path/to/key.pub | \
       aws s3 cp - s3://bucket/snapshots/incr_YYYYMMDD_HHMMSS.tar.gpg
   ```
6. Delete old backups from S3 (keep latest 10 uploaded snapshot objects)
7. Write state: `idle`

Notes:
- Offsite uploads are intentionally not bundled for operational simplicity.
- A full backup run is considered successful only after both local backup and required S3 upload finish successfully.
- External processes (`btrfs`, `gpg`, `aws`) are spawned via `std::process::Command` with piped stdio to chain streams without buffering to disk.

## Interruption Handling

### Graceful Shutdown

On SIGTERM or SIGINT, the daemon:
1. Stops accepting new scheduler ticks
2. Waits for any in-flight backup operation to finish its current step (or times out and marks state as interrupted)
3. Exits cleanly

systemd sends SIGTERM on `systemctl stop btrshot`, so in-progress backups complete before shutdown when possible.

### State File Format

```
<status>:<operation>:<timestamp>
```

Examples:
- `idle::1706000000`
- `in_progress:full:1706000000`
- `in_progress:incremental:1706000000`
- `in_progress:s3_upload:1706000000`

### Cleanup on Interruption Detection

When the daemon starts (or after an unclean shutdown) and finds `in_progress` state:

1. **Full backup interrupted**:
   - Delete incomplete snapshot on B (if exists)
   - Delete temporary snapshot on A (if exists)
   - Reset state to `idle`
   - Re-evaluate what backup is needed

2. **Incremental backup interrupted**:
   - Delete incomplete snapshot on B (if exists)
   - Delete temporary snapshot on A (if exists)
   - Reset state to `idle`
   - Re-evaluate what backup is needed

3. **S3 upload interrupted**:
   - Abort any multipart uploads (`aws s3api abort-multipart-upload`)
   - Reset state to `idle`
   - Retry upload (full backup on B is intact)

## Logging

The daemon logs via the `tracing` crate to stdout/stderr, captured by the systemd journal. The `tracing-subscriber` is configured to emit plain-text lines with level prefixes compatible with journald level detection.

Log levels:
- `INFO` - Normal operation and scheduler ticks
- `WARN` - Recoverable issues
- `ERROR` - Failures requiring attention

View logs:
```bash
journalctl -u btrshot.service
journalctl -u btrshot.service -f   # follow
```

## systemd Unit

Only a single service unit is needed. The daemon manages its own schedule internally.

### btrshot.service

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

`Type=notify` enables the daemon to signal systemd via `sd_notify` (using the `libsystemd` or `sd-notify` crate) once it has finished startup validation and entered the scheduler loop. `Restart=on-failure` ensures the daemon is restarted if it crashes.

`/etc/btrshot/aws.env` optionally holds `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, or `AWS_PROFILE` (mode `0600`).

## File List

| File | Description |
|------|-------------|
| `/usr/local/bin/btrshot` | Compiled Rust daemon binary |
| `/etc/btrshot/config.toml` | Configuration file |
| `/etc/btrshot/aws.env` | AWS credentials environment file (optional) |
| `/etc/systemd/system/btrshot.service` | systemd service unit |

## Security Considerations

1. **Configuration file permissions**: `/etc/btrshot/config.toml` and `aws.env` should be readable only by root
   ```bash
   chmod 600 /etc/btrshot/config.toml /etc/btrshot/aws.env
   ```

2. **GPG key**: Public key only needed for encryption; private key should be stored securely offline for recovery

3. **AWS credentials**: Prefer instance roles or `AWS_PROFILE` over hardcoded credentials in `aws.env`

4. **S3 bucket policy**: Restrict access, enable versioning and server-side encryption as additional protection

5. **No external locking needed**: Because btrshot is a single persistent daemon, only one backup operation runs at a time by design. No `flock` is required.

6. **Mount validation**: At startup and before each backup, verify `/path/to/A` and `/path/to/B` are mounted btrfs filesystems at the expected paths; abort if mounts are missing to avoid writing to the wrong disk.

## Recovery Procedure

To restore files from S3:

1. Download encrypted backup
   ```bash
   aws s3 cp s3://bucket/snapshots/full_YYYYMMDD_HHMMSS.tar.gpg ./
   ```

2. Decrypt with private key
   ```bash
   gpg --decrypt full_YYYYMMDD_HHMMSS.tar.gpg > backup.tar
   ```

3. Extract to a restore directory
   ```bash
   mkdir -p /path/to/restore && tar -xf backup.tar -C /path/to/restore
   ```

4. Recover needed files from `/path/to/restore`

Optional: If you want restored data back on btrfs as a new subvolume:
```bash
btrfs subvolume create /path/to/target_subvol
rsync -aHAX /path/to/restore/full_YYYYMMDD_HHMMSS/ /path/to/target_subvol/
```

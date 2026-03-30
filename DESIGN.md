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
| Implementation | Bash (systemd timer) |

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

The script validates these prerequisites at startup before proceeding:

1. Source data path is a btrfs subvolume (via `btrfs subvolume show`)
2. Disk B is a btrfs filesystem

If validation fails, the script exits with an error message explaining the issue.

### Source Snapshot Retention

Disk A keeps a single read-only base snapshot (`.snap_base_full`) after each full backup. Each incremental uses this as its parent; upon success the new snapshot replaces it. The base is rotated only when the next full backup is taken.

## Architecture

### Components

```
┌─────────────────────────────────────────────────────────────────┐
│                   btrshot.timer (systemd timer)                 │
│  OnBootSec=5min, OnUnitActiveSec=2h                             │
└─────────────────────────────────────────────────────────────────┘
                                 │ fires
                                 ▼
┌─────────────────────────────────────────────────────────────────┐
│             btrshot.service (Type=oneshot)                      │
│             ExecStart=/usr/local/bin/btrshot.sh                 │
└─────────────────────────────────────────────────────────────────┘
                                 │
                                 ▼
┌─────────────────────────────────────────────────────────────────┐
│                         btrshot.sh                              │
│  - Validate prerequisites                                       │
│  - Recover from interrupted previous run (state file)           │
│  - Determine action (full / incremental / none)                 │
│  - Execute backup via btrfs, gpg, aws CLI                       │
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

The systemd timer replaces an internal sleep loop. Each timer tick launches `btrshot.sh` as a short-lived oneshot process that decides what (if anything) to back up, performs the work, then exits.

### Directory Structure

```
/etc/btrshot/
├── btrshot.conf                # Configuration file (bash-sourceable)
└── aws.env                     # AWS credentials (optional)

/usr/local/bin/
└── btrshot.sh                  # Main backup script

/var/lib/btrshot/               # State directory
├── state                       # Current operation state (for interruption detection)
├── last_full_backup            # Timestamp of last full backup (Unix epoch)
└── last_incremental_backup     # Timestamp of last incremental backup (Unix epoch)

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

Configuration file: `/etc/btrshot/btrshot.conf` (sourced by the script)

```bash
SOURCE_PATH="/path/to/A"
SOURCE_SUBVOLUME="data"
BACKUP_PATH="/path/to/B"

S3_BUCKET="s3://your-bucket-name/backups"
S3_RETENTION_COUNT=10
# AWS_PROFILE="backup-profile"   # optional; omit to use instance role or env vars

GPG_PUBLIC_KEY_FILE="/path/to/backup-key.pub"

FULL_BACKUP_INTERVAL=604800   # 7 days in seconds
INCREMENTAL_INTERVAL=86400    # 24 hours in seconds

STATE_DIR="/var/lib/btrshot"
```

The timer interval (`OnUnitActiveSec=` in `btrshot.timer`) is the check frequency and is configured in the systemd unit, not in `btrshot.conf`.

AWS credentials are read from environment variables (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`) or `AWS_PROFILE` at runtime, consistent with standard AWS SDK conventions.

## Timer-based Execution

The systemd timer fires `btrshot.service` periodically (default every 2 hours). Each invocation runs `btrshot.sh` end-to-end:

```
Timer fires
  │
  ▼
btrshot.sh starts
  │
  ▼
Source config, validate prerequisites
  │
  ▼
Recover from interrupted previous run (if state ≠ idle)
  │
  ▼
Decision: full / incremental / nothing
  │
  ▼
Execute backup, update state file, exit 0
```

The script exits with a non-zero code on unrecoverable failure, which systemd records in the journal.

## Backup Process

### Decision Flow

```
btrshot.sh
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
Nothing to do, exit 0
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
2. Stream-upload one snapshot at a time (no bundling)
   ```bash
   tar -cf - -C /path/to/B/snapshots full_YYYYMMDD_HHMMSS/ | \
       gpg --encrypt --recipient-file /path/to/key.pub | \
       aws s3 cp - s3://bucket/snapshots/full_YYYYMMDD_HHMMSS.tar.gpg
   ```
3. Upload incremental snapshots as separate objects:
   ```bash
   tar -cf - -C /path/to/B/snapshots incr_YYYYMMDD_HHMMSS/ | \
       gpg --encrypt --recipient-file /path/to/key.pub | \
       aws s3 cp - s3://bucket/snapshots/incr_YYYYMMDD_HHMMSS.tar.gpg
   ```
4. Delete old backups from S3 (keep latest 10 uploaded snapshot objects)
5. Write state: `idle`

Notes:
- Offsite uploads are intentionally not bundled for operational simplicity.
- A full backup run is considered successful only after both local backup and required S3 upload finish successfully.
- External processes (`btrfs`, `gpg`, `aws`) are chained via bash pipes.

## Interruption Handling

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

Since each run is a oneshot, finding `in_progress` state at script startup means the previous run was killed mid-operation (e.g., host shutdown, OOM kill, or manual `systemctl stop`).

When `btrshot.sh` starts and finds `in_progress` state:

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

The script writes log messages to stdout/stderr, which are captured by the systemd journal via the service unit (`StandardOutput=journal`). Alternatively, use `logger -t btrshot` for direct syslog submission.

View logs:
```bash
journalctl -u btrshot.service
journalctl -u btrshot.service -f   # follow
```

## systemd Units

### btrshot.timer

```ini
[Unit]
Description=btrshot backup timer

[Timer]
OnBootSec=5min
OnUnitActiveSec=2h
Unit=btrshot.service

[Install]
WantedBy=timers.target
```

### btrshot.service

```ini
[Unit]
Description=btrshot backup (btrfs snapshot to local + S3)
After=network-online.target
Wants=network-online.target
ConditionPathIsMountPoint=/path/to/A
ConditionPathIsMountPoint=/path/to/B

[Service]
Type=oneshot
ExecStart=/usr/local/bin/btrshot.sh
StandardOutput=journal
StandardError=journal
EnvironmentFile=-/etc/btrshot/aws.env
```

`ConditionPathIsMountPoint` lines must be customised to actual mount paths during installation.

Enable the timer (not the service directly):
```bash
systemctl enable --now btrshot.timer
```

## File List

| File | Description |
|------|-------------|
| `/usr/local/bin/btrshot.sh` | Main backup script |
| `/etc/btrshot/btrshot.conf` | Configuration file |
| `/etc/btrshot/aws.env` | AWS credentials environment file (optional) |
| `/etc/systemd/system/btrshot.service` | systemd oneshot service unit |
| `/etc/systemd/system/btrshot.timer` | systemd timer unit |

## Security Considerations

1. **Configuration file permissions**: `/etc/btrshot/btrshot.conf` and `aws.env` should be readable only by root
   ```bash
   chmod 600 /etc/btrshot/btrshot.conf /etc/btrshot/aws.env
   ```

2. **GPG key**: Public key only needed for encryption; private key should be stored securely offline for recovery

3. **AWS credentials**: Prefer instance roles or `AWS_PROFILE` over hardcoded credentials in `aws.env`

4. **S3 bucket policy**: Restrict access, enable versioning and server-side encryption as additional protection

5. **Concurrent runs**: systemd's `Type=oneshot` serializes runs naturally; if a previous run is still active when the timer fires, systemd will queue or skip it. Use `flock` on the state file as an additional guard if running outside systemd.

6. **Mount validation**: Before each backup, verify `/path/to/A` and `/path/to/B` are mounted btrfs filesystems at the expected paths; abort if mounts are missing to avoid writing to the wrong disk.

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

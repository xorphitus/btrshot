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

## Automated Testing

### Overview

Integration tests run inside a Docker container to provide an isolated environment with real btrfs filesystems and a local S3-compatible endpoint (MinIO). This avoids polluting the host and allows safe use of privileged btrfs operations.

### Container Requirements

The Docker image must include:

| Package | Purpose |
|---------|---------|
| `btrfs-progs` | `btrfs subvolume`, `btrfs send/receive` |
| `gnupg` | GPG encryption |
| `awscli` (v2) | S3 upload/download/retention |
| `minio` | Local S3-compatible object store |
| `util-linux` | `losetup`, `mount` |
| `coreutils`, `bash` | Script runtime |
| `tar` | Archive creation |

### Container Setup

The test harness script (`test/run.sh`) performs:

1. **Build image** — Build (or reuse cached) the Docker image from `test/Dockerfile`.

2. **Launch container** — Run the container with `--privileged` for btrfs and loopback device access:
   ```bash
   docker run --rm --privileged \
       -v "$PROJECT_DIR:/opt/btrshot:ro" \
       btrshot-test
   ```
   - `--privileged` — required for btrfs subvolume operations, mounting loopback devices, and creating filesystems.
   - `-v ... :ro` — mounts the project directory read-only so the container can access the script and test code.

3. **Exit code** — The container's exit code is propagated as the test suite result.

### Test Environment Initialization

Inside the container, `test/entrypoint.sh` sets up the sandbox:

```
1. Create two loopback btrfs images (512 MB each)
   truncate -s 512M /tmp/disk_a.img /tmp/disk_b.img
   mkfs.btrfs /tmp/disk_a.img && mkfs.btrfs /tmp/disk_b.img
   mount -o loop /tmp/disk_a.img /mnt/A
   mount -o loop /tmp/disk_b.img /mnt/B

2. Create source subvolume with seed data
   btrfs subvolume create /mnt/A/data
   echo "seed" > /mnt/A/data/file1.txt

3. Generate a throwaway GPG key pair (no passphrase)
   gpg --batch --gen-key ...
   gpg --export "btrshot-test" > /tmp/test.gpg

4. Start MinIO in the background on localhost:9000
   minio server /tmp/minio-data &
   aws --endpoint-url http://localhost:9000 s3 mb s3://btrshot-test

5. Write test config (/tmp/btrshot-test.conf)
   SOURCE_PATH=/mnt/A
   SOURCE_SUBVOLUME=data
   BACKUP_PATH=/mnt/B
   S3_BUCKET=btrshot-test
   S3_RETENTION_COUNT=10
   GPG_PUBLIC_KEY_FILE=/tmp/test.gpg
   FULL_BACKUP_INTERVAL=604800
   INCREMENTAL_INTERVAL=86400
   STATE_DIR=/tmp/btrshot-state

6. Export AWS environment for MinIO
   AWS_ACCESS_KEY_ID=minioadmin
   AWS_SECRET_ACCESS_KEY=minioadmin
   AWS_ENDPOINT_URL=http://localhost:9000
```

### Test Cases

Each test case is a Bash function in `test/test_cases.sh`. The harness runs them sequentially, resetting state between tests where noted. A test fails if it exits non-zero or if an assertion (`assert_*` helper) fails.

#### T1: First run triggers full backup

- **Precondition**: Clean state (no `last_full_backup` file).
- **Action**: Run `btrshot.sh`.
- **Assertions**:
  - Exit code 0.
  - A `full_*` snapshot directory exists under `/mnt/B/snapshots/`.
  - `/mnt/B/current` symlink points to the new snapshot.
  - `file1.txt` exists inside the snapshot on B with correct content.
  - `.snap_base_full` exists on A (retained for future incrementals).
  - `last_full_backup` timestamp file exists and contains a recent epoch.
  - State file reads `idle`.
  - A `.tar.gpg` object exists in MinIO bucket.

#### T2: Incremental backup after full

- **Precondition**: T1 completed; modify source data and advance `last_full_backup` to be recent but `last_incremental_backup` to be old (or absent).
- **Action**: Add `file2.txt` to source, then run `btrshot.sh`.
- **Assertions**:
  - An `incr_*` snapshot exists on B.
  - `file2.txt` is present in the incremental snapshot.
  - `.snap_base_full` on A has been rotated (different inode/generation from T1).
  - `last_incremental_backup` timestamp updated.
  - A second `.tar.gpg` object exists in MinIO.

#### T3: Skip when no backup needed

- **Precondition**: Both `last_full_backup` and `last_incremental_backup` are recent (within their intervals).
- **Action**: Run `btrshot.sh`.
- **Assertions**:
  - Exit code 0.
  - stdout contains "No backup needed".
  - No new snapshot created on B.

#### T4: Recovery from interrupted full backup

- **Precondition**: Simulate interruption by writing `in_progress:full:<ts>:` to the state file and creating a partial `.snap_tmp` on A.
- **Action**: Run `btrshot.sh`.
- **Assertions**:
  - `.snap_tmp` on A is deleted (cleanup).
  - Partial `.snap_tmp` on B is deleted (if created).
  - State returns to `idle`.
  - Script re-evaluates and runs the appropriate backup.

#### T5: Recovery from interrupted incremental backup

- **Precondition**: Simulate interruption by writing `in_progress:incremental:<ts>:` and creating `.snap_tmp` on A and B.
- **Action**: Run `btrshot.sh`.
- **Assertions**:
  - Temporary snapshots cleaned up on both A and B.
  - State returns to `idle`.
  - Script re-evaluates and runs the appropriate backup.

#### T6: Recovery from interrupted S3 upload

- **Precondition**: Complete a local full backup (snapshot exists on B), then write `in_progress:s3_upload:<ts>:<snap_name>` to the state file.
- **Action**: Run `btrshot.sh`.
- **Assertions**:
  - S3 upload completes for the named snapshot.
  - Corresponding timestamp file is updated.
  - State returns to `idle`.

#### T7: S3 retention enforcement

- **Precondition**: Upload 11+ objects to the MinIO bucket (mock old backups).
- **Action**: Run a full backup (which triggers `run_s3_upload` with retention logic).
- **Assertions**:
  - Total object count in MinIO bucket <= `S3_RETENTION_COUNT`.
  - Oldest objects were deleted, newest retained.

#### T8: Config validation — missing required variable

- **Precondition**: Config file with `S3_BUCKET` omitted.
- **Action**: Run `btrshot.sh`.
- **Assertions**:
  - Exit code non-zero.
  - stderr contains "missing required config variable(s)".
  - No snapshots created.

#### T9: Source validation — not a btrfs subvolume

- **Precondition**: Config points `SOURCE_SUBVOLUME` to a regular directory (not a subvolume).
- **Action**: Run `btrshot.sh`.
- **Assertions**:
  - Exit code non-zero.
  - stderr contains "not a btrfs subvolume".

#### T10: Backup FS validation — not btrfs

- **Precondition**: `BACKUP_PATH` points to a tmpfs or ext4 mount.
- **Action**: Run `btrshot.sh`.
- **Assertions**:
  - Exit code non-zero.
  - stderr contains "not a btrfs filesystem".

### Test Harness Structure

```
test/
├── run.sh              # Host-side entry point: builds image, launches Docker container
├── Dockerfile          # Docker image with all required packages
├── entrypoint.sh       # Container-side: env setup, runs test cases, reports results
├── test_cases.sh       # Test case functions (T1–T10)
└── helpers.sh          # Assertion utilities (assert_eq, assert_file_exists, etc.)
```

### Assertion Helpers (`test/helpers.sh`)

```bash
assert_eq()          { [[ "$1" == "$2" ]] || fail "expected '$2', got '$1'"; }
assert_ne()          { [[ "$1" != "$2" ]] || fail "expected != '$2'"; }
assert_file_exists() { [[ -f "$1" ]] || fail "file not found: $1"; }
assert_dir_exists()  { [[ -d "$1" ]] || fail "directory not found: $1"; }
assert_contains()    { echo "$1" | grep -qF "$2" || fail "output missing: $2"; }
assert_exit_code()   { [[ "$1" -eq "$2" ]] || fail "exit code $1, expected $2"; }
fail()               { echo "FAIL: $*" >&2; FAILURES=$((FAILURES + 1)); }
```

### Running Tests

From the project root on the host:

```bash
test/run.sh
```

The harness prints each test name and its pass/fail status, then exits non-zero if any test failed. `sudo` is not required if the current user is in the `docker` group.

### AWS Endpoint Compatibility

The script's `aws s3 cp` and `aws s3 ls` commands must reach MinIO inside the container. The AWS CLI respects `AWS_ENDPOINT_URL` (v2) or the `--endpoint-url` flag. The test config exports `AWS_ENDPOINT_URL=http://localhost:9000` so no script modifications are needed when using AWS CLI v2. If using AWS CLI v1, the entrypoint must configure an alias or wrapper that injects `--endpoint-url`.

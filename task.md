# Implementation Tasks

## Checklist

- [x] 1. Project scaffolding and configuration
- [ ] 2. State management functions
- [ ] 3. Startup validation
- [ ] 4. Snapshot naming utilities
- [ ] 5. Full backup
- [ ] 6. Incremental backup
- [ ] 7. S3 upload (encrypt + upload + retention)
- [ ] 8. Interruption recovery
- [ ] 9. Backup decision logic (main entry point)
- [ ] 10. Logging
- [ ] 11. systemd units (timer + service)
- [ ] 12. Example config file
- [ ] 13. Integration smoke test (optional, manual)

---

Tasks are ordered by dependency. Complete them top-to-bottom. Each task references the relevant section of `DESIGN.md`.

---

## 1. Project scaffolding and configuration

**Reference**: DESIGN.md § Configuration, § Directory Structure

- Create `btrshot.sh` as the main script.
- Implement config loading: source `/etc/btrshot/btrshot.conf`.
- Required variables:
  - `SOURCE_PATH`, `SOURCE_SUBVOLUME`
  - `BACKUP_PATH`
  - `S3_BUCKET`, `S3_RETENTION_COUNT`
  - `GPG_PUBLIC_KEY_FILE`
  - `FULL_BACKUP_INTERVAL` (default: 604800)
  - `INCREMENTAL_INTERVAL` (default: 86400)
  - `STATE_DIR` (default: `/var/lib/btrshot`)
- Validate that all required variables are set; exit with error if not.
- Create `STATE_DIR` if it doesn't exist.

---

## 2. State management functions

**Reference**: DESIGN.md § State File Format

- Implement `write_state()` — writes `<status>:<operation>:<timestamp>` to `$STATE_DIR/state`.
- Implement `read_state()` — reads and parses the state file; treats missing file as `idle`.
- Implement `read_timestamp()` — reads `$STATE_DIR/last_full_backup` or `$STATE_DIR/last_incremental_backup`; returns empty string if missing.
- Implement `write_timestamp()` — writes Unix epoch to the appropriate timestamp file.

State file format: `<status>:<operation>:<timestamp>`
- `idle::1706000000`
- `in_progress:full:1706000000`
- `in_progress:incremental:1706000000`
- `in_progress:s3_upload:1706000000`

---

## 3. Startup validation

**Reference**: DESIGN.md § Validation, § Prerequisites

- Implement `validate_source()`:
  - Run `btrfs subvolume show "$SOURCE_PATH/$SOURCE_SUBVOLUME"`.
  - Exit with error if the command fails (not a subvolume).
- Implement `validate_backup_fs()`:
  - Verify `$BACKUP_PATH` is a btrfs filesystem (check `/proc/mounts` or `btrfs filesystem show`).
  - Exit with error if not btrfs.
- Call both validators at script startup before any backup logic.

---

## 4. Snapshot naming utilities

**Reference**: DESIGN.md § Directory Structure

- Implement `timestamp_now()` — returns `YYYYMMDD_HHMMSS` (UTC).
- Define constants/variables:
  - Temp snapshot on A: `$SOURCE_PATH/.snap_tmp`
  - Base snapshot on A: `$SOURCE_PATH/.snap_base_full`
  - Snapshots dir on B: `$BACKUP_PATH/snapshots/`
  - Current symlink: `$BACKUP_PATH/current`
- Implement `full_snapshot_name()` → `full_YYYYMMDD_HHMMSS`
- Implement `incr_snapshot_name()` → `incr_YYYYMMDD_HHMMSS`

---

## 5. Full backup

**Reference**: DESIGN.md § Full Backup Process

Implement `run_full_backup()`:

1. Write state `in_progress:full`.
2. Create read-only snapshot:
   `btrfs subvolume snapshot -r $SOURCE_PATH/$SOURCE_SUBVOLUME $SOURCE_PATH/.snap_tmp`
3. Send to Disk B:
   `btrfs send $SOURCE_PATH/.snap_tmp | btrfs receive $BACKUP_PATH/snapshots/`
4. Rename received snapshot (`.snap_tmp` → `full_<ts>/`).
5. Update `current` symlink to the new full snapshot.
6. Delete old snapshots on B (keep only the new full).
7. On A: remove previous `.snap_base_full` (if exists); rename `.snap_tmp` → `.snap_base_full`.
8. Trigger S3 upload for the new full snapshot.
9. On success: update `last_full_backup` timestamp; write state `idle`.

---

## 6. Incremental backup

**Reference**: DESIGN.md § Incremental Backup Process

Implement `run_incremental_backup()`:

1. Write state `in_progress:incremental`.
2. Create read-only snapshot:
   `btrfs subvolume snapshot -r $SOURCE_PATH/$SOURCE_SUBVOLUME $SOURCE_PATH/.snap_tmp`
3. Send incremental:
   `btrfs send -p $SOURCE_PATH/.snap_base_full $SOURCE_PATH/.snap_tmp | btrfs receive $BACKUP_PATH/snapshots/`
4. Rename received snapshot to `incr_<ts>/`.
5. On A: delete `.snap_base_full`; rename `.snap_tmp` → `.snap_base_full`.
6. Update `last_incremental_backup` timestamp.
7. Write state `idle`.

---

## 7. S3 upload (encrypt + upload + retention)

**Reference**: DESIGN.md § S3 Upload Process

Implement `run_s3_upload()`:

1. Write state `in_progress:s3_upload`.
2. Stream-upload the snapshot:
   ```bash
   tar -cf - -C $BACKUP_PATH/snapshots $SNAPSHOT_NAME/ | \
       gpg --encrypt --recipient-file $GPG_PUBLIC_KEY_FILE | \
       aws s3 cp - s3://$S3_BUCKET/$SNAPSHOT_NAME.tar.gpg
   ```
3. Enforce S3 retention:
   - List objects with `aws s3 ls s3://$S3_BUCKET/`.
   - Sort by name (timestamp-embedded), delete oldest beyond `$S3_RETENTION_COUNT`.
   - Delete each excess object: `aws s3 rm s3://$S3_BUCKET/<object>`.
4. Write state `idle`.

---

## 8. Interruption recovery

**Reference**: DESIGN.md § Cleanup on Interruption Detection

Implement `recover_if_needed()`:

- Read state file.
- If `in_progress:full` or `in_progress:incremental`:
  - Delete `$SOURCE_PATH/.snap_tmp` if it exists (`btrfs subvolume delete`).
  - Delete any partially received `.snap_tmp` snapshot on B (`btrfs subvolume delete`).
  - Write state `idle`.
- If `in_progress:s3_upload`:
  - Abort incomplete multipart uploads:
    `aws s3api list-multipart-uploads --bucket $BUCKET` then
    `aws s3api abort-multipart-upload` for each.
  - Write state `idle`.
- If `idle` or file missing: no-op.

---

## 9. Backup decision logic (main entry point)

**Reference**: DESIGN.md § Decision Flow, § Timer-based Execution

Wire up the main script flow:

1. Source config file.
2. Run startup validation.
3. Call `recover_if_needed()`.
4. Read timestamps.
5. Get current Unix time (`now`).
6. Decision:
   - If `last_full` is empty or `now - last_full >= FULL_BACKUP_INTERVAL` → `run_full_backup`.
   - Else if `last_incremental` is empty or `now - last_incremental >= INCREMENTAL_INTERVAL` → `run_incremental_backup`.
   - Else → log "Nothing to do", exit 0.
7. Exit with non-zero code on unrecoverable failure.

---

## 10. Logging

**Reference**: DESIGN.md § Logging

- Implement `log_info()`, `log_warn()`, `log_error()` helper functions.
- Write to stdout/stderr (captured by systemd journal).
- Alternatively use `logger -t btrshot` for direct syslog submission.
- Key log events:
  - Backup start/end: `"Starting full backup"`, `"Full backup complete"`
  - S3 upload start/end.
  - Recovery actions: `"Interruption detected: <state>; cleaning up"`
  - Validation failure: `"Validation failed: <reason>"`
  - Nothing to do: `"No backup needed"`

---

## 11. systemd units (timer + service)

**Reference**: DESIGN.md § systemd Units

Create `btrshot.timer`:
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

Create `btrshot.service`:
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

Note: `ConditionPathIsMountPoint` lines must be customised during installation.

---

## 12. Example config file

**Reference**: DESIGN.md § Configuration

- Create `btrshot.conf.example` with all fields documented via inline comments.

---

## 13. Integration smoke test (optional, manual)

Document the manual test procedure:

1. Prepare two btrfs filesystems (can use loopback devices).
2. Create a subvolume on the source.
3. Place a test file in the subvolume.
4. Run `btrshot.sh` manually.
5. Verify snapshot appears on Disk B and in S3.
6. Modify the test file; run again after interval elapses.
7. Verify incremental snapshot on Disk B.

---

## Dependency order summary

```
1 (scaffolding + config)
├─ 2 (state management)
├─ 3 (validation)
├─ 4 (snapshot naming)
│  ├─ 5 (full backup) ──────┐
│  ├─ 6 (incremental) ──────┤
│  └─ 8 (recovery)          │
│                            ▼
│                     7 (S3 upload)
│
└─ 9 (decision logic / main) ◄── wires everything together
   └─ 10 (logging, wired throughout)
      └─ 11 (systemd units)
         └─ 12 (example config)
```

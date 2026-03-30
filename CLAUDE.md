# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**btrshot** is a Bash-based incremental backup system that:
- Snapshots a btrfs subvolume on Disk A (source)
- Transfers snapshots to Disk B (local backup) via `btrfs send/receive`
- Encrypts and uploads snapshots to S3 (remote backup) via GPG + AWS CLI
- Runs on a systemd timer (every 2h); self-determines full (every 7d) vs incremental (every 24h)

The previous Rust implementation was abandoned (see git history). The project is now pure Bash.

## Key Files

| File | Purpose |
|------|---------|
| `btrshot.sh` | Main script — the entire implementation |
| `DESIGN.md` | Complete specification; authoritative reference for behavior |
| `btrshot.conf.example` | Documented configuration template |
| `btrshot.service` | systemd oneshot service unit |
| `btrshot.timer` | systemd timer unit (every 2h) |

## Architecture

```
systemd timer (2h) → btrshot.service → btrshot.sh
    ├── Validate prerequisites
    ├── Recover from interruptions (state = in_progress)
    ├── Decide: full / incremental / skip
    └── Execute backup
         ├→ Disk A: create read-only btrfs snapshot
         ├→ Disk B: btrfs send/receive
         └→ S3: tar | gpg --encrypt | aws s3 cp
```

**State machine** (`$STATE_DIR/state`): `idle` ↔ `in_progress`. On startup, `in_progress` triggers recovery: removes partial snapshots, aborts S3 multipart uploads, resets to `idle`.

**Backup decision logic** (in `main`):
- No `last_full_backup` or > 7d since last full → run full backup
- > 24h since last incremental → run incremental backup
- Otherwise → skip (logged)

**S3 retention**: keeps the 10 most recent objects, deletes older ones after each upload.

## Configuration

The script sources `/etc/btrshot/btrshot.conf` (required; override with `$BTRSHOT_CONFIG`). AWS credentials can be supplied via `/etc/btrshot/aws.env`, which is loaded by the systemd service unit (`EnvironmentFile=-`), not by the script itself. See DESIGN.md §Configuration for details.

Required variables: `SOURCE_PATH`, `SOURCE_SUBVOLUME`, `BACKUP_PATH`, `S3_BUCKET`, `S3_RETENTION_COUNT`, `GPG_PUBLIC_KEY_FILE`.
Optional (have defaults): `FULL_BACKUP_INTERVAL` (604800), `INCREMENTAL_INTERVAL` (86400), `STATE_DIR` (/var/lib/btrshot).

## Development Notes

- No build system; no automated test suite. Integration testing requires two btrfs filesystems (loopback devices work).
- The script uses `set -euo pipefail`. All functions are designed to be atomic where possible.
- Refer to DESIGN.md for the recovery procedure and the systemd unit templates before modifying the script.

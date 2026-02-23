# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

`btrshot` is a Rust daemon that performs incremental btrfs snapshot backups from a source disk to a local backup disk and encrypted offsite storage on Amazon S3. See `DESIGN.md` for the full specification.

## Build & Development

This is a Rust project (not yet scaffolded). Once `Cargo.toml` exists:

```bash
cargo build               # debug build
cargo build --release     # release build
cargo test                # run all tests
cargo test <test_name>    # run a single test
cargo clippy              # lint
cargo fmt                 # format
```

The binary is a single daemon: `btrshot --config /etc/btrshot/config.toml`

## Architecture

The daemon is a single Rust binary using `tokio` async runtime. Key design points:

- **Single persistent process** — no external locking or systemd timers needed; the daemon manages its own schedule via `tokio::time::interval`
- **Scheduler loop** — `tokio::select!` between a `check_interval` tick (default every 2 h) and SIGTERM/SIGINT shutdown signal
- **External commands** — `btrfs`, `gpg`, `aws` are spawned via `std::process::Command` with piped stdio for streaming (no temp files for the pipeline)
- **State file** — `/var/lib/btrshot/state` tracks `idle`, `in_progress:full`, `in_progress:incremental`, `in_progress:s3_upload` to enable interruption recovery on restart
- **Backup decision** — full backup every 7 days, incremental every 24 h; timestamps stored in `/var/lib/btrshot/last_full_backup` and `last_incremental_backup`

### Key planned crates

| Crate | Role |
|-------|------|
| `tokio` | Async runtime + timer + signal handling |
| `serde` + `toml` | Config parsing (`/etc/btrshot/config.toml`) |
| `tracing` + `tracing-subscriber` | Structured logging to journald |

### Snapshot naming and retention

- Disk A keeps one read-only base snapshot (`.snap_base_full`) used as the parent for incrementals; rotated on each full backup
- Disk B stores `full_YYYYMMDD_HHMMSS/` and `incr_YYYYMMDD_HHMMSS/` under `snapshots/`, plus a `current` symlink to the latest full
- S3 retains the 10 most recent snapshot objects (one tar.gpg per snapshot, not bundled)

### Startup validation

Before entering the scheduler loop the daemon validates:
1. Source data path is a btrfs subvolume (`btrfs subvolume show`)
2. Disk B mount is a btrfs filesystem

Failure causes an immediate exit with a descriptive error.

## systemd Integration

The daemon uses `Type=notify` (`sd_notify`) to signal readiness after startup validation. It is restarted automatically on crash (`Restart=on-failure`). AWS credentials come from environment variables or `AWS_PROFILE` via `/etc/btrshot/aws.env`.

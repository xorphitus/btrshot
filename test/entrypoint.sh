#!/usr/bin/env bash
# Container-side entrypoint for the btrshot integration test suite.
# Runs inside a privileged Docker container for btrfs/loopback operations.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
export PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# ---------------------------------------------------------------------------
# 1. Create two loopback btrfs images (64 MB each)
# ---------------------------------------------------------------------------
truncate -s 64M /tmp/disk_a.img /tmp/disk_b.img
mkfs.btrfs -f -M -m single /tmp/disk_a.img
mkfs.btrfs -f -M -m single /tmp/disk_b.img
mkdir -p /mnt/A /mnt/B
mount -o loop /tmp/disk_a.img /mnt/A
mount -o loop /tmp/disk_b.img /mnt/B

# ---------------------------------------------------------------------------
# 2. Run test suite via pytest
# ---------------------------------------------------------------------------
exec python3 -m pytest /opt/btrshot/test/ -v --tb=short

#!/usr/bin/env bash
# VM-side entrypoint for the btrshot integration test suite.
# Runs inside a NixOS QEMU VM with real block devices for btrfs.
set -euo pipefail

export PROJECT_DIR="/etc/btrshot"

# Write exit code to shared directory on any exit so the host can read it.
trap 'echo "$?" > /results/exit_code' EXIT

# ---------------------------------------------------------------------------
# 1. Format and mount btrfs block devices
# ---------------------------------------------------------------------------
mkfs.btrfs -f -M -m single /dev/vdb
mkfs.btrfs -f -M -m single /dev/vdc
mkdir -p /mnt/A /mnt/B
mount /dev/vdb /mnt/A
mount /dev/vdc /mnt/B

# ---------------------------------------------------------------------------
# 2. Set S3 endpoint to host via QEMU user-mode networking
# ---------------------------------------------------------------------------
export AWS_ENDPOINT_URL=http://10.0.2.2:4566

# ---------------------------------------------------------------------------
# 3. Run shared test suite and capture exit code
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=run-suite.sh
source "$SCRIPT_DIR/run-suite.sh"

#!/usr/bin/env bash
# Host-side entry point for the btrshot integration test suite.
# Builds (or reuses) the nspawn rootfs, then launches systemd-nspawn
# with the privileges needed for btrfs and loopback devices.
#
# Usage:  sudo test/run.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# ---------------------------------------------------------------------------
# 1. Build (or reuse cached) rootfs via nspawn-rootfs.nix
# ---------------------------------------------------------------------------
NIX_EXPR="$SCRIPT_DIR/nspawn-rootfs.nix"

if [[ ! -f "$NIX_EXPR" ]]; then
  echo "ERROR: $NIX_EXPR not found" >&2
  exit 1
fi

echo "Building rootfs (nix-build)..."
ROOTFS="$(nix-build "$NIX_EXPR" --no-out-link)"
echo "Rootfs: $ROOTFS"

# ---------------------------------------------------------------------------
# 2. Launch systemd-nspawn
# ---------------------------------------------------------------------------
# Pre-create a pool of loop devices so the container can use them.
# loop-control can allocate new indices but nspawn's private /dev won't
# surface the resulting /dev/loopN nodes unless we bind them in.
LOOP_DEVICES=()
for i in $(seq 0 7); do
  node="/dev/loop${i}"
  [[ -b "$node" ]] || mknod -m 0660 "$node" b 7 "$i" 2>/dev/null || true
  if [[ -b "$node" ]]; then
    LOOP_DEVICES+=("--bind=$node")
  fi
done

echo "Launching systemd-nspawn container..."
exec systemd-nspawn \
    --directory="$ROOTFS" \
    --bind-ro="$PROJECT_DIR:/opt/btrshot" \
    --bind-ro=/nix/store \
    --capability=CAP_SYS_ADMIN \
    --property=DeviceAllow="block-loop rwm" \
    --bind=/dev/loop-control \
    "${LOOP_DEVICES[@]}" \
    -- /opt/btrshot/test/entrypoint.sh

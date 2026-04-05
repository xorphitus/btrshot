#!/usr/bin/env bash
# Host-side entry point for the btrshot integration test suite.
#
# Runs tests inside a NixOS QEMU VM (no sudo, no privileged containers).
#
# Usage:  test/run.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# ---------------------------------------------------------------------------
# NixOS QEMU VM path
# ---------------------------------------------------------------------------

# 1. Build the VM
echo "Building NixOS test VM..."
nix build "${PROJECT_DIR}#test-vm" -L --out-link "${PROJECT_DIR}/result"

# 2. Start floci (unprivileged Docker container)
echo "Starting floci S3 emulator..."
docker run -d --rm --name btrshot-floci -p 4566:4566 hectorvent/floci:latest

# 3. Create shared results directory and run the VM
RESULTS_DIR="/tmp/btrshot-test-results"
mkdir -p "$RESULTS_DIR"
# Clear any previous result
rm -f "$RESULTS_DIR/exit_code"

echo "Running test VM..."
# The VM script blocks until poweroff
# NixOS VM binary is named run-<hostname>
"${PROJECT_DIR}/result/bin/run-btrshot-test-vm" || true

# 4. Collect exit code
EXIT_CODE=$(cat "$RESULTS_DIR/exit_code" 2>/dev/null || echo 1)

# 5. Cleanup
echo "Cleaning up..."
docker stop btrshot-floci 2>/dev/null || true

exit "$EXIT_CODE"

#!/usr/bin/env bash
# Host-side entry point for the btrshot integration test suite.
#
# Default: NixOS QEMU VM (no sudo, no privileged containers).
# Fallback: --docker flag uses docker-compose (requires privileged).
#
# Usage:  test/run.sh [--docker]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# ---------------------------------------------------------------------------
# Parse arguments
# ---------------------------------------------------------------------------
USE_DOCKER=false
for arg in "$@"; do
  case "$arg" in
    --docker) USE_DOCKER=true ;;
    *) echo "Unknown argument: $arg" >&2; exit 1 ;;
  esac
done

# ---------------------------------------------------------------------------
# Docker Compose path (backward compat)
# ---------------------------------------------------------------------------
if [[ "$USE_DOCKER" == "true" ]]; then
  COMPOSE_CMD="docker compose"
  if command -v podman >/dev/null 2>&1 && docker --version 2>&1 | grep -qi podman; then
    COMPOSE_CMD="sudo podman-compose"
  fi

  echo "Building and launching test containers..."
  $COMPOSE_CMD -f "$SCRIPT_DIR/docker-compose.yml" up \
      --build --abort-on-container-exit --exit-code-from test
  $COMPOSE_CMD -f "$SCRIPT_DIR/docker-compose.yml" down --volumes
  exit $?
fi

# ---------------------------------------------------------------------------
# NixOS QEMU VM path (default)
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

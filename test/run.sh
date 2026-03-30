#!/usr/bin/env bash
# Host-side entry point for the btrshot integration test suite.
# Builds a Docker image and runs the tests inside a privileged container
# with full access to loopback and btrfs operations.
#
# Usage:  test/run.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# ---------------------------------------------------------------------------
# 1. Build (or reuse cached) Docker image
# ---------------------------------------------------------------------------
echo "Building Docker image..."
docker build -t btrshot-test "$SCRIPT_DIR"

# ---------------------------------------------------------------------------
# 2. Run tests in a privileged container
# ---------------------------------------------------------------------------
echo "Launching test container..."
exec docker run --rm --privileged \
    -v "$PROJECT_DIR:/opt/btrshot:ro" \
    btrshot-test

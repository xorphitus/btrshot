#!/usr/bin/env bash
# Host-side entry point for the btrshot integration test suite.
# Uses docker-compose to run floci (S3 emulator) alongside the privileged
# test container with full access to loopback and btrfs operations.
#
# Usage:  test/run.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# ---------------------------------------------------------------------------
# 1. Build and run via docker-compose
# ---------------------------------------------------------------------------
echo "Building and launching test containers..."
docker compose -f "$SCRIPT_DIR/docker-compose.yml" up \
    --build --abort-on-container-exit --exit-code-from test

# ---------------------------------------------------------------------------
# 2. Tear down
# ---------------------------------------------------------------------------
docker compose -f "$SCRIPT_DIR/docker-compose.yml" down --volumes

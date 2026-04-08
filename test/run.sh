#!/usr/bin/env bash
# Host-side entry point for the btrshot integration test suite.
#
# Runs the flake check that encapsulates the NixOS VM test.
#
# Usage: test/run.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
SYSTEM="${SYSTEM:-$(nix eval --impure --raw --expr builtins.currentSystem)}"

echo "Running sandboxed NixOS integration test for ${SYSTEM}..."
nix build "${PROJECT_DIR}#checks.${SYSTEM}.integration" -L

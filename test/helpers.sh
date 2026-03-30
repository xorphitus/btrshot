#!/usr/bin/env bash
# Assertion utilities for the btrshot test suite.
# Source this file from test_cases.sh or entrypoint.sh.

FAILURES=0

assert_eq() {
  [[ "$1" == "$2" ]] || fail "expected '$2', got '$1'"
}

assert_ne() {
  [[ "$1" != "$2" ]] || fail "expected != '$2'"
}

assert_file_exists() {
  [[ -f "$1" ]] || fail "file not found: $1"
}

assert_dir_exists() {
  [[ -d "$1" ]] || fail "directory not found: $1"
}

assert_contains() {
  printf '%s\n' "$1" | grep -qF "$2" || fail "output missing: $2"
}

assert_exit_code() {
  [[ "$1" -eq "$2" ]] || fail "exit code $1, expected $2"
}

fail() {
  echo "FAIL: $*" >&2
  FAILURES=$((FAILURES + 1))
}

#!/usr/bin/env bash
# Test case functions T1–T10 for btrshot.
# Sourced by entrypoint.sh; depends on helpers.sh being sourced first.
#
# Expected environment (set up by entrypoint.sh):
#   BTRSHOT_CONFIG  — path to test config file
#   BTRSHOT_SH      — path to btrshot.sh
#   SOURCE_PATH     — mount point of disk A  (e.g. /mnt/A)
#   SOURCE_SUBVOLUME — subvolume name         (e.g. data)
#   BACKUP_PATH     — mount point of disk B  (e.g. /mnt/B)
#   STATE_DIR       — test state directory    (e.g. /tmp/btrshot-state)
#   S3_BUCKET       — bucket name             (e.g. btrshot-test)
#   GPG_PUBLIC_KEY_FILE — path to test GPG public key

set -uo pipefail

# ---------------------------------------------------------------------------
# Helpers local to test cases
# ---------------------------------------------------------------------------

run_btrshot() {
  # Run btrshot.sh, capture combined output and exit code.
  local output rc
  output=$(BTRSHOT_CONFIG="$BTRSHOT_CONFIG" bash "$BTRSHOT_SH" 2>&1) && rc=$? || rc=$?
  echo "$output"
  return "$rc"
}

reset_state() {
  # Remove state files so the next test starts from a clean slate.
  rm -f "$STATE_DIR/state" \
        "$STATE_DIR/last_full_backup" \
        "$STATE_DIR/last_incremental_backup"
}

clean_snapshots() {
  # Remove all snapshots on B and the base snapshot on A.
  local dir="$BACKUP_PATH/snapshots"
  if [[ -d "$dir" ]]; then
    for sub in "$dir"/*/; do
      [[ -d "$sub" ]] && btrfs subvolume delete "$sub" 2>/dev/null || true
    done
  fi
  rm -rf "$dir"
  rm -f "$BACKUP_PATH/current"
  if [[ -d "$SOURCE_PATH/.snap_base_full" ]]; then
    btrfs subvolume delete "$SOURCE_PATH/.snap_base_full" 2>/dev/null || true
  fi
  if [[ -d "$SOURCE_PATH/.snap_tmp" ]]; then
    btrfs subvolume delete "$SOURCE_PATH/.snap_tmp" 2>/dev/null || true
  fi
}

count_s3_objects() {
  local n
  n=$(aws s3 ls "s3://${S3_BUCKET}/" | grep -c '\.tar\.gpg$' || true)
  echo "$n"
}

clear_s3_bucket() {
  aws s3 rm "s3://${S3_BUCKET}/" --recursive 2>/dev/null || true
}

# Full reset between unrelated tests.
full_reset() {
  reset_state
  clean_snapshots
  clear_s3_bucket
}

# ---------------------------------------------------------------------------
# T1: First run triggers full backup
# ---------------------------------------------------------------------------

test_t1_first_full_backup() {
  full_reset

  local output rc
  output=$(run_btrshot) && rc=$? || rc=$?

  assert_exit_code "$rc" 0

  # A full_* snapshot directory should exist on B.
  local full_snap
  full_snap=$(find "$BACKUP_PATH/snapshots" -maxdepth 1 -mindepth 1 -name 'full_*' -type d 2>/dev/null | head -1)
  assert_ne "$full_snap" ""
  assert_dir_exists "$full_snap"

  # current symlink exists and points to the snapshot.
  [[ -L "$BACKUP_PATH/current" ]] || fail "current symlink not found"
  local target
  target=$(readlink "$BACKUP_PATH/current")
  assert_contains "$target" "full_"

  # Seed data present inside the snapshot on B.
  assert_file_exists "$full_snap/file1.txt"
  local content
  content=$(cat "$full_snap/file1.txt")
  assert_eq "$content" "seed"

  # Base snapshot retained on A.
  assert_dir_exists "$SOURCE_PATH/.snap_base_full"

  # Timestamp file written.
  assert_file_exists "$STATE_DIR/last_full_backup"
  local ts
  ts=$(cat "$STATE_DIR/last_full_backup")
  assert_ne "$ts" ""

  # State is idle.
  local state
  state=$(cat "$STATE_DIR/state")
  assert_contains "$state" "idle"

  # S3: at least one .tar.gpg object.
  local n
  n=$(count_s3_objects)
  [[ "$n" -ge 1 ]] || fail "expected at least 1 S3 object, got $n"
}

# ---------------------------------------------------------------------------
# T2: Incremental backup after full
# ---------------------------------------------------------------------------

test_t2_incremental_after_full() {
  # Depends on T1 state: full backup already done.
  # Make last_full_backup recent so it won't trigger another full.
  local now
  now=$(date -u '+%s')
  echo "$now" > "$STATE_DIR/last_full_backup"
  # Remove incremental timestamp so an incremental is triggered.
  rm -f "$STATE_DIR/last_incremental_backup"

  # Record current .snap_base_full generation for rotation check.
  local old_gen
  old_gen=$(btrfs subvolume show "$SOURCE_PATH/.snap_base_full" 2>/dev/null \
    | awk '/Generation:/{print $2; exit}' || echo "")

  # Add new data.
  echo "extra" > "$SOURCE_PATH/$SOURCE_SUBVOLUME/file2.txt"

  local output rc
  output=$(run_btrshot) && rc=$? || rc=$?

  assert_exit_code "$rc" 0

  # An incr_* snapshot should exist on B.
  local incr_snap
  incr_snap=$(find "$BACKUP_PATH/snapshots" -maxdepth 1 -mindepth 1 -name 'incr_*' -type d 2>/dev/null | head -1)
  assert_ne "$incr_snap" ""
  assert_dir_exists "$incr_snap"

  # file2.txt present in the incremental snapshot.
  assert_file_exists "$incr_snap/file2.txt"

  # .snap_base_full rotated (different btrfs generation).
  local new_gen
  new_gen=$(btrfs subvolume show "$SOURCE_PATH/.snap_base_full" 2>/dev/null \
    | awk '/Generation:/{print $2; exit}' || echo "")
  assert_ne "$new_gen" "$old_gen"

  # Incremental timestamp updated.
  assert_file_exists "$STATE_DIR/last_incremental_backup"

  # Second S3 object.
  local n
  n=$(count_s3_objects)
  [[ "$n" -ge 2 ]] || fail "expected at least 2 S3 objects, got $n"
}

# ---------------------------------------------------------------------------
# T3: Skip when no backup needed
# ---------------------------------------------------------------------------

test_t3_skip() {
  # Set both timestamps to now so nothing is due.
  local now
  now=$(date -u '+%s')
  echo "$now" > "$STATE_DIR/last_full_backup"
  echo "$now" > "$STATE_DIR/last_incremental_backup"
  # State idle.
  echo "idle::${now}:" > "$STATE_DIR/state"

  # Count snapshots before.
  local before
  before=$(find "$BACKUP_PATH/snapshots" -maxdepth 1 -mindepth 1 -type d 2>/dev/null | wc -l)

  local output rc
  output=$(run_btrshot) && rc=$? || rc=$?

  assert_exit_code "$rc" 0
  assert_contains "$output" "No backup needed"

  # No new snapshot.
  local after
  after=$(find "$BACKUP_PATH/snapshots" -maxdepth 1 -mindepth 1 -type d 2>/dev/null | wc -l)
  assert_eq "$after" "$before"
}

# ---------------------------------------------------------------------------
# T4: Recovery from interrupted full backup
# ---------------------------------------------------------------------------

test_t4_recovery_full() {
  full_reset

  # Simulate interrupted full: create .snap_tmp on A and set state.
  btrfs subvolume snapshot -r "$SOURCE_PATH/$SOURCE_SUBVOLUME" "$SOURCE_PATH/.snap_tmp"
  local ts
  ts=$(date -u '+%s')
  echo "in_progress:full:${ts}:" > "$STATE_DIR/state"

  local output rc
  output=$(run_btrshot) && rc=$? || rc=$?

  assert_exit_code "$rc" 0

  # .snap_tmp on A cleaned up.
  [[ ! -d "$SOURCE_PATH/.snap_tmp" ]] || fail ".snap_tmp on A was not cleaned up"

  # State is idle.
  local state
  state=$(cat "$STATE_DIR/state")
  assert_contains "$state" "idle"

  # Script should have re-evaluated and run a full backup.
  local full_snap
  full_snap=$(find "$BACKUP_PATH/snapshots" -maxdepth 1 -mindepth 1 -name 'full_*' -type d 2>/dev/null | head -1)
  assert_ne "$full_snap" ""
}

# ---------------------------------------------------------------------------
# T5: Recovery from interrupted incremental backup
# ---------------------------------------------------------------------------

test_t5_recovery_incremental() {
  # We need a valid base snapshot first (run a full backup).
  full_reset

  local output rc
  output=$(run_btrshot) && rc=$? || rc=$?
  assert_exit_code "$rc" 0

  # Now simulate interrupted incremental.
  btrfs subvolume snapshot -r "$SOURCE_PATH/$SOURCE_SUBVOLUME" "$SOURCE_PATH/.snap_tmp"
  local ts
  ts=$(date -u '+%s')
  echo "in_progress:incremental:${ts}:" > "$STATE_DIR/state"

  # Make full recent but incremental old so an incremental will be triggered.
  echo "$ts" > "$STATE_DIR/last_full_backup"
  rm -f "$STATE_DIR/last_incremental_backup"

  output=$(run_btrshot) && rc=$? || rc=$?
  assert_exit_code "$rc" 0

  # Temp snapshot cleaned up.
  [[ ! -d "$SOURCE_PATH/.snap_tmp" ]] || fail ".snap_tmp on A was not cleaned up"

  # State idle.
  local state
  state=$(cat "$STATE_DIR/state")
  assert_contains "$state" "idle"
}

# ---------------------------------------------------------------------------
# T6: Recovery from interrupted S3 upload
# ---------------------------------------------------------------------------

test_t6_recovery_s3_upload() {
  full_reset

  # Run a full backup first so there is a snapshot on B.
  local output rc
  output=$(run_btrshot) && rc=$? || rc=$?
  assert_exit_code "$rc" 0

  # Identify the snapshot.
  local snap_name
  snap_name=$(find "$BACKUP_PATH/snapshots" -maxdepth 1 -mindepth 1 -name 'full_*' -printf '%f\n' | head -1)
  assert_ne "$snap_name" ""

  # Clear S3 to pretend the upload didn't complete.
  clear_s3_bucket

  # Simulate interrupted s3_upload state.
  local ts
  ts=$(date -u '+%s')
  echo "in_progress:s3_upload:${ts}:${snap_name}" > "$STATE_DIR/state"
  rm -f "$STATE_DIR/last_full_backup"

  output=$(run_btrshot) && rc=$? || rc=$?
  assert_exit_code "$rc" 0

  # S3 object should now exist.
  local n
  n=$(count_s3_objects)
  [[ "$n" -ge 1 ]] || fail "expected at least 1 S3 object after recovery, got $n"

  # State is idle.
  local state
  state=$(cat "$STATE_DIR/state")
  assert_contains "$state" "idle"
}

# ---------------------------------------------------------------------------
# T7: S3 retention enforcement
# ---------------------------------------------------------------------------

test_t7_s3_retention() {
  full_reset

  # Upload 11 dummy objects to exceed S3_RETENTION_COUNT (10).
  for i in $(seq 1 11); do
    echo "dummy" | aws s3 cp - "s3://${S3_BUCKET}/dummy_$(printf '%02d' "$i").tar.gpg"
  done

  local before
  before=$(count_s3_objects)
  [[ "$before" -ge 11 ]] || fail "pre-condition: expected >= 11 objects, got $before"

  # Run a full backup — its S3 upload path enforces retention.
  local output rc
  output=$(run_btrshot) && rc=$? || rc=$?
  assert_exit_code "$rc" 0

  local after
  after=$(count_s3_objects)
  [[ "$after" -le 10 ]] || fail "S3 retention not enforced: $after objects (expected <= 10)"
}

# ---------------------------------------------------------------------------
# T8: Config validation — missing required variable
# ---------------------------------------------------------------------------

test_t8_config_missing_var() {
  full_reset

  # Write a config with S3_BUCKET omitted.
  local bad_conf="$STATE_DIR/bad.conf"
  cat > "$bad_conf" <<EOF
SOURCE_PATH=$SOURCE_PATH
SOURCE_SUBVOLUME=$SOURCE_SUBVOLUME
BACKUP_PATH=$BACKUP_PATH
S3_RETENTION_COUNT=10
GPG_PUBLIC_KEY_FILE=$GPG_PUBLIC_KEY_FILE
EOF

  local output rc
  output=$(BTRSHOT_CONFIG="$bad_conf" bash "$BTRSHOT_SH" 2>&1) && rc=$? || rc=$?

  assert_ne "$rc" 0
  assert_contains "$output" "missing required config variable(s)"

  # No snapshots created.
  local count
  count=$(find "$BACKUP_PATH/snapshots" -maxdepth 1 -mindepth 1 -type d 2>/dev/null | wc -l)
  assert_eq "$count" "0"
}

# ---------------------------------------------------------------------------
# T9: Source validation — not a btrfs subvolume
# ---------------------------------------------------------------------------

test_t9_source_not_subvolume() {
  full_reset

  # Point SOURCE_SUBVOLUME at a regular directory.
  mkdir -p "$SOURCE_PATH/not_a_subvol"

  local bad_conf="$STATE_DIR/bad_src.conf"
  cat > "$bad_conf" <<EOF
SOURCE_PATH=$SOURCE_PATH
SOURCE_SUBVOLUME=not_a_subvol
BACKUP_PATH=$BACKUP_PATH
S3_BUCKET=$S3_BUCKET
S3_RETENTION_COUNT=10
GPG_PUBLIC_KEY_FILE=$GPG_PUBLIC_KEY_FILE
EOF

  local output rc
  output=$(BTRSHOT_CONFIG="$bad_conf" bash "$BTRSHOT_SH" 2>&1) && rc=$? || rc=$?

  assert_ne "$rc" 0
  assert_contains "$output" "not a btrfs subvolume"
}

# ---------------------------------------------------------------------------
# T10: Backup FS validation — not btrfs
# ---------------------------------------------------------------------------

test_t10_backup_not_btrfs() {
  full_reset

  # Create a tmpfs mount and point BACKUP_PATH at it.
  local tmpdir="/tmp/btrshot-notbtrfs"
  mkdir -p "$tmpdir"
  mount -t tmpfs tmpfs "$tmpdir"

  local bad_conf="$STATE_DIR/bad_bfs.conf"
  cat > "$bad_conf" <<EOF
SOURCE_PATH=$SOURCE_PATH
SOURCE_SUBVOLUME=$SOURCE_SUBVOLUME
BACKUP_PATH=$tmpdir
S3_BUCKET=$S3_BUCKET
S3_RETENTION_COUNT=10
GPG_PUBLIC_KEY_FILE=$GPG_PUBLIC_KEY_FILE
EOF

  local output rc
  output=$(BTRSHOT_CONFIG="$bad_conf" bash "$BTRSHOT_SH" 2>&1) && rc=$? || rc=$?

  # Cleanup the tmpfs.
  umount "$tmpdir" 2>/dev/null || true
  rmdir "$tmpdir" 2>/dev/null || true

  assert_ne "$rc" 0
  assert_contains "$output" "not a btrfs filesystem"
}

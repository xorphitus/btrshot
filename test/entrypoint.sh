#!/usr/bin/env bash
# Container-side entrypoint for the btrshot integration test suite.
# Runs inside systemd-nspawn; expects privileged capabilities for btrfs/loopback.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# ---------------------------------------------------------------------------
# 1. Create two loopback btrfs images (512 MB each)
# ---------------------------------------------------------------------------
truncate -s 512M /tmp/disk_a.img /tmp/disk_b.img
mkfs.btrfs -f /tmp/disk_a.img
mkfs.btrfs -f /tmp/disk_b.img
mkdir -p /mnt/A /mnt/B
mount -o loop /tmp/disk_a.img /mnt/A
mount -o loop /tmp/disk_b.img /mnt/B

# ---------------------------------------------------------------------------
# 2. Create source subvolume with seed data
# ---------------------------------------------------------------------------
btrfs subvolume create /mnt/A/data
echo "seed" > /mnt/A/data/file1.txt

# ---------------------------------------------------------------------------
# 3. Generate a throwaway GPG key pair (no passphrase)
# ---------------------------------------------------------------------------
export GNUPGHOME=/tmp/gnupg
mkdir -p "$GNUPGHOME"
chmod 700 "$GNUPGHOME"

gpg --batch --gen-key <<GPGEOF
%no-protection
Key-Type: RSA
Key-Length: 2048
Name-Real: btrshot-test
Expire-Date: 0
%commit
GPGEOF

gpg --batch --export "btrshot-test" > /tmp/test.gpg

# ---------------------------------------------------------------------------
# 4. Start MinIO in the background and create the bucket
# ---------------------------------------------------------------------------
export MINIO_ROOT_USER=minioadmin
export MINIO_ROOT_PASSWORD=minioadmin
export AWS_ACCESS_KEY_ID=minioadmin
export AWS_SECRET_ACCESS_KEY=minioadmin
export AWS_ENDPOINT_URL=http://localhost:9000

minio server /tmp/minio-data --address ":9000" &
MINIO_PID=$!

# Wait for MinIO to become ready.
for _ in $(seq 1 30); do
  if aws --endpoint-url http://localhost:9000 s3 ls >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

aws --endpoint-url http://localhost:9000 s3 mb s3://btrshot-test

# ---------------------------------------------------------------------------
# 5. Write test config
# ---------------------------------------------------------------------------
mkdir -p /tmp/btrshot-state

cat > /tmp/btrshot-test.conf <<'CONF'
SOURCE_PATH=/mnt/A
SOURCE_SUBVOLUME=data
BACKUP_PATH=/mnt/B
S3_BUCKET=btrshot-test
S3_RETENTION_COUNT=10
GPG_PUBLIC_KEY_FILE=/tmp/test.gpg
FULL_BACKUP_INTERVAL=604800
INCREMENTAL_INTERVAL=86400
STATE_DIR=/tmp/btrshot-state
CONF

# ---------------------------------------------------------------------------
# 6. Export variables expected by test_cases.sh
# ---------------------------------------------------------------------------
export BTRSHOT_CONFIG=/tmp/btrshot-test.conf
export BTRSHOT_SH="$PROJECT_DIR/btrshot.sh"
export SOURCE_PATH=/mnt/A
export SOURCE_SUBVOLUME=data
export BACKUP_PATH=/mnt/B
export STATE_DIR=/tmp/btrshot-state
export S3_BUCKET=btrshot-test
export GPG_PUBLIC_KEY_FILE=/tmp/test.gpg

# ---------------------------------------------------------------------------
# 7. Source helpers and test cases, then run each test
# ---------------------------------------------------------------------------
# shellcheck source=helpers.sh
source "$SCRIPT_DIR/helpers.sh"
# shellcheck source=test_cases.sh
source "$SCRIPT_DIR/test_cases.sh"

TESTS=(
  test_t1_first_full_backup
  test_t2_incremental_after_full
  test_t3_skip
  test_t4_recovery_full
  test_t5_recovery_incremental
  test_t6_recovery_s3_upload
  test_t7_s3_retention
  test_t8_config_missing_var
  test_t9_source_not_subvolume
  test_t10_backup_not_btrfs
)

PASSED=0
FAILED=0

for t in "${TESTS[@]}"; do
  echo "--- $t ---"
  FAILURES=0
  if "$t"; then
    if [[ "$FAILURES" -eq 0 ]]; then
      echo "PASS: $t"
      PASSED=$((PASSED + 1))
    else
      echo "FAIL: $t ($FAILURES assertion failure(s))"
      FAILED=$((FAILED + 1))
    fi
  else
    echo "FAIL: $t (non-zero exit)"
    FAILED=$((FAILED + 1))
  fi
done

# ---------------------------------------------------------------------------
# 9. Summary and cleanup
# ---------------------------------------------------------------------------
echo ""
echo "==============================="
echo "Results: $PASSED passed, $FAILED failed (of ${#TESTS[@]} tests)"
echo "==============================="

kill "$MINIO_PID" 2>/dev/null || true
umount /mnt/A 2>/dev/null || true
umount /mnt/B 2>/dev/null || true

if [[ "$FAILED" -gt 0 ]]; then
  exit 1
fi
exit 0

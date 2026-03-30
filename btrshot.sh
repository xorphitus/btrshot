#!/usr/bin/env bash
# btrshot.sh - btrfs snapshot backup to local disk and S3
set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

CONFIG_FILE="${BTRSHOT_CONFIG:-/etc/btrshot/btrshot.conf}"

if [[ ! -f "$CONFIG_FILE" ]]; then
    echo "ERROR: config file not found: $CONFIG_FILE" >&2
    exit 1
fi

# shellcheck source=/dev/null
source "$CONFIG_FILE"

# Defaults for optional variables
FULL_BACKUP_INTERVAL="${FULL_BACKUP_INTERVAL:-604800}"
INCREMENTAL_INTERVAL="${INCREMENTAL_INTERVAL:-86400}"
STATE_DIR="${STATE_DIR:-/var/lib/btrshot}"

# ---------------------------------------------------------------------------
# Validate required config variables
# ---------------------------------------------------------------------------

required_vars=(
    SOURCE_PATH
    SOURCE_SUBVOLUME
    BACKUP_PATH
    S3_BUCKET
    S3_RETENTION_COUNT
    GPG_PUBLIC_KEY_FILE
)

missing=()
for var in "${required_vars[@]}"; do
    if [[ -z "${!var:-}" ]]; then
        missing+=("$var")
    fi
done

if [[ ${#missing[@]} -gt 0 ]]; then
    echo "ERROR: missing required config variable(s): ${missing[*]}" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Ensure state directory exists
# ---------------------------------------------------------------------------

mkdir -p "$STATE_DIR"

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------

log_info()  { echo "$(date -u '+%Y-%m-%dT%H:%M:%SZ') [INFO]  $*"; }
log_warn()  { echo "$(date -u '+%Y-%m-%dT%H:%M:%SZ') [WARN]  $*" >&2; }
log_error() { echo "$(date -u '+%Y-%m-%dT%H:%M:%SZ') [ERROR] $*" >&2; }

# ---------------------------------------------------------------------------
# State management
# ---------------------------------------------------------------------------

write_state() {
    local status="$1"   # idle | in_progress
    local operation="${2:-}"  # full | incremental | s3_upload | (empty for idle)
    local detail="${3:-}"    # extra info (e.g. snapshot name for s3_upload)
    local ts
    ts=$(date -u '+%s')
    echo "${status}:${operation}:${ts}:${detail}" > "$STATE_DIR/state"
}

read_state() {
    # Outputs: status operation timestamp detail
    if [[ ! -f "$STATE_DIR/state" ]]; then
        echo "idle  $(date -u '+%s') "
        return
    fi
    local raw
    raw=$(cat "$STATE_DIR/state")
    local status operation ts detail
    IFS=':' read -r status operation ts detail <<< "$raw"
    echo "$status $operation $ts $detail"
}

read_timestamp() {
    # Usage: read_timestamp full | incremental
    local kind="$1"
    local file="$STATE_DIR/last_${kind}_backup"
    if [[ -f "$file" ]]; then
        cat "$file"
    else
        echo ""
    fi
}

write_timestamp() {
    # Usage: write_timestamp full | incremental
    local kind="$1"
    date -u '+%s' > "$STATE_DIR/last_${kind}_backup"
}

# ---------------------------------------------------------------------------
# Snapshot naming utilities
# ---------------------------------------------------------------------------

SNAP_TMP="$SOURCE_PATH/.snap_tmp"
SNAP_BASE="$SOURCE_PATH/.snap_base_full"
SNAPSHOTS_DIR="$BACKUP_PATH/snapshots"
CURRENT_LINK="$BACKUP_PATH/current"

timestamp_now() {
    date -u '+%Y%m%d_%H%M%S'
}

full_snapshot_name() {
    echo "full_$(timestamp_now)"
}

incr_snapshot_name() {
    echo "incr_$(timestamp_now)"
}

# ---------------------------------------------------------------------------
# Startup validation
# ---------------------------------------------------------------------------

validate_source() {
    if ! btrfs subvolume show "$SOURCE_PATH/$SOURCE_SUBVOLUME" > /dev/null 2>&1; then
        log_error "Validation failed: $SOURCE_PATH/$SOURCE_SUBVOLUME is not a btrfs subvolume"
        exit 1
    fi
}

validate_backup_fs() {
    if ! awk -v path="$BACKUP_PATH" '$2 == path && $3 == "btrfs"' /proc/mounts | grep -q .; then
        log_error "Validation failed: $BACKUP_PATH is not a btrfs filesystem"
        exit 1
    fi
}

# ---------------------------------------------------------------------------
# S3 upload
# ---------------------------------------------------------------------------

run_s3_upload() {
    local snapshot_name="$1"
    log_info "S3 upload start: $snapshot_name"
    write_state "in_progress" "s3_upload" "$snapshot_name"

    tar -cf - -C "$SNAPSHOTS_DIR" "$snapshot_name/" \
        | gpg --encrypt --recipient-file "$GPG_PUBLIC_KEY_FILE" \
        | aws s3 cp - "s3://${S3_BUCKET}/${snapshot_name}.tar.gpg"

    # Enforce S3 retention
    local objects
    objects=$(aws s3 ls "s3://${S3_BUCKET}/" \
        | awk '{print $NF}' \
        | grep '\.tar\.gpg$' \
        | sort)

    local count
    count=$(echo "$objects" | grep -c . || true)

    if [[ "$count" -gt "$S3_RETENTION_COUNT" ]]; then
        local excess=$(( count - S3_RETENTION_COUNT ))
        local to_delete
        to_delete=$(echo "$objects" | head -n "$excess")
        while IFS= read -r obj; do
            [[ -z "$obj" ]] && continue
            log_info "S3 retention: deleting s3://${S3_BUCKET}/${obj}"
            aws s3 rm "s3://${S3_BUCKET}/${obj}"
        done <<< "$to_delete"
    fi

    write_state "idle"
    log_info "S3 upload complete: $snapshot_name"
}

# ---------------------------------------------------------------------------
# Full backup
# ---------------------------------------------------------------------------

run_full_backup() {
    log_info "Starting full backup"
    write_state "in_progress" "full"

    mkdir -p "$SNAPSHOTS_DIR"

    local snap_name
    snap_name=$(full_snapshot_name)

    # 1. Create read-only snapshot on A
    btrfs subvolume snapshot -r "$SOURCE_PATH/$SOURCE_SUBVOLUME" "$SNAP_TMP"

    # 2. Send to Disk B
    btrfs send "$SNAP_TMP" | btrfs receive "$SNAPSHOTS_DIR/"

    # 3. Rename .snap_tmp to full_<ts>
    mv "$SNAPSHOTS_DIR/.snap_tmp" "$SNAPSHOTS_DIR/$snap_name"

    # 4. Update current symlink
    ln -sfn "snapshots/$snap_name" "$CURRENT_LINK"

    # 5. Delete all old snapshots on B (keep only the new full)
    find "$SNAPSHOTS_DIR" -mindepth 1 -maxdepth 1 \
        ! -name "$snap_name" \
        -exec btrfs subvolume delete {} \;

    # 6. Rotate base snapshot on A
    if [[ -d "$SNAP_BASE" ]]; then
        btrfs subvolume delete "$SNAP_BASE"
    fi
    mv "$SNAP_TMP" "$SNAP_BASE"

    # 7. S3 upload
    run_s3_upload "$snap_name"

    write_timestamp "full"
    write_state "idle"
    log_info "Full backup complete: $snap_name"
}

# ---------------------------------------------------------------------------
# Incremental backup
# ---------------------------------------------------------------------------

run_incremental_backup() {
    log_info "Starting incremental backup"
    write_state "in_progress" "incremental"

    mkdir -p "$SNAPSHOTS_DIR"

    local snap_name
    snap_name=$(incr_snapshot_name)

    # 1. Create read-only snapshot on A
    btrfs subvolume snapshot -r "$SOURCE_PATH/$SOURCE_SUBVOLUME" "$SNAP_TMP"

    # 2. Send incremental to Disk B
    btrfs send -p "$SNAP_BASE" "$SNAP_TMP" | btrfs receive "$SNAPSHOTS_DIR/"

    # 3. Rename received snapshot
    mv "$SNAPSHOTS_DIR/.snap_tmp" "$SNAPSHOTS_DIR/$snap_name"

    # 4. Rotate base snapshot on A
    btrfs subvolume delete "$SNAP_BASE"
    mv "$SNAP_TMP" "$SNAP_BASE"

    # 5. S3 upload
    run_s3_upload "$snap_name"

    write_timestamp "incremental"
    write_state "idle"
    log_info "Incremental backup complete: $snap_name"
}

# ---------------------------------------------------------------------------
# Interruption recovery
# ---------------------------------------------------------------------------

parse_s3_bucket() {
    # Extract bare bucket name from S3_BUCKET (which may contain a prefix)
    echo "${S3_BUCKET%%/*}"
}

recover_if_needed() {
    local state_info status operation detail
    state_info=$(read_state)
    status=$(echo "$state_info" | awk '{print $1}')
    operation=$(echo "$state_info" | awk '{print $2}')
    detail=$(echo "$state_info" | awk '{print $4}')

    if [[ "$status" != "in_progress" ]]; then
        return
    fi

    log_warn "Interruption detected: ${status}:${operation}; cleaning up"

    case "$operation" in
        full|incremental)
            # Clean up temp snapshot on A
            if [[ -d "$SNAP_TMP" ]]; then
                btrfs subvolume delete "$SNAP_TMP" || true
            fi
            # Clean up partial receive on B
            if [[ -d "$SNAPSHOTS_DIR/.snap_tmp" ]]; then
                btrfs subvolume delete "$SNAPSHOTS_DIR/.snap_tmp" || true
            fi
            ;;
        s3_upload)
            # Abort incomplete multipart uploads
            local bucket_name
            bucket_name=$(parse_s3_bucket)
            local uploads
            uploads=$(aws s3api list-multipart-uploads --bucket "$bucket_name" \
                --query 'Uploads[].[UploadId,Key]' --output text 2>/dev/null || true)
            if [[ -n "$uploads" ]]; then
                while IFS=$'\t' read -r upload_id key; do
                    [[ -z "$upload_id" ]] && continue
                    log_info "Aborting multipart upload: $key ($upload_id)"
                    aws s3api abort-multipart-upload \
                        --bucket "$bucket_name" \
                        --key "$key" \
                        --upload-id "$upload_id" || true
                done <<< "$uploads"
            fi
            # Retry the interrupted S3 upload
            if [[ -n "$detail" && -d "$SNAPSHOTS_DIR/$detail" ]]; then
                write_state "idle"
                log_info "Retrying S3 upload for: $detail"
                run_s3_upload "$detail"
                # Update the appropriate timestamp since the backup is now complete
                if [[ "$detail" == full_* ]]; then
                    write_timestamp "full"
                elif [[ "$detail" == incr_* ]]; then
                    write_timestamp "incremental"
                fi
            fi
            ;;
    esac

    write_state "idle"
}

# ---------------------------------------------------------------------------
# Main entry point
# ---------------------------------------------------------------------------

main() {
    # Startup validation
    validate_source
    validate_backup_fs

    # Recovery
    recover_if_needed

    # Read timestamps
    local last_full last_incr now
    last_full=$(read_timestamp "full")
    last_incr=$(read_timestamp "incremental")
    now=$(date -u '+%s')

    # Decision logic
    if [[ -z "$last_full" ]] || (( now - last_full >= FULL_BACKUP_INTERVAL )); then
        run_full_backup
    elif [[ -z "$last_incr" ]] || (( now - last_incr >= INCREMENTAL_INTERVAL )); then
        run_incremental_backup
    else
        log_info "No backup needed"
    fi
}

main "$@"

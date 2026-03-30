#!/usr/bin/env bash
# btrshot-restore.sh - Restore btrshot backups from S3
set -euo pipefail

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------

log_info()  { echo "$(date -u '+%Y-%m-%dT%H:%M:%SZ') [INFO]  $*"; }
log_warn()  { echo "$(date -u '+%Y-%m-%dT%H:%M:%SZ') [WARN]  $*" >&2; }
log_error() { echo "$(date -u '+%Y-%m-%dT%H:%M:%SZ') [ERROR] $*" >&2; }

# ---------------------------------------------------------------------------
# Cleanup
# ---------------------------------------------------------------------------

TMPDIR_RESTORE=""

cleanup() {
    if [[ -n "$TMPDIR_RESTORE" && -d "$TMPDIR_RESTORE" ]]; then
        rm -rf "$TMPDIR_RESTORE"
    fi
}

trap cleanup EXIT

# ---------------------------------------------------------------------------
# Usage
# ---------------------------------------------------------------------------

usage() {
    cat <<'USAGE'
Usage: btrshot-restore.sh [OPTIONS] [BACKUP_NAME | "latest"]

Restore a btrshot backup from S3.

Options:
  --list                  List available backups in S3 and exit
  --output-dir DIR        Directory to extract backup into (required unless --list)
  --gpg-key FILE          Path to GPG private key file for decryption
                          (optional; uses default GPG keyring if omitted)
  --btrfs-subvol PATH     Create a btrfs subvolume at PATH and rsync restored data into it
  --keep-intermediates    Keep .tar.gpg and .tar files after extraction
  --config FILE           Override config file path (default: /etc/btrshot/btrshot.conf)
  --help, -h              Show this help message

Examples:
  btrshot-restore.sh --list
  btrshot-restore.sh latest --output-dir /mnt/restore
  btrshot-restore.sh full_20260301_120000 --output-dir /mnt/restore --gpg-key key.asc
  btrshot-restore.sh latest --output-dir /mnt/restore --btrfs-subvol /mnt/data/restored
USAGE
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

ACTION=""
OUTPUT_DIR=""
GPG_KEY_FILE=""
BTRFS_SUBVOL=""
KEEP_INTERMEDIATES=false
BACKUP_NAME=""

parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --list)              ACTION="list"; shift ;;
            --output-dir)        OUTPUT_DIR="$2"; shift 2 ;;
            --gpg-key)           GPG_KEY_FILE="$2"; shift 2 ;;
            --btrfs-subvol)      BTRFS_SUBVOL="$2"; shift 2 ;;
            --keep-intermediates) KEEP_INTERMEDIATES=true; shift ;;
            --config)            BTRSHOT_CONFIG="$2"; shift 2 ;;
            --help|-h)           usage; exit 0 ;;
            -*)                  log_error "Unknown option: $1"; usage; exit 1 ;;
            *)                   BACKUP_NAME="$1"; shift ;;
        esac
    done
}

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

load_config() {
    local config_file="${BTRSHOT_CONFIG:-/etc/btrshot/btrshot.conf}"

    if [[ ! -f "$config_file" ]]; then
        log_error "Config file not found: $config_file"
        exit 1
    fi

    # shellcheck source=/dev/null
    source "$config_file"

    if [[ -z "${S3_BUCKET:-}" ]]; then
        log_error "S3_BUCKET is not set in $config_file"
        exit 1
    fi
}

# ---------------------------------------------------------------------------
# Prerequisite checks
# ---------------------------------------------------------------------------

check_prerequisites() {
    local missing=()
    command -v aws >/dev/null 2>&1 || missing+=(aws)
    command -v gpg >/dev/null 2>&1 || missing+=(gpg)
    command -v tar >/dev/null 2>&1 || missing+=(tar)

    if [[ -n "$BTRFS_SUBVOL" ]]; then
        command -v btrfs >/dev/null 2>&1 || missing+=(btrfs)
        command -v rsync >/dev/null 2>&1 || missing+=(rsync)
    fi

    if [[ ${#missing[@]} -gt 0 ]]; then
        log_error "Missing required commands: ${missing[*]}"
        exit 1
    fi
}

# ---------------------------------------------------------------------------
# S3 operations
# ---------------------------------------------------------------------------

list_backups() {
    aws s3 ls "s3://${S3_BUCKET}/" \
        | awk '{print $NF}' \
        | grep '\.tar\.gpg$' \
        | sort
}

resolve_backup_name() {
    local name="$1"

    # Normalize: strip .tar.gpg for comparison, then re-add
    name="${name%.tar.gpg}"

    if [[ "$name" == "latest" ]]; then
        local latest
        # Sort by embedded timestamp (YYYYMMDD_HHMMSS), not lexicographic prefix
        latest=$(list_backups | sort -t_ -k2,3 | tail -n 1)
        if [[ -z "$latest" ]]; then
            log_error "No backups found in s3://${S3_BUCKET}/"
            exit 1
        fi
        log_info "Resolved 'latest' to: $latest" >&2
        echo "$latest"
    else
        echo "${name}.tar.gpg"
    fi
}

download_backup() {
    local backup_name="$1"
    local dest_dir="$2"
    local s3_path="s3://${S3_BUCKET}/${backup_name}"

    # Verify the backup exists
    if ! aws s3 ls "$s3_path" >/dev/null 2>&1; then
        log_error "Backup not found: $s3_path"
        exit 1
    fi

    log_info "Downloading $s3_path"
    aws s3 cp "$s3_path" "${dest_dir}/${backup_name}"
    log_info "Download complete: ${dest_dir}/${backup_name}"
}

# ---------------------------------------------------------------------------
# Decrypt and extract
# ---------------------------------------------------------------------------

decrypt_backup() {
    local encrypted_file="$1"
    local output_file="$2"

    if [[ -n "$GPG_KEY_FILE" ]]; then
        log_info "Importing GPG key from $GPG_KEY_FILE"
        gpg --batch --import "$GPG_KEY_FILE"
    fi

    log_info "Decrypting $encrypted_file"
    gpg --decrypt --batch --yes "$encrypted_file" > "$output_file"
    log_info "Decryption complete: $output_file"
}

extract_backup() {
    local tar_file="$1"
    local dest_dir="$2"

    log_info "Extracting $tar_file to $dest_dir"
    mkdir -p "$dest_dir"
    tar -xf "$tar_file" -C "$dest_dir"
    log_info "Extraction complete"
}

# ---------------------------------------------------------------------------
# btrfs subvolume restore
# ---------------------------------------------------------------------------

restore_btrfs_subvol() {
    local source_dir="$1"
    local subvol_path="$2"

    # Find the snapshot directory inside the extraction
    local inner_dir
    inner_dir=$(find "$source_dir" -mindepth 1 -maxdepth 1 -type d -not -name '.*' | head -n 1)
    if [[ -z "$inner_dir" ]]; then
        log_error "No directory found in extracted backup at $source_dir"
        exit 1
    fi

    if [[ -e "$subvol_path" ]]; then
        log_error "Target subvolume path already exists: $subvol_path"
        exit 1
    fi

    log_info "Creating btrfs subvolume: $subvol_path"
    btrfs subvolume create "$subvol_path"

    log_info "Syncing data from $inner_dir to $subvol_path"
    rsync -aHAX "$inner_dir/" "$subvol_path/"

    log_info "btrfs subvolume restore complete: $subvol_path"
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

main() {
    parse_args "$@"
    load_config
    check_prerequisites

    if [[ "$ACTION" == "list" ]]; then
        list_backups
        exit 0
    fi

    # Validate required args for restore
    if [[ -z "$BACKUP_NAME" ]]; then
        log_error "Backup name required (positional argument or 'latest')"
        usage
        exit 1
    fi
    if [[ -z "$OUTPUT_DIR" ]]; then
        log_error "--output-dir is required"
        usage
        exit 1
    fi

    mkdir -p "$OUTPUT_DIR"
    TMPDIR_RESTORE=$(mktemp -d "${OUTPUT_DIR}/.btrshot-restore.XXXXXX")

    local resolved_name
    resolved_name=$(resolve_backup_name "$BACKUP_NAME")
    local tar_gpg_file="${TMPDIR_RESTORE}/${resolved_name}"
    local tar_file="${tar_gpg_file%.gpg}"

    # Step 1: Download
    download_backup "$resolved_name" "$TMPDIR_RESTORE"

    # Step 2: Decrypt
    decrypt_backup "$tar_gpg_file" "$tar_file"

    # Step 3: Extract
    extract_backup "$tar_file" "$OUTPUT_DIR"

    # Step 4: Optionally restore as btrfs subvolume
    if [[ -n "$BTRFS_SUBVOL" ]]; then
        restore_btrfs_subvol "$OUTPUT_DIR" "$BTRFS_SUBVOL"
    fi

    # Keep intermediates if requested
    if [[ "$KEEP_INTERMEDIATES" == true ]]; then
        mv "$tar_gpg_file" "$OUTPUT_DIR/"
        mv "$tar_file" "$OUTPUT_DIR/" 2>/dev/null || true
        log_info "Intermediate files kept in $OUTPUT_DIR/"
    fi

    log_info "Restore complete: $OUTPUT_DIR"
}

main "$@"

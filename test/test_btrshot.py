"""Integration tests T1–T19 for btrshot."""

import os
import subprocess
import time
from pathlib import Path


class TestSequentialWorkflow:
    """T1–T3 run in definition order, each depending on prior state."""

    def test_t1_first_full_backup(self, runner):
        runner.full_reset()
        result = runner.run_ok()

        snaps = runner.find_snapshots("full_*")
        assert snaps, "no full_* snapshot found"
        assert (snaps[0] / "file1.txt").read_text() == "seed"

        current = runner.backup_path / "current"
        assert current.is_symlink()
        assert "full_" in str(os.readlink(current))

        assert (runner.source_path / ".snap_base_full").is_dir()
        assert (runner.state_dir / "last_full_backup").read_text().strip()
        assert "idle" in runner.read_state()
        assert runner.count_s3_objects() >= 1

    def test_t2_incremental_after_full(self, runner):
        # Make last_full_backup recent; remove incremental timestamp.
        now = str(int(time.time()))
        (runner.state_dir / "last_full_backup").write_text(now)
        (runner.state_dir / "last_incremental_backup").unlink(missing_ok=True)

        old_gen = runner.get_btrfs_generation(runner.source_path / ".snap_base_full")

        # Add new data.
        (runner.source_path / runner.source_subvolume / "file2.txt").write_text("extra")

        result = runner.run_ok()

        incr_snaps = runner.find_snapshots("incr_*")
        assert incr_snaps, "no incr_* snapshot found"
        assert (incr_snaps[0] / "file2.txt").exists()

        new_gen = runner.get_btrfs_generation(runner.source_path / ".snap_base_full")
        assert new_gen != old_gen, "snap_base_full was not rotated"

        assert (runner.state_dir / "last_incremental_backup").exists()
        assert runner.count_s3_objects() >= 2

    def test_t3_skip(self, runner):
        now = str(int(time.time()))
        (runner.state_dir / "last_full_backup").write_text(now)
        (runner.state_dir / "last_incremental_backup").write_text(now)
        (runner.state_dir / "state").write_text(f"idle::{now}:")

        before = runner.count_all_snapshots()

        result = runner.run_ok()
        assert "No backup needed" in result.stdout
        assert runner.count_all_snapshots() == before


def test_t4_recovery_full(clean_runner):
    clean_runner.simulate_interruption("full")

    result = clean_runner.run_ok()

    assert not (clean_runner.source_path / ".snap_tmp").is_dir(), ".snap_tmp not cleaned up"
    assert "idle" in clean_runner.read_state()

    snaps = clean_runner.find_snapshots("full_*")
    assert snaps, "no full backup created after recovery"


def test_t5_recovery_incremental(clean_runner):
    # Run a full backup first.
    clean_runner.run_ok()

    # Simulate interrupted incremental.
    clean_runner.simulate_interruption("incremental")
    now = str(int(time.time()))
    (clean_runner.state_dir / "last_full_backup").write_text(now)
    (clean_runner.state_dir / "last_incremental_backup").unlink(missing_ok=True)

    result = clean_runner.run_ok()

    assert not (clean_runner.source_path / ".snap_tmp").is_dir(), ".snap_tmp not cleaned up"
    assert "idle" in clean_runner.read_state()


def test_t6_recovery_s3_upload(clean_runner):
    # Run a full backup first.
    clean_runner.run_ok()

    # Identify the snapshot name.
    snaps = clean_runner.find_snapshots("full_*")
    assert snaps
    snap_name = snaps[0].name

    # Clear S3 and simulate interrupted s3_upload state.
    clean_runner.clear_s3_bucket()
    clean_runner.simulate_interruption("s3_upload", snap_name=snap_name)
    (clean_runner.state_dir / "last_full_backup").unlink(missing_ok=True)

    result = clean_runner.run_ok()

    assert clean_runner.count_s3_objects() >= 1, "no S3 object after recovery"
    assert "idle" in clean_runner.read_state()


def test_t7_s3_retention(clean_runner):
    # Upload 11 dummy objects to exceed S3_RETENTION_COUNT (10).
    for i in range(1, 12):
        subprocess.run(
            ["aws", "s3", "cp", "-", f"s3://{clean_runner.s3_bucket}/dummy_{i:02d}.tar.gpg"],
            input=b"dummy",
            check=True,
        )

    assert clean_runner.count_s3_objects() >= 11

    # Run a full backup — its S3 upload path enforces retention.
    result = clean_runner.run_ok()

    assert clean_runner.count_s3_objects() <= 10, "S3 retention not enforced"


def test_t8_config_missing_var(clean_runner):
    conf = clean_runner.write_config(omit={"S3_BUCKET"})
    result = clean_runner.run_fail(config_path=conf)

    assert "missing required config variable(s)" in result.stdout
    assert clean_runner.count_all_snapshots() == 0


def test_t9_source_not_subvolume(clean_runner):
    fake_dir = clean_runner.source_path / "not_a_subvol"
    fake_dir.mkdir(exist_ok=True)

    conf = clean_runner.write_config(SOURCE_SUBVOLUME="not_a_subvol")
    result = clean_runner.run_fail(config_path=conf)

    assert "not a btrfs subvolume" in result.stdout


def test_t10_backup_not_btrfs(clean_runner):
    tmpdir = Path("/tmp/btrshot-notbtrfs")
    tmpdir.mkdir(exist_ok=True)
    subprocess.run(["mount", "-t", "tmpfs", "tmpfs", str(tmpdir)], check=True)

    try:
        conf = clean_runner.write_config(BACKUP_PATH=str(tmpdir))
        result = clean_runner.run_fail(config_path=conf)

        assert "not a btrfs filesystem" in result.stdout
    finally:
        subprocess.run(["umount", str(tmpdir)], capture_output=True)
        tmpdir.rmdir()


# ---------------------------------------------------------------------------
# Restore tests (T11–T19)
# ---------------------------------------------------------------------------

RESTORE_DIR = Path("/tmp/btrshot-restore-test")


def _cleanup_restore_dir():
    """Remove the restore output directory."""
    if RESTORE_DIR.exists():
        subprocess.run(["rm", "-rf", str(RESTORE_DIR)], capture_output=True)


def _find_restored_snapshot_dir(base: Path) -> Path:
    """Find the first snapshot directory inside the restore output."""
    candidates = [d for d in base.iterdir() if d.is_dir() and not d.name.startswith(".")]
    assert candidates, f"no snapshot directory found in {base}"
    return sorted(candidates)[0]


def test_t11_list_backups(clean_runner):
    clean_runner.run_ok()

    result = clean_runner.run_restore_ok("--list")
    assert ".tar.gpg" in result.stdout


def test_t12_restore_full_backup(clean_runner):
    clean_runner.run_ok()

    _cleanup_restore_dir()
    try:
        clean_runner.run_restore_ok(
            "latest",
            "--output-dir", str(RESTORE_DIR),
            "--gpg-key", str(clean_runner.gpg_private_key_file),
        )

        snap_dir = _find_restored_snapshot_dir(RESTORE_DIR)
        assert (snap_dir / "file1.txt").read_text() == "seed"
    finally:
        _cleanup_restore_dir()


def test_t13_restore_incremental_backup(clean_runner):
    # Full backup
    clean_runner.run_ok()

    # Set up for incremental
    now = str(int(time.time()))
    (clean_runner.state_dir / "last_full_backup").write_text(now)
    (clean_runner.state_dir / "last_incremental_backup").unlink(missing_ok=True)
    (clean_runner.source_path / clean_runner.source_subvolume / "file2.txt").write_text("extra")

    # Incremental backup
    clean_runner.run_ok()

    _cleanup_restore_dir()
    try:
        clean_runner.run_restore_ok(
            "latest",
            "--output-dir", str(RESTORE_DIR),
            "--gpg-key", str(clean_runner.gpg_private_key_file),
        )

        snap_dir = _find_restored_snapshot_dir(RESTORE_DIR)
        assert (snap_dir / "file1.txt").exists()
        assert (snap_dir / "file2.txt").exists()
    finally:
        _cleanup_restore_dir()


def test_t14_restore_by_name(clean_runner):
    clean_runner.run_ok()

    # Get the actual backup name from --list
    list_result = clean_runner.run_restore_ok("--list")
    backup_name = list_result.stdout.strip().splitlines()[-1].strip()
    assert backup_name.endswith(".tar.gpg")

    _cleanup_restore_dir()
    try:
        clean_runner.run_restore_ok(
            backup_name,
            "--output-dir", str(RESTORE_DIR),
            "--gpg-key", str(clean_runner.gpg_private_key_file),
        )

        snap_dir = _find_restored_snapshot_dir(RESTORE_DIR)
        assert (snap_dir / "file1.txt").read_text() == "seed"
    finally:
        _cleanup_restore_dir()


def test_t15_restore_btrfs_subvol(clean_runner):
    clean_runner.run_ok()

    _cleanup_restore_dir()
    subvol_path = clean_runner.backup_path / "restored"
    try:
        clean_runner.run_restore_ok(
            "latest",
            "--output-dir", str(RESTORE_DIR),
            "--gpg-key", str(clean_runner.gpg_private_key_file),
            "--btrfs-subvol", str(subvol_path),
        )

        # Verify it is a btrfs subvolume
        show_result = subprocess.run(
            ["btrfs", "subvolume", "show", str(subvol_path)],
            capture_output=True, text=True,
        )
        assert show_result.returncode == 0, f"not a btrfs subvolume: {show_result.stderr}"

        assert (subvol_path / "file1.txt").read_text() == "seed"
    finally:
        subprocess.run(
            ["btrfs", "subvolume", "delete", str(subvol_path)],
            capture_output=True,
        )
        _cleanup_restore_dir()


def test_t16_restore_keep_intermediates(clean_runner):
    clean_runner.run_ok()

    _cleanup_restore_dir()
    try:
        clean_runner.run_restore_ok(
            "latest",
            "--output-dir", str(RESTORE_DIR),
            "--gpg-key", str(clean_runner.gpg_private_key_file),
            "--keep-intermediates",
        )

        gpg_files = list(RESTORE_DIR.glob("*.tar.gpg"))
        tar_files = list(RESTORE_DIR.glob("*.tar"))
        assert gpg_files, "no .tar.gpg file kept"
        assert tar_files, "no .tar file kept"
    finally:
        _cleanup_restore_dir()


def test_t17_restore_missing_backup(clean_runner):
    result = clean_runner.run_restore_fail(
        "nonexistent_backup_20991231_235959",
        "--output-dir", str(RESTORE_DIR),
        "--gpg-key", str(clean_runner.gpg_private_key_file),
    )

    assert "not found" in result.stdout.lower() or "error" in result.stdout.lower()
    _cleanup_restore_dir()


def test_t18_restore_no_output_dir(clean_runner):
    result = clean_runner.run_restore_fail("latest")

    assert "--output-dir" in result.stdout


def test_t19_restore_subvol_path_exists(clean_runner):
    clean_runner.run_ok()

    _cleanup_restore_dir()
    existing_path = clean_runner.backup_path / "existing-dir"
    existing_path.mkdir(exist_ok=True)
    try:
        result = clean_runner.run_restore_fail(
            "latest",
            "--output-dir", str(RESTORE_DIR),
            "--gpg-key", str(clean_runner.gpg_private_key_file),
            "--btrfs-subvol", str(existing_path),
        )

        assert "already exists" in result.stdout.lower()
    finally:
        existing_path.rmdir()
        _cleanup_restore_dir()

"""Integration tests T1–T10 for btrshot."""

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

"""Session-scoped fixtures and BtrshotRunner helper for btrshot integration tests."""

import os
import re
import subprocess
import time
from pathlib import Path

import pytest


class BtrshotRunner:
    """Helper class encapsulating btrshot execution and common test operations."""

    def __init__(self, project_dir: str):
        self.project_dir = Path(project_dir)
        self.btrshot_sh = self.project_dir / "btrshot.sh"
        self.config_path = Path("/tmp/btrshot-test.conf")
        self.source_path = Path("/mnt/A")
        self.source_subvolume = "data"
        self.backup_path = Path("/mnt/B")
        self.state_dir = Path("/tmp/btrshot-state")
        self.s3_bucket = "btrshot-test"
        self.gpg_public_key_file = Path("/tmp/test.gpg")
        self.gpg_private_key_file = Path("/tmp/test-secret.gpg")

    def run(self, config_path: Path | None = None) -> subprocess.CompletedProcess:
        """Run btrshot.sh and return the CompletedProcess."""
        cfg = str(config_path or self.config_path)
        return subprocess.run(
            ["bash", str(self.btrshot_sh)],
            env={**os.environ, "BTRSHOT_CONFIG": cfg},
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )

    def run_ok(self, config_path=None):
        """Run btrshot.sh and assert success."""
        result = self.run(config_path)
        assert result.returncode == 0, result.stdout
        return result

    def run_fail(self, config_path=None):
        """Run btrshot.sh and assert failure."""
        result = self.run(config_path)
        assert result.returncode != 0, result.stdout
        return result

    def run_restore(self, *args, config_path=None):
        """Run btrshot-restore.sh and return the CompletedProcess."""
        cfg = str(config_path or self.config_path)
        return subprocess.run(
            ["bash", str(self.project_dir / "btrshot-restore.sh"), *args],
            env={**os.environ, "BTRSHOT_CONFIG": cfg},
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )

    def run_restore_ok(self, *args, config_path=None):
        """Run btrshot-restore.sh and assert success."""
        result = self.run_restore(*args, config_path=config_path)
        assert result.returncode == 0, result.stdout
        return result

    def run_restore_fail(self, *args, config_path=None):
        """Run btrshot-restore.sh and assert failure."""
        result = self.run_restore(*args, config_path=config_path)
        assert result.returncode != 0, result.stdout
        return result

    def find_snapshots(self, pattern: str) -> list[Path]:
        """Find snapshot directories matching a glob pattern under BACKUP_PATH/snapshots."""
        snap_dir = self.backup_path / "snapshots"
        if not snap_dir.is_dir():
            return []
        return sorted(snap_dir.glob(pattern))

    def count_all_snapshots(self) -> int:
        """Count all snapshot directories under BACKUP_PATH/snapshots."""
        snap_dir = self.backup_path / "snapshots"
        if not snap_dir.is_dir():
            return 0
        return len([d for d in snap_dir.iterdir() if d.is_dir()])

    def read_state(self) -> str:
        """Read the state file content."""
        return (self.state_dir / "state").read_text().strip()

    def count_s3_objects(self) -> int:
        """Count .tar.gpg objects in the S3 bucket."""
        result = subprocess.run(
            ["aws", "s3", "ls", f"s3://{self.s3_bucket}/"],
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            return 0
        return len(re.findall(r"\.tar\.gpg$", result.stdout, re.MULTILINE))

    def clear_s3_bucket(self):
        """Remove all objects from the S3 bucket."""
        subprocess.run(
            ["aws", "s3", "rm", f"s3://{self.s3_bucket}/", "--recursive"],
            capture_output=True,
        )

    def write_config(self, *, omit: set | None = None, **overrides) -> Path:
        """Write a custom config file and return its path."""
        defaults = {
            "SOURCE_PATH": str(self.source_path),
            "SOURCE_SUBVOLUME": self.source_subvolume,
            "BACKUP_PATH": str(self.backup_path),
            "S3_BUCKET": self.s3_bucket,
            "S3_RETENTION_COUNT": "10",
            "GPG_PUBLIC_KEY_FILE": str(self.gpg_public_key_file),
        }
        defaults.update(overrides)
        if omit:
            for key in omit:
                defaults.pop(key, None)

        conf_path = self.state_dir / "custom.conf"
        lines = [f"{k}={v}" for k, v in defaults.items()]
        conf_path.write_text("\n".join(lines) + "\n")
        return conf_path

    def reset_state(self):
        """Remove state files."""
        for name in ("state", "last_full_backup", "last_incremental_backup"):
            f = self.state_dir / name
            if f.exists():
                f.unlink()

    def clean_snapshots(self):
        """Remove all snapshots on B and base snapshot on A."""
        snap_dir = self.backup_path / "snapshots"
        if snap_dir.is_dir():
            for sub in snap_dir.iterdir():
                if sub.is_dir():
                    subprocess.run(
                        ["btrfs", "subvolume", "delete", str(sub)],
                        capture_output=True,
                    )
            subprocess.run(["rm", "-rf", str(snap_dir)], capture_output=True)

        current = self.backup_path / "current"
        if current.is_symlink() or current.exists():
            current.unlink()

        for snap_name in (".snap_base_full", ".snap_tmp"):
            snap = self.source_path / snap_name
            if snap.is_dir():
                subprocess.run(
                    ["btrfs", "subvolume", "delete", str(snap)],
                    capture_output=True,
                )

    def full_reset(self):
        """Full reset: state + snapshots + S3."""
        self.reset_state()
        self.clean_snapshots()
        self.clear_s3_bucket()

    def simulate_interruption(self, phase, snap_name=None):
        """Write in_progress state and optionally create .snap_tmp."""
        if phase in ("full", "incremental"):
            subprocess.run(
                ["btrfs", "subvolume", "snapshot", "-r",
                 str(self.source_path / self.source_subvolume),
                 str(self.source_path / ".snap_tmp")],
                check=True,
            )
        now = str(int(time.time()))
        snap_part = snap_name or ""
        (self.state_dir / "state").write_text(f"in_progress:{phase}:{now}:{snap_part}")

    def get_btrfs_generation(self, path: Path) -> int:
        """Get the btrfs generation number for a subvolume."""
        result = subprocess.run(
            ["btrfs", "subvolume", "show", str(path)],
            capture_output=True,
            text=True,
        )
        match = re.search(r"Generation:\s+(\d+)", result.stdout)
        return int(match.group(1)) if match else 0


@pytest.fixture(scope="session")
def runner():
    """Set up the test environment and yield a BtrshotRunner instance."""
    project_dir = os.environ.get("PROJECT_DIR", "/opt/btrshot")
    r = BtrshotRunner(project_dir)

    # 1. Create source subvolume with seed data
    subprocess.run(
        ["btrfs", "subvolume", "create", str(r.source_path / "data")],
        check=True,
    )
    (r.source_path / "data" / "file1.txt").write_text("seed")

    # 2. Generate throwaway GPG key pair
    gnupghome = Path("/tmp/gnupg")
    gnupghome.mkdir(mode=0o700, exist_ok=True)
    os.environ["GNUPGHOME"] = str(gnupghome)

    subprocess.run(
        ["gpg", "--batch", "--gen-key"],
        input=(
            "%no-protection\n"
            "Key-Type: RSA\n"
            "Key-Length: 2048\n"
            "Name-Real: btrshot-test\n"
            "Expire-Date: 0\n"
            "%commit\n"
        ),
        check=True,
    )
    subprocess.run(
        ["gpg", "--batch", "--export", "btrshot-test"],
        stdout=open(str(r.gpg_public_key_file), "wb"),
        check=True,
    )
    subprocess.run(
        ["gpg", "--batch", "--export-secret-keys", "--armor", "btrshot-test"],
        stdout=open(str(r.gpg_private_key_file), "wb"),
        check=True,
    )

    # 3. Wait for floci (S3) and create bucket
    os.environ.setdefault("AWS_ACCESS_KEY_ID", "test")
    os.environ.setdefault("AWS_SECRET_ACCESS_KEY", "test")

    for _ in range(30):
        result = subprocess.run(
            ["aws", "s3", "ls"],
            capture_output=True,
        )
        if result.returncode == 0:
            break
        time.sleep(1)

    subprocess.run(["aws", "s3", "mb", f"s3://{r.s3_bucket}"], check=True)

    # 4. Write test config
    r.state_dir.mkdir(parents=True, exist_ok=True)
    r.config_path.write_text(
        f"SOURCE_PATH={r.source_path}\n"
        f"SOURCE_SUBVOLUME={r.source_subvolume}\n"
        f"BACKUP_PATH={r.backup_path}\n"
        f"S3_BUCKET={r.s3_bucket}\n"
        f"S3_RETENTION_COUNT=10\n"
        f"GPG_PUBLIC_KEY_FILE={r.gpg_public_key_file}\n"
        f"FULL_BACKUP_INTERVAL=604800\n"
        f"INCREMENTAL_INTERVAL=86400\n"
        f"STATE_DIR={r.state_dir}\n"
    )

    yield r

    # Teardown
    subprocess.run(["umount", "/mnt/A"], capture_output=True)
    subprocess.run(["umount", "/mnt/B"], capture_output=True)


@pytest.fixture
def clean_runner(runner):
    """Yield a runner that has been fully reset."""
    runner.full_reset()
    return runner

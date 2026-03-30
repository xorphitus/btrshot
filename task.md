# Test Suite Implementation Plan

## Dependency Graph

```
#1 helpers.sh ──┐
                ├─► #2 test_cases.sh ──► #3 entrypoint.sh ──┐
                                                              ├─► #5 run.sh ──┐
#4 nspawn-rootfs.nix ────────────────────────────────────────┘                │
                                                                              ├─► #7 E2E validation
#6 Verify AWS_ENDPOINT_URL ──────────────────────────────────────────────────┘
```

## Tasks

### #1 Create test/helpers.sh — assertion utilities
- **Blocked by**: (none)
- **Status**: done
- **Description**: Implement the assertion helper functions (`assert_eq`, `assert_ne`, `assert_file_exists`, `assert_dir_exists`, `assert_contains`, `assert_exit_code`, `fail`) as defined in DESIGN.md. This is a leaf dependency with no prerequisites.

### #2 Create test/test_cases.sh — test case functions T1–T10
- **Blocked by**: #1
- **Status**: done
- **Description**: Implement all 10 test case functions (T1: first full backup, T2: incremental after full, T3: skip, T4–T6: recovery scenarios, T7: S3 retention, T8–T10: validation failures). Each test is a Bash function that uses helpers.sh assertions.

### #3 Create test/entrypoint.sh — container-side env setup and test runner
- **Blocked by**: #2
- **Status**: done
- **Description**: Implement the container entrypoint that: (1) creates two loopback btrfs images and mounts them, (2) creates the source subvolume with seed data, (3) generates a throwaway GPG key, (4) starts MinIO in the background and creates the bucket, (5) writes the test config and exports AWS env vars, (6) sources test_cases.sh and runs each test sequentially with state reset between tests, (7) reports pass/fail summary and exits with appropriate code.

### #4 Create test/nspawn-rootfs.nix — Nix expression for container rootfs
- **Blocked by**: (none)
- **Status**: done
- **Description**: Write a Nix expression that builds a minimal rootfs directory containing all required packages (btrfs-progs, gnupg, awscli2, minio, util-linux, coreutils, bash, tar).

### #5 Create test/run.sh — host-side entry point
- **Blocked by**: #3, #4
- **Status**: done
- **Description**: Implement the host-side script that: (1) builds (or reuses cached) rootfs via nspawn-rootfs.nix, (2) launches systemd-nspawn with the correct flags (`--capability=CAP_SYS_ADMIN`, `--bind-ro` for project dir, `--property=DeviceAllow`, `--bind=/dev/loop-control`, `--bind=/dev/loopN`), (3) propagates the container exit code.

### #6 Verify AWS_ENDPOINT_URL support in btrshot.sh
- **Blocked by**: (none)
- **Status**: done
- **Description**: Check whether the `aws` CLI calls in `btrshot.sh` work with `AWS_ENDPOINT_URL` (AWS CLI v2). If not, either patch the script to accept an endpoint override or add a wrapper in the test entrypoint. This must be resolved before tests can pass against MinIO.
- **Result**: Verified — all five `aws` calls use standard commands with no hardcoded endpoint. AWS CLI v2 respects `AWS_ENDPOINT_URL` natively. No script changes required; the test entrypoint only needs `export AWS_ENDPOINT_URL=http://localhost:9000`.

### #7 End-to-end validation — run the full test suite
- **Blocked by**: #5, #6
- **Status**: pending
- **Description**: Run `sudo test/run.sh` on the host, verify all 10 tests pass, and fix any issues discovered.

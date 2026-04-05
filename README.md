# btrshot

## Running Tests

### Default (NixOS QEMU VM)

Requires: `nix` (with flakes enabled), `docker`

```sh
test/run.sh
```

No `sudo` or privileged containers needed. btrfs operations run inside a QEMU VM with full kernel access. floci (S3 emulator) runs in an unprivileged Docker container on the host.

### Docker fallback

Requires: `docker compose` (privileged container support)

```sh
test/run.sh --docker
```

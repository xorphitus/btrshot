# btrshot

## Installation (NixOS)

### As a flake input

Add btrshot to your `flake.nix` inputs and import the NixOS module:

```nix
{
  inputs.btrshot.url = "github:xorphitus/btrshot";

  outputs = { self, nixpkgs, btrshot, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      modules = [
        btrshot.nixosModules.default
        {
          services.btrshot = {
            enable = true;
            sourcePath = "/mnt/diskA";
            sourceSubvolume = "data";
            backupPath = "/mnt/diskB";
            s3Bucket = "your-bucket-name";
            gpgPublicKeyFile = "/etc/btrshot/backup-key.pub";
          };
        }
      ];
    };
  };
}
```

This sets up the systemd service, timer, and config file automatically. See `module.nix` for all available options (e.g. `s3RetentionCount`, `fullBackupInterval`, `awsEnvironmentFile`).

### Standalone (without the module)

```sh
nix build github:xorphitus/btrshot#btrshot
```

This produces `result/bin/btrshot` and `result/bin/btrshot-restore` with all runtime dependencies bundled via `wrapProgram`.

## Running Tests

### Default (NixOS QEMU VM)

Requires: `nix` (with flakes enabled), `docker`

```sh
test/run.sh
```

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
nix profile install github:xorphitus/btrshot
```

You can also build the package explicitly:

```sh
nix build github:xorphitus/btrshot#btrshot
```

This produces `result/bin/btrshot` and `result/bin/btrshot-restore` with all runtime dependencies bundled via `wrapProgram`.

## Running Tests

### Default (sandboxed NixOS test)

Requires: `nix` with flakes enabled.

```sh
test/run.sh
```

This runs `.#checks.<system>.integration`, which executes the integration suite inside a NixOS VM with a local S3-compatible server. No host Docker setup is required.

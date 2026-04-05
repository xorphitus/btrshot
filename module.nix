{ config, lib, pkgs, ... }:

let
  cfg = config.services.btrshot;
  confFile = pkgs.writeText "btrshot.conf" ''
    SOURCE_PATH="${cfg.sourcePath}"
    SOURCE_SUBVOLUME="${cfg.sourceSubvolume}"
    BACKUP_PATH="${cfg.backupPath}"
    S3_BUCKET="${cfg.s3Bucket}"
    S3_RETENTION_COUNT=${toString cfg.s3RetentionCount}
    GPG_PUBLIC_KEY_FILE="${cfg.gpgPublicKeyFile}"
    FULL_BACKUP_INTERVAL=${toString cfg.fullBackupInterval}
    INCREMENTAL_INTERVAL=${toString cfg.incrementalInterval}
    STATE_DIR="${cfg.stateDir}"
  '';
in
{
  options.services.btrshot = {
    enable = lib.mkEnableOption "btrshot btrfs backup service";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.btrshot or (pkgs.callPackage ./package.nix { });
      description = "The btrshot package to use.";
    };

    sourcePath = lib.mkOption {
      type = lib.types.str;
      description = "Mount point of the source btrfs filesystem.";
    };

    sourceSubvolume = lib.mkOption {
      type = lib.types.str;
      description = "Name of the btrfs subvolume to snapshot.";
    };

    backupPath = lib.mkOption {
      type = lib.types.str;
      description = "Mount point of the backup btrfs filesystem.";
    };

    s3Bucket = lib.mkOption {
      type = lib.types.str;
      description = "S3 bucket (and optional prefix) for encrypted backups.";
    };

    s3RetentionCount = lib.mkOption {
      type = lib.types.int;
      default = 10;
      description = "Number of most-recent S3 objects to keep.";
    };

    gpgPublicKeyFile = lib.mkOption {
      type = lib.types.str;
      description = "Path to the GPG public key file for encryption.";
    };

    fullBackupInterval = lib.mkOption {
      type = lib.types.int;
      default = 604800;
      description = "Seconds between full backups (default: 7 days).";
    };

    incrementalInterval = lib.mkOption {
      type = lib.types.int;
      default = 86400;
      description = "Seconds between incremental backups (default: 24 hours).";
    };

    stateDir = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/btrshot";
      description = "Directory for btrshot state and timestamp files.";
    };

    timerOnBootSec = lib.mkOption {
      type = lib.types.str;
      default = "5min";
      description = "Time after boot before first backup run.";
    };

    timerOnUnitActiveSec = lib.mkOption {
      type = lib.types.str;
      default = "2h";
      description = "Interval between backup runs.";
    };

    awsEnvironmentFile = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = "/etc/btrshot/aws.env";
      description = "Path to an environment file with AWS credentials. Set to null to disable.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.etc."btrshot/btrshot.conf".source = confFile;

    environment.systemPackages = [ cfg.package ];

    systemd.services.btrshot = {
      description = "btrshot backup (btrfs snapshot to local + S3)";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      unitConfig = {
        ConditionPathIsMountPoint = [
          cfg.sourcePath
          cfg.backupPath
        ];
      };
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${cfg.package}/bin/btrshot";
        StandardOutput = "journal";
        StandardError = "journal";
        Environment = [ "BTRSHOT_CONFIG=/etc/btrshot/btrshot.conf" ];
      } // lib.optionalAttrs (cfg.awsEnvironmentFile != null) {
        EnvironmentFile = "-${cfg.awsEnvironmentFile}";
      };
    };

    systemd.timers.btrshot = {
      description = "btrshot backup timer";
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnBootSec = cfg.timerOnBootSec;
        OnUnitActiveSec = cfg.timerOnUnitActiveSec;
        Unit = "btrshot.service";
      };
    };
  };
}

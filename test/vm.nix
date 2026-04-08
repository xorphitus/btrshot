{ pkgs, projectSrc, btrshotPackage, ... }:

let
  pythonEnv = pkgs.python3.withPackages (ps: [
    ps.pytest
    ps.moto
    ps.flask
    ps.flask-cors
    ps.werkzeug
  ]);
in
{
  boot.supportedFilesystems = [ "btrfs" ];

  virtualisation.graphics = false;
  virtualisation.memorySize = 512;
  virtualisation.emptyDiskImages = [ 64 64 ];

  environment.systemPackages = with pkgs; [
    btrshotPackage
    btrfs-progs
    gnupg
    awscli2
    util-linux
    coreutils
    bash
    gnutar
    gzip
    findutils
    gnugrep
    gnused
    gawk
    kmod
    rsync
    pythonEnv
  ];

  environment.etc."btrshot".source = projectSrc;

  systemd.services.s3-mock = {
    description = "Local S3-compatible object storage for btrshot tests";
    wantedBy = [ "multi-user.target" ];
    after = [ "network.target" ];
    serviceConfig = {
      Type = "simple";
      ExecStart = "${pythonEnv}/bin/moto_server -H 127.0.0.1 -p 9000";
      Restart = "on-failure";
    };
  };

  system.stateVersion = "24.11";
}

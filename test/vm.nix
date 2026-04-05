{ pkgs, lib, projectSrc, ... }:

{
  # --------------------------------------------------------------------------
  # VM hardware
  # --------------------------------------------------------------------------
  virtualisation.graphics = false;
  virtualisation.memorySize = 512;
  # Two empty 64 MB disks → /dev/vdb and /dev/vdc
  virtualisation.emptyDiskImages = [ 64 64 ];

  # 9p shared directory for passing results back to the host
  virtualisation.sharedDirectories.results = {
    source = "/tmp/btrshot-test-results";
    target = "/results";
  };

  # --------------------------------------------------------------------------
  # Packages available inside the VM
  # --------------------------------------------------------------------------
  environment.systemPackages = with pkgs; [
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
    (python3.withPackages (ps: [ ps.pytest ]))
  ];

  # --------------------------------------------------------------------------
  # Project source inside the VM
  # --------------------------------------------------------------------------
  environment.etc."btrshot".source = projectSrc;

  # --------------------------------------------------------------------------
  # Auto-run tests on boot, then power off
  # --------------------------------------------------------------------------
  systemd.services.btrshot-test = {
    description = "btrshot integration test suite";
    wantedBy = [ "multi-user.target" ];
    after = [ "network-online.target" ];
    wants = [ "network-online.target" ];
    path = with pkgs; [
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
      (python3.withPackages (ps: [ ps.pytest ]))
    ];
    serviceConfig = {
      Type = "oneshot";
      ExecStart = "${pkgs.bash}/bin/bash /etc/btrshot/test/vm-entrypoint.sh";
      StandardOutput = "journal+console";
      StandardError = "journal+console";
    };
  };

  # Power off after the test service finishes (success or failure)
  systemd.services.btrshot-test-shutdown = {
    description = "Shut down VM after tests";
    wantedBy = [ "multi-user.target" ];
    after = [ "btrshot-test.service" ];
    serviceConfig = {
      Type = "oneshot";
      ExecStart = "${pkgs.systemd}/bin/systemctl poweroff";
    };
  };
}

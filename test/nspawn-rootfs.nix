# Builds a minimal rootfs directory for systemd-nspawn containing all packages
# required by the btrshot integration test suite.
#
# Usage:
#   nix-build test/nspawn-rootfs.nix
#   # result is a directory suitable for: systemd-nspawn --directory=$(readlink result) ...
#
# IMPORTANT: The rootfs contains symlinks into /nix/store. The host-side
# run.sh must bind-mount the Nix store into the container:
#   systemd-nspawn --bind-ro=/nix/store ...
{ pkgs ? import <nixpkgs> { } }:

let
  env = pkgs.buildEnv {
    name = "btrshot-test-env";
    paths = with pkgs; [
      btrfs-progs
      gnupg
      awscli2
      minio
      util-linux
      coreutils
      bash
      gnutar
      gzip
      findutils
      gnugrep
      gnused
      gawk
      procps     # pgrep / pkill used by tests
      iproute2   # optional; useful for debugging inside container
      kmod       # modprobe (loop module)
    ];
    pathsToLink = [ "/bin" "/lib" "/libexec" "/share" "/etc" ];
  };
in
pkgs.runCommand "btrshot-test-rootfs" { } ''
  mkdir -p $out/{sbin,usr/bin,usr/sbin,etc,tmp,var,run,proc,sys,dev,mnt,nix,opt}

  # Symlink the merged environment directories into the rootfs.
  # Use directory-level symlinks so all binaries are reachable without
  # per-file globbing (which doesn't work in Nix build scripts).
  ln -s ${env}/bin $out/bin
  if [ -d ${env}/lib ]; then
    ln -s ${env}/lib $out/lib
  fi
  if [ -d ${env}/libexec ]; then
    ln -s ${env}/libexec $out/libexec
  fi
  if [ -d ${env}/share ]; then
    ln -s ${env}/share $out/share
  fi

  # /etc: copy as symlinks so the container can write additional files
  if [ -d ${env}/etc ]; then
    cp -rs ${env}/etc/* $out/etc/ 2>/dev/null || true
  fi

  # FHS compatibility: /usr/bin/env is needed by #!/usr/bin/env bash
  ln -s ${env}/bin/env $out/usr/bin/env
''

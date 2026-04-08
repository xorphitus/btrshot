{
  lib,
  stdenv,
  makeWrapper,
  btrfs-progs,
  gnupg,
  awscli2,
  coreutils,
  gnutar,
  gawk,
  gnugrep,
  gnused,
  findutils,
  util-linux,
  rsync,
  bash,
}:

let
  runtimeDeps = [
    btrfs-progs
    gnupg
    awscli2
    coreutils
    gnutar
    gawk
    gnugrep
    gnused
    findutils
    util-linux
    rsync
    bash
  ];
in
stdenv.mkDerivation {
  pname = "btrshot";
  version = "0.1.0";

  src = ./.;

  nativeBuildInputs = [ makeWrapper ];

  postPatch = ''
    patchShebangs btrshot.sh btrshot-restore.sh
  '';

  installPhase = ''
    runHook preInstall

    install -Dm755 btrshot.sh "$out/bin/btrshot"
    install -Dm755 btrshot-restore.sh "$out/bin/btrshot-restore"
    install -Dm644 btrshot.conf.example "$out/share/btrshot/btrshot.conf.example"

    runHook postInstall
  '';

  postFixup = ''
    for bin in "$out/bin/btrshot" "$out/bin/btrshot-restore"; do
      wrapProgram "$bin" --prefix PATH : "${lib.makeBinPath runtimeDeps}"
    done
  '';

  meta = {
    description = "btrfs incremental backup to local disk and S3";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "btrshot";
  };
}

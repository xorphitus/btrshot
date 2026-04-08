{ pkgs, projectSrc, btrshotPackage }:

pkgs.testers.runNixOSTest {
  name = "btrshot-integration";

  nodes.machine = import ./vm.nix {
    inherit pkgs projectSrc btrshotPackage;
  };

  testScript = ''
    start_all()

    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("s3-mock.service")
    machine.succeed("mkfs.btrfs -f -M -m single /dev/vdb")
    machine.succeed("mkfs.btrfs -f -M -m single /dev/vdc")
    machine.succeed("mkdir -p /mnt/A /mnt/B")
    machine.succeed("mount /dev/vdb /mnt/A")
    machine.succeed("mount /dev/vdc /mnt/B")
    machine.succeed("PROJECT_DIR=/etc/btrshot python3 -m pytest /etc/btrshot/test -v --tb=short")
  '';
}

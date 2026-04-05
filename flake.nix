{
  description = "btrshot – btrfs incremental backup system";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};

      testVm = nixpkgs.lib.nixosSystem {
        inherit system;
        specialArgs = {
          projectSrc = self;
        };
        modules = [
          ./test/vm.nix
          ({ modulesPath, ... }: {
            imports = [ "${modulesPath}/virtualisation/qemu-vm.nix" ];
            # Minimal system config
            boot.loader.grub.enable = false;
            fileSystems."/" = {
              device = "/dev/disk/by-label/nixos";
              fsType = "ext4";
            };
            system.stateVersion = "24.11";
            networking.hostName = "btrshot-test";
            users.users.root.initialPassword = "test";
          })
        ];
      };
    in
    {
      packages.${system} = {
        test-vm = testVm.config.system.build.vm;
        default = testVm.config.system.build.vm;
      };
    };
}

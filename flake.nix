{
  description = "btrshot – btrfs incremental backup system";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      lib = nixpkgs.lib;
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = lib.genAttrs supportedSystems;
      mkPkgs = system: import nixpkgs { inherit system; };
    in
    {
      packages = forAllSystems (system:
        let
          pkgs = mkPkgs system;
          btrshot = pkgs.callPackage ./package.nix { };
        in
        {
          inherit btrshot;
          default = btrshot;
        });

      checks = forAllSystems (system:
        let
          pkgs = mkPkgs system;
          btrshot = self.packages.${system}.btrshot;
        in
        {
          integration = import ./test/integration.nix {
            inherit pkgs;
            btrshotPackage = btrshot;
            projectSrc = self;
          };
        });

      overlays.default = final: prev: {
        btrshot = final.callPackage ./package.nix { };
      };

      nixosModules.default = { lib, pkgs, ... }: {
        imports = [ ./module.nix ];
        services.btrshot.package = lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.btrshot;
      };
    };
}

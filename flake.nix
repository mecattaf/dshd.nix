{
  description = "Durable native DSH sessions on NixOS";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      # The checks run NixOS VMs; only the system they are run on carries them.
      checkedSystem = "x86_64-linux";
    in
    {
      lib = import ./lib { inherit (nixpkgs) lib; };

      nixosModules = {
        dshd = ./nixos-modules/dshd;
        default = self.nixosModules.dshd;
      };

      overlays.default = final: _prev: {
        dshd = final.callPackage ./pkgs/dshd.nix { };
      };

      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          dshd = pkgs.callPackage ./pkgs/dshd.nix { };
          default = self.packages.${system}.dshd;
        }
      );

      checks.${checkedSystem} = import ./checks {
        inherit self;
        pkgs = nixpkgs.legacyPackages.${checkedSystem};
      };

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt);
    };
}

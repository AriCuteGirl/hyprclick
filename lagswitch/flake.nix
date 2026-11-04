{
  description = "lagswitch: per-app network freezer and fake lag for Linux";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      forAll = f: nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ] (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAll (pkgs: rec {
        lagswitch = pkgs.callPackage ./nix/package.nix { };
        default = lagswitch;
      });

      nixosModules.default = { lib, pkgs, ... }: {
        imports = [ ./nix/module.nix ];
        services.lagswitch.package = lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.lagswitch;
      };
    };
}

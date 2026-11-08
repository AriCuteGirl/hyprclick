{
  description = "hyprclick: background autoclicker with anti-AFK and lag tools for Hyprland, Sway and KDE Plasma";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      forAll = f: nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ] (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAll (pkgs: rec {
        hyprclick = pkgs.callPackage ./nix/package.nix { };
        lagswitch = pkgs.callPackage ./lagswitch/nix/package.nix { };
        default = hyprclick;
      });

      checks.x86_64-linux.vm = import ./nix/test.nix {
        pkgs = nixpkgs.legacyPackages.x86_64-linux;
        module = self.nixosModules.default;
      };

      # programs.hyprclick and services.lagswitch, built with this flake's nixpkgs.
      nixosModules.default = { lib, pkgs, ... }:
        let
          built = self.packages.${pkgs.stdenv.hostPlatform.system};
        in
        {
          imports = [ ./nix/module.nix ./lagswitch/nix/module.nix ];
          programs.hyprclick.package = lib.mkDefault built.hyprclick;
          services.lagswitch.package = lib.mkDefault built.lagswitch;
        };
    };
}

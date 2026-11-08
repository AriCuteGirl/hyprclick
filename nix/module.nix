# NixOS module: installs hyprclick and gives its users /dev/uinput, which
# Cursor mode and Gamepad anti-AFK need.
{ config, lib, pkgs, ... }:

let
  cfg = config.programs.hyprclick;
in
{
  options.programs.hyprclick = {
    enable = lib.mkEnableOption "hyprclick, a background autoclicker";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./package.nix { };
      defaultText = lib.literalExpression "pkgs.callPackage ./package.nix { }";
      description = "The hyprclick package.";
    };

    users = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "ari" ];
      description = "Users who get /dev/uinput access, through the uinput group.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
    hardware.uinput.enable = true;
    # The window's icons, when the desktop font has none.
    fonts.packages = [ pkgs.nerd-fonts.symbols-only ];
    users.groups.uinput.members = cfg.users;
  };
}

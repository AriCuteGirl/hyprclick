# NixOS module: runs lagswitchd as a root service for one user.
{ config, lib, pkgs, ... }:

let
  cfg = config.services.lagswitch;
in
{
  options.services.lagswitch = {
    enable = lib.mkEnableOption "lagswitch, a per-app network freezer with fake lag";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./package.nix { };
      defaultText = lib.literalExpression "pkgs.callPackage ./package.nix { }";
      description = "The lagswitch package.";
    };

    user = lib.mkOption {
      type = lib.types.str;
      example = "ari";
      description = "The one user allowed to use lagswitch. It only freezes that user's apps.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    systemd.services.lagswitchd = {
      description = "lagswitch per-app network freeze daemon";
      after = [ "network-pre.target" ];
      wantedBy = [ "multi-user.target" ];
      path = [ pkgs.nftables pkgs.iproute2 ];
      serviceConfig = {
        ExecStart = "${cfg.package}/bin/lagswitchd --user ${cfg.user}";
        # Still ends a freeze if the daemon is killed outright.
        ExecStopPost = "${cfg.package}/bin/lagswitchd --release";
        Restart = "on-failure";
        RestartSec = 1;
        RuntimeDirectory = "lagswitch";
        RuntimeDirectoryMode = "0755";
        RuntimeDirectoryPreserve = "restart";
        CapabilityBoundingSet = [ "CAP_NET_ADMIN" "CAP_CHOWN" "CAP_FOWNER" ];
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        RestrictAddressFamilies = [ "AF_UNIX" "AF_NETLINK" ];
      };
    };
  };
}

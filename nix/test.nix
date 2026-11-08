# NixOS VM test: the module on a Sway machine, plus a web server to measure
# freezes and fake lag against. Run with: nix build .#checks.x86_64-linux.vm -L
{ pkgs, module }:

pkgs.testers.runNixOSTest {
  name = "hyprclick";

  nodes.server = {
    networking.firewall.allowedTCPPorts = [ 80 ];
    services.nginx = {
      enable = true;
      virtualHosts.default = {
        default = true;
        locations."/".return = "200 ok";
      };
    };
  };

  nodes.laptop = { pkgs, ... }: {
    imports = [ module ];
    users.users.alice.isNormalUser = true;
    users.users.bob.isNormalUser = true;
    programs.hyprclick = {
      enable = true;
      users = [ "alice" ];
    };
    services.lagswitch = {
      enable = true;
      user = "alice";
    };

    programs.sway.enable = true;
    services.getty.autologinUser = "alice";
    environment.loginShellInit = ''
      if [ "$(tty)" = /dev/tty1 ]; then
        mkdir -p ~/.config/sway
        cp -n /etc/sway/config ~/.config/sway/config
        exec sway > /tmp/sway.log 2>&1
      fi
    '';
    environment.variables.WLR_RENDERER = "pixman";
    virtualisation.qemu.options = [ "-vga none" "-device virtio-gpu-pci" ];
    virtualisation.memorySize = 2048;
    environment.systemPackages = [ pkgs.curl pkgs.python3 ];
  };

  testScript = ''
    import shlex

    start_all()
    server.wait_for_unit("nginx.service")
    laptop.wait_for_unit("lagswitchd.service")
    laptop.wait_until_succeeds("ls /run/user/1000/sway-ipc.*.sock", timeout=120)
    sock = laptop.succeed("ls /run/user/1000/sway-ipc.*.sock").strip()

    def alice(cmd):
        env = f"XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus WAYLAND_DISPLAY=wayland-1 SWAYSOCK={sock}"
        return laptop.succeed(f"su - alice -c {shlex.quote(env + ' ' + cmd)}")

    with subtest("hyprclick setup binds keys in Sway"):
        # cp keeps the read-only mode of NixOS's /etc/sway/config, like a real user's copy.
        alice("test ! -w ~/.config/sway/config")
        out = alice("hyprclick setup")
        print(out)
        assert "fake lag" in out, out
        conf = alice("cat ~/.config/sway/hyprclick.conf")
        assert "/run/current-system/sw/bin/lagswitch --notify lag toggle" in conf, conf
        assert "/run/current-system/sw/bin/hyprclick toggle" in conf, conf
        alice("grep -q 'include .*hyprclick.conf' ~/.config/sway/config")
        assert "hyprclick" in alice("swaymsg -t get_config -r")

    with subtest("the window opens and floats"):
        alice("hyprclick gui > /tmp/gui.log 2>&1 &")
        laptop.wait_until_succeeds(f"su - alice -c 'SWAYSOCK={sock} swaymsg -t get_tree' | grep -q '\"app_id\": \"hyprclick\"'", timeout=60)
        laptop.sleep(2)
        laptop.screenshot("hyprclick-on-sway")

    with subtest("uinput works for alice"):
        laptop.succeed("stat -c %G /dev/uinput | grep -qx uinput")
        alice("test -w /dev/uinput")

    # A throwaway app in its own scope, measuring TCP connect time to the server.
    alice("systemd-run --user --scope --unit=target sh -c 'while :; do curl -s -o /dev/null -m 3 -w \"%{time_connect}\\n\" http://server/ >> /tmp/rtt || echo x >> /tmp/rtt; sleep 0.2; done' > /dev/null 2>&1 &")
    laptop.wait_until_succeeds("test -s /tmp/rtt")
    pid = alice("cat /sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/target.scope/cgroup.procs | head -1").strip()

    def last_rtt():
        laptop.sleep(2)
        value = laptop.succeed("tail -1 /tmp/rtt").strip()
        return None if value == "x" else float(value) * 1000

    with subtest("fake lag adds to the app's ping only"):
        before = last_rtt()
        print(alice(f"lagswitch lag on --pid {pid} --ms 300"))
        lagged = last_rtt()
        assert lagged is not None and 250 < lagged - before < 400, (before, lagged)
        other = float(laptop.succeed("curl -s -o /dev/null -w '%{time_connect}' http://server/")) * 1000
        assert other < 100, other
        print(alice("lagswitch lag off"))
        after = last_rtt()
        assert after is not None and after < 100, after

    with subtest("a freeze cuts the app off and ends by itself"):
        lines = int(laptop.succeed("wc -l < /tmp/rtt"))
        print(alice(f"lagswitch freeze --pid {pid} --ms 3000 --dir both"))
        laptop.wait_until_succeeds("su - alice -c 'lagswitch status' | grep -qx idle", timeout=10)
        # A blocked connect gives up after curl's 3 s, so look for that failure afterwards.
        laptop.wait_until_succeeds(f"tail -n +{lines + 1} /tmp/rtt | grep -qx x", timeout=10)
        laptop.wait_until_succeeds("tail -1 /tmp/rtt | grep -vx x", timeout=10)

    with subtest("the daemon only takes alice"):
        out = laptop.fail("su - bob -c 'lagswitch status' 2>&1")
        assert "permission" in out.lower(), out
  '';
}

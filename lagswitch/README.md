# lagswitch

A per-app network freezer for Linux. Pick a running app and its network traffic
stops for as long as you choose, while the app keeps running and the rest of your
PC stays online. Nothing is reset, so connections carry on when the freeze ends,
as long as the other side hasn't given up on you by then.

Built on EndeavourOS with Hyprland and kernel 6.18. It needs systemd, cgroup v2 and
nftables 1.0.8 or newer.

**Using this in online games can break their rules and get your account banned.**
It can't hide anything from a game server.

## Install

From this folder:

```bash
bash scripts/install.sh
```

This builds as your user, then uses sudo to put `lagswitch` and `lagswitchd` in
`/usr/local/bin` and enable the `lagswitchd` systemd service. Remove it all with:

```bash
bash scripts/uninstall.sh
```

## Use

```bash
lagswitch list                                  # what can be frozen, Flatpak apps first
lagswitch list example                          # the same, filtered
lagswitch freeze --app org.example.App --ms 5000
lagswitch toggle --app org.example.App
lagswitch hold start --app org.example.App
lagswitch hold stop
lagswitch release                               # end any freeze now
lagswitch status
```

`--app` takes a Flatpak app ID, part of one, or an exact process name. `--pid` takes
a process ID. Add `--dir in`, `--dir out` or `--dir both` (the default) to choose a
direction, and `--cap-ms N` to change the failsafe for that freeze.

| Mode | Start | Ends when |
|---|---|---|
| Timed | `freeze` | The timer runs out, or you run `freeze` again |
| Toggle | `toggle` | You run `toggle` again |
| Hold | `hold start` | You run `hold stop` |

Every mode also stops at the failsafe cap: 30 seconds by default, and never more
than 5 minutes whatever the config says.

## Config

`~/.config/lagswitch/config.toml`. Without `--app` or `--pid`, the last app is used,
which keeps keybinds short.

```toml
default_ms = 5000
direction = "both"
max_cap_ms = 30000
last_app = "org.example.App"
```

Change it from the command line with `lagswitch set`:

```bash
lagswitch set --app org.example.App --ms 5000 --dir both --cap-ms 30000
```

## Hyprland keys

[hyprclick](../README.md)'s Lag tab has an app picker,
settings and recordable keys, and writes the binds for you. To bind them by hand in
Lua config:

```lua
hl.bind("ALT + Z", hl.dsp.exec_cmd("/usr/local/bin/lagswitch freeze"))
hl.bind("ALT + X", hl.dsp.exec_cmd("/usr/local/bin/lagswitch toggle"))
hl.bind("ALT + C", hl.dsp.exec_cmd("/usr/local/bin/lagswitch hold start"))
hl.bind("ALT + C", hl.dsp.exec_cmd("/usr/local/bin/lagswitch hold stop"), { release = true, ignore_mods = true })
```

Or in `hyprland.conf`:

```ini
bind = ALT, Z, exec, /usr/local/bin/lagswitch freeze
bind = ALT, X, exec, /usr/local/bin/lagswitch toggle
bind = ALT, C, exec, /usr/local/bin/lagswitch hold start
bindri = ALT, C, exec, /usr/local/bin/lagswitch hold stop
```

## How it works

Changing firewall rules needs root, so `lagswitchd` runs as a small root service and
everything else talks to it over `/run/lagswitch/sock`, which only your user can open.
The daemon owns the rules and the timer, so a freeze still ends on time if the client
dies. It will only freeze cgroups under your own `user.slice`, and it refuses ones
shared with your whole session.

All rules live in two tables named `lagswitch`, so your own firewall rules are never
touched. They are swapped in and out as one atomic change:

- **Incoming:** a rule in the input hook drops packets for the app's sockets using
  `socket cgroupv2`.
- **Outgoing:** the output hook only marks the app's packets, and a netdev egress chain
  drops them. Dropping in the output hook itself makes UDP `send()` fail with `EPERM`,
  which the app can notice and treat as a network failure. This way `send()` succeeds
  and the packets just vanish, like real packet loss.
- **Loopback** isn't touched, so an app's local connections keep working.

Rules are removed on normal exit, on SIGINT and SIGTERM, on panic, and when the daemon
starts. If the daemon is killed outright, systemd cleans up and restarts it.

## Limitations

- **Whole cgroups only.** Everything in a cgroup freezes together, and `lagswitch list`
  shows how many processes that is. Apps with no scope of their own sit in your login
  session and are marked `shared`, which can't be frozen. A game started by another
  program can share that program's cgroup, and then both freeze.
- **Sockets count, not processes.** Traffic is matched by the cgroup a socket was created
  in. A socket passed to a process outside the cgroup still matches the original cgroup.
- **Several instances.** Freezing an app ID freezes every running instance of it.
- **Network devices** are listed when a freeze starts. One added during a freeze isn't
  covered for outgoing traffic until the next freeze.
- **Incoming-only freezes** can briefly slow other apps talking to the same server as the
  frozen app, and a fetch right after one can occasionally fail. Other servers weren't
  affected in testing and the cause isn't known. This wasn't seen with `both` or `out`.
- **Incoming ICMP** isn't matched, because it has no socket. Pings stop with `both` or
  `out`, since the outgoing request is dropped.
- **IPv6** is covered by the rules but untested, because the test machine has no IPv6
  connection.
- **Long freezes** can still get you kicked if a game times out silent clients. The limit
  depends on the game.

## Testing

```bash
cargo test
```

These need the installed daemon and sudo, and use a throwaway Flatpak sandbox (Okular by
default, or pass another app that isn't running):

```bash
bash scripts/verify-daemon.sh org.kde.okular
```

```bash
bash scripts/check-host.sh org.kde.okular
```

`scripts/probe-inbound.py` is the original test of which netfilter hooks can match
`socket cgroupv2` on a given kernel, and what the app sees when packets drop.

## Dependencies

`libc` only, pinned to an exact version. No telemetry.

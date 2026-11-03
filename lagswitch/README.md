# lagswitch

A network freezer for Linux with two kinds of freeze, plus fake lag:

- **Whole PC:** everything on the PC loses its network at once, as if it were unplugged,
  including voice chat and streams. It comes back instantly when the freeze ends,
  because the connection itself never went down.
- **One app:** the same for one app, while it keeps running and the rest of your PC
  stays online.

- **Fake lag:** one app's ping goes up by the amount you set, for as long as you leave
  it on. The app stays connected, and everything else keeps its normal ping.

Nothing is reset in either freeze mode, so connections carry on when the freeze ends, as long
as the other side hasn't given up on you by then.

Built on EndeavourOS with Hyprland and kernel 6.18. It needs systemd, cgroup v2,
nftables 1.0.8 or newer and iproute2.

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
lagswitch freeze --whole-pc --ms 5000           # the whole PC loses its network
lagswitch release                               # end any freeze now
lagswitch status
lagswitch lag on --app org.example.App --ms 150   # +150 ms ping
lagswitch lag toggle                            # on with the saved app and amount, or off
lagswitch lag off
```

`--whole-pc` also works with `toggle` and `hold start`. `--app` takes a Flatpak app ID, part of one, or an exact process name. `--pid` takes
a process ID. Add `--dir in`, `--dir out` or `--dir both` (the default) to choose a
direction, and `--cap-ms N` to change the failsafe for that freeze.

| Mode | Start | Ends when |
|---|---|---|
| Timed | `freeze` | The timer runs out, or you run `freeze` again |
| Toggle | `toggle` | You run `toggle` again |
| Hold | `hold start` | You run `hold stop` |

Every mode also stops at the failsafe cap: 30 seconds by default, and never more
than 5 minutes whatever the config says.

Fake lag is separate from freezes and has no cap: it stays on until `lag off` or
`lag toggle`, from 1 to 5000 ms. Running `lag on` again while it's on changes the
amount or the app. A freeze on a lagged app still freezes it, and the lag carries on
afterwards. It only works on one app, not the whole PC.

## Config

`~/.config/lagswitch/config.toml`. With no target named, a command uses the saved
choice, which keeps keybinds short: the whole PC if `whole_pc` is true, otherwise the
last app.

```toml
default_ms = 5000
direction = "both"
max_cap_ms = 30000
whole_pc = false
lag_ms = 150
last_app = "org.example.App"
```

Change it from the command line with `lagswitch set`:

```bash
lagswitch set --app org.example.App --ms 5000 --dir both --cap-ms 30000 --lag-ms 150
```

## Hyprland keys

[hyprclick](https://github.com/AriCuteGirl/hyprclick)'s Lag tab has an app picker,
settings and recordable keys, and writes the binds for you. To bind them by hand in
Lua config:

```lua
hl.bind("ALT + Z", hl.dsp.exec_cmd("/usr/local/bin/lagswitch freeze"))
hl.bind("ALT + X", hl.dsp.exec_cmd("/usr/local/bin/lagswitch toggle"))
hl.bind("ALT + C", hl.dsp.exec_cmd("/usr/local/bin/lagswitch hold start"))
hl.bind("ALT + C", hl.dsp.exec_cmd("/usr/local/bin/lagswitch hold stop"), { release = true })
hl.bind("ALT + Alt_L", hl.dsp.exec_cmd("/usr/local/bin/lagswitch hold stop"), { release = true, non_consuming = true })
hl.bind("ALT + B", hl.dsp.exec_cmd("/usr/local/bin/lagswitch lag toggle"))
```

Or in `hyprland.conf`:

```ini
bind = ALT, Z, exec, /usr/local/bin/lagswitch freeze
bind = ALT, X, exec, /usr/local/bin/lagswitch toggle
bind = ALT, C, exec, /usr/local/bin/lagswitch hold start
bindr = ALT, C, exec, /usr/local/bin/lagswitch hold stop
bindrn = ALT, Alt_L, exec, /usr/local/bin/lagswitch hold stop
bind = ALT, B, exec, /usr/local/bin/lagswitch lag toggle
```

Don't use `ignore_mods` on the release bind: Hyprland then swallows that letter
everywhere, even typed without ALT.

## How it works

### Whole PC

An `unreachable` routing rule at priority 1 makes every send fail straight away with
`ENETUNREACH`, the same error an app gets with no network. It sits just after the local
table, so loopback and the PC's own addresses keep working. A nft rule drops incoming
packets. Interfaces and addresses stay up, so ending a freeze is instant: both rules are
removed and there's nothing to reconnect. They don't survive a reboot, and the daemon
clears any left over when it starts.

### One app

Changing firewall rules needs root, so `lagswitchd` runs as a small root service and
everything else talks to it over `/run/lagswitch/sock`, which only your user can open.
The daemon owns the rules and the timer, so a freeze still ends on time if the client
dies. It will only freeze cgroups under your own `user.slice`, and it refuses ones
shared with your whole session.

Rules live in one table named `lagswitch`, so your own firewall rules are never
touched. They are swapped in and out as one atomic change:

- **Incoming:** a rule in the input hook drops packets for the app's sockets using
  `socket cgroupv2`.
- **Outgoing:** a rule in the output hook drops the app's packets, which makes its UDP
  `send()` fail straight away with `EPERM`, so the game sees its network fail.
- **Loopback** isn't touched, so an app's local connections keep working.

### Fake lag

A rule in the output hook and one in the input hook send the app's packets to netfilter
queue 4242, the same `socket cgroupv2` match as a freeze. The daemon listens on that
queue and lets each packet through after half the delay on the way out and half on the
way in, so the round trip a game measures grows by the full amount. It only reads each
packet's ID, never its contents.

The rules use `queue flags bypass`: if the daemon isn't listening, packets go straight
through rather than getting stuck. If more than 8192 packets are waiting at once, the
extra ones pass undelayed.

Rules are removed on normal exit, on SIGINT and SIGTERM, on panic, and when the daemon
starts. If the daemon is killed outright, systemd cleans up and restarts it.

## Limitations

- **Whole PC blocks everything**, including voice chat, and the direction setting doesn't
  apply to it. Traffic that's forwarded for VMs and containers is cut too.
- **One app mode freezes whole cgroups.** Everything in a cgroup freezes together, and `lagswitch list`
  shows how many processes that is. Apps with no scope of their own sit in your login
  session and are marked `shared`, which can't be frozen. A game started by another
  program can share that program's cgroup, and then both freeze.
- **Sockets count, not processes.** Traffic is matched by the cgroup a socket was created
  in. A socket passed to a process outside the cgroup still matches the original cgroup.
- **Several instances.** Freezing an app ID freezes every running instance of it.
- **Incoming-only freezes** can briefly slow other apps talking to the same server as the
  frozen app, and a fetch right after one can occasionally fail. Other servers weren't
  affected in testing and the cause isn't known. This wasn't seen with `both` or `out`.
- **Incoming ICMP** isn't matched, because it has no socket. Pings stop with `both` or
  `out`, since the outgoing request is dropped.
- **IPv6** is covered by the rules but untested, because the test machine has no IPv6
  connection.
- **Long freezes** can still get you kicked if a game times out silent clients. The limit
  depends on the game.
- **Fake lag follows the app's current cgroup.** If the app restarts, it gets a new one, so
  turn fake lag off and on again. Pings (ICMP) aren't delayed on the way in.

## Testing

```bash
cargo test
```

These need the installed daemon and sudo, and use a throwaway Flatpak sandbox (Okular by
default, or pass another app that isn't running):

```bash
bash scripts/verify-daemon.sh org.kde.okular
```

This one checks both modes and takes the whole PC offline several times:

```bash
bash scripts/verify-whole-pc.sh
```

Fake lag, on a throwaway process, measured over TCP and UDP (300 ms by default):

```bash
bash scripts/verify-lag.sh 300
```

```bash
bash scripts/check-host.sh org.kde.okular
```

`scripts/probe-inbound.py` is the original test of which netfilter hooks can match
`socket cgroupv2` on a given kernel, and what the app sees when packets drop.

## Dependencies

`libc` only, pinned to an exact version. No telemetry.

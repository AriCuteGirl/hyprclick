# hyprclick

Background autoclicker for **Hyprland** and **KDE Plasma**, written in Rust with an egui UI. Other desktops (GNOME, Sway, plain X11) aren't supported.

| | Hyprland | KDE Plasma 6 |
|---|---|---|
| Window mode (click a window in the background) | yes | no, see [KDE](#kde-plasma) |
| Cursor mode | yes | yes |
| Anti-AFK: Window | yes | no |
| Anti-AFK: Gamepad | yes | yes |
| Hotkey from `hyprclick setup` / Key tab | Hyprland bind (keys + mouse buttons) | KDE global shortcut (keys only) |

 The window takes its colors and font from the Noctalia rice (`~/.config/noctalia/rice.toml` → `palettes/<custom_palette>.json`), with the Ari palette built in as a fallback.

## Install

```bash
mkdir -p ~/.local/bin
curl -L -o ~/.local/bin/hyprclick https://github.com/AriCuteGirl/hyprclick/releases/latest/download/hyprclick-x86_64-linux
chmod +x ~/.local/bin/hyprclick
hyprclick setup
```

`hyprclick setup` binds ALT+H (pick any other key in the app's Key tab), makes the window float, and starts the virtual gamepad at login. Then open it with `hyprclick` or from your app launcher.

Needs on Hyprland:
- Hyprland, tested on **0.56.2** (Lua config; the classic `hyprland.conf` is supported but untested). The plugin is compiled against Hyprland's internals on first launch, so other versions may not build.
- `g++`, `pkg-config` and Hyprland's headers: run `hyprpm update` once (and again after Hyprland updates).
- For Cursor mode and Gamepad anti-AFK: access to `/dev/uinput` (see below).

Needs on KDE: Plasma 6 (Wayland or X11), `kwriteconfig6`/`kreadconfig6` (part of KDE's base install) and the same `/dev/uinput` access. No compiler or headers.

### `/dev/uinput` access

Cursor mode and Gamepad anti-AFK create a virtual mouse and pad, which needs write access to `/dev/uinput`. Once per machine:

```bash
sudo usermod -aG input $USER
echo 'KERNEL=="uinput", GROUP="input", MODE="0660"' | sudo tee /etc/udev/rules.d/99-hyprclick-uinput.rules
echo uinput | sudo tee /etc/modules-load.d/uinput.conf
```

Then reboot (or log out and back in, and run `sudo modprobe uinput && sudo udevadm trigger`). The error "/dev/uinput: Permission denied (are you in the 'input' group?)" means this step is missing.

## KDE Plasma

### Quick start

1. Run the [install](#install) commands in Konsole, and set up [`/dev/uinput` access](#devuinput-access) if you haven't.
2. Open hyprclick: `hyprclick` in Konsole (or `hyprclick & disown` to close the terminal afterwards).
3. **Target** tab: pick your game (Roblox is `org.vinegarhq.Sober`) from the dropdown.
4. Mode **Cursor**. To always click the same spot, turn on **Lock cursor to the picked spot**, then press **Pick on screen** and hover the spot in the game for 3 seconds.
5. **Click** tab: set the speed.
6. Press **Alt+H** to start, and again to stop. While the lock is on, your mouse is pulled back to the spot before every click, so use Alt+H rather than the button to stop.
7. Optional, **AFK** tab: choose **Gamepad** and switch Anti-AFK on. Roblox only reacts to it while you're inside a game.

### Troubleshooting

- **Alt+H does nothing**: run `hyprclick setup` again and check that "hyprclick" is listed in System Settings → Shortcuts. If you picked another key there, make sure it isn't already used by something else.
- **Clicking won't start, red "Window mode needs Hyprland"**: switch the mode to Cursor. Settings are shared between desktops, so a Window mode picked on Hyprland comes along. Picking Cursor on KDE also makes Hyprland open in Cursor mode until you switch back.
- **Sober (Roblox) won't open after switching sessions**, with an error about `/run/user/1000/doc`: Flatpak's file portal lost its mount. Fix: `systemctl --user restart xdg-document-portal`, then start Sober again.
- **The first clicks after opening hyprclick are missed**: KWin takes about a second to accept the new virtual mouse. Only the first start after opening is affected.

### What setup does

`hyprclick setup` on KDE:
- adds a hidden `~/.local/share/applications/hyprclick-toggle.desktop` that runs `hyprclick toggle` and binds it as a global shortcut through kglobalaccel. It shows up in System Settings → Shortcuts as "hyprclick". KDE global shortcuts can't be mouse buttons. To use a side button, bind a key here, then map the button to that key in System Settings → Mouse.
- adds a KWin window rule (`~/.config/kwinrulesrc`, group `hyprclick`) that opens the window centered.
- starts the virtual gamepad at login through `~/.config/autostart/hyprclick-pad.desktop`.

The window list, focus and cursor position come from small KWin scripts that hyprclick loads over D-Bus. Notifications go through the standard notification service.

**Window mode doesn't exist on KDE.** On Hyprland it works through a compositor plugin. KWin has no plugin API, and none of the ways around that work in the background:
- XTest from an X client (tested on KWin 6.8): KWin routes it into the compositor as real input, after a "Remote control" permission prompt, so it moves your real cursor.
- XSendEvent: produces synthetic core X events only. Sober loads XInput2 (libXi) for X11 input, so they most likely never reach it. This wasn't tried against Sober on KDE. A native Wayland window can't be reached from outside KWin at all.
- The RemoteDesktop portal / libei, and KWin's fake-input protocol: these drive the real seat pointer, which is what Cursor mode already does.
- KWin scripts and effects can't send input.

So on KDE use **Cursor** mode, with "Lock cursor to the picked spot" to always hit the same spot in the game (your mouse is taken over while it clicks) or "Only click while this window is focused", and **Gamepad** anti-AFK. A fresh config on KDE starts in those modes.

## Modes

- **Window**: clicks a chosen spot inside a chosen window. The window doesn't need focus and your cursor stays where it is. By default it pauses while the window's workspace isn't shown on any monitor, so Roblox on one screen keeps clicking while you use the other. A small Hyprland plugin (`plugin/hyprclick.cpp`) sends pointer events straight to the window's surface, timed by a timerfd inside Hyprland. It clicks at a picked spot (always that spot; your cursor gets pulled there if it's inside the window) or "where the mouse is" (right under your cursor while it's inside, else where it last was).
- **Cursor**: classic. A uinput virtual mouse clicks wherever the cursor is. Optionally it only clicks while the target window has focus, and/or locks the cursor to the picked spot in the target: before every click the real cursor is pulled back onto that spot, so your mouse is taken over while it runs. That's the closest thing to Window mode's picked spot on KDE.

Button: left/right/middle. Speed: 0.1–1000 CPS. Hold duration is off by default (instant press + release).

Fail-safe: clicking stops, with a notification, when the target window closes or crashes. A restarted game is a new window and is never clicked automatically.

## Anti-AFK

Every few minutes (default 10) it pokes the target window, so Roblox doesn't kick you for idling:

- **Window**: the plugin moves the target's own pointer 8 px in a random diagonal (or taps right-click), in the background, only for that window.
- **Gamepad**: the roblox-caffeine method. A virtual Xbox pad nudges both sticks; every game sees it, and Roblox only reacts while you're inside a game. The pad lives in `hyprclick pad`, which `hyprclick setup` starts at login, so it isn't re-plugged every time.

It's locked to the exact target window and stops when that window goes away. It runs from the app's AFK tab, or in a terminal with caffeine-style output: `hyprclick afk`.

## Hotkey

On KDE, see [KDE Plasma](#kde-plasma). On Hyprland, `hyprclick setup` writes the toggle hotkey and the small floating-window rule into `~/.config/hypr/hyprclick.lua` (or `hyprclick.conf` on the classic config). It adds one line to `hyprland.lua`/`hyprland.conf` to load it (with a backup), then reloads Hyprland. Change it from the app's Key tab by pressing the new combo (side mouse buttons work), or with `hyprclick setup 'ALT + J'`, `'CTRL + SHIFT + G'`, `'mouse:275'` (back), `'ALT + mouse:276'`.

## Use

```
hyprclick            # open the window
hyprclick toggle     # what the hotkey runs (default ALT+H)
hyprclick start|stop|status
```

`toggle` controls the open window if there is one. Otherwise it starts a headless daemon with the saved settings (`~/.config/hyprclick/config.json`), and the next toggle stops it.

## Plugin

The app embeds the plugin source. On first use it builds the plugin for the running Hyprland commit into `~/.local/share/hyprclick/` and loads it with `hyprctl plugin load`. After a Hyprland update it rebuilds automatically, and if the headers don't match it asks you to run `hyprpm update`. Manual build: `make -C plugin`.

XWayland details (Sober is XWayland):
- Xwayland handles `leave` right away but queues button events, so focus is handed back after 3 ms instead of immediately.
- Xwayland hit-tests by X stacking order, and windows on other workspaces overlap in X coordinates, so the target is raised in X (not on screen) before clicking.

## Build

```
cargo build --release && install -m755 target/release/hyprclick ~/.local/bin/
```

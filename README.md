# hyprclick

Background autoclicker for Hyprland, written in Rust with an egui UI. The window takes its colors and font from the Noctalia rice (`~/.config/noctalia/rice.toml` → `palettes/<custom_palette>.json`), with the Ari palette built in as a fallback.

## Install

```bash
mkdir -p ~/.local/bin
curl -L -o ~/.local/bin/hyprclick https://github.com/AriCuteGirl/hyprclick/releases/latest/download/hyprclick-x86_64-linux
chmod +x ~/.local/bin/hyprclick
hyprclick setup
```

`hyprclick setup` binds ALT+H (pick any other key in the app's Key tab), makes the window float, and starts the virtual gamepad at login. Then open it with `hyprclick` or from your app launcher.

Needs:
- Hyprland, tested on **0.56.2** (Lua config; the classic `hyprland.conf` is supported but untested). The plugin is compiled against Hyprland's internals on first launch, so other versions may not build.
- `g++`, `pkg-config` and Hyprland's headers: run `hyprpm update` once (and again after Hyprland updates).
- For Cursor mode and Gamepad anti-AFK: access to `/dev/uinput`, e.g. be in the `input` group with a rule like `KERNEL=="uinput", GROUP="input", MODE="0660"`.

## Modes

- **Window**: clicks a chosen spot inside a chosen window. The window doesn't need focus and your cursor stays where it is. By default it pauses while the window's workspace isn't shown on any monitor, so Roblox on one screen keeps clicking while you use the other. A small Hyprland plugin (`plugin/hyprclick.cpp`) sends pointer events straight to the window's surface, timed by a timerfd inside Hyprland. It clicks at a picked spot (always that spot; your cursor gets pulled there if it's inside the window) or "where the mouse is" (right under your cursor while it's inside, else where it last was).
- **Cursor**: classic. A uinput virtual mouse clicks wherever the cursor is. Optionally it only clicks while the target window has focus.

Button: left/right/middle. Speed: 0.1–1000 CPS. Hold duration is off by default (instant press + release).

Fail-safe: clicking stops, with a notification, when the target window closes or crashes. A restarted game is a new window and is never clicked automatically.

## Anti-AFK

Every few minutes (default 10) it pokes the target window, so Roblox doesn't kick you for idling:

- **Window**: the plugin moves the target's own pointer 8 px in a random diagonal (or taps right-click), in the background, only for that window.
- **Gamepad**: the roblox-caffeine method. A virtual Xbox pad nudges both sticks; every game sees it, and Roblox only reacts while you're inside a game. The pad lives in `hyprclick pad`, which `hyprclick setup` starts at login, so it isn't re-plugged every time.

It's locked to the exact target window and stops when that window goes away. It runs from the app's AFK tab, or in a terminal with caffeine-style output: `hyprclick afk`.

## Hotkey

`hyprclick setup` writes the toggle hotkey and the small floating-window rule into `~/.config/hypr/hyprclick.lua` (or `hyprclick.conf` on the classic config). It adds one line to `hyprland.lua`/`hyprland.conf` to load it (with a backup), then reloads Hyprland. Change it from the app's Key tab by pressing the new combo (side mouse buttons work), or with `hyprclick setup 'ALT + J'`, `'CTRL + SHIFT + G'`, `'mouse:275'` (back), `'ALT + mouse:276'`.

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

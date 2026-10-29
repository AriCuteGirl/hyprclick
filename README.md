# hyprclick

Background autoclicker for Hyprland, written in Rust with an egui UI. The window takes its colors and font from the Noctalia rice (`~/.config/noctalia/rice.toml` → `palettes/<custom_palette>.json`), with the Ari palette built in as a fallback.

## Modes

- **Window**: clicks a chosen spot inside a chosen window. The window doesn't need focus and your cursor stays where it is. By default it pauses while the window's workspace isn't shown on any monitor (so Roblox on one screen keeps clicking while you use the other). Turn that off to click on hidden workspaces too. A small Hyprland plugin (`plugin/hyprclick.cpp`) sends pointer events straight to the window's surface. Timing runs on a timerfd inside Hyprland, so there's no IPC per click.
- **Cursor**: classic. A uinput virtual mouse clicks wherever the cursor is. Optionally it only clicks while the target window has focus.
- **Gamepad**: a virtual Xbox 360 pad (the roblox-caffeine trick). Games read it in the background. In Roblox, RT uses the tool.

Window mode clicks either at a picked spot or "where the mouse is": the spot your mouse last was inside the window, so the game's pointer never moves.

Button: left/right/middle (or pad button). Speed: 0.1–1000 CPS. Hold duration is off by default, so press and release go out together.

## Use

```
hyprclick            # open the window
hyprclick toggle     # bound to ALT+H in ~/.config/hypr/custom/keybinds.lua
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

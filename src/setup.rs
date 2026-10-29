//! `hyprclick setup`: put the toggle hotkey and the small floating window
//! rule into Hyprland's config. Everything goes into one file hyprclick owns
//! (~/.config/hypr/hyprclick.lua or .conf) plus one line that loads it, so
//! the user's own files are only touched once, with a backup.

use crate::hotkey::Hotkey;
use crate::{hypr, APP_ID};
use std::path::{Path, PathBuf};

const WINDOW_SIZE: (u32, u32) = (380, 640);

fn hypr_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("hypr")
}

/// Path of the binary the hotkey should run.
fn exe() -> String {
    let installed = dirs::home_dir().unwrap_or_default().join(".local/bin/hyprclick");
    let path = if installed.exists() { installed } else { std::env::current_exe().unwrap_or(installed) };
    path.display().to_string()
}

/// Writes the managed file, makes sure Hyprland loads it, reloads, and
/// returns a short report (plus any warnings).
pub fn apply(hotkey: &Hotkey) -> Result<String, String> {
    let dir = hypr_dir();
    let lua = dir.join("hyprland.lua");
    let conf = dir.join("hyprland.conf");
    let (managed, main) = if lua.exists() {
        let file = dir.join("hyprclick.lua");
        std::fs::write(&file, lua_file(hotkey)).map_err(|e| e.to_string())?;
        ensure_line(&lua, "require(\"hyprclick\")", "\n-- hyprclick (added by `hyprclick setup`)\npcall(require, \"hyprclick\")\n")?;
        (file, lua)
    } else if conf.exists() {
        let file = dir.join("hyprclick.conf");
        std::fs::write(&file, conf_file(hotkey, true)).map_err(|e| e.to_string())?;
        ensure_line(&conf, "hyprclick.conf", "\n# hyprclick (added by `hyprclick setup`)\nsource = ~/.config/hypr/hyprclick.conf\n")?;
        (file, conf)
    } else {
        return Err(format!("no Hyprland config found in {}", dir.display()));
    };

    hypr::request("reload")?;
    let errors = hypr::request("configerrors").unwrap_or_default();
    let ours: Vec<&str> = errors.lines().filter(|l| l.contains("hyprclick")).collect();
    if !ours.is_empty() {
        // Window rule syntax differs between Hyprland versions; the bind is
        // what matters, so retry without the rules before giving up.
        if managed.extension().is_some_and(|e| e == "conf") {
            std::fs::write(&managed, conf_file(hotkey, false)).map_err(|e| e.to_string())?;
            hypr::request("reload")?;
            let errors = hypr::request("configerrors").unwrap_or_default();
            if !errors.lines().any(|l| l.contains("hyprclick")) {
                return Ok(format!("{} bound (window rules skipped: unsupported here)", hotkey.label()));
            }
        }
        return Err(format!("Hyprland rejected it: {}", ours.join(" / ")));
    }

    // Start the pad now too, so it's there before the next Roblox launch.
    let _ = crate::afk::ensure_pad_service();

    let mut report = format!("{} → hyprclick toggle  ({})", hotkey.label(), managed.display());
    let dupes = other_binds(&dir, &managed, &main);
    if !dupes.is_empty() {
        report += &format!("\nalso bound in: {} (remove those lines)", dupes.join(", "));
    }
    if hotkey.bare_mouse() {
        report += "\nnote: without a modifier that mouse button only toggles hyprclick now";
    }
    Ok(report)
}

fn lua_file(hk: &Hotkey) -> String {
    let (w, h) = WINDOW_SIZE;
    format!(
        "-- Managed by hyprclick: change the key in its Key tab, or run: hyprclick setup \"ALT + J\"\n\
         -- (edits here get overwritten)\n\n\
         hl.bind(\"{combo}\", hl.dsp.exec_cmd(\"{exe} toggle\"), {{ description = \"hyprclick: Toggle autoclicker\" }})\n\n\
         hl.window_rule({{match = {{class = \"^({APP_ID})$\" }}, float = true}})\n\
         hl.window_rule({{match = {{class = \"^({APP_ID})$\" }}, size = {{{w}, {h}}}}})\n\
         hl.window_rule({{match = {{class = \"^({APP_ID})$\" }}, center = true}})\n\n\
         -- Virtual gamepad for Gamepad anti-AFK; must exist before Roblox starts.\n\
         hl.on(\"hyprland.start\", function() hl.exec_cmd(\"{exe} pad\") end)\n",
        combo = hk.combo(),
        exe = exe(),
    )
}

fn conf_file(hk: &Hotkey, rules: bool) -> String {
    let (w, h) = WINDOW_SIZE;
    let mut out = format!(
        "# Managed by hyprclick: change the key in its Key tab, or run: hyprclick setup \"ALT + J\"\n\
         # (edits here get overwritten)\n\n\
         bind = {mods}, {key}, exec, {exe} toggle\n\
         exec-once = {exe} pad\n",
        mods = hk.mods.join(" "),
        key = hk.key,
        exe = exe(),
    );
    if rules {
        out += &format!(
            "\nwindowrule = float, class:^({APP_ID})$\nwindowrule = size {w} {h}, class:^({APP_ID})$\nwindowrule = center, class:^({APP_ID})$\n"
        );
    }
    out
}

/// Append `block` to `file` unless it already mentions `needle`. Backs the
/// file up once before the first change.
fn ensure_line(file: &Path, needle: &str, block: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    if text.contains(needle) {
        return Ok(());
    }
    let backup = file.with_extension(format!("{}.bak-before-hyprclick", file.extension().unwrap_or_default().to_string_lossy()));
    if !backup.exists() {
        std::fs::copy(file, &backup).map_err(|e| e.to_string())?;
    }
    std::fs::write(file, text + block).map_err(|e| e.to_string())
}

/// Other config files that also bind `hyprclick toggle` (old manual binds).
fn other_binds(dir: &Path, managed: &Path, main: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let name = p.to_string_lossy();
            if p == managed || p == main || name.contains(".bak") || !(name.ends_with(".lua") || name.ends_with(".conf")) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            let active = text.lines().any(|l| {
                let l = l.trim_start();
                !l.starts_with("--") && !l.starts_with('#') && l.contains("hyprclick") && l.contains("toggle")
            });
            if active {
                found.push(p.strip_prefix(dir).unwrap_or(&p).display().to_string());
            }
        }
    }
    found
}

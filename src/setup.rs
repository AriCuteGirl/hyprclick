//! `hyprclick setup`: put the toggle hotkey and the small floating window
//! rule into the compositor's config.
//!
//! Hyprland: everything goes into one file hyprclick owns
//! (~/.config/hypr/hyprclick.lua or .conf) plus one line that loads it, so
//! the user's own files are only touched once, with a backup.
//!
//! KDE: a hidden .desktop file that runs `hyprclick toggle`, bound as a
//! global shortcut through kglobalaccel (it shows up in System Settings →
//! Shortcuts), a KWin window rule for the size, and an XDG autostart entry
//! for the pad service.

use crate::desktop::{self, Desktop};
use crate::hotkey::Hotkey;
use crate::{hypr, kde, APP_ID};
use std::path::{Path, PathBuf};
use std::process::Command;

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

/// Binds the hotkey (and window rule, pad autostart) and returns a short
/// report (plus any warnings).
pub fn apply(hotkey: &Hotkey) -> Result<String, String> {
    match desktop::get() {
        Desktop::Hyprland => apply_hyprland(hotkey),
        Desktop::Kde => apply_kde(hotkey),
    }
}

/// Writes the managed file, makes sure Hyprland loads it and reloads.
fn apply_hyprland(hotkey: &Hotkey) -> Result<String, String> {
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

const SHORTCUT_FILE: &str = "hyprclick-toggle.desktop";
const RULE_GROUP: &str = "hyprclick";

fn apply_kde(hotkey: &Hotkey) -> Result<String, String> {
    let key = hotkey.qt_key()?;
    let exe = exe();
    let data = dirs::data_dir().ok_or("no data dir")?;
    let apps = data.join("applications");
    std::fs::create_dir_all(&apps).map_err(|e| e.to_string())?;
    let launcher = apps.join(SHORTCUT_FILE);
    std::fs::write(
        &launcher,
        format!(
            "[Desktop Entry]\n\
             # Managed by hyprclick: change the key in its Key tab, or run: hyprclick setup \"ALT + J\"\n\
             Type=Application\n\
             Name=hyprclick: Toggle autoclicker\n\
             Icon=input-mouse\n\
             Exec=\"{exe}\" toggle\n\
             NoDisplay=true\n\
             StartupNotify=false\n"
        ),
    )
    .map_err(|e| e.to_string())?;

    // kglobalaccel launches "_launch" of a .desktop component itself.
    let action = vec![SHORTCUT_FILE, "_launch", "hyprclick", "Toggle autoclicker"];
    const ACCEL: [&str; 3] = ["org.kde.kglobalaccel", "/kglobalaccel", "org.kde.KGlobalAccel"];
    kde::call(ACCEL[0], ACCEL[1], ACCEL[2], "doRegister", &(&action,))?;
    // 2 = SetPresent: grab the key now (else it only works after the next
    // login), 4 = NoAutoloading: take this key, not a saved older one.
    let keys = vec![(vec![key],)];
    let set: Vec<(Vec<i32>,)> = kde::call(ACCEL[0], ACCEL[1], ACCEL[2], "setShortcutKeys", &(&action, keys, 6u32))?.body().deserialize().map_err(|e| e.to_string())?;
    if !set.iter().any(|(k,)| k.first() == Some(&key)) {
        return Err(format!("{} is already a KDE shortcut, pick another (or free it in System Settings → Shortcuts)", hotkey.label()));
    }

    let mut report = format!("{} → hyprclick toggle  (KDE global shortcut \"hyprclick: Toggle autoclicker\")", hotkey.label());
    if let Err(e) = kde_window_rule() {
        report += &format!("\nwindow rule skipped: {e}");
    }
    if let Err(e) = kde_autostart(&exe) {
        report += &format!("\npad autostart skipped: {e}");
    }
    let _ = crate::afk::ensure_pad_service();
    Ok(report)
}

/// KWin rule: open hyprclick at its size, centered. KDE windows float anyway.
fn kde_window_rule() -> Result<(), String> {
    let (w, h) = WINDOW_SIZE;
    let size = format!("{w},{h}");
    let entries = [
        ("Description", "hyprclick (managed by hyprclick setup)"),
        ("wmclass", APP_ID),
        ("wmclassmatch", "1"), // exact
        ("size", size.as_str()),
        ("sizerule", "3"), // apply initially
        ("placement", "5"), // centered
        ("placementrule", "2"), // force
    ];
    for (key, value) in entries {
        kwriteconfig(RULE_GROUP, key, value)?;
    }
    // Newer KWin keeps the rule list in "Order" (and migrates the older
    // "rules" + "count" pair to it).
    let order = kreadconfig("General", "Order")?;
    let (key, current) = if order.is_empty() { ("rules", kreadconfig("General", "rules")?) } else { ("Order", order) };
    let mut list: Vec<&str> = current.split(',').map(str::trim).filter(|r| !r.is_empty()).collect();
    if !list.contains(&RULE_GROUP) {
        list.push(RULE_GROUP);
        kwriteconfig("General", key, &list.join(","))?;
        if key == "rules" {
            kwriteconfig("General", "count", &list.len().to_string())?;
        }
    }
    kde::call("org.kde.KWin", "/KWin", "org.kde.KWin", "reconfigure", &())?;
    Ok(())
}

fn kwriteconfig(group: &str, key: &str, value: &str) -> Result<(), String> {
    let out = Command::new("kwriteconfig6")
        .args(["--file", "kwinrulesrc", "--group", group, "--key", key, value])
        .output()
        .map_err(|e| format!("kwriteconfig6: {e}"))?;
    if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
}

fn kreadconfig(group: &str, key: &str) -> Result<String, String> {
    let out = Command::new("kreadconfig6")
        .args(["--file", "kwinrulesrc", "--group", group, "--key", key])
        .output()
        .map_err(|e| format!("kreadconfig6: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Start the pad service at login, before Roblox.
fn kde_autostart(exe: &str) -> Result<(), String> {
    let dir = dirs::config_dir().ok_or("no config dir")?.join("autostart");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(
        dir.join("hyprclick-pad.desktop"),
        format!(
            "[Desktop Entry]\n\
             # Managed by hyprclick: virtual gamepad for Gamepad anti-AFK\n\
             Type=Application\n\
             Name=hyprclick pad\n\
             Exec=\"{exe}\" pad\n\
             NoDisplay=true\n"
        ),
    )
    .map_err(|e| e.to_string())
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

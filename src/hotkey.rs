//! The toggle hotkey: modifiers + one key or mouse button, in Hyprland's
//! names ("ALT + H", "CTRL + SHIFT + G", "mouse:275" for the back side button).
//! On KDE the same thing is turned into a Qt key code for kglobalaccel.

use eframe::egui::{Key, Modifiers, PointerButton};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Hotkey {
    pub mods: Vec<String>,
    pub key: String,
}

impl Default for Hotkey {
    fn default() -> Self {
        Self { mods: vec!["ALT".into()], key: "H".into() }
    }
}

const MOD_ORDER: [&str; 4] = ["SUPER", "CTRL", "ALT", "SHIFT"];

impl Hotkey {
    /// Hyprland Lua form, also what we show: "ALT + H".
    pub fn combo(&self) -> String {
        self.mods.iter().map(String::as_str).chain([self.key.as_str()]).collect::<Vec<_>>().join(" + ")
    }

    /// Friendlier text for the UI ("ALT + Mouse back").
    pub fn label(&self) -> String {
        let key = match self.key.as_str() {
            "mouse:274" => "Middle click",
            "mouse:275" => "Mouse back",
            "mouse:276" => "Mouse forward",
            k => k,
        };
        self.mods.iter().map(String::as_str).chain([key]).collect::<Vec<_>>().join(" + ")
    }

    /// Mouse button bound with no modifier: Hyprland will swallow it
    /// everywhere (e.g. "back" stops working in the browser).
    pub fn bare_mouse(&self) -> bool {
        self.mods.is_empty() && self.key.starts_with("mouse:")
    }

    /// Qt key code (modifiers included), what KDE's global shortcuts store.
    /// KDE can't bind mouse buttons as global shortcuts.
    pub fn qt_key(&self) -> Result<i32, String> {
        if self.key.starts_with("mouse:") {
            return Err("KDE global shortcuts can't use mouse buttons: bind a key, then map the button to it in System Settings → Mouse → extra buttons".into());
        }
        let mods = self.mods.iter().fold(0, |m, name| {
            m | match name.as_str() {
                "SHIFT" => 0x0200_0000,
                "CTRL" => 0x0400_0000,
                "ALT" => 0x0800_0000,
                _ => 0x1000_0000, // SUPER = Meta
            }
        });
        let key = qt_keycode(&self.key).ok_or_else(|| format!("KDE doesn't know the key '{}'", self.key))?;
        Ok(mods | key)
    }

    /// Parse "ALT + SHIFT + J", "alt+j", "mouse:275", "CTRL + mouse:276".
    pub fn parse(s: &str) -> Result<Self, String> {
        let parts: Vec<String> = s.split('+').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
        let (key, mods) = parts.split_last().ok_or("empty hotkey")?;
        let mut out = Vec::new();
        for m in mods {
            let m = match m.to_uppercase().as_str() {
                "CONTROL" | "CTRL" => "CTRL",
                "ALT" => "ALT",
                "SHIFT" => "SHIFT",
                "SUPER" | "WIN" | "META" | "LOGO" | "MOD4" => "SUPER",
                other => return Err(format!("unknown modifier '{other}' (use SUPER, CTRL, ALT, SHIFT)")),
            };
            if !out.contains(&m.to_string()) {
                out.push(m.to_string());
            }
        }
        let key = if key.to_lowercase().starts_with("mouse:") { key.to_lowercase() } else { key.clone() };
        if MOD_ORDER.contains(&key.to_uppercase().as_str()) {
            return Err("needs a key after the modifiers".into());
        }
        Ok(Self { mods: sorted(out), key: if key.chars().count() == 1 { key.to_uppercase() } else { key } })
    }

    /// From a key press seen by the UI. SUPER isn't reported to apps on
    /// Linux, so SUPER combos have to be typed in instead.
    pub fn from_key(key: Key, mods: Modifiers) -> Option<Self> {
        Some(Self { mods: mods_of(mods), key: keysym(key)?.to_string() })
    }

    /// From a mouse press. Left/right only with a modifier, or every click
    /// would toggle the clicker.
    pub fn from_pointer(button: PointerButton, mods: Modifiers) -> Option<Self> {
        let code = match button {
            PointerButton::Primary => 272,
            PointerButton::Secondary => 273,
            PointerButton::Middle => 274,
            PointerButton::Extra1 => 275,
            PointerButton::Extra2 => 276,
        };
        let mods = mods_of(mods);
        if code < 274 && mods.is_empty() {
            return None;
        }
        Some(Self { mods, key: format!("mouse:{code}") })
    }
}

fn mods_of(m: Modifiers) -> Vec<String> {
    let mut v = Vec::new();
    if m.ctrl {
        v.push("CTRL".into());
    }
    if m.alt {
        v.push("ALT".into());
    }
    if m.shift {
        v.push("SHIFT".into());
    }
    sorted(v)
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort_by_key(|m| MOD_ORDER.iter().position(|o| o == m).unwrap_or(9));
    v
}

/// egui key -> xkb keysym name Hyprland understands.
fn keysym(key: Key) -> Option<&'static str> {
    use Key::*;
    Some(match key {
        Space => "space",
        Enter => "Return",
        Tab => "Tab",
        Escape => "Escape",
        Backspace => "BackSpace",
        Delete => "Delete",
        Insert => "Insert",
        Home => "Home",
        End => "End",
        PageUp => "Prior",
        PageDown => "Next",
        ArrowUp => "Up",
        ArrowDown => "Down",
        ArrowLeft => "Left",
        ArrowRight => "Right",
        Minus => "minus",
        Equals => "equal",
        Comma => "comma",
        Period => "period",
        Slash => "slash",
        Backslash => "backslash",
        Semicolon => "semicolon",
        Quote => "apostrophe",
        OpenBracket => "bracketleft",
        CloseBracket => "bracketright",
        Backtick => "grave",
        other => {
            let name = other.name();
            // Letters, digits and F1..F35 are already right.
            let ok = name.len() == 1 || (name.starts_with('F') && name[1..].parse::<u8>().is_ok()) || name.starts_with("Num");
            if !ok {
                return None;
            }
            match name {
                n if n.starts_with("Num") => &n[3..],
                n => n,
            }
        }
    })
}

/// xkb keysym name (as stored in `Hotkey::key`) -> Qt::Key.
fn qt_keycode(name: &str) -> Option<i32> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        // Letters, digits and punctuation are their (uppercase) ASCII code.
        return c.is_ascii_graphic().then(|| c.to_ascii_uppercase() as i32);
    }
    if let Some(n) = name.strip_prefix('F').and_then(|n| n.parse::<i32>().ok()).filter(|n| (1..=35).contains(n)) {
        return Some(0x0100_0030 + n - 1);
    }
    Some(match name.to_lowercase().as_str() {
        "space" => 0x20,
        "apostrophe" => 0x27,
        "comma" => 0x2c,
        "minus" => 0x2d,
        "period" => 0x2e,
        "slash" => 0x2f,
        "semicolon" => 0x3b,
        "equal" => 0x3d,
        "bracketleft" => 0x5b,
        "backslash" => 0x5c,
        "bracketright" => 0x5d,
        "grave" => 0x60,
        "escape" => 0x0100_0000,
        "tab" => 0x0100_0001,
        "backspace" => 0x0100_0003,
        "return" => 0x0100_0004,
        "insert" => 0x0100_0006,
        "delete" => 0x0100_0007,
        "pause" => 0x0100_0008,
        "print" => 0x0100_0009,
        "home" => 0x0100_0010,
        "end" => 0x0100_0011,
        "left" => 0x0100_0012,
        "up" => 0x0100_0013,
        "right" => 0x0100_0014,
        "down" => 0x0100_0015,
        "prior" | "page_up" => 0x0100_0016,
        "next" | "page_down" => 0x0100_0017,
        _ => return None,
    })
}

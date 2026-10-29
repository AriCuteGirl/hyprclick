//! The toggle hotkey: modifiers + one key or mouse button, in Hyprland's
//! names ("ALT + H", "CTRL + SHIFT + G", "mouse:275" for the back side button).

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

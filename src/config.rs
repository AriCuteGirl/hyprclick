use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    /// Virtual mouse clicks wherever the real cursor is.
    Cursor,
    /// Hyprland plugin clicks inside the target window without focusing it
    /// (Hyprland only).
    /// (Also what an old "Gamepad" setting loads as.)
    #[default]
    #[serde(other)]
    Window,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum AfkMethod {
    /// Plugin pokes only the target window.
    #[default]
    Window,
    /// Virtual Xbox pad, roblox-caffeine style (every game sees it).
    Gamepad,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum AfkAction {
    #[default]
    Wiggle,
    RightClick,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MouseButton {
    #[default]
    Left,
    Right,
    Middle,
}

impl MouseButton {
    /// Linux input event code (BTN_LEFT / BTN_RIGHT / BTN_MIDDLE).
    pub fn code(self) -> u16 {
        match self {
            MouseButton::Left => 0x110,
            MouseButton::Right => 0x111,
            MouseButton::Middle => 0x112,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MouseButton::Left => "Left",
            MouseButton::Right => "Right",
            MouseButton::Middle => "Middle",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    pub mode: Mode,
    /// Window class to target; resolved to a live window when clicking starts.
    pub target_class: String,
    /// Preferred title among windows sharing the class (empty = any).
    pub target_title: String,
    /// Click spot as a fraction of the window size, so it survives resizes.
    pub pos: [f32; 2],
    pub button: MouseButton,
    pub cps: f64,
    pub hold_enabled: bool,
    pub hold_ms: f64,
    /// Cursor mode: only click while the target window has focus.
    pub cursor_only_in_target: bool,
    /// Cursor mode: pull the real cursor onto `pos` in the target before
    /// every click (KDE's stand-in for Window mode's picked spot).
    pub cursor_lock: bool,
    /// Window mode: pause while the target's workspace isn't on any monitor.
    pub pause_when_hidden: bool,
    /// Window mode: click where the target's pointer already is (where the
    /// mouse last was inside it) instead of moving it to `pos`.
    pub at_mouse: bool,
    pub notify: bool,
    pub afk_method: AfkMethod,
    pub afk_action: AfkAction,
    pub afk_minutes: f64,
    /// Toggle key, written into Hyprland's config / KDE's global shortcuts by `hyprclick setup`.
    pub hotkey: crate::hotkey::Hotkey,
    /// lagswitch keys (Hyprland only, bound when lagswitch is installed).
    pub lag_timed_key: crate::hotkey::Hotkey,
    pub lag_toggle_key: crate::hotkey::Hotkey,
    pub lag_hold_key: crate::hotkey::Hotkey,
}

impl Default for Config {
    fn default() -> Self {
        // Without the Hyprland plugin only the uinput ways work.
        let window = crate::desktop::get().window_mode().is_ok();
        Self {
            mode: if window { Mode::Window } else { Mode::Cursor },
            target_class: "org.vinegarhq.Sober".into(),
            target_title: String::new(),
            pos: [0.5, 0.5],
            button: MouseButton::Left,
            cps: 20.0,
            hold_enabled: false,
            hold_ms: 15.0,
            cursor_only_in_target: false,
            cursor_lock: false,
            pause_when_hidden: true,
            at_mouse: false,
            notify: true,
            afk_method: if window { AfkMethod::Window } else { AfkMethod::Gamepad },
            afk_action: AfkAction::Wiggle,
            afk_minutes: 10.0,
            hotkey: crate::hotkey::Hotkey::default(),
            lag_timed_key: crate::hotkey::Hotkey { mods: vec!["ALT".into()], key: "Z".into() },
            lag_toggle_key: crate::hotkey::Hotkey { mods: vec!["ALT".into()], key: "X".into() },
            lag_hold_key: crate::hotkey::Hotkey { mods: vec!["ALT".into()], key: "C".into() },
        }
    }
}

pub const MAX_CPS: f64 = 1000.0;

impl Config {
    fn path() -> PathBuf {
        dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("hyprclick/config.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }

    pub fn interval(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.cps.clamp(0.1, MAX_CPS))
    }

    /// Zero when hold is off: press and release go out back to back.
    pub fn hold(&self) -> Duration {
        if self.hold_enabled {
            Duration::from_secs_f64(self.hold_ms.max(0.0) / 1000.0).min(self.interval().mul_f64(0.9))
        } else {
            Duration::ZERO
        }
    }
}

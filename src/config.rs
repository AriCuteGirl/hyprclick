use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    /// Hyprland plugin clicks inside the target window without focusing it.
    #[default]
    Window,
    /// Virtual mouse clicks wherever the real cursor is.
    Cursor,
    /// Virtual Xbox pad; games read it even when unfocused (roblox-caffeine trick).
    Gamepad,
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

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PadButton {
    #[default]
    RT,
    LT,
    A,
    B,
    X,
    Y,
    RB,
    LB,
}

impl PadButton {
    pub const ALL: [PadButton; 8] = [
        PadButton::RT,
        PadButton::LT,
        PadButton::A,
        PadButton::B,
        PadButton::X,
        PadButton::Y,
        PadButton::RB,
        PadButton::LB,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PadButton::RT => "RT",
            PadButton::LT => "LT",
            PadButton::A => "A",
            PadButton::B => "B",
            PadButton::X => "X",
            PadButton::Y => "Y",
            PadButton::RB => "RB",
            PadButton::LB => "LB",
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
    pub pad_button: PadButton,
    pub cps: f64,
    pub hold_enabled: bool,
    pub hold_ms: f64,
    /// Cursor mode: only click while the target window has focus.
    pub cursor_only_in_target: bool,
    /// Window mode: pause while the target's workspace isn't on any monitor.
    pub pause_when_hidden: bool,
    pub notify: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: Mode::Window,
            target_class: "org.vinegarhq.Sober".into(),
            target_title: String::new(),
            pos: [0.5, 0.5],
            button: MouseButton::Left,
            pad_button: PadButton::RT,
            cps: 20.0,
            hold_enabled: false,
            hold_ms: 15.0,
            cursor_only_in_target: false,
            pause_when_hidden: true,
            notify: true,
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

//! Which compositor hyprclick runs under, and the window queries the rest of
//! the app needs from it. Hyprland goes through its IPC socket (hypr.rs),
//! KDE Plasma through KWin scripts over D-Bus (kde.rs).

use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Desktop {
    Hyprland,
    Kde,
}

static CURRENT: OnceLock<Option<Desktop>> = OnceLock::new();

/// The running compositor, if it's one hyprclick supports.
pub fn detect() -> Option<Desktop> {
    *CURRENT.get_or_init(|| {
        if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
            return Some(Desktop::Hyprland);
        }
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        if desktop.split(':').any(|d| d.eq_ignore_ascii_case("KDE")) || std::env::var_os("KDE_FULL_SESSION").is_some() {
            return Some(Desktop::Kde);
        }
        None
    })
}

/// main() exits early when detect() is None, so this always has an answer.
pub fn get() -> Desktop {
    detect().unwrap_or(Desktop::Hyprland)
}

impl Desktop {
    pub fn name(self) -> &'static str {
        match self {
            Desktop::Hyprland => "Hyprland",
            Desktop::Kde => "KDE Plasma",
        }
    }

    /// Window mode and Window anti-AFK click a window without focusing it,
    /// which only the Hyprland plugin can do.
    pub fn window_mode(self) -> Result<(), String> {
        match self {
            Desktop::Hyprland => Ok(()),
            Desktop::Kde => Err("Window mode needs Hyprland: KWin can't send clicks to a window in the background. Use Cursor mode.".into()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Client {
    /// Hyprland address ("0x55d0…") or KWin internal id ("{uuid}").
    pub address: String,
    pub class: String,
    pub title: String,
    pub at: [f64; 2],
    pub size: [f64; 2],
    /// None where the compositor doesn't say (KWin scripts).
    pub xwayland: Option<bool>,
    /// Workspace / virtual desktop name, for display.
    pub workspace: String,
    /// On a workspace that's shown and not minimized.
    pub visible: bool,
    /// Lower = more likely on top (focus history or stacking order).
    pub recency: i64,
}

impl Client {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.at[0] && y >= self.at[1] && x < self.at[0] + self.size[0] && y < self.at[1] + self.size[1]
    }
}

pub fn clients() -> Vec<Client> {
    match get() {
        Desktop::Hyprland => crate::hypr::clients(),
        Desktop::Kde => crate::kde::clients(),
    }
}

pub fn cursor_pos() -> Option<(f64, f64)> {
    match get() {
        Desktop::Hyprland => crate::hypr::cursor_pos(),
        Desktop::Kde => crate::kde::cursor_pos(),
    }
}

pub fn active_address() -> Option<String> {
    match get() {
        Desktop::Hyprland => crate::hypr::active_address(),
        Desktop::Kde => crate::kde::active_address(),
    }
}

/// The visible window under a global point (topmost guess).
pub fn window_at(x: f64, y: f64) -> Option<Client> {
    clients()
        .into_iter()
        .filter(|c| c.class != crate::APP_ID && c.visible && c.contains(x, y))
        .min_by_key(|c| c.recency)
}

pub fn find_target(class: &str, title: &str) -> Option<Client> {
    let mut matches: Vec<Client> = clients().into_iter().filter(|c| c.class == class).collect();
    matches.sort_by_key(|c| (title.is_empty() || c.title != title, c.recency));
    matches.into_iter().next()
}

pub fn notify(text: &str) {
    match get() {
        Desktop::Hyprland => crate::hypr::notify(text),
        Desktop::Kde => crate::kde::notify(text),
    }
}

/// Bring the already open hyprclick window to the front.
pub fn focus_app() {
    match get() {
        Desktop::Hyprland => {
            let _ = crate::hypr::request(&format!("dispatch hl.dsp.focus({{window=\"class:^({})$\"}})", crate::APP_ID));
        }
        Desktop::Kde => crate::kde::activate_class(crate::APP_ID),
    }
}

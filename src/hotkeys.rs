//! Global hotkeys on Windows. There's no compositor to bind keys in, so
//! hyprclick registers them itself with RegisterHotKey. They work while
//! hyprclick runs (minimized is fine): the clicker key and the Lag keys.

use crate::config::Config;
use crate::hotkey::Hotkey;
use crate::win;
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const RELOAD: u32 = win::WM_USER + 1;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Action {
    Clicker = 1,
    LagTimed,
    LagToggle,
    LagHold,
    LagFake,
}

const ACTIONS: [Action; 5] = [Action::Clicker, Action::LagTimed, Action::LagToggle, Action::LagHold, Action::LagFake];

fn key_for(cfg: &Config, action: Action) -> &Hotkey {
    match action {
        Action::Clicker => &cfg.hotkey,
        Action::LagTimed => &cfg.lag_timed_key,
        Action::LagToggle => &cfg.lag_toggle_key,
        Action::LagHold => &cfg.lag_hold_key,
        Action::LagFake => &cfg.lag_fake_key,
    }
}

/// RegisterHotKey's modifiers and virtual key for a hotkey.
pub fn vk(hk: &Hotkey) -> Result<(u32, u32), String> {
    let mods = hk.mods.iter().fold(win::MOD_NOREPEAT, |m, name| {
        m | match name.as_str() {
            "ALT" => win::MOD_ALT,
            "CTRL" => win::MOD_CONTROL,
            "SHIFT" => win::MOD_SHIFT,
            _ => win::MOD_WIN,
        }
    });
    let key = hk.key.as_str();
    let code = match key {
        k if k.starts_with("mouse:") => return Err(format!("{}: Windows hotkeys can't use mouse buttons, pick a key", hk.label())),
        k if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphanumeric()) => k.to_ascii_uppercase().as_bytes()[0] as u32,
        k if k.len() > 1 && k.starts_with('F') && k[1..].parse::<u32>().is_ok_and(|n| (1..=24).contains(&n)) => {
            0x6F + k[1..].parse::<u32>().unwrap_or(1)
        }
        "space" => 0x20,
        "Return" => 0x0D,
        "Tab" => 0x09,
        "Escape" => 0x1B,
        "BackSpace" => 0x08,
        "Delete" => 0x2E,
        "Insert" => 0x2D,
        "Home" => 0x24,
        "End" => 0x23,
        "Prior" => 0x21,
        "Next" => 0x22,
        "Left" => 0x25,
        "Up" => 0x26,
        "Right" => 0x27,
        "Down" => 0x28,
        "minus" => 0xBD,
        "equal" => 0xBB,
        "comma" => 0xBC,
        "period" => 0xBE,
        "slash" => 0xBF,
        "backslash" => 0xDC,
        "semicolon" => 0xBA,
        "apostrophe" => 0xDE,
        "bracketleft" => 0xDB,
        "bracketright" => 0xDD,
        "grave" => 0xC0,
        other => return Err(format!("Windows doesn't know the key '{other}'")),
    };
    Ok((mods, code))
}

/// The hotkey thread's id, and where its reload results come back.
type Thread = (u32, Mutex<mpsc::Receiver<Result<String, String>>>);

fn thread() -> Option<&'static Thread> {
    static THREAD: OnceLock<Option<Thread>> = OnceLock::new();
    THREAD
        .get_or_init(|| {
            let (ready_tx, ready_rx) = mpsc::channel();
            let (reply_tx, reply_rx) = mpsc::channel();
            std::thread::spawn(move || run(ready_tx, reply_tx));
            ready_rx.recv().ok().map(|id| (id, Mutex::new(reply_rx)))
        })
        .as_ref()
}

/// Register (or re-register) the keys from the saved config, in this process.
pub fn reload() -> Result<String, String> {
    let (id, replies) = thread().ok_or("can't start the hotkey thread")?;
    let replies = replies.lock().unwrap();
    // SAFETY: posting a plain message to the hotkey thread, which has a queue.
    if unsafe { win::PostThreadMessageW(*id, RELOAD, 0, 0) } == 0 {
        return Err("the hotkey thread isn't running".to_string());
    }
    replies.recv_timeout(Duration::from_secs(3)).map_err(|_| "the hotkey thread didn't answer".to_string())?
}

fn register(cfg: &Config) -> Result<String, String> {
    let mut failed = Vec::new();
    for action in ACTIONS {
        // SAFETY: unregistering an id of this thread; fine if it wasn't registered.
        unsafe { win::UnregisterHotKey(0, action as i32) };
        let key = key_for(cfg, action);
        if action != Action::Clicker && !crate::lag::installed() {
            continue;
        }
        match vk(key) {
            // SAFETY: registering for this thread (hwnd 0), which runs the message loop.
            Ok((mods, code)) if unsafe { win::RegisterHotKey(0, action as i32, mods, code) } != 0 => {}
            Ok(_) => failed.push(format!("{} is already taken by another program", key.label())),
            Err(e) => failed.push(e),
        }
    }
    if failed.is_empty() {
        let mut report = format!("{} → hyprclick toggle (while hyprclick runs)", cfg.hotkey.label());
        if crate::lag::installed() {
            report += &format!(
                "\nLag: {} timed, {} toggle, {} hold, {} fake lag",
                cfg.lag_timed_key.label(),
                cfg.lag_toggle_key.label(),
                cfg.lag_hold_key.label(),
                cfg.lag_fake_key.label()
            );
        }
        Ok(report)
    } else {
        Err(failed.join("\n"))
    }
}

fn run(ready: mpsc::Sender<u32>, replies: mpsc::Sender<Result<String, String>>) {
    let mut msg = win::MSG::default();
    // A message queue exists once the thread asks for messages.
    // SAFETY: msg is valid for writing; PM_NOREMOVE leaves the queue alone.
    unsafe { win::PeekMessageW(&mut msg, 0, 0, 0, 0) };
    // SAFETY: no preconditions.
    let _ = ready.send(unsafe { win::GetCurrentThreadId() });
    let mut cfg = Config::load();
    // SAFETY: msg is valid for writing; 0 for hwnd takes every message of this thread.
    while unsafe { win::GetMessageW(&mut msg, 0, 0, 0) } > 0 {
        match msg.message {
            RELOAD => {
                cfg = Config::load();
                let _ = replies.send(register(&cfg));
            }
            win::WM_HOTKEY => {
                let Some(action) = ACTIONS.into_iter().find(|a| *a as usize == msg.wParam) else { continue };
                let held = vk(key_for(&cfg, action)).ok().map(|(_, code)| code);
                // Off this thread, so the next key press is never kept waiting.
                std::thread::spawn(move || fire(action, held));
            }
            _ => {}
        }
    }
}

fn fire(action: Action, held: Option<u32>) {
    match action {
        Action::Clicker => {
            if crate::ipc::send("toggle").is_none() {
                crate::win::notify("hyprclick isn't running");
            }
        }
        Action::LagTimed => crate::lag::key(&["freeze"]),
        Action::LagToggle => crate::lag::key(&["toggle"]),
        Action::LagFake => crate::lag::key(&["lag", "toggle"]),
        Action::LagHold => {
            crate::lag::key(&["hold", "start"]);
            // RegisterHotKey only reports the press, so watch for the release.
            if let Some(code) = held {
                std::thread::sleep(Duration::from_millis(50));
                while win::key_down(code) {
                    std::thread::sleep(Duration::from_millis(15));
                }
            }
            crate::lag::key(&["hold", "stop"]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hk(mods: &[&str], key: &str) -> Hotkey {
        Hotkey { mods: mods.iter().map(|m| m.to_string()).collect(), key: key.to_string() }
    }

    #[test]
    fn maps_keys_to_windows_codes() {
        assert_eq!(vk(&hk(&["ALT"], "H")), Ok((win::MOD_NOREPEAT | win::MOD_ALT, b'H' as u32)));
        assert_eq!(vk(&hk(&["CTRL", "SHIFT"], "F5")).map(|v| v.1), Ok(0x74));
        assert_eq!(vk(&hk(&[], "space")).map(|v| v.1), Ok(0x20));
        assert!(vk(&hk(&["ALT"], "mouse:275")).is_err());
    }
}

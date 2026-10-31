//! The Lag tab's link to lagswitch, a separate root daemon plus a CLI. Only the
//! CLI is used here, so the GUI never needs root and works unchanged when
//! lagswitch isn't installed.

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const CLI: &str = "/usr/local/bin/lagswitch";

pub fn installed() -> bool {
    std::path::Path::new(CLI).exists()
}

fn run(args: &[&str]) -> Result<String, String> {
    let out = Command::new(CLI).args(args).output().map_err(|e| format!("lagswitch: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.trim().trim_start_matches("lagswitch: ").to_string())
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Status {
    Idle,
    Frozen { mode: String, direction: String, left_ms: u64 },
    /// Daemon not reachable, with the reason.
    Down(String),
}

fn parse_status(text: &str) -> Status {
    let mut parts = text.split(' ');
    if parts.next() != Some("frozen") {
        return Status::Idle;
    }
    Status::Frozen {
        mode: parts.next().unwrap_or("").to_string(),
        direction: parts.next().unwrap_or("").to_string(),
        left_ms: parts.next().and_then(|ms| ms.parse().ok()).unwrap_or(0),
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Entry {
    pub name: String,
    pub is_flatpak: bool,
    pub processes: u32,
    pub freezable: bool,
}

fn parse_list(text: &str) -> Vec<Entry> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let [name, _pid, app_id, processes, status, _cgroup] = fields[..] else { return None };
            Some(Entry {
                name: name.to_string(),
                is_flatpak: app_id != "-",
                processes: processes.parse().unwrap_or(1),
                freezable: status == "ok",
            })
        })
        .collect()
}

#[derive(Clone, PartialEq, Debug)]
pub struct Settings {
    pub seconds: f32,
    pub direction: Direction,
    pub cap_seconds: f32,
    pub app: String,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Direction {
    In,
    Out,
    Both,
}

impl Direction {
    fn name(self) -> &'static str {
        match self {
            Direction::In => "in",
            Direction::Out => "out",
            Direction::Both => "both",
        }
    }
}

fn parse_settings(text: &str) -> Settings {
    let mut s = Settings { seconds: 1.5, direction: Direction::Both, cap_seconds: 30.0, app: String::new() };
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim().trim_matches('"');
        match key.trim() {
            "default_ms" => s.seconds = value.parse::<f32>().unwrap_or(1500.0) / 1000.0,
            "max_cap_ms" => s.cap_seconds = value.parse::<f32>().unwrap_or(30_000.0) / 1000.0,
            "direction" => {
                s.direction = match value {
                    "in" => Direction::In,
                    "out" => Direction::Out,
                    _ => Direction::Both,
                }
            }
            "last_app" => s.app = value.to_string(),
            _ => {}
        }
    }
    s
}

fn to_ms(seconds: f32) -> String {
    ((seconds * 1000.0).round() as u64).to_string()
}

pub fn load_settings() -> Settings {
    run(&["config"]).map(|text| parse_settings(&text)).unwrap_or_else(|_| parse_settings(""))
}

/// Saved where the keybinds read it, so changing these needs no rebind.
pub fn save_settings(s: &Settings) -> Result<(), String> {
    let (ms, cap) = (to_ms(s.seconds), to_ms(s.cap_seconds));
    let mut args = vec!["set", "--ms", &ms, "--dir", s.direction.name(), "--cap-ms", &cap];
    if !s.app.is_empty() {
        args.extend(["--app", &s.app]);
    }
    run(&args).map(|_| ())
}

/// Status and app list, refreshed by a thread while the tab is on screen.
#[derive(Clone)]
pub struct Live {
    pub status: Status,
    pub entries: Vec<Entry>,
    pub message: Option<String>,
}

pub struct Monitor {
    live: Arc<Mutex<Live>>,
    visible: Arc<AtomicBool>,
    started: bool,
}

impl Monitor {
    pub fn new() -> Self {
        let live = Live { status: Status::Idle, entries: Vec::new(), message: None };
        Self { live: Arc::new(Mutex::new(live)), visible: Arc::new(AtomicBool::new(false)), started: false }
    }

    pub fn live(&self) -> Live {
        self.live.lock().unwrap().clone()
    }

    /// Call each frame the tab shows; the thread idles once frames stop saying so.
    pub fn show(&mut self) {
        self.visible.store(true, Ordering::Relaxed);
        if self.started {
            return;
        }
        self.started = true;
        let (live, visible) = (self.live.clone(), self.visible.clone());
        std::thread::spawn(move || {
            let mut round = 0u32;
            loop {
                if !visible.swap(false, Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
                refresh_status(&live);
                if round % 8 == 0
                    && let Ok(text) = run(&["list"])
                {
                    live.lock().unwrap().entries = parse_list(&text);
                }
                round = round.wrapping_add(1);
                std::thread::sleep(Duration::from_millis(250));
            }
        });
    }

    /// Run a CLI command off the UI thread and show any error.
    pub fn send(&self, args: Vec<String>) {
        let live = self.live.clone();
        std::thread::spawn(move || {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let result = run(&args);
            live.lock().unwrap().message = result.err();
            refresh_status(&live);
        });
    }
}

fn refresh_status(live: &Mutex<Live>) {
    let status = match run(&["status"]) {
        Ok(text) => parse_status(&text),
        Err(e) => Status::Down(e),
    };
    live.lock().unwrap().status = status;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_status() {
        assert_eq!(parse_status("idle"), Status::Idle);
        assert_eq!(
            parse_status("frozen toggle both 12345 a/b.scope"),
            Status::Frozen { mode: "toggle".into(), direction: "both".into(), left_ms: 12345 }
        );
    }

    #[test]
    fn reads_the_list() {
        let text = "org.foo\t1\torg.foo\t3\tok\ta/x.scope\nzsh\t2\t-\t58\tshared\ta/s.scope\nbad line\n";
        let entries = parse_list(text);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].is_flatpak && entries[0].freezable);
        assert_eq!(entries[1].processes, 58);
        assert!(!entries[1].is_flatpak && !entries[1].freezable);
    }

    #[test]
    fn reads_settings() {
        let s = parse_settings("default_ms = 5000\ndirection = \"out\"\nmax_cap_ms = 10000\nlast_app = \"org.foo\"\n");
        assert_eq!(s, Settings { seconds: 5.0, direction: Direction::Out, cap_seconds: 10.0, app: "org.foo".into() });
    }

    #[test]
    fn writes_milliseconds_without_float_noise() {
        assert_eq!(to_ms(1.1), "1100");
        assert_eq!(to_ms(0.3), "300");
    }
}

//! Anti-AFK: every few minutes, poke the target window so Roblox doesn't kick
//! you for being idle. Two ways:
//! - Window: the hyprclick plugin wiggles the target's own pointer (or taps
//!   right-click) in the background, like a click, only for that window.
//!   Hyprland only.
//! - Gamepad: the roblox-caffeine way, a virtual Xbox pad nudges both sticks.
//!   Games read pads even when unfocused, but every game sees it. Roblox
//!   only reacts while you're inside a game. The pad lives in a small
//!   always-running service (`hyprclick pad`, started at login by
//!   `hyprclick setup`) so it isn't re-plugged every time anti-AFK starts.
//! Either way it's locked to the target window and stops if that window goes
//! away (closed or crashed).

use crate::config::{AfkAction, AfkMethod, Config};
use crate::desktop;
use crate::hypr;
use chrono::{DateTime, Local};
use evdev::uinput::VirtualDevice;
use evdev::{AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode, UinputAbsSetup};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Same deflection and hold as roblox-caffeine.
const STICK_DEFLECTION: i32 = 22000;
const NUDGE_HOLD: Duration = Duration::from_millis(300);
const HISTORY: usize = 50;
/// Window method: how far the pointer wiggles (px).
const WIGGLE: f64 = 8.0;
const WATCH_EVERY: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct Nudge {
    pub number: u64,
    pub at: DateTime<Local>,
    pub dir: &'static str,
}

#[derive(Default)]
pub struct Status {
    pub running: bool,
    /// What it's poking, e.g. "org.vinegarhq.Sober · window".
    pub desc: String,
    pub interval: Duration,
    pub started: Option<Instant>,
    pub next_at: Option<Instant>,
    /// Newest last.
    pub nudges: VecDeque<Nudge>,
    pub error: Option<String>,
}

impl Status {
    /// "~7m" / "<1m", like caffeine's progress line.
    pub fn next_in(&self) -> String {
        let left = self.next_at.map(|n| n.saturating_duration_since(Instant::now()).as_secs()).unwrap_or(0);
        let mins = left.div_ceil(60);
        if mins > 1 { format!("~{mins}m") } else { "<1m".into() }
    }

    /// 0..1 through the current wait.
    pub fn progress(&self) -> f32 {
        let (Some(next), false) = (self.next_at, self.interval.is_zero()) else { return 0.0 };
        let left = next.saturating_duration_since(Instant::now()).as_secs_f32();
        (1.0 - left / self.interval.as_secs_f32()).clamp(0.0, 1.0)
    }

    pub fn uptime(&self) -> String {
        format_duration(self.started.map(|s| s.elapsed()).unwrap_or_default())
    }
}

pub fn format_duration(d: Duration) -> String {
    let s = d.as_secs();
    match (s / 3600, (s % 3600) / 60, s % 60) {
        (0, 0, sec) => format!("{sec}s"),
        (0, m, sec) => format!("{m}m {sec}s"),
        (h, m, _) => format!("{h}h {m}m"),
    }
}

/// Bar like caffeine's: `██████░░░░`.
pub fn bar(progress: f32, len: usize) -> String {
    let filled = ((progress * len as f32) as usize).min(len);
    "█".repeat(filled) + &"░".repeat(len - filled)
}

pub struct AntiAfk {
    status: Arc<Mutex<Status>>,
    stop: Option<Arc<AtomicBool>>,
    on_nudge: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl AntiAfk {
    pub fn new() -> Self {
        Self { status: Arc::new(Mutex::new(Status::default())), stop: None, on_nudge: None }
    }

    /// Called from the worker thread after every nudge (UI repaint, printing).
    pub fn on_nudge(&mut self, f: impl Fn() + Send + Sync + 'static) {
        self.on_nudge = Some(Arc::new(f));
    }

    pub fn status(&self) -> std::sync::MutexGuard<'_, Status> {
        self.status.lock().unwrap()
    }

    pub fn running(&self) -> bool {
        self.status().running
    }

    /// Starts nudging the config's target window. Fails if it isn't open.
    pub fn start(&mut self, cfg: &Config) -> Result<(), String> {
        self.stop();
        let interval = Duration::from_secs_f64(cfg.afk_minutes.max(0.1) * 60.0);
        let target = desktop::find_target(&cfg.target_class, &cfg.target_title)
            .ok_or_else(|| format!("window '{}' isn't open", cfg.target_class))?;
        let mut driver = match cfg.afk_method {
            AfkMethod::Window => {
                desktop::get().window_mode().map_err(|_| "Window anti-AFK needs Hyprland, use Gamepad".to_string())?;
                hypr::plugin::ensure()?;
                Driver::Window { address: target.address.clone(), action: cfg.afk_action }
            }
            AfkMethod::Gamepad => {
                ensure_pad_service()?;
                Driver::Pad
            }
        };
        let method = match cfg.afk_method {
            AfkMethod::Window => "window",
            AfkMethod::Gamepad => "gamepad",
        };
        let stop = Arc::new(AtomicBool::new(false));
        {
            let mut s = self.status();
            *s = Status {
                running: true,
                desc: format!("{} · {method}", target.class),
                interval,
                started: Some(Instant::now()),
                ..Default::default()
            };
        }
        let (status, stop2, cb) = (self.status.clone(), stop.clone(), self.on_nudge.clone());
        let address = target.address;
        std::thread::spawn(move || {
            let mut number = 0;
            let mut rng = Instant::now().elapsed().as_nanos() as u64 ^ std::process::id() as u64 ^ 0x9e3779b97f4a7c15;
            while !stop2.load(Ordering::Relaxed) {
                number += 1;
                let (sx, sy) = (random_sign(&mut rng), random_sign(&mut rng));
                let result = driver.nudge(sx, sy);
                {
                    let mut s = status.lock().unwrap();
                    match result {
                        Ok(dir) => {
                            s.nudges.push_back(Nudge { number, at: Local::now(), dir });
                            if s.nudges.len() > HISTORY {
                                s.nudges.pop_front();
                            }
                            s.error = None;
                        }
                        Err(e) => s.error = Some(e),
                    }
                    s.next_at = Some(Instant::now() + interval);
                }
                if let Some(cb) = &cb {
                    cb();
                }
                // Wait in short steps so stop() returns quickly, and check
                // that the target is still there (fail-safe).
                let wake = Instant::now() + interval;
                let mut checked = Instant::now();
                while Instant::now() < wake && !stop2.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(200));
                    if checked.elapsed() >= WATCH_EVERY {
                        checked = Instant::now();
                        if !desktop::clients().iter().any(|c| c.address == address) {
                            stop2.store(true, Ordering::Relaxed);
                            let mut s = status.lock().unwrap();
                            s.running = false;
                            s.next_at = None;
                            s.error = Some("target window closed, anti-AFK stopped".into());
                            drop(s);
                            desktop::notify("hyprclick: target window closed, anti-AFK stopped");
                            if let Some(cb) = &cb {
                                cb();
                            }
                        }
                    }
                }
            }
        });
        self.stop = Some(stop);
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop.store(true, Ordering::Relaxed);
            let mut s = self.status();
            s.running = false;
            s.next_at = None;
        }
    }
}

impl Drop for AntiAfk {
    fn drop(&mut self) {
        self.stop();
    }
}

fn random_sign(state: &mut u64) -> i32 {
    // xorshift64: plenty for picking a direction.
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    if *state & 1 == 0 { 1 } else { -1 }
}

enum Driver {
    Window { address: String, action: AfkAction },
    /// Nudges go to the `hyprclick pad` service.
    Pad,
}

impl Driver {
    /// One anti-AFK poke in diagonal (sx, sy); returns what to show.
    fn nudge(&mut self, sx: i32, sy: i32) -> Result<&'static str, String> {
        match self {
            Driver::Pad => {
                ensure_pad_service()?;
                let reply = pad_request(&format!("nudge {} {}", sx * STICK_DEFLECTION, sy * STICK_DEFLECTION)).ok_or("pad service not answering")?;
                if reply != "ok" {
                    return Err(reply);
                }
                Ok(direction(sx, sy))
            }
            Driver::Window { address, action } => {
                // The plugin may have been unloaded (Hyprland update etc.).
                hypr::plugin::ensure()?;
                let size = hypr::clients().into_iter().find(|c| c.address == *address).map(|c| c.size).ok_or("target window closed")?;
                let (x, y) = ((size[0] / 2.0).round(), (size[1] / 2.0).round());
                let button = match action {
                    AfkAction::Wiggle => 0,
                    AfkAction::RightClick => 0x111,
                };
                let reply = hypr::request(&format!(
                    "hyprclick afk address:{address} {x} {y} {} {} {button}",
                    sx as f64 * WIGGLE,
                    sy as f64 * WIGGLE
                ))?;
                let reply = reply.trim();
                if reply.starts_with("skipped") {
                    return Ok("✓ you were on it");
                }
                if reply != "ok" {
                    return Err(reply.to_string());
                }
                Ok(match action {
                    AfkAction::Wiggle => direction(sx, sy),
                    AfkAction::RightClick => "◉ RIGHT-CLICK",
                })
            }
        }
    }
}

fn direction(x: i32, y: i32) -> &'static str {
    match (x > 0, y < 0) {
        (true, true) => "↗ UP-RIGHT",
        (false, true) => "↖ UP-LEFT",
        (true, false) => "↘ DOWN-RIGHT",
        (false, false) => "↙ DOWN-LEFT",
    }
}

/// Move with the left stick and turn the camera with the right one, then
/// re-center everything (same as caffeine).
fn nudge_pad(pad: &mut VirtualDevice, x: i32, y: i32) -> Result<(), String> {
    let abs = |axis: AbsoluteAxisCode, v: i32| InputEvent::new(EventType::ABSOLUTE.0, axis.0, v);
    pad.emit(&[abs(AbsoluteAxisCode::ABS_X, x), abs(AbsoluteAxisCode::ABS_Y, y), abs(AbsoluteAxisCode::ABS_RX, -x)])
        .map_err(|e| e.to_string())?;
    std::thread::sleep(NUDGE_HOLD);
    pad.emit(&[
        abs(AbsoluteAxisCode::ABS_X, 0),
        abs(AbsoluteAxisCode::ABS_Y, 0),
        abs(AbsoluteAxisCode::ABS_RX, 0),
        abs(AbsoluteAxisCode::ABS_RY, 0),
    ])
    .map_err(|e| e.to_string())
}

fn uinput_err(e: std::io::Error) -> String {
    format!("/dev/uinput: {e} (are you in the 'input' group?)")
}

fn make_pad() -> Result<VirtualDevice, String> {
    let keys = AttributeSet::from_iter([
        KeyCode::BTN_SOUTH,
        KeyCode::BTN_EAST,
        KeyCode::BTN_NORTH,
        KeyCode::BTN_WEST,
        KeyCode::BTN_TL,
        KeyCode::BTN_TR,
        KeyCode::BTN_SELECT,
        KeyCode::BTN_START,
        KeyCode::BTN_THUMBL,
        KeyCode::BTN_THUMBR,
    ]);
    let stick = AbsInfo::new(0, -32768, 32767, 16, 128, 0);
    let mut b = VirtualDevice::builder()
        .map_err(uinput_err)?
        .name("Microsoft X-Box 360 pad")
        // Xbox 360 ids so SDL (Sober) picks its standard mapping.
        .input_id(InputId::new(BusType::BUS_USB, 0x045e, 0x028e, 0x0110))
        .with_keys(&keys)
        .map_err(uinput_err)?;
    for axis in [AbsoluteAxisCode::ABS_X, AbsoluteAxisCode::ABS_Y, AbsoluteAxisCode::ABS_RX, AbsoluteAxisCode::ABS_RY] {
        b = b.with_absolute_axis(&UinputAbsSetup::new(axis, stick)).map_err(uinput_err)?;
    }
    b.build().map_err(uinput_err)
}

fn pad_socket() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(runtime).join("hyprclick-pad.sock")
}

fn pad_request(cmd: &str) -> Option<String> {
    let mut sock = UnixStream::connect(pad_socket()).ok()?;
    let _ = sock.set_read_timeout(Some(Duration::from_secs(3)));
    sock.write_all(format!("{cmd}\n").as_bytes()).ok()?;
    let mut reply = String::new();
    BufReader::new(sock).read_line(&mut reply).ok()?;
    Some(reply.trim().to_string())
}

/// The pad's /dev/input node, if the service is running.
fn pad_node() -> Option<String> {
    pad_request("ping")?.strip_prefix("pad ok ").map(String::from)
}

/// Start `hyprclick pad` in the background if it isn't running.
pub fn ensure_pad_service() -> Result<(), String> {
    if pad_node().is_some() {
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(exe)
        .arg("pad")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(100));
        if pad_node().is_some() {
            return Ok(());
        }
    }
    Err("virtual pad service didn't start (/dev/uinput access?)".into())
}

/// `hyprclick pad`: own the virtual Xbox pad for as long as the session
/// runs (started at login), and nudge it on request.
pub fn run_pad_service() {
    if pad_node().is_some() {
        println!("hyprclick pad: already running");
        return;
    }
    let path = pad_socket();
    let _ = std::fs::remove_file(&path);
    let mut pad = match make_pad() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("hyprclick pad: {e}");
            std::process::exit(1);
        }
    };
    let node = pad
        .enumerate_dev_nodes_blocking()
        .ok()
        .and_then(|mut nodes| nodes.next())
        .and_then(Result::ok)
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("hyprclick pad: {e}");
            std::process::exit(1);
        }
    };
    for stream in listener.incoming().flatten() {
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let reply = match parts.next() {
            Some("ping") => format!("pad ok {node}"),
            Some("nudge") => {
                let x = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                let y = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                match nudge_pad(&mut pad, x, y) {
                    Ok(()) => "ok".into(),
                    Err(e) => format!("error: {e}"),
                }
            }
            Some("quit") => {
                let _ = (&stream).write_all(b"bye\n");
                break;
            }
            _ => "error: unknown command".into(),
        };
        let _ = (&stream).write_all(format!("{reply}\n").as_bytes());
    }
    let _ = std::fs::remove_file(&path);
}

/// `hyprclick afk`: run in the terminal with caffeine-style output.
pub fn run_terminal(cfg: &Config) {
    const YELLOW: &str = "\x1b[93m";
    const BLUE: &str = "\x1b[94m";
    const CYAN: &str = "\x1b[96m";
    const GREEN: &str = "\x1b[92m";
    const PINK: &str = "\x1b[95m";
    const RESET: &str = "\x1b[0m";
    const CLEAR: &str = "\r\x1b[K";
    const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    const WIDTH: usize = 40;

    let mut afk = AntiAfk::new();
    if let Err(e) = afk.start(cfg) {
        eprintln!("hyprclick afk: {e}");
        std::process::exit(1);
    }
    println!(
        "{PINK}♥ hyprclick anti-AFK{RESET}  {} · every {} · Ctrl+C to stop\n",
        afk.status().desc,
        format_duration(Duration::from_secs_f64(cfg.afk_minutes.max(0.1) * 60.0))
    );

    let mut shown = 0;
    let mut tick = 0usize;
    loop {
        let s = afk.status();
        if !s.running {
            println!("{CLEAR}{}", s.error.as_deref().unwrap_or("stopped"));
            return;
        }
        if let Some(e) = &s.error {
            println!("{CLEAR}error: {e}");
        }
        let new: Vec<Nudge> = s.nudges.iter().filter(|n| n.number > shown).cloned().collect();
        for n in new {
            let next = n.at + chrono::Duration::from_std(s.interval).unwrap_or_default();
            let line = "─".repeat(WIDTH);
            print!("{CLEAR}");
            println!(" ┌{line}┐");
            println!("   {YELLOW}☕{RESET} : Caffeine Active #{:03} [{}]", n.number, n.at.format("%I:%M %p"));
            println!("   {BLUE}\u{f11b}{RESET} : Motion: {}", n.dir);
            println!("   {CYAN}\u{f017}{RESET} : Next: {} (in {})", next.format("%I:%M %p"), format_duration(s.interval));
            println!(" └{line}┘");
            shown = n.number;
        }
        if s.next_at.is_some() {
            print!(
                "{CLEAR} {CYAN}{}{RESET} Next in {} [{GREEN}{}{RESET}] • Uptime: {} ",
                SPINNER[tick % SPINNER.len()],
                s.next_in(),
                bar(s.progress(), 10),
                s.uptime()
            );
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
        drop(s);
        tick += 1;
        std::thread::sleep(Duration::from_millis(100));
    }
}

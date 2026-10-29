//! The three click engines behind one start/stop interface.

use crate::config::{Config, Mode};
use crate::desktop::{self, Desktop};
use crate::hypr;
use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode, RelativeAxisCode};
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

type Device = Arc<Mutex<VirtualDevice>>;

const KDE_FOCUS_POLL: Duration = Duration::from_millis(250);
/// KWin drops events from a new uinput mouse for a few hundred ms (300 ms
/// was too short, 1.5 s enough).
const KDE_DEVICE_SETTLE: Duration = Duration::from_millis(1000);
/// Cursor lock: re-read the target's geometry this often.
const LOCK_REFRESH: Duration = Duration::from_secs(1);
/// Cursor lock: at most this many correction moves per click, each given
/// this long to reach the compositor.
const LOCK_STEPS: usize = 8;
const LOCK_SETTLE: Duration = Duration::from_millis(3);

enum Running {
    Plugin,
    Thread { stop: Arc<AtomicBool>, handle: JoinHandle<()> },
}

#[derive(Default)]
pub struct Engine {
    running: Option<Running>,
    clicks: Arc<AtomicU64>,
    mouse: Option<Device>,
}

impl Engine {
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// Starts clicking; returns a short description of what is being clicked.
    pub fn start(&mut self, cfg: &Config) -> Result<String, String> {
        self.stop();
        self.clicks.store(0, Ordering::Relaxed);
        match cfg.mode {
            Mode::Window => self.start_plugin(cfg),
            Mode::Cursor => self.start_cursor(cfg),
        }
    }

    pub fn stop(&mut self) {
        match self.running.take() {
            Some(Running::Plugin) => {
                let _ = hypr::request("hyprclick stop");
            }
            Some(Running::Thread { stop, handle }) => {
                stop.store(true, Ordering::Relaxed);
                let _ = handle.join();
            }
            None => {}
        }
    }

    /// Click count, whether clicking is paused (target off screen), and the
    /// reason if the plugin stopped on its own (target window closed).
    pub fn poll(&mut self) -> (u64, bool, Option<String>) {
        if let Some(Running::Plugin) = self.running {
            let status = hypr::request("hyprclick status").unwrap_or_default();
            let field = |k: &str| status.split(' ').find_map(|f| f.strip_prefix(k)).unwrap_or("").to_string();
            let clicks = field("clicks=").parse().unwrap_or(0);
            let paused = field("paused=") == "1";
            if field("running=") == "0" {
                self.running = None;
                let err = status.split_once("error=").map(|(_, e)| e.trim().to_string()).filter(|e| e != "-");
                return (clicks, false, Some(err.unwrap_or_else(|| "stopped".into())));
            }
            return (clicks, paused, None);
        }
        (self.clicks.load(Ordering::Relaxed), false, None)
    }

    fn start_plugin(&mut self, cfg: &Config) -> Result<String, String> {
        desktop::get().window_mode()?;
        hypr::plugin::ensure()?;
        let target = desktop::find_target(&cfg.target_class, &cfg.target_title)
            .ok_or_else(|| format!("window '{}' not found", cfg.target_class))?;
        let x = (cfg.pos[0] as f64 * target.size[0]).round();
        let y = (cfg.pos[1] as f64 * target.size[1]).round();
        let reply = hypr::request(&format!(
            "hyprclick start address:{} {x} {y} {} {} {} {} {}",
            target.address,
            cfg.button.code(),
            cfg.interval().as_nanos(),
            cfg.hold().as_nanos(),
            cfg.pause_when_hidden as u8,
            cfg.at_mouse as u8
        ))?;
        if reply.trim() != "ok" {
            return Err(reply.trim().to_string());
        }
        self.running = Some(Running::Plugin);
        if cfg.at_mouse {
            return Ok(format!("{} @ where the mouse is", target.class));
        }
        Ok(format!("{} @ {x},{y}", target.class))
    }

    fn start_cursor(&mut self, cfg: &Config) -> Result<String, String> {
        let dev = match &self.mouse {
            Some(d) => d.clone(),
            None => {
                let d = Arc::new(Mutex::new(make_mouse()?));
                // libinput needs a moment to adopt a fresh device before it
                // forwards its events (KWin is slower about it).
                std::thread::sleep(if desktop::get() == Desktop::Kde { KDE_DEVICE_SETTLE } else { Duration::from_millis(250) });
                self.mouse = Some(d.clone());
                d
            }
        };
        let code = cfg.button.code();
        let target = if cfg.cursor_only_in_target || cfg.cursor_lock {
            Some(desktop::find_target(&cfg.target_class, &cfg.target_title).ok_or_else(|| format!("window '{}' not found", cfg.target_class))?)
        } else {
            None
        };
        let gate = target.as_ref().filter(|_| cfg.cursor_only_in_target).map(|t| t.address.clone());
        let lock = target.as_ref().filter(|_| cfg.cursor_lock).map(|t| Mutex::new(SpotLock::new(t, cfg.pos, dev.clone())));

        let stop = Arc::new(AtomicBool::new(false));
        let focused = Arc::new(AtomicBool::new(true));
        if let Some(addr) = gate.clone() {
            focused.store(desktop::active_address().as_deref() == Some(addr.as_str()), Ordering::Relaxed);
            watch_focus(addr, focused.clone(), stop.clone());
        }

        let locked = lock.is_some();
        let press = move |down: bool| {
            let ev = InputEvent::new(EventType::KEY.0, code, down as i32);
            let _ = dev.lock().unwrap().emit(&[ev]);
        };
        // The lock pulls the cursor back onto the spot before every click.
        let allowed = move || focused.load(Ordering::Relaxed) && lock.as_ref().is_none_or(|l| l.lock().unwrap().pull());
        let handle = spawn_loop(cfg, stop.clone(), self.clicks.clone(), allowed, press);
        self.running = Some(Running::Thread { stop, handle });
        Ok(match (locked, gate) {
            (true, _) => format!("cursor locked to {}", cfg.target_class),
            (false, Some(_)) => format!("cursor, only in {}", cfg.target_class),
            (false, None) => "at cursor".into(),
        })
    }
}

/// Keeps the real cursor on a spot inside a window. Compositors don't let
/// apps place the cursor, so it's walked there with relative virtual-mouse
/// moves, measuring how far each one went (pointer acceleration).
struct SpotLock {
    address: String,
    pos: [f32; 2],
    mouse: Device,
    spot: Option<(f64, f64)>,
    refreshed: Option<Instant>,
    /// Pixels moved per unit sent, per axis.
    gain: [f64; 2],
}

impl SpotLock {
    fn new(target: &desktop::Client, pos: [f32; 2], mouse: Device) -> Self {
        Self { address: target.address.clone(), pos, mouse, spot: None, refreshed: None, gain: [1.0, 1.0] }
    }

    /// The spot in global coordinates; follows the window if it moves.
    /// None while the window is gone or not on screen.
    fn spot(&mut self) -> Option<(f64, f64)> {
        if self.refreshed.is_none_or(|t| t.elapsed() > LOCK_REFRESH) {
            self.refreshed = Some(Instant::now());
            self.spot = desktop::clients().into_iter().find(|c| c.address == self.address && c.visible).map(|w| {
                let inside = |i: usize| (self.pos[i] as f64 * w.size[i]).round().clamp(0.0, (w.size[i] - 1.0).max(0.0));
                (w.at[0] + inside(0), w.at[1] + inside(1))
            });
        }
        self.spot
    }

    /// Move the cursor onto the spot. False if it can't get there (window
    /// hidden, or the cursor is held elsewhere): skip this click then.
    fn pull(&mut self) -> bool {
        let Some((tx, ty)) = self.spot() else { return false };
        let Some(mut at) = desktop::cursor_pos_now() else { return false };
        for _ in 0..LOCK_STEPS {
            let d = [tx - at.0, ty - at.1];
            if d[0].abs() < 1.0 && d[1].abs() < 1.0 {
                return true;
            }
            let send: Vec<i32> = (0..2)
                .map(|i| {
                    let units = (d[i] / self.gain[i]).round() as i32;
                    if units == 0 && d[i].abs() >= 1.0 { d[i].signum() as i32 } else { units }
                })
                .collect();
            let rel = |axis: RelativeAxisCode, v: i32| InputEvent::new(EventType::RELATIVE.0, axis.0, v);
            if self.mouse.lock().unwrap().emit(&[rel(RelativeAxisCode::REL_X, send[0]), rel(RelativeAxisCode::REL_Y, send[1])]).is_err() {
                return false;
            }
            std::thread::sleep(LOCK_SETTLE);
            let Some(now) = desktop::cursor_pos_now() else { return false };
            for (i, moved) in [now.0 - at.0, now.1 - at.1].into_iter().enumerate() {
                if send[i] != 0 && moved != 0.0 && moved.signum() == send[i].signum() as f64 {
                    self.gain[i] = (moved / send[i] as f64).clamp(0.1, 10.0);
                }
            }
            at = now;
        }
        false
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

fn uinput_err(e: std::io::Error) -> String {
    format!("/dev/uinput: {e} (are you in the 'input' group?)")
}

fn make_mouse() -> Result<VirtualDevice, String> {
    let keys = AttributeSet::from_iter([KeyCode::BTN_LEFT, KeyCode::BTN_RIGHT, KeyCode::BTN_MIDDLE]);
    // libinput only treats it as a pointer if it has relative axes.
    let axes = AttributeSet::from_iter([RelativeAxisCode::REL_X, RelativeAxisCode::REL_Y]);
    VirtualDevice::builder()
        .map_err(uinput_err)?
        .name("hyprclick virtual mouse")
        .with_keys(&keys)
        .map_err(uinput_err)?
        .with_relative_axes(&axes)
        .map_err(uinput_err)?
        .build()
        .map_err(uinput_err)
}

/// Fixed-rate click loop. Deadlines are absolute so timing never drifts; the
/// last ~1ms before each deadline is spun instead of slept for precision.
fn spawn_loop(
    cfg: &Config,
    stop: Arc<AtomicBool>,
    clicks: Arc<AtomicU64>,
    allowed: impl Fn() -> bool + Send + 'static,
    press: impl Fn(bool) + Send + 'static,
) -> JoinHandle<()> {
    let interval = cfg.interval();
    let hold = cfg.hold();
    std::thread::spawn(move || {
        let mut next = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            if allowed() {
                press(true);
                if !hold.is_zero() {
                    sleep_until(Instant::now() + hold);
                }
                press(false);
                clicks.fetch_add(1, Ordering::Relaxed);
            }
            next += interval;
            let now = Instant::now();
            if next < now {
                // Fell behind (system hiccup): resync instead of bursting.
                next = now;
            }
            sleep_until(next);
        }
    })
}

fn sleep_until(deadline: Instant) {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        let left = deadline - now;
        if left > Duration::from_micros(1500) {
            std::thread::sleep(left - Duration::from_millis(1));
        } else {
            std::hint::spin_loop();
        }
    }
}

/// Tracks whether `address` has keyboard focus: Hyprland's event socket,
/// or asking KWin a few times a second.
fn watch_focus(address: String, focused: Arc<AtomicBool>, stop: Arc<AtomicBool>) {
    if desktop::get() == Desktop::Kde {
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                focused.store(desktop::active_address().as_deref() == Some(address.as_str()), Ordering::Relaxed);
                std::thread::sleep(KDE_FOCUS_POLL);
            }
        });
        return;
    }
    let Some(path) = hypr::event_socket() else { return };
    std::thread::spawn(move || {
        let Ok(sock) = UnixStream::connect(path) else { return };
        let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));
        let want = address.trim_start_matches("0x").to_string();
        let mut reader = BufReader::new(sock);
        let mut line = String::new();
        while !stop.load(Ordering::Relaxed) {
            match reader.read_line(&mut line) {
                Ok(0) => return,
                Ok(_) => {
                    if let Some(addr) = line.trim().strip_prefix("activewindowv2>>") {
                        focused.store(addr.trim_start_matches("0x") == want, Ordering::Relaxed);
                    }
                    line.clear();
                }
                // Read timeout: keep any partial line and check the stop flag.
                Err(_) => {}
            }
        }
    });
}

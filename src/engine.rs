//! The three click engines behind one start/stop interface.

use crate::config::{Config, Mode, PadButton};
use crate::hypr;
use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode,
    RelativeAxisCode, UinputAbsSetup,
};
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

type Device = Arc<Mutex<VirtualDevice>>;

enum Running {
    Plugin,
    Thread { stop: Arc<AtomicBool>, handle: JoinHandle<()> },
}

#[derive(Default)]
pub struct Engine {
    running: Option<Running>,
    clicks: Arc<AtomicU64>,
    mouse: Option<Device>,
    pad: Option<Device>,
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
            Mode::Gamepad => self.start_gamepad(cfg),
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
        hypr::plugin::ensure()?;
        let target = hypr::find_target(&cfg.target_class, &cfg.target_title)
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
                // forwards its events.
                std::thread::sleep(Duration::from_millis(250));
                self.mouse = Some(d.clone());
                d
            }
        };
        let code = cfg.button.code();
        let gate = if cfg.cursor_only_in_target {
            let target = hypr::find_target(&cfg.target_class, &cfg.target_title)
                .ok_or_else(|| format!("window '{}' not found", cfg.target_class))?;
            Some(target.address)
        } else {
            None
        };

        let stop = Arc::new(AtomicBool::new(false));
        let focused = Arc::new(AtomicBool::new(true));
        if let Some(addr) = gate.clone() {
            focused.store(hypr::active_address().as_deref() == Some(addr.as_str()), Ordering::Relaxed);
            watch_focus(addr, focused.clone(), stop.clone());
        }

        let press = move |down: bool| {
            let ev = InputEvent::new(EventType::KEY.0, code, down as i32);
            let _ = dev.lock().unwrap().emit(&[ev]);
        };
        let handle = spawn_loop(cfg, stop.clone(), self.clicks.clone(), move || focused.load(Ordering::Relaxed), press);
        self.running = Some(Running::Thread { stop, handle });
        Ok(match gate {
            Some(_) => format!("cursor, only in {}", cfg.target_class),
            None => "at cursor".into(),
        })
    }

    fn start_gamepad(&mut self, cfg: &Config) -> Result<String, String> {
        let dev = match &self.pad {
            Some(d) => d.clone(),
            None => {
                let d = Arc::new(Mutex::new(make_pad()?));
                // Give SDL/Roblox time to hotplug the pad before the first press.
                std::thread::sleep(Duration::from_millis(600));
                self.pad = Some(d.clone());
                d
            }
        };
        let button = cfg.pad_button;
        let press = move |down: bool| {
            let ev = match button {
                PadButton::RT => InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_RZ.0, if down { 255 } else { 0 }),
                PadButton::LT => InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_Z.0, if down { 255 } else { 0 }),
                other => InputEvent::new(EventType::KEY.0, pad_key(other).0, down as i32),
            };
            let _ = dev.lock().unwrap().emit(&[ev]);
        };
        let stop = Arc::new(AtomicBool::new(false));
        let handle = spawn_loop(cfg, stop.clone(), self.clicks.clone(), || true, press);
        self.running = Some(Running::Thread { stop, handle });
        Ok(format!("gamepad {}", button.label()))
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

fn pad_key(b: PadButton) -> KeyCode {
    match b {
        PadButton::A => KeyCode::BTN_SOUTH,
        PadButton::B => KeyCode::BTN_EAST,
        PadButton::X => KeyCode::BTN_NORTH,
        PadButton::Y => KeyCode::BTN_WEST,
        PadButton::RB => KeyCode::BTN_TR,
        PadButton::LB => KeyCode::BTN_TL,
        PadButton::RT | PadButton::LT => unreachable!("triggers are axes"),
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
        KeyCode::BTN_MODE,
        KeyCode::BTN_THUMBL,
        KeyCode::BTN_THUMBR,
    ]);
    let stick = AbsInfo::new(0, -32768, 32767, 16, 128, 0);
    let trigger = AbsInfo::new(0, 0, 255, 0, 0, 0);
    let hat = AbsInfo::new(0, -1, 1, 0, 0, 0);
    let mut b = VirtualDevice::builder()
        .map_err(uinput_err)?
        .name("Microsoft X-Box 360 pad")
        // Xbox 360 ids so SDL picks its standard mapping.
        .input_id(InputId::new(BusType::BUS_USB, 0x045e, 0x028e, 0x0110))
        .with_keys(&keys)
        .map_err(uinput_err)?;
    for (axis, info) in [
        (AbsoluteAxisCode::ABS_X, stick),
        (AbsoluteAxisCode::ABS_Y, stick),
        (AbsoluteAxisCode::ABS_RX, stick),
        (AbsoluteAxisCode::ABS_RY, stick),
        (AbsoluteAxisCode::ABS_Z, trigger),
        (AbsoluteAxisCode::ABS_RZ, trigger),
        (AbsoluteAxisCode::ABS_HAT0X, hat),
        (AbsoluteAxisCode::ABS_HAT0Y, hat),
    ] {
        b = b.with_absolute_axis(&UinputAbsSetup::new(axis, info)).map_err(uinput_err)?;
    }
    b.build().map_err(uinput_err)
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

/// Tracks whether `address` has keyboard focus via Hyprland's event socket.
fn watch_focus(address: String, focused: Arc<AtomicBool>, stop: Arc<AtomicBool>) {
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

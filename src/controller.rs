use crate::config::{Config, Mode};
use crate::engine::Engine;
use crate::hypr;
use std::time::Instant;

/// Config + engine + what the UI shows. Shared by the GUI and the IPC thread.
pub struct Controller {
    pub cfg: Config,
    engine: Engine,
    pub desc: String,
    pub error: Option<String>,
    pub clicks: u64,
    /// Running but holding off because the target isn't on screen.
    pub paused: bool,
    pub started: Option<Instant>,
}

impl Controller {
    pub fn new(cfg: Config) -> Self {
        Self { cfg, engine: Engine::default(), desc: String::new(), error: None, clicks: 0, paused: false, started: None }
    }

    pub fn running(&self) -> bool {
        self.engine.is_running()
    }

    pub fn start(&mut self) {
        self.cfg.save();
        match self.engine.start(&self.cfg) {
            Ok(desc) => {
                self.error = None;
                self.clicks = 0;
                self.started = Some(Instant::now());
                if self.cfg.notify {
                    hypr::notify(&format!("hyprclick ON  ·  {} CPS  ·  {desc}", fmt_cps(self.cfg.cps)));
                }
                self.desc = desc;
            }
            Err(e) => {
                if self.cfg.notify {
                    hypr::notify(&format!("hyprclick: {e}"));
                }
                self.error = Some(e);
            }
        }
    }

    pub fn stop(&mut self) {
        if !self.running() {
            return;
        }
        self.poll();
        self.engine.stop();
        self.started = None;
        if self.cfg.notify {
            hypr::notify(&format!("hyprclick OFF  ·  {} clicks", self.clicks));
        }
    }

    pub fn toggle(&mut self) {
        if self.running() { self.stop() } else { self.start() }
    }

    /// Refresh the click count; picks up the plugin stopping on its own.
    pub fn poll(&mut self) {
        if !self.running() {
            return;
        }
        let (clicks, paused, ended) = self.engine.poll();
        self.clicks = clicks;
        self.paused = paused;
        if let Some(reason) = ended {
            self.started = None;
            self.error = Some(reason);
        }
    }

    pub fn status_line(&self) -> String {
        if self.running() {
            let state = if self.paused { "paused (target off screen)" } else { "running" };
            format!("{state} {} clicks · {}", self.clicks, self.desc)
        } else {
            format!("idle · mode {:?}", self.cfg.mode)
        }
    }

    pub fn needs_target(&self) -> bool {
        self.cfg.mode == Mode::Window || (self.cfg.mode == Mode::Cursor && self.cfg.cursor_only_in_target)
    }
}

pub fn fmt_cps(cps: f64) -> String {
    if cps.fract().abs() < 1e-9 { format!("{cps:.0}") } else { format!("{cps:.1}") }
}

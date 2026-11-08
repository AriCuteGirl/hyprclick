//! `hyprclick overlay`: a see-through banner a bit above the middle of the
//! screen while lagswitch freezes the app or fakes lag, with a countdown for
//! timed freezes. It is a Wayland layer surface on the overlay layer, so it
//! shows over fullscreen games, takes no clicks or keys, and only exists while
//! there's something to show. It asks lagswitch's daemon for its status ten
//! times a second, the same `status` line the CLI prints.

use crate::lag::{self, FakeLag, Status};
use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::EventLoop,
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, QueueHandle,
            globals::registry_queue_init,
            protocol::{wl_output, wl_shm, wl_surface},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        WaylandSurface,
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

/// The surface sits centered on the screen; the banner fills its top
/// `BANNER_H` rows and the rest stays clear, which puts the banner's middle
/// `(H - BANNER_H) / 2` = 120 px above the crosshair.
const W: u32 = 250;
const H: u32 = 300;
const BANNER_H: u32 = 60;
const TICK: Duration = Duration::from_millis(100);
/// How long "UNFROZEN" / "FAKE LAG OFF" stays up.
const OFF_FOR: Duration = Duration::from_millis(1200);
const SOCKET: &str = "/run/lagswitch/sock";

#[derive(Clone, PartialEq, Debug)]
struct Banner {
    title: String,
    detail: String,
    /// Bright for a freeze, the accent for fake lag, dim for "off".
    tone: Tone,
    /// How much of a timed freeze is left, 0 to 1.
    bar: Option<f32>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tone {
    Frozen,
    Lag,
    Off,
}

/// Turns status lines into what to show, remembering enough to say when
/// something turned off and how long a timed freeze was.
#[derive(Default)]
struct Watch {
    was_frozen: bool,
    was_lagged: bool,
    /// Longest time left seen in this freeze, which is close to its length.
    total_ms: u64,
    off: Option<(Banner, Instant)>,
}

impl Watch {
    fn update(&mut self, status: &Status, fake_lag: Option<&FakeLag>, now: Instant) -> Option<Banner> {
        let frozen = matches!(status, Status::Frozen { .. });
        if self.was_frozen && !frozen {
            self.off = Some((off_banner("UNFROZEN"), now));
        } else if self.was_lagged && fake_lag.is_none() && !frozen {
            self.off = Some((off_banner("FAKE LAG OFF"), now));
        }
        if !frozen {
            self.total_ms = 0;
        }
        self.was_frozen = frozen;
        self.was_lagged = fake_lag.is_some();

        let lag_text = fake_lag.map(|l| if l.choke { format!("choke {} ms", l.ms) } else { format!("+{} ms ping", l.ms) });
        if let Status::Frozen { mode, left_ms, .. } = status {
            self.off = None;
            self.total_ms = self.total_ms.max(*left_ms);
            let (mut detail, bar) = match mode.as_str() {
                "timed" => (format!("{:.1} s", *left_ms as f64 / 1000.0), Some(*left_ms as f32 / self.total_ms.max(1) as f32)),
                "hold" => ("while held".to_string(), None),
                _ => ("press again".to_string(), None),
            };
            if let Some(lag) = lag_text {
                detail += &format!(" · lag {lag}");
            }
            return Some(Banner { title: "FROZEN".into(), detail, tone: Tone::Frozen, bar });
        }
        if let Some(lag) = lag_text {
            self.off = None;
            return Some(Banner { title: "FAKE LAG".into(), detail: lag, tone: Tone::Lag, bar: None });
        }
        match &self.off {
            Some((banner, since)) if now.duration_since(*since) < OFF_FOR => Some(banner.clone()),
            _ => None,
        }
    }
}

fn off_banner(title: &str) -> Banner {
    Banner { title: title.into(), detail: String::new(), tone: Tone::Off, bar: None }
}

/// The daemon's status line as the CLI prints it, or None when it isn't running.
fn ask_status() -> Option<String> {
    let mut stream = UnixStream::connect(SOCKET).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(500))).ok()?;
    stream.set_write_timeout(Some(Duration::from_millis(500))).ok()?;
    writeln!(stream, "status").ok()?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply).ok()?;
    // "ok idle", or "err ..." which counts as nothing to show.
    reply.trim().strip_prefix("ok ").map(str::to_string)
}

pub fn run() {
    // One overlay per session: a second `hyprclick overlay` just leaves.
    let Ok(_lock) = single_instance() else { return };
    if let Err(e) = serve() {
        eprintln!("hyprclick overlay: {e}");
        std::process::exit(1);
    }
}

fn single_instance() -> std::io::Result<std::os::unix::net::UnixListener> {
    use std::os::linux::net::SocketAddrExt;
    let addr = std::os::unix::net::SocketAddr::from_abstract_name(b"hyprclick-overlay")?;
    std::os::unix::net::UnixListener::bind_addr(&addr)
}

/// Start the overlay in the background if it's turned on and not running yet.
pub fn ensure_running() {
    if !crate::config::Config::load().lag_overlay || !lag::installed() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let _ = std::process::Command::new(exe)
        .arg("overlay")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn serve() -> Result<(), String> {
    let fonts = Fonts::load()?;
    let conn = Connection::connect_to_env().map_err(|e| format!("no Wayland session: {e}"))?;
    let (globals, queue) = registry_queue_init::<Overlay>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| format!("wl_compositor: {e}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|_| "this compositor has no layer shell".to_string())?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| format!("wl_shm: {e}"))?;
    let pool = SlotPool::new((W * H * 4) as usize, &shm).map_err(|e| e.to_string())?;

    let mut event_loop = EventLoop::<Overlay>::try_new().map_err(|e| e.to_string())?;
    WaylandSource::new(conn, queue).insert(event_loop.handle()).map_err(|e| e.to_string())?;

    let mut overlay = Overlay {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        shm,
        pool,
        fonts,
        layer: None,
        configured: false,
        shown: None,
        want: None,
    };
    let mut watch = Watch::default();
    let mut enabled = (Instant::now(), true);
    loop {
        let now = Instant::now();
        // The Lag tab's checkbox; checked once a second.
        if now.duration_since(enabled.0) >= Duration::from_secs(1) {
            enabled = (now, crate::config::Config::load().lag_overlay);
        }
        let banner = ask_status().and_then(|text| {
            let fake_lag = lag::parse_fake_lag(&text);
            watch.update(&lag::parse_status(&text), fake_lag.as_ref(), now)
        });
        overlay.want = banner.filter(|_| enabled.1);
        overlay.sync(&qh);
        event_loop.dispatch(TICK, &mut overlay).map_err(|e| e.to_string())?;
    }
}

struct Overlay {
    registry: RegistryState,
    outputs: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    pool: SlotPool,
    fonts: Fonts,
    layer: Option<LayerSurface>,
    configured: bool,
    /// What's on screen now, and what should be.
    shown: Option<Banner>,
    want: Option<Banner>,
}

impl Overlay {
    /// Make the screen match `want`: open, redraw or close the surface.
    fn sync(&mut self, qh: &QueueHandle<Self>) {
        match (&self.want, &self.layer) {
            (None, Some(_)) => {
                // Dropping the surface takes it off screen.
                self.layer = None;
                self.shown = None;
            }
            (Some(_), None) => self.open(qh),
            (Some(want), Some(_)) if self.configured && self.shown.as_ref() != Some(want) => self.draw(),
            _ => {}
        }
    }

    fn open(&mut self, qh: &QueueHandle<Self>) {
        let surface = self.compositor.create_surface(qh);
        // No output given: the compositor puts it on the focused monitor.
        let layer = self.layer_shell.create_layer_surface(qh, surface, Layer::Overlay, Some("hyprclick-overlay"), None);
        // No anchors centers it; -1 centers it on the whole screen, bars or not.
        layer.set_anchor(Anchor::empty());
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(W, H);
        // An empty input region lets every click through to the game.
        if let Ok(region) = Region::new(&self.compositor) {
            layer.wl_surface().set_input_region(Some(region.wl_region()));
        }
        layer.commit();
        self.layer = Some(layer);
        self.configured = false;
        self.shown = None;
    }

    fn draw(&mut self) {
        let (Some(layer), Some(banner)) = (&self.layer, &self.want) else { return };
        let Ok((buffer, canvas)) = self.pool.create_buffer(W as i32, H as i32, (W * 4) as i32, wl_shm::Format::Argb8888) else {
            return;
        };
        let pixels = render(banner, &self.fonts);
        for (dst, px) in canvas.chunks_exact_mut(4).zip(pixels) {
            dst.copy_from_slice(&px.to_le_bytes());
        }
        layer.wl_surface().damage_buffer(0, 0, W as i32, H as i32);
        if buffer.attach_to(layer.wl_surface()).is_err() {
            return;
        }
        layer.commit();
        self.shown = Some(banner.clone());
    }
}

impl CompositorHandler for Overlay {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for Overlay {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for Overlay {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        // The compositor took it away (its monitor went off); open a new one next tick.
        self.layer = None;
        self.shown = None;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface, _: LayerSurfaceConfigure, _: u32) {
        self.configured = true;
        self.shown = None;
        self.draw();
    }
}

impl ShmHandler for Overlay {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Overlay {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState];
}

delegate_compositor!(Overlay);
delegate_output!(Overlay);
delegate_shm!(Overlay);
delegate_layer!(Overlay);
delegate_registry!(Overlay);

struct Fonts {
    bold: FontVec,
    regular: FontVec,
}

impl Fonts {
    /// The rice's font like the window uses, else DejaVu Sans.
    fn load() -> Result<Self, String> {
        let theme = crate::theme::get();
        let read = |path: &Option<std::path::PathBuf>| path.as_ref().and_then(|p| std::fs::read(p).ok()).and_then(|b| FontVec::try_from_vec(b).ok());
        let fallback = || read(&crate::theme::fallback_font());
        let regular = read(&theme.font_regular).or_else(fallback).ok_or("no font found (install DejaVu Sans)")?;
        let bold = read(&theme.font_bold).or_else(|| read(&theme.font_regular)).or_else(fallback).ok_or("no font found")?;
        Ok(Self { bold, regular })
    }
}

/// Premultiplied ARGB, one u32 per pixel, row by row.
fn render(banner: &Banner, fonts: &Fonts) -> Vec<u32> {
    let theme = crate::theme::get();
    let mut px = vec![0u32; (W * H) as usize];
    let rgb = |c: eframe::egui::Color32| [c.r(), c.g(), c.b()];
    let tint = match banner.tone {
        Tone::Frozen => rgb(theme.bad),
        Tone::Lag => rgb(theme.accent),
        Tone::Off => rgb(theme.weak),
    };
    let bg = theme.deep;

    // The banner: a rounded box with a colored edge.
    let (bw, bh) = (W as f32 - 4.0, BANNER_H as f32 - 4.0);
    let (bx, by) = (2.0, 2.0);
    rounded_rect(&mut px, bx, by, bw, bh, 12.0, tint, 0.95);
    rounded_rect(&mut px, bx + 2.0, by + 2.0, bw - 4.0, bh - 4.0, 10.0, rgb(bg), 0.88);

    let title_y = if banner.detail.is_empty() { by + bh / 2.0 + 8.0 } else { by + 25.0 };
    text(&mut px, &fonts.bold, &banner.title, 22.0, W as f32 / 2.0, title_y, tint);
    if !banner.detail.is_empty() {
        text(&mut px, &fonts.regular, &banner.detail, 13.0, W as f32 / 2.0, by + 43.0, rgb(theme.text));
    }
    if let Some(left) = banner.bar {
        let (x, y, w, h) = (bx + 14.0, by + bh - 8.0, bw - 28.0, 3.0);
        rounded_rect(&mut px, x, y, w, h, 1.5, rgb(theme.line), 0.9);
        rounded_rect(&mut px, x, y, (w * left.clamp(0.0, 1.0)).max(h), h, 1.5, tint, 1.0);
    }
    px
}

/// Paint `rgb` at `alpha` (0 to 1) over the pixel.
fn blend(px: &mut [u32], x: i32, y: i32, rgb: [u8; 3], alpha: f32) {
    if x < 0 || y < 0 || x >= W as i32 || y >= H as i32 || alpha <= 0.0 {
        return;
    }
    let i = (y as u32 * W + x as u32) as usize;
    let d = px[i];
    let a = alpha.min(1.0);
    let ch = |src: u8, shift: u32| src as f32 * a + ((d >> shift) & 0xff) as f32 * (1.0 - a);
    let out_a = 255.0 * a + ((d >> 24) & 0xff) as f32 * (1.0 - a);
    px[i] = ((out_a.round() as u32) << 24) | ((ch(rgb[0], 16).round() as u32) << 16) | ((ch(rgb[1], 8).round() as u32) << 8) | ch(rgb[2], 0).round() as u32;
}

/// A filled box with round corners and smooth edges.
fn rounded_rect(px: &mut [u32], x: f32, y: f32, w: f32, h: f32, r: f32, rgb: [u8; 3], alpha: f32) {
    let r = r.min(w / 2.0).min(h / 2.0);
    for py in y.floor() as i32..(y + h).ceil() as i32 {
        for pxx in x.floor() as i32..(x + w).ceil() as i32 {
            // Distance outside the rounded shape, measured at the pixel's center.
            let (cx, cy) = (pxx as f32 + 0.5, py as f32 + 0.5);
            let qx = (cx - (x + w / 2.0)).abs() - (w / 2.0 - r);
            let qy = (cy - (y + h / 2.0)).abs() - (h / 2.0 - r);
            let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r;
            blend(px, pxx, py, rgb, alpha * (0.5 - outside).clamp(0.0, 1.0));
        }
    }
}

/// One line of text, centered on `cx`, with its baseline at `baseline`.
fn text(px: &mut [u32], font: &FontVec, s: &str, size: f32, cx: f32, baseline: f32, rgb: [u8; 3]) {
    let scaled = font.as_scaled(PxScale::from(size));
    let mut glyphs = Vec::new();
    let mut pen = 0.0;
    let mut prev = None;
    for ch in s.chars() {
        let id = scaled.glyph_id(ch);
        if let Some(p) = prev {
            pen += scaled.kern(p, id);
        }
        glyphs.push(id.with_scale_and_position(PxScale::from(size), ab_glyph::point(pen, 0.0)));
        pen += scaled.h_advance(id);
        prev = Some(id);
    }
    let left = cx - pen / 2.0;
    for mut glyph in glyphs {
        glyph.position = ab_glyph::point(left + glyph.position.x, baseline);
        if let Some(outline) = font.outline_glyph(glyph) {
            let bounds = outline.px_bounds();
            outline.draw(|gx, gy, coverage| blend(px, bounds.min.x as i32 + gx as i32, bounds.min.y as i32 + gy as i32, rgb, coverage));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lag(ms: u64) -> FakeLag {
        FakeLag { ms, choke: false, held: (0, 0) }
    }

    #[test]
    fn counts_down_a_timed_freeze() {
        let mut w = Watch::default();
        let t = Instant::now();
        let frozen = |left_ms| Status::Frozen { mode: "timed".into(), direction: "both".into(), left_ms };
        let b = w.update(&frozen(3000), None, t).unwrap();
        assert_eq!((b.title.as_str(), b.detail.as_str(), b.bar), ("FROZEN", "3.0 s", Some(1.0)));
        let b = w.update(&frozen(1500), None, t).unwrap();
        assert_eq!((b.detail.as_str(), b.bar), ("1.5 s", Some(0.5)));
    }

    #[test]
    fn says_off_for_a_moment() {
        let mut w = Watch::default();
        let t = Instant::now();
        assert!(w.update(&Status::Idle, None, t).is_none());
        let frozen = Status::Frozen { mode: "toggle".into(), direction: "both".into(), left_ms: 0 };
        assert_eq!(w.update(&frozen, Some(&lag(200)), t).unwrap().detail, "press again · lag +200 ms ping");
        // Unfrozen with fake lag still on: show the lag, not "UNFROZEN".
        assert_eq!(w.update(&Status::Idle, Some(&lag(200)), t).unwrap().title, "FAKE LAG");
        assert_eq!(w.update(&Status::Idle, None, t).unwrap().title, "FAKE LAG OFF");
        assert!(w.update(&Status::Idle, None, t + OFF_FOR).is_none());
    }

    #[test]
    fn hides_when_the_daemon_is_idle() {
        let mut w = Watch::default();
        assert!(w.update(&Status::Down("no daemon".into()), None, Instant::now()).is_none());
    }
}

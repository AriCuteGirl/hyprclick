//! The lag overlay: a small see-through banner while lagswitch freezes the
//! app or fakes lag, with a countdown for timed freezes, and a short "off"
//! flash when that ends. It takes no clicks or keys and is only on screen
//! while there's something to show. The drawing is shared; showing it is a
//! Wayland layer surface on Linux (`hyprclick overlay`, its own process) and a
//! layered topmost window on Windows (a thread in the hyprclick window).

use crate::config::{Config, OverlayPlace};
use crate::lag::{FakeLag, Status};
use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
mod wayland;
#[cfg(target_os = "linux")]
pub use wayland::{ensure_running, run};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{demo, ensure_running};

/// At the top or bottom the surface is just the banner. In the middle it is
/// centered on the screen and `H` tall; the banner fills its top `BANNER_H`
/// rows and the rest stays clear, which puts the banner's middle
/// `(H - BANNER_H) / 2` = 120 px above the crosshair.
const W: u32 = 250;
const H: u32 = 300;
const BANNER_H: u32 = 60;
const TICK: Duration = Duration::from_millis(100);
/// How long "UNFROZEN" / "FAKE LAG OFF" stays up.
const OFF_FOR: Duration = Duration::from_millis(1200);

/// Surface height for a place.
fn height(place: OverlayPlace) -> u32 {
    if place == OverlayPlace::Middle { H } else { BANNER_H }
}

/// Status lines in, banners out, with the Lag tab's settings read once a second.
struct Feed {
    watch: Watch,
    checked: Option<Instant>,
    enabled: bool,
    place: OverlayPlace,
}

impl Feed {
    fn new() -> Self {
        Self { watch: Watch::default(), checked: None, enabled: true, place: OverlayPlace::default() }
    }

    /// What to show now, given lagswitch's status line (None when it's not there).
    fn next(&mut self, status: Option<String>) -> Option<Banner> {
        let now = Instant::now();
        if self.checked.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(1)) {
            let cfg = Config::load();
            (self.enabled, self.place, self.checked) = (cfg.lag_overlay, cfg.lag_overlay_place, Some(now));
        }
        let text = status?;
        let fake_lag = crate::lag::parse_fake_lag(&text);
        self.watch.update(&crate::lag::parse_status(&text), fake_lag.as_ref(), now).filter(|_| self.enabled)
    }
}

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
            // Toggle and hold freezes have a time limit too, so they count down as well.
            let seconds = format!("{:.1} s", *left_ms as f64 / 1000.0);
            let bar = (*left_ms > 0).then(|| *left_ms as f32 / self.total_ms.max(1) as f32);
            let mut detail = match (mode.as_str(), *left_ms > 0) {
                ("timed", _) => seconds,
                ("hold", true) => format!("held · {seconds}"),
                ("hold", false) => "while held".to_string(),
                (_, true) => format!("{seconds} left"),
                (_, false) => "press again".to_string(),
            };
            // Short, to fit next to the time.
            if let Some(l) = fake_lag {
                detail += &if l.choke { format!(" · choke {}", l.ms) } else { format!(" · +{} ms", l.ms) };
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

struct Fonts {
    bold: FontVec,
    regular: FontVec,
}

impl Fonts {
    /// The rice's font like the window uses, else Segoe UI on Windows or DejaVu Sans.
    fn load() -> Result<Self, String> {
        let read = |path: &Option<std::path::PathBuf>| path.as_ref().and_then(|p| std::fs::read(p).ok()).and_then(|b| FontVec::try_from_vec(b).ok());
        #[cfg(windows)]
        let theme = {
            let fonts = std::env::var_os("WINDIR").map_or_else(|| std::path::PathBuf::from(r"C:\Windows"), std::path::PathBuf::from).join("Fonts");
            (Some(fonts.join("segoeui.ttf")), Some(fonts.join("segoeuib.ttf")))
        };
        #[cfg(not(windows))]
        let theme = (crate::theme::get().font_regular.clone(), crate::theme::get().font_bold.clone());
        let fallback = || read(&crate::theme::fallback_font());
        let regular = read(&theme.0).or_else(fallback).ok_or("no font found (install DejaVu Sans)")?;
        let bold = read(&theme.1).or_else(|| read(&theme.0)).or_else(fallback).ok_or("no font found")?;
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
        let frozen = Status::Frozen { mode: "toggle".into(), direction: "both".into(), left_ms: 25000 };
        let b = w.update(&frozen, Some(&lag(200)), t).unwrap();
        assert_eq!((b.detail.as_str(), b.bar), ("25.0 s left · +200 ms", Some(1.0)));
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

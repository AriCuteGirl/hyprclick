use crate::config::{Config, Mode, MouseButton, PadButton, MAX_CPS};
use crate::controller::{fmt_cps, Controller};
use crate::{hypr, ipc, theme, APP_ID};
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily,
    FontId, Layout, Margin, Rect, RichText, Sense, Stroke, StrokeKind, Ui,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Colors and font come from the user's Noctalia rice (see theme.rs).
fn pal() -> &'static theme::Theme {
    theme::get()
}

// Nerd Font glyphs.
const I_POWER: &str = "\u{f011}";
const I_WINDOW: &str = "\u{f2d0}";
const I_CURSOR: &str = "\u{f245}";
const I_PAD: &str = "\u{f11b}";
const I_REFRESH: &str = "\u{f021}";
const I_TARGET: &str = "\u{f05b}";
const I_HEART: &str = "\u{f004}";
const I_BOLT: &str = "\u{f0e7}";

const PICK_SECS: f32 = 3.0;
const MAX_CONTENT_WIDTH: f32 = 400.0;

#[derive(Clone)]
enum PluginState {
    Checking,
    Ready,
    Failed(String),
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Target,
    Click,
}

struct App {
    ctrl: Arc<Mutex<Controller>>,
    plugin: Arc<Mutex<PluginState>>,
    windows: Vec<hypr::Client>,
    windows_at: Instant,
    pick_until: Option<Instant>,
    pick_preview: Option<(hypr::Client, f64, f64)>,
    polled_at: Instant,
    meter: (u64, Instant, f64),
    saved: String,
    dirty: bool,
    tab: Tab,
}

pub fn run() {
    let ctrl = Arc::new(Mutex::new(Controller::new(Config::load())));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id(APP_ID)
            .with_title("hyprclick")
            .with_inner_size([380.0, 640.0])
            .with_min_inner_size([260.0, 240.0])
            .with_transparent(true),
        ..Default::default()
    };
    let _ = eframe::run_native(
        APP_ID,
        options,
        Box::new(move |cc| {
            setup_style(&cc.egui_ctx);
            let ctx = cc.egui_ctx.clone();
            let _ = ipc::serve(ctrl.clone(), "gui", move |_| ctx.request_repaint());
            Ok(Box::new(App::new(ctrl, &cc.egui_ctx)))
        }),
    );
}

impl App {
    fn new(ctrl: Arc<Mutex<Controller>>, ctx: &egui::Context) -> Self {
        // Build/load the plugin up front so the first start is instant.
        let plugin = Arc::new(Mutex::new(PluginState::Checking));
        let (p, ctx2) = (plugin.clone(), ctx.clone());
        std::thread::spawn(move || {
            *p.lock().unwrap() = match hypr::plugin::ensure() {
                Ok(()) => PluginState::Ready,
                Err(e) => PluginState::Failed(e),
            };
            ctx2.request_repaint();
        });
        let saved = serde_json::to_string(&ctrl.lock().unwrap().cfg).unwrap_or_default();
        Self {
            ctrl,
            plugin,
            windows: hypr::clients(),
            windows_at: Instant::now(),
            pick_until: None,
            pick_preview: None,
            polled_at: Instant::now(),
            meter: (0, Instant::now(), 0.0),
            saved,
            dirty: false,
            tab: Tab::Click,
        }
    }

    fn tick(&mut self, ctx: &egui::Context, c: &mut Controller) {
        let now = Instant::now();
        if now - self.windows_at > Duration::from_secs(2) {
            self.windows = hypr::clients();
            self.windows_at = now;
        }
        if c.running() && now - self.polled_at > Duration::from_millis(200) {
            c.poll();
            self.polled_at = now;
        }
        let (last, at, ema) = self.meter;
        let dt = (now - at).as_secs_f64();
        if dt >= 0.5 {
            let rate = if c.running() { c.clicks.saturating_sub(last) as f64 / dt } else { 0.0 };
            let ema = if ema == 0.0 { rate } else { ema * 0.4 + rate * 0.6 };
            self.meter = (c.clicks, now, if c.running() { ema } else { 0.0 });
        }

        if let Some(until) = self.pick_until {
            let pos = hypr::cursor_pos();
            self.pick_preview = pos.and_then(|(x, y)| hypr::window_at(x, y).map(|w| (w, x, y)));
            if now >= until {
                self.pick_until = None;
                if let Some((w, x, y)) = self.pick_preview.take() {
                    c.cfg.target_class = w.class.clone();
                    c.cfg.target_title = w.title.clone();
                    c.cfg.pos = [
                        ((x - w.at[0]) / w.size[0]).clamp(0.0, 1.0) as f32,
                        ((y - w.at[1]) / w.size[1]).clamp(0.0, 1.0) as f32,
                    ];
                }
            }
        }

        // Persist changes; apply them live once the user lets go of a drag.
        let now_cfg = serde_json::to_string(&c.cfg).unwrap_or_default();
        if now_cfg != self.saved {
            self.saved = now_cfg;
            c.cfg.save();
            self.dirty = c.running();
        }
        if self.dirty && !ctx.input(|i| i.pointer.any_down()) {
            self.dirty = false;
            if c.running() {
                let notify = std::mem::replace(&mut c.cfg.notify, false);
                c.start();
                c.cfg.notify = notify;
            }
        }

        if c.running() || self.pick_until.is_some() {
            ctx.request_repaint_after(Duration::from_millis(16));
        } else {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctrl = self.ctrl.clone();
        let mut c = ctrl.lock().unwrap();
        self.tick(ui.ctx(), &mut c);

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(pal().bg).inner_margin(Margin::symmetric(12, 10)))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    // Keep a narrow column even when the window is tiled wide.
                    let width = ui.available_width().min(MAX_CONTENT_WIDTH);
                    let side = (ui.available_width() - width) / 2.0;
                    ui.horizontal_top(|ui| {
                        ui.add_space(side);
                        ui.vertical(|ui| {
                            ui.set_width(width);
                            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                            self.header(ui);
                            self.hero(ui, &mut c);
                            self.mode_card(ui, &mut c);
                            // Target and click settings share one spot so
                            // the window never needs scrolling.
                            if c.needs_target() {
                                let tabs = [(Tab::Target, format!("{I_TARGET}  Target")), (Tab::Click, format!("{I_BOLT}  Click"))];
                                segmented(ui, "tab", &mut self.tab, &tabs);
                            } else {
                                self.tab = Tab::Click;
                            }
                            match self.tab {
                                Tab::Target => self.target_card(ui, &mut c),
                                Tab::Click => self.click_card(ui, &mut c),
                            }
                            self.footer(ui);
                        });
                    });
                });
            });
    }
}

// ───────────────────────────── sections ─────────────────────────────

impl App {
    fn header(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(I_HEART).font(FontId::new(16.0, icons())).color(pal().accent));
            ui.label(RichText::new("hyprclick").font(FontId::new(18.0, bold())).color(pal().text));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (txt, col) = match &*self.plugin.lock().unwrap() {
                    PluginState::Checking => ("plugin …".to_string(), pal().weak),
                    PluginState::Ready => ("plugin ready".to_string(), pal().good),
                    PluginState::Failed(_) => ("plugin off".to_string(), pal().bad),
                };
                pill(ui, &txt, col);
            });
        });
    }

    fn hero(&mut self, ui: &mut Ui, c: &mut Controller) {
        let running = c.running();
        card_with(ui, None, Stroke::new(2.0, pal().accent), |ui| {
            ui.horizontal(|ui| {
                if power_button(ui, running).clicked() {
                    c.toggle();
                }
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.add_space(2.0);
                    let (state, col) = match (running, c.paused) {
                        (true, true) => ("PAUSED", pal().accent2),
                        (true, false) => ("CLICKING", pal().accent),
                        _ => ("IDLE", pal().weak),
                    };
                    ui.label(RichText::new(state).font(FontId::new(19.0, bold())).color(col));
                    let sub = if running && c.paused {
                        "target isn't on screen, resumes when it is".into()
                    } else if running {
                        c.desc.clone()
                    } else {
                        "ALT+H or the button to start".into()
                    };
                    ui.label(RichText::new(sub).color(pal().weak).size(11.5));
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        stat(ui, "clicks", &c.clicks.to_string());
                        stat(ui, "live", &format!("{:.1}/s", self.meter.2));
                        let t = c.started.map(|s| s.elapsed().as_secs()).unwrap_or(0);
                        stat(ui, "time", &format!("{}:{:02}", t / 60, t % 60));
                    });
                });
            });
        });
        if let Some(err) = &c.error {
            ui.label(RichText::new(format!("⚠ {err}")).color(pal().bad).size(12.0));
        }
    }

    fn mode_card(&mut self, ui: &mut Ui, c: &mut Controller) {
        card(ui, Some("MODE"), |ui| {
            segmented(
                ui,
                "mode",
                &mut c.cfg.mode,
                &[
                    (Mode::Window, format!("{I_WINDOW}  Window")),
                    (Mode::Cursor, format!("{I_CURSOR}  Cursor")),
                    (Mode::Gamepad, format!("{I_PAD}  Gamepad")),
                ],
            );
            let hint = match c.cfg.mode {
                Mode::Window => "Clicks the chosen window in the background, mouse stays free.",
                Mode::Cursor => "Clicks wherever your cursor is.",
                Mode::Gamepad => "Virtual Xbox pad, works in the background (Roblox: RT = tool).",
            };
            ui.label(RichText::new(hint).color(pal().weak).size(11.5));
        });
    }

    fn target_card(&mut self, ui: &mut Ui, c: &mut Controller) {
        let target = hypr::find_target(&c.cfg.target_class, &c.cfg.target_title);
        card(ui, None, |ui| {
            ui.horizontal(|ui| {
                let w = ui.available_width() - 44.0;
                let shown = if c.cfg.target_class.is_empty() { "choose a window".into() } else { c.cfg.target_class.clone() };
                egui::ComboBox::from_id_salt("target")
                    .selected_text(RichText::new(shown).color(pal().text))
                    .width(w)
                    .height(320.0)
                    .show_ui(ui, |ui| {
                        for win in self.windows.iter().filter(|w| w.class != APP_ID) {
                            let sel = win.class == c.cfg.target_class && (c.cfg.target_title.is_empty() || win.title == c.cfg.target_title);
                            let label = format!("{}  ·  {}", win.class, truncate(&win.title, 34));
                            if ui.selectable_label(sel, label).clicked() {
                                c.cfg.target_class = win.class.clone();
                                c.cfg.target_title = win.title.clone();
                            }
                        }
                    });
                if icon_button(ui, I_REFRESH).on_hover_text("Refresh window list").clicked() {
                    self.windows = hypr::clients();
                    self.windows_at = Instant::now();
                }
            });

            match &target {
                Some(t) => {
                    let kind = if t.xwayland { "XWayland" } else { "Wayland" };
                    ui.label(
                        RichText::new(format!("●  open · {}×{} · {kind} · ws {}", t.size[0], t.size[1], t.workspace_id))
                            .color(pal().good)
                            .size(11.5),
                    );
                }
                None => {
                    ui.label(RichText::new("○  not open right now").color(pal().weak).size(11.5));
                }
            }

            if c.cfg.mode == Mode::Cursor {
                ui.horizontal(|ui| {
                    toggle_switch(ui, &mut c.cfg.cursor_only_in_target);
                    ui.label(RichText::new("Only click while this window is focused").color(pal().text));
                });
                return;
            }

            ui.horizontal(|ui| {
                toggle_switch(ui, &mut c.cfg.pause_when_hidden);
                ui.label(RichText::new("Pause when target isn't on screen").color(pal().text));
            });

            ui.add_space(2.0);
            let opts = [(false, format!("{I_TARGET}  Picked spot")), (true, format!("{I_CURSOR}  Where mouse is"))];
            segmented(ui, "click_at", &mut c.cfg.at_mouse, &opts);
            if c.cfg.at_mouse {
                ui.label(
                    RichText::new("Doesn't move the game's pointer. Clicks wherever your mouse last was inside the window (until then, the picked spot).")
                        .color(pal().weak)
                        .size(11.5),
                );
                return;
            }

            position_pad(ui, &mut c.cfg.pos, target.as_ref().map(|t| t.size), self.pick_until.is_some());

            ui.horizontal(|ui| {
                let size = target.as_ref().map(|t| t.size).unwrap_or([0.0, 0.0]);
                for (i, axis) in ["X", "Y"].iter().enumerate() {
                    ui.label(RichText::new(*axis).color(pal().weak));
                    if size[i] > 0.0 {
                        let mut px = (c.cfg.pos[i] as f64 * size[i]).round();
                        if ui.add(egui::DragValue::new(&mut px).range(0.0..=size[i] - 1.0).speed(1.0).suffix(" px")).changed() {
                            c.cfg.pos[i] = (px / size[i]) as f32;
                        }
                    } else {
                        let mut pct = c.cfg.pos[i] * 100.0;
                        if ui.add(egui::DragValue::new(&mut pct).range(0.0..=100.0).speed(0.2).suffix(" %")).changed() {
                            c.cfg.pos[i] = pct / 100.0;
                        }
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let label = match self.pick_until {
                        Some(until) => {
                            let left = until.saturating_duration_since(Instant::now()).as_secs_f32();
                            format!("hover it… {left:.1}s")
                        }
                        None => format!("{I_TARGET}  Pick on screen"),
                    };
                    if accent_button(ui, &label, self.pick_until.is_some()).clicked() && self.pick_until.is_none() {
                        self.pick_until = Some(Instant::now() + Duration::from_secs_f32(PICK_SECS));
                    }
                });
            });
            if let (Some(_), Some((w, x, y))) = (self.pick_until, &self.pick_preview) {
                ui.label(
                    RichText::new(format!("{} @ {:.0}, {:.0}", w.class, x - w.at[0], y - w.at[1]))
                        .color(pal().accent)
                        .size(11.5),
                );
            }
        });
    }

    fn click_card(&mut self, ui: &mut Ui, c: &mut Controller) {
        card(ui, None, |ui| {
            if c.cfg.mode == Mode::Gamepad {
                let row1: Vec<(PadButton, String)> = PadButton::ALL[..4].iter().map(|b| (*b, b.label().to_string())).collect();
                let row2: Vec<(PadButton, String)> = PadButton::ALL[4..].iter().map(|b| (*b, b.label().to_string())).collect();
                segmented(ui, "pad1", &mut c.cfg.pad_button, &row1);
                segmented(ui, "pad2", &mut c.cfg.pad_button, &row2);
            } else {
                let opts: Vec<(MouseButton, String)> =
                    [MouseButton::Left, MouseButton::Right, MouseButton::Middle].iter().map(|b| (*b, b.label().to_string())).collect();
                segmented(ui, "button", &mut c.cfg.button, &opts);
            }

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut c.cfg.cps)
                        .range(0.1..=MAX_CPS)
                        .speed(0.25)
                        .max_decimals(1)
                        .custom_formatter(|v, _| fmt_cps(v)),
                );
                ui.label(RichText::new("CPS").font(FontId::new(13.0, bold())).color(pal().accent));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let ms = 1000.0 / c.cfg.cps.max(0.1);
                    ui.label(RichText::new(format!("every {ms:.2} ms")).color(pal().weak).size(11.5));
                });
            });
            ui.spacing_mut().slider_width = ui.available_width();
            ui.add(egui::Slider::new(&mut c.cfg.cps, 1.0..=MAX_CPS).logarithmic(true).show_value(false));
            ui.horizontal_wrapped(|ui| {
                for preset in [5.0, 10.0, 20.0, 50.0, 100.0, 250.0, 500.0] {
                    if chip(ui, &fmt_cps(preset), (c.cfg.cps - preset).abs() < 1e-6).clicked() {
                        c.cfg.cps = preset;
                    }
                }
            });

            ui.add_space(4.0);
            ui.separator();
            ui.horizontal(|ui| {
                toggle_switch(ui, &mut c.cfg.hold_enabled);
                ui.label(RichText::new("Hold duration").color(pal().text));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if c.cfg.hold_enabled {
                        ui.add(egui::DragValue::new(&mut c.cfg.hold_ms).range(0.0..=1000.0).speed(0.5).suffix(" ms"));
                    }
                });
            });
            if c.cfg.hold_enabled {
                ui.add(egui::Slider::new(&mut c.cfg.hold_ms, 0.0..=250.0).show_value(false));
                let capped = c.cfg.hold().as_secs_f64() * 1000.0;
                if capped + 0.01 < c.cfg.hold_ms {
                    ui.label(RichText::new(format!("capped to {capped:.1} ms at this CPS")).color(pal().accent2).size(12.0));
                }
            } else {
                ui.label(RichText::new("Off: instant press + release").color(pal().weak).size(11.5));
            }

            ui.separator();
            ui.horizontal(|ui| {
                toggle_switch(ui, &mut c.cfg.notify);
                ui.label(RichText::new("Notifications").color(pal().text));
            });
        });
    }

    fn footer(&mut self, ui: &mut Ui) {
        if let PluginState::Failed(e) = &*self.plugin.lock().unwrap() {
            ui.label(RichText::new(format!("Window mode unavailable: {e}")).color(pal().bad).size(12.0));
        }
        ui.label(RichText::new("ALT+H toggles from anywhere  ·  hyprclick toggle").color(pal().weak).size(10.5));
    }
}

// ───────────────────────────── widgets ─────────────────────────────

fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

fn icons() -> FontFamily {
    FontFamily::Name("icons".into())
}

fn card(ui: &mut Ui, title: Option<&str>, add: impl FnOnce(&mut Ui)) {
    card_with(ui, title, Stroke::new(1.0, pal().line), add);
}

fn card_with(ui: &mut Ui, title: Option<&str>, stroke: Stroke, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(pal().card.gamma_multiply(0.92))
        .stroke(stroke)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(11))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some(t) = title {
                ui.label(RichText::new(t).font(FontId::new(10.0, bold())).color(pal().weak).extra_letter_spacing(1.4));
            }
            add(ui);
        });
}

fn pill(ui: &mut Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.12))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.5)))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(format!("● {text}")).color(color).size(10.5));
        });
}

fn stat(ui: &mut Ui, label: &str, value: &str) {
    egui::Frame::new()
        .fill(pal().deep)
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(7, 3))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(RichText::new(value).font(FontId::monospace(13.0)).color(pal().text));
                ui.label(RichText::new(label).color(pal().weak).size(9.5));
            });
        });
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

fn power_button(ui: &mut Ui, running: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(82.0, 82.0), Sense::click());
    let p = ui.painter();
    let center = rect.center();
    let r = 31.0;
    let t = ui.input(|i| i.time) as f32;
    let on = ui.ctx().animate_bool_with_time(resp.id, running, 0.25);

    // Soft glow, stronger when running.
    for i in 0..10 {
        let a = (0.10 - i as f32 * 0.01).max(0.0) * (0.35 + on * 0.65);
        p.circle_filled(center, r + 3.0 + i as f32 * 1.0, pal().accent.gamma_multiply(a));
    }
    if running {
        for k in 0..2 {
            let phase = (t * 0.9 + k as f32 * 0.5).fract();
            p.circle_stroke(center, r + 2.0 + phase * 9.0, Stroke::new(2.0, pal().accent.gamma_multiply((1.0 - phase) * 0.7)));
        }
    }
    let hover = if resp.hovered() { 0.12 } else { 0.0 };
    let fill = lerp_color(pal().card_hi, pal().accent, on);
    p.circle_filled(center, r, lerp_color(fill, Color32::WHITE, hover));
    p.circle_stroke(center, r, Stroke::new(2.0, lerp_color(pal().accent.gamma_multiply(0.6), Color32::WHITE, on * 0.6)));
    p.text(center, Align2::CENTER_CENTER, I_POWER, FontId::new(24.0, icons()), lerp_color(pal().accent, pal().on_accent, on));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn segmented<T: PartialEq + Copy>(ui: &mut Ui, salt: &str, value: &mut T, items: &[(T, String)]) -> bool {
    let h = 30.0;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let p = ui.painter().clone();
    p.rect_filled(rect, CornerRadius::same(6), pal().deep);
    let pad = 4.0;
    let w = (rect.width() - pad * 2.0) / items.len() as f32;
    let mut changed = false;
    for (i, (v, label)) in items.iter().enumerate() {
        let r = Rect::from_min_size(pos2(rect.min.x + pad + w * i as f32, rect.min.y + pad), vec2(w, h - pad * 2.0)).shrink2(vec2(2.0, 0.0));
        let resp = ui.interact(r, ui.scope_id().with((salt, i)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
        let selected = *v == *value;
        let anim = ui.ctx().animate_bool_with_time(resp.id, selected, 0.15);
        if anim > 0.0 {
            p.rect_filled(r, CornerRadius::same(4), pal().accent.gamma_multiply(anim));
        } else if resp.hovered() {
            p.rect_filled(r, CornerRadius::same(4), pal().card_hi);
        }
        let font = FontId::new(12.0, if selected { bold() } else { FontFamily::Proportional });
        p.text(r.center(), Align2::CENTER_CENTER, label, font, lerp_color(pal().text, pal().on_accent, anim));
        if resp.clicked() && !selected {
            *value = *v;
            changed = true;
        }
    }
    changed
}

fn toggle_switch(ui: &mut Ui, on: &mut bool) -> egui::Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(34.0, 19.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.15);
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(6), lerp_color(pal().card_hi, pal().accent, t));
    p.rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, pal().line), StrokeKind::Inside);
    let x = egui::lerp((rect.left() + 9.5)..=(rect.right() - 9.5), t);
    p.circle_filled(pos2(x, rect.center().y), 6.5, lerp_color(pal().weak, pal().deep, t));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn chip(ui: &mut Ui, text: &str, selected: bool) -> egui::Response {
    let (fill, col) = if selected { (pal().accent.gamma_multiply(0.18), pal().accent) } else { (pal().deep, pal().weak) };
    ui.add(
        egui::Button::new(RichText::new(text).color(col).font(FontId::monospace(11.5)))
            .fill(fill)
            .stroke(Stroke::new(1.0, if selected { pal().accent.gamma_multiply(0.6) } else { pal().line }))
            .corner_radius(CornerRadius::same(4))
            .min_size(vec2(36.0, 22.0)),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn icon_button(ui: &mut Ui, icon: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(icon).font(FontId::new(13.0, icons())).color(pal().accent))
            .fill(pal().deep)
            .stroke(Stroke::new(1.0, pal().line))
            .corner_radius(CornerRadius::same(4))
            .min_size(vec2(28.0, 24.0)),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn accent_button(ui: &mut Ui, text: &str, busy: bool) -> egui::Response {
    let col = if busy { pal().accent2 } else { pal().accent };
    ui.add(
        egui::Button::new(RichText::new(text).color(pal().on_accent).font(FontId::new(11.5, bold())))
            .fill(col)
            .corner_radius(CornerRadius::same(4))
            .min_size(vec2(0.0, 24.0)),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Scaled stand-in for the target window: click or drag to place the spot.
fn position_pad(ui: &mut Ui, pos: &mut [f32; 2], size: Option<[f64; 2]>, picking: bool) {
    let aspect = size.map(|s| (s[1] / s[0].max(1.0)) as f32).unwrap_or(9.0 / 16.0);
    let w = ui.available_width();
    let h = (w * aspect).clamp(50.0, 90.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click_and_drag());
    if let Some(p) = resp.interact_pointer_pos() {
        if resp.clicked() || resp.dragged() {
            pos[0] = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            pos[1] = ((p.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
        }
    }
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(6), pal().deep);
    let grid = pal().line.gamma_multiply(0.45);
    for i in 1..8 {
        let x = rect.left() + rect.width() * i as f32 / 8.0;
        p.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0, grid));
    }
    for i in 1..5 {
        let y = rect.top() + rect.height() * i as f32 / 5.0;
        p.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], Stroke::new(1.0, grid));
    }
    let border = if picking { pal().accent2 } else if resp.hovered() { pal().accent.gamma_multiply(0.6) } else { pal().line };
    p.rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, border), StrokeKind::Inside);

    let dot = pos2(rect.left() + pos[0] * rect.width(), rect.top() + pos[1] * rect.height());
    let cross = pal().accent.gamma_multiply(0.35);
    p.line_segment([pos2(rect.left(), dot.y), pos2(rect.right(), dot.y)], Stroke::new(1.0, cross));
    p.line_segment([pos2(dot.x, rect.top()), pos2(dot.x, rect.bottom())], Stroke::new(1.0, cross));
    for i in 0..6 {
        p.circle_filled(dot, 7.0 + i as f32 * 2.0, pal().accent.gamma_multiply(0.06));
    }
    p.circle_filled(dot, 6.0, pal().accent);
    p.circle_stroke(dot, 6.0, Stroke::new(2.0, pal().deep));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n).collect::<String>()) }
}

fn setup_style(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let t = pal();
    let mut load = |name: &str, path: &Option<std::path::PathBuf>| -> bool {
        match path.as_ref().and_then(|p| std::fs::read(p).ok()) {
            Some(bytes) => {
                fonts.font_data.insert(name.into(), Arc::new(FontData::from_owned(bytes)));
                true
            }
            None => false,
        }
    };
    let regular = load("rice", &t.font_regular);
    let strong = load("rice-bold", &t.font_bold);

    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        if regular {
            fonts.families.entry(family).or_default().insert(0, "rice".into());
        }
    }
    let mut bold_family = Vec::new();
    if strong {
        bold_family.push("rice-bold".to_string());
    }
    bold_family.extend(fonts.families[&FontFamily::Proportional].iter().cloned());
    fonts.families.insert(bold(), bold_family);
    fonts.families.insert(icons(), fonts.families[&FontFamily::Proportional].clone());
    ctx.set_fonts(fonts);

    let mut v = egui::Visuals::dark();
    v.panel_fill = pal().bg;
    v.window_fill = pal().card;
    v.window_stroke = Stroke::new(1.0, pal().line);
    v.window_corner_radius = CornerRadius::same(6);
    v.menu_corner_radius = CornerRadius::same(6);
    v.extreme_bg_color = pal().deep;
    v.faint_bg_color = pal().card_hi;
    v.override_text_color = None;
    v.selection.bg_fill = pal().accent.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.5, pal().accent);
    v.slider_trailing_fill = true;
    v.hyperlink_color = pal().accent;
    for (w, fill) in [
        (&mut v.widgets.noninteractive, pal().card),
        (&mut v.widgets.inactive, pal().deep),
        (&mut v.widgets.hovered, pal().card_hi),
        (&mut v.widgets.active, pal().card_hi),
        (&mut v.widgets.open, pal().card_hi),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.corner_radius = CornerRadius::same(4);
        w.fg_stroke.color = pal().text;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, pal().line);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, pal().line);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, pal().accent.gamma_multiply(0.6));
    v.widgets.active.bg_stroke = Stroke::new(1.0, pal().accent);
    v.widgets.noninteractive.fg_stroke.color = pal().weak;

    ctx.options_mut(|o| o.theme_preference = egui::ThemePreference::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.all_styles_mut(|s| {
        s.spacing.button_padding = vec2(9.0, 3.0);
        s.spacing.interact_size.y = 22.0;
        s.text_styles.insert(egui::TextStyle::Body, FontId::new(12.5, FontFamily::Proportional));
        s.text_styles.insert(egui::TextStyle::Button, FontId::new(12.5, FontFamily::Proportional));
        s.text_styles.insert(egui::TextStyle::Monospace, FontId::monospace(12.5));
    });
}

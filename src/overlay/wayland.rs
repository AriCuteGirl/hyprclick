//! Linux: `hyprclick overlay`, a layer surface on the overlay layer, so it
//! shows over fullscreen games. It asks lagswitch's daemon for its status ten
//! times a second, the same `status` line the CLI prints.

use super::{Banner, Feed, Fonts, H, TICK, W, height, render};
use crate::config::{Config, OverlayPlace};
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
use std::time::Duration;

const SOCKET: &str = "/run/lagswitch/sock";

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
    if !Config::load().lag_overlay || !crate::lag::installed() {
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
        place: Config::load().lag_overlay_place,
        configured: false,
        shown: None,
        want: None,
    };
    let mut feed = Feed::new();
    loop {
        overlay.want = feed.next(ask_status());
        if feed.place != overlay.place {
            overlay.place = feed.place;
            // Opened again in the new place on the next sync.
            overlay.layer = None;
        }
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
    place: OverlayPlace,
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
        match self.place {
            // Zone 0 keeps it clear of bars at that edge.
            OverlayPlace::Top => {
                layer.set_anchor(Anchor::TOP);
                layer.set_margin(16, 0, 0, 0);
            }
            OverlayPlace::Bottom => {
                layer.set_anchor(Anchor::BOTTOM);
                layer.set_margin(0, 0, 140, 0);
            }
            // No anchors centers it; -1 centers it on the whole screen, bars or not.
            OverlayPlace::Middle => {
                layer.set_anchor(Anchor::empty());
                layer.set_exclusive_zone(-1);
            }
        }
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(W, self.height());
        // An empty input region lets every click through to the game.
        if let Ok(region) = Region::new(&self.compositor) {
            layer.wl_surface().set_input_region(Some(region.wl_region()));
        }
        layer.commit();
        self.layer = Some(layer);
        self.configured = false;
        self.shown = None;
    }

    fn height(&self) -> u32 {
        height(self.place)
    }

    fn draw(&mut self) {
        let height = self.height() as i32;
        let (Some(layer), Some(banner)) = (&self.layer, &self.want) else { return };
        let Ok((buffer, canvas)) = self.pool.create_buffer(W as i32, height, (W * 4) as i32, wl_shm::Format::Argb8888) else {
            return;
        };
        // The banner is in the top rows, so a shorter surface takes just those.
        let pixels = render(banner, &self.fonts);
        for (dst, px) in canvas.chunks_exact_mut(4).zip(pixels) {
            dst.copy_from_slice(&px.to_le_bytes());
        }
        layer.wl_surface().damage_buffer(0, 0, W as i32, height);
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


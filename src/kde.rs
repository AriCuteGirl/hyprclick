//! KDE Plasma (KWin) backend. KWin has no plugin API like Hyprland's, so
//! window info comes from tiny KWin scripts loaded over D-Bus that call back
//! into this process with JSON; notifications go through
//! org.freedesktop.Notifications. Setup (global shortcut, window rule,
//! autostart) is in setup.rs.

use crate::desktop::Client;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use zbus::blocking::Connection;

const REPLY_PATH: &str = "/hyprclick";
const REPLY_IFACE: &str = "org.hyprclick.Kwin";
/// Window list + cursor + active window, reused for this long (the UI asks
/// every frame while picking a window).
const SNAPSHOT_TTL: Duration = Duration::from_millis(100);
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(2);

type Pending = Arc<Mutex<HashMap<String, mpsc::Sender<String>>>>;

/// Where KWin scripts deliver their result: `Reply(token, json)`.
struct Replies {
    pending: Pending,
}

#[zbus::interface(name = "org.hyprclick.Kwin")]
impl Replies {
    fn reply(&self, token: String, json: String) {
        if let Some(tx) = self.pending.lock().unwrap().remove(&token) {
            let _ = tx.send(json);
        }
    }
}

struct Bus {
    conn: Connection,
    pending: Pending,
}

fn bus() -> Result<&'static Bus, String> {
    static BUS: OnceLock<Result<Bus, String>> = OnceLock::new();
    BUS.get_or_init(|| {
        let conn = Connection::session().map_err(|e| format!("D-Bus: {e}"))?;
        let pending = Pending::default();
        conn.object_server()
            .at(REPLY_PATH, Replies { pending: pending.clone() })
            .map_err(|e| format!("D-Bus: {e}"))?;
        Ok(Bus { conn, pending })
    })
    .as_ref()
    .map_err(Clone::clone)
}

pub fn call<B>(dest: &str, path: &str, iface: &str, method: &str, body: &B) -> Result<zbus::message::Message, String>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    bus()?.conn.call_method(Some(dest), path, Some(iface), method, body).map_err(|e| format!("{dest}.{method}: {e}"))
}

/// Run `body` as a one-off KWin script. It ends by calling `reply(value)`;
/// returns that value.
fn run_script(body: &str) -> Result<Value, String> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let b = bus()?;
    let token = format!("{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed));
    let name = format!("hyprclick-{token}");
    let me = b.conn.unique_name().ok_or("D-Bus: no unique name")?.to_string();
    let source = format!(
        "function reply(v) {{ callDBus({me:?}, {REPLY_PATH:?}, {REPLY_IFACE:?}, \"Reply\", {token:?}, JSON.stringify(v)); }}\n{body}\n"
    );
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let file = std::path::PathBuf::from(runtime).join(format!("{name}.js"));
    std::fs::write(&file, source).map_err(|e| e.to_string())?;

    let (tx, rx) = mpsc::channel();
    b.pending.lock().unwrap().insert(token.clone(), tx);
    let result = (|| {
        let id: i32 = call("org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting", "loadScript", &(file.display().to_string(), &name))?
            .body()
            .deserialize()
            .map_err(|e| e.to_string())?;
        if id < 0 {
            return Err("KWin refused the script".to_string());
        }
        // Plasma 6 path; Plasma 5 used "/<id>".
        call("org.kde.KWin", &format!("/Scripting/Script{id}"), "org.kde.kwin.Script", "run", &())
            .or_else(|_| call("org.kde.KWin", &format!("/{id}"), "org.kde.kwin.Script", "run", &()))?;
        let json = rx.recv_timeout(SCRIPT_TIMEOUT).map_err(|_| "KWin script didn't answer".to_string())?;
        serde_json::from_str(&json).map_err(|e| e.to_string())
    })();
    b.pending.lock().unwrap().remove(&token);
    let _ = call("org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting", "unloadScript", &(&name,));
    let _ = std::fs::remove_file(&file);
    result
}

const SNAPSHOT_JS: &str = r#"
const cur = workspace.currentDesktop;
const stack = workspace.stackingOrder; // bottom to top
const out = { cursor: [workspace.cursorPos.x, workspace.cursorPos.y], active: "", windows: [] };
if (workspace.activeWindow) out.active = workspace.activeWindow.internalId.toString();
for (let i = 0; i < stack.length; i++) {
    const w = stack[i];
    if (!(w.normalWindow || w.dialog) || w.deleted || !w.managed || w.resourceClass == "kwin_wayland") continue;
    const g = w.clientGeometry;
    const here = w.onAllDesktops || w.desktops.some(d => d.id == cur.id);
    out.windows.push({
        id: w.internalId.toString(),
        class: w.resourceClass,
        title: w.caption,
        at: [g.x, g.y],
        size: [g.width, g.height],
        visible: here && !w.minimized && !w.hidden,
        desktop: w.onAllDesktops ? "all desktops" : (w.desktops.length ? w.desktops[0].name : ""),
        above: stack.length - i,
    });
}
reply(out);
"#;

#[derive(Default, Clone)]
struct Snapshot {
    cursor: Option<(f64, f64)>,
    active: String,
    windows: Vec<Client>,
}

fn snapshot() -> Snapshot {
    static CACHE: Mutex<Option<(Instant, Snapshot)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap();
    if let Some((at, snap)) = cache.as_ref()
        && at.elapsed() < SNAPSHOT_TTL
    {
        return snap.clone();
    }
    // One failed call (KWin busy) mustn't look like every window closing:
    // keep the last good answer and retry on the next call.
    let Ok(v) = run_script(SNAPSHOT_JS) else {
        return cache.as_ref().map(|(_, snap)| snap.clone()).unwrap_or_default();
    };
    let pair = |v: &Value| [v[0].as_f64().unwrap_or(0.0), v[1].as_f64().unwrap_or(0.0)];
    let snap = Snapshot {
        cursor: Some((v["cursor"][0].as_f64().unwrap_or(0.0), v["cursor"][1].as_f64().unwrap_or(0.0))),
        active: v["active"].as_str().unwrap_or_default().into(),
        windows: v["windows"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|w| Client {
                address: w["id"].as_str().unwrap_or_default().into(),
                class: w["class"].as_str().unwrap_or_default().into(),
                title: w["title"].as_str().unwrap_or_default().into(),
                at: pair(&w["at"]),
                size: pair(&w["size"]),
                xwayland: None,
                workspace: w["desktop"].as_str().unwrap_or_default().into(),
                visible: w["visible"].as_bool().unwrap_or(false),
                recency: w["above"].as_i64().unwrap_or(i64::MAX),
            })
            .collect(),
    };
    *cache = Some((Instant::now(), snap.clone()));
    snap
}

pub fn clients() -> Vec<Client> {
    snapshot().windows
}

pub fn cursor_pos() -> Option<(f64, f64)> {
    snapshot().cursor
}

pub fn cursor_pos_now() -> Option<(f64, f64)> {
    let v = run_script("reply([workspace.cursorPos.x, workspace.cursorPos.y]);").ok()?;
    Some((v[0].as_f64()?, v[1].as_f64()?))
}

pub fn active_address() -> Option<String> {
    Some(snapshot().active).filter(|a| !a.is_empty())
}

/// Focus the first window of `class`.
pub fn activate_class(class: &str) {
    let _ = run_script(&format!(
        "for (const w of workspace.windowList()) {{ if (w.resourceClass == {class:?}) {{ workspace.activeWindow = w; break; }} }}\nreply(true);"
    ));
}

pub fn notify(text: &str) {
    // Replace our previous bubble instead of stacking ON/OFF ones.
    static LAST: AtomicU32 = AtomicU32::new(0);
    let hints: HashMap<&str, zbus::zvariant::Value> = HashMap::from([("transient", true.into())]);
    let body = (crate::APP_ID, LAST.load(Ordering::Relaxed), "input-mouse", text, "", Vec::<&str>::new(), hints, 1400i32);
    if let Ok(msg) = call("org.freedesktop.Notifications", "/org/freedesktop/Notifications", "org.freedesktop.Notifications", "Notify", &body)
        && let Ok(id) = msg.body().deserialize::<u32>()
    {
        LAST.store(id, Ordering::Relaxed);
    }
}

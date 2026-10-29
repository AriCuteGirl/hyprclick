//! Hyprland IPC (the same socket hyprctl uses) and the hyprclick plugin lifecycle.

use serde_json::Value;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Command;

const PLUGIN_SRC: &str = include_str!("../plugin/hyprclick.cpp");
const PLUGIN_PKGS: &str = "pixman-1 libdrm hyprland pangocairo libinput libudev wayland-server xkbcommon";

fn socket_dir() -> Option<PathBuf> {
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
    Some(PathBuf::from(runtime).join("hypr").join(sig))
}

pub fn request(cmd: &str) -> Result<String, String> {
    let path = socket_dir().ok_or("not running under Hyprland")?.join(".socket.sock");
    let mut sock = UnixStream::connect(&path).map_err(|e| format!("hyprland socket: {e}"))?;
    sock.write_all(cmd.as_bytes()).map_err(|e| e.to_string())?;
    let mut out = String::new();
    sock.read_to_string(&mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

fn request_json(cmd: &str) -> Option<Value> {
    serde_json::from_str(&request(&format!("j/{cmd}")).ok()?).ok()
}

/// Path of Hyprland's event socket (activewindowv2 etc.).
pub fn event_socket() -> Option<PathBuf> {
    Some(socket_dir()?.join(".socket2.sock"))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Client {
    pub address: String,
    pub class: String,
    pub title: String,
    pub at: [f64; 2],
    pub size: [f64; 2],
    pub xwayland: bool,
    pub workspace_id: i64,
    pub focus_history: i64,
}

impl Client {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.at[0] && y >= self.at[1] && x < self.at[0] + self.size[0] && y < self.at[1] + self.size[1]
    }
}

pub fn clients() -> Vec<Client> {
    let Some(Value::Array(list)) = request_json("clients") else {
        return Vec::new();
    };
    let pair = |v: &Value| [v[0].as_f64().unwrap_or(0.0), v[1].as_f64().unwrap_or(0.0)];
    list.iter()
        .filter(|c| c["mapped"].as_bool().unwrap_or(true))
        .map(|c| Client {
            address: c["address"].as_str().unwrap_or_default().into(),
            class: c["class"].as_str().unwrap_or_default().into(),
            title: c["title"].as_str().unwrap_or_default().into(),
            at: pair(&c["at"]),
            size: pair(&c["size"]),
            xwayland: c["xwayland"].as_bool().unwrap_or(false),
            workspace_id: c["workspace"]["id"].as_i64().unwrap_or(0),
            focus_history: c["focusHistoryID"].as_i64().unwrap_or(i64::MAX),
        })
        .collect()
}

pub fn cursor_pos() -> Option<(f64, f64)> {
    let v = request_json("cursorpos")?;
    Some((v["x"].as_f64()?, v["y"].as_f64()?))
}

pub fn active_address() -> Option<String> {
    request_json("activewindow")?["address"].as_str().map(String::from)
}

fn visible_workspaces() -> Vec<i64> {
    let Some(Value::Array(mons)) = request_json("monitors") else {
        return Vec::new();
    };
    mons.iter()
        .flat_map(|m| [m["activeWorkspace"]["id"].as_i64(), m["specialWorkspace"]["id"].as_i64()])
        .flatten()
        .filter(|id| *id != 0)
        .collect()
}

/// The visible window under a global point, topmost guess = most recently focused.
pub fn window_at(x: f64, y: f64) -> Option<Client> {
    let visible = visible_workspaces();
    clients()
        .into_iter()
        .filter(|c| c.class != crate::APP_ID && visible.contains(&c.workspace_id) && c.contains(x, y))
        .min_by_key(|c| c.focus_history)
}

pub fn find_target(class: &str, title: &str) -> Option<Client> {
    let mut matches: Vec<Client> = clients().into_iter().filter(|c| c.class == class).collect();
    matches.sort_by_key(|c| (title.is_empty() || c.title != title, c.focus_history));
    matches.into_iter().next()
}

pub fn notify(text: &str) {
    let _ = request(&format!("notify -1 1400 rgb(5ad1ff) fontsize:14 {text}"));
}

pub mod plugin {
    use super::*;

    /// (version, path) of the loaded plugin, if any.
    fn ping() -> Option<(String, String)> {
        let reply = request("hyprclick ping").ok()?;
        let mut parts = reply.trim().strip_prefix("hyprclick ok ")?.splitn(2, ' ');
        Some((parts.next()?.to_string(), parts.next().unwrap_or("").to_string()))
    }

    fn bundled_version() -> &'static str {
        PLUGIN_SRC
            .lines()
            .find_map(|l| l.trim().strip_prefix("constexpr int HYPRCLICK_VERSION = "))
            .map(|v| v.trim_end_matches(';'))
            .unwrap_or("0")
    }

    fn data_dir() -> PathBuf {
        dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("hyprclick")
    }

    /// Make sure the plugin is loaded into the running Hyprland, building it
    /// for this exact Hyprland commit first if needed (e.g. after an update).
    pub fn ensure() -> Result<(), String> {
        match ping() {
            Some((ver, _)) if ver == bundled_version() => return Ok(()),
            // Older build from a previous hyprclick version: swap it out.
            Some((_, path)) if !path.is_empty() => {
                request(&format!("plugin unload {path}"))?;
            }
            // v1 builds don't report their path: try every build we made.
            Some(_) => {
                for entry in std::fs::read_dir(data_dir()).into_iter().flatten().flatten() {
                    if entry.path().extension().is_some_and(|e| e == "so") {
                        let _ = request(&format!("plugin unload {}", entry.path().display()));
                    }
                }
            }
            None => {}
        }
        let version = request_json("version").ok_or("can't read Hyprland version")?;
        let commit = version["commit"].as_str().unwrap_or("unknown");
        let commit = &commit[..commit.len().min(12)];
        // The source hash is part of the name so an updated plugin never hits
        // dlopen's cache of an older build at the same path.
        let src_hash = PLUGIN_SRC.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
        let dir = data_dir();
        let so = dir.join(format!("hyprclick-{commit}-{:08x}.so", src_hash as u32));

        if !so.exists() {
            build(&dir, &so, version["tag"].as_str().unwrap_or(""))?;
        }

        let reply = request(&format!("plugin load {}", so.display()))?;
        if ping().is_some_and(|(ver, _)| ver == bundled_version()) {
            Ok(())
        } else {
            Err(format!("plugin load failed: {}", reply.trim()))
        }
    }

    fn build(dir: &PathBuf, so: &PathBuf, running_tag: &str) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let headers = Command::new("pkg-config").args(["--modversion", "hyprland"]).output().map_err(|e| e.to_string())?;
        let headers = String::from_utf8_lossy(&headers.stdout).trim().to_string();
        if headers.is_empty() {
            return Err("Hyprland headers missing, run: hyprpm update".into());
        }
        if !running_tag.trim_start_matches('v').starts_with(&headers) {
            return Err(format!("headers are {headers} but Hyprland is {running_tag}, run: hyprpm update"));
        }

        let src = dir.join("hyprclick.cpp");
        std::fs::write(&src, PLUGIN_SRC).map_err(|e| e.to_string())?;
        let cflags = Command::new("pkg-config").arg("--cflags").args(PLUGIN_PKGS.split(' ')).output().map_err(|e| e.to_string())?;
        let cflags = String::from_utf8_lossy(&cflags.stdout).to_string();
        let out = Command::new("g++")
            .args(["-shared", "-fPIC", "-std=c++2b", "-O2"])
            .arg(&src)
            .arg("-o")
            .arg(so)
            .args(cflags.split_whitespace())
            .output()
            .map_err(|e| format!("g++: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let first = err.lines().find(|l| l.contains("error")).unwrap_or("unknown error");
            return Err(format!("plugin build failed: {first}"));
        }
        Ok(())
    }
}

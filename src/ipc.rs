//! Single-instance control socket: `hyprclick toggle` (ALT+H) talks to
//! whichever hyprclick is running, GUI or headless daemon.

use crate::controller::Controller;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub fn path() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(runtime).join("hyprclick.sock")
}

pub fn send(cmd: &str) -> Option<String> {
    let mut sock = UnixStream::connect(path()).ok()?;
    let _ = sock.set_read_timeout(Some(Duration::from_secs(10)));
    sock.write_all(format!("{cmd}\n").as_bytes()).ok()?;
    let mut reply = String::new();
    BufReader::new(sock).read_line(&mut reply).ok()?;
    Some(reply.trim().to_string())
}

/// Serve control requests until the process exits. `role` is "gui" or
/// "daemon"; `on_change` runs after every handled request (repaint, exit check).
pub fn serve(ctrl: Arc<Mutex<Controller>>, role: &'static str, on_change: impl Fn(&str) + Send + 'static) -> std::io::Result<()> {
    let p = path();
    if p.exists() {
        if UnixStream::connect(&p).is_ok() {
            return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, "hyprclick already running"));
        }
        let _ = std::fs::remove_file(&p);
    }
    let listener = UnixListener::bind(&p)?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            let mut reader = BufReader::new(&stream);
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let cmd = line.trim();
            let reply = {
                let mut c = ctrl.lock().unwrap();
                match cmd {
                    "toggle" => {
                        if !c.running() {
                            // Pick up settings saved by the GUI since we started.
                            if role == "daemon" {
                                c.cfg = crate::config::Config::load();
                            }
                        }
                        c.toggle();
                        c.error.clone().map(|e| format!("error: {e}")).unwrap_or_else(|| c.status_line())
                    }
                    "start" => {
                        c.start();
                        c.error.clone().map(|e| format!("error: {e}")).unwrap_or_else(|| c.status_line())
                    }
                    "stop" => {
                        c.stop();
                        c.status_line()
                    }
                    "status" => {
                        c.poll();
                        c.status_line()
                    }
                    "role" => role.to_string(),
                    "quit" => {
                        c.stop();
                        "bye".into()
                    }
                    _ => "error: unknown command".into(),
                }
            };
            let _ = (&stream).write_all(format!("{reply}\n").as_bytes());
            on_change(cmd);
        }
    });
    Ok(())
}

pub fn cleanup() {
    let _ = std::fs::remove_file(path());
}

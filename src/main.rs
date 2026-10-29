mod config;
mod controller;
mod engine;
mod hypr;
mod ipc;
mod theme;
mod ui;

use config::Config;
use controller::Controller;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const APP_ID: &str = "hyprclick";

const USAGE: &str = "hyprclick — background autoclicker for Hyprland

usage:
  hyprclick            open the window
  hyprclick toggle     start/stop clicking (bind this, e.g. ALT+H)
  hyprclick start|stop|status
";

fn main() {
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        None | Some("gui") => gui(),
        Some("toggle") => toggle(),
        Some(cmd @ ("start" | "stop" | "status")) => match ipc::send(cmd) {
            Some(reply) => println!("{reply}"),
            None if cmd == "start" => spawn_daemon(),
            None => println!("not running"),
        },
        Some("daemon") => daemon(),
        _ => print!("{USAGE}"),
    }
}

fn toggle() {
    match ipc::send("toggle") {
        Some(reply) => println!("{reply}"),
        // Nothing running: start clicking headless with the saved settings.
        None => spawn_daemon(),
    }
}

fn spawn_daemon() {
    let exe = std::env::current_exe().expect("own path");
    let _ = std::process::Command::new(exe)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn daemon() {
    let ctrl = Arc::new(Mutex::new(Controller::new(Config::load())));
    let (tx, rx) = mpsc::channel::<()>();
    if ipc::serve(ctrl.clone(), "daemon", move |_| {
        let _ = tx.send(());
    })
    .is_err()
    {
        return;
    }
    ctrl.lock().unwrap().start();

    // Live while clicking; exit once toggled off, quit, or the target closed.
    loop {
        let _ = rx.recv_timeout(Duration::from_millis(500));
        let mut c = ctrl.lock().unwrap();
        c.poll();
        if !c.running() {
            break;
        }
    }
    ipc::cleanup();
}

fn gui() {
    match ipc::send("role").as_deref() {
        Some("gui") => {
            let _ = hypr::request(&format!("dispatch hl.dsp.focus({{window=\"class:^({APP_ID})$\"}})"));
            return;
        }
        // A headless daemon holds the socket: take over from it.
        Some(_) => {
            ipc::send("quit");
            for _ in 0..20 {
                if ipc::send("role").is_none() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        None => {}
    }
    ui::run();
    ipc::cleanup();
}

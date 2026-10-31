use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::time::Duration;

use lagswitch::proto::{FreezeRequest, Mode, Request, SOCKET, Target};
use lagswitch::rules::Direction;
use lagswitch::cgroup;
use lagswitch::config::Config;

const USAGE: &str = "usage:
  lagswitch list [FILTER]
  lagswitch freeze [--app NAME | --pid PID] [--ms N] [--dir in|out|both] [--cap-ms N]
  lagswitch toggle [--app NAME | --pid PID] [--dir in|out|both] [--cap-ms N]
  lagswitch hold start [--app NAME | --pid PID] [--dir in|out|both] [--cap-ms N]
  lagswitch hold stop
  lagswitch release
  lagswitch status
  lagswitch config
  lagswitch set [--app NAME] [--ms N] [--dir in|out|both] [--cap-ms N]

Without --app or --pid the last app is used; other defaults come from
~/.config/lagswitch/config.toml.";

fn send(request: &Request) -> Result<(), String> {
    let mut stream = UnixStream::connect(SOCKET).map_err(|e| match e.kind() {
        ErrorKind::NotFound | ErrorKind::ConnectionRefused => {
            "the daemon isn't running, start it with: sudo systemctl start lagswitchd".to_string()
        }
        ErrorKind::PermissionDenied => format!("no permission to use {SOCKET}"),
        _ => format!("can't reach the daemon: {e}"),
    })?;
    let timeout = Some(Duration::from_secs(3));
    let io_error = |e: std::io::Error| format!("can't talk to the daemon: {e}");
    stream.set_read_timeout(timeout).map_err(io_error)?;
    stream.set_write_timeout(timeout).map_err(io_error)?;
    writeln!(stream, "{}", request.encode()).map_err(io_error)?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply).map_err(io_error)?;
    match reply.trim_end().strip_prefix("ok") {
        Some(text) => {
            println!("{}", text.trim_start());
            Ok(())
        }
        None => Err(reply.trim_end().trim_start_matches("err ").to_string()),
    }
}

fn number(value: Option<String>, flag: &str) -> Result<u64, String> {
    let value = value.ok_or_else(|| format!("{flag} needs a value"))?;
    value.parse().map_err(|_| format!("{flag} needs a whole number, not \"{value}\""))
}

fn freeze_request(
    mode: Mode,
    mut args: impl Iterator<Item = String>,
    config: &Config,
) -> Result<Request, String> {
    let (mut app, mut pid) = (None, None);
    let mut ms = config.default_ms;
    let mut direction = config.direction;
    let mut cap_ms = config.max_cap_ms;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--app" => app = Some(args.next().ok_or("--app needs a name")?),
            "--pid" => pid = Some(number(args.next(), "--pid")? as u32),
            "--ms" if mode == Mode::Timed => ms = number(args.next(), "--ms")?,
            "--dir" => direction = Direction::parse(&args.next().ok_or("--dir needs a value")?)?,
            "--cap-ms" => cap_ms = number(args.next(), "--cap-ms")?,
            _ => return Err(format!("unknown option {flag}\n{USAGE}")),
        }
    }
    let target = match (app, pid) {
        (Some(name), None) => Target::App(name),
        (None, Some(pid)) => Target::Pid(pid),
        (None, None) => Target::App(config.last_app.clone().ok_or(
            "no app chosen yet, pick one with --app NAME (see lagswitch list)",
        )?),
        _ => return Err("give either --app or --pid".to_string()),
    };
    let ms = (mode == Mode::Timed).then_some(ms);
    Ok(Request::Freeze(FreezeRequest { mode, ms, direction, cap_ms, target }))
}

/// Send a freeze, then remember the app for next time.
fn freeze(mode: Mode, args: impl Iterator<Item = String>) -> Result<(), String> {
    let mut config = Config::load()?;
    let request = freeze_request(mode, args, &config)?;
    send(&request)?;
    if let Request::Freeze(FreezeRequest { target: Target::App(name), .. }) = request
        && config.last_app.as_ref() != Some(&name)
    {
        config.last_app = Some(name);
        if let Err(e) = config.save() {
            eprintln!("lagswitch: {e}");
        }
    }
    Ok(())
}

/// Tab-separated, one cgroup per row: name, pid, app ID, processes, status, cgroup.
fn list(filter: Option<String>) -> Result<(), String> {
    // SAFETY: geteuid has no preconditions.
    let mut entries = cgroup::entries(unsafe { libc::geteuid() })?;
    if let Some(text) = filter {
        entries = cgroup::filter(entries, &text);
    }
    for e in entries {
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            e.name,
            e.pid,
            e.app_id.as_deref().unwrap_or("-"),
            e.processes,
            if e.freezable { "ok" } else { "shared" },
            e.cgroup
        );
    }
    Ok(())
}

/// Change the saved defaults, which is what the keybinds read.
fn set(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let mut config = Config::load()?;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--app" => config.last_app = Some(args.next().ok_or("--app needs a name")?),
            "--ms" => config.default_ms = number(args.next(), "--ms")?,
            "--dir" => config.direction = Direction::parse(&args.next().ok_or("--dir needs a value")?)?,
            "--cap-ms" => config.max_cap_ms = number(args.next(), "--cap-ms")?,
            _ => return Err(format!("unknown option {flag}\n{USAGE}")),
        }
    }
    config.save()
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("list") => list(args.next()),
        Some("freeze") => freeze(Mode::Timed, args),
        Some("toggle") => freeze(Mode::Toggle, args),
        Some("hold") => match args.next().as_deref() {
            Some("start") => freeze(Mode::Hold, args),
            Some("stop") => send(&Request::HoldStop),
            _ => Err(USAGE.to_string()),
        },
        Some("release") => send(&Request::Release),
        Some("status") => send(&Request::Status),
        Some("config") => {
            print!("{}", Config::load()?.render());
            Ok(())
        }
        Some("set") => set(args),
        _ => Err(USAGE.to_string()),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lagswitch: {e}");
            ExitCode::FAILURE
        }
    }
}

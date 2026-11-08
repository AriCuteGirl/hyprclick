//! The command line, shared by the Linux CLI and hyprclick's built-in Windows
//! version. It only parses and formats: a Backend carries out each request.

use crate::config::Config;
use crate::proto::{FreezeRequest, Mode, Request, Target};
use crate::rules::Direction;
use crate::shape::{Shape, Style};

pub const USAGE: &str = "usage:
  lagswitch list [FILTER]
  lagswitch freeze [--whole-pc | --app NAME | --pid PID] [--ms N] [--dir in|out|both] [--cap-ms N]
  lagswitch toggle [--whole-pc | --app NAME | --pid PID] [--dir in|out|both] [--cap-ms N]
  lagswitch hold start [--whole-pc | --app NAME | --pid PID] [--dir in|out|both] [--cap-ms N]
  lagswitch hold stop
  lagswitch lag on|toggle [--app NAME | --pid PID] [--ms N] [--style steady|choke] [--jitter PERCENT]
  lagswitch lag off
  lagswitch release
  lagswitch status
  lagswitch config
  lagswitch set [--whole-pc true|false] [--app NAME] [--ms N] [--dir in|out|both] [--cap-ms N]
                [--lag-ms N] [--lag-style steady|choke] [--lag-jitter PERCENT]

Any command can start with --notify to also show errors as a desktop notification,
and whether the freeze or fake lag is now on or off. Keybinds use it.

--whole-pc blocks the network for the whole PC. With no target named, the
saved choice is used; defaults come from ~/.config/lagswitch/config.toml.
`lag` adds fake lag to one app until `lag off`. steady raises its ping by --ms;
choke holds its outgoing packets for up to --ms and sends them in bursts, so others
see it jump. --jitter randomizes each delay by up to that percentage.";


/// Where requests go: the root daemon on Linux, the engine itself on Windows.
pub trait Backend {
    /// The reply to a request, without the "ok".
    fn ask(&self, request: &Request) -> Result<String, String>;
    /// Tab-separated rows of what can be frozen: name, pid, app ID, processes, status, cgroup.
    fn list(&self, filter: Option<&str>) -> Result<String, String>;
}

/// What a command printed, and what to show as a notification with --notify.
pub struct Outcome {
    pub output: String,
    pub notice: Option<String>,
}

impl Outcome {
    fn quiet(output: String) -> Self {
        Self { output, notice: None }
    }
}

fn number(value: Option<String>, flag: &str) -> Result<u64, String> {
    let value = value.ok_or_else(|| format!("{flag} needs a value"))?;
    value.parse().map_err(|_| format!("{flag} needs a whole number, not \"{value}\""))
}

fn percent(value: Option<String>, flag: &str) -> Result<u8, String> {
    let n = number(value, flag)?;
    u8::try_from(n).ok().filter(|p| *p <= 100).ok_or_else(|| format!("{flag} must be 0 to 100"))
}

fn freeze_request(
    mode: Mode,
    mut args: impl Iterator<Item = String>,
    config: &Config,
) -> Result<Request, String> {
    let (mut app, mut pid) = (None, None);
    let mut whole_pc = false;
    let mut ms = config.default_ms;
    let mut direction = config.direction;
    let mut cap_ms = config.max_cap_ms;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--whole-pc" => whole_pc = true,
            "--app" => app = Some(args.next().ok_or("--app needs a name")?),
            "--pid" => pid = Some(number(args.next(), "--pid")? as u32),
            "--ms" if mode == Mode::Timed => ms = number(args.next(), "--ms")?,
            "--dir" => direction = Direction::parse(&args.next().ok_or("--dir needs a value")?)?,
            "--cap-ms" => cap_ms = number(args.next(), "--cap-ms")?,
            _ => return Err(format!("unknown option {flag}\n{USAGE}")),
        }
    }
    let target = match (app, pid) {
        (None, None) if whole_pc || config.whole_pc => Target::Network,
        _ if whole_pc => return Err("give only one of --whole-pc, --app or --pid".to_string()),
        (Some(name), None) => Target::App(name),
        (None, Some(pid)) => Target::Pid(pid),
        (None, None) => Target::App(config.last_app.clone().ok_or(
            "no app chosen yet, pick one with --app NAME (see lagswitch list) or use --whole-pc",
        )?),
        _ => return Err("give either --app or --pid".to_string()),
    };
    let ms = (mode == Mode::Timed).then_some(ms);
    Ok(Request::Freeze(FreezeRequest { mode, ms, direction, cap_ms, target }))
}

/// Remember the app a command named, for the keys and the next command.
fn remember(request: &Request) {
    let name = match request {
        Request::Freeze(FreezeRequest { target: Target::App(name), .. }) | Request::Lag { target: Target::App(name), .. } => name,
        _ => return,
    };
    let Ok(mut config) = Config::load() else { return };
    if config.last_app.as_ref() != Some(name) {
        config.last_app = Some(name.clone());
        if let Err(e) = config.save() {
            eprintln!("lagswitch: {e}");
        }
    }
}

/// A notification line for the freeze part of a status reply
/// ("frozen timed both 5000 network ..." or "idle ...").
fn freeze_summary(status: &str) -> String {
    let parts: Vec<&str> = status.split(' ').collect();
    let ["frozen", mode, direction, left, target, ..] = parts[..] else { return "Unfrozen".to_string() };
    let what = if target == "network" {
        "whole PC".to_string()
    } else {
        let direction = match direction {
            "in" => "incoming",
            "out" => "outgoing",
            _ => "both ways",
        };
        format!("one app, {direction}")
    };
    let seconds = left.parse::<f64>().map_or(0.0, |ms| ms / 1000.0);
    match mode {
        "timed" => format!("Frozen for {seconds:.1} s ({what})"),
        "hold" => format!("Frozen while held ({what})"),
        _ => format!("Frozen until you press again ({what})"),
    }
}

fn freeze(mode: Mode, args: impl Iterator<Item = String>, backend: &dyn Backend) -> Result<Outcome, String> {
    let config = Config::load()?;
    let request = freeze_request(mode, args, &config)?;
    let reply = backend.ask(&request)?;
    remember(&request);
    Ok(Outcome { notice: Some(freeze_summary(&reply)), output: reply })
}

fn lag_request(toggle: bool, mut args: impl Iterator<Item = String>, config: &Config) -> Result<Request, String> {
    let (mut app, mut pid) = (None, None);
    let mut shape = Shape { ms: config.lag_ms, style: config.lag_style, jitter: config.lag_jitter };
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--app" => app = Some(args.next().ok_or("--app needs a name")?),
            "--pid" => pid = Some(number(args.next(), "--pid")? as u32),
            "--ms" => shape.ms = number(args.next(), "--ms")?,
            "--style" => shape.style = Style::parse(&args.next().ok_or("--style needs steady or choke")?)?,
            "--jitter" => shape.jitter = percent(args.next(), "--jitter")?,
            _ => return Err(format!("unknown option {flag}\n{USAGE}")),
        }
    }
    let target = match (app, pid) {
        (Some(name), None) => Target::App(name),
        (None, Some(pid)) => Target::Pid(pid),
        (None, None) => Target::App(
            config.last_app.clone().ok_or("no app chosen yet, pick one with --app NAME (see lagswitch list)")?,
        ),
        _ => return Err("give either --app or --pid".to_string()),
    };
    shape.check()?;
    Ok(Request::Lag { toggle, shape, target })
}

/// A notification line for the fake lag part of a status reply.
fn lag_summary(status: &str) -> String {
    let mut parts = status.split(' ').skip_while(|p| *p != "lag");
    let Some(ms) = parts.nth(1) else { return "Fake lag off".to_string() };
    let field = |key: &str| status.split(' ').find_map(|p| p.strip_prefix(key)).unwrap_or("?");
    match field("style=") {
        "choke" => format!("Fake lag on: choke {ms} ms, jitter {}%", field("jitter=")),
        _ => format!("Fake lag on: +{ms} ms ping, jitter {}%", field("jitter=")),
    }
}

/// Fake lag on or off, remembering the app like a freeze does.
fn lag(mut args: impl Iterator<Item = String>, backend: &dyn Backend) -> Result<Outcome, String> {
    let request = match args.next().as_deref() {
        Some("on") => lag_request(false, args, &Config::load()?)?,
        Some("toggle") => lag_request(true, args, &Config::load()?)?,
        Some("off") => Request::LagOff,
        _ => return Err(USAGE.to_string()),
    };
    let reply = backend.ask(&request)?;
    remember(&request);
    Ok(Outcome { notice: Some(lag_summary(&reply)), output: reply })
}

/// Change the saved defaults, which is what the keybinds read.
fn set(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let mut config = Config::load()?;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--whole-pc" => {
                let value = args.next().ok_or("--whole-pc needs true or false")?;
                config.whole_pc = value.parse().map_err(|_| "--whole-pc needs true or false")?;
            }
            "--app" => config.last_app = Some(args.next().ok_or("--app needs a name")?),
            "--ms" => config.default_ms = number(args.next(), "--ms")?,
            "--dir" => config.direction = Direction::parse(&args.next().ok_or("--dir needs a value")?)?,
            "--cap-ms" => config.max_cap_ms = number(args.next(), "--cap-ms")?,
            "--lag-ms" => config.lag_ms = number(args.next(), "--lag-ms")?,
            "--lag-style" => config.lag_style = Style::parse(&args.next().ok_or("--lag-style needs steady or choke")?)?,
            "--lag-jitter" => config.lag_jitter = percent(args.next(), "--lag-jitter")?,
            _ => return Err(format!("unknown option {flag}\n{USAGE}")),
        }
    }
    config.save()
}

/// Releasing the hold key. The bind also fires when nothing is held (ALT
/// coming up, or after a timed freeze), so only a hold that ends is announced.
fn hold_stop(backend: &dyn Backend) -> Result<Outcome, String> {
    let holding = backend.ask(&Request::Status)?.starts_with("frozen hold ");
    let reply = backend.ask(&Request::HoldStop)?;
    Ok(Outcome { notice: holding.then(|| freeze_summary(&reply)), output: reply })
}

/// Run one command. The arguments don't include the program name, and a leading
/// --notify is the caller's to handle.
pub fn run(args: Vec<String>, backend: &dyn Backend) -> Result<Outcome, String> {
    let mut args = args.into_iter();
    match args.next().as_deref() {
        Some("list") => backend.list(args.next().as_deref()).map(Outcome::quiet),
        Some("freeze") => freeze(Mode::Timed, args, backend),
        Some("toggle") => freeze(Mode::Toggle, args, backend),
        Some("hold") => match args.next().as_deref() {
            Some("start") => freeze(Mode::Hold, args, backend),
            Some("stop") => hold_stop(backend),
            _ => Err(USAGE.to_string()),
        },
        Some("lag") => lag(args, backend),
        Some("release") => {
            let reply = backend.ask(&Request::Release)?;
            Ok(Outcome { notice: Some(freeze_summary(&reply)), output: reply })
        }
        Some("status") => backend.ask(&Request::Status).map(Outcome::quiet),
        Some("config") => Ok(Outcome::quiet(Config::load()?.render().trim_end().to_string())),
        Some("set") => set(args).map(|()| Outcome::quiet(String::new())),
        _ => Err(USAGE.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_freezes() {
        assert_eq!(freeze_summary("idle"), "Unfrozen");
        assert_eq!(freeze_summary("idle lag 200 a/b style=choke"), "Unfrozen");
        assert_eq!(freeze_summary("frozen timed both 5000 network"), "Frozen for 5.0 s (whole PC)");
        assert_eq!(freeze_summary("frozen toggle in 30000 a/b.scope"), "Frozen until you press again (one app, incoming)");
        assert_eq!(freeze_summary("frozen hold both 29990 a/b.scope lag 200 a/b"), "Frozen while held (one app, both ways)");
    }

    #[test]
    fn summarizes_fake_lag() {
        assert_eq!(lag_summary("idle"), "Fake lag off");
        assert_eq!(lag_summary("frozen toggle both 900 a/b"), "Fake lag off");
        assert_eq!(
            lag_summary("idle lag 200 a/b.scope style=choke jitter=50 out=0 in=0"),
            "Fake lag on: choke 200 ms, jitter 50%"
        );
        assert_eq!(
            lag_summary("idle lag 150 a/b.scope style=steady jitter=0 out=3 in=2"),
            "Fake lag on: +150 ms ping, jitter 0%"
        );
    }
}

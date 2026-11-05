//! The daemon's socket protocol: one request line, one reply line.
//!
//!   timed <ms> <in|out|both> <cap_ms> <target>
//!   toggle <in|out|both> <cap_ms> <target>
//!   hold-start <in|out|both> <cap_ms> <target>
//!   hold-stop
//!   release
//!   status
//!   lag-on <ms> <steady|choke> <jitter%> <target>
//!   lag-toggle <ms> <steady|choke> <jitter%> <target>
//!   lag-off
//!
//! A target is app:<name>, pid:<n> or network (the whole PC). Replies start with "ok" or "err".

use crate::delay::{Shape, Style};
use crate::rules::Direction;

pub const SOCKET: &str = "/run/lagswitch/sock";

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    App(String),
    Pid(u32),
    /// Block all network traffic on the PC.
    Network,
}

impl Target {
    fn parse(text: &str) -> Result<Self, String> {
        if text == "network" {
            return Ok(Self::Network);
        }
        if let Some(name) = text.strip_prefix("app:") {
            if name.is_empty() {
                return Err("empty app name".to_string());
            }
            return Ok(Self::App(name.to_string()));
        }
        if let Some(pid) = text.strip_prefix("pid:") {
            return pid.parse().map(Self::Pid).map_err(|_| format!("bad pid \"{pid}\""));
        }
        Err(format!("bad target \"{text}\""))
    }

    pub fn encode(&self) -> String {
        match self {
            Self::App(name) => format!("app:{name}"),
            Self::Pid(pid) => format!("pid:{pid}"),
            Self::Network => "network".to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Timed,
    Toggle,
    Hold,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Timed => "timed",
            Self::Toggle => "toggle",
            Self::Hold => "hold",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FreezeRequest {
    pub mode: Mode,
    /// None runs until the cap.
    pub ms: Option<u64>,
    pub direction: Direction,
    pub cap_ms: u64,
    pub target: Target,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    Freeze(FreezeRequest),
    HoldStop,
    Release,
    Status,
    /// Fake lag on one app.
    Lag { toggle: bool, shape: Shape, target: Target },
    LagOff,
}

fn number(text: Option<&str>, what: &str) -> Result<u64, String> {
    let text = text.ok_or_else(|| format!("missing {what}"))?;
    text.parse().map_err(|_| format!("bad {what} \"{text}\""))
}

impl Request {
    pub fn parse(line: &str) -> Result<Self, String> {
        let (verb, rest) = line.trim_end().split_once(' ').unwrap_or((line.trim_end(), ""));
        match verb {
            "hold-stop" => return Ok(Self::HoldStop),
            "release" => return Ok(Self::Release),
            "status" => return Ok(Self::Status),
            "lag-off" => return Ok(Self::LagOff),
            "lag-on" | "lag-toggle" => {
                let mut parts = rest.splitn(4, ' ');
                let ms = number(parts.next(), "ms")?;
                let style = Style::parse(parts.next().ok_or("missing lag style")?)?;
                let jitter = number(parts.next(), "jitter")?;
                let jitter = u8::try_from(jitter).map_err(|_| format!("bad jitter \"{jitter}\""))?;
                let target = Target::parse(parts.next().ok_or("missing lag target")?)?;
                return Ok(Self::Lag { toggle: verb == "lag-toggle", shape: Shape { ms, style, jitter }, target });
            }
            _ => {}
        }
        let mode = match verb {
            "timed" => Mode::Timed,
            "toggle" => Mode::Toggle,
            "hold-start" => Mode::Hold,
            _ => return Err(format!("unknown request \"{verb}\"")),
        };
        // App names can't hold spaces, but the target stays one field regardless.
        let fields = if mode == Mode::Timed { 4 } else { 3 };
        let mut parts = rest.splitn(fields, ' ');
        let ms = if mode == Mode::Timed { Some(number(parts.next(), "ms")?) } else { None };
        let direction = Direction::parse(parts.next().ok_or("missing direction")?)?;
        let cap_ms = number(parts.next(), "cap")?;
        let target = Target::parse(parts.next().ok_or("missing target")?)?;
        Ok(Self::Freeze(FreezeRequest { mode, ms, direction, cap_ms, target }))
    }

    pub fn encode(&self) -> String {
        match self {
            Self::HoldStop => "hold-stop".to_string(),
            Self::Release => "release".to_string(),
            Self::Status => "status".to_string(),
            Self::LagOff => "lag-off".to_string(),
            Self::Lag { toggle, shape, target } => format!(
                "{} {} {} {} {}",
                if *toggle { "lag-toggle" } else { "lag-on" },
                shape.ms,
                shape.style.name(),
                shape.jitter,
                target.encode()
            ),
            Self::Freeze(f) => {
                let verb = match f.mode {
                    Mode::Timed => "timed",
                    Mode::Toggle => "toggle",
                    Mode::Hold => "hold-start",
                };
                let ms = f.ms.map(|ms| format!("{ms} ")).unwrap_or_default();
                format!("{verb} {ms}{} {} {}", f.direction.name(), f.cap_ms, f.target.encode())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(request: Request) {
        assert_eq!(Request::parse(&request.encode()), Ok(request));
    }

    #[test]
    fn requests_survive_encoding() {
        round_trip(Request::HoldStop);
        round_trip(Request::Release);
        round_trip(Request::Status);
        round_trip(Request::LagOff);
        for (toggle, style) in [(false, Style::Steady), (true, Style::Choke)] {
            let shape = Shape { ms: 150, style, jitter: 30 };
            round_trip(Request::Lag { toggle, shape, target: Target::App("org.foo.Bar".to_string()) });
        }
        round_trip(Request::Freeze(FreezeRequest {
            mode: Mode::Timed,
            ms: Some(5000),
            direction: Direction::Both,
            cap_ms: 30_000,
            target: Target::App("org.foo.Bar".to_string()),
        }));
        round_trip(Request::Freeze(FreezeRequest {
            mode: Mode::Hold,
            ms: None,
            direction: Direction::Both,
            cap_ms: 1000,
            target: Target::Network,
        }));
        for mode in [Mode::Toggle, Mode::Hold] {
            round_trip(Request::Freeze(FreezeRequest {
                mode,
                ms: None,
                direction: Direction::In,
                cap_ms: 1000,
                target: Target::Pid(42),
            }));
        }
    }

    #[test]
    fn bad_requests_are_errors() {
        for line in ["", "nope", "timed", "timed x both 1 app:a", "timed 1 up 1 app:a",
                     "toggle both 1", "toggle both 1 app:", "toggle both 1 pid:x", "toggle both 1 who",
                     "lag-on", "lag-on 100", "lag-on x steady 0 app:a", "lag-toggle 100 steady 0 who",
                     "lag-on 100 wobbly 0 app:a", "lag-on 100 choke 300 app:a", "lag-on 100 app:a"] {
            assert!(Request::parse(line).is_err(), "{line}");
        }
    }
}

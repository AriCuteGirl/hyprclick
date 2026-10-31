//! The daemon's socket protocol: one request line, one reply line.
//!
//!   timed <ms> <in|out|both> <cap_ms> <target>
//!   toggle <in|out|both> <cap_ms> <target>
//!   hold-start <in|out|both> <cap_ms> <target>
//!   hold-stop
//!   release
//!   status
//!
//! A target is app:<name> or pid:<n>. Replies start with "ok" or "err".

use crate::rules::Direction;

pub const SOCKET: &str = "/run/lagswitch/sock";

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    App(String),
    Pid(u32),
}

impl Target {
    fn parse(text: &str) -> Result<Self, String> {
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
        round_trip(Request::Freeze(FreezeRequest {
            mode: Mode::Timed,
            ms: Some(5000),
            direction: Direction::Both,
            cap_ms: 30_000,
            target: Target::App("org.foo.Bar".to_string()),
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
                     "toggle both 1", "toggle both 1 app:", "toggle both 1 pid:x", "toggle both 1 who"] {
            assert!(Request::parse(line).is_err(), "{line}");
        }
    }
}

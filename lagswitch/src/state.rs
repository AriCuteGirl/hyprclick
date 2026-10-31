//! What each request does given the current freeze. No I/O, so it can be tested.

use std::time::{Duration, Instant};

use crate::proto::{FreezeRequest, Mode, Request};
use crate::rules::Direction;
use crate::timer;

#[derive(Clone, Debug, PartialEq)]
pub struct Active {
    pub mode: Mode,
    pub direction: Direction,
    pub cgroups: Vec<String>,
    pub deadline: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Decision {
    /// Apply rules for this long.
    Freeze(Duration),
    Release,
    Report,
}

pub fn decide(active: Option<&Active>, request: &Request) -> Decision {
    match request {
        Request::Status => Decision::Report,
        Request::Release => Decision::Release,
        // A timed freeze started under a held key keeps running out its timer.
        Request::HoldStop => match active {
            Some(a) if a.mode == Mode::Hold => Decision::Release,
            _ => Decision::Report,
        },
        Request::Freeze(f) => decide_freeze(active, f),
    }
}

fn decide_freeze(active: Option<&Active>, f: &FreezeRequest) -> Decision {
    let length = timer::capped(f.ms.unwrap_or(u64::MAX), f.cap_ms);
    match f.mode {
        // Pressing the key again during a freeze ends it.
        Mode::Timed | Mode::Toggle if active.is_some() => Decision::Release,
        _ => Decision::Freeze(length),
    }
}

/// Milliseconds until the deadline, rounded up so the wake-up is never early.
pub fn millis_until(deadline: Instant, now: Instant) -> u64 {
    let left = deadline.saturating_duration_since(now);
    left.as_millis().min(u128::from(u64::MAX)) as u64 + u64::from(!left.subsec_nanos().is_multiple_of(1_000_000))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::Target;

    fn request(mode: Mode, ms: Option<u64>) -> Request {
        Request::Freeze(FreezeRequest {
            mode,
            ms,
            direction: Direction::Both,
            cap_ms: 30_000,
            target: Target::App("a".to_string()),
        })
    }

    fn active(mode: Mode) -> Active {
        Active { mode, direction: Direction::Both, cgroups: vec![], deadline: Instant::now() }
    }

    #[test]
    fn timed_runs_for_its_duration() {
        let d = decide(None, &request(Mode::Timed, Some(5000)));
        assert_eq!(d, Decision::Freeze(Duration::from_secs(5)));
    }

    #[test]
    fn timed_is_capped() {
        let d = decide(None, &request(Mode::Timed, Some(90_000)));
        assert_eq!(d, Decision::Freeze(Duration::from_secs(30)));
    }

    #[test]
    fn toggle_and_hold_stop_at_the_cap() {
        for mode in [Mode::Toggle, Mode::Hold] {
            assert_eq!(decide(None, &request(mode, None)), Decision::Freeze(Duration::from_secs(30)));
        }
    }

    #[test]
    fn pressing_timed_or_toggle_again_releases() {
        let frozen = active(Mode::Timed);
        assert_eq!(decide(Some(&frozen), &request(Mode::Timed, Some(5000))), Decision::Release);
        assert_eq!(decide(Some(&frozen), &request(Mode::Toggle, None)), Decision::Release);
    }

    #[test]
    fn hold_start_over_a_freeze_restarts_it() {
        let frozen = active(Mode::Timed);
        let d = decide(Some(&frozen), &request(Mode::Hold, None));
        assert_eq!(d, Decision::Freeze(Duration::from_secs(30)));
    }

    #[test]
    fn hold_stop_only_ends_a_hold() {
        assert_eq!(decide(Some(&active(Mode::Hold)), &Request::HoldStop), Decision::Release);
        assert_eq!(decide(Some(&active(Mode::Toggle)), &Request::HoldStop), Decision::Report);
        assert_eq!(decide(None, &Request::HoldStop), Decision::Report);
    }

    #[test]
    fn release_always_releases() {
        assert_eq!(decide(None, &Request::Release), Decision::Release);
    }

    #[test]
    fn wake_up_is_never_early() {
        let now = Instant::now();
        assert_eq!(millis_until(now + Duration::from_millis(5), now), 5);
        assert_eq!(millis_until(now + Duration::from_micros(5001), now), 6);
        assert_eq!(millis_until(now, now + Duration::from_secs(1)), 0);
    }
}

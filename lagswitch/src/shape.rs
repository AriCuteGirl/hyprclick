//! How fake lag times packets, shared by the Linux daemon and the Windows
//! engine. Two styles:
//!
//! - Steady: half the delay on the way out and half on the way in, so the round
//!   trip a game measures grows by the full amount.
//! - Choke: outgoing packets are held for up to the limit and then sent in one
//!   burst, again and again, so others see you freeze and jump. Incoming
//!   packets aren't touched, so your own game stays smooth.
//!
//! Jitter shortens each hold, or the steady delay for a quarter second at a
//! time, by a random part of up to that percentage.

use std::time::{Duration, Instant};

/// The longest delay accepted.
pub const MAX_MS: u64 = 5000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Style {
    Steady,
    Choke,
}

impl Style {
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "steady" => Ok(Self::Steady),
            "choke" => Ok(Self::Choke),
            _ => Err(format!("style must be steady or choke, not \"{text}\"")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Steady => "steady",
            Self::Choke => "choke",
        }
    }
}

/// How fake lag treats packets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    /// Steady: added ping. Choke: the longest hold.
    pub ms: u64,
    pub style: Style,
    /// Percent, 0 to 100.
    pub jitter: u8,
}

impl Shape {
    pub fn check(&self) -> Result<(), String> {
        if self.ms == 0 || self.ms > MAX_MS {
            return Err(format!("fake lag must be 1 to {MAX_MS} ms"));
        }
        if self.jitter > 100 {
            return Err("jitter must be 0 to 100 percent".to_string());
        }
        Ok(())
    }

    /// Choke leaves incoming packets alone, so they needn't be queued.
    pub fn delays_inbound(&self) -> bool {
        self.style == Style::Steady
    }
}

/// When each packet may go. No I/O, so it can be tested.
pub struct Schedule {
    pub shape: Shape,
    rng: u64,
    /// The latest time given out per direction, so packets never overtake.
    last_out: Option<Instant>,
    last_in: Option<Instant>,
    /// Choke: when the current hold ends.
    pub choke_until: Option<Instant>,
    /// Steady: the share of the delay in use, and when to pick a new one.
    steady_share: f64,
    next_pick: Option<Instant>,
}

/// Steady jitter picks a new delay this often. Per packet it would barely show,
/// because a packet can't overtake the ones before it.
const PICK_EVERY: Duration = Duration::from_millis(250);

impl Schedule {
    pub fn new(shape: Shape, seed: u64) -> Self {
        Self {
            shape,
            rng: seed | 1,
            last_out: None,
            last_in: None,
            choke_until: None,
            steady_share: 1.0,
            next_pick: None,
        }
    }

    /// A random number in [0, 1), from xorshift64.
    fn random(&mut self) -> f64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 11) as f64 / (1u64 << 53) as f64
    }

    /// 1, less a random part of up to the jitter percentage.
    fn share(&mut self) -> f64 {
        1.0 - f64::from(self.shape.jitter) / 100.0 * self.random()
    }

    fn millis(ms: u64, share: f64) -> Duration {
        Duration::from_secs_f64(ms as f64 * share / 1000.0)
    }

    /// When a packet arriving now may go.
    pub fn due(&mut self, now: Instant, inbound: bool) -> Instant {
        let ms = self.shape.ms;
        let due = match self.shape.style {
            Style::Choke if inbound => now,
            Style::Choke => match self.choke_until {
                Some(until) if until > now => until,
                _ => {
                    let until = now + Self::millis(ms, self.share());
                    self.choke_until = Some(until);
                    until
                }
            },
            Style::Steady => {
                if self.next_pick.is_none_or(|at| now >= at) {
                    self.steady_share = self.share();
                    self.next_pick = Some(now + PICK_EVERY);
                }
                now + Self::millis(if inbound { ms - ms / 2 } else { ms / 2 }, self.steady_share)
            }
        };
        let last = if inbound { &mut self.last_in } else { &mut self.last_out };
        let due = last.map_or(due, |l| due.max(l));
        *last = Some(due);
        due
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn shape(ms: u64, style: Style, jitter: u8) -> Shape {
        Shape { ms, style, jitter }
    }

    #[test]
    fn steady_splits_the_delay_both_ways() {
        let now = Instant::now();
        let mut s = Schedule::new(shape(301, Style::Steady, 0), 1);
        assert_eq!(s.due(now, false) - now, Duration::from_millis(150));
        assert_eq!(s.due(now, true) - now, Duration::from_millis(151));
    }

    #[test]
    fn choke_holds_outgoing_then_sends_them_together() {
        let now = Instant::now();
        let mut s = Schedule::new(shape(400, Style::Choke, 0), 1);
        let first = s.due(now, false);
        assert_eq!(first - now, Duration::from_millis(400));
        // Everything sent during the hold goes out with the first packet.
        assert_eq!(s.due(now + Duration::from_millis(250), false), first);
        // After the burst, the next packet starts a new hold.
        let later = now + Duration::from_millis(410);
        assert_eq!(s.due(later, false) - later, Duration::from_millis(400));
        // Incoming packets pass straight away.
        assert_eq!(s.due(later, true), later);
    }

    #[test]
    fn jitter_only_shortens_and_never_reorders() {
        let now = Instant::now();
        let mut s = Schedule::new(shape(400, Style::Steady, 100), 7);
        let mut last = now;
        let mut seen = std::collections::BTreeSet::new();
        for i in 0..200 {
            let at = now + Duration::from_millis(i * 20);
            let due = s.due(at, false);
            assert!(due >= last, "packets kept in order");
            assert!(due - at <= Duration::from_millis(200));
            seen.insert((due - at).as_millis() / 20);
            last = due;
        }
        assert!(seen.len() > 3, "delays vary: {seen:?}");
        assert!(shape(400, Style::Choke, 101).check().is_err());
        assert!(shape(0, Style::Choke, 0).check().is_err());
        assert!(!shape(400, Style::Choke, 0).delays_inbound());
    }
}

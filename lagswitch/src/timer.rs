//! The failsafe: no freeze ever runs longer than the cap.

use std::time::Duration;

pub const DEFAULT_CAP_MS: u64 = 30_000;
/// The daemon's own limit, whatever cap a client asks for.
pub const HARD_MAX_MS: u64 = 300_000;

pub fn capped(requested_ms: u64, cap_ms: u64) -> Duration {
    Duration::from_millis(requested_ms.min(cap_ms).min(HARD_MAX_MS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_limits_long_freezes() {
        assert_eq!(capped(1500, DEFAULT_CAP_MS), Duration::from_millis(1500));
        assert_eq!(capped(30_000, DEFAULT_CAP_MS), Duration::from_secs(30));
        assert_eq!(capped(u64::MAX, DEFAULT_CAP_MS), Duration::from_secs(30));
        assert_eq!(capped(5000, 1000), Duration::from_secs(1));
    }

    #[test]
    fn client_cap_cannot_pass_the_hard_limit() {
        assert_eq!(capped(u64::MAX, u64::MAX), Duration::from_millis(HARD_MAX_MS));
    }
}

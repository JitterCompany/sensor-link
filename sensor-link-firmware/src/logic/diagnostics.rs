//! Diagnostic mode: a bounded period in which the device stays online.
//!
//! Started by [`Cmd::DiagnosticsOn`] with a timeout and ended by
//! [`Cmd::DiagnosticsOff`] or by that timeout expiring, whichever comes first.
//! While it lasts, [`network_task`] keeps an open connection open: it defers a
//! disconnect requested by the application and does not close the connection
//! when it goes idle. With the `mqtt-log` feature the device's own log records
//! are published during this time, straight from memory. They are never
//! persisted, which is why the device has to stay online for them.
//!
//! The mode is global rather than owned by the client or the network task: the
//! client starts it, the network task and the logger act on it, and the network
//! task is restarted for every connection.
//!
//! [`Cmd::DiagnosticsOn`]: sensor_link_protocol::cmd::Cmd::DiagnosticsOn
//! [`Cmd::DiagnosticsOff`]: sensor_link_protocol::cmd::Cmd::DiagnosticsOff
//! [`network_task`]: crate::logic::network::network_task

use core::sync::atomic::{AtomicU32, Ordering};

use sensor_link_protocol::MAX_DIAGNOSTICS_TIMEOUT_S;

use crate::monotonic_time;

/// End of diagnostic mode, in monotonic seconds since boot, or [`Self::OFF`].
///
/// Seconds in a `u32` rather than the monotonic microseconds: not every target
/// has 64-bit atomics, and a `u32` of seconds lasts over a century.
struct Deadline(AtomicU32);

impl Deadline {
    /// No diagnostic mode. A real deadline is never 0, as it lies at least a
    /// second after boot.
    const OFF: u32 = 0;

    const fn new() -> Self {
        Self(AtomicU32::new(Self::OFF))
    }

    /// Start (or restart) diagnostic mode at `now_s` for `timeout_s`, capped to
    /// [`MAX_DIAGNOSTICS_TIMEOUT_S`]. Returns the timeout applied.
    fn start_at(&self, now_s: u32, timeout_s: u32) -> u32 {
        let timeout_s = timeout_s.clamp(1, MAX_DIAGNOSTICS_TIMEOUT_S);
        self.0
            .store(now_s.saturating_add(timeout_s), Ordering::Relaxed);
        timeout_s
    }

    fn stop(&self) {
        self.0.store(Self::OFF, Ordering::Relaxed);
    }

    fn is_active(&self) -> bool {
        self.0.load(Ordering::Relaxed) != Self::OFF
    }

    /// Seconds of diagnostic mode left at `now_s`, if any.
    ///
    /// Ends diagnostic mode once its deadline has passed.
    fn remaining_at(&self, now_s: u32) -> Option<u32> {
        let deadline = self.0.load(Ordering::Relaxed);
        if deadline == Self::OFF {
            return None;
        }
        if now_s < deadline {
            return Some(deadline - now_s);
        }
        // Only clear the deadline just read: a restart since then stands.
        self.0
            .compare_exchange(deadline, Self::OFF, Ordering::Relaxed, Ordering::Relaxed)
            .ok();
        None
    }
}

static DEADLINE: Deadline = Deadline::new();

fn now_s() -> u32 {
    (monotonic_time::now().micros_since_init() / 1_000_000) as u32
}

/// Start diagnostic mode for `timeout_s` seconds, or restart it if it is
/// already active.
///
/// The timeout is capped to [`MAX_DIAGNOSTICS_TIMEOUT_S`]; returns the timeout
/// applied.
pub fn start(timeout_s: u32) -> u32 {
    DEADLINE.start_at(now_s(), timeout_s)
}

/// End diagnostic mode now.
pub fn stop() {
    DEADLINE.stop()
}

/// Whether diagnostic mode was started and not yet ended.
///
/// Does not read the clock, so it is safe to call from any context (the logger
/// calls it for every record). The price is that an expired timeout only counts
/// once [`remaining_s`] has seen it, which the network task does while
/// connected. Until then this stays `true`.
pub fn is_active() -> bool {
    DEADLINE.is_active()
}

/// Seconds of diagnostic mode left, or `None` if it is not active.
///
/// Ends diagnostic mode once its timeout has expired.
pub fn remaining_s() -> Option<u32> {
    DEADLINE.remaining_at(now_s())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inactive_by_default() {
        let deadline = Deadline::new();

        assert!(!deadline.is_active());
        assert_eq!(deadline.remaining_at(0), None);
    }

    #[test]
    fn test_expires_after_timeout() {
        let deadline = Deadline::new();

        assert_eq!(deadline.start_at(100, 60), 60);
        assert!(deadline.is_active());
        assert_eq!(deadline.remaining_at(100), Some(60));
        assert_eq!(deadline.remaining_at(159), Some(1));

        assert_eq!(deadline.remaining_at(160), None);
        assert!(!deadline.is_active(), "expiry must end diagnostic mode");
    }

    #[test]
    fn test_stop_ends_early() {
        let deadline = Deadline::new();
        deadline.start_at(100, 60);

        deadline.stop();

        assert!(!deadline.is_active());
        assert_eq!(deadline.remaining_at(101), None);
    }

    #[test]
    fn test_restart_extends() {
        let deadline = Deadline::new();
        deadline.start_at(100, 60);

        deadline.start_at(150, 60);

        assert_eq!(deadline.remaining_at(170), Some(40));
    }

    /// A timeout of 0 would store the "off" value at boot; a huge one would
    /// keep the device online for good.
    #[test]
    fn test_timeout_is_bounded() {
        let deadline = Deadline::new();

        assert_eq!(deadline.start_at(0, 0), 1);
        assert!(deadline.is_active());

        assert_eq!(deadline.start_at(0, u32::MAX), MAX_DIAGNOSTICS_TIMEOUT_S);
        assert_eq!(deadline.remaining_at(0), Some(MAX_DIAGNOSTICS_TIMEOUT_S));
    }
}

//! Rate limiting for outgoing e-mail.
//!
//! Mail providers typically enforce several send limits at once (per minute, per hour, per day).
//! [`RateLimiter`] tracks the sends that already happened and answers the only question the mail
//! task needs: *when* may the next batch of sends happen without exceeding any of those limits.

use std::{collections::VecDeque, time::Duration};

use tokio::time::Instant;

const MINUTE: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(60 * 60);
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// Maximum number of e-mails that may be sent per time window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThrottleConfig {
    pub per_minute: u32,
    pub per_hour: u32,
    pub per_day: u32,
}

impl Default for ThrottleConfig {
    /// Conservative defaults that stay well below what mail providers commonly allow, leaving
    /// headroom for urgent mail that is sent immediately.
    fn default() -> Self {
        ThrottleConfig {
            per_minute: 20,
            per_hour: 200,
            per_day: 800,
        }
    }
}

/// Sliding window rate limiter.
///
/// One entry is recorded per e-mail *recipient*, since that is what costs a separate transaction at
/// the mail server.
pub struct RateLimiter {
    config: ThrottleConfig,
    /// Timestamps of the sends in the last day, oldest first.
    sends: VecDeque<Instant>,
}

impl ThrottleConfig {
    /// How long the queue has to stay empty before a bulk of non-urgent email is considered done.
    ///
    /// Emails are produced one by one (a report is generated before its email is queued), so the
    /// queue regularly runs empty in the middle of a bulk. The idle time therefore has to cover the
    /// longest gap the limiter itself can put between two sends: once a window is full, the next
    /// email waits until the oldest send leaves that window. With these limits that is the hour
    /// window, unless the per-minute limit is the stricter one and the hour window never fills up.
    ///
    /// The day window is deliberately not taken into account: a bulk that runs into the daily limit
    /// spans more than a day, and is reported as one batch per day instead of one batch of days.
    pub fn batch_idle_time(&self) -> Duration {
        let longest_gap = if u64::from(self.per_hour) < u64::from(self.per_minute) * 60 {
            HOUR
        } else {
            MINUTE
        };
        // Plus the resolution at which emails are released once that window has room again.
        longest_gap + MINUTE
    }
}

impl RateLimiter {
    pub fn new(config: ThrottleConfig) -> Self {
        RateLimiter {
            config,
            sends: VecDeque::new(),
        }
    }

    /// Account for `count` sends that happened at `now`.
    pub fn record(&mut self, now: Instant, count: usize) {
        self.prune(now);
        for _ in 0..count {
            self.sends.push_back(now);
        }
    }

    /// The earliest instant at which `count` more sends fit within every window.
    ///
    /// Returns `None` when they fit right now.
    pub fn next_allowed(&mut self, now: Instant, count: usize) -> Option<Instant> {
        self.prune(now);

        [
            (MINUTE, self.config.per_minute),
            (HOUR, self.config.per_hour),
            (DAY, self.config.per_day),
        ]
        .into_iter()
        .filter_map(|(window, limit)| self.window_release(now, window, limit, count))
        .max()
    }

    /// Drop sends that have fallen out of the longest window.
    fn prune(&mut self, now: Instant) {
        let Some(cutoff) = now.checked_sub(DAY) else {
            return;
        };
        while self.sends.front().is_some_and(|ts| *ts <= cutoff) {
            self.sends.pop_front();
        }
    }

    /// The instant at which `count` more sends fit in a single window, or `None` if they fit now.
    fn window_release(
        &self,
        now: Instant,
        window: Duration,
        limit: u32,
        count: usize,
    ) -> Option<Instant> {
        let limit = limit as usize;

        // `sends` is sorted, so this is the index of the first send still inside the window.
        let first_in_window = match now.checked_sub(window) {
            Some(cutoff) => self.sends.partition_point(|ts| *ts <= cutoff),
            None => 0,
        };
        let in_window = self.sends.len() - first_in_window;

        if in_window + count <= limit {
            return None;
        }

        // How many of the sends in the window have to expire before `count` more fit. A single
        // e-mail with more recipients than the limit can never fit, so wait for the window to clear
        // completely rather than block forever.
        let must_expire = (in_window + count - limit).min(in_window);
        let last_to_expire = self.sends[first_in_window + must_expire - 1];

        Some(last_to_expire + window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter(per_minute: u32, per_hour: u32, per_day: u32) -> RateLimiter {
        RateLimiter::new(ThrottleConfig {
            per_minute,
            per_hour,
            per_day,
        })
    }

    #[test]
    fn sends_are_allowed_until_the_window_is_full() {
        let mut limiter = limiter(2, 100, 100);
        let start = Instant::now();

        assert_eq!(limiter.next_allowed(start, 1), None);
        limiter.record(start, 1);
        assert_eq!(limiter.next_allowed(start, 1), None);
        limiter.record(start, 1);

        // The window is full: the next send has to wait for the first one to expire.
        assert_eq!(limiter.next_allowed(start, 1), Some(start + MINUTE));
        assert_eq!(limiter.next_allowed(start + MINUTE, 1), None);
    }

    #[test]
    fn sends_expire_one_by_one() {
        let mut limiter = limiter(2, 100, 100);
        let start = Instant::now();

        limiter.record(start, 1);
        limiter.record(start + Duration::from_secs(10), 1);

        // Room for one more mail once the first send leaves the window, ...
        assert_eq!(limiter.next_allowed(start, 1), Some(start + MINUTE));
        // ... but two more mails need both sends to have expired.
        assert_eq!(
            limiter.next_allowed(start, 2),
            Some(start + Duration::from_secs(10) + MINUTE)
        );
    }

    #[test]
    fn the_most_restrictive_window_wins() {
        let mut limiter = limiter(10, 3, 100);
        let start = Instant::now();

        limiter.record(start, 3);

        // The per-minute window has room, the per-hour window does not.
        assert_eq!(limiter.next_allowed(start, 1), Some(start + HOUR));
    }

    #[test]
    fn a_mail_larger_than_the_limit_waits_for_an_empty_window() {
        let mut limiter = limiter(2, 100, 100);
        let start = Instant::now();

        limiter.record(start, 1);
        limiter.record(start + Duration::from_secs(10), 1);

        // 5 recipients never fit in a window of 2, so wait until the window is empty and send
        // anyway instead of blocking forever.
        assert_eq!(
            limiter.next_allowed(start, 5),
            Some(start + Duration::from_secs(10) + MINUTE)
        );
    }

    #[test]
    fn batch_idle_time_covers_the_gap_the_limiter_can_cause() {
        // The hourly limit is reached before the per-minute limit is, so the limiter can hold an
        // email back until the hour window has room again.
        let hourly = ThrottleConfig {
            per_minute: 20,
            per_hour: 200,
            per_day: 800,
        };
        assert!(hourly.batch_idle_time() > HOUR);

        // Here 20 per minute is the stricter limit (the hour window never fills up), so emails are
        // never held back for more than a minute.
        let per_minute = ThrottleConfig {
            per_hour: 20 * 60,
            ..hourly
        };
        assert!(per_minute.batch_idle_time() > MINUTE);
        assert!(per_minute.batch_idle_time() < HOUR);
    }

    #[test]
    fn sends_outside_the_day_window_are_pruned() {
        let mut limiter = limiter(2, 3, 4);
        let start = Instant::now();

        limiter.record(start, 4);
        // The day limit is reached, so nothing may be sent until a day has passed.
        assert_eq!(limiter.next_allowed(start, 1), Some(start + DAY));

        let next_day = start + DAY;
        assert_eq!(limiter.next_allowed(next_day, 1), None);
        assert_eq!(limiter.sends.len(), 0);
    }
}

//! Wall-clock time: the RTC combined with the microsecond monotonic.

use demo_firmware_core::{
    drivers::time::{self, CombinedTime, TimestampSource},
    monotonic_time::{self, now},
    sync::OnceLock,
};
use hardware::RTC;

use hardware::cortex_m::interrupt::free;

// 1us timestamp wrapper around RTIC Mono
pub struct FastClock {
    t_0: monotonic_time::MonotonicInstant,
}
impl TimestampSource for FastClock {
    fn timestamp_us(&self) -> Result<i64, time::Error> {
        Ok(now().micros_since(&self.t_0) as i64)
    }
}
impl FastClock {
    pub fn new() -> Self {
        FastClock { t_0: now() }
    }
}

// Max deviation between Mono and RTC. CombinedTime tries to keep the clocks within this range.
// If the deviation is exceeded the time will jump to the RTC time.
pub const MAX_DEVIATION_US: u32 = 100_000;

/// Initializes `CombinedTime` with RTC and microsecond timer.
pub fn init(rtc: RTC) -> Result<(), ()> {
    static INSTANCE: OnceLock<CombinedTime<RTC, FastClock>> = OnceLock::new();
    free(|_cs| {
        time::init_timer_with(|| {
            let time = CombinedTime::new(rtc)
                .with_high_resolution_clock(FastClock::new(), MAX_DEVIATION_US);

            // Unwrap won't panic because
            // - this runs from a critical section
            // - no external references to INSTANCE can exist (it is scoped to this function)
            // - this only runs once (guarantee provided by init_timer_with)
            INSTANCE.get_or_try_init(|| time).unwrap()
        })
    })
}

use crate::{
    drivers::time::{self, AdjustableTimestampSource, TimestampSource},
    monotonic_time,
};

// 1us timestamp wrapper around RTIC Mono
pub struct FastClock {
    t_0: monotonic_time::MonotonicInstant,
}
impl TimestampSource for FastClock {
    fn timestamp_us(&self) -> Result<i64, time::Error> {
        Ok(monotonic_time::now().micros_since(&self.t_0) as i64)
    }
}
impl FastClock {
    pub fn new() -> Self {
        FastClock {
            t_0: monotonic_time::now(),
        }
    }
}

impl AdjustableTimestampSource for FastClock {
    /// Dummy implementation
    fn adjust_us(&self, _target_time_us: i64, _offset_limit: u32) -> Result<i64, time::Error> {
        Ok(0)
    }
}

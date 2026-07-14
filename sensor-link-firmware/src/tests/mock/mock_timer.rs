use crate::drivers::time::{AdjustableTimestampSource, Error, TimestampSource};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct State {
    micros: i64,
    error: Option<Error>,
}

#[derive(Clone)]
pub struct MockTimer {
    state: Arc<Mutex<State>>,
}

impl MockTimer {
    pub fn new() -> Self {
        let state: Arc<Mutex<State>> = Arc::new(Mutex::new(State {
            // Dummy date: 15 march 2023 17:27:14.567890
            micros: 1_678_901_234__567_890,
            error: None,
        }));
        MockTimer { state }
    }

    /// Set a specified error
    ///
    /// The MockTimer will keep returning this error untill unset via clear_error()
    #[allow(unused)]
    pub fn set_error(&mut self, error: Error) {
        self.state.lock().unwrap().error = Some(error);
    }

    /// Clear error (if any was set)
    ///
    /// The MockTimer is forced to keep returning Ok() untill an error is set via set_error()
    #[allow(unused)]
    pub fn clear_error(&mut self) {
        self.state.lock().unwrap().error = None;
    }

    /// Reset the clock time to a specific timestamp_micros
    ///
    /// Note: this does not clear any errors. Use `clear_error()` for that.
    #[allow(unused)]
    pub fn set_time_micros(&mut self, micros: i64) {
        self.state.lock().unwrap().micros = micros;
    }

    /// Increment the clock time by a given amount of time
    pub fn inc_time_micros(&mut self, dt_us: i64) {
        let mut state = self.state.lock().unwrap();
        state.micros = state.micros.wrapping_add(dt_us);
    }
}

impl TimestampSource for MockTimer {
    fn timestamp_us(&self) -> Result<i64, Error> {
        let state = self.state.lock().unwrap();
        if let Some(err) = state.error {
            Err(err)
        } else {
            Ok(state.micros)
        }
    }
}

impl AdjustableTimestampSource for MockTimer {
    fn adjust_us(&self, target_time_us: i64, _offset_limit: u32) -> Result<i64, Error> {
        let before = self.timestamp_us();
        self.state.lock().unwrap().micros = target_time_us;

        let after = self.timestamp_us();
        match (before, after) {
            (Ok(t0), Ok(t1)) => Ok(t1 - t0),
            (Err(_), Ok(now)) => Ok(now),
            (_, Err(err)) => Err(err),
        }
    }
}

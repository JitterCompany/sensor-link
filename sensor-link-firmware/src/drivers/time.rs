//! Wall clock time API
//!
//! Suitable for generating timestamps or checking the current date / time-of-day.
//! This is *not* guaranteed to be monotonic. Time may jump backwards if adjusted.

use core::num::TryFromIntError;

use crate::{
    logic::time_adjust::{self, DriftError, ErrorEstimate, TempErrorEstimate},
    sync::{DoubleBuffer, OnceLock},
};

static DUMMY: DummySource = DummySource { _dummy: () };
static TIME_INSTANCE: OnceLock<&dyn AdjustableTimestampSource> = OnceLock::new();

/// Initialize the global timer directly
///
/// Note: this function must only be run once. All successive calls will return Error.
/// See also `init_timer_with()`
pub fn init_timer(timer: &'static dyn AdjustableTimestampSource) -> Result<(), ()> {
    init_timer_with(|| timer)
}

/// Initialize the global timer by running a provided function
///
/// This only succeeds the first time: the argument `make_timer` is executed and `Ok` is returned.
/// Any successive calls return an error without running the provided function.
pub fn init_timer_with<F>(make_timer: F) -> Result<(), ()>
where
    F: FnOnce() -> &'static dyn AdjustableTimestampSource,
{
    TIME_INSTANCE.get_or_try_init(make_timer)?;
    Ok(())
}

/// Unix timestamp in _microseconds_ since unix epoch (1970-01-01)
///
/// Note: initialize the global timer via `init_timer()` or `init_timer_with()`.
/// Calling this function before initalizing returns `Error::TimeUnknown`
/// After initializing, this function simply wraps the provided timer.
pub fn timestamp_us() -> Result<i64, Error> {
    instance().timestamp_us()
}

/// Infallible variant of [timestamp_us()]: _microseconds_ since unix epoch (1970-01-01).
/// If time is unknown or invalid this defaults to 0 as a 'special value'.
///
/// _This is not a monotonic timer_. Time may jump backwards during certain
/// events such as time adjustments.
pub fn timestamp_or_default_us() -> i64 {
    timestamp_us().unwrap_or(0)
}

/// Adjust the time to the given target time
///
/// If the clock error is below `offset_limit` us, the time is adjusted
/// gradually, and `None` is returned.
///
/// If the clock error is above the limit, the clock is immediately adjusted
/// and the applied change is returned in microseconds (e.g. -1_000_000 means one second
/// was subtracted from the clock)
pub fn adjust_us(target_time_us: i64, offset_limit: u32) -> Result<i64, Error> {
    instance().adjust_us(target_time_us, offset_limit)
}

/// Calibrate the clocksource by correcting it by the specified amount
pub fn calibrate(correction: &time_adjust::TotalDriftError) -> Result<(), CalError> {
    instance().calibrate(correction)
}

/// Clock accuracy based on typical tolerances depending on board revision
///
/// NB: This estimate does not include temperature effects.
/// See [temp_correction] to estimate the additional
/// tolerances due to temperature effects.
pub fn initial_accuracy() -> DriftError {
    instance().initial_accuracy()
}

/// Estimated temperature-dependent error
///
/// Can be used to improve calibration. Note that the result is *relative*
/// and independent of the absolute (board/tolerance specific) error.
pub fn temp_correction(temp_celsius: f32) -> TempErrorEstimate {
    instance().temp_correction(temp_celsius)
}

fn instance() -> &'static dyn AdjustableTimestampSource {
    if let Some(instance) = TIME_INSTANCE.get() {
        *instance
    } else {
        &DUMMY
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Error {
    /// Time has not been configured yet
    TimeUnknown,

    /// Time is misconfigured / timer is in invalid state that can't represent a valid time
    TimeInvalid,

    /// Time is not advancing (clock not running)
    TimeNotTicking,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CalError {
    /// A previous calibration is already busy (try again later)
    Busy,
}

pub trait TimestampSource {
    /// Unix timestamp in _microseconds_ since unix epoch (1970-01-01)
    ///
    /// Note: the timestampsource may return errors, for example
    /// if it needs to be (re-)initialized
    fn timestamp_us(&self) -> Result<i64, Error>;
}

pub trait AdjustableTimestampSource: TimestampSource {
    /// Adjust the time to the given target time
    ///
    /// If the clock error is below `offset_limit` us, the time is adjusted
    /// gradually, and `None` is returned.
    ///
    /// If the clock error is above the limit, the clock is immediately adjusted
    /// and the applied change is returned in microseconds (e.g. -1_000_000 means one second
    /// was subtracted from the clock)
    fn adjust_us(&self, target_time_us: i64, offset_limit: u32) -> Result<i64, Error>;

    /// Calibrate the clocksource by correcting it for the given error
    ///
    /// For example, an error of +100ppm means the uncompensated clock runs 100ppm too fast
    /// NB: implementers must ensure calibration is *absolute*: successive
    /// calls must not have an accumulating effect!
    fn calibrate(&self, _correction: &time_adjust::TotalDriftError) -> Result<(), CalError> {
        Ok(())
    }

    /// Clock accuracy based on typical tolerances depending on board revision
    ///
    /// NB: This estimate does not include temperature effects.
    /// See [AdjustableTimestampSource::temp_correction] to estimate the additional
    /// tolerances due to temperature effects.
    ///
    /// Implementers note: the default implementation assumes maximum uncertainty
    fn initial_accuracy(&self) -> DriftError {
        DriftError(ErrorEstimate::unknown())
    }

    /// Estimated temperature-dependent error
    ///
    /// Can be used to improve calibration. Note that the result is *relative*
    /// and independent of the absolute (board/tolerance specific) error.
    ///
    /// Implementers note: the default implementation assumes no temperature effects
    fn temp_correction(&self, _temp_celsius: f32) -> time_adjust::TempErrorEstimate {
        TempErrorEstimate::zero()
    }
}

/// This type cannot be instantiated.
/// It is used as a placeholder type when declaring a Time struct
/// without using the high resolution clock
///
/// # Examples
///
/// ```
/// use sensor_link_firmware::drivers::time::*;
/// struct MyTimer{}
/// impl TimestampSource for MyTimer {
///     fn timestamp_us(&self) -> Result<i64, Error> {
///         // <- Your platform-specific timer implementation here
///         Err(Error::TimeUnknown)
///     }
/// }
/// let my_timer = MyTimer{};
///
/// let time: CombinedTime<MyTimer, DummySource> = CombinedTime::new(my_timer);
/// ```
pub struct DummySource {
    _dummy: (),
}
impl TimestampSource for DummySource {
    fn timestamp_us(&self) -> Result<i64, Error> {
        Err(Error::TimeUnknown)
    }
}
impl AdjustableTimestampSource for DummySource {
    fn adjust_us(&self, _target_time_us: i64, _offset_limit: u32) -> Result<i64, Error> {
        Err(Error::TimeUnknown)
    }
}

/// Time Correction: keeps track of error between two timers and calculates drift correction
///
/// Used by `CombinedTime` for combining two clocks sources.
#[derive(Clone)]
pub struct TimeCorrection {
    /// Set after first update() or if initialized via new()
    is_initialized: bool,

    /// Global offset between fast/accurate clock
    offset: i64,

    /// Timestamp when this correction has started
    last_corrected: i64,

    /// Slope correction: [-SLOPE_RESOLUTION = -1, SLOPE_RESOLUTION = +1] to be applied since `last_corrected`
    slope: i32,

    /// Error at t=last_corrected. Apply an extra (fixed) slope to counter this
    error: i32,
}

impl Default for TimeCorrection {
    fn default() -> Self {
        Self {
            is_initialized: false,
            offset: 0,
            last_corrected: 0,
            slope: 0,
            error: 0,
        }
    }
}

// 'normal' clock sync interval
pub const UPDATE_INTERVAL_US: i64 = 120_000_000;

// minimum interval: dont sync more often than this (resolution is limited on short intervals)
pub const UPDATE_INTERVAL_MIN_US: i64 = 4_000_000;

const SLOPE_RESOLUTION_I32: i32 = 1 << 24;
const SLOPE_RESOLUTION: i64 = SLOPE_RESOLUTION_I32 as i64;

impl TimeCorrection {
    /// Create a new TimeCorrection instance with initial values.
    ///
    /// Note that initial_offset is an offset relative to the reference time (see `update()`)
    /// See `TimeCorrection::default()` for instantiating without initializing
    pub fn initialized(initial_time: i64, initial_offset: i64) -> Self {
        Self {
            is_initialized: true,
            offset: initial_offset,
            last_corrected: initial_time,
            slope: 0,
            error: 0,
        }
    }

    pub fn correct(&self, raw_time: i64) -> i64 {
        // Correct for offset between timers
        let mut now = raw_time + self.offset;

        // Correct for slope of timer (if possible)
        if now >= self.last_corrected {
            let t_since_last = now - self.last_corrected;
            if let Some(compensation) = t_since_last.checked_mul(i64::from(self.slope)) {
                now += compensation / SLOPE_RESOLUTION
            }
        }

        // TODO correct for steady state error

        now
    }

    /// Hints whether or not calling update() should or should not be called
    ///
    /// Simply calling update() directly is safe but might be inconvenient as it needs
    /// &mut self.
    pub fn should_update(&self, ref_time: i64, offset: i64, offset_limit: u32) -> bool {
        let t_now = ref_time + offset;
        let t_since_last = t_now - self.last_corrected;

        !self.is_initialized
            || offset.abs() >= offset_limit as i64
            || t_since_last >= UPDATE_INTERVAL_MIN_US
    }

    /// Update the time correction parameters based on the current `ref_time` and
    /// known `offset` to the last `correct()` output.
    ///
    /// For example, ref time 1000 with offset 40 means that the time correction
    /// output should be `1000` but actually outputs `1000+40=1040`: the TimeCorrection output
    /// clock is slightly too fast and needs to be adjusted to tick slower.
    ///
    /// If the offset is too large, it is applied immediately.
    pub fn update(&mut self, ref_time: i64, offset: i64, offset_limit: u32) {
        let t_now = ref_time + offset;
        let error = offset;
        let t_since_last = t_now - self.last_corrected;

        // First time: initialize
        if !self.is_initialized {
            self.offset = -error;
            self.last_corrected = t_now;
            self.is_initialized = true;

        // Error too large: immediately jump time by adjusting the offset
        } else if error.abs() >= offset_limit as i64 {
            self.offset -= error;
            self.last_corrected = t_now;
            self.error = 0;

        // Error is small: gradually correct by adjusting the slope
        } else if (t_since_last > UPDATE_INTERVAL_US)
            || (self.error_since_last(error).unsigned_abs() >= (u64::from(offset_limit / 4)))
        {
            // Note: self.offset stays the same: since the error is small, no need for discontinuous jumps

            self.update_slope(t_since_last, error).ok();
            self.last_corrected = t_now;
            self.error = error as i32; // TODO filter? ideally weighted by t_since_last
        }
    }

    /// Update slope correction
    ///
    /// error is relative to ref: +error means the clock runs to fast relative to ref clock
    fn update_slope(&mut self, t_since_last: i64, error: i64) -> Result<(), TryFromIntError> {
        if t_since_last > 0 {
            // This is guaranteed to fit in i32 unless the error is extremely large (clock frequency off be >> 100%)
            let slope_correction =
                i32::try_from((self.error_since_last(error) * SLOPE_RESOLUTION) / t_since_last)?;

            // Note: strictly mathematically, we should multiply these slopes together instead of adding them.
            // However for small fractions the difference is neglectible (e.g. 0.05 + 0.001 == 0.051, while 1.05 * 1.001 == 1.05105)
            self.slope = self.slope.saturating_add(slope_correction);
        }

        Ok(())
    }

    fn error_since_last(&self, error: i64) -> i64 {
        i64::from(self.error) - error
    }
}

/// Timekeeping that combines high accuracy with high resolution
///
/// Two clock sources can be combined: an accurate clock that always
/// keeps ticking and (optionally) a high resolution clock. The goal
/// is to reach 1us time resolution while maintaining a long-term
/// accurate time with minimal discontinuities.
///
/// # Examples
///
/// ```
/// use sensor_link_firmware::drivers::time::*;
/// struct MyTimer{}
/// impl TimestampSource for MyTimer {
///     fn timestamp_us(&self) -> Result<i64, Error> {
///         // <- Your platform-specific timer implementation here
///         Err(Error::TimeUnknown)
///     }
/// }
/// let my_timer = MyTimer{};
///
/// // Just a wrapper around `MyTimer` which is supposedly good enough
///  let time = CombinedTime::new(my_timer);
///
/// // Combining an RTC (slow update rate but stable)
/// // with a general purpose timer (fast but may drift or stop in low power modes)
/// let rtc = MyTimer{};
/// let rtc_resolution_us = 1_000_000;
/// let timer_1mhz = MyTimer{};
/// let time = CombinedTime::new(rtc).with_high_resolution_clock(timer_1mhz, rtc_resolution_us);
/// ```
pub struct CombinedTime<AC, FC> {
    accurate_clock: AC,
    fast_clock: Option<FC>,
    max_deviation: u32,
    time_correction: DoubleBuffer<TimeCorrection>,
}

impl<AC> CombinedTime<AC, DummySource>
where
    AC: TimestampSource,
{
    /// Instantiate a Time struct with an accurate clock
    ///
    /// The given clock should be as accurate as possible (always keeps ticking)
    /// with a focus on minimizing long-term drift (= time offset from 'real' time).
    ///
    /// A high time resolution is not required. The resolution can be improved by
    /// calling `with_high_resolution_clock()`.
    pub fn new(accurate_clock: AC) -> Self {
        Self {
            accurate_clock,
            fast_clock: None,
            max_deviation: 0,
            time_correction: DoubleBuffer::new(TimeCorrection::default()),
        }
    }

    /// Optionally add a high (1 microsecond) resolution timer to increase the time resolution
    ///
    /// The output from this timer is only used to interpolate the main `accurate_clock` so
    /// the accuracy is less important. It may even be stopped temporarily if the resulting
    /// lower time resolution is acceptable (for example in low power standby mode).
    ///
    /// If the interpolated time deviates more than `max_deviation_us` from the accurate clock, the
    /// accurate clock takes over. The max deviation must be >= the accurate clock resolution.
    pub fn with_high_resolution_clock<FC: TimestampSource>(
        self,
        fast_clock: FC,
        max_deviation_us: u32,
    ) -> CombinedTime<AC, FC> {
        let time = CombinedTime {
            accurate_clock: self.accurate_clock,
            fast_clock: Some(fast_clock),
            max_deviation: max_deviation_us,
            time_correction: self.time_correction,
        };

        // Kick off correction if possible
        time.timestamp_us().ok();
        time
    }
}

#[derive(Debug)]
enum ClockStatus {
    /// Accurate clock has no valid time
    Error(Error),

    /// Accurate clock is OK, but no fast clock is available or out of sync
    LimitedResolution(i64, Option<i64>),

    /// Accurate clock is in sync: fast clock offset is valid
    FastClock(i64, i64),
}

impl<AC, FC> CombinedTime<AC, FC>
where
    AC: TimestampSource,
    FC: TimestampSource,
{
    /// Returns the current time and interpolation offset info
    fn clock_status(&self) -> ClockStatus {
        let fast = self
            .fast_clock
            .as_ref()
            .and_then(|fast| fast.timestamp_us().ok());

        match (self.accurate_clock.timestamp_us(), fast) {
            (Err(error), _) => ClockStatus::Error(error),
            (Ok(accurate), None) => ClockStatus::LimitedResolution(accurate, None),
            (Ok(accurate), Some(fast)) => {
                let fast = self.time_correction.read().correct(fast);
                let delta = fast - accurate;

                let abs_delta = delta.unsigned_abs();
                if abs_delta < self.max_deviation as u64 {
                    ClockStatus::FastClock(accurate, delta)
                } else {
                    ClockStatus::LimitedResolution(accurate, Some(delta))
                }
            }
        }
    }

    fn update_time_correction(&self, time: i64, offset: i64) {
        // Check first, before attempting to acquire the write lock
        if self
            .time_correction
            .read()
            .should_update(time, offset, self.max_deviation)
        {
            // Note: if we fail to acquire the lock, that's fine: another thread/context is already applying the correction
            if let Some(mut correction) = self.time_correction.try_write() {
                correction.update(time, offset, self.max_deviation);
            }
        }
    }

    pub fn timestamp_us(&self) -> Result<i64, Error> {
        let clock_status = self.clock_status();

        match clock_status {
            ClockStatus::Error(error) => Err(error),
            ClockStatus::LimitedResolution(time, None) => Ok(time),

            ClockStatus::LimitedResolution(time, Some(offset)) => {
                self.update_time_correction(time, offset);
                Ok(time)
            }
            ClockStatus::FastClock(time, offset) => {
                self.update_time_correction(time, offset);
                Ok(time.wrapping_add(offset))
            }
        }
    }
}

impl<AC, FC> TimestampSource for CombinedTime<AC, FC>
where
    AC: TimestampSource,
    FC: TimestampSource,
{
    fn timestamp_us(&self) -> Result<i64, Error> {
        self.timestamp_us()
    }
}

// Combined source: adjust the accurate clock
impl<AC, FC> AdjustableTimestampSource for CombinedTime<AC, FC>
where
    AC: AdjustableTimestampSource,
    FC: TimestampSource,
{
    fn adjust_us(&self, target_time_us: i64, offset_limit: u32) -> Result<i64, Error> {
        self.accurate_clock.adjust_us(target_time_us, offset_limit)
    }

    fn calibrate(&self, correction: &time_adjust::TotalDriftError) -> Result<(), CalError> {
        self.accurate_clock.calibrate(correction)
    }

    fn initial_accuracy(&self) -> DriftError {
        self.accurate_clock.initial_accuracy()
    }

    #[inline]
    fn temp_correction(&self, temp_celsius: f32) -> TempErrorEstimate {
        self.accurate_clock.temp_correction(temp_celsius)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mock::mock_timer::*;

    #[test]
    fn global_instance() {
        let mut timer = MockTimer::new();

        let tmr: &'static MockTimer = Box::leak(Box::new(timer.clone()));

        init_timer(tmr).expect("Settin timer for first time should be OK");
        init_timer(tmr).expect_err("Setting timer instance twice must not be possible");

        // Global timer instance should follow provided mock timer
        assert_eq!(timer.timestamp_us().unwrap(), timestamp_us().unwrap());
        timer.inc_time_micros(5_000);
        assert_eq!(timer.timestamp_us().unwrap(), timestamp_us().unwrap());
    }

    #[test]
    fn timestamp_passthrough() {
        let mut mock_timer = MockTimer::new();
        let t_start = mock_timer.timestamp_us().unwrap();

        // Initial time matches
        let time = CombinedTime::new(mock_timer.clone());
        assert_eq!(mock_timer.timestamp_us(), time.timestamp_us());

        // Still matches after increment
        mock_timer.inc_time_micros(9_876_543);
        let new_time = time.timestamp_us();
        assert_eq!(mock_timer.timestamp_us(), new_time);
        assert_eq!(t_start + 9_876_543, new_time.unwrap())
    }

    #[test]
    fn timestamp_increased_resolution() {
        // Simulates RTC (slow but supposedly accurate)
        let mut mock_rtc = MockTimer::new();

        // Simulates fast clock
        let mut mock_fast = MockTimer::new();
        mock_fast.set_time_micros(9_999_999);

        // Initial time matches RTC
        let time = CombinedTime::new(mock_rtc.clone())
            .with_high_resolution_clock(mock_fast.clone(), 100_000);

        assert_eq!(mock_rtc.timestamp_us(), time.timestamp_us());

        // Time keeps tracking RTC
        mock_rtc.inc_time_micros(1_000_000);
        mock_fast.inc_time_micros(1_000_000);
        let new_time = time.timestamp_us();
        assert_eq!(mock_rtc.timestamp_us(), new_time);

        // Time slightly incremented by fast clock
        mock_fast.inc_time_micros(10);
        assert_eq!(
            mock_rtc.timestamp_us().unwrap() + 10,
            time.timestamp_us().unwrap()
        )
    }

    /// Verify that Time keeps tracking the accurate timer,
    /// even if the 'fast' timer offset becomes too large
    /// (for example because of large drift or temporarily stopped)
    #[test]
    fn timestamp_readjusts_after_fastclock_halted() {
        // Simulates RTC (slow but supposedly accurate)
        let mut mock_rtc = MockTimer::new();

        // Simulates fast clock
        let mut mock_fast = MockTimer::new();
        mock_fast.set_time_micros(9_999_999);

        // Initial time matches RTC
        const MAX_DEVIATION_US: u32 = 100_000;
        let time = CombinedTime::new(mock_rtc.clone())
            .with_high_resolution_clock(mock_fast.clone(), MAX_DEVIATION_US);

        assert_eq!(mock_rtc.timestamp_us(), time.timestamp_us());

        // Time keeps tracking RTC even though fast clock has halted for 1 second (> max deviation)
        mock_rtc.inc_time_micros(1_000_000);
        let new_time = time.timestamp_us();
        assert_eq!(mock_rtc.timestamp_us(), new_time);

        // Time slightly incremented by fast clock
        mock_fast.inc_time_micros(10);
        assert_eq!(
            mock_rtc.timestamp_us().unwrap() + 10,
            time.timestamp_us().unwrap()
        )
    }

    /// Verify that Time keeps tracking the accurate timer,
    /// even if the 'fast' timer ticks at a slightly different frequency
    /// (some frequency variation / drift is expected)
    #[test]
    fn timestamp_drift_correction() {
        // Simulates RTC (slow but supposedly accurate)
        let mut mock_rtc = MockTimer::new();

        // Simulates fast clock
        let mut mock_fast = MockTimer::new();
        mock_fast.set_time_micros(9_999_999);

        // Initial time matches RTC
        const MAX_DEVIATION_US: u32 = 100_000;
        let time = CombinedTime::new(mock_rtc.clone())
            .with_high_resolution_clock(mock_fast.clone(), MAX_DEVIATION_US);

        assert_eq!(mock_rtc.timestamp_us(), time.timestamp_us());

        // 8 second passes by. The 'fast clock' runs 1% too fast: this leads to some error within MAX_DEVIATION_US of the RTC
        mock_rtc.inc_time_micros(8_000_000);
        mock_fast.inc_time_micros(8_080_000);
        {
            let error = time.timestamp_us().unwrap() - mock_rtc.timestamp_us().unwrap();
            println!("error after 8 sec: {}", error);
            assert!(error <= 80_000);
        }

        // Another 510ms pass by
        // - RTC increases just 500 (doesn't have the resolution)
        // - fast timer increases 515.1 (510 ms + 1% since its running too fast)
        // - Time should recognize the 1% speed difference and compensate for it
        mock_rtc.inc_time_micros(500_000);
        mock_fast.inc_time_micros(515_100);

        // This is the exact actual time: 8.510 seconds has passed in this test
        let expected = mock_rtc.timestamp_us().unwrap() + 10_000;

        {
            let error = time.timestamp_us().unwrap() - expected;
            println!("error after 0.510 more sec: {}", error);
            assert!(error > -10 && error < 80_010); // error should stay more or less constant +/- some rounding error
        }
    }

    #[test]
    fn timestamp_drift_correction_is_stable() {
        // Simulates RTC (slow but supposedly accurate)
        let mut mock_rtc = MockTimer::new();

        // Simulates fast clock
        let mut mock_fast = MockTimer::new();
        mock_fast.set_time_micros(9_999_999);

        // Initial time matches RTC
        const MAX_DEVIATION_US: u32 = 100_000;
        let time = CombinedTime::new(mock_rtc.clone())
            .with_high_resolution_clock(mock_fast.clone(), MAX_DEVIATION_US);

        assert_eq!(mock_rtc.timestamp_us(), time.timestamp_us());

        // 8 second passes by. The 'fast clock' runs 1% too fast: this leads to some error within MAX_DEVIATION_US of the RTC
        mock_rtc.inc_time_micros(8_000_000);
        mock_fast.inc_time_micros(8_080_000);
        {
            let error = time.timestamp_us().unwrap() - mock_rtc.timestamp_us().unwrap();
            println!("error after 8 sec: {}", error);
            assert!(error <= 80_000);
        }

        // Fast repeated correction attempts
        let _ = time.timestamp_us();
        mock_fast.inc_time_micros(6);
        let _ = time.timestamp_us();
        mock_rtc.inc_time_micros(500_000);
        mock_fast.inc_time_micros(515_100);

        // This is the exact actual time: 8.510 seconds has passed in this test
        let expected = mock_rtc.timestamp_us().unwrap() + 10_006;

        {
            let error = time.timestamp_us().unwrap() - expected;
            println!("error after 0.510 more sec: {}", error);
            assert!(error > -10 && error < 80_010); // error should stay more or less constant +/- some rounding error
        }
    }
}

//! Monotonic time API
//!
//! The Monotonic time guarantees _monotonicity_ : a newer timestamp is always guaranteed >= an earlier timestamp.
//! This means the time can never tick / glitch backwards, which _can_ happen for the normal wall-clock-time during time sync.

use futures::{
    future::{select, Either},
    pin_mut, Future,
};

use crate::drivers::timer_queue::{self, timer_queue};

/// Dispatched to the driver implemented by the library user.
/// See [init_monotonic](crate::init_monotonic)
fn instant_after_micros(microseconds: u64, offset: Option<&MonotonicInstant>) -> MonotonicInstant {
    // offset: start at given instant or now()
    let offset = match offset {
        Some(instant) => instant.to_driver(),
        None => timer_queue().now(),
    };

    // microsecond timestamp in the future
    let driver_instant = offset.saturating_add(microseconds);

    // convert to instant
    MonotonicInstant::from_driver(driver_instant)
}

#[derive(Debug, Clone)]
pub struct Time;

/// Keeps track of a timeout
#[derive(Debug, Clone)]
pub struct Timeout {
    delay: Option<MonotonicInstant>,
}

#[derive(Debug, Clone)]
pub struct MonotonicInstant {
    /// Private: microseconds since init.
    /// Use for relative comparisons only: _this is not wall clock time!_
    micros_since_init: u64,
}

impl MonotonicInstant {
    /// Monotonic timestamp representing 'now'.
    pub fn now() -> Self {
        instant_after_micros(0, None)
    }

    #[cfg(any(test, feature = "test-mono"))]
    pub fn test(micros_since_init: i64) -> Self {
        MonotonicInstant {
            micros_since_init: micros_since_init as u64,
        }
    }

    /// Create a new timestamp that is `micros` microseconds further in the future
    pub fn add_micros(&self, micros: u64) -> Self {
        instant_after_micros(micros, Some(self))
    }

    /// Microseconds since boot. Use for relative comparisons only —
    /// _this is not wall clock time_.
    pub fn micros_since_init(&self) -> u64 {
        self.micros_since_init
    }

    /// Amount of time _in microseconds_ that has elapsed between earlier and this timestamp.
    /// Returns 0 if `self` < `earlier`
    pub fn micros_since(&self, earlier: &Self) -> u64 {
        self.micros_since_init
            .saturating_sub(earlier.micros_since_init)
    }

    /// Amount of time _in microseconds_ that has elapsed since this timestamp
    pub fn elapsed_us(&self) -> u64 {
        let now = instant_after_micros(0, None);
        now.micros_since(self)
    }

    /// Amount of time _in seconds_ that has elapsed since this timestamp
    pub fn elapsed_sec(&self) -> u64 {
        self.elapsed_us() / 1_000_000
    }

    /// Create future for this instant.
    ///
    /// Future completes when the time is >= instant
    pub async fn to_future(&self) {
        timer_queue().delay_until(self.to_driver()).await
    }

    fn from_driver(driver_instant: timer_queue::Ticks) -> Self {
        Self {
            micros_since_init: driver_instant,
        }
    }
    fn to_driver(&self) -> timer_queue::Ticks {
        self.micros_since_init
    }
}

/// Await a delay in miliseconds
///
/// Example:
/// ```
/// use sensor_link_firmware::monotonic_time::{delay_ms, traits::MonotonicTime};
///
/// async fn wait_a_sec<T: MonotonicTime>() {
///     delay_ms(1_000).await
/// }
/// ```
pub async fn delay_ms(milliseconds: u32) {
    let micros = milliseconds as u64 * 1_000;
    delay_us(micros).await
}

/// Await a delay in microseconds
///
/// Example:
/// ```
/// use sensor_link_firmware::monotonic_time::{delay_us, traits::MonotonicTime};
///
/// async fn wait_a_sec<T: MonotonicTime>() {
///     delay_us(1_000_000).await
/// }
/// ```
pub async fn delay_us(microseconds: u64) {
    timer_queue().delay(microseconds).await
}

/// Create a [Timeout] instance
pub fn timeout() -> Timeout {
    Timeout::new()
}

/// Create a [MonotonicInstant]
pub fn now() -> MonotonicInstant {
    MonotonicInstant::now()
}

impl Timeout {
    /// Create a new timeout instance (which is not configured yet, see `set_ms`)
    pub const fn new() -> Self {
        Self { delay: None }
    }

    /// Configure and start a timeout/delay with a duration in ms.
    /// NB: This does nothing untill you await `wait()`.
    ///
    /// Example:
    /// ```
    /// use sensor_link_firmware::monotonic_time::Timeout;
    ///
    /// async fn wait_a_sec() {
    ///     let mut timeout = Timeout::new();
    ///
    ///     // Set a timeout (relative to current time t_0)
    ///     timeout.set_ms(1_000);
    ///
    ///     // ... (do stuff)
    ///
    ///     // Wait untill the timeout expires (time >= t_0 + 1_000)
    ///     timeout.wait().await;
    /// }
    /// ```
    pub fn set_ms(&mut self, delay_ms: u32) {
        let micros = delay_ms as u64 * 1_000;
        self.set_us(micros)
    }

    /// Configure and start a timeout/delay with a duration in _microseconds_.
    /// See [set_ms](method@Timeout::set_ms)
    pub fn set_us(&mut self, delay_micros: u64) {
        let t_until = instant_after_micros(delay_micros, None);
        self.delay.replace(t_until);
    }

    /// Set the timeout to the minimum of the current timeout and the given timeout.
    /// If the timeout is not set, it will be set to the given timeout.
    pub fn reduce_to_ms(&mut self, delay_ms: u32) {
        let micros = delay_ms as u64 * 1_000;
        self.reduce_to_us(micros)
    }

    /// Set the timeout to the minimum of the current timeout and the given timeout.
    /// If the timeout is not set, it will be set to the given timeout.
    pub fn reduce_to_us(&mut self, delay_micros: u64) {
        let t_until = instant_after_micros(delay_micros, None);
        if let Some(delay) = &self.delay {
            if delay < &t_until {
                return;
            }
        }
        self.delay.replace(t_until);
    }

    pub fn is_set(&self) -> bool {
        self.delay.is_some()
    }

    /// Clear the timeout (if any)
    pub fn clear(&mut self) {
        self.delay = None;
    }

    /// Check non-blocking if timeout has expired.
    pub fn is_expired(&self) -> bool {
        if let Some(delay) = &self.delay {
            let now = instant_after_micros(0, None);
            now > *delay
        } else {
            false
        }
    }

    /// Check if timeout has expired and by how many microseconds (if expired)
    pub fn expired_by(&self) -> Option<u64> {
        let expired_us = self.delay.as_ref()?.elapsed_us();
        match expired_us {
            // expired (by at least 1 us)
            expired @ 1..=u64::MAX => Some(expired),

            // not expired yet (now <= delay)
            0 => None,
        }
    }

    /// Wait for timeout to finish _if_ one was configure with `set_ms`.
    pub async fn wait(&mut self) {
        if let Some(instant) = &self.delay {
            instant.to_future().await
        }
    }
}
impl PartialEq for MonotonicInstant {
    fn eq(&self, other: &Self) -> bool {
        self.micros_since_init == other.micros_since_init
    }
}

impl PartialOrd for MonotonicInstant {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        self.micros_since_init.partial_cmp(&other.micros_since_init)
    }
}

/// Trait to extend futures with a timeout
///
/// If this trait is in scope, a future can be awaited as `my_future.with_timeout_ms(300).await`
pub trait FutureTimeout: Future
where
    Self: Sized,
{
    async fn with_timeout_ms(self, timeout_ms: u32) -> Option<<Self as Future>::Output> {
        // pin_mut does not like working with self
        let this = self;
        pin_mut!(this);

        let timeout = delay_ms(timeout_ms);
        pin_mut!(timeout);
        match select(this, timeout).await {
            Either::Left((l, _)) => Some(l),
            Either::Right((_, _)) => None,
        }
    }

    async fn with_timeout_us(self, timeout_us: u64) -> Option<<Self as Future>::Output> {
        // pin_mut does not like working with self
        let this = self;
        pin_mut!(this);

        let timeout = delay_us(timeout_us);
        pin_mut!(timeout);
        match select(this, timeout).await {
            Either::Left((l, _)) => Some(l),
            Either::Right((_, _)) => None,
        }
    }
}

impl<F: Future> FutureTimeout for F {}

pub mod traits {
    /// Trait defining the globally available monotonic API.
    ///
    /// Note: these functions are also directly available as global functions.
    /// The trait can still be useful to decouple implementation from user,
    /// for example to make a struct easier to unit-test.
    pub trait MonotonicTime {
        type Timeout: Timeout;
        type Instant;

        async fn delay_ms(milliseconds: u32);

        async fn delay_us(microseconds: u64);

        fn timeout() -> Self::Timeout;
        fn now() -> Self::Instant;
    }

    pub trait Timeout {
        /// Create a new timeout instance (which is not configured yet, see `set_ms`)
        fn new() -> Self;

        /// Configure and start a timeout/delay with a duration in ms.
        /// NB: This does nothing untill you await `wait()`.
        fn set_ms(&mut self, delay_ms: u32);

        /// Configure and start a timeout/delay with a duration in _microseconds_.
        /// See [set_ms](method@Timeout::set_ms)
        fn set_us(&mut self, delay_micros: u64);

        fn is_set(&self) -> bool;

        /// Clear the timeout (if any)
        fn clear(&mut self);

        /// Check non-blocking if timeout has expired.
        fn is_expired(&self) -> bool;

        /// Check if timeout has expired and by how many microseconds (if expired)
        fn expired_by(&self) -> Option<u64>;

        /// Wait for timeout to finish _if_ one was configure with `set_ms`.
        async fn wait(&mut self);
    }

    pub trait Instant {
        /// Monotonic timestamp representing 'now'.
        fn now() -> Self;

        /// Create a new timestamp that is `micros` microseconds further in the future
        fn add_micros(&self, micros: u64) -> Self;

        /// Amount of time _in microseconds_ that has elapsed between earlier and this timestamp.
        /// Returns 0 if `self` < `earlier`
        fn micros_since(&self, earlier: &Self) -> u64;

        /// Amount of time _in microseconds_ that has elapsed since this timestamp
        fn elapsed_us(&self) -> u64;

        /// Amount of time _in seconds_ that has elapsed since this timestamp
        fn elapsed_sec(&self) -> u64 {
            self.elapsed_us() / 1_000_000
        }
    }
}

impl traits::MonotonicTime for Time {
    async fn delay_ms(milliseconds: u32) {
        delay_ms(milliseconds).await
    }

    async fn delay_us(microseconds: u64) {
        delay_us(microseconds).await
    }

    fn timeout() -> Timeout {
        timeout()
    }

    fn now() -> MonotonicInstant {
        now()
    }

    type Timeout = Timeout;
    type Instant = MonotonicInstant;
}

impl traits::Timeout for Timeout {
    fn new() -> Self {
        Timeout::new()
    }

    fn set_ms(&mut self, delay_ms: u32) {
        self.set_ms(delay_ms)
    }

    fn set_us(&mut self, delay_micros: u64) {
        self.set_us(delay_micros)
    }

    fn is_set(&self) -> bool {
        self.is_set()
    }

    fn clear(&mut self) {
        self.clear()
    }

    fn is_expired(&self) -> bool {
        self.is_expired()
    }

    fn expired_by(&self) -> Option<u64> {
        self.expired_by()
    }

    async fn wait(&mut self) {
        self.wait().await
    }
}

impl traits::Instant for MonotonicInstant {
    fn now() -> Self {
        MonotonicInstant::now()
    }

    fn add_micros(&self, micros: u64) -> Self {
        self.add_micros(micros)
    }

    fn micros_since(&self, earlier: &Self) -> u64 {
        self.micros_since(earlier)
    }

    fn elapsed_us(&self) -> u64 {
        self.elapsed_us()
    }

    fn elapsed_sec(&self) -> u64 {
        self.elapsed_sec()
    }
}

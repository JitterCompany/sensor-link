use core::marker::PhantomData;

pub type Ticks = u64;

// Never actually called. It's here to validate that both test/not_test
// implementations of TimerQueue implement the required methods.
#[allow(unused)]
fn timerqueue_must_impl_trait(queue: TimerQueue) {
    struct ValidImpl<T: TimerQueueTrait>(T);
    ValidImpl(queue);
}

impl TimerQueueTrait for TimerQueue {
    fn initialize(&self, backend: Backend) {
        self.initialize(backend);
    }

    fn now(&self) -> Ticks {
        self.now()
    }

    async fn delay(&self, duration: Ticks) {
        self.delay(duration).await
    }

    async fn delay_until(&self, instant: Ticks) {
        self.delay_until(instant).await
    }

    unsafe fn on_monotonic_interrupt(&self) {
        self.on_monotonic_interrupt()
    }
}

// Implicit trait: methods that the TimerQueue type should have.
#[allow(unused)]
trait TimerQueueTrait {
    fn initialize(&self, backend: Backend);

    fn now(&self) -> Ticks;

    /// Delay for at least some duration of time.
    async fn delay(&self, duration: Ticks);

    /// Delay to some specific time instant.
    async fn delay_until(&self, instant: Ticks);

    /// Call this in the interrupt handler.
    unsafe fn on_monotonic_interrupt(&self);
}

pub struct Backend {
    private: PhantomData<()>,
}

pub struct MonoDriver {
    pub inner_driver: &'static dyn MonotonicDriver,
}

impl MonoDriver {
    pub fn initialize(&self) {
        timer_queue().initialize(Backend {
            private: PhantomData,
        })
    }
}

// Dependencies: these must be implemented by the user
extern "Rust" {

    /// This symbol must be provided by the user to implement platform-specific stuff.
    /// If the build fails with a linker error `undefined symbol: _PRIVATE_MONOTONIC_DRIVER`,
    /// the symbol is not found: double-check and add `#[no_mangle]`.
    ///
    /// Safety: implementers must be careful that the implementation is `Sync`:
    /// the [_PRIVATE_MONOTONIC_DRIVER] holds a `&'static` to the Driver which is used
    /// by all tasks/threads/contexts/interrupt levels.
    static _PRIVATE_MONOTONIC_DRIVER: MonoDriver;

}

#[macro_export]
macro_rules! init_monotonic {
    ($type:tt, $instance:expr) => {
        use $crate::drivers::timer_queue::MonoDriver;

        // paste::paste! {

        static _PRIVATE_MONOTONIC_DRIVER_INSTANCE: $type = $instance;

        #[no_mangle]
        static _PRIVATE_MONOTONIC_DRIVER: MonoDriver = MonoDriver {
            inner_driver: &_PRIVATE_MONOTONIC_DRIVER_INSTANCE,
        };
        _PRIVATE_MONOTONIC_DRIVER.initialize();
        //}
    };
}

/// Call this from the interrupt handler for the timer implemented by [MonotonicDriver].
///
/// ## Safety
///
/// This method is safe to call as long as it is only called from the interrupt.
pub unsafe fn interrupt() {
    timer_queue().on_monotonic_interrupt()
}

/// Monotonic time driver: to be implemented by each platform.
///
/// This is a low-level interface for platform compatibility.
/// Do _NOT_ depend on this trait directly from high-level drivers / libraries.
///
/// ## Implementation
///
/// the Driver trait mimics the typical features of a timer peripheral as found
/// in most microcontrollers, but can be implemented on any platform.
///
/// ### Interrupts
///
/// The following two events should trigger an interrupt (call [interrupt]):
/// - the timer has reached the compare value
/// - pend_interrupt was called
/// [interrupt] must be called from the interrupt thread/queue/task only.
pub trait MonotonicDriver: 'static + Sync {
    /// Get current time in ticks
    fn now(&self) -> Ticks;

    /// Set the compare value of the timer interrupt
    ///
    /// The interrupt should call [interrupt] as soon as the
    /// timer matches (>=) `instant`.
    fn set_compare(&self, instant: Ticks);

    /// Clear compare interrupt flag
    /// Called by [interrupt] to acknowledge the interrupt
    fn clear_compare_flag(&self);

    /// Pend timer interrupt
    ///
    /// This should trigger the interrupt to call [interrupt]
    fn pend_interrupt(&self);

    /// Called on each interrupt. Optional
    fn on_interrupt(&self) {}
}

#[cfg(not(any(test, feature = "test-mono")))]
pub use normal_implementation::*;

/// This part of the implementation is not used in the tests.
/// The reason is it is hard to implement this right with the
/// tokio test framework as each test gets a separate runtime
/// but the time API is global. See [test_implementation]
#[cfg(not(any(test, feature = "test-mono")))]
mod normal_implementation {
    pub use super::*;
    use rtic_time::timer_queue;
    pub type TimerQueue = timer_queue::TimerQueue<Backend>;

    static TIMER_QUEUE: TimerQueue = TimerQueue::new();

    pub fn timer_queue() -> &'static TimerQueue {
        &TIMER_QUEUE
    }

    fn driver() -> &'static dyn MonotonicDriver {
        unsafe { _PRIVATE_MONOTONIC_DRIVER.inner_driver }
    }

    impl timer_queue::TimerQueueBackend for Backend {
        type Ticks = Ticks;

        fn now() -> Self::Ticks {
            driver().now()
        }

        fn set_compare(instant: Self::Ticks) {
            driver().set_compare(instant)
        }

        fn clear_compare_flag() {
            driver().clear_compare_flag()
        }

        fn pend_interrupt() {
            driver().pend_interrupt()
        }

        fn timer_queue() -> &'static TimerQueue {
            timer_queue()
        }

        fn on_interrupt() {
            driver().on_interrupt()
        }
    }
}

#[cfg(any(test, feature = "test-mono"))]
pub use test_implementation::*;

/// This part of the implementation is only used in the tests.
/// See [normal_implementation]
#[cfg(any(test, feature = "test-mono"))]
mod test_implementation {
    use super::*;
    use crate::sync::OnceLock;
    use std::sync::atomic::{AtomicI64, Ordering};
    use tokio::time;

    static T0: OnceLock<time::Instant> = OnceLock::new();
    // Time offset in microseconds (can be negative for "rewinding")
    static TIME_OFFSET: AtomicI64 = AtomicI64::new(0);

    fn get_t0() -> &'static time::Instant {
        // First ever timestamp is the reference time t0
        let now = time::Instant::now();

        // This may fail in due to two reasons:
        // 1. already initialized (which is fine)
        // 2. busy on another thread/core. NB: while busy,TO.get() will fail!
        T0.get_or_try_init(|| now.clone()).ok();

        // Retry loop: if get() fails, assume this is due to case 2 above.
        // This won't happen often but may occur on multicore platforms
        loop {
            match T0.get() {
                Some(instant) => break instant,
                None => {
                    std::thread::yield_now();
                }
            }
        }
    }

    pub struct TokioBasedTimer;

    pub type TimerQueue = TokioBasedTimer;
    impl TimerQueue {
        pub fn initialize(&self, _backend: Backend) {
            // NOP
        }

        pub fn now(&self) -> Ticks {
            let t0 = get_t0();

            // Calculate ticks (=micros) since t0, plus any offset
            let real_time = t0.elapsed().as_micros() as i64;
            let offset = TIME_OFFSET.load(Ordering::SeqCst);

            // Ensure we don't return negative time
            (real_time + offset).max(0) as Ticks
        }

        pub async fn delay(&self, duration: Ticks) {
            time::sleep(time::Duration::from_micros(duration)).await
        }

        pub async fn delay_until(&self, instant: Ticks) {
            if instant > self.now() {
                self.delay(instant - self.now()).await
            }
        }

        pub unsafe fn on_monotonic_interrupt(&self) {
            // NOP
        }

        /// Sets an absolute time offset in microseconds
        /// Positive values advance time, negative values rewind time (within limits)
        pub fn set_time_offset(&self, offset_micros: i64) {
            TIME_OFFSET.store(offset_micros, Ordering::SeqCst);
        }

        /// Advances the timer by the specified number of microseconds
        pub fn advance_time(&self, micros: u64) {
            TIME_OFFSET.fetch_add(micros as i64, Ordering::SeqCst);
        }

        /// Resets the timer offset back to zero
        pub fn reset_time(&self) {
            TIME_OFFSET.store(0, Ordering::SeqCst);
        }
    }

    pub fn timer_queue() -> &'static TimerQueue {
        &TokioBasedTimer
    }
}

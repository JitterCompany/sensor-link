//! Monotonic timer implementation for use with async rtic v2.
//!

use core::sync::atomic::{AtomicU32, Ordering};

use sensor_link_firmware::drivers::timer_queue::{self, MonotonicDriver};
use stm32ral::{modify_reg, read_reg, tim5, write_reg};

use crate::rcc::Clocks;

pub const TIMER_HZ: u32 = 1_000_000;

// Global state for keeping track of time and scheduled events.
static TIMER_OVERFLOWS: AtomicU32 = AtomicU32::new(0);
static NEXT_COMPARE_LO: AtomicU32 = AtomicU32::new(0);
static NEXT_COMPARE_HI: AtomicU32 = AtomicU32::new(0);

/// Monotonic timer. This takes pseudo ownership of TIMER5.
/// Make sure to call `interrupt_handler()` from the TIMER5 interrupt handler.
/// This implementation assumes to run as the highest interrupt priority
/// to ensure integrity of 64 bits timestamps.
pub struct Mono;

impl Mono {
    pub fn start(tim5: tim5::Instance, clocks: &Clocks) {
        crate::rcc::enable_rst_timer5();

        let clock_speed = clocks.pclk1();

        modify_reg!(tim5, tim5, CR1, CEN: Disabled);

        // Configure timer rate
        // PSC and ARR range: 0 to 65535
        // (PSC+1)*(ARR+1) = TIMclk/Updatefrequency = TIMclk * period
        let ratio = clock_speed.raw() / TIMER_HZ;
        let psc = ratio - 1;

        write_reg!(tim5, tim5, PSC, psc);

        // Enable update interrupt for keeping track of overflows.
        modify_reg!(tim5, tim5, DIER, UIE: 1, CC1IE: 1);

        // Set compare register to 0
        write_reg!(tim5, tim5, CCR1, 0u32);

        // Trigger update event to commit PSC
        write_reg!(tim5, tim5, EGR, UG: 1);

        // enable timer
        modify_reg!(tim5, tim5, CR1, CEN: Enabled);

        // Wait for interrupt triggered by UG
        while read_reg!(tim5, tim5, SR, UIF == 0) {}
        // Clear interrupt flag and initialize counter back to 0
        // because `set_match_value` depends on this via `duration_since_epoch`
        write_reg!(tim5, tim5, SR, UIF: 0);
        write_reg!(tim5, tim5, CNT, 0);

        // Init monotonic timer queue
        sensor_link_firmware::init_monotonic!(Mono, Mono {});

        // (Unmasking the TIM5 vector in NVIC and mapping it to interrupt_handler() is done by application)
    }

    /// Call this from the TIM5 interrupt handler
    ///
    /// # Safety
    ///
    /// Make sure to only call this from the TIM5 interrupt handler.
    pub unsafe fn interrupt_handler() {
        sensor_link_firmware::drivers::timer_queue::interrupt();
    }

    fn set_match_value(&self, instant: u64) {
        let now = self.now();
        let max = u32::MAX as u64;

        let delta = instant.saturating_sub(now);

        let ticks = match delta {
            // In the past: nothing to do
            0 => 0,

            // Timer will overflow at most once before reaching the target instant
            delta if delta <= max => {
                // Reset next compare value
                NEXT_COMPARE_LO.store(0, Ordering::SeqCst);
                NEXT_COMPARE_HI.store(0, Ordering::SeqCst);

                // Return ticks
                instant & max
            }

            // Instant more than one overflow in the future: save it to be retried after next overflow
            // Eventually enough overflows have happened that the instant will be less than 'max' in the future
            _ => {
                let ticks_lo = (instant & max) as u32;
                let ticks_hi = ((instant >> 32) & max) as u32;
                // Will overflow, we have to save the instant to apply it later
                NEXT_COMPARE_LO.store(ticks_lo, Ordering::SeqCst);
                NEXT_COMPARE_HI.store(ticks_hi, Ordering::SeqCst);
                // Use 0 for current compare so we do not interrupt twice
                0
            }
        };

        unsafe {
            write_reg!(tim5, TIM5, CCR1, ticks as u32);
        };
    }
}

/// Safely read 64 bits next_compare value
fn load_next_compare() -> u64 {
    let mut compare_hi0 = NEXT_COMPARE_HI.load(Ordering::SeqCst);
    loop {
        let compare_lo = NEXT_COMPARE_LO.load(Ordering::SeqCst);
        let compare_hi1 = NEXT_COMPARE_HI.load(Ordering::SeqCst);
        if compare_hi0 == compare_hi1 {
            return (compare_hi1 as u64) << 32 | (compare_lo as u64);
        }
        compare_hi0 = compare_hi1;
    }
}

impl timer_queue::MonotonicDriver for Mono {
    fn now(&self) -> timer_queue::Ticks {
        let mut overflows0 = TIMER_OVERFLOWS.load(Ordering::SeqCst) as u64;
        let now = loop {
            let count = unsafe { read_reg!(tim5, TIM5, CNT) };
            let overflows1 = TIMER_OVERFLOWS.load(Ordering::SeqCst) as u64;
            // Make sure we didn't have an additional overflow in between.
            if overflows0 == overflows1 {
                break overflows1 << 32 | (count as u64);
            }
            overflows0 = overflows1;
        };

        now
    }

    fn set_compare(&self, instant: timer_queue::Ticks) {
        self.set_match_value(instant);
    }

    fn clear_compare_flag(&self) {
        unsafe { modify_reg!(tim5, TIM5, SR, CC1IF: 0) }
    }

    fn pend_interrupt(&self) {
        cortex_m::peripheral::NVIC::pend(stm32ral::interrupt::TIM5);
    }

    fn on_interrupt(&self) {
        // Clear update flag if it was set
        unsafe {
            if read_reg!(tim5, TIM5, SR, UIF == 1) {
                modify_reg!(tim5, TIM5, SR, UIF: 0);
                TIMER_OVERFLOWS.fetch_add(1, Ordering::SeqCst);
                let compare = load_next_compare();
                self.set_match_value(compare);
            }
        }
    }
}

use crate::{
    gpio::{self, Pullup},
    syscfg, PinN,
};
use core::{
    convert::Infallible,
    future::poll_fn,
    sync::atomic::{AtomicU32, Ordering},
    task::Poll,
};
use cortex_m::interrupt;
use embassy_sync::waitqueue::AtomicWaker;
use embedded_hal::digital::{ErrorType, InputPin};
use sensor_link_firmware::{traits::Trigger as TriggerTrait, utils::bitwise::*};

use stm32ral::{exti, modify_reg, read_reg, reset_reg, write_reg};

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum Interrupt {
    Int0,
    Int1,
    Int2,
    Int3,
    Int4,
    Int5,
    Int6,
    Int7,
    Int8,
    Int9,
    Int10,
    Int11,
    Int12,
    Int13,
    Int14,
    Int15,
    Int16,

    Int18 = 18,
    Int19,
}
// Support EXTI 0..15 (all GPIO sources) + some more
const N_EXTI: usize = Interrupt::Int19 as usize + 1;

impl From<&PinN> for Interrupt {
    fn from(value: &PinN) -> Self {
        match value {
            PinN::P0 => Interrupt::Int0,
            PinN::P1 => Interrupt::Int1,
            PinN::P2 => Interrupt::Int2,
            PinN::P3 => Interrupt::Int3,
            PinN::P4 => Interrupt::Int4,
            PinN::P5 => Interrupt::Int5,
            PinN::P6 => Interrupt::Int6,
            PinN::P7 => Interrupt::Int7,
            PinN::P8 => Interrupt::Int8,
            PinN::P9 => Interrupt::Int9,
            PinN::P10 => Interrupt::Int10,
            PinN::P11 => Interrupt::Int11,
            PinN::P12 => Interrupt::Int12,
            PinN::P13 => Interrupt::Int13,
            PinN::P14 => Interrupt::Int14,
            PinN::P15 => Interrupt::Int15,
        }
    }
}

/// Exti trigger type: trigger on rising/falling or both edges?
#[derive(Debug, Clone, Copy)]
pub enum Edge {
    Rising,
    Falling,
    Both,
}

impl Edge {
    fn config_rising(&self) -> bool {
        match self {
            Edge::Rising | Edge::Both => true,
            Edge::Falling => false,
        }
    }

    fn config_falling(&self) -> bool {
        match self {
            Edge::Falling | Edge::Both => true,
            Edge::Rising => false,
        }
    }
}

/// Async trigger: an external signal that can be awaited
///
/// For example, a rising edge on a GPIO pin
///
/// Triggers can be constructed via the Exti driver, see [`Exti::create_pin_trigger()`](Exti::create_pin_trigger)
pub struct Trigger {
    exti_no: Interrupt,
    waker: &'static AtomicWaker,
}

impl Trigger {
    fn new(exti_no: Interrupt, waker: &'static AtomicWaker) -> Self {
        Self { exti_no, waker }
    }

    /// Check if already ready, and if so, clear the flag and unmask interrupt to register a new change
    fn poll_and_clear_ready(&mut self) -> bool {
        let exti_flag = 1 << self.exti_no as u32;
        let ready = READY.fetch_and(!exti_flag, Ordering::Relaxed) & exti_flag != 0;
        if !ready {
            // Make sure the interrupt is unmasked (without interrupts unmasked, READY wont update!)
            interrupt::free(|_| unsafe {
                modify_reg!(exti, EXTI, IMR1, |reg| { reg | exti_flag });
            })
        }
        ready
    }

    /// Wait for one edge (single-shot trigger)
    ///
    /// With `include_previous=false`, only trigger on the first (next) edge found.
    /// With `include_previous=true`, also trigger immediately if at least one edge had occurred since previous call.
    async fn wait_untill_edge(&mut self, include_previous: bool) {
        let exti_flag = 1 << self.exti_no as u32;

        // Clear the ready flag and unmask interrupt
        let already_triggered = interrupt::free(|_| unsafe {
            modify_reg!(exti, EXTI, IMR1, |reg| { reg | exti_flag });
            READY.fetch_and(!exti_flag, Ordering::Relaxed)
        });

        // A previous edge had already occurred. No need to await a next irq.
        // Note that the interrupt is kept unmasked on purpose: If an edge
        // occurs right now, we want to be able to detect it next time.
        if include_previous && (already_triggered & exti_flag) != 0 {
            return;
        }

        // Future: Pends untill interrupt is pending
        poll_fn(|cx| {
            self.waker.register(cx.waker());

            // if an edge has occurred since last poll, clear it and return ready
            if READY.fetch_and(!exti_flag, Ordering::Relaxed) & exti_flag == 0 {
                //if READY.load(Ordering::Relaxed) & exti_flag == 0 {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Error {
    AlreadyClaimed,
}

/// External interrupt driver: creates async triggers
pub struct Exti {
    peripheral: exti::Instance,
}

const _NEW_WAKER: AtomicWaker = AtomicWaker::new();
static WAKERS: [AtomicWaker; N_EXTI] = [_NEW_WAKER; N_EXTI];
// bitfield: one bit per triggered EXTI line
static READY: AtomicU32 = AtomicU32::new(0);

impl Exti {
    pub fn new(peripheral: exti::Instance) -> Self {
        // Reset the peripheral. As this 'peripheral' cannot be reset via rcc,
        // write all registers to defaults instead.
        reset_reg!(exti, peripheral, EXTI, IMR1);
        reset_reg!(exti, peripheral, EXTI, EMR1);
        reset_reg!(exti, peripheral, EXTI, RTSR1);
        reset_reg!(exti, peripheral, EXTI, FTSR1);
        reset_reg!(exti, peripheral, EXTI, SWIER1);
        reset_reg!(exti, peripheral, EXTI, PR1);
        reset_reg!(exti, peripheral, EXTI, IMR2);
        reset_reg!(exti, peripheral, EXTI, EMR2);
        reset_reg!(exti, peripheral, EXTI, RTSR2);
        reset_reg!(exti, peripheral, EXTI, FTSR2);
        reset_reg!(exti, peripheral, EXTI, SWIER2);
        reset_reg!(exti, peripheral, EXTI, PR2);

        Self { peripheral }
    }

    /// Create a new Trigger, triggered by a GPIO input pin edge
    ///
    /// A few things to consider:
    /// - Each trigger pin is assumed to be in input mode
    ///
    /// - Each trigger pin must have a unique pin number.
    ///     For example, creating triggers for both `PA1` and `PB2` is fine,
    ///     but `PA1` and `PB1` will conflict as they share the same interrupt line.
    ///     The attempt to create the second pin wil fail with `Error::AlreadyClaimed`.
    ///     Note that this is a hardware limitation on STM32.
    ///
    /// - The application should define the relevant interrupt handler(s) that call [Exti::isr()](Exti::isr()).
    ///     There are multiple vectors which depend on the EXTI number (the same as the pin number):
    ///     For example, `EXTI5`..`EXTI9` share `EXTI9_5`, but `EXTI0`..`EXTI4` each have a dedicated vector.
    pub fn create_pin_trigger(&mut self, io: &gpio::Pin, edge: Edge) -> Result<Trigger, Error> {
        let trigger = self.create_trigger(Interrupt::from(&io.pin), edge)?;

        syscfg::configure_exti(io);
        Ok(trigger)
    }

    /// Create a new TriggeredInputPin which acts as a GPIO input pin + edge [Trigger]
    ///
    /// See [create_pin_trigger](Exti::create_pin_trigger) for details.
    /// The difference is that this takes ownership of the io and wraps it together with the trigger in a [TriggeredInputPin],
    /// while [create_pin_trigger](Exti::create_pin_trigger) returns only the trigger.
    pub fn create_triggered_input_pin(
        &mut self,
        io: gpio::Pin,
        edge: Edge,
    ) -> Result<TriggeredInputPin, Error> {
        let trigger = self.create_trigger(Interrupt::from(&io.pin), edge)?;

        syscfg::configure_exti(&io);

        Ok(TriggeredInputPin::new(io, trigger))
    }

    /// Create a trigger for a specified EXTI line. So far we only implement EXTI0-15 = GPIO,
    /// but we could support other sources in the future (for example COMP1/2 outputs)
    pub fn create_trigger(&mut self, interrupt: Interrupt, edge: Edge) -> Result<Trigger, Error> {
        interrupt::free(|_| {
            // Configure this interrupt in the EXTI registers.
            // Note that we don't have to enable any clocks as EXTI is always-on
            // (connected to APB2 bus but has no enable/disable bit)

            let int_flag = 1 << interrupt as u32;

            // Check if an edge is already configured for this interrupt line
            // this can happen if trying to create a trigger for conflicting pins, e.g. PA1 and PB1 (both EXTI1)
            if (read_reg!(exti, self.peripheral, RTSR1) & int_flag) != 0
                || ((read_reg!(exti, self.peripheral, FTSR1) & int_flag) != 0)
            {
                return Err(Error::AlreadyClaimed);
            }

            // Configure rising and/or falling edge triggers
            if edge.config_rising() {
                modify_reg!(exti, self.peripheral, RTSR1, |reg| { reg | int_flag });
            }
            if edge.config_falling() {
                modify_reg!(exti, self.peripheral, FTSR1, |reg| { reg | int_flag });
            }

            // clear pending interrupts if any
            write_reg!(exti, self.peripheral, PR1, int_flag);

            // Interrupt vector
            // (Unmasking the correct vector in NVIC and mapping it to Self::isr() is done by application)
            let _line = match interrupt {
                Interrupt::Int0 => stm32ral::Interrupt::EXTI0,
                Interrupt::Int1 => stm32ral::Interrupt::EXTI1,
                Interrupt::Int2 => stm32ral::Interrupt::EXTI2,
                Interrupt::Int3 => stm32ral::Interrupt::EXTI3,
                Interrupt::Int4 => stm32ral::Interrupt::EXTI4,
                Interrupt::Int5
                | Interrupt::Int6
                | Interrupt::Int7
                | Interrupt::Int8
                | Interrupt::Int9 => stm32ral::Interrupt::EXTI9_5,
                Interrupt::Int10
                | Interrupt::Int11
                | Interrupt::Int12
                | Interrupt::Int13
                | Interrupt::Int14
                | Interrupt::Int15 => stm32ral::Interrupt::EXTI15_10,
                Interrupt::Int16 => stm32ral::Interrupt::PVD_PVM,
                Interrupt::Int18 => stm32ral::Interrupt::RTC_ALARM,
                Interrupt::Int19 => stm32ral::Interrupt::TAMP_STAMP,
            };

            Ok(Trigger::new(interrupt, &WAKERS[interrupt as usize]))
        })
    }

    /// ISR function: must be called from the EXTI Interrupt handler
    ///
    /// ## Safety
    ///
    /// This function is marked unsafe because it must be called from the correct interrupt handler only.
    pub unsafe fn isr() {
        let interrupts = unsafe {
            let pending_ints = read_reg!(exti, EXTI, PR1);

            // Update a shadow 'pending register': set one bit per triggered EXTI line
            let was_ready = READY.fetch_or(pending_ints, Ordering::Relaxed);

            // Only mask interrupts that were still pending since last interrupt.
            // This means an interrupt source is only disabled after triggering twice,
            // which allows `Trigger` to notice it has missed an edge
            // registering two successive wakers.
            modify_reg!(exti, EXTI, IMR1, |reg| { reg & !was_ready });

            // Clear pending interrupt flags
            write_reg!(exti, EXTI, PR1, pending_ints);
            pending_ints
        };

        // Wake waker for each pending bit
        interrupts.each_set(|int| {
            let index = int as usize;
            if index < WAKERS.len() {
                WAKERS[index].wake();
            }
        });
    }
}

impl TriggerTrait for Trigger {
    async fn wait_untill_next_edge(&mut self) {
        self.wait_untill_edge(false).await
    }

    async fn wait_untill_any_edge(&mut self) {
        self.wait_untill_edge(true).await
    }

    fn poll_ready(&mut self) -> bool {
        self.poll_and_clear_ready()
    }
}

/// Triggered Input Pin
///
/// A GPIO pin that can be used as a Trigger at the same time.
/// This allows logic such as:
/// 1. read pin state (e.g. `pin.is_high()`)
/// 2. wait for a state change (e.g. `pin.wait_untill_any_edge().await`)
/// 3. read pin state again (e.g. `pin.is_high()`)
/// which is guaranteed to pick up any state change that happened.
/// wait_untill_any_edge may return early if the state change happened just before/during the first readout,
/// but importantly it never misses a state change.
pub struct TriggeredInputPin {
    pin: gpio::Pin,
    trigger: Trigger,
}
impl TriggeredInputPin {
    pub fn new(pin: gpio::Pin, trigger: Trigger) -> Self {
        Self { pin, trigger }
    }
}

impl Pullup for TriggeredInputPin {
    #[inline]
    fn set_pullup(&mut self, config: gpio::PullupConfig) {
        self.pin.set_pullup(config);
    }
}

impl TriggerTrait for TriggeredInputPin {
    #[inline]
    async fn wait_untill_next_edge(&mut self) {
        self.trigger.wait_untill_next_edge().await
    }

    #[inline]
    async fn wait_untill_any_edge(&mut self) {
        self.trigger.wait_untill_any_edge().await
    }

    #[inline]
    fn poll_ready(&mut self) -> bool {
        self.trigger.poll_ready()
    }
}

impl ErrorType for TriggeredInputPin {
    type Error = Infallible;
}

impl InputPin for TriggeredInputPin {
    #[inline]
    fn is_high(&mut self) -> Result<bool, Self::Error> {
        // Make sure that the next edge can be detected by the Trigger trait.
        // This allows upper level to call `is_high` followed by `wait_untill_any_edge`,
        // without missing the falling edge if it happens in between.
        let _ = self.trigger.poll_and_clear_ready();

        Ok(self.pin.is_high())
    }

    #[inline]
    fn is_low(&mut self) -> Result<bool, Self::Error> {
        // Make sure that the next edge can be detected by the Trigger trait.
        // This allows upper level to call `is_high` followed by `wait_untill_any_edge`,
        // without missing the rising edge if it happens in between.
        let _ = self.trigger.poll_and_clear_ready();

        Ok(self.pin.is_low())
    }
}

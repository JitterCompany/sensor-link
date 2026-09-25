//! The factory self-test: what it checks, what it reports and what it shows.
//!
//! Laid out like our other factory tests: the same operators run them, so the
//! RTT grammar, the verdict machinery and the blink-counting all behave the
//! same way. See `docs/factory-test.md`.

pub mod checks;
pub mod led;
pub mod modem;
pub mod report;
pub mod result;

use core::future::Future;
use hardware::{Pin, PinMode, PinN, PinState, Port, PullupConfig};

use sensor_link_firmware::monotonic_time::{now, FutureTimeout as _};

/// A pin's identity, separate from any configured instance of it.
///
/// The test configures the same physical pin in different ways at different
/// times -- an LED, a strap to probe, an output to park -- so the identity is
/// worth naming once and the mode chosen at the point of use.
#[derive(Clone, Copy)]
pub struct PinId {
    port: Port,
    pin: PinN,
}

impl PinId {
    pub const fn new(port: Port, pin: PinN) -> Self {
        Self { port, pin }
    }

    /// Configure as an output at `state`, and forget it: the register write is
    /// the whole point.
    pub fn park(self, state: PinState) {
        let _ = self.output(state);
    }

    pub fn output(self, state: PinState) -> Pin {
        Pin::new(self.port, self.pin, PinMode::Output(state))
    }

    pub fn input(self) -> Pin {
        Pin::new(self.port, self.pin, PinMode::Input)
    }

    /// Configure as an input held at a defined level, and forget it.
    ///
    /// A parked pin facing an unpowered module must not drive, and must not
    /// float either: a floating input sits near the switching threshold and
    /// burns current in the input buffer.
    pub fn park_input_low(self) {
        let mut pin = self.input();
        pin.set_pullup(PullupConfig::Pulldown);
    }
}

pub use result::current_code;
use result::{Step, ZonePhase};

// ---------------------------------------------------------------- pin map ---
// From the debug-tools repo, SL23-modem-tester/pinmap.toml, generated from the KiCad
// schematic. The LED and strap pins are ex-FMC address lines on v0, where the
// external SRAM only ever *reads* them, so driving them there is harmless and
// the same binary runs on both boards with no branch.
//
// The two UART names are the exception: the modem's TX and RX are crossed
// relative to USART3's default pin functions, which the driver undoes with
// `CR2.SWAP`. So `cellular_uart_tx` is the pin the MCU listens on. The names
// below are the MCU's directions, which is what parking has to get right.

/// `test_led_main_busy`: activity, and never a verdict
pub const MAIN_GREEN: PinId = PinId::new(Port::F, PinN::P15);
/// `test_led_main_fail`: the jig's own fault code
pub const MAIN_RED: PinId = PinId::new(Port::G, PinN::P0);
/// `test_led_modem_ok`. On v0 this reaches the UI board's green LED through
/// J12; on the tester it is the local `Pass` LED beside the mPCIe slot.
pub const MODEM_GREEN: PinId = PinId::new(Port::G, PinN::P12);
/// `test_led_modem_fail`, the matching red
pub const MODEM_RED: PinId = PinId::new(Port::G, PinN::P11);
/// `test_led_sensor_ok` / `_fail`. This firmware tests modems only, so the pair
/// shows no result: it joins the lamp test, and its Fail LED lights steadily
/// on a jig error like every other Fail LED.
pub const SENSOR_GREEN: PinId = PinId::new(Port::F, PinN::P13);
pub const SENSOR_RED: PinId = PinId::new(Port::F, PinN::P14);

/// `park_sram_cs`: the external SRAM's chip select on v0.
const SRAM_CS: PinId = PinId::new(Port::D, PinN::P7);
/// `flash_ss_n`: the SPI flash's chip select, active low
const FLASH_CS: PinId = PinId::new(Port::B, PinN::P0);
/// `cellular_pwr_en`: U6 load switch feeding 3V3_PCIE
const MODEM_PWR_EN: PinId = PinId::new(Port::G, PinN::P6);
/// `cellular_reset_n`: PERST# on the mPCIe slot
const MODEM_RESET: PinId = PinId::new(Port::B, PinN::P12);
/// `cellular_dtr`
const MODEM_DTR: PinId = PinId::new(Port::B, PinN::P15);
/// `cellular_uart_rx` in the pin map -- but USART3 runs with `CR2.SWAP` set, so
/// this is the MCU's *transmit* pin, and the only modem line it ever drives high.
const MODEM_UART_TX: PinId = PinId::new(Port::B, PinN::P11);
/// `cellular_uart_tx` in the pin map; the MCU's receive pin under the same swap.
const MODEM_UART_RX: PinId = PinId::new(Port::B, PinN::P10);
/// `5v5_disable_n`: U2 SiC473 enable, feeding the extension FFC only
const RAIL_5V5_EN: PinId = PinId::new(Port::G, PinN::P14);
/// `sd_en`: SD card power, on the UI board
const SD_EN: PinId = PinId::new(Port::G, PinN::P10);
/// `bat_ce_n`: charger charge-enable, active low
const CHARGER_CE: PinId = PinId::new(Port::C, PinN::P6);

// ------------------------------------------------------------- step runner --

/// What a step returns: pass or fail, and for some steps a value later steps
/// need, which only exists when the step passed.
pub trait Outcome {
    fn passed(&self) -> bool;
    /// The outcome of a step that timed out
    fn timed_out() -> Self;
}

impl Outcome for bool {
    fn passed(&self) -> bool {
        *self
    }
    fn timed_out() -> Self {
        false
    }
}

impl<T> Outcome for Option<T> {
    fn passed(&self) -> bool {
        self.is_some()
    }
    fn timed_out() -> Self {
        None
    }
}

/// Run one step: announce it, bound it with its timeout, record the verdict.
///
/// A failing step never aborts the run, so one capture shows everything that
/// is wrong rather than just the first thing. A step that fails without naming
/// a fault gets its [`Step::default_fault`], so no failure goes without a code.
pub async fn run<O: Outcome>(step: Step, check: impl Future<Output = O>) -> O {
    result::set_current(step);
    result::set_zone_phase(step.zone(), ZonePhase::Running);
    report::step_start(step.code(), step.name());

    let started = now();
    let outcome = match check.with_timeout_ms(step.timeout_ms()).await {
        Some(outcome) => outcome,
        None => {
            report::note(format_args!("timed out after {} ms", step.timeout_ms()));
            O::timed_out()
        }
    };
    let ok = outcome.passed();
    let elapsed_ms = now().micros_since(&started) / 1000;

    report::step_end(step.code(), step.name(), ok, elapsed_ms);
    result::record(step, ok);
    if !ok && result::zone_fault(step.zone()).is_none() {
        result::fail(step.default_fault());
    }
    outcome
}

// ------------------------------------------------------------------- park ---

/// Drive every output to its safe state.
///
/// Called from the deadline backstop and at the end of a normal run, so the
/// board sits inert while the operator reads the LEDs. Builds its pins directly
/// rather than going through driver objects, because neither caller holds them.
/// Not called from the panic handler, which resets instead -- see there for why
/// parking before a reset is useless and a way to hang.
///
/// The LEDs are deliberately not parked: the LED task owns them, and it is still
/// rendering when this runs. A parked indicator would be a lie.
pub fn park_all_outputs() {
    use PinState::{High, Low};

    for (pin, state) in [
        // Modem: rail off, and both control lines low so nothing is left
        // driving an unpowered module.
        (MODEM_PWR_EN, Low),
        (MODEM_RESET, Low),
        (MODEM_DTR, Low),
        // Extension rail off, so the FFC is dead.
        (RAIL_5V5_EN, Low),
        // SD power off (the socket is on the UI board).
        (SD_EN, Low),
        // Chip selects deasserted, so neither memory drives its bus
        (FLASH_CS, High),
        (SRAM_CS, High),
        // Charging off: charge current flows through IBUS and would swamp a
        // module's own draw in every current measurement.
        (CHARGER_CE, High),
    ] {
        pin.park(state);
    }

    // The modem UART, which the loop above cannot make safe by driving low.
    //
    // USART3 idles TX high, push-pull. Left that way over an unpowered module,
    // the module's input protection conducts into its dead 3V3 rail and the pin
    // sources tens of milliamps -- the leak `UART::new` guards against by
    // holding TX open-drain, and `suspend()` restores. But suspend only happens
    // where something still owns the UART. Park runs where nothing does: a step
    // dropped on its deadline, or the run backstop. So make the pins safe from
    // the pin registers alone, without needing the UART that owns them.
    MODEM_UART_TX.park_input_low();
    MODEM_UART_RX.park_input_low();
}

/// One-line summary for the operator's log, before the steps start.
pub fn log_banner(version: hardware::BoardVersion) {
    log::info!(target: "factory", "# Modem factory test -- {}", version.name());
}

//! The indicator scheme: one red/green pair per [`Zone`]. The board zone's pair
//! is `Status` (green `Busy`, red `Error`); each module zone has `Pass`/`Fail`.
//!
//! * Steady red anywhere: the jig failed; `Error` blinks the reason.
//! * `Busy` blinking: the run is still going. When it stops, verdicts are final.
//! * Steady green: that zone passed. `Busy` is never steady, so a green near the
//!   power input can't be read as a pass.
//!
//! Rendered from the atomics in [`super::result`] rather than a channel, so a
//! verdict still appears when the deadline task publishes it over a wedged step.

use hardware::{Pin, PinState};
use sensor_link_firmware::monotonic_time::delay_ms;

use super::{
    result::{self, Phase, Zone, ZonePhase},
    PinId,
};

/// Render granularity. Every pattern below is a whole number of ticks.
const TICK_MS: u32 = 50;
/// Half period of the 1 Hz "busy" blink
const BUSY_HALF: u32 = 500 / TICK_MS;
/// One flash of a blink code: 2 Hz, deliberate enough to count up to six of
/// them without losing place. The gap below is nearly seven times the pause
/// between flashes, so where one group ends is never in question.
const FLASH_ON: u32 = 200 / TICK_MS;
const FLASH_OFF: u32 = 300 / TICK_MS;
/// Dark gap after a code, long enough that counting restarts unambiguously
const CODE_GAP: u32 = 2000 / TICK_MS;
/// All LEDs on at power-up, so a dead LED cannot masquerade as a verdict
const LAMP_TEST_MS: u32 = 400;

/// What one indicator is asked to show, independent of how it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Show {
    /// Nothing yet
    Dark,
    /// Something is running
    Busy,
    /// This zone passed
    Pass,
    /// This zone failed; count the flashes
    Fault(u8),
    /// The jig failed, so this zone's own result means nothing
    Untrustworthy,
}

impl Show {
    /// Resolve to the two lamps at this instant.
    fn lamps(self, tick: u32) -> Lamps {
        match self {
            Show::Dark => Lamps::OFF,
            Show::Busy => Lamps::green(blink(tick, BUSY_HALF)),
            Show::Pass => Lamps::green(true),
            Show::Fault(code) => Lamps::red(code_blink(tick, code)),
            // The one steady red in the whole scheme.
            Show::Untrustworthy => Lamps::red(true),
        }
    }
}

/// The state of one pair at one instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Lamps {
    green: bool,
    red: bool,
}

impl Lamps {
    const OFF: Self = Self {
        green: false,
        red: false,
    };
    const BOTH: Self = Self {
        green: true,
        red: true,
    };

    const fn green(on: bool) -> Self {
        Self {
            green: on,
            red: false,
        }
    }

    const fn red(on: bool) -> Self {
        Self {
            green: false,
            red: on,
        }
    }
}

/// A zone's two LEDs. Both are active high: the MCU drives the anode.
pub struct Pair {
    green: Pin,
    red: Pin,
}

impl Pair {
    /// Both lamps start **on**, continuing the lamp test that board init began
    /// rather than interrupting it. Constructing these dark would blink every
    /// LED off and on again between init and the first render, which reads as a
    /// pattern when it is only a seam.
    pub fn new(green: PinId, red: PinId) -> Self {
        Self {
            green: green.output(PinState::High),
            red: red.output(PinState::High),
        }
    }

    fn show(&mut self, lamps: Lamps) {
        self.green.set_state(state(lamps.green));
        self.red.set_state(state(lamps.red));
    }
}

const fn state(on: bool) -> PinState {
    if on {
        PinState::High
    } else {
        PinState::Low
    }
}

/// Every indicator on the board: one pair per [`Zone`], plus the jig's
/// `Sensor test` pair, which no zone of this firmware uses.
///
/// On v0 only the modem pair is physically fitted; the others drive
/// ex-FMC address lines that reach nothing, which is harmless and needs no
/// board-specific branch.
pub struct Indicators {
    pairs: [Pair; Zone::COUNT],
    /// Takes part in the lamp test and shows the jig error; dark otherwise.
    unused: Pair,
}

impl Indicators {
    pub fn new(pairs: [Pair; Zone::COUNT], unused: Pair) -> Self {
        Self { pairs, unused }
    }

    fn show(&mut self, zone: Zone, lamps: Lamps) {
        self.pairs[zone.index()].show(lamps);
    }

    fn show_all(&mut self, lamps: Lamps) {
        for pair in &mut self.pairs {
            pair.show(lamps);
        }
        self.unused.show(lamps);
    }
}

/// Symmetric blink with the given half period
fn blink(tick: u32, half_period: u32) -> bool {
    (tick % (2 * half_period)) < half_period
}

/// `code` short flashes, then a long dark gap, repeating.
fn code_blink(tick: u32, code: u8) -> bool {
    let flashes = u32::from(code);
    let flash = FLASH_ON + FLASH_OFF;
    let t = tick % (flashes * flash + CODE_GAP);
    t < flashes * flash && (t % flash) < FLASH_ON
}

/// The code a failed zone blinks. A zone can be failed a moment before its code
/// is recorded; it then shows the lowest code rather than a single flash.
fn fault_code(zone: Zone) -> u8 {
    result::zone_fault(zone).unwrap_or(result::MIN_FAULT_CODE)
}

/// What a zone should show right now.
fn show_for(zone: Zone, run_active: bool) -> Show {
    let phase = result::zone_phase(zone);

    if zone == Zone::Board {
        // The jig's own pair. Its green is activity, never a verdict: while the
        // run is going it blinks, and when the run ends it simply goes out.
        return match phase {
            ZonePhase::Fail => Show::Fault(fault_code(zone)),
            _ if run_active => Show::Busy,
            _ => Show::Dark,
        };
    }

    if result::zone_phase(Zone::Board) == ZonePhase::Fail {
        // No module verdict from a jig that failed its own self-test is worth
        // anything, so none is shown. This is also what makes a jig fault
        // visible on v0, where the modem pair is the only one fitted.
        return Show::Untrustworthy;
    }

    match phase {
        ZonePhase::Idle => Show::Dark,
        ZonePhase::Running => Show::Busy,
        ZonePhase::Pass => Show::Pass,
        ZonePhase::Fail => Show::Fault(fault_code(zone)),
    }
}

/// Render the indicators forever.
pub async fn led_task(mut leds: Indicators) -> ! {
    // Lamp test: without it a green LED with a dry joint makes every good module
    // read as fail forever, and nobody suspects the tester. Brief, simultaneous
    // and before any blinking, so it cannot be mistaken for a verdict.
    leds.show_all(Lamps::BOTH);
    delay_ms(LAMP_TEST_MS).await;
    leds.show_all(Lamps::OFF);

    let mut tick = 0u32;
    // Each pair's pattern restarts when what it shows changes, so a blink code
    // always begins with a complete group rather than wherever the cycle was.
    let mut current = [(Show::Dark, 0u32); Zone::COUNT];
    loop {
        let run_active = result::phase() == Phase::Running;
        for zone in Zone::ALL {
            let show = show_for(zone, run_active);
            let (shown, since) = &mut current[zone.index()];
            if *shown != show {
                *shown = show;
                *since = tick;
            }
            leds.show(zone, show.lamps(tick.wrapping_sub(*since)));
        }
        // A jig error lights every Fail LED, including the one no zone uses,
        // so nothing on the board can be read as a result.
        let jig_failed = result::zone_phase(Zone::Board) == ZonePhase::Fail;
        let unused = if jig_failed {
            Show::Untrustworthy
        } else {
            Show::Dark
        };
        leds.unused.show(unused.lamps(tick));
        tick = tick.wrapping_add(1);
        delay_ms(TICK_MS).await;
    }
}

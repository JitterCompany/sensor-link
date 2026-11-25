//! What the two LED pairs of the demo board show.
//!
//! Pure: the application renders it on a fixed tick from the latest states.
//!
//! * Modem pair, green only: dark while the modem is off, 1 Hz while it powers
//!   up and connects, steady once MQTT is connected, and a fast blink for a
//!   while after a `blink` command so a device can be found. Red stays dark.
//! * Status pair (Busy / Error): Busy gives a short heartbeat flash while
//!   measuring and is dark when stopped. Error blinks a code in ErrorRecovery
//!   and Error, and blinks fast during a firmware update.

use super::AppState;

/// Render granularity. Every pattern below is a whole number of ticks.
pub const TICK_MS: u32 = 50;
/// All LEDs on at power-up, so a dead LED cannot pass for a state
pub const LAMP_TEST_MS: u32 = 400;
/// How long the `blink` command keeps the modem LED blinking fast
pub const IDENTIFY_MS: u32 = 10_000;

/// Half period of the 1 Hz "busy" blink
const SLOW_HALF: u32 = 500 / TICK_MS;
/// Half period of the fast blink, well apart from the 1 Hz one
const FAST_HALF: u32 = 100 / TICK_MS;
/// Heartbeat: one short flash every two seconds
const HEARTBEAT_ON: u32 = 100 / TICK_MS;
const HEARTBEAT_PERIOD: u32 = 2_000 / TICK_MS;
/// One flash of a blink code: slow enough to count without losing place
const FLASH_ON: u32 = 200 / TICK_MS;
const FLASH_OFF: u32 = 300 / TICK_MS;
/// Dark gap after a code, long enough that counting restarts unambiguously
const CODE_GAP: u32 = 2_000 / TICK_MS;

/// Error code blinked in ErrorRecovery. Codes start at 2: a single flash
/// with a long gap cannot be told apart from a slow blink.
pub const CODE_ERROR_RECOVERY: u8 = 2;
/// Error code blinked in Error
pub const CODE_ERROR: u8 = 3;

/// The modem as far as the LEDs are concerned
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Modem {
    Off,
    /// Powering up, registering or connecting to the broker
    Connecting,
    /// MQTT connected
    Connected,
}

impl Modem {
    pub const fn from_u8(value: u8) -> Self {
        match value {
            1 => Modem::Connecting,
            2 => Modem::Connected,
            _ => Modem::Off,
        }
    }
}

/// What one LED shows, independent of how it is drawn
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Show {
    Dark,
    Steady,
    /// 1 Hz symmetric blink
    Slow,
    /// 5 Hz symmetric blink
    Fast,
    /// Short flash every two seconds
    Heartbeat,
    /// `n` flashes, then a long gap
    Code(u8),
}

impl Show {
    /// Whether the LED is lit `tick` ticks after this pattern started
    pub fn lit(self, tick: u32) -> bool {
        match self {
            Show::Dark => false,
            Show::Steady => true,
            Show::Slow => blink(tick, SLOW_HALF),
            Show::Fast => blink(tick, FAST_HALF),
            Show::Heartbeat => tick % HEARTBEAT_PERIOD < HEARTBEAT_ON,
            Show::Code(code) => code_blink(tick, code),
        }
    }
}

/// Symmetric blink with the given half period
fn blink(tick: u32, half_period: u32) -> bool {
    (tick % (2 * half_period)) < half_period
}

/// `code` short flashes, then a long dark gap, repeating
fn code_blink(tick: u32, code: u8) -> bool {
    let flashes = u32::from(code);
    let flash = FLASH_ON + FLASH_OFF;
    let t = tick % (flashes * flash + CODE_GAP);
    t < flashes * flash && (t % flash) < FLASH_ON
}

/// The green modem LED. `identify` is set while a `blink` command runs.
pub fn modem_green(modem: Modem, identify: bool) -> Show {
    if identify {
        return Show::Fast;
    }
    match modem {
        Modem::Off => Show::Dark,
        Modem::Connecting => Show::Slow,
        Modem::Connected => Show::Steady,
    }
}

/// The Busy LED of the status pair
pub fn busy(app: AppState) -> Show {
    match app {
        AppState::Active => Show::Heartbeat,
        _ => Show::Dark,
    }
}

/// The Error LED of the status pair
pub fn error(app: AppState) -> Show {
    match app {
        AppState::ErrorRecovery => Show::Code(CODE_ERROR_RECOVERY),
        AppState::Error => Show::Code(CODE_ERROR),
        AppState::FirmwareUpdate => Show::Fast,
        _ => Show::Dark,
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn lit_ticks(show: Show, ticks: u32) -> u32 {
        (0..ticks).filter(|&t| show.lit(t)).count() as u32
    }

    #[test]
    fn modem_led_follows_the_connection() {
        assert_eq!(modem_green(Modem::Off, false), Show::Dark);
        assert_eq!(modem_green(Modem::Connecting, false), Show::Slow);
        assert_eq!(modem_green(Modem::Connected, false), Show::Steady);
        for modem in [Modem::Off, Modem::Connecting, Modem::Connected] {
            assert_eq!(modem_green(modem, true), Show::Fast);
        }
    }

    #[test]
    fn modem_state_roundtrips_through_u8() {
        for modem in [Modem::Off, Modem::Connecting, Modem::Connected] {
            assert_eq!(Modem::from_u8(modem as u8), modem);
        }
    }

    #[test]
    fn busy_only_while_measuring() {
        assert_eq!(busy(AppState::Active), Show::Heartbeat);
        for app in [AppState::Ready, AppState::Boot, AppState::Error] {
            assert_eq!(busy(app), Show::Dark);
        }
    }

    #[test]
    fn error_codes_and_update() {
        assert_eq!(error(AppState::ErrorRecovery), Show::Code(2));
        assert_eq!(error(AppState::Error), Show::Code(3));
        assert_eq!(error(AppState::FirmwareUpdate), Show::Fast);
        assert_eq!(error(AppState::Active), Show::Dark);
    }

    #[test]
    fn patterns_have_the_documented_rates() {
        // One second of ticks
        let second = 1_000 / TICK_MS;
        assert_eq!(lit_ticks(Show::Slow, second), second / 2);
        assert_eq!(lit_ticks(Show::Fast, second), second / 2);
        assert_eq!(lit_ticks(Show::Heartbeat, HEARTBEAT_PERIOD), HEARTBEAT_ON);
    }

    #[test]
    fn code_has_n_flashes_per_group() {
        for code in [CODE_ERROR_RECOVERY, CODE_ERROR] {
            let group = u32::from(code) * (FLASH_ON + FLASH_OFF) + CODE_GAP;
            let mut rising = 0;
            let mut prev = false;
            for t in 0..group {
                let on = Show::Code(code).lit(t);
                if on && !prev {
                    rising += 1;
                }
                prev = on;
            }
            assert_eq!(rising, u32::from(code));
        }
    }
}

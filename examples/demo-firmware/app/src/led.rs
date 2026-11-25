//! Renders the LED patterns of `demo_firmware_core::logic::led`.
//!
//! The inputs are atomics and the latest app state rather than channels the
//! LED task waits on, so a busy or wedged task elsewhere never freezes an
//! indicator: the LED task only ever reads, on its own tick.

use core::sync::atomic::{AtomicU32, AtomicU8, Ordering};

use demo_firmware_core::{
    logic::{
        led::{self, Modem, Show, IDENTIFY_MS, LAMP_TEST_MS, TICK_MS},
        AppState,
    },
    monotonic_time::{delay_ms, now},
};
use hardware::{Leds, Pin};

use crate::AppStateRx;

static MODEM: AtomicU8 = AtomicU8::new(Modem::Off as u8);
/// Monotonic millisecond until which the `blink` command runs; 0 = not running
static IDENTIFY_UNTIL_MS: AtomicU32 = AtomicU32::new(0);

/// Report the modem state, from the network task
pub fn set_modem(modem: Modem) {
    MODEM.store(modem as u8, Ordering::Relaxed);
}

/// Start (or restart) the fast blink that answers a `blink` command
pub fn identify() {
    let until = now_ms().wrapping_add(IDENTIFY_MS).max(1);
    IDENTIFY_UNTIL_MS.store(until, Ordering::Relaxed);
}

fn now_ms() -> u32 {
    (now().micros_since_init() / 1_000) as u32
}

fn identifying() -> bool {
    let until = IDENTIFY_UNTIL_MS.load(Ordering::Relaxed);
    // Wrapping difference: correct for any window far below 49 days
    until != 0 && (until.wrapping_sub(now_ms()) as i32) > 0
}

/// One LED and the pattern it shows, restarted whenever the pattern changes
/// so a blink code always begins with a complete group
struct Lamp {
    pin: Pin,
    show: Show,
    since: u32,
}

impl Lamp {
    fn new(pin: Pin) -> Self {
        Self {
            pin,
            show: Show::Dark,
            since: 0,
        }
    }

    fn render(&mut self, show: Show, tick: u32) {
        if show != self.show {
            self.show = show;
            self.since = tick;
        }
        set(&mut self.pin, show.lit(tick.wrapping_sub(self.since)));
    }
}

fn set(pin: &mut Pin, on: bool) {
    if on {
        pin.set_high();
    } else {
        pin.set_low();
    }
}

/// Render the indicators forever
pub async fn led_task(leds: Leds, app_state_r: &mut AppStateRx) -> ! {
    let Leds {
        mut modem_green,
        mut modem_red,
        mut busy,
        mut error,
    } = leds;

    // Lamp test: board init switched every LED on. Keep them on briefly so a
    // dead LED shows, then start from dark.
    for pin in [&mut modem_green, &mut modem_red, &mut busy, &mut error] {
        pin.set_high();
    }
    delay_ms(LAMP_TEST_MS).await;
    // The red modem LED has no meaning in this firmware
    modem_red.set_low();

    let mut green = Lamp::new(modem_green);
    let mut busy = Lamp::new(busy);
    let mut error = Lamp::new(error);
    let mut app_state = AppState::Boot;
    let mut tick = 0u32;
    loop {
        if let Some(state) = app_state_r.try_recv() {
            app_state = state;
        }
        let modem = Modem::from_u8(MODEM.load(Ordering::Relaxed));
        green.render(led::modem_green(modem, identifying()), tick);
        busy.render(led::busy(app_state), tick);
        error.render(led::error(app_state), tick);

        tick = tick.wrapping_add(1);
        delay_ms(TICK_MS).await;
    }
}

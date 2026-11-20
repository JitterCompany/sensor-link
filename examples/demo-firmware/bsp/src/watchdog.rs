use crate::dbgmcu;

use sensor_link_firmware::traits::Watchdog;
use stm32ral::{iwdg, modify_reg, read_reg};

pub struct IWDG {
    enabled: bool,
    peripheral: iwdg::Instance,
}

// Datasheet: average LSI speed is 31.75 kHz +/- 7%. Let's take +/- 10% to take main clock error into account
const F_NOM_KHZ: f32 = 31.750;
const F_MIN_KHZ: f32 = F_NOM_KHZ / 256.0 * 0.9;
const F_MAX_KHZ: f32 = F_NOM_KHZ / 256.0 * 1.1;

// Feeding watchdog within this range guarantees no reset.
// Going slightly outside the range is a grey area due to tolerances: deviate too far and the watchdog trips.
// Note: the upper limit is capped at ca 30 seconds, setting a higher value will saturate the max downcount value (0xFFF)
pub const WINDOW_RANGE_LOWER_MS: u32 = 23_000;
pub const WINDOW_RANGE_UPPER_MS: u32 = 30_000;

const WINDOW_VALUE: u32 = (((WINDOW_RANGE_LOWER_MS as f32) * F_MIN_KHZ) as u32).saturating_sub(1);
const DOWNCOUNT_VALUE: u32 =
    (((WINDOW_RANGE_UPPER_MS as f32) * F_MAX_KHZ) as u32).saturating_add(1);

impl IWDG {
    pub fn new(peripheral: iwdg::Instance) -> Self {
        Self {
            peripheral,
            enabled: false,
        }
    }

    /// Enable the watchdog. Once enabled, it cannot be disabled except by rebooting
    pub fn enable(&mut self) {
        if !self.enabled {
            // Halt watchdog while debugging. TODO disable this in production for extra robustness?
            dbgmcu::freeze_iwdg();

            // See RM0432 Rev 9 44.3.2
            //
            // 1. Enable IWDG (auto-enables LSI clock)
            self.peripheral.KR.write(0x0000_CCCC);

            // 2. Enable register access
            self.peripheral.KR.write(0x0000_5555);

            // 3. Set prescaler to 256
            while read_reg!(stm32ral::iwdg, self.peripheral, SR, PVU != 0) {}
            modify_reg!(stm32ral::iwdg,  self.peripheral, PR, PR: 0b110 );

            // 4. Set reload register
            while read_reg!(stm32ral::iwdg, self.peripheral, SR, RVU != 0) {}
            modify_reg!(stm32ral::iwdg,  self.peripheral, RLR, RL: DOWNCOUNT_VALUE.min(0xFFF) );

            // 5. wait for registers to be updated
            while read_reg!(stm32ral::iwdg, self.peripheral, SR) != 0 {}

            // 6. write the window value, which auto-refreshes the counter
            modify_reg!(stm32ral::iwdg,  self.peripheral, WINR, WIN: 0xFFF - WINDOW_VALUE.min(0xFFE) );
        }

        self.enabled = true;
    }

    /// Feed the watchdog. Must be done between `WINDOW_RANGE_LOWER_MS` and `WINDOW_RANGE_UPPER_MS`
    /// since the previous feed/enable
    pub fn feed(&mut self) {
        if self.enabled {
            self.peripheral.KR.write(0x0000_AAAA);
        }
    }
}

impl Watchdog for IWDG {
    const WINDOW_MS: core::ops::Range<u32> = WINDOW_RANGE_LOWER_MS..WINDOW_RANGE_UPPER_MS;

    fn feed(&mut self) {
        self.feed()
    }
}

//! Peripheral reset and clock configuration module
//!

use cortex_m::interrupt::free;
use stm32ral::{
    flash, modify_reg,
    rcc::{self, RCC},
    read_reg, reset_reg, write_reg,
};

mod clocks;
pub use clocks::Clocks;

pub struct Rcc {
    rcc: rcc::Instance,
}
impl Rcc {
    pub fn new(rcc: rcc::Instance) -> Self {
        Rcc { rcc }
    }

    pub fn setup(&self) -> Clocks {
        // At power up we're running from the MSI at 4 MHz
        // Wait for it to be ready
        while read_reg!(rcc, self.rcc, CR, MSIRDY != 1) {}

        // start setting up PLL
        disable_rst_peripherals(&self.rcc);

        // We want to run at 72MHz, we have MSI=4MHz
        // VOS is in Range1 by default, so up to 80MHz we can leave it as is.
        // Note: for USB, we want Q=48MHz, the best we can do for main clock = 72MHz.
        // PLLCLK = input / M x N / R
        // PLLQ (PLL48M1CLK) = input / M x N / Q
        // PLLP (PLLSAI3CLK) = input / M x N / P
        // M = 1
        // N = 72
        // VCO = input * N = 288MHz (limits: 64-344MHz)
        // R = 4 -> 72MHz
        // Q = 6 -> 48MHz
        let pllm = 0u32; // divider of 1
        let plln = 72u32; // multiplier of 72
        let pllr = 0b01; // 0b01 == divider of 4
        let pllq = 0b10; // 0b10 == divider of 6
        let pllp = 0u32; // 0 == divider of 7
        let pllpdiv = 0u32;

        // NB: update this if changing variables above!
        let sysclk = 72_000_000;

        // Configure PLL from MSI
        modify_reg!(
            rcc,
            self.rcc,
            PLLCFGR,
            PLLSRC: 0b01, // MSI
            PLLM: pllm,
            PLLR: pllr,
            PLLN: plln,
            PLLP: pllp,
            PLLQ: pllq,
            PLLPDIV: pllpdiv
        );

        // Enable PLL
        modify_reg!(rcc, self.rcc, CR, PLLON: 1);
        // Wait until RDY bit is set
        while read_reg!(rcc, self.rcc, CR, PLLRDY == 1) {}

        // Enable PLL Output
        modify_reg!(rcc, self.rcc, PLLCFGR, PLLREN: 1);

        // Adjust flash wait states. For 80 MHz we need 3 WS (4 CPU cycles)
        unsafe { modify_reg!(flash, flash::FLASH, ACR, LATENCY: 0b11) }

        // Swap system clock to PLL
        modify_reg!(rcc, self.rcc, CFGR, SW: 0b11u32);

        // Wait for system clock to be PLL
        while read_reg!(rcc, self.rcc, CFGR, SWS != 0b11) {}

        if read_reg!(rcc, self.rcc, BDCR, LSERDY == 1) {
            modify_reg!(rcc, self.rcc, CR, MSIPLLEN: 1);
        }

        // // CLOCK OUT
        // // Divide clock out by 8
        // modify_reg!(rcc, self.rcc, CFGR, MCOPRE: 0b011);

        // // SYSCLK to clock-out, enable
        // modify_reg!(rcc, self.rcc, CFGR, MCOSEL: 0b0001);
        clocks::set_sysclock(sysclk);
        Clocks::from_global()
    }
}

/// Reset a specific peripheral
macro_rules! reset_periph {
    ($reg:ident, $field:ident) => {
        paste! {
            modify_reg!(rcc, RCC, $reg, $field: 1);
            while read_reg!(rcc, RCC, $reg, $field != 1) {}
            modify_reg!(rcc, RCC, $reg, $field: 0);
            while read_reg!(rcc, RCC, $reg, $field != 0) {}
        }
    };
}

/// Enable a specific clock
macro_rules! enable_clock {
    ($reg:ident, $field:ident) => {
        paste! {
            // Enable clock & wait for confirmation
            modify_reg!(rcc, RCC, $reg, $field: 1);
            while read_reg!(rcc, RCC, $reg, $field != 1) {}
        }
    };
}

/// Disable a specific clock
macro_rules! disable_clock {
    ($reg:ident, $field:ident) => {
        paste! {
            // Disable clock & wait for confirmation
            modify_reg!(rcc, RCC, $reg, $field: 0);
            while read_reg!(rcc, RCC, $reg, $field != 0) {}
        }
    };
}

/// Disable all peripheral clocks on a bus to defaults, resetting peripherals before disabling them
///
/// Only peripherals that are enabled-by-default are left on
macro_rules! reset_peripherals {
    ($rcc: ident, $bus:tt, $skip:tt) => {
        inner_reset_peripherals!($rcc, $bus, $skip, R)
    };
    ($rcc: ident, $bus:tt, $skip:tt, $suffix:tt) => {
        paste! {
            inner_reset_peripherals!($rcc, $bus, $skip, [<R $suffix>])
        }
    };
}
macro_rules! inner_reset_peripherals {
    ($rcc: ident, $bus:tt, $skip:tt, $suffix:tt) => {
        paste! {
            // Which peripherals were enabled on this bus?
            let enabled = read_reg!(rcc, $rcc, [<$bus EN $suffix>]);

            // Mask off peripherals that are enabled-by-default or should be skipped
            let to_be_disabled = enabled & (!(RCC::reset.[<$bus EN $suffix>] | $skip));
            if to_be_disabled != 0 {
                log::info!(target: "RCC", "Reset & disable {}: 0x{to_be_disabled:08X} (skip {})", stringify!($bus), $skip);

                // Reset peripherals to-be-disabled
                write_reg!(rcc, $rcc, [<$bus RST $suffix>], to_be_disabled);
                while read_reg!(rcc, $rcc, [<$bus RST $suffix>]) != to_be_disabled {}
                write_reg!(rcc, $rcc, [<$bus RST $suffix>], 0);
                while read_reg!(rcc, $rcc, [<$bus RST $suffix>]) != 0 {}
            }

            // Disable the clocks
            reset_reg!(rcc, $rcc, RCC, [<$bus EN $suffix>]);
        }
    };
}

/// Reset all non-essential peripherals and disable their clock
fn disable_rst_peripherals(rcc: &rcc::Instance) {
    // Disable all non-default-enabled peripherals clocks
    // (Each peripheral-to-be-disabled is reset first)
    reset_peripherals!(rcc, AHB1, 0);
    reset_peripherals!(rcc, AHB2, 0);
    reset_peripherals!(rcc, AHB3, 0);
    reset_peripherals!(rcc, APB1, (1 << 10 | 1 << 11), 1); // Cant't reset RTC and WWDG (bit 10,11)
    reset_peripherals!(rcc, APB1, 0, 2);
    reset_peripherals!(rcc, APB2, (1 << 7)); // Can't reset FW (bit 7)

    // Disable PLL
    modify_reg!(rcc, rcc, CR, PLLON: 0);
    // Wait until RDY bit is cleared
    while read_reg!(rcc, rcc, CR, PLLRDY != 0) {}
}

/// Enables the peripheral clock for the USART3 peripheral
/// And resets the peripheral
pub fn enable_rst_usart3() {
    free(|_| {
        unsafe {
            enable_clock!(APB1ENR1, USART3EN);
            reset_periph!(APB1RSTR1, USART3RST);
        };
    });
}

/// Reset UART3 and disable its clock
pub fn disable_rst_usart3() {
    free(|_| unsafe {
        reset_periph!(APB1RSTR1, USART3RST);
        disable_clock!(APB1ENR1, USART3EN);
    });
}

/// Enables the peripheral clock for DMA1 and resets the peripheral
pub fn enable_rst_dma1() {
    free(|_| unsafe {
        enable_clock!(AHB1ENR, DMA1EN);
        reset_periph!(AHB1RSTR, DMA1RST);
    });
}

/// Enables the peripheral clock for DMAMUX1 and resets the peripheral
pub fn enable_rst_dmamux1() {
    free(|_| unsafe {
        enable_clock!(AHB1ENR, DMAMUX1EN);
        reset_periph!(AHB1RSTR, DMAMUX1RST);
    });
}

// Clock-bit-only DMA1/DMAMUX1 gating (toggle `AHB1ENR`, no `AHB1RSTR`):
// retains register state across a gated Sleep. Reset-for-teardown is
// `enable_rst_*`/`disable_rst_*`.

/// Enable the DMA1 clock without resetting the peripheral.
pub fn enable_clk_dma1() {
    free(|_| unsafe {
        enable_clock!(AHB1ENR, DMA1EN);
    });
}

/// Disable the DMA1 clock without resetting the peripheral (state retained).
pub fn disable_clk_dma1() {
    free(|_| unsafe {
        disable_clock!(AHB1ENR, DMA1EN);
    });
}

/// Enable the DMAMUX1 clock without resetting the peripheral.
pub fn enable_clk_dmamux1() {
    free(|_| unsafe {
        enable_clock!(AHB1ENR, DMAMUX1EN);
    });
}

/// Disable the DMAMUX1 clock without resetting the peripheral (state retained).
pub fn disable_clk_dmamux1() {
    free(|_| unsafe {
        disable_clock!(AHB1ENR, DMAMUX1EN);
    });
}

/// Enables the peripheral clock for the TIMER5 peripheral
/// And resets the peripheral
pub fn enable_rst_timer5() {
    free(|_| {
        unsafe {
            enable_clock!(APB1ENR1, TIM5EN);
            reset_periph!(APB1RSTR1, TIM5RST);
        };
    });
}

/// Enable the peripheral clock for the I2C3 peripheral
/// And resets the peripheral
pub fn enable_rst_i2c3() {
    free(|_| {
        unsafe {
            enable_clock!(APB1ENR1, I2C3EN);
            reset_periph!(APB1RSTR1, I2C3RST);
        };
    });
}

/// Reset I2C3 and disable its clock
pub fn disable_rst_i2c3() {
    free(|_| {
        unsafe {
            reset_periph!(APB1RSTR1, I2C3RST);
            disable_clock!(APB1ENR1, I2C3EN);
        };
    });
}

/// Enable the peripheral clock for the ADC peripheral
/// And resets the peripheral
pub fn enable_rst_adc() {
    free(|_| {
        unsafe {
            enable_clock!(AHB2ENR, ADCEN);
            reset_periph!(AHB2RSTR, ADCRST);
        };
    });
}

/// Reset ADC and disable its clock
pub fn disable_rst_adc() {
    free(|_| {
        unsafe {
            reset_periph!(AHB2RSTR, ADCRST);
            disable_clock!(AHB2ENR, ADCEN);
        };
    });
}

/// Enable clock to power interface registers
pub fn enable_pwr_interface() {
    free(|_| unsafe {
        enable_clock!(APB1ENR1, PWREN);
    });
}

/// Disable clock to power interface registers (without resetting)
pub fn disable_pwr_interface() {
    free(|_| {
        unsafe {
            // Note: PWR should not be reset before stopping clock to register interface.
            // (no risk of pending IRQs since this peripheral has none)
            disable_clock!(APB1ENR1, PWREN);
        }
    });
}

/// Enable clock to SYSCFG / COMP / VREFBUF peripherals
pub fn enable_syscfg_comp_vrefbuff() {
    free(|_| unsafe {
        enable_clock!(APB2ENR, SYSCFGEN);
    })
}

/// Disable clock to SYSCFG / COMP / VREFBUF peripherals (without resetting)
pub fn disable_syscfg_comp_vrefbuff() {
    free(|_| unsafe {
        // Note: SYSCFG should not be reset before stopping clock to register interface.
        // (no risk of pending IRQs since this peripheral has none)
        disable_clock!(APB2ENR, SYSCFGEN);
    })
}

/// Enable clock to RTC interface registers
pub fn enable_rtc_interface() {
    free(|_| {
        unsafe {
            // Enable peripheral clock
            enable_clock!(APB1ENR1, RTCAPBEN);
        }
    });
}

#[derive(Debug, Clone)]
pub struct ResetFlags {
    pub watchdog: bool,
    pub software: bool,
    pub por: bool,
}

/// Read the reset flags (and clear them)
///
/// These flags indicate the source of the reset(s) that occurred since the previous check
pub fn read_and_clear_reset_flags() -> ResetFlags {
    free(|_| unsafe {
        let watchdog =
            read_reg!(rcc, RCC, CSR, WWDGRSTF != 0) || read_reg!(rcc, RCC, CSR, IWDGRSTF != 0);
        let software = read_reg!(rcc, RCC, CSR, SFTRSTF != 0);
        let por = read_reg!(rcc, RCC, CSR, BORRSTF != 0);
        modify_reg!(rcc, RCC, CSR, RMVF: 1);

        ResetFlags {
            watchdog,
            software,
            por,
        }
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum RTCClockStatus {
    /// RTC is turned off (not initialized yet or starting)
    Off = 0,

    /// RTC (LSE oscillator) is starting up (should take < 1 second unless something is wrong)
    Starting,

    /// RTC is running normally
    Running,
}
pub fn rtc_clock_status() -> RTCClockStatus {
    let (rtc_en, lse_on, lse_rdy) = unsafe { read_reg!(rcc, RCC, BDCR, RTCEN, LSEON, LSERDY) };

    match (rtc_en, lse_on, lse_rdy) {
        (1, 1, 1) => RTCClockStatus::Running,
        (_, 1, 0) => RTCClockStatus::Starting,
        _ => RTCClockStatus::Off,
    }
}

/// Enable the RTC clock (via LSE crystal oscillator)
///
/// Note that the LSE oscillator may not be running yet when this function returns.
/// Monitor the status via `rtc_clock_status()`. If the LSE takes more than a second
/// to start, it is likely that something is wrong (defect).
pub fn enable_rtc() {
    free(|_| unsafe {
        crate::pwr::unprotect_backup_domain();
        crate::pwr::enable_vbat_charging();
        enable_rtc_interface();

        // Enable clock ready interrupt
        modify_reg!(rcc, RCC, CIER, LSERDYIE: 1);

        // (Unmasking the TAMP_STAMP + RCC vectors in NVIC and mapping them to isr() is done by application)

        match rtc_clock_status() {
            // Clock was already running: nothing to do
            RTCClockStatus::Running => {
                log::info!("RCC: skip enable RTC (already running)");
                return;
            }

            // Clock was off so we need to initialize it
            RTCClockStatus::Off => {
                log::info!("RCC: initialize RTC clock..");
                // Reset backup domain just to be sure. While the RTC is not running,
                // this is no guarantee that it is in a consistent state. For example,
                // a previous boot could have half-initialized the peripheral (has actually happened while debugging)
                modify_reg!(rcc, RCC, BDCR, BDRST: 1);
                modify_reg!(rcc, RCC, BDCR, BDRST: 0);
            }

            // Clock was supposedly starting but it could have failed. Handled the same as if it was off
            RTCClockStatus::Starting => {
                log::info!("RCC: re-initialize RTC clock..");

                // Handled the same as if it was off
                modify_reg!(rcc, RCC, BDCR, BDRST: 1);
                modify_reg!(rcc, RCC, BDCR, BDRST: 0);
            }
        }

        modify_reg!(rcc, RCC, BDCR,
            // switch RTC to LSE 23_768 Hz crystal
            RTCSEL: LSE,

            // Enable LSE with medium drive strength (faster start, less efficient)
            // TODO test best drive strength settings with prototype, see AN2867
            LSEDRV: 2, LSEON: 1
        );

        // Note: LSE crystal is still starting up at this point. As soon as it is ready,
        // the isr() triggers, which will finish enabling the RTC
    });
}

/// ISR function: must be called from the RCC Interrupt handler
///
/// ## Safety
///
/// This function is marked unsafe because it must be called from the correct interrupt handler only.
pub unsafe fn isr() {
    // LSE ready: finalize enabling of RTC peripheral
    if read_reg!(rcc, RCC, CIFR, LSERDYF == 1) {
        write_reg!(rcc, RCC, CICR, LSERDYC: 1);
        free(|_| {
            modify_reg!(rcc, RCC, BDCR,
                // reduce drive strength for lower power (TODO what is correct? verify according to AN2867)
                LSEDRV: 1,

                // Enable RTC
                RTCEN: Enabled
            );

            // MSI running? Might as well set it in MSI-PLL mode (greatly enhances its accuracy)
            if read_reg!(rcc, RCC, CR, MSIRDY == 1) {
                log::info!(target: "RCC", "SET MSIPLLEN");
                modify_reg!(rcc, RCC, CR, MSIPLLEN: 1);
            }
        })
    }
}

use paste::paste;

macro_rules! enable_rst_gpio {
    ($port: tt) => {
        paste! {
            /// Enables and resets the peripheral.
            pub fn [<enable_rst_ $port:lower>] () {
                free(|_| {
                    unsafe {
                        modify_reg!(rcc, RCC, AHB2ENR, [<$port EN>]: 1);
                        modify_reg!(rcc, RCC, AHB2RSTR, [<$port RST>]: 1);
                        modify_reg!(rcc, RCC, AHB2RSTR, [<$port RST>]: 0);
                    }
                })
            }

            /// Returns true if the peripheral is enabled.
            pub fn [<$port:lower _is_enabled>] () -> bool {
                unsafe { read_reg!(stm32ral::rcc, RCC, AHB2ENR, [<$port EN>] == 1) }
            }

        }
    };
}

enable_rst_gpio!(GPIOA);
enable_rst_gpio!(GPIOB);
enable_rst_gpio!(GPIOC);
enable_rst_gpio!(GPIOD);
enable_rst_gpio!(GPIOE);
enable_rst_gpio!(GPIOF);
enable_rst_gpio!(GPIOG);

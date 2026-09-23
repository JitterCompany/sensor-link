#![no_std]
#![allow(async_fn_in_trait)]
//! Board support for the SL23-modem-tester board (STM32L4R7).
//!
//! A trimmed copy of an in-house STM32L4R7 BSP, cut down to what the factory
//! test uses: clocks, GPIO/EXTI, USART3 with DMA, I2C3, the ADC (with VDDA),
//! RTC backup registers, the TIM5 monotonic and the 5V5 power switch.
//! There is one board init, [`Board::init`], and it is not kept in
//! sync with the BSP it was copied from.
//!
//! Supported hardware: the SL23-modem-tester board, and the pin-compatible v0
//! it was developed on. Both have the same `hw_v0_1`/`hw_v0_2` straps; `hw_v1`
//! tells them apart (see [`Straps`]).
//!
//! Drivers are written interrupt based.
//! Interrupt handlers should be placed in the application and should call the correct driver.
// Inspired by https://gist.github.com/peter9477/1c8e6496a99df173ed8aaee6aad9e9a9

mod adc;
pub mod dma;
mod exti;
mod gpio;
pub mod i2c;
pub mod mono;
pub mod power_switch;
mod pwr;
mod rcc;
mod rtc;
mod straps;
mod syscfg;
mod uartasync;

pub use cortex_m;
use cortex_m::interrupt::free;
pub use fugit;
use rtic_device::modify_reg;
use sensor_link_firmware::{
    drivers::boot_stats,
    logic::signal::BootReason,
    monotonic_time::{self, traits::MonotonicTime},
};
use stm32ral::scb;

/// Expose RAL for rtic to use as a device.
/// Note: RTIC will take _all_ peripherals.
pub use stm32ral as rtic_device;

use crate::gpio::OutputSpeed;
pub use crate::{
    adc::{Adc, Measurements},
    exti::{Edge, Exti, Trigger, TriggeredInputPin},
    gpio::{Pin, PinMode, PinN, PinState, Port, PullupConfig},
    i2c::I2C,
    mono::TIMER_HZ,
    power_switch::{GpioPowerSwitch, PowerSwitch, PowerSwitchError},
    rcc::isr as rcc_isr,
    rtc::{BackupRegister, RTC},
    straps::{BoardVersion, Strap, Straps},
    uartasync::*,
};

const MODEM_BAUDRATE: u32 = 3000000;

/// Max time to wait for the 5V5 SIC473 rail's PG to assert after enable.
/// PG rises ~16 ms after EN typ (measured); 50 ms is headroom.
const SIC473_PG_TIMEOUT_MS: u32 = 50;

/// Extra delay after PG rising for 5V5 rail settling.
/// 5V5 reaches 5.50 V within 4 ms after PG rising (measured); 10 ms is margin.
const SIC473_SETTLING_MS: u32 = 10;

/// Every driver the modem test uses, initialised. Move the fields out.
pub struct Board<T> {
    pub straps: Straps,
    pub uart: (UART, UartISR, UartRxDmaISR, UartTxDmaISR),
    pub clocks: rcc::Clocks,
    pub modem_enable: Pin,
    pub modem_reset: Pin,
    pub charger_int: exti::Trigger,
    pub charger_ce: Pin,
    pub rtc: RTC,
    pub i2c: I2C,
    pub board_temperature: Adc<T>,
    /// U2 SiC473 -> +5V5, which feeds only the SL23 extension connector. Nothing
    /// is brought up on that connector; the test only proves the rail itself.
    pub rail_5v5: GpioPowerSwitch<Pin, TriggeredInputPin>,
}

/// Busy wait using Monotonic timer
///
/// Only intended for short delays during init (as busy waiting is a waste of CPU cycles)
fn busy_wait_delay_us(delay_us: u64) {
    let mut delay = monotonic_time::timeout();
    delay.set_us(delay_us);
    while !delay.is_expired() {
        core::hint::spin_loop();
    }
}

fn pin_states<const N: usize>(pins: &mut [Pin; N], pullup: gpio::PullupConfig) -> [PinState; N] {
    for pin in pins.iter_mut() {
        pin.set_pullup(pullup);
    }

    // Give pins some time to change state. 5us should be enough assuming R_pu < 50K and C < 50pF
    busy_wait_delay_us(5);

    pins.each_mut().map(|p| p.get_state())
}

/// Read the board-identity straps.
///
/// Every outcome is reported as read, so a board this firmware does not
/// support fails the boot step's strap check instead of being taken for one
/// it does.
///
/// Called before the SRAM standby block, which drives `hw_v1` (an SRAM address
/// line on v0) low afterwards; that is harmless against its strap to GND.
fn read_straps() -> Straps {
    let hw_v0_1 = Pin::new(Port::A, PinN::P4, PinMode::Input);
    let hw_v0_2 = Pin::new(Port::A, PinN::P15, PinMode::Input);
    let hw_v1 = Pin::new(Port::G, PinN::P1, PinMode::Input);
    let mut pins = [hw_v0_1, hw_v0_2, hw_v1];

    let pulled_up = pin_states(&mut pins, gpio::PullupConfig::Pullup);
    let pulled_down = pin_states(&mut pins, gpio::PullupConfig::Pulldown);

    // set strap pins floating to reduce current consumption
    let _ = pin_states(&mut pins, gpio::PullupConfig::None);

    let level = |i: usize| match (pulled_up[i], pulled_down[i]) {
        (PinState::High, PinState::High) => Strap::High,
        (PinState::Low, PinState::Low) => Strap::Low,
        _ => Strap::Floating,
    };
    Straps {
        hw_v0_1: level(0),
        hw_v0_2: level(1),
        hw_v1: level(2),
    }
}

impl<T: MonotonicTime> Board<T> {
    /// Bring up everything the factory self-test needs, and nothing else.
    ///
    /// Deliberately skips the external SRAM. The jig has none, and on v0 the
    /// FMC would claim PF13/PF14/PF15/PG0 -- which are the jig's indicator
    /// LEDs -- and PG1, which is its board-id strap. Those pins keep the
    /// standby state given to them below instead.
    ///
    /// Peripherals the test does not use (SPI1 flash, SDMMC1) get no driver,
    /// but their pins are put in the same idle state the drivers would leave
    /// them in, so the board draws what it would under the full firmware.
    pub fn init(device: stm32ral::Peripherals) -> Self {
        let rcc = rcc::Rcc::new(device.RCC);
        let clocks = rcc.setup();

        // Init LEDs high, so there is visible feedback that device is on.
        // The test keeps them on as its lamp test.
        let _led_r = Pin::new(Port::G, PinN::P11, PinMode::Output(PinState::High));
        let _led_g = Pin::new(Port::G, PinN::P12, PinMode::Output(PinState::High));

        // Init monotonic timer required by rtic
        mono::Mono::start(device.TIM5, &clocks);

        // No pull up required because QON is pulled high in bq25672.
        let _button = Pin::new(Port::C, PinN::P7, PinMode::Input); // !QON used as input for button (also connected to battery charger)

        let straps = read_straps();

        // The DC-DC converters can be forced to switch ultrasonic (>20KHz) to reduce audible noise.
        let _dcdc_ultrasonic = Pin::new(Port::G, PinN::P15, PinMode::Output(PinState::Low));

        // Battery pack balancing enable
        let _battery_balance = Pin::new(Port::C, PinN::P3, PinMode::Output(PinState::High));

        let modem_enable = Pin::new(Port::G, PinN::P6, PinMode::Output(PinState::Low));
        let modem_reset = Pin::new(Port::B, PinN::P12, PinMode::Output(PinState::Low));

        // Charger pins (STAT is not used)
        let charger_ce = Pin::new(Port::C, PinN::P6, PinMode::Output(PinState::Low));

        // Power-good inputs: PD6 = 5V5 PG (U2), PD3 = 3V3 PG (U3). Neither has
        // an external pullup. PD6 pulls are applied per-state by
        // `GpioPowerSwitch` (see `PgPullStrategy::Dynamic`).
        //
        // PD3 still needs the internal pullup even where nothing reads it
        // here: floating PGOOD costs ~1.1 mA of extra IQ on the SIC473
        // (measured). Do not switch this to Analog mode -- STM32L4 disables
        // PUPDR in Analog and the leak comes back.
        let pwr_5v5_good = Pin::new(Port::D, PinN::P6, PinMode::Input);
        let mut _pwr_3v3_good = Pin::new(Port::D, PinN::P3, PinMode::Input);
        _pwr_3v3_good.set_pullup(gpio::PullupConfig::Pullup);
        let pwr_5v5_enable = Pin::new(Port::G, PinN::P14, PinMode::Output(PinState::Low));

        // SPI flash (SPI1), idle: CLK and MOSI parked low, MISO in its
        // alternate function with a pulldown anchoring it while the flash
        // output is high-Z, and CS (`flash_ss_n`, active low) deasserted so
        // the flash ignores the bus. Nothing after this drives CS low.
        {
            let mut clock = Pin::new(Port::A, PinN::P5, PinMode::Output(PinState::Low));
            let mut mosi = Pin::new(Port::A, PinN::P7, PinMode::Output(PinState::Low));
            let mut miso = Pin::new(Port::A, PinN::P6, PinMode::Alt(5));
            miso.set_pullup(gpio::PullupConfig::Pulldown);
            let mut cs = Pin::new(Port::B, PinN::P0, PinMode::Output(PinState::High));

            clock.set_output_speed(OutputSpeed::VeryHigh);
            mosi.set_output_speed(OutputSpeed::VeryHigh);
            miso.set_output_speed(OutputSpeed::VeryHigh);
            cs.set_output_speed(OutputSpeed::VeryHigh);
        }

        // Debug UART pins in standby
        {
            let _tx = Pin::new(Port::B, PinN::P6, PinMode::Output(PinState::High));
            let mut rx = Pin::new(Port::B, PinN::P7, PinMode::Input);
            rx.set_pullup(gpio::PullupConfig::Pullup);
        }

        // SRAM pins in standby (v0 carries the SRAM; on the jig these are the
        // indicator LEDs, UI lines and the board-id strap, which the test
        // re-claims after init)
        {
            let _nbl0 = Pin::new(Port::E, PinN::P0, PinMode::Output(PinState::High));
            let _nbl1 = Pin::new(Port::E, PinN::P1, PinMode::Output(PinState::High));
            let _oe = Pin::new(Port::D, PinN::P4, PinMode::Output(PinState::High));
            let _we = Pin::new(Port::D, PinN::P5, PinMode::Output(PinState::High));
            // `park_sram_cs`, active low: deselected, so the SRAM ignores every
            // address/control line and never drives the data bus.
            let _cs = Pin::new(Port::D, PinN::P7, PinMode::Output(PinState::High));

            let _a0 = Pin::new(Port::F, PinN::P0, PinMode::Output(PinState::Low));
            let _a1 = Pin::new(Port::F, PinN::P1, PinMode::Output(PinState::Low));
            let _a2 = Pin::new(Port::F, PinN::P2, PinMode::Output(PinState::Low));
            let _a3 = Pin::new(Port::F, PinN::P3, PinMode::Output(PinState::Low));
            let _a4 = Pin::new(Port::F, PinN::P4, PinMode::Output(PinState::Low));
            let _a5 = Pin::new(Port::F, PinN::P5, PinMode::Output(PinState::Low));

            let _a6 = Pin::new(Port::F, PinN::P12, PinMode::Output(PinState::Low));
            let _a7 = Pin::new(Port::F, PinN::P13, PinMode::Output(PinState::Low));
            let _a8 = Pin::new(Port::F, PinN::P14, PinMode::Output(PinState::Low));
            let _a9 = Pin::new(Port::F, PinN::P15, PinMode::Output(PinState::Low));

            let _a10 = Pin::new(Port::G, PinN::P0, PinMode::Output(PinState::Low));
            let _a11 = Pin::new(Port::G, PinN::P1, PinMode::Output(PinState::Low));
            let _a12 = Pin::new(Port::G, PinN::P2, PinMode::Output(PinState::Low));
            let _a13 = Pin::new(Port::G, PinN::P3, PinMode::Output(PinState::Low));
            let _a14 = Pin::new(Port::G, PinN::P4, PinMode::Output(PinState::Low));
            let _a15 = Pin::new(Port::G, PinN::P5, PinMode::Output(PinState::Low));

            let _a16 = Pin::new(Port::D, PinN::P11, PinMode::Output(PinState::Low));
            let _a17 = Pin::new(Port::D, PinN::P12, PinMode::Output(PinState::Low));
            let _a18 = Pin::new(Port::D, PinN::P13, PinMode::Output(PinState::Low));

            let mut data_pins = [
                // D0..=D1
                Pin::new(Port::D, PinN::P14, PinMode::Input),
                Pin::new(Port::D, PinN::P15, PinMode::Input),
                // D2..=D3
                Pin::new(Port::D, PinN::P0, PinMode::Input),
                Pin::new(Port::D, PinN::P1, PinMode::Input),
                // D4..=D12
                Pin::new(Port::E, PinN::P7, PinMode::Input),
                Pin::new(Port::E, PinN::P8, PinMode::Input),
                Pin::new(Port::E, PinN::P9, PinMode::Input),
                Pin::new(Port::E, PinN::P10, PinMode::Input),
                Pin::new(Port::E, PinN::P11, PinMode::Input),
                Pin::new(Port::E, PinN::P12, PinMode::Input),
                Pin::new(Port::E, PinN::P13, PinMode::Input),
                Pin::new(Port::E, PinN::P14, PinMode::Input),
                Pin::new(Port::E, PinN::P15, PinMode::Input),
                // D13..=D15
                Pin::new(Port::D, PinN::P8, PinMode::Input),
                Pin::new(Port::D, PinN::P9, PinMode::Input),
                Pin::new(Port::D, PinN::P10, PinMode::Input),
            ];
            for d in &mut data_pins {
                d.set_pullup(gpio::PullupConfig::Pullup);
            }
        }

        // Enable FPU flush-to-zero mode (FPSCR.FZ bit 24).
        // Denormal f32 results are flushed to zero, avoiding a ~10 cycle penalty per
        // denormal operation in the Cortex-M4F FPU.
        {
            let mut fpscr = cortex_m::register::fpscr::read();
            fpscr.set_fz(true);
            // SAFETY: only FZ changes, before any floating point code runs.
            unsafe { cortex_m::register::fpscr::write(fpscr) };
        }

        let mut exti = Exti::new(device.EXTI);
        let pwr_5v5_good = exti
            .create_triggered_input_pin(pwr_5v5_good, Edge::Both)
            .expect("Failed to create trigger on pwr_5v5_good");

        log::info!(
            "Straps {}, Sysclk: {:?} MHz",
            straps,
            clocks.sysclk().to_MHz()
        );

        // Note: pins are swapped: B10 is normally tx but we swap it inside the peripheral
        let modem_rx = Pin::new(Port::B, PinN::P10, PinMode::Alt(7));
        // TX will be used as output, but we have to be careful not to drive it high
        // before the modem is powered on. Otherwise the IO pin could leak a lot of current (> 50mA)
        // into the modem. That would waste power and might even damage stuff.
        // The modem UART probably won't work in open-drain mode, but the UART driver will switch to push-pull on resume.
        let modem_tx = {
            let mut tx = Pin::new(Port::B, PinN::P11, PinMode::Input);
            tx.set_output_type(crate::gpio::OutputType::OpenDrain);
            tx.mode(PinMode::Alt(7));
            tx
        };

        // Set up DMA1 controller and DMAMUX, split into channel handles.
        // Only CH1/CH2 (the modem UART) are used.
        let dma_channels = dma::Dma1::new(device.DMA1, device.DMAMUX1).split();

        let uart = UART::new(
            device.USART3,
            modem_tx,
            modem_rx,
            MODEM_BAUDRATE,
            dma_channels.ch1,
            dma_channels.ch2,
        );

        let i2c = {
            let scl = Pin::new(Port::G, PinN::P7, PinMode::Alt(4));
            let sda = Pin::new(Port::G, PinN::P8, PinMode::Alt(4));
            I2C::new(device.I2C3, sda, scl, &clocks)
        };

        let charger_int = Pin::new(Port::A, PinN::P8, PinMode::Input);
        let charger_int = exti
            .create_pin_trigger(&charger_int, Edge::Falling)
            .expect("Failed to create trigger on charger_int");

        let board_temperature = {
            let _analog_temp = Pin::new(Port::C, PinN::P0, PinMode::Analog); // ADC: IN1
            adc::Adc::new(device.ADC_Common, device.ADC1)
        };

        let rtc = RTC::new(device.RTC);

        // SD card (SDMMC1, on the UI board), idle: card unpowered, every bus
        // line in its alternate function with a pulldown against leakage.
        {
            let _enable = Pin::new(Port::G, PinN::P10, PinMode::Output(PinState::Low));
            let mut card_detect = Pin::new(Port::G, PinN::P9, PinMode::Input);
            card_detect.set_pullup(gpio::PullupConfig::Pullup);

            let mut bus = [
                Pin::new(Port::C, PinN::P12, PinMode::Alt(12)), // CLK
                Pin::new(Port::D, PinN::P2, PinMode::Alt(12)),  // CMD
                Pin::new(Port::C, PinN::P8, PinMode::Alt(12)),  // D0..=D3
                Pin::new(Port::C, PinN::P9, PinMode::Alt(12)),
                Pin::new(Port::C, PinN::P10, PinMode::Alt(12)),
                Pin::new(Port::C, PinN::P11, PinMode::Alt(12)),
            ];
            for p in &mut bus {
                p.set_output_speed(OutputSpeed::High);
                p.set_pullup(gpio::PullupConfig::Pulldown);
            }
        }

        Board {
            straps,
            uart,
            clocks,
            modem_enable,
            modem_reset,
            charger_int,
            charger_ce,
            rtc,
            i2c,
            board_temperature,
            rail_5v5: GpioPowerSwitch::new(
                pwr_5v5_enable,
                pwr_5v5_good,
                power_switch::Config {
                    settling_delay_ms: SIC473_SETTLING_MS,
                    timeout_ms: SIC473_PG_TIMEOUT_MS,
                    // Nothing is fitted to the rail, so it needs no discharge
                    // time before being switched back on.
                    min_off_ms: 100,
                    // No external pullup on PG: the internal one while the rail
                    // is on, a pulldown while it is off.
                    pg_pulls: power_switch::PgPullStrategy::Dynamic,
                },
            ),
        }
    }
}

/// The STM32L4's 96-bit unique device ID
pub fn device_uid() -> [u32; 3] {
    const UID_BASE: *const u32 = 0x1FFF_7590 as *const u32;
    // Safety: factory-programmed, always readable and never written.
    unsafe {
        [
            UID_BASE.read_volatile(),
            UID_BASE.add(1).read_volatile(),
            UID_BASE.add(2).read_volatile(),
        ]
    }
}

/// Reboot the board
pub fn reboot() -> ! {
    use core::sync::atomic::{compiler_fence, Ordering::SeqCst};
    free(|_| unsafe {
        modify_reg!(scb, SCB, AIRCR, VECTKEYSTAT: 0x05FA, SYSRESETREQ: 1);
    });
    loop {
        compiler_fence(SeqCst);
    }
}

/// Retrieve & update boot stats
///
/// Note: call this once per boot cycle to keep stats accurate
pub fn boot_stats(rtc: &mut RTC) -> boot_stats::Stats {
    let mut regs = rtc.split_backup_registers().unwrap();
    let hw_flags = rcc::read_and_clear_reset_flags();

    if hw_flags.por {
        boot_stats::set_flag(&mut regs[boot_stats::Stat::POR as usize]);
    }
    if hw_flags.watchdog {
        boot_stats::set_flag(&mut regs[boot_stats::Stat::WDT as usize]);
    }
    let fallback_reason = if hw_flags.software {
        BootReason::Software
    } else {
        BootReason::Unknown
    };
    boot_stats::Stats::try_from_registers_or(&mut regs, fallback_reason).unwrap()
}

/// Directly access a register tracking some boot stats
///
/// Safety: make sure no duplicate instances occor, including those given
/// by `RTC::split_backup_registers()`. Otherwise the stats could become corrupted
pub unsafe fn boot_stat_register(stat: boot_stats::Stat) -> BackupRegister {
    let reg = match stat {
        boot_stats::Stat::UptimeTotal => rtc::BackupReg::BACKUP0,
        boot_stats::Stat::UptimeCurrent => rtc::BackupReg::BACKUP1,
        boot_stats::Stat::Boot => rtc::BackupReg::BACKUP2,
        boot_stats::Stat::POR => rtc::BackupReg::BACKUP3,
        boot_stats::Stat::WDT => rtc::BackupReg::BACKUP4,
        boot_stats::Stat::Panic => rtc::BackupReg::BACKUP5,
        boot_stats::Stat::Fault => rtc::BackupReg::BACKUP6,
    };
    BackupRegister::from_reg(reg)
}

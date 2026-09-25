//! Modem factory test, running on the SL23-modem-tester board.
//!
//! The board under test is not this board: the jig verifies LTE modems in its
//! mPCIe slot, and nothing else -- sensor modules on its SL23 connector need
//! sensor-specific tests that do not belong here. A few jigs go to the PCBA
//! factory with this firmware so modules can be checked on arrival, with no
//! network and no engineer present.
//!
//! The same binary runs unmodified on v0, the pin-compatible board it was
//! developed and proven on before any jig existed. Nothing gating branches on
//! which board it is.
//!
//! Operator-facing behaviour is in `docs/factory-test.md`; the indicator scheme
//! is in [`factory::led`] and the RTT grammar in [`factory::report`].

#![no_std]
#![no_main]

use core::fmt::Write as _;
use sensor_link_firmware::{bootloader::common_rtt_logger as rtt_logger, drivers::boot_stats};

use cortex_m_rt::{exception, ExceptionFrame};
use rtic::app;
use sensor_link_firmware::monotonic_time::now;

mod factory;

#[exception]
unsafe fn HardFault(ef: &ExceptionFrame) -> ! {
    panic!("HardFault: {:#?}", ef);
}

#[exception]
unsafe fn DefaultHandler(irqn: i16) {
    panic!("DefaultHandler: unexpected irq {irqn}");
}

/// Record, then reset. The flag survives in a backup register, so the next boot
/// shows board fault 6 instead of re-running into the same panic.
///
/// Outputs are not parked: the reset releases them anyway, and parking touches
/// port G, whose `Pin::new` can spin forever if VDDIO2 is not up yet. The
/// message bypasses the logger, whose lock the interrupted code may hold.
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    critical_section::with(|_| {
        // Safety: interrupts are off and this core is on its way to a reset, so
        // nothing else can touch the backup registers.
        let mut stat = unsafe { hardware::boot_stat_register(boot_stats::Stat::Panic) };
        boot_stats::set_flag(&mut stat);

        // Safety: channel 0 is the one `init` created, and the reset that
        // follows makes any race with the logger moot.
        if let Some(mut channel) = unsafe { rtt_target::UpChannel::conjure(0) } {
            channel.set_mode(rtt_target::ChannelMode::NoBlockTrim);
            let _ = writeln!(
                channel,
                "FACTORY TEST: PANIC at step {}: {}",
                factory::current_code(),
                info
            );
        }
    });
    hardware::reboot()
}

#[app(device = hardware::rtic_device, peripherals = true, dispatchers = [SPI2, CAN1_TX, CAN1_RX0])]
mod app {
    use super::*;
    use factory::{
        checks,
        led::{Indicators, Pair},
        modem, report,
        result::{self, BoardFault, Phase, Step, Zone, ZonePhase},
    };
    use hardware::{mono::Mono, Straps};
    use sensor_link_firmware::{
        drivers::bq25672::BQ25672, logic::signal::BootReason, monotonic_time::delay_ms,
    };

    use checks::{Charger, Rail5V5, TimeoutProvider};

    /// Every step must have reported by this point, or the deadline task calls
    /// the run failed. A verdict the operator can read always exists.
    const RUN_DEADLINE_MS: u32 = 120_000;

    #[shared]
    struct Shared {}

    #[local]
    struct Local {
        straps: Straps,
        sysclk_hz: u32,
        charger: Charger,
        rail_5v5: Rail5V5,
        board_temperature: hardware::Adc<TimeoutProvider>,
        rtc: hardware::RTC,
        modem: modem::ModemIo,
        uart_isr: hardware::UartISR,
        uart_rx_dma_isr: hardware::UartRxDmaISR,
        uart_tx_dma_isr: hardware::UartTxDmaISR,
    }

    #[init]
    fn init(cx: init::Context) -> (Shared, Local) {
        // Control block pinned at 0x2009FF00 by the linker, so a capture tool
        // can be told the address instead of scanning RAM.
        let channels = rtt_target::rtt_init! {
            up: {
                0: {
                    // Holds a whole run (~3 KB), since a capture is often only
                    // attached afterwards; NoBlockTrim drops what does not fit.
                    size: 8192,
                    mode: rtt_target::ChannelMode::NoBlockTrim,
                    name: "Print Output",
                }
            }
            section_cb: ".rtt_cb"
        };
        rtt_logger::init_lock_logger(channels.up.0);

        let hw = hardware::Board::<TimeoutProvider>::init(cx.device);
        factory::log_banner(hw.straps.board_version());

        let (uart, uart_isr, uart_rx_dma_isr, uart_tx_dma_isr) = hw.uart;

        let leds = Indicators::new(
            [
                Pair::new(factory::MAIN_GREEN, factory::MAIN_RED),
                Pair::new(factory::MODEM_GREEN, factory::MODEM_RED),
            ],
            Pair::new(factory::SENSOR_GREEN, factory::SENSOR_RED),
        );

        led::spawn(leds).ok();
        sequence::spawn().ok();
        deadline::spawn().ok();

        (
            Shared {},
            Local {
                straps: hw.straps,
                sysclk_hz: hw.clocks.sysclk().raw(),
                charger: BQ25672::new(hw.i2c, hw.charger_ce, hw.charger_int),
                rail_5v5: hw.rail_5v5,
                board_temperature: hw.board_temperature,
                rtc: hw.rtc,
                modem: modem::ModemIo {
                    uart,
                    enable: hw.modem_enable,
                    reset: hw.modem_reset,
                },
                uart_isr,
                uart_rx_dma_isr,
                uart_tx_dma_isr,
            },
        )
    }

    /// The test itself: every step in order, each bounded by its own timeout.
    #[task(priority = 1, local = [
        straps, sysclk_hz, charger, rail_5v5, board_temperature, rtc, modem,
    ])]
    async fn sequence(cx: sequence::Context) {
        let started = now();
        let local = cx.local;

        // Read once, before any step: this both consumes the flags the previous
        // boot left in the backup registers and tells us whether that boot
        // ended in a panic.
        let stats = hardware::boot_stats(local.rtc);
        if matches!(stats.boot_reason, BootReason::Panic) {
            // Re-running would panic again, reset again, and leave the operator
            // watching a lamp test flicker with no verdict ever appearing. Say
            // what happened and stop; every step reports NOT RUN.
            result::set_current(Step::Boot);
            report::note(format_args!(
                "previous run ended in a panic ({} total); not re-running",
                stats.panic_total
            ));
            result::fail(BoardFault::Firmware);
            result::finish(now().micros_since(&started) / 1000);
            return;
        }

        factory::run(
            Step::Boot,
            checks::boot(*local.straps, *local.sysclk_hz, &stats),
        )
        .await;
        factory::run(Step::Charger, checks::charger(local.charger)).await;
        let ibus_baseline_ma =
            factory::run(Step::Rails, checks::rails(local.charger, local.rail_5v5)).await;
        factory::run(
            Step::Temperature,
            checks::temperature(local.board_temperature),
        )
        .await;

        // A jig that failed its self-test cannot judge a module, and should not
        // power one: skip the modem, whose steps then report NOT RUN.
        match ibus_baseline_ma {
            Some(baseline_ma) if result::zone_phase(Zone::Board) != ZonePhase::Fail => {
                modem_steps(local.modem, local.charger, baseline_ma).await
            }
            _ => report::note(format_args!(
                "jig failed its self-test; modem steps not run"
            )),
        }

        factory::park_all_outputs();
        result::finish(now().micros_since(&started) / 1000);
    }

    /// Steps 5-8. An empty slot or a silent module stops early, so the remaining
    /// steps report NOT RUN rather than piling on failures with one cause.
    async fn modem_steps(modem: &mut modem::ModemIo, charger: &mut Charger, baseline_ma: i32) {
        let powered_on = modem.power_on().await;
        if !factory::run(Step::ModemRail, modem::rail(charger, baseline_ma)).await {
            modem.power_off();
            return;
        }
        if factory::run(Step::ModemAt, modem::at(modem, &powered_on)).await {
            factory::run(Step::ModemSim, modem::sim(modem)).await;
        }
        factory::run(
            Step::ModemCurrent,
            modem::current(modem, charger, baseline_ma),
        )
        .await;
    }

    #[task(priority = 2)]
    async fn led(_cx: led::Context, leds: Indicators) {
        factory::led::led_task(leds).await
    }

    /// Backstop: a hung step must still produce a verdict, blamed on the jig
    /// since an unfinished run says nothing about the module.
    #[task(priority = 3)]
    async fn deadline(_cx: deadline::Context) {
        delay_ms(RUN_DEADLINE_MS).await;
        if result::phase() != Phase::Running {
            return;
        }
        log::error!(target: "factory", "  run deadline of {RUN_DEADLINE_MS} ms expired");
        factory::park_all_outputs();
        result::fail_unreported();
        result::fail(BoardFault::Firmware);
        result::finish(RUN_DEADLINE_MS.into());
    }

    #[task(binds = USART3, local = [uart_isr], priority = 3)]
    fn usart3(cx: usart3::Context) {
        unsafe {
            cx.local.uart_isr.handle_interrupt();
        }
    }

    #[task(binds = DMA1_CH1, local = [uart_rx_dma_isr], priority = 3)]
    fn dma1_ch1(cx: dma1_ch1::Context) {
        unsafe {
            cx.local.uart_rx_dma_isr.handle_interrupt();
        }
    }

    #[task(binds = DMA1_CH2, local = [uart_tx_dma_isr], priority = 3)]
    fn dma1_ch2(cx: dma1_ch2::Context) {
        unsafe {
            cx.local.uart_tx_dma_isr.handle_interrupt();
        }
    }

    /// Finishes enabling the RTC once the LSE is ready. Without it every boot
    /// resets the backup domain, wiping the boot statistics and panic flag.
    #[task(binds = RCC, priority = 3)]
    fn rcc_isr(_cx: rcc_isr::Context) {
        unsafe {
            hardware::rcc_isr();
        }
    }

    /// TAMP_STAMP is EXTI19: RTC tamper, RTC timestamp, and the LSE clock
    /// security system, which is the other way the ready event arrives.
    #[task(binds = TAMP_STAMP, priority = 3)]
    fn exti_19_isr(_cx: exti_19_isr::Context) {
        unsafe {
            hardware::Exti::isr();
            hardware::rcc_isr();
        }
    }

    #[task(binds = EXTI9_5, priority = 3)]
    fn exti9_5(_cx: exti9_5::Context) {
        unsafe {
            hardware::Exti::isr();
        }
    }

    #[task(binds = I2C3_ER, priority = 2)]
    fn i2c3_er(_cx: i2c3_er::Context) {
        unsafe {
            hardware::I2C::isr();
        }
    }

    #[task(binds = I2C3_EV, priority = 2)]
    fn i2c3_ev(_cx: i2c3_ev::Context) {
        unsafe {
            hardware::I2C::isr();
        }
    }

    #[task(binds = ADC1, priority = 2)]
    fn adc1(_cx: adc1::Context) {
        unsafe {
            hardware::Adc::<TimeoutProvider>::isr();
        }
    }

    #[task(binds = TIM5, priority = 7)]
    fn tim5(_cx: tim5::Context) {
        unsafe {
            Mono::interrupt_handler();
        }
    }
}

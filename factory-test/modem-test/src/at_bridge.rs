//! AT bridge for bench work on the SL23-modem-tester: raw AT to the modem in the
//! mPCIe slot over RTT, with control of the slot's power and `~PERST`.
//!
//! Not a factory image. The Busy LED stays on so a bridge-flashed jig is never
//! mistaken for a test in progress.
//!
//! RTT down channel 0 takes lines. A line starting with `!` is a bridge command,
//! anything else goes to the modem with `\r` appended:
//!
//! | Command | Effect |
//! |---|---|
//! | `!on` | `VCC_3V3` on and `~PERST` released, UART resumed |
//! | `!off` | UART suspended, `~PERST` low, `VCC_3V3` off |
//! | `!vcc 0\|1` | the slot supply alone |
//! | `!perst 0\|1` | `~PERST` alone: 0 asserts, 1 releases |
//! | `!baud N` | UART baud rate |
//! | `!ibus MS` | IBUS sample period in ms, 0 stops sampling |
//! | `!probe MS` | probe period of the card's `UART_CTS` in ms, 0 stops |
//!
//! The probe reads `UART_CTS` (pin 23) with a pull-up, then a pull-down. A card
//! whose level shifter runs from the module's IO rail stops driving the pin when
//! the module is off, so the pin follows the pull: `float`. It logs on change.
//!
//! Up channel 0 carries one event per line, `<ms since boot> <tag> <text>`:
//! `<` a line from the modem, `>` a line sent, `I` IBUS and VSYS, `C` the `UART_CTS` probe, `#` the bridge.
//! The host side is `scripts/at-bridge.py`.

#![no_std]
#![no_main]

use core::fmt::Write as _;

use cortex_m_rt::{exception, ExceptionFrame};
use rtic::app;
use sensor_link_firmware::bootloader::common_rtt_logger as rtt_logger;

#[exception]
unsafe fn HardFault(ef: &ExceptionFrame) -> ! {
    panic!("HardFault: {:#?}", ef);
}

#[exception]
unsafe fn DefaultHandler(irqn: i16) {
    panic!("DefaultHandler: unexpected irq {irqn}");
}

/// Print and halt. A bench tool has someone watching, and a reset loop would
/// hide the message.
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    critical_section::with(|_| {
        // Safety: interrupts are off and this core halts below, so a race with
        // the logger cannot matter.
        if let Some(mut channel) = unsafe { rtt_target::UpChannel::conjure(0) } {
            channel.set_mode(rtt_target::ChannelMode::NoBlockTrim);
            let _ = writeln!(channel, "# PANIC: {info}");
        }
    });
    loop {}
}

#[app(device = hardware::rtic_device, peripherals = true, dispatchers = [SPI2, CAN1_TX])]
mod app {
    use embedded_io_async::{Read as _, Write as _};
    use hardware::{Pin, PinMode, PinN, PinState, Port, PullupConfig, UART};
    use sensor_link_firmware::{
        drivers::bq25672::{BatteryCharger as _, BQ25672},
        heapless::Vec,
        monotonic_time::{delay_ms, now, FutureTimeout as _, MonotonicInstant},
        traits::{BaudRateControl as _, Suspend as _},
    };

    use super::rtt_logger;

    type TimeoutProvider = sensor_link_firmware::monotonic_time::Time;
    type Charger = BQ25672<hardware::I2C, Pin, hardware::Trigger, TimeoutProvider>;

    /// The module's power-on default
    const BAUD_DEFAULT: u32 = 115_200;
    /// Longest line kept from the modem or the host. Longer lines are split.
    const LINE_MAX: usize = 256;
    /// How long one pass waits for modem bytes before it looks at the host side
    const RX_SLICE_MS: u32 = 5;
    const IBUS_PERIOD_DEFAULT_MS: u32 = 100;

    #[shared]
    struct Shared {}

    #[local]
    struct Local {
        bridge: Bridge,
        uart_isr: hardware::UartISR,
        uart_rx_dma_isr: hardware::UartRxDmaISR,
        uart_tx_dma_isr: hardware::UartTxDmaISR,
    }

    struct Bridge {
        uart: UART,
        /// `cellular_pwr_en`: U6 load switch feeding 3V3_PCIE
        enable: Pin,
        /// `cellular_reset_n`: `~PERST` on the mPCIe slot, high to release
        perst: Pin,
        /// `cellular_uart_cts`: an output of the card, read with alternating pulls
        cts: Pin,
        charger: Charger,
        host: rtt_target::DownChannel,
        boot: MonotonicInstant,
    }

    #[init]
    fn init(cx: init::Context) -> (Shared, Local) {
        let channels = rtt_target::rtt_init! {
            up: {
                0: {
                    size: 8192,
                    mode: rtt_target::ChannelMode::NoBlockTrim,
                    name: "Print Output",
                }
            }
            down: {
                0: {
                    size: 512,
                    name: "Terminal",
                }
            }
            section_cb: ".rtt_cb"
        };
        rtt_logger::init_lock_logger(channels.up.0);

        let hw = hardware::Board::<TimeoutProvider>::init(cx.device);
        let (uart, uart_isr, uart_rx_dma_isr, uart_tx_dma_isr) = hw.uart;

        // `test_led_main_busy` on: this jig is not running the factory test
        let _busy = Pin::new(Port::F, PinN::P15, PinMode::Output(PinState::High));

        bridge::spawn().ok();

        (
            Shared {},
            Local {
                bridge: Bridge {
                    uart,
                    enable: hw.modem_enable,
                    perst: hw.modem_reset,
                    cts: Pin::new(Port::B, PinN::P13, PinMode::Input),
                    charger: BQ25672::new(hw.i2c, hw.charger_ce, hw.charger_int),
                    host: channels.down.0,
                    boot: now(),
                },
                uart_isr,
                uart_rx_dma_isr,
                uart_tx_dma_isr,
            },
        )
    }

    impl Bridge {
        fn ms(&self) -> f32 {
            now().micros_since(&self.boot) as f32 / 1000.0
        }

        fn event(&self, tag: char, text: core::fmt::Arguments) {
            log::info!(target: "bridge", "{:.1} {tag} {text}", self.ms());
        }

        fn power_on(&mut self) {
            self.enable.set_high();
            self.perst.set_high();
            self.uart.resume();
        }

        fn power_off(&mut self) {
            self.uart.suspend();
            self.perst.set_low();
            self.enable.set_low();
        }

        async fn sample_ibus(&mut self) {
            match self.charger.measure_power().await {
                Ok(m) => self.event(
                    'I',
                    format_args!("ibus_ma={} vsys_mv={}", m.current_bus_ma, m.system_mv),
                ),
                Err(e) => self.event('#', format_args!("ibus read failed: {e:?}")),
            }
        }

        /// `UART_CTS` as `high`, `low`, or `float` when it follows the pull
        async fn probe_cts(&mut self) -> &'static str {
            self.cts.set_pullup(PullupConfig::Pullup);
            delay_ms(1).await;
            let up = self.cts.is_high();
            self.cts.set_pullup(PullupConfig::Pulldown);
            delay_ms(1).await;
            let down = self.cts.is_high();
            match (up, down) {
                (true, true) => "high",
                (false, false) => "low",
                (true, false) => "float",
                (false, true) => "odd",
            }
        }

        /// Handle one `!` command
        fn command(&mut self, line: &str, periods: &mut Periods) {
            let mut words = line.split_whitespace();
            let name = words.next().unwrap_or("");
            let arg: Option<u32> = words.next().and_then(|w| w.parse().ok());
            match (name, arg) {
                ("!on", None) => self.power_on(),
                ("!off", None) => self.power_off(),
                ("!vcc", Some(0)) => self.enable.set_low(),
                ("!vcc", Some(1)) => self.enable.set_high(),
                ("!perst", Some(0)) => self.perst.set_low(),
                ("!perst", Some(1)) => self.perst.set_high(),
                ("!baud", Some(baud)) => self.uart.set_baud_rate(baud),
                ("!ibus", Some(period_ms)) => periods.ibus_ms = period_ms,
                ("!probe", Some(period_ms)) => periods.probe_ms = period_ms,
                _ => {
                    self.event('#', format_args!("unknown command: {line}"));
                    return;
                }
            }
            self.event('#', format_args!("ok {line}"));
        }
    }

    struct Periods {
        ibus_ms: u32,
        probe_ms: u32,
    }

    /// Append `bytes` to `line`, calling `done` for every complete line.
    /// `\r` and `\n` both end a line; empty lines are dropped.
    fn split_lines(line: &mut Vec<u8, LINE_MAX>, bytes: &[u8], mut done: impl FnMut(&[u8])) {
        for &byte in bytes {
            if byte == b'\r' || byte == b'\n' || line.is_full() {
                if !line.is_empty() {
                    done(line);
                }
                line.clear();
                if byte == b'\r' || byte == b'\n' {
                    continue;
                }
            }
            let _ = line.push(byte);
        }
    }

    /// One loop, polled: the UART has no split halves, and a read cut short by
    /// its timeout loses nothing, because reception runs on circular DMA.
    #[task(priority = 1, local = [bridge])]
    async fn bridge(cx: bridge::Context) {
        let b = cx.local.bridge;

        if let Err(e) = b.charger.configure().await {
            b.event(
                '#',
                format_args!("charger configure failed, no IBUS: {e:?}"),
            );
        }
        b.uart.set_baud_rate(BAUD_DEFAULT);
        b.event(
            '#',
            format_args!(
                "AT bridge ready, slot off. uid={:08x?}",
                hardware::device_uid()
            ),
        );

        let mut periods = Periods {
            ibus_ms: IBUS_PERIOD_DEFAULT_MS,
            probe_ms: 0,
        };
        let mut last_ibus = now();
        let mut last_probe = now();
        let mut cts_level = "";
        let mut modem_line: Vec<u8, LINE_MAX> = Vec::new();
        let mut host_line: Vec<u8, LINE_MAX> = Vec::new();
        let mut chunk = [0_u8; 64];

        loop {
            if b.uart.is_suspended() {
                delay_ms(RX_SLICE_MS).await;
            } else if let Some(Ok(n)) = b.uart.read(&mut chunk).with_timeout_ms(RX_SLICE_MS).await {
                split_lines(&mut modem_line, &chunk[..n], |line| {
                    b.event('<', format_args!("{}", Printable(line)))
                });
            }

            let n = b.host.read(&mut chunk);
            let mut lines: Vec<Vec<u8, LINE_MAX>, 4> = Vec::new();
            split_lines(&mut host_line, &chunk[..n], |line| {
                let _ = lines.push(Vec::from_slice(line).unwrap_or_default());
            });
            for line in lines {
                let text = core::str::from_utf8(&line).unwrap_or("");
                if text.starts_with('!') {
                    b.command(text, &mut periods);
                } else if b.uart.is_suspended() {
                    b.event('#', format_args!("slot is off, not sent: {text}"));
                } else {
                    b.event('>', format_args!("{text}"));
                    let _ = b.uart.write(&line).await;
                    let _ = b.uart.write(b"\r").await;
                }
            }

            if periods.ibus_ms > 0
                && now().micros_since(&last_ibus) >= u64::from(periods.ibus_ms) * 1000
            {
                last_ibus = now();
                b.sample_ibus().await;
            }

            if periods.probe_ms > 0
                && now().micros_since(&last_probe) >= u64::from(periods.probe_ms) * 1000
            {
                last_probe = now();
                let level = b.probe_cts().await;
                if level != cts_level {
                    b.event('C', format_args!("cts={level}"));
                    cts_level = level;
                }
            }
        }
    }

    /// Modem bytes as text, with anything unprintable escaped
    struct Printable<'a>(&'a [u8]);

    impl core::fmt::Display for Printable<'_> {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            for &byte in self.0 {
                if byte.is_ascii_graphic() || byte == b' ' {
                    core::fmt::Write::write_char(f, byte as char)?;
                } else {
                    write!(f, "\\x{byte:02x}")?;
                }
            }
            Ok(())
        }
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

    #[task(binds = RCC, priority = 3)]
    fn rcc_isr(_cx: rcc_isr::Context) {
        unsafe {
            hardware::rcc_isr();
        }
    }

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
            hardware::mono::Mono::interrupt_handler();
        }
    }
}

use core::{future::poll_fn, marker::PhantomData, task::Poll};

use cortex_m::interrupt;
use embassy_sync::waitqueue::AtomicWaker;
use sensor_link_firmware::monotonic_time::{delay_us, traits::MonotonicTime};
use stm32ral::{adc1, adc_common, modify_reg, read_reg, write_reg};

use crate::rcc::{self, Clocks};

/// ADC peripheral driver
pub struct Adc<T> {
    _timeout: PhantomData<T>,
    periph_common: adc_common::Instance,
    periph_adc1: adc1::Instance,
    channels: [ChannelConfig; 2],
}

static WAKER: AtomicWaker = AtomicWaker::new();

#[derive(Debug, Clone)]
pub enum Error {
    /// ADC calibration failed
    Calibration,

    /// ADC channel(s) misconfigured
    ChannelConfig,
}

#[derive(Debug, Clone, PartialEq)]
enum Event {
    /// ADC is ready to start measuring (ADRDY=1)
    ADReady,

    /// A conversion result is ready
    EndOfConversion,

    /// The sequence is complete (no further conversions)
    EndOfSequence,
}

#[allow(unused)]
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum Channel {
    CH0 = 0,
    CH1,
    CH2,
    CH3,
    CH4,
    CH5,
    CH6,
    CH7,
    CH8,
    CH9,
    CH10,
    CH11,
    CH12,
    CH13,
    CH14,
    CH15,
    CH16,
}

/// No more than 16 channels can be configured (in one regular sequence)
const MAX_N_CHANNELS: usize = 16;

/// Sample time in ADC clock cycles
#[derive(Debug, Clone, Copy, Default)]
#[repr(u16)]
enum SampleTime {
    #[default]
    T2_5 = 0,
    T6_5 = 1,
    T12_5 = 2,
    T24_5 = 3,
    T47_5 = 4,
    T92_5 = 5,
    T247_5 = 6,
    T640_5 = 7,
}

impl SampleTime {
    /// Try to get a sample time that is at least as long as the requested time
    pub fn from_ticks(ticks: u32) -> Self {
        match ticks {
            0..=2 => Self::T2_5,
            3..=6 => Self::T6_5,
            7..=12 => Self::T12_5,
            13..=24 => Self::T24_5,
            25..=47 => Self::T47_5,
            48..=92 => Self::T92_5,
            93..=247 => Self::T247_5,
            _ => Self::T640_5,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChannelConfig {
    channel: Channel,
    sample_time: SampleTime,
}

impl ChannelConfig {
    pub fn new(channel: Channel) -> Self {
        Self {
            channel,
            sample_time: SampleTime::default(),
        }
    }
    pub fn with_sample_time_ns(mut self, spl_time_ns: u32) -> Self {
        let t_adc_ns = 1_000_000_000 / f_adc_clock_hz();
        let adc_ticks = spl_time_ns / t_adc_ns;

        self.sample_time = SampleTime::from_ticks(adc_ticks);
        self
    }
}

#[derive(Debug, Clone)]
pub struct Measurements {
    /// Temperature as measured on the main board
    pub board_temp_millicelsius: i32,

    /// Analog supply, derived from the internal reference and its factory
    /// calibration. This is the 3V3 rail as the MCU actually sees it, which is
    /// otherwise invisible: every other reading on this ADC is relative to it.
    pub vdda_millivolts: u16,

    /// Raw internal-reference reading and the factory calibration it is
    /// compared against, so a suspect `vdda_millivolts` can be diagnosed from a
    /// log without a debugger.
    pub vrefint_raw: u16,
    pub vrefint_cal: u16,
}

/// Factory calibration of the internal reference, measured at VDDA = 3.0 V and
/// 30 C (RM0432 / DS12023). 12-bit, so it is directly comparable to a reading.
const VREFINT_CAL: *const u16 = 0x1FFF_75AA as *const u16;
/// The VDDA the calibration was taken at
const VREFINT_CAL_VDDA_MV: u32 = 3_000;

/// divides from hclk. valid range: 1-4. Choose 4 because we don't need fast ADC
const ADC_CLK_DIV: u32 = 4;

impl<T> Adc<T>
where
    T: MonotonicTime,
{
    pub fn new(periph_common: adc_common::Instance, periph_adc1: adc1::Instance) -> Self {
        Self {
            _timeout: PhantomData,
            periph_common,
            periph_adc1,
            channels: [
                // Internal reference, for VDDA
                ChannelConfig::new(Channel::CH0).with_sample_time_ns(100_000),
                // Temp sensor: 500ns should be plenty (DS12023 table 82: 12bit Rain < 2200 ohm, temp sensor impedance is < 1000)
                ChannelConfig::new(Channel::CH1).with_sample_time_ns(500),
            ],
        }
    }

    async fn enable(&mut self) {
        rcc::enable_rst_adc();

        // ADC1/2 common: enable clock divided from AHB clock
        modify_reg!(adc_common, self.periph_common, CCR,
                CH18SEL: 0,
                CH17SEL: 0,
                VREFEN: 1,
                PRESC: 0,  // don't use the separate clock domain (keep it simple)
                CKMODE: (ADC_CLK_DIV-1),
                MDMA: 0,
                DUAL: 0);

        // Power up ADC1 peripheral + voltage regulator, but keep ADEN off (required during config)
        modify_reg!(adc1, self.periph_adc1, CR, DEEPPWD: 0, ADVREGEN: 1, ADEN: 0);

        // wait for t_ADCVREG_STUP <= 20us (DS12023 table 81)
        delay_us(20).await;

        // (Unmasking ADC1 interrupt in NVIC is done by application)
    }

    fn disable(&mut self) {
        // disable: set ADEN=0 via ADDIS
        modify_reg!(adc1, self.periph_adc1, CR, ADDIS: 1);
        while read_reg!(adc1, self.periph_adc1, CR, ADDIS == 1) {}

        // Power down ADC1 peripheral + voltage regulator
        modify_reg!(adc1, self.periph_adc1, CR, DEEPPWD: 1, ADVREGEN: 0, ADEN: 0);

        // todo disable rcc, check  if the procedure above is correct
        rcc::disable_rst_adc();
    }

    async fn calibrate(&mut self) -> Result<(), Error> {
        // Start calibration
        modify_reg!(adc1, self.periph_adc1, CR, ADCAL: 1, ADCALDIF: 0);

        let f_adc = f_adc_clock_hz();

        // Calibration should take just 116 fADC cycles
        let cal_time_us = 1 + (116 * 1_000_000) / f_adc;

        // ...but we can't just wait for ADCAL=0,
        // as (RM0432 21.4.9) states that we have to wait for ADCAL=0 + 4 cycles(!)
        // before the peripheral is actually ready. To be safe, we wait 2 times the expected 116 cycles
        // and verify that the calibration is indeed complete afterwards.
        delay_us(2 * cal_time_us as u64).await;
        if read_reg!(adc1, self.periph_adc1, CR, ADCAL == 0) {
            Ok(())
        } else {
            Err(Error::Calibration)
        }
    }

    /// Await a specific ADC Event
    async fn untill(&mut self, event: Event) -> Result<(), Error> {
        poll_fn(|cx| {
            WAKER.register(cx.waker());
            // Return if the even has already occurred
            match event {
                Event::ADReady => {
                    if read_reg!(adc1, self.periph_adc1, ISR, ADRDY == 1) {
                        write_reg!(adc1, self.periph_adc1, ISR, ADRDY: 1);
                        return Poll::Ready(Ok(()));
                    }
                }
                Event::EndOfConversion => {
                    if read_reg!(adc1, self.periph_adc1, ISR, EOC == 1) {
                        write_reg!(adc1, self.periph_adc1, ISR, EOC: 1);
                        return Poll::Ready(Ok(()));
                    }
                }
                Event::EndOfSequence => {
                    if read_reg!(adc1, self.periph_adc1, ISR, EOS == 1) {
                        write_reg!(adc1, self.periph_adc1, ISR, EOS: 1);
                        return Poll::Ready(Ok(()));
                    }
                }
            }

            // Setup IRQ to wake us when a relevant event hapens
            interrupt::free(|_| {
                modify_reg!(adc1, self.periph_adc1, IER,
                    ADRDYIE: u32::from(event == Event::ADReady),
                    EOSIE: u32::from(event == Event::EndOfSequence),
                    EOCIE: u32::from(event == Event::EndOfConversion)
                )
            });
            Poll::Pending
        })
        .await
    }

    /// Measure all analog input (single-shot measurement)
    ///
    /// This automatically enables + disables the peripheral to save power
    pub async fn measure(&mut self) -> Result<Measurements, Error> {
        // Enable the peripheral. Note: this function should not return early before re-disabling
        self.enable().await;

        let mut values_12b = [0_u16; 2];
        let result = self.inner_measure(&mut values_12b).await;

        // Disable peripheral: returning is allowed after this point
        self.disable();
        result?;

        // MCP9701A conversion: 400mV at 0 celsius, 19.5 mV/degree
        // calculation below is in units of 0.5 millivolt ('halfmv') which keeps more resolution
        // and makes scaling easier (19.5 mv = 39 'half millivolts')
        let offset_zero_celsius_halfmv = 400 * 2;
        let halfmv = (values_12b[1] as i32 * 3300 * 2) / 4095 - offset_zero_celsius_halfmv;
        let board_temp_millicelsius = (1000 * halfmv) / 39;

        // VDDA from the internal reference: the reference is a fixed voltage, so
        // its reading scales inversely with the supply the ADC measures against.
        // A zero reading would mean the reference never converted, so report 0
        // rather than dividing by it.
        let vrefint = values_12b[0] as u32;
        let vdda_millivolts = match vrefint {
            0 => 0,
            raw => (VREFINT_CAL_VDDA_MV * unsafe { VREFINT_CAL.read_volatile() } as u32 / raw)
                .min(u16::MAX as u32) as u16,
        };

        Ok(Measurements {
            board_temp_millicelsius,
            vdda_millivolts,
            vrefint_raw: values_12b[0],
            vrefint_cal: unsafe { VREFINT_CAL.read_volatile() },
        })
    }

    /// Measure all configured ADC channels once (in sequence).
    /// This assumes the peripheral is enabled.
    async fn inner_measure(&mut self, results: &mut [u16]) -> Result<(), Error> {
        self.calibrate().await?;

        // note: this driver only supports single ended inputs for now.
        // This is also the default setting for the `DIFSEL` register.

        write_reg!(adc1, self.periph_adc1, ISR, ADRDY: 1);
        modify_reg!(adc1, self.periph_adc1, CR, ADEN: 1);

        self.untill(Event::ADReady).await?;

        modify_reg!(adc1, self.periph_adc1, CFGR,
            AUTDLY: 1, // delay next conversion untill data was read
            CONT: 0, // single conversion mode
            RES: 0 // 12-bit resolution
        );

        let n_channels = self.channels.len();

        // Channel count mismatch: config error
        if n_channels > MAX_N_CHANNELS || n_channels != results.len() {
            return Err(Error::ChannelConfig);
        }

        // No channels configured: nothing to do
        if n_channels == 0 {
            return Ok(());
        }

        // Configure channel sampling sequence
        for (seq_no, ch) in self.channels.iter().enumerate() {
            self.configure_sample_timing(&ch);
            self.configure_sequence(seq_no, &ch)?;
        }
        modify_reg!(adc1, self.periph_adc1, SQR1, L: n_channels as u32);

        // Start conversion sequence. Note that ADSTART stays at 1 during the conversion
        // and that most registers cannot be changed while it is 1
        modify_reg!(adc1, self.periph_adc1, CR, ADSTART: 1);

        for result in results {
            self.untill(Event::EndOfConversion).await?;
            *result = (read_reg!(adc1, self.periph_adc1, DR) & 0xFFF) as u16;
        }

        self.untill(Event::EndOfSequence).await
    }

    /// Configure the sample time for a channel
    ///
    /// This defines how long the ADC should ideally wait for an input
    /// to settle before starting the sampling.
    fn configure_sample_timing(&self, ch: &ChannelConfig) {
        let ch_no = ch.channel as u32;

        let smp = ch.sample_time as u32;
        let mask = 0b111;
        // SMPR1: Channels 0..=9
        if ch_no <= 9 {
            let shift = ch_no * 3;
            modify_reg!(adc1, self.periph_adc1, SMPR1, |reg| {
                let reg = reg & !(mask << shift);
                reg | (smp & mask) << shift
            })
        // SMPR2: Channels 10..=18
        } else {
            let shift = (ch_no - 10) * 3;
            modify_reg!(adc1, self.periph_adc1, SMPR2, |reg| {
                let reg = reg & !(mask << shift);
                reg | (smp & mask) << shift
            })
        }
    }

    /// Configure a channel to be sampled at the given index in a conversion sequence
    fn configure_sequence(&self, index: usize, ch: &ChannelConfig) -> Result<(), Error> {
        // registers are 1-based: SQ1..=SQ16
        let seq_no = index + 1;

        // helper to modify the sqr1..4 registers
        fn mod_sqr(reg: u32, shift: u32, value: u32) -> u32 {
            let reg: u32 = reg & !(0b11111 << shift);
            reg | (value & 0b11111) << shift
        }

        let channel_no = ch.channel as u32;
        match seq_no {
            1..=4 => {
                let shift = (seq_no * 6) as u32;
                modify_reg!(adc1, self.periph_adc1, SQR1, |reg| mod_sqr(
                    reg, shift, channel_no
                ));
            }
            5..=9 => {
                let shift = ((seq_no - 5) * 6) as u32;
                modify_reg!(adc1, self.periph_adc1, SQR2, |reg| mod_sqr(
                    reg, shift, channel_no
                ));
            }
            10..=14 => {
                let shift = ((seq_no - 10) * 6) as u32;
                modify_reg!(adc1, self.periph_adc1, SQR3, |reg| mod_sqr(
                    reg, shift, channel_no
                ));
            }
            15..=16 => {
                let shift = ((seq_no - 15) * 6) as u32;
                modify_reg!(adc1, self.periph_adc1, SQR4, |reg| mod_sqr(
                    reg, shift, channel_no
                ));
            }

            // should not happen (driver bug)
            _ => return Err(Error::ChannelConfig),
        }
        Ok(())
    }

    /// ISR function: must be called from the ADC1 Interrupt handler
    ///
    /// ## Safety
    ///
    /// This function is marked unsafe because it must be called from the correct interrupt handler only.
    pub unsafe fn isr() {
        // Disable interrupts and wake the waker.
        // The waker will re-enable interrupts if it wants to await them
        interrupt::free(|_| unsafe {
            modify_reg!(adc1, ADC1, IER,
                ADRDYIE: 0,
                EOSIE: 0,
                EOCIE: 0
            )
        });
        WAKER.wake();
    }
}

fn f_adc_clock_hz() -> u32 {
    Clocks::from_global().hclk().raw() / ADC_CLK_DIV
}

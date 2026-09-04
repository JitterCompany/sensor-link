//! RTD driver for external RTD-style sensors (PT100 or PT1000)

mod pt100;
mod pt1000;

use embedded_hal::digital::OutputPin;
use embedded_hal_async::spi;

use crate::{
    drivers::ads124s08::{
        self, AnalogPin, BurnoutSource, InputConfig, RefConfig, RefStatus, ADS124S08, FULL_SCALE,
    },
    traits::{self, Trigger},
};

/// Anti-alias filter: series resistance on ADC input [Ohm]
const INPUT_FILTER_R: f32 = 3.92e3;

/// Anti-alias filter: differential input capacitor at ADC input [F]
#[allow(dead_code)]
const INPUT_FILTER_C_DIFF: f32 = 2.2e-9 + INPUT_FILTER_C_COMMON / 2.0;

/// Common-mode filter capacitor on each input pin [F]
#[allow(dead_code)]
const INPUT_FILTER_C_COMMON: f32 = 220e-12;

/// Input reference resistance [Ohm]
///
/// Temperature input is ratiometric to this reference resistance (i.e. full-scale with PGA=1 means 2kOhm)
const INPUT_REFERENCE_R: f32 = 2_000.0;

/// Read out an RTD-style temperature sensor
///
/// Currently only supports 4-wire PT100/PT1000 sensors,
/// but could be extended to support 3-wire sensors too.
pub struct RTD {
    pin_map: PinMap,
    ch_state: ChannelState,
}

#[derive(Debug)]
pub enum Error {
    ADC(ads124s08::Error),
}

impl From<ads124s08::Error> for Error {
    fn from(value: ads124s08::Error) -> Self {
        Self::ADC(value)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ReadError {
    /// ADC issue: retry is not meaningful. ADC driver must be reset or hardware problem.
    ADC(ads124s08::Error),

    /// Readout-related: retry later may succeed
    Read(SensorReadError),
}

impl From<SensorReadError> for ReadError {
    fn from(value: SensorReadError) -> Self {
        Self::Read(value)
    }
}

impl From<ads124s08::Error> for ReadError {
    fn from(value: ads124s08::Error) -> Self {
        Self::ADC(value)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum SensorReadError {
    CRCMismatch,
    Overrange,
    Disconnected,
    Fault(ChannelFault),
}

/// Miswired, detect or unsupported sensor type
#[derive(Debug, Clone, Copy)]
pub enum ChannelFault {
    /// Unexpected low resistance: short circuit?
    ShortCircuit(i32),

    /// Unexpected resistance: bad connection or unsupported sensor type
    UnexpectedResistance(i32),

    /// Unexpected high resistance: no connection or unsupported sensor type
    HighResistance(i32),
}

#[derive(Debug, Clone, Copy, Default)]
pub enum ChannelState {
    // Disconnected
    #[default]
    Disconnected,

    // Fault: miswired, defect or unsupported sensor
    Fault(ChannelFault),

    // Connected, with a supported sensor type
    Connected(SupportedSensor),
}

#[derive(Debug, Clone, Copy)]
pub enum RangeError {
    OutOfRange,
}

/// Supported sensor types
#[derive(Debug, Clone, Copy)]
pub enum SupportedSensor {
    /// PT100 sensor: assuming a DIN EN 60751 class A sensor. Supported range: -50°C..250°C (R=80..194 Ohms)
    PT100,

    /// PT1000 sensor: assuming a DIN EN 60751 class A sensor. Supported range: -50°C..250°C (R=800..1941 Ohms)
    PT1000,
}

impl SupportedSensor {
    pub const fn resistance_range(&self) -> core::ops::RangeInclusive<f32> {
        match self {
            SupportedSensor::PT100 => pt100::R_MIN..=pt100::R_MAX,
            SupportedSensor::PT1000 => pt1000::R_MIN..=pt1000::R_MAX,
        }
    }

    pub const fn convert_celsius(&self, resistance: f32) -> Result<f32, RangeError> {
        let range = &self.resistance_range();
        if resistance < *range.start() || resistance > *range.end() {
            return Err(RangeError::OutOfRange);
        }
        Ok(match self {
            SupportedSensor::PT100 => pt100::celsius_from_resistance(resistance),
            SupportedSensor::PT1000 => pt1000::celsius_from_resistance(resistance),
        })
    }

    /// Try to autodetect the sensor type from an (approximate) resistance measurement (Ohm))
    pub fn try_from_resistance(resistance: i32) -> Result<Self, ChannelFault> {
        let resistance_f32 = resistance as f32;
        // Valid PT100
        if SupportedSensor::PT100
            .resistance_range()
            .contains(&resistance_f32)
        {
            log::debug!(target: "rtd::SupportedSensor", "detected PT100 (R={resistance} Ohm)");
            Ok(SupportedSensor::PT100)

        // Valid PT1000
        } else if SupportedSensor::PT1000
            .resistance_range()
            .contains(&resistance_f32)
        {
            log::debug!(target: "rtd::SupportedSensor", "detected PT1000 (R={resistance} Ohm)");
            Ok(SupportedSensor::PT1000)

        // Unsupported sensor
        } else if resistance_f32 < *SupportedSensor::PT100.resistance_range().start() {
            Err(ChannelFault::ShortCircuit(resistance))
        } else if resistance_f32 > *SupportedSensor::PT1000.resistance_range().end() {
            Err(ChannelFault::HighResistance(resistance))
        } else {
            Err(ChannelFault::UnexpectedResistance(resistance))
        }
    }
}

/// Pin map for RTD measurement
///
/// The ADC pins pair `in_p` and `in_n` must be connected across the RTD sensor terminals.
/// The `idac_out` ADC pin will drive a small test current through the RTD sensor.
/// The RTD sensor is assumed to be connected in series with a reference resistor wired to REF0.
pub struct PinMap {
    pub in_p: AnalogPin,
    pub in_n: AnalogPin,

    // IDAC output pin for 4-wire RTD measurement
    pub idac_out: AnalogPin,
}

impl RTD {
    pub fn new(pin_map: PinMap) -> Self {
        Self {
            pin_map,
            ch_state: Default::default(),
        }
    }

    /// Get last-known channel state
    ///
    /// The channel state is updated by [autodetect()](Self::autodetect)
    /// and [read()](Self::read) (in case of readout errors)
    pub fn channel_state(&self) -> ChannelState {
        self.ch_state
    }

    /// Autodetect the presence and sensor type (takes 1 SYNC time slot)
    ///
    /// Call this periodically to autodetect the presence of a newly connected sensor
    /// or discover a disconnection or wiring issue with an RTD sensor.
    pub async fn autodetect<DEV, OP, INT>(
        &mut self,
        adc: &mut ADS124S08<DEV, OP, INT>,
    ) -> Result<(), Error>
    where
        DEV: spi::SpiDevice + traits::CRC8Receiver + traits::Suspend,
        OP: OutputPin,
        INT: Trigger,
    {
        log::debug!(target: "RTD", "autodetect");

        // Measurement of total resistance + detection of disconnect
        // Note: this assumes 4-wire connection.
        // TODO to support 3-wire connection, re-test with IDAC on I-3Wire_1 / I-3Wire_2
        {
            // select the TMP_ADC_P/N pins as inputs
            // Note: they are connected inverted on purpose!
            // This has the same effect as driving a negative IDAC bias current:
            // we expect negative values if connected, or positive values if floating
            let inv_input = InputConfig {
                in_p: self.pin_map.in_n,
                in_n: self.pin_map.in_p,

                // 1ms delay to make sure the 10µA burnout current has time to discharge C=3nF from 3V to a negative voltage
                delay: ads124s08::Delay::TMod256,
                pga: Some(ads124s08::Gain::G1),

                // With 4khz, the sample delay is about 0.4ms. Together with the 1ms delay this is similar
                // to the time it takes to readout a channel (1.4ms for FS_800)
                datarate: ads124s08::DataRate::FS_4_000,
            };
            adc.configure_input(&inv_input).await?;

            // If effective input capacitor is larger than 3.2nF,
            // the timing of the burnout source (see above) needs to be re-evaluated
            const _: () = assert!(INPUT_FILTER_C_DIFF < 3.2e-9);

            // Inject -10µA into input. For disconnected input this should force the inputs towards
            // negative full scale
            adc.configure_burnout_source(BurnoutSource::Burnout10)
                .await?;

            // Apply 500µA of test current and measure against internal 2.5V reference
            // This implies a maximum RTD resistance of ~3kOhm
            // (the next highest IDAC current is 750µA, which may exceed the IDAC compliance voltage)
            let ref_config = RefConfig {
                ref_sel: ads124s08::RefSource::Internal,
                idac_mag: ads124s08::IDACMagnitude::I_500μA,
                idac1_out: Some(self.pin_map.idac_out),
                idac2_out: None,
                ..Default::default()
            };
            adc.configure_ref(ref_config).await?;

            let (_ref_status, raw_value) = adc.read().await?;

            adc.configure_burnout_source(BurnoutSource::BurnoutDisabled)
                .await?;
            // input polarity is inverted.
            // (this is done to be able to inject a negative burnout current)
            let raw_value = -raw_value;

            // bias applied across r_pot: 500µA IDAC - 10µA burnout
            const I_IDAC_UA: f32 = 500.0;
            // 10µA burnout current
            const I_BURNOUT_UA: f32 = 10.0;
            const I_BIAS_UA: f32 = I_IDAC_UA - I_BURNOUT_UA;
            const _: () = assert!(I_BIAS_UA > 0.0);

            // voltage drop across input filter due to burnout current
            const U_BURNOUT_OFFSET_UV: i32 =
                (I_BURNOUT_UA * (INPUT_FILTER_R + ads124s08::INTERNAL_MUX_R) * 2.0) as i32;
            const _: () = assert!(U_BURNOUT_OFFSET_UV > 0);
            const _: () = assert!(U_BURNOUT_OFFSET_UV < (1e6 * ads124s08::INTERNAL_VREF) as i32);

            const TICKS_TO_UV: f32 = 1e6 * ads124s08::INTERNAL_VREF / ads124s08::FULL_SCALE as f32;
            // measurement at ADC input [µV]
            let u_adc_uv = (raw_value as f32 * TICKS_TO_UV) as i32;
            let u_rtd_uv = u_adc_uv + U_BURNOUT_OFFSET_UV;

            // If the burnout current is strong enough to pull the input negative,
            // the RTD must be disconnected (or it would have to have a negative resistance...)
            // Note: values near zero might either indicate negative resistance (=disconnect) or short-circuit.
            // the difference is hard to tell due to unknown tolerances on burnout current + ADC internal multiplexer resistance.
            // we could also say that values < $some_threshold are most likely short-circuit.
            let channel_state = if u_rtd_uv < 0 {
                ChannelState::Disconnected
            } else {
                let r_rtd = u_rtd_uv / (I_BIAS_UA as i32);

                // Input saturated: resistance too high (if we ever want to support higher values, lower the IDAC current)
                if raw_value >= FULL_SCALE {
                    ChannelState::Fault(ChannelFault::HighResistance(r_rtd))

                // Try to autodetect sensor type
                } else {
                    match SupportedSensor::try_from_resistance(r_rtd) {
                        Ok(sensor) => ChannelState::Connected(sensor),
                        Err(fault) => ChannelState::Fault(fault),
                    }
                }
            };
            self.set_ch_state(channel_state);
        }

        Ok(())
    }

    /// Reads the RTD sensor temperature in °C (takes 1 SYNC time slot)
    ///
    /// The samplerate implicitly depends on the generation of SYNC pulses to the ADS124S08 START/SYNC pin.
    /// The recommended way is to toggle SYNC at a fixed frequency f > fs and use every n-th slot to either take a reading or
    /// perform autodetection. This keeps the samplerate stable and allows to use the ADC for other purposes as well.
    ///
    /// NB: if the SYNC pulse interval is too fast, this can have adverse side-effects:
    /// - faster than the ADC sampling time: this function never completes (ADC is continuously restarted and never finishes)
    /// - faster than the ADC sampling time + 0.5ms: analog filters may not have settled yet, leading to noise in the result
    pub async fn read<DEV, OP, INT>(
        &mut self,
        adc: &mut ADS124S08<DEV, OP, INT>,
    ) -> Result<(SupportedSensor, f32), ReadError>
    where
        DEV: spi::SpiDevice + traits::CRC8Receiver + traits::Suspend,
        OP: OutputPin,
        INT: Trigger,
    {
        match self.ch_state {
            ChannelState::Disconnected => {
                Self::dummy_read(adc).await?;
                Err(SensorReadError::Disconnected.into())
            }
            ChannelState::Fault(fault) => {
                Self::dummy_read(adc).await?;
                Err(SensorReadError::Fault(fault).into())
            }
            ChannelState::Connected(sensor) => {
                let gain = match sensor {
                    SupportedSensor::PT100 => ads124s08::Gain::G8,
                    SupportedSensor::PT1000 => ads124s08::Gain::G1,
                };

                // configure reference input & IDAC
                let ref_config = RefConfig {
                    ref_sel: ads124s08::RefSource::Ref0,
                    idac_mag: ads124s08::IDACMagnitude::I_500μA,
                    idac1_out: Some(self.pin_map.idac_out),
                    idac2_out: None,
                    ..Default::default()
                };
                adc.configure_ref(ref_config).await?;

                // configure temperature channel input
                adc.configure_input(&InputConfig {
                    in_p: self.pin_map.in_p,
                    in_n: self.pin_map.in_n,
                    delay: ads124s08::Delay::TMod14,
                    pga: Some(gain),
                    datarate: ads124s08::DataRate::FS_800,
                })
                .await?;

                match adc.read().await {
                    Ok((ref_status, value)) => match ref_status {
                        RefStatus::OK | RefStatus::BelowSupplyDiv3 => {
                            let fraction_of_r_ref = value as f32
                                / (ADS124S08::<DEV, OP, INT>::FULL_SCALE * gain.as_multiplier())
                                    as f32;
                            let rtd_ohm: f32 = fraction_of_r_ref * INPUT_REFERENCE_R;
                            match sensor.convert_celsius(rtd_ohm) {
                                Ok(t) => Ok((sensor, t)),
                                Err(_err) => Err(ReadError::Read(SensorReadError::Overrange)),
                            }
                        }
                        RefStatus::Below0V3 => {
                            self.set_ch_state(ChannelState::Disconnected);
                            Err(SensorReadError::Disconnected.into())
                        }
                    },
                    Err(ads124s08::Error::Read(read_error)) => Err(match read_error {
                        ads124s08::ReadError::CRCMismatch => SensorReadError::CRCMismatch,
                        ads124s08::ReadError::PGAOverrange => SensorReadError::Overrange,
                    }
                    .into()),
                    Err(e) => Err(ReadError::ADC(e)),
                }
            }
        }
    }

    /// Perform a dummy read to keep SYNC timing constant
    ///
    /// Internal use only: called when channel is disconnected or faulted
    /// to keep the SYNC pulse interval constant for other channels
    async fn dummy_read<DEV, OP, INT>(adc: &mut ADS124S08<DEV, OP, INT>) -> Result<(), ReadError>
    where
        DEV: spi::SpiDevice + traits::CRC8Receiver + traits::Suspend,
        OP: OutputPin,
        INT: Trigger,
    {
        // dummy config: IDAC off
        let ref_config = RefConfig {
            ref_sel: ads124s08::RefSource::Ref0,
            idac_mag: ads124s08::IDACMagnitude::Off,
            idac1_out: None,
            idac2_out: None,
            ..Default::default()
        };
        adc.configure_ref(ref_config).await?;

        // dummy config: fastest sampling, no PGA,
        adc.configure_input(&InputConfig {
            in_p: AnalogPin::AIN0,
            in_n: AnalogPin::AIN0,
            delay: ads124s08::Delay::TMod1,
            pga: None,
            datarate: ads124s08::DataRate::FS_4_000,
        })
        .await?;

        // dummy read: ignore result
        adc.read().await?;
        Ok(())
    }

    fn set_ch_state(&mut self, new_state: ChannelState) {
        let state = &mut self.ch_state;
        match (&state, &new_state) {
            (ChannelState::Disconnected, ChannelState::Connected(sensor)) => {
                log::debug!(target: "RTD", "Channel connect: {sensor:?}");
            }
            (ChannelState::Connected(_), ChannelState::Disconnected) => {
                log::debug!(target: "RTD", "Channel disconnect");
            }
            (
                ChannelState::Connected(_) | ChannelState::Disconnected,
                ChannelState::Fault(fault),
            ) => {
                log::debug!(target: "RTD", "Channel fault: {fault:?}");
            }
            _ => {}
        }
        *state = new_state;
    }
}

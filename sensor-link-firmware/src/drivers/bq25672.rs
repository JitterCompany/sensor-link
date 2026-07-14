//! TI BQ25672 Driver: I2C-controlled Battery charger

use embedded_hal::digital::OutputPin;
use embedded_hal_async::i2c::{I2c, SevenBitAddress};

use crate::{
    monotonic_time::{traits::MonotonicTime, FutureTimeout, MonotonicInstant},
    traits::{Suspend, Trigger},
    utils::bitwise::{width8::*, *},
};
use core::{fmt::Debug, marker::PhantomData};

pub trait BatteryCharger {
    type Error: core::fmt::Debug;

    /// Configure the battery charger to suitable default settings
    ///
    /// Charging is disabled by default to conserve standby power(!)
    async fn configure(&mut self) -> Result<(), Self::Error>;

    /// Wait for the charger to report a significant state change.
    ///
    /// Returns `Ok(())` when an unmasked flag is observed, or `Err(Timeout)` if
    /// none occurs within `timeout_ms`. Masked flags do not wake the caller.
    async fn await_change(&mut self, timeout_ms: u32) -> Result<(), Self::Error>;

    /// Select which power source to use
    ///
    /// The charger will attempt to use the selected source as primary power source.
    async fn select_source(&mut self, source: PowerSource) -> Result<(), Self::Error>;

    /// Disable battery charging
    ///
    /// Battery will not be charged, even thoug it may not be full.
    async fn disable_charging(&mut self) -> Result<(), Self::Error>;

    /// Enable battery charging
    ///
    /// Allows the battery to be charged. The charger only actually charges the battery if
    /// it is needed and automatically terminates the charge when complete.
    async fn enable_charging(&mut self) -> Result<(), Self::Error>;

    /// Configure the internal watchdog timer
    ///
    /// If the watchdog time expires, all settings are reset to defaults.
    /// To reset the watchdog timer, call this function again.
    async fn set_watchdog(&mut self, config: WatchdogConfig) -> Result<(), Self::Error>;

    /// Take a measurement of power supplies
    ///
    /// The resulting `PowerMeasurements` struct gives the voltages & currents for all possible power rails.
    /// Note that all power sources (such as battery or USB) are included even if they are not connected.
    /// See `charger_status()` for more information about connected adapters
    async fn measure_power(&mut self) -> Result<PowerMeasurements, Self::Error>;

    /// Reads charger status PartInformation
    ///
    /// Returns details such as which power sources are available, if the battery is being charged, etc.
    async fn charger_status(&mut self) -> Result<Status, Self::Error>;

    /// Indicates the battery temperature in terms of JEITA temperature ranges T1-T5
    async fn battery_temperature(&mut self) -> Result<BatteryTempRange, Self::Error>;

    /// Read the input limits
    async fn input_limits(&mut self) -> Result<InputLimits, Self::Error>;

    /// Indicates if any relevant faults have been detected
    ///
    /// In normal cases, the result `Ok(None)` is expected.
    async fn faults(&mut self) -> Result<Option<Faults>, Self::Error>;
}

/// BQ25672 Battery charger driver
///
/// Controls charging of the battery and detect available power supplies and their performance
pub struct BQ25672<P, CE, INT, T>
where
    P: I2c + Suspend,
    CE: OutputPin,
    INT: Trigger,
    T: MonotonicTime,
{
    peripheral: P,
    pin_ce: CE,
    pin_int: INT,

    addr: SevenBitAddress,
    _time: PhantomData<T>,

    /// Last selected source
    selected_source: Option<PowerSource>,
}

/// Inner 'RAII style' guard object. Created from `BQ25672::acquire()`
///
/// This guard ensures the I2C peripheral is powered on while transfers are being
/// done and makes sure it is always powered down as soon as it is dropped.
struct ActiveBus<'a, P: I2c + Suspend, INT: Trigger> {
    peripheral: &'a mut P,
    addr: SevenBitAddress,
    pin_int: &'a mut INT,
}

impl<P: I2c + Suspend, INT: Trigger> Drop for ActiveBus<'_, P, INT> {
    fn drop(&mut self) {
        self.peripheral.suspend();
    }
}

/// Charging status
#[derive(Debug, Clone, Copy)]
pub enum Charging {
    None = 0,
    Trickle = 1,
    Pre = 2,
    FastCC = 3,
    TaperCV = 4,
    TopOff = 6,
    Done = 7,
}

impl From<u8> for Charging {
    fn from(value: u8) -> Self {
        match value {
            1 => Self::Trickle,
            2 => Self::Pre,
            3 => Self::FastCC,
            4 => Self::TaperCV,
            // (5 is reserved)
            6 => Self::TopOff,
            7 => Self::Done,
            _ => Self::None,
        }
    }
}

/// Status details that relate to the charger chip itself.
///
/// See `BQ25672::charger_status()`
#[derive(Debug, Clone)]
pub struct ChipInfo {
    /// Watchdog expired? (this means the settings have been reset to defaults)
    pub watchdog_expired: bool,

    /// Charger IC is in thermal regulation (degraded performance to limit further overheating)
    pub overheating: bool,

    /// Mosfet for switching AC2 (USB) is present on the PCB?
    pub ac2_mosfet_found: bool,

    /// Mosfet for switching AC1 (adapter) is present on the PCB?
    pub ac1_mosfet_found: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum PowerSource {
    BatteryOnly,

    /// Power from adapter (input regulated to given minimum in mv)
    Adapter(u16),

    /// Power from USB (input regulated to given minimum in mv)
    USB(u16),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SourceStatus {
    /// Poor power source: cannot effectively power the device
    Poor,

    /// Power source present but in VINDPM regulation
    VoltageRegulation,

    /// Power source present but in IINDPM regulation
    CurrentRegulation,

    /// Power source present, no limits active
    Present,

    /// Power source not present (running on battery power)
    NoAdapter,
}

/// Charger status summary
///
/// A collection of useful stats about the charger / power sources
///
/// Some info is ommited from this status as it is not very useful:
/// - power source detection (status1 bits 1..4): always Unknown or Not qualified (USB not implemented)
/// - OTG related info (not implemented)
/// - .. (we may remove stuff later if it is not used)
#[derive(Debug, Clone)]
pub struct Status {
    /// Output power is within spec?
    pub power_good: bool,

    /// USB power is present (VAC2)
    pub usb_present: bool,

    /// External power adapter is present (VAC1)
    pub adapter_present: bool,

    pub battery_present: bool,

    pub source_status: SourceStatus,

    pub charging: Charging,
    pub chip_info: ChipInfo,
}

/// Input power limits
///
/// See `BQ25672::input_limits()`
#[derive(Debug, Clone)]
pub struct InputLimits {
    /// VINDPM: threshold voltage detected when adapter was inserted
    pub input_voltage_limit_mv: u16,

    /// IINDPM: effective input current limit
    ///
    /// Minimum of:
    /// - 3A
    /// - bus autodetection (USB / Adapter type)
    /// - ICO optimizer
    pub input_current_limit_ma: u16,

    /// input current limit calculated by *Input Current Optimizer* algorithm
    pub ico_optimizer_limit_ma: u16,
}

/// Info about which faults have occured in the charger..
///
/// See `BQ25672::faults()`
#[derive(Debug, Clone)]
pub struct Faults {
    /// Charger input (VBUS) in overvoltage protection
    pub bus_overvoltage: bool,

    /// USB source (VAC2) in overvoltage protection
    pub usb_overvoltage: bool,

    /// Adapter source (VAC1) in overvoltage protection
    pub adapter_overvoltage: bool,

    /// Charger input in overcurrent protection
    pub bus_overcurrent: bool,

    /// Charging circuit in overcurrent protection
    pub converter_overcurrent: bool,

    pub battery_overvoltage: bool,
    pub battery_overcurrent: bool,

    pub output_short_circuit: bool,
    pub output_overvoltage: bool,
    pub overtemp_shutdown: bool,
}

/// Power measurement results
///
/// See `BQ25672::measure_power()`
#[derive(Debug, Clone)]
pub struct PowerMeasurements {
    /// Current consumed from adapter is positive, from battery to adapter is negative
    pub current_bus_ma: i16,

    /// Charging current is positive, discharging is negative
    pub current_battery_ma: i16,

    pub adapter_mv: i16,
    pub usb_mv: i16,
    pub battery_mv: i16,
    pub system_mv: i16,
}

/// Battery temperature range information
///
/// Note: this is intended to quickly check if the battery
/// temperature is in the expected range. The charger can
/// also measure the exact temperature via the ADC (but
/// this is not currently implemented).
///
/// See `BQ25672::battery_temperature()`
#[derive(Debug, Clone, Copy)]
pub enum BatteryTempRange {
    /// below JEITA T1 (no charging allowed)
    Cold,

    // JEITA  T1-T2 range (charge speed may be reduced)
    Cool,

    // Jeita T2-T3 range (charging at full speed allowed)
    Normal,

    // JEITA T3-T5 range (charging speed and voltage may be reduced)
    Warm,

    // above JEITA T5 (no charging allowed)
    Hot,

    // No temperature info available (no battery present)
    Unknown,
}

impl From<u8> for BatteryTempRange {
    fn from(tmp: u8) -> Self {
        if tmp.bit(Bit::B0) {
            Self::Hot
        } else if tmp.bit(Bit::B1) {
            Self::Warm
        } else if tmp.bit(Bit::B2) {
            Self::Cool
        } else if tmp.bit(Bit::B3) {
            Self::Cold
        } else {
            Self::Normal
        }
    }
}

/// Charge Timeout settings
///
/// Fast charge is terminated after this timeout.
/// Select the shortest timeout that will fully
/// charge the battery (depends on capacity)
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum ChargeTimeout {
    Max5Hours = 0,
    Max8Hours = 1,
    Max12Hours = 2,
    Max24Hours = 3,
}

/// Watchdog setting
///
/// If enabled, the charger chip resets itself
/// to factory default settings if the watchdog
/// is not triggered faster than this interval
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum WatchdogConfig {
    Off = 0,
    ResetAfter500ms = 1,
    ResetAfter1sec = 2,
    ResetAfter2sec = 3,
    ResetAfter20sec = 4,
    ResetAfter40sec = 5,
    ResetAfter80sec = 6,
    ResetAfter160sec = 7,
}

/// State of the power supply
///
/// Normally Idle, but the charger can shutdown or reboot
/// the whole system.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum PowerMode {
    Idle = 0,
    Shutdown = 1,
    Ship = 2,
    SystemReset = 3,
}

#[derive(Debug, Clone)]
pub enum Error<I2CE>
where
    I2CE: Debug,
{
    /// Something went wrong on the I2C bus
    I2C(I2CE),

    /// Charger IC is not recognized
    ChipNotRecognized,

    /// Charger is configured wrongly, for example unexpected number of battery cells
    MisConfigured,

    /// Operation timed out (charger may not be in the correct state?)
    Timeout,

    /// Generic IO error (something wrong with the GPIO interface)
    IO,
}

// ADC conversion normally takes 100-200ms
const ADC_TIMEOUT_MS: u32 = 500;

impl<P, CE, INT, T> BatteryCharger for BQ25672<P, CE, INT, T>
where
    P: I2c + Suspend,
    CE: OutputPin,
    INT: Trigger,
    T: MonotonicTime,
{
    type Error = Error<P::Error>;

    async fn configure(&mut self) -> Result<(), Error<P::Error>> {
        self.disable_charging().await?;
        let mut guard = self.acquire();
        guard.inner_configure().await
    }

    async fn await_change(&mut self, timeout_ms: u32) -> Result<(), Self::Error> {
        // BatteryOnly: don't care about the battery temperature flags
        // Otherwise a battery with no sensor (~=very cold battery) somehow keeps false triggering.
        // There is probably some side-effect where the TS status is reset to zero causing INT
        let mask3 = match &self.selected_source {
            Some(PowerSource::BatteryOnly) => CHARGER_MASK_3_TS,
            _ => 0,
        };

        // Track from start so spurious /INT edges can't extend the wait indefinitely.
        let start = MonotonicInstant::now();
        let timeout_us = timeout_ms as u64 * 1_000;

        // Check flags BEFORE waiting: a prior /INT pulse may have been consumed by
        // `await_flag` (or any other `wait_untill_any_edge`) during chg_update, leaving
        // the event's flag latched but no wake pending. Post-wait drain (next loop
        // iteration's pre-check) filters spurious wakes.
        loop {
            {
                let mut guard = self.acquire();
                let mut significant = false;
                for (flag_reg, mask) in [
                    (RegU8::ChargerFlag0, CHARGER_MASK_0),
                    (RegU8::ChargerFlag1, CHARGER_MASK_1),
                    (RegU8::ChargerFlag2, 0),
                    (RegU8::ChargerFlag3, mask3),
                    (RegU8::FaultFlag0, 0),
                    (RegU8::FaultFlag1, 0),
                ] {
                    let value = guard.read_reg_8(flag_reg).await?;
                    if (value & !mask) != 0 {
                        log::debug!(target: "BQ25672", "{flag_reg:?}: {value:}");
                        significant = true;
                    }
                }

                if significant {
                    return Ok(());
                }
            };

            let elapsed = start.elapsed_us();
            if elapsed >= timeout_us {
                return Err(Error::Timeout);
            }

            let opt = self
                .pin_int
                .wait_untill_any_edge()
                .with_timeout_us(timeout_us - elapsed)
                .await;
            opt.ok_or(Error::Timeout)?;
        }
    }

    async fn select_source(&mut self, source: PowerSource) -> Result<(), Self::Error> {
        self.selected_source = Some(source);

        let (_hiz, en_acdrv1, en_acdrv2, vindpm) = match source {
            PowerSource::BatteryOnly => (1, 0, 0, None),
            PowerSource::Adapter(vindpm) => (0, 1, 0, Some(vindpm)),
            PowerSource::USB(vindpm) => (0, 0, 1, Some(vindpm)),
        };

        let mut guard = self.acquire();

        // Overrule default input voltage regulation
        // Charger IC auto-detects a reasonable value upon adapter insertion
        // but that may be too strict for less well-regulated power sources
        if let Some(vindpm) = vindpm {
            let vindpm = (vindpm / 100).min(255) as u8;
            guard.write_reg_8(RegU8::InputVoltageLimit, vindpm).await?;
        }

        // Reset HiZ mode: this forces a retry of poor-source detection
        guard
            .set_charger_control0(INPUT_CURRENT_OPTIMIZER, HIZ_MODE)
            .await?;

        // Select between ACDRV1 (adapter) and ACDRV2 (USB)
        let acdrv_bits = en_acdrv1 << 6 | en_acdrv2 << 7;
        if acdrv_bits != 0 {
            guard
                .write_reg_8(RegU8::ChargerControl4, acdrv_bits)
                .await?;
        }

        Ok(())
    }

    /// Disable battery charging
    ///
    /// Battery will not be charged, even thoug it may not be full.
    async fn disable_charging(&mut self) -> Result<(), Error<P::Error>> {
        // ~CE pin is active low
        self.pin_ce.set_high().map_err(|_| Error::IO)
    }

    /// Enable battery charging
    ///
    /// Allows the battery to be charged. The charger only actually charges the battery if
    /// it is needed and automatically terminates charging when complete.
    async fn enable_charging(&mut self) -> Result<(), Error<P::Error>> {
        // ~CE pin is active low
        self.pin_ce.set_low().map_err(|_| Error::IO)
    }

    /// Configure the internal watchdog timer
    ///
    /// If the watchdog time expires, all settings are reset to defaults.
    /// To reset the watchdog timer, call this function again.
    async fn set_watchdog(&mut self, config: WatchdogConfig) -> Result<(), Error<P::Error>> {
        let mut guard = self.acquire();
        guard.set_watchdog(config).await
    }

    /// Take a measurement of power supplies
    ///
    /// The resulting `PowerMeasurements` struct gives the voltages & currents for all possible power rails.
    /// Note that all power sources (such as battery or USB) are included even if they are not connected.
    /// See `charger_status()` for more information about connected adapters
    async fn measure_power(&mut self) -> Result<PowerMeasurements, Error<P::Error>> {
        let mut guard = self.acquire();
        guard.set_battery_current_measurement(true).await?;
        let result = guard.inner_measure_power().await;
        guard.set_battery_current_measurement(false).await?;
        result
    }

    /// Reads charger status PartInformation
    ///
    /// Returns details such as which power sources are available, if the battery is being charged, etc.
    async fn charger_status(&mut self) -> Result<Status, Error<P::Error>> {
        let mut guard = self.acquire();
        let status0 = guard.read_reg_8(RegU8::ChargerStatus0).await?;
        let status1 = guard.read_reg_8(RegU8::ChargerStatus1).await?;
        let status2 = guard.read_reg_8(RegU8::ChargerStatus2).await?;
        let status3 = guard.read_reg_8(RegU8::ChargerStatus3).await?;

        let source_status = if !status0.bit(Bit::B3) {
            SourceStatus::Poor
        } else if status0.bit(Bit::B6) {
            SourceStatus::VoltageRegulation
        } else if status0.bit(Bit::B7) {
            SourceStatus::CurrentRegulation
        } else if status0.bit(Bit::B0) {
            SourceStatus::Present
        } else {
            SourceStatus::NoAdapter
        };

        Ok(Status {
            power_good: status0.bit(Bit::B3),
            usb_present: status0.bit(Bit::B2),
            adapter_present: status0.bit(Bit::B1),
            battery_present: status2.bit(Bit::B0),
            source_status,
            charging: Charging::from((status1 >> 5) & 0b111),
            chip_info: ChipInfo {
                watchdog_expired: status0.bit(Bit::B5),
                overheating: status2.bit(Bit::B2),
                ac2_mosfet_found: status3.bit(Bit::B7),
                ac1_mosfet_found: status3.bit(Bit::B6),
            },
        })
    }

    /// Indicates the battery temperature in terms of JEITA temperature ranges T1-T5
    async fn battery_temperature(&mut self) -> Result<BatteryTempRange, Error<P::Error>> {
        let mut guard = self.acquire();
        let status2 = guard.read_reg_8(RegU8::ChargerStatus2).await?;
        let status4 = guard.read_reg_8(RegU8::ChargerStatus4).await?;

        if status2.bit(Bit::B0) {
            Ok(BatteryTempRange::from(status4 & 0b1111))
        } else {
            Ok(BatteryTempRange::Unknown)
        }
    }

    async fn input_limits(&mut self) -> Result<InputLimits, Error<P::Error>> {
        let mut guard = self.acquire();
        let ico_optimizer_limit = guard.read_reg_u16(RegU16::InputCurrentOptimalLimit).await?;
        let input_voltage_limit = guard.read_reg_8(RegU8::InputVoltageLimit).await?;
        let input_current_limit = guard.read_reg_u16(RegU16::InputCurrentLimit).await?;

        let input_voltage_limit_mv = (input_voltage_limit as u16) * 100; // 8-bit register in steps of 100mV
        let input_current_limit_ma = (input_current_limit & 0x1FF) * 10; // 9-bit register in steps of 10mA
        let ico_optimizer_limit_ma = (ico_optimizer_limit & 0x1FF) * 10; // 9-bit register in steps of 10mA
        Ok(InputLimits {
            input_voltage_limit_mv,
            input_current_limit_ma,
            ico_optimizer_limit_ma,
        })
    }

    /// Indicates if any relevant faults have been detected
    ///
    /// In normal cases, the result `Ok(None)` is expected.
    async fn faults(&mut self) -> Result<Option<Faults>, Error<P::Error>> {
        let mut guard = self.acquire();
        let fault0 = guard.read_reg_8(RegU8::Fault0).await?;
        let fault1 = guard.read_reg_8(RegU8::Fault1).await?;

        if fault0 == 0 && fault1 == 0 {
            Ok(None)
        } else {
            Ok(Some(Faults {
                bus_overvoltage: fault0.bit(Bit::B6),
                usb_overvoltage: fault0.bit(Bit::B1),
                adapter_overvoltage: fault0.bit(Bit::B0),
                bus_overcurrent: fault0.bit(Bit::B4),
                converter_overcurrent: fault0.bit(Bit::B2),
                battery_overvoltage: fault0.bit(Bit::B5),
                battery_overcurrent: fault0.bit(Bit::B3),
                output_short_circuit: fault1.bit(Bit::B7),
                output_overvoltage: fault1.bit(Bit::B6),
                overtemp_shutdown: fault1.bit(Bit::B2),
            }))
        }
    }
}

// fixed settings
// Floor for VSYS when charger sources it from VBUS (battery dead/missing).
// Chosen so the downstream SIC473 5V5 buck stays in spec: VOUT_max = 0.92 × VIN,
// so a 5.5 V output needs VIN ≥ 5.98 V; 6.3 V leaves margin for load droop and
// BATFET drop. Only active on external power — battery-only runs follow VBAT,
// see hardware repo issue #113 for the broader discussion.
const MIN_SYSTEM_VOLTAGE_MV: u16 = 6_300;
const CHARGE_LIMIT_MV: u16 = 8_200;
const CHARGE_LIMIT_MA: u16 = 2_000;
const CHARGE_N_CELLS: u8 = 2;
const TIMEOUT: ChargeTimeout = ChargeTimeout::Max24Hours;
const WATCHDOG: WatchdogConfig = WatchdogConfig::Off;
const POWERMODE: PowerMode = PowerMode::Idle;
const EN_IBAT: bool = false;
const INPUT_CURRENT_OPTIMIZER: bool = true;
const HIZ_MODE: bool = false;

const CHARGER_MASK_0: u8 =
    1 << 7 // IINDPM
    | 1 << 6// VinDPM
        ;

const CHARGER_MASK_1: u8 =
1 << 6 // ICO
;

const CHARGER_MASK_3_TS: u8 = 0b1111; // All TS flags

impl<P, CE, INT, T> BQ25672<P, CE, INT, T>
where
    P: I2c + Suspend,
    CE: OutputPin,
    INT: Trigger,
    T: MonotonicTime,
{
    pub fn new(peripheral: P, charge_enable: CE, interrupt: INT) -> Self {
        Self {
            peripheral,
            addr: 0x6B,
            pin_ce: charge_enable,
            pin_int: interrupt,
            _time: PhantomData,
            selected_source: None,
        }
    }

    //                                     //
    // ----   Private methods below   ---- //
    //                                     //

    /// Power on the I2C bus and return a handle for register access.
    /// The bus is powered off when the returned [`ActiveBus`] is dropped.
    fn acquire(&mut self) -> ActiveBus<'_, P, INT> {
        self.peripheral.resume();
        ActiveBus {
            peripheral: &mut self.peripheral,
            addr: self.addr,
            pin_int: &mut self.pin_int,
        }
    }
}

impl<P, INT> ActiveBus<'_, P, INT>
where
    P: I2c + Suspend,
    INT: Trigger,
{
    async fn inner_configure(&mut self) -> Result<(), Error<P::Error>> {
        self.reset_to_defaults().await?;
        self.verify_chip().await?;

        if self.n_cells_in_series().await? != CHARGE_N_CELLS {
            return Err(Error::MisConfigured);
        }

        self.set_min_sys_voltage(MIN_SYSTEM_VOLTAGE_MV).await?;
        self.configure_charging(CHARGE_LIMIT_MA).await?;

        self.set_charger_control0(INPUT_CURRENT_OPTIMIZER, HIZ_MODE)
            .await?;
        self.set_watchdog(WATCHDOG).await?;
        self.set_power_mode(POWERMODE).await?;
        self.set_battery_current_measurement(EN_IBAT).await?;
        self.configure_adc_inputs().await?;

        self.write_reg_8(RegU8::ChargerMask0, CHARGER_MASK_0)
            .await?;
        self.write_reg_8(RegU8::ChargerMask1, CHARGER_MASK_1)
            .await?;

        Ok(())
    }

    async fn reset_to_defaults(&mut self) -> Result<(), Error<P::Error>> {
        self.write_reg_8(RegU8::TerminationControl, 1 << 6).await
    }

    async fn verify_chip(&mut self) -> Result<(), Error<P::Error>> {
        let part_id = self.read_reg_8(RegU8::PartInformation).await?;
        if (part_id & 0b111111) == 0b100001 {
            Ok(())
        } else {
            Err(Error::ChipNotRecognized)
        }
    }

    /// Reads for how many cells the charger is configured
    ///
    /// This is configured by a pulldown resistor, but recommended to verify
    async fn n_cells_in_series(&mut self) -> Result<u8, Error<P::Error>> {
        let reg = self.read_reg_8(RegU8::ReChargeControl).await?;
        Ok((reg >> 6) + 1)
    }

    async fn inner_measure_power(&mut self) -> Result<PowerMeasurements, Error<P::Error>> {
        self.write_reg_8(
            RegU8::ADCControl,
            1 << 7 // Enable ADC
            | 1 << 6 // One-shot conversion (auto-disable)
            | 1 << 4 // 14-bit = ca 1mV resolution
            | 0 << 3 // No averaging
            | 1 << 2, // Start with new ADC conversion
        )
        .await?;

        self.await_flag(Flag::ADCDone, ADC_TIMEOUT_MS).await?;

        Ok(PowerMeasurements {
            current_bus_ma: self.read_reg_i16(RegI16::ADCIbus).await?,
            current_battery_ma: self.read_reg_i16(RegI16::ADCIbat).await?,
            adapter_mv: self.read_reg_i16(RegI16::ADCVac1).await?,

            usb_mv: self.read_reg_i16(RegI16::ADCVac2).await?,
            battery_mv: self.read_reg_i16(RegI16::ADCVbat).await?,
            system_mv: self.read_reg_i16(RegI16::ADCVsys).await?,
        })
    }

    /// Wait until the given status flag is set. Peripheral must be acquired.
    async fn await_flag(&mut self, flag: Flag, timeout_ms: u32) -> Result<(), Error<P::Error>> {
        loop {
            let flag_found = match flag {
                Flag::ADCDone => self.read_reg_8(RegU8::ChargerFlag2).await?.bit(Bit::B5),
            };

            if flag_found {
                return Ok(());
            }

            // `any_edge` so that a pulse that fired between the caller's I2C write
            // (which triggered the device event) and this wait is captured via READY.
            let opt = self
                .pin_int
                .wait_untill_any_edge()
                .with_timeout_ms(timeout_ms)
                .await;
            opt.ok_or(Error::Timeout)?;
        }
    }

    async fn set_charger_control0(
        &mut self,
        optimize_input_current: bool,
        enter_hiz_mode: bool,
    ) -> Result<(), Error<P::Error>> {
        self.write_reg_8(
            RegU8::ChargerControl0,
            1 << 7 // Discharge battery on overvoltage
            | 0 << 6 // Don't force-discharge battery
            | 1 << 5 // Enable charging (if ~CE pin is also low, see enable_charging())
            | u8::from(optimize_input_current) << 4 // Enable ICO?
            | u8::from(optimize_input_current) << 3 // Force ICO?
            | u8::from(enter_hiz_mode) << 2 // disconnect adapter / stop switching (untill next vbus plugin)
            | 1 << 1, // Enable auto-termination of charging (based on timers etc)
        )
        .await
    }

    async fn set_watchdog(&mut self, config: WatchdogConfig) -> Result<(), Error<P::Error>> {
        self.write_reg_8(
            RegU8::ChargerControl1,
            0 << 4 // OVP level : 24V
        | 0 << 3 // reset watchdog timeout
        | config as u8,
        )
        .await
    }

    async fn set_power_mode(&mut self, power_mode: PowerMode) -> Result<(), Error<P::Error>> {
        self.write_reg_8(
            RegU8::ChargerControl2,
            0 << 7 // Don't force D+/D- detection
            | 0 << 6 // disable D+/D- detection
            | 0 << 5 // Disable HVDC 12V level
            | 0 << 4 // Disable HVDC 9V level
            | 0 << 3 // Disable HVDC
            | (power_mode as u8) << 1 // new power mode
            | (1 as u8), // Immediately apply power mode
        )
        .await
    }

    async fn configure_adc_inputs(&mut self) -> Result<(), Error<P::Error>> {
        self.write_reg_8(
            RegU8::ADCDisable0,
            0 << 7   // current_bus_ma
        | 0 << 6 // current_battery_ma
        | 1 << 5 // VBUS disabled (we already measure adapter + usb)
        | 0 << 4 // battery_mv
        | 0 << 3 // system_mv
        | 0 << 2 // TS disabled (BatteryTempRange is good enough)
        | 0 << 1, // TDIE disabled (overheating status is good enough)
        )
        .await?;

        self.write_reg_8(
            RegU8::ADCDisable1,
            1 << 7   // DP disabled (not connected on the PCB)
        | 1 << 6 //DM disabled (not connected on the PCB)
        | 0 << 5 // usb_mv
        | 0 << 4, // adapter_mv
        )
        .await
    }

    /// Configures battery current measurement on/off
    ///
    /// This is required for measuring battery current if no adapter is present.
    /// If measurement is not used, disable to conserve power.
    async fn set_battery_current_measurement(
        &mut self,
        enable_measurement: bool,
    ) -> Result<(), Error<P::Error>> {
        self.write_reg_8(
            RegU8::ChargerControl5,
            1 << 7 // Ship FET is present in our circuit
            | u8::from(enable_measurement) << 5 // EN_IBAT
            | 2 << 3 // leave IBAT_REG at defaults (we dont use OTG)
            | 1 << 2 // Keep IINDPM enabled
            | 1 << 1 // Keep EXTILIM enabled
            | 1 << 0, // Enable battery discharge overcurrent protection
        )
        .await
    }

    /// Configures the minimum system voltage
    ///
    /// The charger attempt to regulate to at least this minimum if possible (charger present)
    /// Running on battery-only the system voltage may still go below this level..
    async fn set_min_sys_voltage(&mut self, min_system_mv: u16) -> Result<(), Error<P::Error>> {
        // Min system voltage: 0=2500, 250mv per bit, max=16_000
        self.write_reg_8(
            RegU8::MinSystemVoltage,
            (min_system_mv.min(16_000).saturating_sub(2_500) / 250) as u8,
        )
        .await
    }

    /// Configure battery charging limits
    async fn configure_charging(&mut self, current_limit_ma: u16) -> Result<(), Error<P::Error>> {
        let current_limit_ma = current_limit_ma.min(CHARGE_LIMIT_MA);

        let voltage_limit_mv = CHARGE_LIMIT_MV;
        let timeout = TIMEOUT;

        // Charge voltage limit: 0=0, 10mV per bit, max=18_800
        self.write_reg_u16(
            RegU16::ChargeVoltageLimit,
            (voltage_limit_mv.min(18_800) / 10) as u16,
        )
        .await?;

        // Charge current limit: 0=0, 10mA per bit, max=3_000
        self.write_reg_u16(RegU16::ChargeCurrentLimit, (current_limit_ma / 10) as u16)
            .await?;

        // terminate charge at 10% of current limit
        let termination_limit = current_limit_ma / 10;
        self.write_reg_8(
            RegU8::TerminationControl,
            (termination_limit.min(1_000) / 40) as u8,
        )
        .await?;

        self.write_reg_8(
            RegU8::TimerControl,
            0 << 6 // no topoff timer
            | 1 << 5 // enable trickle-charge timer
            | 1 << 4 // enable pre-charge timer
            | 1 << 3 // enable fast-charge timer
            | (timeout as u8) << 1 // fast charge timeout
            | 1, // double timeouts if not enough power is available
        )
        .await
    }

    // ---- Private register API below ---- //

    async fn read_reg_8(&mut self, register: RegU8) -> Result<u8, Error<P::Error>> {
        let mut res = [0xFF];
        self.peripheral
            .write_read(self.addr, &[register as u8], &mut res)
            .await
            .map_err(Error::I2C)?;

        Ok(res[0])
    }

    async fn read_reg_u16(&mut self, register: RegU16) -> Result<u16, Error<P::Error>> {
        let mut res = [0xFF, 0xFF];
        self.peripheral
            .write_read(self.addr, &[register as u8], &mut res)
            .await
            .map_err(Error::I2C)?;

        Ok(u16::from_be_bytes(res))
    }

    async fn read_reg_i16(&mut self, register: RegI16) -> Result<i16, Error<P::Error>> {
        let mut res = [0xFF, 0xFF];
        self.peripheral
            .write_read(self.addr, &[register as u8], &mut res)
            .await
            .map_err(Error::I2C)?;

        Ok(i16::from_be_bytes(res))
    }

    async fn write_reg_8(&mut self, register: RegU8, value: u8) -> Result<(), Error<P::Error>> {
        self.peripheral
            .write(self.addr, &[register as u8, value])
            .await
            .map_err(Error::I2C)
    }

    async fn write_reg_u16(&mut self, register: RegU16, value: u16) -> Result<(), Error<P::Error>> {
        let data = value.to_be_bytes();
        let tx = [register as u8, data[0], data[1]];
        self.peripheral
            .write(self.addr, &tx)
            .await
            .map_err(Error::I2C)
    }
}

#[derive(Debug, Clone, Copy)]
enum Flag {
    ADCDone,
}

#[allow(unused)]
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
enum RegU8 {
    MinSystemVoltage = 0x00,
    InputVoltageLimit = 0x05,
    PrechargeControl = 0x08,
    TerminationControl = 0x09,
    ReChargeControl = 0x0A,
    OTGCurrentRegulation = 0x0D,
    TimerControl = 0x0E,
    ChargerControl0 = 0x0F,
    ChargerControl1 = 0x10,
    ChargerControl2 = 0x11,
    ChargerControl3 = 0x12,
    ChargerControl4 = 0x13,
    ChargerControl5 = 0x14,
    MPPTControl = 0x15,
    TempControl = 0x16,
    NTCControl0 = 0x17,
    NTCControl1 = 0x18,

    // Status bits
    ChargerStatus0 = 0x1B,
    ChargerStatus1 = 0x1C,
    ChargerStatus2 = 0x1D,
    ChargerStatus3 = 0x1E,
    ChargerStatus4 = 0x1F,
    Fault0 = 0x20,
    Fault1 = 0x21,

    // INT flags
    ChargerFlag0 = 0x22,
    ChargerFlag1 = 0x23,
    ChargerFlag2 = 0x24,
    ChargerFlag3 = 0x25,
    FaultFlag0 = 0x26,
    FaultFlag1 = 0x27,

    // INT masks
    ChargerMask0 = 0x28,
    ChargerMask1 = 0x29,
    ChargerMask2 = 0x2A,
    ChargerMask3 = 0x2B,
    FaultMask0 = 0x2C,
    FaultMask1 = 0x2D,

    ADCControl = 0x2E,
    ADCDisable0 = 0x2F,
    ADCDisable1 = 0x30,
    DPDMDriver = 0x47,
    PartInformation = 0x48,
}

#[allow(unused)]
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
enum RegU16 {
    ChargeVoltageLimit = 0x01,
    ChargeCurrentLimit = 0x03,
    InputCurrentLimit = 0x06,
    OTGVoltageRegulation = 0x0B,
    InputCurrentOptimalLimit = 0x19,
    ADCTS = 0x3F,
    ADCDP = 0x43,
    ADCDM = 0x45,
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
enum RegI16 {
    ADCIbus = 0x31,
    ADCIbat = 0x33,
    _ADCVbus = 0x35,
    ADCVac1 = 0x37,
    ADCVac2 = 0x39,
    ADCVbat = 0x3B,
    ADCVsys = 0x3D,
    _ADCTDIE = 0x41,
}

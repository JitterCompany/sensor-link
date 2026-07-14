//! TI ADS124S08 Driver: 24-bit multiplexed ADC with integrated PGA

use embedded_hal::digital::OutputPin;
use embedded_hal_async::spi;

use crate::{
    monotonic_time::{delay_us, FutureTimeout},
    traits::{self, Trigger},
};

mod config;
mod register;

pub use config::*;
use register::Register;

/// Full scale of the ADC: scale factor = gain * raw value/FULL_SCALE * reference voltage
pub const FULL_SCALE: i32 = 0x7F_FFFF;

/// Internal reference voltage in V
pub const INTERNAL_VREF: f32 = 2.5;

/// Internal mux resistance in Ohm (estimated, not in datasheet)
pub const INTERNAL_MUX_R: f32 = 570.0;

/// ADS124S08 Driver: 24-bit ADC with integrated PGA
///
/// This driver is optimized for quick switching between
/// input channels (single shot conversions).
///
/// To generate a stable sample frequency, the driver does
/// not issue start commands but assumes the hardware generates
/// a stable SYNC pulse directly wired to the ADC.
pub struct ADS124S08<DEV, OP, INT> {
    spi: DEV,

    reset_inv: OP,
    ready: INT,

    // set to true once the input has been configured.
    // prevents sampling from undefined inputs
    input_configured: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReadError {
    /// PGA output is in overrange: gain is too high or wrong input
    PGAOverrange,

    /// Received data is corrupt
    CRCMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Error {
    /// ADC driver is not enabled.
    NotEnabled,

    /// ADC has seen a Power-On-Reset.
    /// Caller must reset & reconfigure the ADC
    UnexpectedPOR,

    /// ADC is busy. Caller should wait & reconfigure the ADC
    NotReady,

    /// Something went wrong with the low-level communication
    Bus,

    /// ADC could not be detected. Most likely a hardware issue
    SensorDetect,

    /// Config readback does not match written value (bus error or adc defect?)
    ConfigReadback,

    /// Attempted to sample without specifying input config (bug in caller code)
    /// See [ADS124S08::configure_input()]
    NoInputConfig,

    /// Hardware API is not compatible
    PlatformSupport,

    /// Error related to the last sample readout. A retry or readout of a different channel may succeed.
    Read(ReadError),
}

/// Diagnostics info about the ADS124S08 and its supply voltages
///
/// Intended as a health-check / manufacturing verification
#[derive(Debug)]
pub struct Diagnostics {
    pub analog_supply_volt: f32,
    pub digital_supply_volt: f32,
    pub internal_temp_celsius: f32,
}

/// Status of the voltage reference
///
/// The ADC voltage reference is monitored against two thresholds:
/// - 0.3V
/// - (AVDD-AVSS)/3 (e.g. 1.66V for AVDD=5V,AVSS=0V)
///
/// This information may be used to detect unexpected conditions
/// (e.g. broken wire in ratiometric measurement setup).
#[derive(Debug, Clone, Copy)]
pub enum RefStatus {
    /// Reference voltage OK (above (AVDD-AVSS)/3)
    OK,

    /// Reference voltage below 0.3V
    Below0V3,

    /// Reference voltage below (AVDD-AVSS)/3 but above 0.3V
    BelowSupplyDiv3,
}

impl<DEV, OP, INT> ADS124S08<DEV, OP, INT>
where
    DEV: spi::SpiDevice + traits::CRC8Receiver + traits::Suspend,
    OP: OutputPin,
    INT: Trigger,
{
    /// Full scale of the ADC: scale factor = gain * raw value/FULL_SCALE * reference voltage
    pub const FULL_SCALE: i32 = FULL_SCALE;

    /// Internal reference voltage in V
    pub const INTERNAL_VREF: f32 = INTERNAL_VREF;

    /// Internal mux resistance in Ohm (estimated, not in datasheet)
    pub const INTERNAL_MUX_R: f32 = INTERNAL_MUX_R;

    // common values for SYS register except the SYS_MON selection
    const REG_SYS_COMMON: u8 = register::SYS::SENDSTAT::Enabled as u8
        | register::SYS::CRC::Enabled as u8
        | register::SYS::TIMEOUT::Disabled as u8
        | register::SYS::CAL_SAMP::Avg8 as u8;

    /// Create an ADS124S08. Requires an SPI device, !RESET pin and DRDY trigger
    pub fn new(spi_device: DEV, reset_inv: OP, ready: INT) -> Self {
        Self {
            spi: spi_device,
            reset_inv,
            ready,
            input_configured: false,
        }
    }

    /// Enable the ADC (release reset and configure it)
    pub async fn enable(&mut self) -> Result<(), Error> {
        self.spi.resume();
        self.reset_inv.set_high().ok();

        // Must be at least 4096 tclock = 1ms, or > 2.2ms after power-on.
        delay_us(5_000).await;

        let mut rx = [0];
        if (self.read_reg(Register::STATUS, &mut rx).await?[0] & (register::STATUS::NOTRDY as u8))
            != 0
        {
            log::warn!(target: "ADS124S08", "Not ready?: {rx:02X?}");
            if self.sensor_detect_after_reset().await.is_ok() {
                return Err(Error::NotReady);
            }
        }

        self.sensor_detect_after_reset().await?;

        // Clear POR status bit
        self.write_reg(Register::STATUS, &[0]).await?;
        if (self.read_reg(Register::STATUS, &mut rx).await?[0] & (register::STATUS::FL_POR as u8))
            != 0
        {
            return Err(Error::ConfigReadback);
        }

        // VBIAS: disconnect from any inputs
        self.write_reg_and_verify(Register::VBIAS, &[0]).await?;

        // Set a sane default config
        self.reset_all_config().await
    }

    /// Perform diagnostics measurements.
    ///
    /// Note 1: the SYNC pulse must be disabled (or running at f < 50 Hz) while running diagnostics
    /// Note 2: this resets any config that may have been applied before (like [ADS124S08::enable()])
    pub async fn diagnostics(&mut self) -> Result<Diagnostics, Error> {
        if self.spi.is_suspended() {
            return Err(Error::NotEnabled);
        }

        self.reset_all_config().await?;

        // configure PGA gain=1 and long delay to make sure internal monitors will fully settle
        // (in_p/in_n config are dummy, these are not used in monitoring-mode)
        let input_cfg = &InputConfig {
            in_p: AnalogPin::AIN0,
            in_n: AnalogPin::AIN0,
            delay: Delay::TMod1024,
            pga: Some(Gain::G1),
            datarate: DataRate::FS_100,
        };
        self.configure_input(&input_cfg).await?;

        let analog_supply_volt = {
            self.write_reg_and_verify(
                Register::SYS,
                &[Self::REG_SYS_COMMON | register::SYS::SYS_MON::SupplyAnalogDiv4 as u8],
            )
            .await?;
            let (_ref_stat, analog_supply) = self.read_now(20).await?;

            // Measurement: supply/4 versus internal reference voltage
            (analog_supply as f32 / Self::FULL_SCALE as f32) * Self::INTERNAL_VREF * 4.0
        };

        let digital_supply_volt = {
            self.write_reg_and_verify(
                Register::SYS,
                &[Self::REG_SYS_COMMON | register::SYS::SYS_MON::SupplyDigitalDiv4 as u8],
            )
            .await?;
            let (_ref_stat, digital_supply) = self.read_now(20).await?;
            // Measurement: supply/4 versus internal reference voltage
            (digital_supply as f32 / Self::FULL_SCALE as f32) * Self::INTERNAL_VREF * 4.0
        };

        let internal_temp_celsius = {
            self.write_reg_and_verify(
                Register::SYS,
                &[Self::REG_SYS_COMMON | register::SYS::SYS_MON::InternalTemperature as u8],
            )
            .await?;

            let (_ref_stat, internal_temp) = self.read_now(20).await?;
            // Measurement: internal temperature sensor versus internal reference voltage
            let internal_temp_volt: f32 =
                (internal_temp as f32 / Self::FULL_SCALE as f32) * Self::INTERNAL_VREF;

            // internal temperature sensor: offset=129mV @ 25°C, scale factor=403μA/°C
            25.0 + (internal_temp_volt - 129e-3) / 403e-6
        };

        self.reset_all_config().await?;

        Ok(Diagnostics {
            analog_supply_volt,
            digital_supply_volt,
            internal_temp_celsius,
        })
    }

    /// Configure the burnout current sources
    ///
    /// Recommended to leave them off during normal measurements for optimal acuracy,
    /// but can be useful to detect floating inputs.
    ///
    /// The burn-out current (0.2-10µA) is injected into the selected input and drives the input towards full-scale
    pub async fn configure_burnout_source(&mut self, cfg: BurnoutSource) -> Result<(), Error> {
        if self.spi.is_suspended() {
            return Err(Error::NotEnabled);
        }

        self.write_reg_and_verify(Register::SYS, &[Self::REG_SYS_COMMON | cfg.reg_sys_mon()])
            .await
    }

    /// (re-)configure the input for the next measurement
    ///
    /// Note: readout of measurement data + configuring input for the next measurement can
    /// be combined in one. See [ADS124S08::read_and_reconfigure_input()]
    pub async fn configure_input(&mut self, cfg: &InputConfig) -> Result<(), Error> {
        if self.spi.is_suspended() {
            return Err(Error::NotEnabled);
        }

        let values = &[cfg.reg_inpmux(), cfg.reg_pga(), cfg.reg_datarate()];
        self.write_reg_and_verify(Register::INPMUX, values).await?;
        self.input_configured = true;

        // clear ~DATA_READY~ trigger: if it was already ready, it was for a previous configuration
        let _ = self.ready.poll_ready();
        Ok(())
    }

    /// Read a sample from the ADC and reconfigure the input for the next measurement
    ///
    /// The reconfiguration is done at the same time (full-duplex SPI) as reading the sample
    /// to optimize switching speed between different inputs
    ///
    /// Note: this assumes the ADC was started via the SYNC pin and waits for the conversion to become ready
    /// If the SYNC frequency is too high for the InputConfig, the ADC is restarted before the conversion is ready
    /// and this future will never complete.
    #[inline]
    pub async fn read_and_reconfigure_input(
        &mut self,
        cfg: &InputConfig,
    ) -> Result<(RefStatus, i32), Error> {
        if self.spi.is_suspended() {
            return Err(Error::NotEnabled);
        }

        // Prepare tx part of the transfer: write 3 registers INPMUX, PGA and DATARATE.
        const N_REGS: u8 = 3;
        let tx = &[
            Cmd::WriteReg(Register::INPMUX).as_u8(),
            N_REGS - 1,
            cfg.reg_inpmux(),
            cfg.reg_pga(),
            cfg.reg_datarate(),
        ];
        self.read_while_tx_full_duplex(tx).await
    }

    /// Read a sample from the ADC
    ///
    /// Note: this assumes the ADC was started via the SYNC pin and waits for the conversion to become ready
    /// If the SYNC frequency is too high for the InputConfig, the ADC is restarted before the conversion is ready
    /// and this future will never complete.
    #[inline]
    pub async fn read(&mut self) -> Result<(RefStatus, i32), Error> {
        if self.spi.is_suspended() {
            return Err(Error::NotEnabled);
        }

        self.read_while_tx_full_duplex(&[]).await
    }

    async fn read_while_tx_full_duplex(&mut self, tx: &[u8]) -> Result<(RefStatus, i32), Error> {
        if !self.input_configured {
            return Err(Error::NoInputConfig);
        }
        let mut rx = [0_u8; 5];

        // Wait for DRDY falling edge if it has not occured yet
        self.ready.wait_untill_any_edge().await;

        // Full duplex transfer:
        // - Write: configuration for next sample
        // - Read: last sample (STATUS + 24-bit ADC value + CRC8)
        self.spi
            .transfer(&mut rx, tx)
            .await
            .map_err(|_| Error::Bus)?;

        // crc over whole rx transaction *including the received crc8* should be zero
        let crc = self.spi.rx_crc8();
        if crc != 0 {
            return Err(Error::Read(ReadError::CRCMismatch));
        }

        self.parse(rx)
    }

    /// Read a sample from the ADC immediately
    ///
    /// This does not depend on SYNC and waits up to `millis` ms after issuing START
    /// even if DRDY signal does not trigger.
    /// This assumes a valid input & reference config was already done, and that
    /// SYNC is either disabled or running slow enough for the current input config.
    async fn read_now(&mut self, millis: u32) -> Result<(RefStatus, i32), Error> {
        if !self.input_configured {
            return Err(Error::NoInputConfig);
        }

        self.spi
            .write(&[Cmd::Start.as_u8()])
            .await
            .map_err(|_| Error::Bus)?;

        self.ready
            .wait_untill_next_edge()
            .with_timeout_ms(millis)
            .await;

        let cmd = [Cmd::RData.as_u8()];
        let mut result = [0; 5];
        self.spi
            .transaction(&mut [
                spi::Operation::Write(&cmd),
                spi::Operation::Read(&mut result),
            ])
            .await
            .map_err(|_| Error::Bus)?;

        // crc over whole rx transaction *including the received crc8* should be zero
        // Note: the SPIDevice may include the dummy byte while sending Cmd::RData in the CRC.
        // This is usually zero so won't casue issues. If it ever does, we have to make the SPIDevice
        // implementation better (e.g. only calculate CRC over non-write operations)
        let crc = self.spi.rx_crc8();
        if crc != 0 {
            log::warn!("CRC {crc:02X} over {result:02X?}");
            return Err(Error::Read(ReadError::CRCMismatch));
        }

        self.parse(result)
    }

    /// Apply configuration related to reference & IDAC current sources
    pub async fn configure_ref(&mut self, cfg: RefConfig) -> Result<(), Error> {
        // Write REF,IDACMAG,IDACMUX in one transaction (registers are successive)
        self.write_reg_and_verify(
            Register::REF,
            &[cfg.reg_ref(), cfg.reg_idacmag(), cfg.reg_idacmux()],
        )
        .await
    }

    /// Detect sensor by checking default register values
    ///
    /// Note: only use this after a reset. If registers have been reconfigured,
    /// this could give false positives!
    async fn sensor_detect_after_reset(&mut self) -> Result<(), Error> {
        let mut rx = [0];

        // 1. check ID register. Unfortunately the 5 high bits are undefined, so we can only
        // check the lower 3 bits, for which the expected value is 0 (1 would be ADS124S06).
        // This is hard to distinguish from a disconnected/miswired/shorted SPI bus.
        if (self.read_reg(Register::ID, &mut rx).await?[0] & 0b111) != 0 {
            log::error!(target: "ADS124S08", "Not an ADS124S08: is this a 6-channel variant?: {rx:02X?}");
            return Err(Error::SensorDetect);
        }

        // 2. Extra check: readback another register that is clearly different
        // from 0x00 or 0xFF (the most common SPI bus failure modes).
        if 0x14 != self.read_reg(Register::DATARATE, &mut rx).await?[0] {
            log::error!(target: "ADS124S08", "Expected 0x14, got: {rx:02X?}");
            return Err(Error::SensorDetect);
        }
        Ok(())
    }

    /// (re-)Set all config to initial values
    async fn reset_all_config(&mut self) -> Result<(), Error> {
        self.input_configured = false;

        // System config
        self.write_reg_and_verify(
            Register::SYS,
            &[register::SYS::SENDSTAT::Enabled as u8
                | register::SYS::CRC::Enabled as u8
                | register::SYS::TIMEOUT::Disabled as u8
                | register::SYS::CAL_SAMP::Avg8 as u8
                | register::SYS::SYS_MON::Disabled as u8],
        )
        .await?;

        // CRC8 polynomial config
        self.spi
            .configure_polynomial(0b111)
            .map_err(|_| Error::PlatformSupport)?;

        self.write_reg_and_verify(
            Register::GPIOCON,
            &[register::GPIOCON::CON::AllAnalog as u8],
        )
        .await?;

        self.configure_ref(Default::default()).await
    }

    /// parse STATUS + 24-bit result
    fn parse(&self, rx: [u8; 5]) -> Result<(RefStatus, i32), Error> {
        let status = rx[0];
        // 1. Status: check for error flags
        if status
            & (register::STATUS::FL_POR as u8
                | register::STATUS::NOTRDY as u8
                | register::STATUS::FL_P_RAILP as u8
                | register::STATUS::FL_P_RAILN as u8
                | register::STATUS::FL_N_RAILP as u8
                | register::STATUS::FL_N_RAILN as u8)
            != 0
        {
            // convert error flags into more specific error value
            if status & (register::STATUS::FL_POR as u8) != 0 {
                return Err(Error::UnexpectedPOR);
            } else if status & (register::STATUS::NOTRDY as u8) != 0 {
                return Err(Error::NotReady);
            } else {
                return Err(Error::Read(ReadError::PGAOverrange));
            }
        }

        // 2. Status: check ref status. Other values than RefStatus::OK are not an error per definition,
        // we pass it to upper layer and let it decide whether or not the reference voltage is out-of-spec
        // (as this depends on the context of the measurement)
        let ref_status = if status & (register::STATUS::FL_REF_L0 as u8) != 0 {
            RefStatus::Below0V3
        } else if status & (register::STATUS::FL_REF_L1 as u8) != 0 {
            RefStatus::BelowSupplyDiv3
        } else {
            RefStatus::OK
        };

        // 24-bit big-endian to i32
        Ok((
            ref_status,
            i32::from_be_bytes([rx[1], rx[2], rx[3], 0]) >> 8,
        ))
    }

    async fn read_reg<'r>(
        &mut self,
        reg: Register,
        result: &'r mut [u8],
    ) -> Result<&'r [u8], Error> {
        if result.len() == 0 {
            return Ok(&[]);
        }

        // cap length to 0x12 as there are no more registers (length field is only 5 bit anyways)
        let len = result.len().min(0x12);

        // RREG: [0b001rrrrr, n] where r = register address triggers an (n+1) byte read
        let cmd: [u8; 2] = [Cmd::ReadReg(reg).as_u8(), (len - 1) as u8];
        let mut result = &mut result[..len];

        self.spi
            .transaction(&mut [
                spi::Operation::Write(&cmd),
                spi::Operation::Read(&mut result),
            ])
            .await
            .map_err(|_| Error::Bus)?;

        Ok(result)
    }

    async fn write_reg<'r>(&mut self, reg: Register, data: &[u8]) -> Result<(), Error> {
        if data.len() == 0 {
            return Ok(());
        }

        // cap length to 0x12 as there are no more registers (length field is only 5 bit anyways)
        let len = data.len().min(0x12);

        // WREG: [0b010rrrrr, n] where r = register address triggers an (n+1) byte write
        let cmd = [Cmd::WriteReg(reg).as_u8(), (len - 1) as u8]; // READ REG5, 1 register
        let data = &data[..len];

        self.spi
            .transaction(&mut [spi::Operation::Write(&cmd), spi::Operation::Write(&data)])
            .await
            .map_err(|_| Error::Bus)?;

        Ok(())
    }

    /// Write a set of registers and verify they read back exactly as written
    async fn write_reg_and_verify<'r>(&mut self, reg: Register, data: &[u8]) -> Result<(), Error> {
        self.write_reg(reg, data).await?;
        let mut read_buff = [0_u8; 0x12];
        let read_buff = &mut read_buff[..data.len().min(0x12)];
        let readback = self.read_reg(reg, read_buff).await?;

        if readback != data {
            return Err(Error::ConfigReadback);
        }
        Ok(())
    }

    /// Disable the ADC (hold in reset)
    pub fn disable(&mut self) {
        self.reset_inv.set_low().ok();

        self.spi.suspend();
    }
}

/// SPI commands for ADS124S08
enum Cmd {
    ReadReg(Register),
    WriteReg(Register),
    Start,
    RData,
}

impl Cmd {
    pub fn as_u8(self) -> u8 {
        match self {
            Cmd::WriteReg(register) => 0b0100_0000 | (register as u8),
            Cmd::ReadReg(register) => 0b0010_0000 | (register as u8),
            Cmd::Start => 0b0000_1000,
            Cmd::RData => 0b0001_0010,
        }
    }
}

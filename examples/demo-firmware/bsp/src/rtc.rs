//! The RTC: wall-clock time (calendar plus LSE drift calibration) and the
//! backup registers that hold the boot statistics and the panic flag across a
//! reset.

use crate::rcc::{self, RTCClockStatus};
pub use chrono::NaiveDateTime;
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, Timelike};
use cortex_m::interrupt;
use sensor_link_firmware::{
    drivers::time::{self, TimestampSource as _},
    logic::time_adjust::{self, DriftError, ErrorEstimate, TempErrorEstimate},
    traits::{MutU32, U32},
};
use stm32ral::{modify_reg, read_reg, rtc, write_reg};

const MIN_YEAR: i32 = 2000;

fn bcd_to_u32(tens: u32, units: u32) -> u32 {
    tens * 10 + units
}

fn u32_to_bcd(value: u32) -> (u32, u32) {
    (value / 10, value % 10)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Error {
    /// RTC time has not been configured yet
    TimeUnknown,

    /// RTC is misconfigured / timer is in invalid state that can't represent a valid time
    TimeInvalid,

    /// RTC clock not (yet?) running
    ClockNotRunning,
}

pub struct RTC {
    peripheral: rtc::Instance,

    /// Safety: only create the BackupRegisters once
    backup_registers_available: bool,
}

pub struct BackupRegister {
    /// Safety: private so that BackupRegisters can only be constructed via RTC API
    reg: BackupReg,
}

impl BackupRegister {
    /// Safety: caller must ensure to never access the same register twice
    /// If this is violated, the register contents may get corrupted
    pub unsafe fn from_reg(reg: BackupReg) -> Self {
        Self { reg }
    }
}

#[derive(Debug)]
pub enum BackupReg {
    BACKUP0,
    BACKUP1,
    BACKUP2,
    BACKUP3,
    BACKUP4,
    BACKUP5,
    BACKUP6,
    BACKUP7,
    // (... UP TO BACKUP31 exist)
}

impl BackupRegister {
    /// Read a value from one of the backup registers
    pub fn read(&self) -> u32 {
        // Safety: this is safe because each BackupRegister can only be constructed once
        // from the RTC driver and is the only one accessing these registers
        unsafe {
            match self.reg {
                BackupReg::BACKUP0 => read_reg!(rtc, RTC, BKP0R),
                BackupReg::BACKUP1 => read_reg!(rtc, RTC, BKP1R),
                BackupReg::BACKUP2 => read_reg!(rtc, RTC, BKP2R),
                BackupReg::BACKUP3 => read_reg!(rtc, RTC, BKP3R),
                BackupReg::BACKUP4 => read_reg!(rtc, RTC, BKP4R),
                BackupReg::BACKUP5 => read_reg!(rtc, RTC, BKP5R),
                BackupReg::BACKUP6 => read_reg!(rtc, RTC, BKP6R),
                BackupReg::BACKUP7 => read_reg!(rtc, RTC, BKP7R),
            }
        }
    }
    pub fn write(&mut self, value: u32) {
        // Safety: this is safe because each BackupRegister can only be constructed once
        // from the RTC driver and is the only one accessing these registers
        unsafe {
            match self.reg {
                BackupReg::BACKUP0 => write_reg!(rtc, RTC, BKP0R, value),
                BackupReg::BACKUP1 => write_reg!(rtc, RTC, BKP1R, value),
                BackupReg::BACKUP2 => write_reg!(rtc, RTC, BKP2R, value),
                BackupReg::BACKUP3 => write_reg!(rtc, RTC, BKP3R, value),
                BackupReg::BACKUP4 => write_reg!(rtc, RTC, BKP4R, value),
                BackupReg::BACKUP5 => write_reg!(rtc, RTC, BKP5R, value),
                BackupReg::BACKUP6 => write_reg!(rtc, RTC, BKP6R, value),
                BackupReg::BACKUP7 => write_reg!(rtc, RTC, BKP7R, value),
            }
        }
    }
}

impl U32 for BackupRegister {
    fn read(&self) -> u32 {
        self.read()
    }
}

impl MutU32 for BackupRegister {
    fn write(&mut self, new_value: u32) {
        self.write(new_value)
    }
}

/// Nominal frequency of external RTC crystal (fixed)
const CRYSTAL_HZ: u32 = 32_768;

/// Calibration adjust period of the RTC peripheral (see `CALR` register)
/// RTC peripheral inserts/skips pulses during this period to achieve an
/// accurage clock frequency *on average*
const CAL_PERIOD_SEC: u32 = 32;

/// Calibration adjustment resolution in parts-per-billion
/// The smallest calibration step the RTC peripheral can make
const CAL_RESOLUTION_PPB: u32 = 1_000_000_000 / (CAL_PERIOD_SEC * CRYSTAL_HZ);

/// RTC resolution (steps/sec). Must be a power of two. higher resolution = higher power usage.
const SUBSEC_RESOLUTION: u32 = 256;

/// Predivider: divides crystal frequency to ticks in subsecond resolution
const ASYNC_PREDIVIDER: u32 = CRYSTAL_HZ / SUBSEC_RESOLUTION;

/// Converts from subsecond downcount value to microseconds
///
/// Note: the result can be negative in some cases.
/// Examples:
/// - 16:04 with micros 400_000 means the time is 16:04.400_000
/// - 16:04 with micros -600_000 means the time is 16:03.400_000
fn subsec_to_micros(subsec: u32) -> i32 {
    const SUBSEC_MAX: i32 = (SUBSEC_RESOLUTION - 1) as i32;

    (1_000_000 * (SUBSEC_MAX - subsec as i32)) / SUBSEC_RESOLUTION as i32
}

/// Converts from microseconds [0-1_000_000] to subsecond downcount value
///
/// NB: micros is assumed to be < 1_000_000 (1 second). Larger values are truncated.
fn micros_to_subsec(micros: u32) -> u32 {
    let frac_of_max = (micros.min(999_999) * SUBSEC_RESOLUTION) / 1_000_000;
    (SUBSEC_RESOLUTION - 1) - frac_of_max
}

impl RTC {
    pub fn new(peripheral: rtc::Instance) -> Self {
        // Enable clock if not already running
        rcc::enable_rtc();

        Self {
            peripheral,
            backup_registers_available: true,
        }
    }

    /// Split off the backup registers. This only succeeds once
    pub fn split_backup_registers(&mut self) -> Result<[BackupRegister; 8], ()> {
        if !self.backup_registers_available {
            return Err(());
        }
        // NOTE: for Backup registers to be writeable, rcc::unprotect_backup_domain()
        // is required. this is already done by rcc::enable_rtc().
        self.backup_registers_available = false;

        // Safety: This is safe as it can only happen once, so no two copies of the same
        // register should exist. **unless the unsafe from_reg() API is used directly**
        unsafe {
            Ok([
                BackupRegister::from_reg(BackupReg::BACKUP0),
                BackupRegister::from_reg(BackupReg::BACKUP1),
                BackupRegister::from_reg(BackupReg::BACKUP2),
                BackupRegister::from_reg(BackupReg::BACKUP3),
                BackupRegister::from_reg(BackupReg::BACKUP4),
                BackupRegister::from_reg(BackupReg::BACKUP5),
                BackupRegister::from_reg(BackupReg::BACKUP6),
                BackupRegister::from_reg(BackupReg::BACKUP7),
            ])
        }
    }

    pub fn is_initialized(&self) -> bool {
        read_reg!(rtc, self.peripheral, ISR, INITS == 1)
            && rcc::rtc_clock_status() == RTCClockStatus::Running
    }

    /// Try to set the RTC time
    ///
    /// This can fail if the RTC clock is not running or has failed.
    /// In case Error::ClockNotRunning is returned, the RTC clock is not ready (yet).
    /// A retry 1 second later should normally succeed (the clock is (re-)started as needed)
    pub fn set_time(&self, time: NaiveDateTime) -> Result<(), Error> {
        // Check for invalid time before applying
        let year = time.year();
        if year < MIN_YEAR {
            return Err(Error::TimeInvalid);
        }

        // Set time in critical section so that the update appears atomic* to readers
        // (*) almost: time+date happens at once, subsecond offset takes effect after ca 20ms
        let result = interrupt::free(|_| {
            let status = rcc::rtc_clock_status();
            match status {
                RTCClockStatus::Running => {}

                // No clock yet, but rcc is still trying
                RTCClockStatus::Starting => return Err(Error::ClockNotRunning),

                // No clock: (re-)start RTC clock
                RTCClockStatus::Off => {
                    rcc::enable_rtc();
                    return Err(Error::ClockNotRunning);
                }
            }

            // Unlock write access in case of backup domain reset
            write_reg!(rtc, self.peripheral, WPR, 0xCA);
            write_reg!(rtc, self.peripheral, WPR, 0x53);

            // Enter init mode
            modify_reg!(rtc, self.peripheral, ISR, INIT: 1);
            while read_reg!(rtc, self.peripheral, ISR, INITF != 1) {}

            // Configure prescalers based on external crystal = 32_768 Hz
            // 32_768 / 128 = 256 sub-second ticks. Lower A-div would give more resultion but slightly higher power consumption
            modify_reg!(rtc, self.peripheral, PRER, PREDIV_A: ASYNC_PREDIVIDER-1, PREDIV_S: SUBSEC_RESOLUTION-1);

            modify_reg!(rtc, self.peripheral, CR,

                // No external refclock detection
                REFCKON: 0,

                // 24 hour/day format
                FMT: 0,

                // Bypass shadow registers: we just read the date/time in a loop instead of relying
                // waiting on the shadow status (there are some Errata about this for similar STM32 chips..)
                BYPSHAD: 1
            );

            // Set time
            {
                let (hour_t, hour_u) = u32_to_bcd(time.hour());
                let (min_t, min_u) = u32_to_bcd(time.minute());
                let (sec_t, sec_u) = u32_to_bcd(time.second());
                modify_reg!(rtc, self.peripheral, TR, PM:0,
                    HT: hour_t, HU: hour_u,
                    MNT: min_t, MNU: min_u,
                    ST: sec_t, SU: sec_u
                );
            }

            // Set date
            {
                let (year_t, year_u) = u32_to_bcd((year - MIN_YEAR) as u32);
                let (month_t, month_u) = u32_to_bcd(time.month());
                let (day_t, day_u) = u32_to_bcd(time.day());

                modify_reg!(rtc, self.peripheral, DR,
                    YT: year_t, YU: year_u,
                    MT: month_t, MU: month_u,
                    DT: day_t, DU: day_u,
                    WDU: time.weekday().number_from_monday() // monday = 1, sunday = 7
                );
            }

            // Setup shift register to add subsecond part to the time (bit 31 = add 1 second, subsec is subtracted)
            let subsec_written = micros_to_subsec(time.and_utc().timestamp_subsec_micros());
            write_reg!(rtc, self.peripheral, SHIFTR, 1 << 31 | subsec_written);

            // Exit init mode
            modify_reg!(rtc, self.peripheral, ISR, INIT: 0);
            while read_reg!(rtc, self.peripheral, ISR, INITF != 0) {}

            // Don't wait untill subsecond adjust has been applied: this typically takes 20ms.
            // Blocking the critical section for 20+ ms would be unacceptable.
            // Instead, callers of time() may observe two discontinuities:
            // 1. The jump to the currently set time (rounded down to whole seconds)
            // 2. Ca 20ms later, the time will jump formward (less than one second) to complete the time adjustment.
            //while read_reg!(rtc, self.peripheral, ISR, SHPF == 1) {}

            Ok(())
        });

        result
    }

    pub fn time(&self) -> Result<NaiveDateTime, Error> {
        match rcc::rtc_clock_status() {
            RTCClockStatus::Running => (),
            RTCClockStatus::Starting => return Err(Error::ClockNotRunning),
            RTCClockStatus::Off => return Err(Error::TimeUnknown),
        }
        // This loop typically just runs once, occasionally twice. The purpose is to
        // sycnronize data between the RTC clock domain and the memory bus (APB?):
        // The RTC is configured with BYPSHAD=1. In this mode we directly read from the RTC,
        // which is clocked asynchronously: any of the date/time registers can read as invalid
        // if an RTC clock tick occurs at the time of readout. This is detected by re-reading the SSR.
        // See RM0432 (rev 9) 47.3.10
        loop {
            let subsec = read_reg!(rtc, self.peripheral, SSR);

            let (hour_t, hour_u, min_t, min_u, sec_t, sec_u) =
                read_reg!(rtc, self.peripheral, TR, HT, HU, MNT, MNU, ST, SU);
            let (year_t, year_u, mon_t, mon_u, day_t, day_u) =
                read_reg!(rtc, self.peripheral, DR, YT, YU, MT, MU, DT, DU);

            let subsec_check = read_reg!(rtc, self.peripheral, SSR);

            // Time is consistent (no changes while reading the registers): parse datetime
            if subsec == subsec_check {
                let date = NaiveDate::from_ymd_opt(
                    bcd_to_u32(year_t, year_u) as i32 + MIN_YEAR,
                    bcd_to_u32(mon_t, mon_u),
                    bcd_to_u32(day_t, day_u),
                )
                .ok_or(Error::TimeInvalid)?;

                let time = NaiveTime::from_hms_opt(
                    bcd_to_u32(hour_t, hour_u),
                    bcd_to_u32(min_t, min_u),
                    bcd_to_u32(sec_t, sec_u),
                )
                .ok_or(Error::TimeInvalid)?;

                let datetime = NaiveDateTime::new(date, time);
                let offset_micros = subsec_to_micros(subsec);
                let datetime = datetime
                    .checked_add_signed(Duration::microseconds(offset_micros.into()))
                    .ok_or(Error::TimeInvalid)?;
                return Ok(datetime);
            }
        }
    }

    pub fn calibrate(
        &self,
        correction: &time_adjust::TotalDriftError,
    ) -> Result<(), time::CalError> {
        // RM0432 Rev 9, 46.3.12 smooth calibration on the fly:

        // 1. Poll RECALPF
        // Fairly unlikely for upper layer to request multiple calibrations back-toback.
        // Error out instead of a blocking poll loop
        if read_reg!(rtc, self.peripheral, ISR, RECALPF == 1) {
            return Err(time::CalError::Busy);
        }

        // 2. Write value to RTC_CALR. this auto-sets RECALPF
        {
            let adjust_ppb = correction.error_ppb;
            let steps: i32 = adjust_ppb / (CAL_RESOLUTION_PPB as i32);
            let (cal_plus512, cal_min) = match steps {
                // negative: only set 9-bit cal_min value
                neg_steps @ i32::MIN..0 => (0, neg_steps.abs().min(511) as u32),
                // zero: no adjust
                0 => (0, 0),

                // positive: adjust by +512 - cal_min
                pos_steps @ 1..=i32::MAX => (1, 512_u32.saturating_sub(pos_steps as u32)),
            };
            write_reg!(rtc, self.peripheral, CALR, CALP: cal_plus512, CALM: cal_min);
        }

        // 3. calibration will take effect after `3/SUBSEC_RESOLUTION` seconds (ca 12ms for SUBSEC_RESOLUTION=256)
        Ok(())
    }

    pub fn temp_correction(temp_celsius: i32) -> TempErrorEstimate {
        // temperature error model based on ABS06-32.768kHz: (-0.03 +/-0.01 ppm) * (T-25)**2
        let dt = temp_celsius.clamp(-25, 85) - 25;
        let dt_squared = dt * dt;
        let error_ppb = -30 * dt_squared;
        let uncertainty_ppb = 10 * dt_squared as u32;
        TempErrorEstimate {
            error_ppb,
            uncertainty_ppb,
        }
    }
}

impl time::TimestampSource for RTC {
    fn timestamp_us(&self) -> Result<i64, time::Error> {
        let now = self.time().map_err(|e| match e {
            Error::TimeUnknown => time::Error::TimeUnknown,
            Error::TimeInvalid => time::Error::TimeInvalid,
            Error::ClockNotRunning => time::Error::TimeNotTicking,
        })?;
        let micros = now.and_utc().timestamp_micros();
        if micros <= 0 {
            Err(time::Error::TimeInvalid)
        } else {
            Ok(micros)
        }
    }
}

impl time::AdjustableTimestampSource for RTC {
    fn adjust_us(&self, target_time_us: i64, offset_limit: u32) -> Result<i64, time::Error> {
        let immediate_adjust = match self.timestamp_us() {
            Ok(rtc_time) => {
                let delta = target_time_us - rtc_time;
                let abs_delta = delta.unsigned_abs();
                if abs_delta < offset_limit as u64 {
                    // TODO implement smooth calibration (RTC_CALR)
                    log::warn!(
                        "RTC: smooth adjustment not implemented yet! Ignoring delta={abs_delta} us"
                    );
                    0
                } else {
                    delta
                }
            }
            Err(_) => target_time_us,
        };

        if immediate_adjust != 0 {
            let datetime =
                DateTime::from_timestamp_micros(target_time_us).ok_or(time::Error::TimeInvalid)?;
            self.set_time(datetime.naive_utc()).map_err(|e| match e {
                Error::ClockNotRunning => time::Error::TimeNotTicking,
                Error::TimeUnknown => time::Error::TimeUnknown,
                Error::TimeInvalid => time::Error::TimeInvalid,
            })?;
        }
        Ok(immediate_adjust)
    }

    #[inline]
    fn calibrate(&self, correction: &time_adjust::TotalDriftError) -> Result<(), time::CalError> {
        RTC::calibrate(self, correction)
    }

    fn initial_accuracy(&self) -> DriftError {
        // Placeholder until this board's LSE is characterized
        DriftError(ErrorEstimate {
            error_ppb: 0,
            uncertainty_ppb: 40_000,
        })
    }

    #[inline]
    fn temp_correction(&self, temp_celsius: f32) -> TempErrorEstimate {
        RTC::temp_correction(temp_celsius as i32)
    }
}

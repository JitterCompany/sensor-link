//! The RTC, used here for its backup registers only.
//!
//! The backup registers hold the boot statistics and the panic flag across a
//! reset. They live in the RTC's backup domain, which is only kept (rather than
//! reset) when the RTC clock is running -- hence `RTC::new` starting it. The
//! calendar itself is not used: timestamps come from the TIM5 monotonic.

use sensor_link_firmware::traits::{MutU32, U32};
use stm32ral::{read_reg, rtc, write_reg};

use crate::rcc;

pub struct RTC {
    /// Held so that no one else can claim the peripheral.
    _peripheral: rtc::Instance,

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

impl RTC {
    pub fn new(peripheral: rtc::Instance) -> Self {
        // Enable clock if not already running
        rcc::enable_rtc();

        Self {
            _peripheral: peripheral,
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
}

use crate::rcc;
use cortex_m::interrupt;
use stm32ral::{modify_reg, pwr, read_reg};

/// Declares that the VDDIO2 supply is valid
///
/// **NB**: required for operation of GPIO PG2..PG15
pub fn enable_vddio2() {
    interrupt::free(|_| {
        rcc::enable_pwr_interface();
        unsafe {
            modify_reg!(pwr, PWR, CR2, IOSV: 1);
            while read_reg!(pwr, PWR, CR2, IOSV != 1) {}
        }
        rcc::disable_pwr_interface();
    });
}

pub fn enable_vbat_charging() {
    interrupt::free(|_| {
        rcc::enable_pwr_interface();
        unsafe {
            modify_reg!(pwr, PWR, CR4, VBE: 1);
            while read_reg!(pwr, PWR, CR4, VBE != 1) {}
        }
        rcc::disable_pwr_interface();
    });
}

/// Allow writes to backup domain (RTC, backup registers)
///
/// After a reset, the RTC and backup registers are protected
/// against (unintended) write access. Unprotect before modifying
/// registers in the RTC domain
pub fn unprotect_backup_domain() {
    interrupt::free(|_| {
        rcc::enable_pwr_interface();
        unsafe {
            modify_reg!(pwr, PWR, CR1, DBP: 1);
            while read_reg!(pwr, PWR, CR1, DBP != 1) {}
        }
        rcc::disable_pwr_interface();
    });
}

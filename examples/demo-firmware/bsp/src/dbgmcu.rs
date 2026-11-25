//! Peripheral reset and clock configuration module
//!

use cortex_m::interrupt::free;
use stm32ral::{dbgmcu, modify_reg};

/// Freeze the IWDG while core is halted for debugging
pub fn freeze_iwdg() {
    free(|_| {
        unsafe {
            modify_reg!(dbgmcu, DBGMCU, APB1FZR1, DBG_IWDG_STOP: 1);
        };
    });
}

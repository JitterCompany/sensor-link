use crate::{gpio, rcc};
use cortex_m::interrupt;
use stm32ral::{modify_reg, syscfg};

/// Configure a given EXTI interrupt with a given pinmapping
///
/// Note that each exti channel can only support a specific range of pins
pub fn configure_exti(pin: &gpio::Pin) {
    let exti_no = pin.pin as u8;
    let reg = exti_no / 4; // 4 configs per register
    let shift = (exti_no % 4) * 4; // 4 bits per entry
    interrupt::free(|_| {
        // Note: this assumes COMP / VREFBUFF peripherals are not used.
        rcc::enable_syscfg_comp_vrefbuff();

        let modifier = |reg| {
            let masked = reg & !(0b1111 << shift);
            masked | (pin.port as u32) << shift
        };

        unsafe {
            match reg {
                0 => modify_reg!(syscfg, SYSCFG, EXTICR1, modifier),
                1 => modify_reg!(syscfg, SYSCFG, EXTICR2, modifier),
                2 => modify_reg!(syscfg, SYSCFG, EXTICR3, modifier),
                3 => modify_reg!(syscfg, SYSCFG, EXTICR4, modifier),
                4..=255 => unreachable!(),
            }
        }

        // Note: this assumes no other drivers had already enabled syscfg / comp or vrefbuff
        // If such a conflict arises, we need to change the enable/disable API to refcounting
        rcc::disable_syscfg_comp_vrefbuff();
    });
}

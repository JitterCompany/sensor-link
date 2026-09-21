use core::sync::atomic::AtomicU32;
use fugit::{Hertz, RateExtU32};
use stm32ral::{rcc, read_reg};

static SYSCLOCK_HZ: AtomicU32 = AtomicU32::new(4000000);

pub(super) fn set_sysclock(sysclk_hz: u32) {
    SYSCLOCK_HZ.store(sysclk_hz, core::sync::atomic::Ordering::SeqCst);
}

/// Read only struct for obtaining clock rates.
pub struct Clocks {
    sysclk: u32,
}

impl Clocks {
    pub fn from_global() -> Self {
        Clocks {
            sysclk: SYSCLOCK_HZ.load(core::sync::atomic::Ordering::SeqCst),
        }
    }

    pub fn sysclk(&self) -> Hertz<u32> {
        self.sysclk.Hz()
    }

    pub fn hclk(&self) -> Hertz<u32> {
        let rcc = unsafe { &*rcc::RCC };
        let hpre = read_reg!(rcc, rcc, CFGR, HPRE);
        match hpre {
            0b1000 => self.sysclk() / 2,
            0b1001 => self.sysclk() / 4,
            0b1010 => self.sysclk() / 8,
            0b1011 => self.sysclk() / 16,
            0b1100 => self.sysclk() / 64,
            0b1101 => self.sysclk() / 128,
            0b1110 => self.sysclk() / 256,
            0b1111 => self.sysclk() / 512,
            _ => self.sysclk(),
        }
    }

    pub fn pclk1(&self) -> Hertz<u32> {
        let hclk = self.hclk();

        let rcc = unsafe { &*rcc::RCC };
        let ppre = read_reg!(rcc, rcc, CFGR, PPRE1);
        match ppre {
            0b100 => hclk / 2,
            0b101 => hclk / 4,
            0b110 => hclk / 8,
            0b111 => hclk / 16,
            _ => hclk,
        }
    }
}

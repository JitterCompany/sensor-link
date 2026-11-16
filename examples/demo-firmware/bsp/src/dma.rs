//! DMA1 + DMAMUX1 driver for STM32L4R5.
//!
//! [`Dma1::split`] → [`DmaChannel<N>`] tokens → [`DmaChannel::erase`] →
//! [`DmaCh`]; consumers are [`Transfer`] (one-shot) and
//! [`CircularRx`] (RX ring).
//!
//! Facts: L4R5 has no D-cache (no cache maintenance; the pre-`EN`
//! `fence(SeqCst)` is still needed for CPU→DMA write ordering). DMAMUX
//! channels are 0-indexed, DMA channels 1-indexed (DMA1 `N` ↔ DMAMUX
//! `C{N-1}CR`). Per-channel IRQ vectors `DMA1_CH1`..`CH7`.

use core::sync::atomic::{fence, AtomicU8, Ordering};

use cortex_m::interrupt;
use stm32ral::{dma, dmamux1, read_reg, write_reg, RWRegister};

use crate::rcc;

// --- DMA1/DMAMUX1 clock power-gating --------
// Refcount mask gates the shared AHB clock when no channel votes for it; the
// hold/release live inside the channel primitives, so no upper layer touches
// RCC.

/// Bit `n-1` set ⇒ channel `n` votes the DMA1/DMAMUX1 clock on.
static DMA1_CLK_HOLD: AtomicU8 = AtomicU8::new(0);

/// Channel `n` votes the clock on; enables it on the `0→nonzero` edge
/// (idempotent). The edge runs under `interrupt::free` so the bit transition
/// and the clock flip are atomic together.
fn dma1_clk_hold(n: u8) {
    interrupt::free(|_| {
        let prev = DMA1_CLK_HOLD.fetch_or(1 << (n - 1), Ordering::Relaxed);
        if prev == 0 {
            rcc::enable_clk_dmamux1();
            rcc::enable_clk_dma1();
        }
    });
}

/// Channel `n` drops its clock vote; gates the clock when the last voter
/// releases (mask→0). Idempotent.
fn dma1_clk_release(n: u8) {
    interrupt::free(|_| {
        let bit = 1 << (n - 1);
        let prev = DMA1_CLK_HOLD.fetch_and(!bit, Ordering::Relaxed);
        if prev == bit {
            rcc::disable_clk_dma1();
            rcc::disable_clk_dmamux1();
        }
    });
}

// --- Async one-shot Transfer layer ----------------------------------------
// Embassy-style future over a one-shot DMA transfer + its move-only stop /
// ISR handles, in its own module; see [`oneshot`].
mod oneshot;

pub use oneshot::{Error, OneShotDmaIrq, Transfer};
pub(crate) use oneshot::{OneShot, OneShotAbort};

// --- Circular RX layer ----------------------------------------------------
// Streaming circular DMA receiver (the UART RX ring) + its four move-only
// role handles, in its own module; see [`circular`].
mod circular;

pub(crate) use circular::{CircShared, CircularRxControl, CircularRxLineIrq};
pub use circular::{CircularRx, CircularRxDmaIrq, TryRead};

/// Transfer direction.
#[derive(Clone, Copy)]
pub enum Direction {
    /// Peripheral → Memory (e.g. UART RX).
    PeripheralToMemory,
    /// Memory → Peripheral (e.g. UART TX).
    MemoryToPeripheral,
}

/// Per-transfer word size (PSIZE / MSIZE in CCR).
#[derive(Clone, Copy)]
pub enum Word {
    Byte,
    Half,
    Word,
}

/// Channel arbitration priority (PL in CCR).
#[derive(Clone, Copy)]
pub enum Priority {
    Low,
    Medium,
    High,
    VeryHigh,
}

/// DMAMUX1 request line. Numeric values come from the L4R5 chip metadata
/// (verified against stm32-data, which mirrors RM0432).
#[derive(Clone, Copy)]
pub struct Request(pub u8);

impl Request {
    pub const USART3_RX: Request = Request(28);
    pub const USART3_TX: Request = Request(29);
}

/// Snapshot of the per-channel ISR flags.
#[derive(Default, Clone, Copy, Debug)]
pub struct ChannelFlags {
    pub gif: bool,
    pub tcif: bool,
    pub htif: bool,
    pub teif: bool,
}

/// Owner type for DMA1 + DMAMUX1. Constructed once at boot.
pub struct Dma1 {
    _priv: (),
}

impl Dma1 {
    /// Enable peripheral clocks and reset DMA1/DMAMUX1.
    ///
    /// The `dma::Instance` / `dmamux1::Instance` arguments are consumed purely
    /// as an ownership token: they are dropped immediately and all subsequent
    /// access goes through the channel handles. Soundness therefore relies on
    /// no other code calling `stm32ral::dma::DMA1::steal()` (or the DMAMUX1
    /// equivalent) for the lifetime of the program.
    pub fn new(_dma: dma::Instance, _mux: dmamux1::Instance) -> Self {
        rcc::enable_rst_dma1();
        rcc::enable_rst_dmamux1();
        Self { _priv: () }
    }

    /// Split into seven independent channel tokens.
    pub fn split(self) -> Dma1Channels {
        Dma1Channels {
            ch1: DmaChannel::new(),
            ch2: DmaChannel::new(),
            ch3: DmaChannel::new(),
            ch4: DmaChannel::new(),
            ch5: DmaChannel::new(),
            ch6: DmaChannel::new(),
            ch7: DmaChannel::new(),
        }
    }
}

/// Seven typed DMA1 channel tokens.
pub struct Dma1Channels {
    pub ch1: DmaChannel<1>,
    pub ch2: DmaChannel<2>,
    pub ch3: DmaChannel<3>,
    pub ch4: DmaChannel<4>,
    pub ch5: DmaChannel<5>,
    pub ch6: DmaChannel<6>,
    pub ch7: DmaChannel<7>,
}

/// Compile-time uniqueness token for DMA1 channel `N` (1..=7): the const
/// generic proves a channel is claimed once at the allocation site. Wraps
/// the runtime [`DmaCh`]; `erase()` unwraps it to operate.
pub struct DmaChannel<const N: u8> {
    inner: DmaCh,
}

impl<const N: u8> DmaChannel<N> {
    const fn new() -> Self {
        const { assert!(N >= 1 && N <= 7, "DmaChannel N must be 1..=7") };
        Self {
            inner: DmaCh {
                raw: RawChannel { n: N },
            },
        }
    }

    /// Consume the token → owned move-only [`DmaCh`] (preserves
    /// single-ownership at runtime).
    pub fn erase(self) -> DmaCh {
        self.inner
    }
}

// CCR field bit positions (identical for all CR{N}).
const CR_EN: u32 = 1 << 0;
const CR_TCIE: u32 = 1 << 1;
const CR_HTIE: u32 = 1 << 2;
const CR_TEIE: u32 = 1 << 3;
const CR_DIR: u32 = 1 << 4;
const CR_CIRC: u32 = 1 << 5;
const CR_MINC: u32 = 1 << 7;
const CR_PSIZE_SHIFT: u32 = 8;
const CR_MSIZE_SHIFT: u32 = 10;
const CR_PL_SHIFT: u32 = 12;

fn word_bits(w: Word) -> u32 {
    match w {
        Word::Byte => 0b00,
        Word::Half => 0b01,
        Word::Word => 0b10,
    }
}

fn priority_bits(p: Priority) -> u32 {
    match p {
        Priority::Low => 0b00,
        Priority::Medium => 0b01,
        Priority::High => 0b10,
        Priority::VeryHigh => 0b11,
    }
}

/// Typed refs to one channel's CR/NDTR/PAR/MAR — the sole channel→register
/// map. stm32ral has flat `CR1..CR7` (no array), hence the match; no
/// hand-computed offsets.
struct ChannelRegs {
    cr: &'static RWRegister<u32>,
    ndtr: &'static RWRegister<u32>,
    par: &'static RWRegister<u32>,
    mar: &'static RWRegister<u32>,
}

/// Crate-private register accessor. Minted
/// only from an owned [`DmaCh`]; holding one is delegated
/// access, not ownership (sharing serialized by RTIC priority + critical
/// sections).
#[derive(Clone)]
pub(crate) struct RawChannel {
    n: u8,
}

// The interrupt-safe register logic. `RawChannel` is the move-only owned
// channel handle (`Clone` only so the split ctors can mint one per-role
// handle; never `Copy`, never public).
impl RawChannel {
    #[inline]
    pub(crate) fn channel(&self) -> u8 {
        self.n
    }
    /// One-time DMAMUX/CR setup. Self-brackets the clock for its writes.
    pub(crate) fn configure(&mut self, req: Request, dir: Direction, word: Word, prio: Priority) {
        let n = self.n;
        dma1_clk_hold(n);
        // Route DMAMUX request line. DMAMUX channel index = n - 1.
        let mux = unsafe { &*dmamux1::DMAMUX1 };
        let req_id = (req.0 as u32) & 0x7F;
        match self.n {
            1 => write_reg!(dmamux1, mux, C0CR, req_id),
            2 => write_reg!(dmamux1, mux, C1CR, req_id),
            3 => write_reg!(dmamux1, mux, C2CR, req_id),
            4 => write_reg!(dmamux1, mux, C3CR, req_id),
            5 => write_reg!(dmamux1, mux, C4CR, req_id),
            6 => write_reg!(dmamux1, mux, C5CR, req_id),
            7 => write_reg!(dmamux1, mux, C6CR, req_id),
            _ => unreachable!(),
        }
        let mut ccr: u32 = 0;
        ccr |= word_bits(word) << CR_PSIZE_SHIFT;
        ccr |= word_bits(word) << CR_MSIZE_SHIFT;
        ccr |= priority_bits(prio) << CR_PL_SHIFT;
        ccr |= match dir {
            Direction::PeripheralToMemory => 0,
            Direction::MemoryToPeripheral => CR_DIR,
        };
        ccr |= CR_MINC; // always MINC for buffer transfers; PINC stays 0
        self.ch_regs().cr.write(ccr);
        dma1_clk_release(n);
    }

    /// Arm a one-shot M→P transfer (TCIE/TEIE; stale flags cleared before
    /// `EN=1`).
    ///
    /// # Safety
    /// Caller owns channel `n`; `buffer`/`len` valid for the transfer.
    pub(crate) unsafe fn start_oneshot(
        &mut self,
        peripheral_addr: u32,
        buffer: *const u8,
        len: usize,
    ) {
        debug_assert!(len <= u16::MAX as usize);
        dma1_clk_hold(self.n); // released by stop/cancel

        interrupt::free(|_| {
            let r = self.ch_regs();
            let cr = r.cr.read() & !CR_EN; // defensively disable
            r.cr.write(cr);
            r.par.write(peripheral_addr);
            r.mar.write(buffer as u32);
            r.ndtr.write(len as u32);
            self.clear_flags();
            let cr = (r.cr.read() & !CR_CIRC & !CR_HTIE) | CR_TCIE | CR_TEIE;
            fence(Ordering::SeqCst); // order source-buffer writes ahead of EN
            r.cr.write(cr | CR_EN);
        });
    }

    /// Read this channel's ISR flags.
    #[inline]
    pub(crate) fn flags(&self) -> ChannelFlags {
        let dma_inst = unsafe { &*dma::DMA1 };
        let isr = read_reg!(dma, dma_inst, ISR);
        let base = (self.n as u32 - 1) * 4;
        ChannelFlags {
            gif: isr & (1 << base) != 0,
            tcif: isr & (1 << (base + 1)) != 0,
            htif: isr & (1 << (base + 2)) != 0,
            teif: isr & (1 << (base + 3)) != 0,
        }
    }

    /// Clear this channel's four IFCR flags (write-1-to-clear).
    #[inline]
    pub(crate) fn clear_flags(&mut self) {
        let dma_inst = unsafe { &*dma::DMA1 };
        let mask: u32 = 0b1111 << ((self.n as u32 - 1) * 4);
        write_reg!(dma, dma_inst, IFCR, mask);
    }
    #[inline]
    /// CNDTR (remaining count); producer index in circular mode is `len - this`.
    pub(crate) fn ndtr(&self) -> u16 {
        (self.ch_regs().ndtr.read() & 0xFFFF) as u16
    }

    #[inline]
    fn ch_regs(&self) -> ChannelRegs {
        let r: &'static dma::RegisterBlock = unsafe { &*dma::DMA1 };
        match self.n {
            1 => ChannelRegs {
                cr: &r.CR1,
                ndtr: &r.NDTR1,
                par: &r.PAR1,
                mar: &r.MAR1,
            },
            2 => ChannelRegs {
                cr: &r.CR2,
                ndtr: &r.NDTR2,
                par: &r.PAR2,
                mar: &r.MAR2,
            },
            3 => ChannelRegs {
                cr: &r.CR3,
                ndtr: &r.NDTR3,
                par: &r.PAR3,
                mar: &r.MAR3,
            },
            4 => ChannelRegs {
                cr: &r.CR4,
                ndtr: &r.NDTR4,
                par: &r.PAR4,
                mar: &r.MAR4,
            },
            5 => ChannelRegs {
                cr: &r.CR5,
                ndtr: &r.NDTR5,
                par: &r.PAR5,
                mar: &r.MAR5,
            },
            6 => ChannelRegs {
                cr: &r.CR6,
                ndtr: &r.NDTR6,
                par: &r.PAR6,
                mar: &r.MAR6,
            },
            7 => ChannelRegs {
                cr: &r.CR7,
                ndtr: &r.NDTR7,
                par: &r.PAR7,
                mar: &r.MAR7,
            },
            _ => unreachable!("DMA1 channel must be 1..=7"),
        }
    }

    /// Disable the channel, wait for it to halt, drop its clock vote
    /// (idempotent). Leading `dma1_clk_hold` self-guards the register
    /// accesses on defensive paths where the clock may already be gated.
    /// EN-clear RMW is in a critical section; the drain busy-wait + IFCR
    /// clear are not (a wedged bus can't hold interrupts off).
    pub(crate) fn stop(&mut self) {
        let n = self.n;
        dma1_clk_hold(n);
        let r = self.ch_regs();
        interrupt::free(|_| {
            let cr = r.cr.read();
            r.cr.write(cr & !CR_EN);
        });
        while r.cr.read() & CR_EN != 0 {}
        self.clear_flags();
        dma1_clk_release(n);
    }

    /// Re-enable TC/TE interrupt enables (the one-shot ISR cleared them when
    /// it last fired). RMW in a critical section so an IRQ landing mid-RMW
    /// isn't lost. Used by [`Transfer`](oneshot::Transfer)'s poll.
    #[inline]
    pub(crate) fn reenable_tc_te_ie(&mut self) {
        let r = self.ch_regs();
        interrupt::free(|_| {
            let cr = r.cr.read();
            r.cr.write(cr | CR_TCIE | CR_TEIE);
        });
    }

    /// Disable TC/TE IE so the one-shot channel can't re-fire before the
    /// future polls. The one-shot ISR entry; flags are cleared by the
    /// future, not here.
    #[inline]
    pub(crate) fn disable_tc_te_ie(&mut self) {
        let r = self.ch_regs();
        interrupt::free(|_| {
            let cr = r.cr.read();
            r.cr.write(cr & !(CR_TCIE | CR_TEIE));
        });
    }

    /// Cancel a one-shot transfer: clear EN/TC/TE-IE (critical section),
    /// busy-wait `EN==0` so a beat in flight has truly quiesced, clear
    /// flags, then a trailing `fence` so no buffer access is reordered
    /// before the EN-cleared point. Used by [`Transfer`](oneshot::Transfer)'s
    /// `Drop`.
    pub(crate) fn cancel(&mut self) {
        let n = self.n;
        dma1_clk_hold(n); // self-guard; released below
        let r = self.ch_regs();
        interrupt::free(|_| {
            let cr = r.cr.read();
            r.cr.write(cr & !(CR_EN | CR_TCIE | CR_TEIE));
        });
        while r.cr.read() & CR_EN != 0 {}
        self.clear_flags();
        fence(Ordering::SeqCst);
        dma1_clk_release(n);
    }

    /// Clear `EN` from a non-owner context *if* the clock is on
    ///
    /// Any code path that disables the clock must first halt the transfer,
    /// so we don't have to do anything if clock is off.
    pub(crate) fn halt_if_clocked(&mut self) {
        let bit = 1u8 << (self.n - 1);
        interrupt::free(|_| {
            if DMA1_CLK_HOLD.load(Ordering::Relaxed) & bit != 0 {
                let r = self.ch_regs();
                let cr = r.cr.read();
                r.cr.write(cr & !CR_EN);
            }
        });
    }
}

/// Owned, move-only runtime handle to one DMA1 channel (only via
/// [`DmaChannel::erase`]; not `Copy`/`Clone` so single-ownership is
/// preserved at runtime). Internal multi-context use goes via the
/// crate-private [`raw`](Self::raw).
pub struct DmaCh {
    raw: RawChannel,
}

impl DmaCh {
    /// The 1-based DMA1 channel number (useful for logging).
    #[inline]
    pub fn channel(&self) -> u8 {
        self.raw.channel()
    }

    /// Configure DMAMUX request, direction, word size, priority. Channel
    /// left disabled. Must not be called while the channel is enabled.
    pub fn configure(&mut self, req: Request, dir: Direction, word: Word, priority: Priority) {
        self.raw.configure(req, dir, word, priority);
    }

    /// Internal accessor for raw channel
    #[inline]
    fn raw(&mut self) -> &mut RawChannel {
        &mut self.raw
    }

    /// Arm a circular P→M transfer (HTIE/TCIE/TEIE; stale flags cleared before
    /// `EN=1`). `EN` is defensively cleared first (as in `start_oneshot`) so
    /// reconfiguring PAR/MAR/NDTR is always done with the channel disabled —
    /// RM0432 requires that, and it makes a re-arm racing a concurrent
    /// `stop()` harmless. The descriptor writes + CR RMW are in a critical
    /// section so a preempting context that also touches this channel's CR
    /// cannot corrupt it.
    ///
    /// # Safety
    /// Caller owns channel `n`; `buffer`/`len` valid until stopped/reconfigured.
    unsafe fn start_circular(&mut self, peripheral_addr: u32, buffer: *mut u8, len: usize) {
        debug_assert!(len <= u16::MAX as usize);
        dma1_clk_hold(self.raw.channel()); // released by stop
        interrupt::free(|_| {
            let r = self.raw().ch_regs();
            let cr = r.cr.read() & !CR_EN; // defensively disable before reconfig
            r.cr.write(cr);
            r.par.write(peripheral_addr);
            r.mar.write(buffer as u32);
            r.ndtr.write(len as u32);
            self.raw.clear_flags();
            let cr = (r.cr.read() & !CR_EN) | CR_CIRC | CR_HTIE | CR_TCIE | CR_TEIE;
            fence(Ordering::SeqCst); // order descriptor/buffer writes ahead of EN
            r.cr.write(cr | CR_EN);
        });
    }
}

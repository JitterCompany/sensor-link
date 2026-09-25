//! Async one-shot Transfer layer.
//!
//! Intended fore one-shot transfers to/from a peripheral (e.g UART)
//! [`OneShot`] can create [`Transfer`]s that represent a fixed-length
//! one-shot data transfer to/from a peripheral.

use core::{
    future::Future,
    marker::PhantomData,
    pin::Pin,
    sync::atomic::{fence, AtomicBool, Ordering},
    task::{Context, Poll},
};

use embassy_sync::waitqueue::AtomicWaker;

use super::{DmaCh, RawChannel};

static DMA1_WAKERS: [AtomicWaker; 7] = [const { AtomicWaker::new() }; 7];

/// Per-channel "abort the in-flight `Transfer`" request. Set by
/// [`OneShotAbort::abort`] (any context), consumed by [`Transfer::poll`]
/// (the single owner that releases the clock + fully quiesces the channel).
/// Cleared by [`OneShot::write`] so a stale request can't kill a fresh
/// transfer.
static ONESHOT_ABORT: [AtomicBool; 7] = [const { AtomicBool::new(false) }; 7];

/// Error surfaced by a [`Transfer`].
#[derive(Debug)]
pub enum Error {
    /// TEIF was asserted: a forbidden bus access (bad peripheral/memory
    /// pointer or alignment).
    TransferError,
    /// The channel was disabled out from under the transfer before it
    /// completed (e.g. another handle's `stop()` / a suspend). The future
    /// resolves with this rather than hanging on a stopped channel — the
    /// `oneshot` API is self-terminating, independent of any consumer-side
    /// "suspended" flag.
    Aborted,
}

/// A one-shot DMA transfer in flight: `await` drives it to completion,
/// `Drop` cancels and waits for the controller to quiesce. The channel
/// must be [`configure`](DmaCh::configure)d first; `'a` ties
/// the channel borrow and buffer together.
#[must_use = "Transfer must be awaited or explicitly dropped"]
pub struct Transfer<'a> {
    ch: &'a mut DmaCh,
    _buffer: PhantomData<&'a mut [u8]>,
}

impl<'a> Transfer<'a> {
    /// One-shot M→P. Channel must be configured `MemoryToPeripheral`.
    ///
    /// # Safety
    /// `peri_addr` a valid byte-wide data register and `mem`/`len` a valid
    /// readable region for the transfer's duration. **Leak-amplification:**
    /// if leaked instead of dropped, `mem` must stay valid for the rest of
    /// the program (`Drop` is the only stop path) — [`OneShot::write`]
    /// discharges this with its owned `'static` scratch.
    pub(crate) unsafe fn new_write(
        ch: &'a mut DmaCh,
        peri_addr: u32,
        mem: *const u8,
        len: usize,
    ) -> Self {
        ch.raw().start_oneshot(peri_addr, mem, len);
        Self {
            ch,
            _buffer: PhantomData,
        }
    }
}

impl Future for Transfer<'_> {
    type Output = Result<(), Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let raw = this.ch.raw();
        let n = raw.channel();
        DMA1_WAKERS[(n - 1) as usize].register(cx.waker());

        let f = raw.flags();
        if f.teif {
            // Acknowledge + stop. `stop()` clears flags.
            raw.stop();
            fence(Ordering::SeqCst);
            return Poll::Ready(Err(Error::TransferError));
        }
        if f.tcif {
            raw.clear_flags();
            // Ensure the consumer's subsequent reads see what DMA wrote.
            fence(Ordering::SeqCst);
            return Poll::Ready(Ok(()));
        }
        // Abort requested (`OneShotAbort::abort`). It may have halted EN
        // already; the owner does the full quiesce + clock release here.
        // After TCIF/TEIF so a genuine completion still wins.
        if ONESHOT_ABORT[(n - 1) as usize].swap(false, Ordering::Acquire) {
            raw.stop();
            fence(Ordering::SeqCst);
            return Poll::Ready(Err(Error::Aborted));
        }

        // Re-enable the IRQ (the ISR cleared TCIE/TEIE when it last fired).
        raw.reenable_tc_te_ie();
        Poll::Pending
    }
}

impl Drop for Transfer<'_> {
    fn drop(&mut self) {
        // The synchronous stop: `cancel()` quiesces the channel + drops its
        // clock vote (idempotent).
        self.ch.raw().cancel();
    }
}

/// Owning one-shot channel handle bound to a fixed peripheral data
/// register: mints a [`Transfer`] per call. Move-only (wraps the
/// non-`Clone` [`DmaCh`]), so single-ownership is preserved at runtime —
/// symmetric with [`CircularRx`](super::CircularRx).
pub(crate) struct OneShot {
    ch: DmaCh,
    /// Peripheral data register, fixed at construction (one channel serves
    /// one peripheral). Saves threading it through every transfer call.
    peri: u32,
    /// Owned `'static` TX scratch (DMA reads it during a write). Held as a
    /// raw pointer for the same reason as the circular ring — never a
    /// reference while the engine is active.
    scratch: *mut u8,
    scratch_len: usize,
}

// SAFETY: `scratch` targets a `'static` buffer of which `OneShot` is the
// unique (move-only) owner; staging into it and the DMA read of it are
// serialized by `&mut self` / the `Transfer` lifetime, and only the DMA
// engine (not Rust code) reads it concurrently. No `&`/`&mut` is formed
// over it while a transfer is live.
unsafe impl Send for OneShot {}

impl OneShot {
    /// **Consume** the channel and split into the owning handle plus the
    /// two non-owning role handles (abort, DMA ISR). `peri_addr` is
    /// the peripheral data register every transfer targets; `scratch` is
    /// the `'static` buffer writes are staged through (owned here,
    /// mirroring the RX ring on [`CircularRx`](super::CircularRx)).
    ///
    /// # Safety
    /// `peri_addr` must be a valid byte-wide data register for the
    /// channel's configured direction, and `ch` must already be
    /// [`configure`](DmaCh::configure)d. This is the sole unsafe boundary;
    /// once constructed, [`write`](Self::write) is safe.
    pub(crate) unsafe fn new(
        mut ch: DmaCh,
        peri_addr: u32,
        scratch: &'static mut [u8],
    ) -> (Self, OneShotAbort, OneShotDmaIrq) {
        let raw = (*ch.raw()).clone();
        (
            OneShot {
                ch,
                peri: peri_addr,
                scratch: scratch.as_mut_ptr(),
                scratch_len: scratch.len(),
            },
            OneShotAbort { ch: raw.clone() },
            OneShotDmaIrq { ch: raw },
        )
    }

    /// Largest `src` a single [`write`](Self::write) accepts (the scratch
    /// size) — callers chunk to this.
    #[inline]
    pub(crate) fn scratch_len(&self) -> usize {
        self.scratch_len
    }

    /// Stage `src` into the owned scratch and start a one-shot M→P transfer.
    /// Clock vote is internal to the channel primitives (taken by
    /// `start_oneshot`, dropped by the `Transfer`'s teardown).
    ///
    /// # Panics
    /// If `src.len() > scratch_len()` (chunk to [`scratch_len`](Self::scratch_len)).
    #[inline]
    pub(crate) fn write(&mut self, src: &[u8]) -> Transfer<'_> {
        assert!(src.len() <= self.scratch_len, "src exceeds OneShot scratch");
        let n = self.ch.channel();
        // Clear any stale abort request from a previous (already-finished)
        // transfer so it can't immediately kill this fresh one.
        ONESHOT_ABORT[(n - 1) as usize].store(false, Ordering::Release);
        // SAFETY: `src`/`scratch` are distinct regions (the scratch ptr is
        // never exposed, so no caller slice can alias it) and `src.len()`
        // fits (asserted); `peri` was validated in `new` and the scratch
        // is owned `'static` — discharging `new_write`'s contract.
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), self.scratch, src.len());
            Transfer::new_write(
                &mut self.ch,
                self.peri,
                self.scratch as *const u8,
                src.len(),
            )
        }
    }
}

/// Non-owning, abort-only handle for a one-shot channel, for a control
/// context that must tear down an in-flight `Transfer` it can't reach to
/// drop (normal cancellation is dropping the `Transfer`). It cannot start
/// transfers and does not gate new ones — best-effort. Move-only.
pub(crate) struct OneShotAbort {
    ch: RawChannel,
}

impl OneShotAbort {
    /// Halt DMA activity now
    ///
    /// If any transfer is live, it willl resolve [`Error::Aborted`]
    /// upon its next poll. Dropping the transfer then fully turns off the clock.
    /// `abort` = halted on return, teardown = quiesced.
    #[inline]
    pub(crate) fn abort(&mut self) {
        let n = self.ch.channel();
        ONESHOT_ABORT[(n - 1) as usize].store(true, Ordering::Release);
        self.ch.halt_if_clocked();
        DMA1_WAKERS[(n - 1) as usize].wake();
    }
}

/// One-shot DMA-ISR handle
///
/// The application binds one of these to the TX
/// channel's `DMA1_CHx` vector (re-exported as `UartTxDmaISR`). Move-only.
pub struct OneShotDmaIrq {
    ch: RawChannel,
}

impl OneShotDmaIrq {
    /// # Safety
    /// Must only be called from the DMA1 channel vector for this channel.
    pub unsafe fn handle_interrupt(&mut self) {
        // disable so the channel can't re-fire before the future polls, then wake the future.
        // Flags are cleared by the future, not here.
        self.ch.disable_tc_te_ie();
        DMA1_WAKERS[(self.ch.channel() - 1) as usize].wake();
    }
}

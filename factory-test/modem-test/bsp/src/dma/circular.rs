//! Circular RX layer — a purpose-built streaming circular DMA receiver
//! (peripheral → ring). The UART RX path is its only consumer;
//! SPI etc. use the one-shot [`Transfer`](super::Transfer) instead (their
//! transfer size is known up front). Encapsulating the ring here keeps the
//! UART driver free of low-level DMA / register / CNDTR-accounting code.
//!
//! Construction yields four *distinct, move-only* role handles
//! (`CircularRx` consumer, `CircularRxControl`, `CircularRxDmaIrq`,
//! `CircularRxLineIrq`), each moved into exactly one RTIC context — no
//! `Copy` blob to misuse. Shared state is only the genuinely-concurrent
//! [`CircShared`] (per RX stream, caller-provided `&'static`); all config /
//! bookkeeping is consumer-owned. Control↔consumer hand-off is a lock-free
//! CAS state machine ([`st`]): one register/clock mutator per step, correct
//! under any RTIC task layout.

use core::{
    sync::atomic::{fence, AtomicBool, AtomicU32, AtomicU8, Ordering},
    task::Waker,
};

use embassy_sync::waitqueue::AtomicWaker;

use super::{DmaCh, RawChannel};

/// Control↔consumer coordination (`CircShared::state`); all transitions by
/// `compare_exchange`. The context that wins a transition is the sole
/// register/clock mutator for that step.
mod st {
    /// Not armed; clock not held (initial / suspended / post-stop).
    pub const IDLE: u8 = 0;
    /// Armed & running; clock held; no consumer mid-register-access.
    pub const ARMED: u8 = 1;
    /// Consumer is inside its DMA-register span (the "busy"/pseudo-
    /// critical marker). Control must not touch registers/clock.
    pub const READING: u8 = 2;
    /// A stop was parked while the consumer was `READING`; the consumer
    /// performs it when it exits (bounded to the in-progress read).
    pub const STOP_REQ: u8 = 3;
    /// (Re)arm requested; the consumer arms on its next read.
    pub const REARM_REQ: u8 = 4;
}

/// Shared state for one circular-RX stream. Caller-provided `&'static`
/// (one per RX stream, not a global per-channel array). Every field is
/// potentially touched from more than one context (consumer / control /
/// DMA ISR / out-of-band signaller).
pub(crate) struct CircShared {
    /// Wrap count maintained by the DMA ISR (one per `TC`). Redundant with
    /// the consumer's own count; `observe` takes whichever leads, so
    /// detection holds if *either* keeps up.
    isr_laps: AtomicU32,
    /// Sticky overrun: TEIF / external ORE / coalesced-ISR / overtake.
    overrun: AtomicBool,
    waker: AtomicWaker,
    /// Control↔consumer coordination ([`st`] values). Lock-free CAS;
    /// the owner of each transition is the sole register/clock mutator.
    state: AtomicU8,
}

impl CircShared {
    pub(crate) const fn new() -> Self {
        Self {
            isr_laps: AtomicU32::new(0),
            overrun: AtomicBool::new(false),
            waker: AtomicWaker::new(),
            state: AtomicU8::new(st::IDLE),
        }
    }
}

/// Outcome of a non-blocking [`CircularRx::try_read`].
pub enum TryRead {
    /// `n` bytes copied out.
    Bytes(usize),
    /// Nothing buffered yet.
    Empty,
    /// The producer overtook the unread region (or a bus / hardware
    /// overrun was signalled). Consumer state has been resynced; no
    /// corrupted bytes were returned.
    Overrun,
}

/// Consumer half of the streaming circular DMA receiver
///
/// Move-only: only one consumer can exist per DMA channel.
pub struct CircularRx {
    ch: DmaCh,
    st: &'static CircShared,
    /// Base of the `'static` DMA ring. Held as a raw pointer (never a
    /// reference) because the DMA engine mutates the region concurrently;
    /// reads go via `copy_nonoverlapping` of bytes already behind the
    /// producer cursor.
    buf: *mut u8,
    len: usize,
    peri: u32,
    // Consumer-private wrap bookkeeping; no other context touches these.
    cons_laps: u32,
    cons_prev_ndtr: u32,
    consumed: u32,
}

// SAFETY: `buf` targets a `'static` buffer of which `CircularRx` is the
// unique (move-only) owner; the only other accessor is the DMA engine, not
// Rust code, and consumer reads vs producer writes are kept to disjoint
// byte ranges by the overrun protocol. No `&`/`&mut` is ever formed over
// the live region. Moving the handle between RTIC tasks is therefore
// sound.
unsafe impl Send for CircularRx {}

impl CircularRx {
    /// Split into the four role handles. The channel is **not** armed here
    /// — it is unarmed at construction and armed lazily by the consumer on
    /// its first read after a [`CircularRxControl::request_rearm`]. Channel
    /// must already be [`configure`](DmaCh::configure)d `PeripheralToMemory`.
    ///
    /// # Safety
    /// `peri_addr` a valid byte-wide register; `buf` is `'static` and runs
    /// until `stop` (no leak-amplification hazard). `st` must be dedicated
    /// to this stream (one [`CircShared`] per RX channel).
    pub(crate) unsafe fn new(
        mut ch: DmaCh,
        peri_addr: u32,
        buf: &'static mut [u8],
        st: &'static CircShared,
    ) -> (Self, CircularRxControl, CircularRxDmaIrq, CircularRxLineIrq) {
        let len = buf.len();
        // Unarmed at construction; first read after a `request_rearm` arms it.
        st.state.store(st::REARM_REQ, Ordering::Release);
        let rx_ch = (*ch.raw()).clone();
        let dma_ch = rx_ch.clone();
        let consumer = Self {
            ch,
            st,
            buf: buf.as_mut_ptr(),
            len,
            peri: peri_addr,
            cons_laps: 0,
            cons_prev_ndtr: len as u32,
            consumed: 0,
        };
        (
            consumer,
            CircularRxControl { ch: rx_ch, st },
            CircularRxDmaIrq { ch: dma_ch, st },
            CircularRxLineIrq { st },
        )
    }

    /// (Re)arm: reset accounting + `start_circular`. Sole arming site.
    fn arm(&mut self) {
        self.st.isr_laps.store(0, Ordering::SeqCst);
        self.st.overrun.store(false, Ordering::SeqCst);
        self.cons_laps = 0;
        self.cons_prev_ndtr = self.len as u32; // arms with NDTR = len
        self.consumed = 0;
        // SAFETY: `buf`/`len` from the `'static` slice handed to `new`;
        // `peri` is the caller-validated register addr; we own the channel.
        unsafe { self.ch.start_circular(self.peri, self.buf, self.len) };
    }

    /// Stop the channel, then settle `STOP_REQ→IDLE` (a raced re-arm is left
    /// so the next read re-arms). Sole consumer-side stop site.
    fn do_stop(&mut self) {
        self.ch.raw().stop();
        let _ = self.st.state.compare_exchange(
            st::STOP_REQ,
            st::IDLE,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    /// Enter the `READING` span (CAS `ARMED→READING`): on success the channel
    /// is armed + clocked for the whole span. On failure, call
    /// `service_not_armed` and touch no registers.
    #[inline]
    fn enter_reading(&mut self) -> bool {
        self.st
            .state
            .compare_exchange(st::ARMED, st::READING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Handle the non-`ARMED` states without touching DMA registers (the
    /// clock is not guaranteed). `REARM_REQ` ⇒ arm now (consumer is the
    /// single arming context); a stop racing the arm is honored.
    fn service_not_armed(&mut self) {
        match self.st.state.load(Ordering::Acquire) {
            st::REARM_REQ => {
                self.arm();
                if self
                    .st
                    .state
                    .compare_exchange(
                        st::REARM_REQ,
                        st::ARMED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_err()
                {
                    match self.st.state.load(Ordering::Acquire) {
                        // Stop parked, or control cancelled the rearm → undo.
                        st::STOP_REQ | st::IDLE => self.do_stop(),
                        // Another rearm: stay armed.
                        _ => self.st.state.store(st::ARMED, Ordering::Release),
                    }
                }
            }
            // Normally handled at the READING exit; defensive here.
            st::STOP_REQ => self.do_stop(),
            // IDLE / ARMED / READING: nothing to do.
            _ => {}
        }
    }

    /// Leave the `READING` span (CAS `READING → ARMED`). Returns whether
    /// the just-computed result is still valid: `true` for a clean exit or
    /// a parked stop (the bytes were read with the clock on — perform the
    /// stop, keep the result); `false` if a re-arm/reconfigure happened
    /// under us (discard the possibly-stale result).
    fn exit_reading(&mut self) -> bool {
        match self.st.state.compare_exchange(
            st::READING,
            st::ARMED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => true,
            Err(st::STOP_REQ) => {
                self.do_stop();
                true
            }
            Err(st::REARM_REQ) => false,
            Err(_) => true,
        }
    }

    /// Live producer byte position `laps*len + (len - CNDTR)` (monotonic,
    /// `wrapping_*`; idempotent within a `CNDTR`). Wrap = `CNDTR` rose
    /// since last look, combined with the redundant ISR counter (whichever
    /// leads).
    fn observe(&mut self) -> u32 {
        let len = self.len as u32;
        // Stable CNDTR (decrements live; re-read until two agree).
        let mut c = self.ch.raw().ndtr() as u32;
        loop {
            let c2 = self.ch.raw().ndtr() as u32;
            if c == c2 {
                break;
            }
            c = c2;
        }
        let mut laps = self.cons_laps;
        if c > self.cons_prev_ndtr {
            laps = laps.wrapping_add(1); // ≥1 wrap (ISR/`>len` covers >1)
        }
        self.cons_prev_ndtr = c;
        // Adopt the ISR counter if it leads (tiny wrapping delta; <2^31 ⇒
        // isr ≥ cons), keeping the combined count monotone.
        let isr = self.st.isr_laps.load(Ordering::Acquire);
        if isr.wrapping_sub(laps) < (1u32 << 31) {
            laps = isr;
        }
        self.cons_laps = laps;
        let w = (len - c) % len; // c ∈ [0..=len]; both ends ⇒ 0 into lap
        laps.wrapping_mul(len).wrapping_add(w)
    }

    /// Register the consumer waker (woken by the channel ISR / external
    /// sources).
    #[inline]
    pub(crate) fn register_waker(&self, w: &Waker) {
        self.st.waker.register(w);
    }

    /// True if unread bytes are buffered.
    #[inline]
    pub fn data_ready(&mut self) -> bool {
        if !self.enter_reading() {
            self.service_not_armed();
            return false;
        }
        let produced = self.observe();
        let has = produced != self.consumed;
        if self.exit_reading() {
            has
        } else {
            false // reconfigured under us; report no data
        }
    }

    /// Non-blocking read of up to `dst.len()` bytes.
    ///
    /// A [`TryRead::Bytes`] result is never torn (unread distance revalidated
    /// after the copy); any lap / `TEIF` / signalled `ORE` is reported as
    /// `Overrun` + resync, detected by two independent observers (the
    /// `CNDTR` position and the ISR lap counter).
    pub fn try_read(&mut self, dst: &mut [u8]) -> TryRead {
        // Enter the pseudo-critical span (clock guaranteed on, channel
        // armed) or service a non-armed state without touching registers.
        if !self.enter_reading() {
            self.service_not_armed();
            return TryRead::Empty;
        }
        let result = self.read_in_reading(dst);
        // Single exit: settle the state (and honor a parked stop). If a
        // reconfigure raced under us, discard the possibly-stale result.
        if self.exit_reading() {
            result
        } else {
            TryRead::Empty
        }
    }

    /// All DMA-register-touching read work; runs only inside the `READING`
    /// span. No early state mutation — the caller's `exit_reading` is the
    /// single exit.
    fn read_in_reading(&mut self, dst: &mut [u8]) -> TryRead {
        if self.st.overrun.swap(false, Ordering::SeqCst) {
            return TryRead::Overrun;
        }
        let produced = self.observe();
        let len = self.len;
        let consumed = self.consumed;
        let avail = produced.wrapping_sub(consumed);

        if avail as usize > len {
            // Producer overtook the unread region; resync, don't hand back
            // overwritten bytes.
            self.consumed = produced;
            return TryRead::Overrun;
        }
        if avail == 0 {
            return TryRead::Empty;
        }

        let to_copy = (avail as usize).min(dst.len());
        let read_head = (consumed as usize) % len;
        fence(Ordering::Acquire); // producer read ahead of ring reads
        let ring = self.buf as *const u8;
        let first = (len - read_head).min(to_copy);
        // SAFETY: `ring`/`len` describe the live `'static` DMA buffer;
        // `read_head + to_copy` wraps within it.
        unsafe {
            core::ptr::copy_nonoverlapping(ring.add(read_head), dst.as_mut_ptr(), first);
            if to_copy > first {
                core::ptr::copy_nonoverlapping(ring, dst.as_mut_ptr().add(first), to_copy - first);
            }
        }

        // Revalidate: a lap during the copy would have torn these bytes.
        fence(Ordering::Acquire);
        let produced2 = self.observe();
        if produced2.wrapping_sub(consumed) as usize > len
            || self.st.overrun.swap(false, Ordering::SeqCst)
        {
            self.consumed = produced2;
            return TryRead::Overrun;
        }

        self.consumed = consumed.wrapping_add(to_copy as u32);
        TryRead::Bytes(to_copy)
    }
}

/// Control-side handle: `stop` / `request_rearm`. Mutates the channel/clock
/// only when it wins `ARMED→IDLE`; otherwise parks a request the running
/// consumer honors. Move-only.
pub(crate) struct CircularRxControl {
    ch: RawChannel,
    st: &'static CircShared,
}

impl CircularRxControl {
    /// Stop the RX channel. Wins `ARMED→IDLE` ⇒ stop synchronously;
    /// `READING` ⇒ halt `EN` now (`halt_if_clocked`) and park `STOP_REQ` for
    /// the running consumer to do the full teardown (quiesce + clock release)
    /// at its bounded read-exit; `REARM_REQ` ⇒ cancel it. Mirrors
    /// [`OneShotAbort::abort`](super::OneShotAbort) — abort now, defer the
    /// disable.
    pub(crate) fn stop(&mut self) {
        loop {
            match self.st.state.compare_exchange(
                st::ARMED,
                st::IDLE,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    // not mid-access → stop synchronously
                    self.ch.stop();
                    return;
                }
                Err(st::READING) => {
                    if self
                        .st
                        .state
                        .compare_exchange(
                            st::READING,
                            st::STOP_REQ,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        // Halt EN now (clock vote stays held, so the frozen
                        // reader's `observe()` is still clocked); the reader
                        // does the full teardown at its read-exit.
                        self.ch.halt_if_clocked();
                        return;
                    }
                    // consumer left READING → retry
                }
                Err(st::REARM_REQ) => {
                    // Not armed yet (no clock held by control); cancel.
                    if self
                        .st
                        .state
                        .compare_exchange(
                            st::REARM_REQ,
                            st::IDLE,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return;
                    }
                }
                Err(_) => return, // IDLE / STOP_REQ: already down/parked
            }
        }
    }

    /// Signal the consumer to (re)arm on its next read. Pure signal — no
    /// register/clock access.
    #[inline]
    pub(crate) fn request_rearm(&mut self) {
        self.st.state.store(st::REARM_REQ, Ordering::Release);
        self.st.waker.wake();
    }
}

/// DMA-channel-ISR handle: redundant wrap counter + error flagger + waker.
/// The application binds one of these to the RX channel's `DMA1_CHx`
/// vector (re-exported as `UartRxDmaISR`). Move-only.
pub struct CircularRxDmaIrq {
    ch: RawChannel,
    st: &'static CircShared,
}

impl CircularRxDmaIrq {
    /// DMA channel ISR: waker, error flagger, redundant wrap counter (`+1`
    /// per `TC`). No byte accounting.
    ///
    /// # Safety
    /// Must only be called from the DMA1 channel vector for this channel.
    pub unsafe fn handle_interrupt(&mut self) {
        let flags = self.ch.flags();
        self.ch.clear_flags();

        if flags.teif {
            self.st.overrun.store(true, Ordering::SeqCst);
        } else if flags.htif && flags.tcif {
            // Coalesced ≥2 firings ⇒ a TC may have been missed ⇒ overrun.
            self.st.overrun.store(true, Ordering::Release);
        } else if flags.tcif {
            self.st.isr_laps.fetch_add(1, Ordering::Release); // one wrap
        }
        self.st.waker.wake();
    }
}

/// Out-of-band signaller: lets a context other than the DMA channel ISR
/// wake the consumer or flag an externally-detected overrun. No channel /
/// register access. Move-only.
pub(crate) struct CircularRxLineIrq {
    st: &'static CircShared,
}

impl CircularRxLineIrq {
    /// Wake the consumer from an out-of-band source (e.g. a peripheral
    /// line IRQ on receive-timeout / idle).
    #[inline]
    pub(crate) fn wake(&mut self) {
        self.st.waker.wake();
    }

    /// Flag an externally-detected overrun (e.g. a peripheral overrun flag).
    #[inline]
    pub(crate) fn signal_overrun(&mut self) {
        self.st.overrun.store(true, Ordering::SeqCst);
        self.st.waker.wake();
    }
}

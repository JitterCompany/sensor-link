use core::{
    future::{poll_fn, Future},
    ops::Deref,
    pin::pin,
    sync::atomic::{AtomicU8, Ordering},
    task::Poll,
};

use cortex_m::interrupt;
use embassy_sync::waitqueue::AtomicWaker;

use embedded_io_async::{ErrorType, Read, ReadReady, Write};
use stm32ral::{modify_reg, read_reg, reset_reg, usart, write_reg};

use crate::{
    dma::{
        CircShared, CircularRx, CircularRxControl, CircularRxDmaIrq, CircularRxLineIrq, Direction,
        DmaChannel, OneShot, OneShotAbort, OneShotDmaIrq, Priority, Request, TryRead, Word,
    },
    gpio::{Pin, PullupConfig},
    rcc::{self, Clocks},
};
use sensor_link_firmware::traits::{BaudRateControl, Suspend};
use static_cell::StaticCell;

/// Async UART driver with DMA-driven RX (circular ring) and TX (one-shot).
///
/// Can be used via embedded_io [Read](embedded_io_async::Read) and [Write](embedded_io_async::Write)
/// traits and can be split into [UartControl], [Tx], and [Rx] parts.
///
/// Three IRQ contexts are produced at construction time and must each be wired
/// to their respective vector by the application:
/// - [UartISR] → `USART{2,3}` vector (handles RTOF/IDLE/error frames; also signals resume).
/// - [UartRxDmaISR] → the `DMA1_CHx` vector for the RX channel passed to
///   [`UART::new`] (lap detection via HT/TC, transfer errors).
/// - [UartTxDmaISR] → the `DMA1_CHx` vector for the TX channel passed to
///   [`UART::new`] (TX-complete wakeup).
///
/// The channels are chosen by the caller (const generics on [`DmaChannel`]):
/// USART3/modem uses CH1+CH2; a second USART could use CH3+CH4. The
/// application must wire each `DMA1_CHx` RTIC vector to the matching ISR.
pub struct UART {
    control: UartControl,
    tx: Tx,
    rx: Rx,
}

/// USART line-event handler. Wakes the RX/TX/resume wakers on
/// RTOF/IDLE/errors/TC and signals the resume-state transition.
pub struct UartISR {
    id: UartID,
    rx: CircularRxLineIrq,
}

/// DMA RX channel handler — bind to the RX channel's `DMA1_CHx` vector
/// and call `handle_interrupt()`. Just the dma-layer role handle.
pub type UartRxDmaISR = CircularRxDmaIrq;

/// DMA TX channel handler — bind to the TX channel's `DMA1_CHx` vector
/// and call `handle_interrupt()`. Just the dma-layer role handle.
pub type UartTxDmaISR = OneShotDmaIrq;

/// Uart 'control' part of the driver.
///
/// Used for all control/config except transmit/receive.
pub struct UartControl {
    id: UartID,
    _rx: Pin,
    tx: Pin,

    /// RX ring control — re-arm on resume/baud-change, stop on suspend.
    rx: CircularRxControl,
    /// TX one-shot abort — halt the in-flight TX transfer on suspend (the
    /// owned `OneShot` lives in [`Tx`]). New TX writes are gated by
    /// `is_suspended()`, not by this handle.
    tx_abort: OneShotAbort,

    /// Target baudrate (Hz)
    target_baudrate: u32,

    /// Actual baudrate (Hz). May deviate from target due to clock divider rounding errors
    actual_baudrate: u32,

    format: UartFormat,
}

const RX_BUFFER_LEN: usize = 2 * 1024;
const TX_SCRATCH_LEN: usize = 256;

/// UART Read part
pub struct Rx {
    id: UartID,
    rx: CircularRx,
    error_occurred: bool,
}

/// UART Write part
pub struct Tx {
    id: UartID,
    /// Owned, move-only one-shot TX channel — `tx.write()` mints a fresh
    /// [`Transfer`] per `write`. Delegated abort / ISR go via the move-only
    /// `OneShotAbort` / `OneShotDmaIrq` handles split off at construction.
    tx: OneShot,
}

#[repr(u8)]
enum SuspendState {
    Active = 1,
    Suspended = 2,
    Resuming = 3,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UartFormat {
    /// 8N1 - 8 data bits, no parity
    Bits8NoParity,

    /// 8E1 - 8 data bits, even parity
    Bits8EvenParity,

    /// 8O1 - 8 data bits, odd parity
    Bits8OddParity,
}

struct SharedUartState {
    suspended: AtomicU8,
    /// Woken by the USART ISR on `TC` (used by `Tx::flush`/`wait_for_tc`)
    /// and by `suspend()` to unstick a pending `Tx::write`.
    tx_waker: AtomicWaker,
    /// Woken by the USART ISR once the resume transition completes.
    resume_waker: AtomicWaker,
    /// Backing storage for the RX / TX (handled via dma)
    rx_ring: StaticCell<[u8; RX_BUFFER_LEN]>,
    tx_scratch: StaticCell<[u8; TX_SCRATCH_LEN]>,
    circ_shared: CircShared,
}

impl SharedUartState {
    const fn new() -> Self {
        Self {
            suspended: AtomicU8::new(SuspendState::Suspended as u8),
            tx_waker: AtomicWaker::new(),
            resume_waker: AtomicWaker::new(),
            rx_ring: StaticCell::new(),
            tx_scratch: StaticCell::new(),
            circ_shared: CircShared::new(),
        }
    }
}

static SHARED: [SharedUartState; 1] = [SharedUartState::new()];

#[derive(Debug, Clone, Copy, PartialEq)]
enum UartID {
    /// USART3: modem (EC2x)
    U3,
}

impl UartID {
    fn from_instance(inst: &usart::Instance) -> Self {
        match inst.deref() as *const _ {
            usart::USART3 => UartID::U3,
            _ => panic!("UART must be USART3"),
        }
    }

    const fn as_interrupt(&self) -> stm32ral::Interrupt {
        match self {
            UartID::U3 => stm32ral::Interrupt::USART3,
        }
    }

    #[inline]
    const fn register_block(&self) -> *const usart::RegisterBlock {
        match self {
            UartID::U3 => usart::USART3,
        }
    }

    /// Address of the USART RDR register (DMA peripheral address for RX).
    #[inline]
    fn rdr_addr(&self) -> u32 {
        // Take the address of the stm32ral-typed field rather than
        // hand-computing an offset from the block base.
        unsafe { core::ptr::addr_of!((*self.register_block()).RDR) as u32 }
    }

    /// Address of the USART TDR register (DMA peripheral address for TX).
    #[inline]
    fn tdr_addr(&self) -> u32 {
        unsafe { core::ptr::addr_of!((*self.register_block()).TDR) as u32 }
    }

    #[inline]
    fn shared(&self) -> &'static SharedUartState {
        let idx = match self {
            UartID::U3 => 0,
        };
        &SHARED[idx]
    }

    #[inline]
    fn is_suspended(&self) -> bool {
        self.shared().suspended.load(Ordering::Acquire) == SuspendState::Suspended as u8
    }

    #[inline]
    fn enter_suspend(&self) -> bool {
        self.shared()
            .suspended
            .compare_exchange(
                SuspendState::Active as u8,
                SuspendState::Suspended as u8,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .is_ok()
    }

    #[inline]
    fn is_resuming(&self) -> bool {
        self.shared().suspended.load(Ordering::Acquire) == SuspendState::Resuming as u8
    }

    #[inline]
    fn force_suspended(&self) {
        self.shared()
            .suspended
            .store(SuspendState::Suspended as u8, Ordering::SeqCst);
    }

    #[inline]
    fn set_active(&self) {
        self.shared()
            .suspended
            .store(SuspendState::Active as u8, Ordering::SeqCst);
    }

    #[inline]
    fn set_resuming(&self) {
        self.shared()
            .suspended
            .store(SuspendState::Resuming as u8, Ordering::SeqCst);
    }
}

impl UART {
    /// Create a (suspended) UART instance.
    ///
    /// UART must be [resumed](method@Suspend::resume) before calling other methods.
    /// Suspend/resume power down the peripheral and IO pins.
    ///
    /// `rx_dma` and `tx_dma` are the DMA channels for RX (circular) and TX
    /// (one-shot). The channel numbers are picked at the call site via the
    /// const generics on [`DmaChannel`]; no two `UART` instances may share a
    /// channel.
    ///
    /// Note: The default format is 8N1. Other formats are not supported.
    pub fn new<const RX_N: u8, const TX_N: u8>(
        uart: usart::Instance,
        mut tx_pin: Pin,
        mut rx_pin: Pin,
        baudrate: u32,
        rx_dma: DmaChannel<RX_N>,
        tx_dma: DmaChannel<TX_N>,
    ) -> (Self, UartISR, UartRxDmaISR, UartTxDmaISR) {
        let id = UartID::from_instance(&uart);

        // Prevent current leakage out of pins
        tx_pin.set_output_type(crate::gpio::OutputType::OpenDrain);
        rx_pin.set_pullup(PullupConfig::Pulldown);

        // Deinit peripheral in case it was initialized and reset state to Suspended
        cortex_m::interrupt::free(|_| {
            unsafe {
                deinit(id);
            }
            cortex_m::peripheral::NVIC::unpend(id.as_interrupt());
            id.force_suspended();
        });

        // Configure DMA channels (left disabled). Routed via DMAMUX.
        let (rx_request, tx_request) = match id {
            UartID::U3 => (Request::USART3_RX, Request::USART3_TX),
        };
        let mut rx_dma = rx_dma.erase();
        let mut tx_dma = tx_dma.erase();
        rx_dma.configure(
            rx_request,
            Direction::PeripheralToMemory,
            Word::Byte,
            Priority::High,
        );
        tx_dma.configure(
            tx_request,
            Direction::MemoryToPeripheral,
            Word::Byte,
            Priority::High,
        );
        // Allocate the static RX ring + TX scratch.
        let shared = id.shared();
        let rx_ring = shared.rx_ring.init([0u8; RX_BUFFER_LEN]);
        let tx_scratch = shared.tx_scratch.init([0u8; TX_SCRATCH_LEN]);

        // SAFETY: `tdr_addr()` is the USART TDR data register and `tx_dma`
        // was just `configure`d M2P — the sole `OneShot` unsafe boundary.
        let (tx_oneshot, tx_abort, tx_irq) =
            unsafe { OneShot::new(tx_dma, id.tdr_addr(), tx_scratch) };

        // Split the RX role handles (consumes the owned RX handle). The
        // channel is *not* armed here: the UART starts suspended, so the
        // consumer arms it on its first read after `resume()` requests it.
        let (rx, rx_ctl, rx_dma_irq, rx_line_irq) =
            unsafe { CircularRx::new(rx_dma, id.rdr_addr(), rx_ring, &shared.circ_shared) };

        (
            UART {
                control: UartControl {
                    id,
                    tx: tx_pin,
                    _rx: rx_pin,
                    rx: rx_ctl,
                    tx_abort,
                    target_baudrate: baudrate,
                    actual_baudrate: 0, // zero as long as peripheral is not initialized yet
                    format: UartFormat::Bits8NoParity, // default
                },
                tx: Tx { id, tx: tx_oneshot },
                rx: Rx {
                    id,
                    rx, // the owned move-only consumer handle
                    error_occurred: false,
                },
            },
            UartISR {
                id,
                rx: rx_line_irq,
            },
            rx_dma_irq,
            tx_irq,
        )
    }
}

impl UartISR {
    /// USART line-event interrupt service routine.
    ///
    /// # Safety
    /// Must only be called from the USART vector for this instance.
    pub unsafe fn handle_interrupt(&mut self) {
        if self.id.is_suspended() {
            // Wake the reader so it can observe the suspended state.
            self.rx.wake();
            return;
        }
        let per = self.id.register_block();

        // Hardware overrun (with DMA active this should not happen — would
        // mean the DMA controller failed to drain RDR fast enough, e.g. bus
        // contention). Surface to the consumer so atat clears its ingress.
        if read_reg!(usart, per, ISR, ORE == 1) {
            log::error!("{:?} hardware overrun", self.id);
            write_reg!(usart, per, ICR, ORECF: 1);
            self.rx.signal_overrun();
        }

        if read_reg!(usart, per, ISR, NF == 1) {
            log::error!("{:?} hardware noise error", self.id);
            write_reg!(usart, per, ICR, NCF: 1);
        }

        if read_reg!(usart, per, ISR, FE == 1) {
            log::trace!("{:?} hardware framing error", self.id);
            write_reg!(usart, per, ICR, FECF: 1);
        }

        if read_reg!(usart, per, ISR, PE == 1) {
            log::error!("{:?} hardware parity error", self.id);
            write_reg!(usart, per, ICR, PECF: 1);
        }

        // RTOF / IDLE: line went idle, wake the reader so it picks up
        // anything DMA has dropped into the ring since the last poll.
        let rtof = read_reg!(usart, per, ISR, RTOF == 1);
        let idle = read_reg!(usart, per, ISR, IDLE == 1);
        if rtof {
            write_reg!(usart, per, ICR, RTOCF: 1);
        }
        if idle {
            write_reg!(usart, per, ICR, IDLECF: 1);
        }
        if rtof || idle {
            self.rx.wake();
        }

        // TC: line idle after a transmit. Used by `Tx::flush` (TCIE is set
        // by the flush future and cleared here so the IRQ doesn't re-fire).
        // TC itself stays asserted until cleared via ICR.TCCF; the future
        // re-reads ISR.TC after the wake to confirm completion.
        if read_reg!(usart, per, ISR, TC == 1) && read_reg!(usart, per, CR1, TCIE == Enabled) {
            modify_reg!(usart, per, CR1, TCIE: Disabled);
            self.id.shared().tx_waker.wake();
        }

        if self.id.is_resuming() {
            // Pending wakers can now make progress.
            self.id.shared().resume_waker.wake();
            self.rx.wake();
            self.id.set_active();
        }
    }
}

fn configure_baudrate(id: UartID, baudrate: u32) {
    // Configure baudrate
    // USART1 gets its clock PLK2 via the APB2_PRESCALER
    // USART2-5 gets their clock PLK1 via the APB1_PRESCALER
    // For oversampling 16 (default)
    // baud = usart_ker_clk / usart_div
    // BRR = usart_div
    let clocks = Clocks::from_global();

    let per = id.register_block();

    // For OVER8 true means oversampling by 8, false means oversampling by 16
    let over8 = unsafe { read_reg!(usart, per, CR1, OVER8 == 1) };

    let brr = calc_brr(clocks.pclk1().raw(), baudrate, over8);
    unsafe { modify_reg!(usart, per, BRR, BRR: brr) };
}

fn current_baudrate(id: UartID) -> u32 {
    let per = id.register_block();
    let over8 = unsafe { read_reg!(usart, per, CR1, OVER8 == 1) };
    let brr = unsafe { read_reg!(usart, per, BRR) };

    let div = if over8 {
        2 * (brr & 0xFFF0 | (brr & 0b111) << 1)
    } else {
        brr
    };

    if div == 0 {
        0
    } else {
        let clocks = Clocks::from_global();
        clocks.pclk1().raw() / div
    }
}

impl BaudRateControl for UartControl {
    fn set_baud_rate(&mut self, baudrate: u32) {
        if self.target_baudrate == baudrate {
            return;
        }
        self.target_baudrate = baudrate;

        if !self.id.is_suspended() {
            let per = self.id.register_block();

            // The BRR write disables the whole USART (UE=0), interrupting
            // both directions. RX is continuous → stop + re-arm clears the
            // receive buffer; tx is aborted.
            // Caller is responsible for not having any ongoing transfers:
            // - tx is truncated (not re-sent at the new baud, which would be worse)
            // - rx is interrupted, any data before baud change is lost
            interrupt::free(|_| {
                self.rx.stop();
                self.tx_abort.abort();
                unsafe {
                    modify_reg!(usart, per, CR1, UE: Disabled);
                };
                configure_baudrate(self.id, baudrate);
                self.actual_baudrate = current_baudrate(self.id);
                unsafe {
                    modify_reg!(usart, per, CR1, UE: Enabled);
                };
                self.rx.request_rearm();
            })
        }
    }

    fn get_baud_rate(&self) -> u32 {
        self.actual_baudrate
    }
}

impl BaudRateControl for UART {
    #[inline]
    fn set_baud_rate(&mut self, baudrate: u32) {
        self.control.set_baud_rate(baudrate)
    }

    #[inline]
    fn get_baud_rate(&self) -> u32 {
        self.control.get_baud_rate()
    }
}

impl Suspend for UartControl {
    fn suspend(&mut self) {
        if self.id.enter_suspend() {
            log::debug!("[uart] Suspending");

            // Pend interrupt so that pending futures can be woken up.
            cortex_m::peripheral::NVIC::pend(self.id.as_interrupt());
            // Abort the in-flight TX transfer (halts EN now; its clock vote
            // is dropped when the woken TX future polls/drops) and stop the
            // RX channel. Independent, so order is immaterial.
            self.tx_abort.abort();
            self.rx.stop();
            unsafe {
                deinit(self.id);
            }
            // Unstick a pending `Tx::write` (its poll-fn registers
            // `tx_waker` too): it will observe the suspended state and
            // resolve `Err(Suspended)`, dropping its `Transfer` (which
            // cancels the channel). Preserves the pre-rewire behaviour
            // where control and tx may run on independent tasks.
            self.id.shared().tx_waker.wake();

            // Prevent current leakage out of tx pin
            // Note: rx pin config is already safe (input-pulldown)
            self.tx.set_output_type(crate::gpio::OutputType::OpenDrain);
        }
    }

    fn resume(&mut self) {
        // Note that this is not strictly atomic.
        // This is only safe under the following assumptions:
        // 1. There is only one UART instance per ID
        // 2. Because this function and suspend require `&mut self` we guarantee exclusive access
        // 3. The critical section prevents the IRQ from accessing the peripheral while initializing
        if self.id.is_suspended() {
            log::debug!(target: "UART", "Resuming");

            // Restore pin config
            self.tx.set_output_type(crate::gpio::OutputType::PushPull);

            cortex_m::interrupt::free(|_| {
                let uart = match self.id {
                    UartID::U3 => unsafe { stm32ral::usart::USART3::steal() },
                };
                init(uart, self.target_baudrate, self.format, self.id);
                self.actual_baudrate = current_baudrate(self.id);

                // Request circular-RX re-arm; the consumer arms + resets
                // accounting on its next read.
                self.rx.request_rearm();

                self.id.set_resuming();
                // State will be set to active in the interrupt
                cortex_m::peripheral::NVIC::pend(self.id.as_interrupt());
            });
        }
    }

    #[inline]
    fn is_suspended(&self) -> bool {
        self.id.is_suspended()
    }
}

impl Suspend for UART {
    #[inline]
    fn suspend(&mut self) {
        self.control.suspend()
    }
    #[inline]
    fn resume(&mut self) {
        self.control.resume()
    }
    #[inline]
    fn is_suspended(&self) -> bool {
        self.control.is_suspended()
    }
}

impl Write for UART {
    #[inline]
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.tx.write(buf).await
    }

    #[inline]
    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.tx.flush().await
    }
}

impl Read for UART {
    #[inline]
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.rx.read(buf).await
    }
}

impl Tx {
    fn fifo_is_empty(&self) -> bool {
        let per = self.id.register_block();
        unsafe { read_reg!(usart, per, ISR, TXFE == 1) }
    }

    /// Wait for the line to go idle (TC=1).
    async fn wait_for_tc(&mut self) -> Result<(), Error> {
        let per = self.id.register_block();
        // Enable TC interrupt; the USART ISR will clear TCIE and wake.
        unsafe { modify_reg!(usart, per, CR1, TCIE: Enabled) };
        poll_fn(|cx| {
            if self.id.is_suspended() {
                return Poll::Ready(Err(Error::Suspended));
            }
            self.id.shared().tx_waker.register(cx.waker());
            let per = self.id.register_block();
            if unsafe { read_reg!(usart, per, ISR, TC == 1) } {
                unsafe { modify_reg!(usart, per, CR1, TCIE: Disabled) };
                Poll::Ready(Ok(()))
            } else {
                Poll::Pending
            }
        })
        .await
    }

    async fn write(&mut self, words: &[u8]) -> Result<usize, Error> {
        if words.is_empty() {
            return Ok(0);
        }
        if self.id.is_suspended() {
            return Err(Error::Suspended);
        }
        let id = self.id;

        let mut total = 0usize;
        for chunk in words.chunks(self.tx.scratch_len()) {
            if id.is_suspended() {
                return Err(Error::Suspended);
            }
            // `OneShot::write` stages the chunk into its owned 'static
            // scratch and DMAs it via `Transfer`. Driven inside a
            // poll-fn that also registers `tx_waker`, so `suspend()` (which
            // wakes it) unsticks a pending write even when control and tx
            // run on independent tasks.
            // `chunk.len()` ≤ `scratch_len()` by the chunking, so `write` won't panic.
            let mut xfer = pin!(self.tx.write(chunk));
            poll_fn(|cx| {
                if id.is_suspended() {
                    return Poll::Ready(Err(Error::Suspended));
                }
                id.shared().tx_waker.register(cx.waker());
                match xfer.as_mut().poll(cx) {
                    Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
                    Poll::Ready(Err(_)) => Poll::Ready(Err(Error::Overrun)),
                    Poll::Pending => Poll::Pending,
                }
            })
            .await?;
            total += chunk.len();
        }
        Ok(total)
    }

    async fn flush(&mut self) -> Result<(), Error> {
        if self.fifo_is_empty() {
            return Ok(());
        }
        self.wait_for_tc().await
    }
}

impl Rx {
    /// Read at least 1 and maximum `buf.len()` bytes.
    ///
    /// Correctness guarantee (from [`CircularRx`]): a returned `Ok(n)` was
    /// never overwritten/torn by the DMA during the read. Any ring lap, DMA
    /// bus error (`TEIF`), or hardware overrun (`ORE`) is reported as
    /// `Err(Overrun)` after an internal resync — corrupted bytes are never
    /// returned. Visibility does not depend on HT/TC firing, and overrun
    /// detection does not depend on the DMA IRQ (the consumer observes
    /// wraps itself; see `CircularRx`).
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        poll_fn(|cx| {
            if self.id.is_suspended() {
                return Poll::Ready(Err(Error::Suspended));
            }
            self.rx.register_waker(cx.waker());
            match self.rx.try_read(buf) {
                TryRead::Bytes(n) => Poll::Ready(Ok(n)),
                TryRead::Empty => Poll::Pending,
                TryRead::Overrun => {
                    log::error!("UART {:?} RX overrun", self.id);
                    self.error_occurred = true;
                    Poll::Ready(Err(Error::Overrun))
                }
            }
        })
        .await
    }

    fn read_ready_inner(&mut self) -> bool {
        self.rx.data_ready()
    }
}

impl ErrorType for Tx {
    type Error = Error;
}

impl ErrorType for Rx {
    type Error = Error;
}

impl ErrorType for UART {
    type Error = Error;
}

impl ReadReady for Rx {
    fn read_ready(&mut self) -> Result<bool, Self::Error> {
        Ok(self.read_ready_inner())
    }
}

#[derive(Debug)]
pub enum Error {
    Overrun,
    Suspended,
}

impl embedded_io_async::Error for Error {
    fn kind(&self) -> embedded_io_async::ErrorKind {
        embedded_io_async::ErrorKind::Other
    }
}

fn calc_brr(usart_ker_ckpres: u32, baudrate: u32, over8: bool) -> u32 {
    if over8 {
        let usart_div = ((usart_ker_ckpres * 2) + (baudrate / 2)) / baudrate;
        let brr = usart_div & 0xFFF0;
        brr | ((usart_div & 0xF) >> 1)
    } else {
        ((usart_ker_ckpres) + (baudrate / 2)) / baudrate
    }
}

fn init(uart: usart::Instance, baudrate: u32, mode: UartFormat, id: UartID) {
    // Step 1: enable and reset peripheral in RCC
    match id {
        UartID::U3 => rcc::enable_rst_usart3(),
    }

    // Reset control registers
    match id {
        UartID::U3 => {
            reset_reg!(usart, uart, USART3, CR1);
            reset_reg!(usart, uart, USART3, CR2);
            reset_reg!(usart, uart, USART3, CR3);
        }
    }

    let (m0, pce, ps) = match mode {
        UartFormat::Bits8NoParity => (0, 0, 0),
        // Note: peripheral counts parity as part of word length,
        // so M0=1 selects 9-bit mode for 8 data bits + 1 parity bit
        UartFormat::Bits8EvenParity => (1, 1, 0),
        UartFormat::Bits8OddParity => (1, 1, 1),
    };

    // Configure CR1
    modify_reg!(usart, uart, CR1,
        FIFOEN: 1,
        TE: Enabled,
        RE: Enabled,
        RTOIE: 1,
        OVER8: 0,
        M1: 0,    // 8 or 9-bit mode (1 would select 7-bit)
        M0: m0,   // (0 = 8-bit, 1 = 9-bit, parity counts as data bit)
        PCE: pce, // Parity enable
        PS: ps    // Even/odd parity (0 = even, 1 = odd)
    );

    // Should be called after configuring OVER8
    configure_baudrate(id, baudrate);

    // The modem's RX/TX are crossed relative to USART3's default pins
    modify_reg!(usart, uart, CR2,
        RTOEN: Enabled,
        SWAP: 1 // 1: swap rx/tx
    );

    // CR3: enable DMA on both directions. RXFTIE/RXFTCFG are intentionally
    // not set — with DMA RX in circular mode, the DMA HT/TC events plus the
    // USART RTOF/IDLE handle wakeups; FIFO threshold IRQs would just add
    // noise.
    modify_reg!(usart, uart, CR3,
        DMAR: Enabled,
        DMAT: Enabled
    );

    // Set receiver timeout to 10 bits
    modify_reg!(usart, uart, RTOR, RTO: 10);

    // Clear all interrupts
    write_reg!(usart, uart, ICR, 0x00123BFF);

    // Enable peripheral
    modify_reg!(usart, uart, CR1, UE: Enabled);

    // (Unmasking the USART vector in NVIC and mapping it to Uart::isr() is done by application)
}

/// Deinitialize the UART peripheral (USART `UE=0` + gate the USART clock).
///
/// Disables the USART from the control context while `Rx`/`Tx` also touch
/// USART registers; safe only because control never preempts the reader or
/// writer (do not relocate USART control to a context that can preempt the
/// reader/writer without serializing it).
///
/// # Safety
///
/// This is unsafe because it _must_ only be called when the
/// suspend state is managed correctly by the caller
unsafe fn deinit(id: UartID) {
    let per = match id {
        UartID::U3 => unsafe { stm32ral::usart::USART3::steal() },
    };

    // Note: we don't disable interrupts here because we
    // need them for suspend/resume signaling.

    // Disable Peripheral
    modify_reg!(usart, per, CR1, UE: Disabled);

    // Disable peripheral clk
    match id {
        UartID::U3 => rcc::disable_rst_usart3(),
    }
}

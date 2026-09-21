//! I2C driver implementing embedded_hal_async traits.

use core::{future::poll_fn, ops::Deref, task::Poll};

use cortex_m::interrupt;
use embassy_sync::waitqueue::AtomicWaker;
use stm32ral::{modify_reg, read_reg, write_reg};

use embedded_hal_async::i2c::SevenBitAddress;
use sensor_link_firmware::{traits::Suspend, utils::num::div_ceil};

use crate::{
    gpio,
    rcc::{self, Clocks},
    rtic_device::i2c,
};

static WAKER: AtomicWaker = AtomicWaker::new();

pub struct I2C {
    _sda: gpio::Pin,
    _scl: gpio::Pin,

    peripheral: i2c::Instance,
    periph_clock_hz: u32,
    enabled: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum Error {
    /// Previous transaction still busy (can this be triggered by bus stuck low?)
    Busy,

    /// Timeout error (SCL stuck low)
    Timeout,

    /// Bus error
    Bus,

    /// Arbitration loss (should only happen with multiple masters)
    Arbitration,

    /// No answer (technically not an error)
    Nack,

    /// Maximum transfer length of 255 bytes exceeded (longer transfers not implemented due to errata 2.19.5)
    MaxLength,

    /// I2C address not a valid 7bit address
    InvalidAddress,
}

#[derive(Debug, PartialEq)]
enum Event {
    TXDREmpty,
    Stop,
    TransferComplete,
    RXNotEmpty,
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum End {
    /// Send a stop bit when transfer completes (next transfer will begin with START)
    Stop,

    /// Don't send STOP bit after transfer completes (next transfer will begin with RESTART)
    None,
}

/// I2C driver implementing `Suspend` and `embedded_hal_async::i2c::I2C`
///
/// Note that this driver assumes a fixed baudrate and requires peripheral `I2C3`.
/// This driver is intended to use through two traits:
/// - `Suspend` for enabling/disabling the I2C peripheral
/// - `embedded_hal_async::i2c::I2C` for the I2C data transactions
impl I2C {
    /// Create a new I2C instance
    ///
    /// Note: expects I2C3 (panics if called with the wrong instance)
    ///
    /// ## Safety
    ///
    /// No more than one instance of this driver should be created. Which should not
    /// be possible if the assumption holds that only one I2C3 peripheral instance can exist
    /// (since we take ownership of it)
    pub fn new(peripheral: i2c::Instance, sda: gpio::Pin, scl: gpio::Pin, clocks: &Clocks) -> Self {
        // Note: if more peripherals are ever supported, make sure each has a dedicated waker!
        if peripheral.deref() as *const _ != i2c::I2C3 {
            panic!("I2C must be I2C3");
        }

        Self {
            _sda: sda,
            _scl: scl,
            peripheral,
            periph_clock_hz: clocks.pclk1().raw(),
            enabled: false,
        }
    }

    fn configure_timing_400khz(&mut self) {
        // RM0432 Rev9, table 343: prescale down to ca 8MHz (125ns ticks)
        let prescaler = (self.periph_clock_hz / 8_000_000).min(16).max(1);
        let ns_per_tick = 1_000_000_000 / (self.periph_clock_hz / prescaler);
        let ns_per_periph_tick = ns_per_tick / prescaler;

        // I2C timings (I2C fastmode spec)
        const I2C_LOW_TIME_MIN_NS: u32 = 1300;
        const I2C_HIGH_TIME_MIN_NS: u32 = 600;
        const I2C_FALLTIME_MAX_NS: u32 = 300; // Measured on prototype: < 20ns
        const I2C_RISETIME_MAX_NS: u32 = 300; // Measured on prototype: requires <= 4.12K pullups
        const I2C_SETUP_MIN_NS: u32 = 100;
        const I2C_TOTAL_INTERVAL_MIN_NS: u32 = 2500; // 1/2500ns = 400kHz

        // Fall time: RM0432 Rev9, page 1651
        //
        // SDADEL time >= fall_max - hold_min - filter_min - 2 cycles.
        // (hold_min=0 for I2C)
        const ANALOG_FILTER_MIN_NS: u32 = 50; // Datasheet 6.3.30
        let sdadel_min_ns = I2C_FALLTIME_MAX_NS
            .saturating_sub(ANALOG_FILTER_MIN_NS)
            .saturating_sub(2 * ns_per_periph_tick);

        // Rise time: RM0432 Rev9, page 1651
        // SCLDEL time >= rise_max + setup_min
        const SCLDEL_MIN_NS: u32 = I2C_RISETIME_MAX_NS + I2C_SETUP_MIN_NS;

        // Frequency limit to 400 kHz: with fast rise/fall times, the effective bitrate could exceed the maximum (e.g. > 400kHz for fast mode)
        // This guarantees that the interval will be > I2C_TOTAL_INTERVAL_MS as the rise/fall time will be faster than zero.
        let t_overhead_min = 4 * ns_per_periph_tick + 2 * ANALOG_FILTER_MIN_NS;
        let t_total_min = I2C_LOW_TIME_MIN_NS + I2C_HIGH_TIME_MIN_NS + t_overhead_min;
        let t_extra = I2C_TOTAL_INTERVAL_MIN_NS.saturating_sub(t_total_min);
        let high_time_ns = I2C_HIGH_TIME_MIN_NS + t_extra;

        // Calculate timings from nanoseconds to I2C clock ticks (after prescaler)
        let scll_ticks = div_ceil(I2C_LOW_TIME_MIN_NS, ns_per_tick);
        let sclh_ticks = div_ceil(high_time_ns, ns_per_tick);
        let sdadel_ticks = div_ceil(sdadel_min_ns, ns_per_tick);
        let scldel_ticks = div_ceil(SCLDEL_MIN_NS, ns_per_tick);

        // Note: total frequence will be scll + sclh + 4*ns_per_periph_tick + rise/fall times.
        // SCLL will effectively never be smaller than (SDADEL + SCLDEL + 1) due to clock stretching

        // Registers are all 'off-by one'. values are clamped within their
        // min/max range (4bit or 8bit fields) and compensated for this offset
        modify_reg!(i2c, self.peripheral, TIMINGR,
            PRESC: prescaler - 1,
            SCLDEL: scldel_ticks.min(16).max(1)- 1,
            SDADEL: sdadel_ticks.min(16).max(1) - 1,
            SCLH: sclh_ticks.min(256).max(1) - 1,
            SCLL: scll_ticks.min(256).max(1) - 1
        );

        // Enable bus timeout: if clock is held low longer than this timeout, the peripheral will
        // trigger a timeout error (this is not in the I2C spec but helps make it more robust)
        const CLOCK_TIMEOUT_MS: u32 = 10;
        let timeout_resolution_ns = 2048 * ns_per_periph_tick;
        let timeout_value = (CLOCK_TIMEOUT_MS * 1_000_000) / timeout_resolution_ns;
        modify_reg!(i2c, self.peripheral, TIMEOUTR,
            TIMOUTEN: 1,
            TIDLE: 0,
            TIMEOUTA: timeout_value.min(4096).max(1) - 1);
    }

    fn enable(&mut self) {
        // Enable peripheral clock
        rcc::enable_rst_i2c3();

        // RM0432 Rev9, Fig 462:
        // 1. clear PE bit
        modify_reg!(i2c, self.peripheral, CR1, PE: 0);
        while read_reg!(i2c, self.peripheral, CR1, PE != 0) {}

        // 2. Configure ANOFF/DNF
        modify_reg!(i2c, self.peripheral, CR1,
            ANFOFF: 0,  // Enable analog noise filter
            DNF: 0      // No extra digital filtering
        );

        // 3. configure PRESC, SDADEL, SCLDEL, SCLH, SCLL
        self.configure_timing_400khz();

        // 4. Configure NOSTRETCH
        modify_reg!(i2c, self.peripheral, CR1,
            NOSTRETCH: 0 // Must be zero in master mode
        );

        // 5. Set PE bit
        modify_reg!(i2c, self.peripheral, CR1, PE: 1);
        while read_reg!(i2c, self.peripheral, CR1, PE != 1) {}

        // (Unmasking the I2C3_ER+I2C3_EV vectors in NVIC and mapping them to Self::isr() is done by application)
        self.enabled = true;
    }

    fn disable(&mut self) {
        // Debug: check if any transfers are still busy (most likely STOP was set but not on the bus yet?)
        if read_reg!(i2c, self.peripheral, ISR, BUSY != 0)
            || read_reg!(i2c, self.peripheral, CR2, START != 0)
        {
            log::warn!("I2C still busy!");
        }

        // Disable peripheral (I2C pins idle)
        modify_reg!(i2c, self.peripheral, CR1, PE: 0);
        while read_reg!(i2c, self.peripheral, CR1, PE != 0) {}

        // Stop peripheral clock
        rcc::disable_rst_i2c3();

        self.enabled = false;
    }

    /// Prepare writing up to 255 bytes to the I2C bus
    ///
    /// Note: the transaction is finished when:
    /// - with `End::Stop`: wait for `Event::Stop`
    /// - with `End::None`: wait for `Event::TransferComplete`
    fn prepare_write(
        &mut self,
        address: SevenBitAddress,
        len: usize,
        end_condition: End,
    ) -> Result<(), Error> {
        if address >= 0x80 {
            return Err(Error::InvalidAddress);
        }
        // start bit already set: should not be possible?
        if read_reg!(i2c, self.peripheral, CR2, START != 0) {
            return Err(Error::Busy);
        }

        if len > 255 {
            return Err(Error::MaxLength);
        }

        write_reg!(i2c, self.peripheral, ICR, STOPCF: 1);
        modify_reg!(i2c, self.peripheral, CR2,
            ADD10: 0,
            SADD: u32::from(address) << 1,
            RD_WRN: 0,
            NBYTES: len as u32,
            RELOAD: 0,
            AUTOEND: u32::from(end_condition == End::Stop),
            START: 1
        );
        Ok(())
    }

    /// Prepare reading up to 255 bytes from the I2C bus
    fn prepare_read(
        &mut self,
        address: SevenBitAddress,
        len: usize,
        end_condition: End,
    ) -> Result<(), Error> {
        if address >= 0x80 {
            return Err(Error::InvalidAddress);
        }
        // start bit already set: should not be possible?
        if read_reg!(i2c, self.peripheral, CR2, START != 0) {
            return Err(Error::Busy);
        }

        if len > 255 {
            return Err(Error::MaxLength);
        }

        write_reg!(i2c, self.peripheral, ICR, STOPCF: 1);
        modify_reg!(i2c, self.peripheral, CR2,
            ADD10: 0,
            SADD: u32::from(address) << 1,
            RD_WRN: 1,
            NBYTES: len as u32,
            RELOAD: 0,
            AUTOEND: u32::from(end_condition == End::Stop),
            START: 1
        );
        Ok(())
    }

    fn clear_error_and_stop(&mut self) {
        write_reg!(i2c, self.peripheral, ICR, TIMOUTCF: 1, ARLOCF: 1, BERRCF: 1, NACKCF: 1);

        // PE soft reset: releases SCL+SDA, clears CR2 (including any
        // hardware-set STOP from TIMEOUTA), resets state machine.
        // TIMINGR, TIMEOUTR, CR1 filter config survive PE toggle.
        modify_reg!(i2c, self.peripheral, CR1, PE: 0);
        while read_reg!(i2c, self.peripheral, CR1, PE != 0) {}
        modify_reg!(i2c, self.peripheral, CR1, PE: 1);
        while read_reg!(i2c, self.peripheral, CR1, PE != 1) {}
    }

    async fn wait_untill(&mut self, event: Event) -> Result<(), Error> {
        poll_fn(|cx| {
            WAKER.register(cx.waker());

            // 1. Return any unhandled error that has already occurred
            if read_reg!(i2c, self.peripheral, ISR, TIMEOUT != 0) {
                self.clear_error_and_stop();
                return Poll::Ready(Err(Error::Timeout));
            }
            if read_reg!(i2c, self.peripheral, ISR, ARLO != 0) {
                self.clear_error_and_stop();
                return Poll::Ready(Err(Error::Arbitration));
            }
            if read_reg!(i2c, self.peripheral, ISR, BERR != 0) {
                self.clear_error_and_stop();
                return Poll::Ready(Err(Error::Bus));
            }
            if read_reg!(i2c, self.peripheral, ISR, NACKF != 0) {
                self.clear_error_and_stop();
                return Poll::Ready(Err(Error::Nack));
            }

            // 2. Return if the even has already occurred
            match event {
                Event::TXDREmpty => {
                    if read_reg!(i2c, self.peripheral, ISR, TXIS != 0) {
                        return Poll::Ready(Ok(()));
                    }
                }
                Event::TransferComplete => {
                    if read_reg!(i2c, self.peripheral, ISR, TC != 0) {
                        return Poll::Ready(Ok(()));
                    }
                }
                Event::Stop => {
                    if read_reg!(i2c, self.peripheral, ISR, STOPF != 0) {
                        return Poll::Ready(Ok(()));
                    }
                }
                Event::RXNotEmpty => {
                    if read_reg!(i2c, self.peripheral, ISR, RXNE != 0) {
                        return Poll::Ready(Ok(()));
                    }
                }
            }

            // 3. Setup IRQ to wake us when a relevant event hapens
            interrupt::free(|_| unsafe {
                modify_reg!(i2c, I2C3, CR1,
                    ERRIE: 1,
                    NACKIE: 1,
                    TXIE: u32::from(event == Event::TXDREmpty),
                    RXIE: u32::from(event == Event::RXNotEmpty),
                    TCIE: u32::from(event == Event::TransferComplete),
                    STOPIE: u32::from(event == Event::Stop)
                )
            });
            Poll::Pending
        })
        .await
    }

    /// Perform an I2C read, assuming the transaction is started by prepare_read()
    ///
    /// `End::Stop`: the I2C transaction is guaranteed to finish (bus idle) before
    /// returning.
    ///
    /// `End::None`: the I2C transaction may still be ongoing but the peripheral is
    /// guaranteed to be ready for a next transaction (e.g. `prepare_read()` may be called immediately)
    async fn inner_read(&mut self, data: &mut [u8], end_condition: End) -> Result<(), Error> {
        for byte in data {
            self.wait_untill(Event::RXNotEmpty).await?;
            *byte = (read_reg!(i2c, self.peripheral, RXDR, RXDATA) & 0xFF) as u8;
        }

        // Stop: wait untill stop is sent
        // The peripheral in AUTOEND mode should send it automatically, but it
        // won't do it if a next transfer is already scheduled: this would cause a repeated-start which is not what we want.
        if end_condition == End::Stop {
            self.wait_untill(Event::Stop).await?;

        // None: as soon as the start bit is clear, we can already return (peripheral still reading, but ready for next transfer)
        // If bit is still set, await the whole transfer (as we can't await the start bit)
        } else if read_reg!(i2c, self.peripheral, CR2, START != 0) {
            self.wait_untill(Event::TransferComplete).await?;
        }

        Ok(())
    }

    /// Perform an I2C write, assuming the transaction is started by prepare_write()
    ///
    /// `End::Stop`: the I2C transaction is guaranteed to finish (bus idle) before
    /// returning.
    ///
    /// `End::None`: the I2C transaction may still be ongoing but the peripheral is
    /// guaranteed to be ready for a next transaction (e.g. `prepare_read()` may be called immediately)
    async fn inner_write(&mut self, data: &[u8], end_condition: End) -> Result<(), Error> {
        let n_bytes = data.len();

        for (i, byte) in data.iter().enumerate() {
            // First byte can be enqueued immediately (before first TXDREmpty)
            write_reg!(i2c, self.peripheral, TXDR, TXDATA: *byte as u32);

            // TXDREmpty does not occur after the last byte as the peripheral knowns
            // it has all n_bytes to complete the transaction
            if (i + 1) < n_bytes {
                self.wait_untill(Event::TXDREmpty).await?;
            }
        }

        // Stop: wait untill stop is sent
        // The peripheral in AUTOEND mode should send it automatically, but it
        // won't do it if a next transfer is already scheduled: this would cause a repeated-start which is not what we want.
        if end_condition == End::Stop {
            self.wait_untill(Event::Stop).await?;

        // None: as soon as the start bit is clear, we can already return (peripheral still transmitting, but ready for next transfer)
        // If bit is still set, await the whole transfer (as we can't await the start bit)
        } else if read_reg!(i2c, self.peripheral, CR2, START != 0) {
            self.wait_untill(Event::TransferComplete).await?;
        }

        Ok(())
    }

    /// ISR function: must be called from the I2C3 Interrupt handler
    ///
    /// ## Safety
    ///
    /// This function is marked unsafe because it must be called from the correct interrupt handler only.
    pub unsafe fn isr() {
        // Disable interrupts and wake the waker.
        // The waker will re-enable interrupts if it wants to await them
        interrupt::free(|_| unsafe {
            modify_reg!(i2c, I2C3, CR1,
                ERRIE: 0,
                TCIE: 0,
                STOPIE: 0,
                NACKIE: 0,
                ADDRIE: 0,
                RXIE: 0,
                TXIE: 0
            )
        });
        WAKER.wake();
    }
}

impl Suspend for I2C {
    fn suspend(&mut self) {
        self.disable()
    }

    fn resume(&mut self) {
        self.enable()
    }

    fn is_suspended(&self) -> bool {
        !self.enabled
    }
}

type ErrorKind = embedded_hal_async::i2c::ErrorKind;
impl embedded_hal_async::i2c::Error for Error {
    fn kind(&self) -> ErrorKind {
        match self {
            Error::Busy => ErrorKind::Other,
            Error::Timeout => ErrorKind::Other,
            Error::Bus => ErrorKind::Bus,
            Error::Arbitration => ErrorKind::ArbitrationLoss,
            Error::Nack => {
                ErrorKind::NoAcknowledge(embedded_hal_async::i2c::NoAcknowledgeSource::Unknown)
            }
            Error::MaxLength => ErrorKind::Other,
            Error::InvalidAddress => ErrorKind::Other,
        }
    }
}

impl embedded_hal_async::i2c::ErrorType for I2C {
    type Error = Error;
}

impl embedded_hal_async::i2c::I2c for I2C {
    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [embedded_hal_async::i2c::Operation<'_>],
    ) -> Result<(), Self::Error> {
        let mut first = true;
        let mut was_reading = false;

        let mut ops = operations
            .iter_mut()
            // filter out no-ops: empty transactions would confuse the START/STOP logic
            .filter(|op| match op {
                embedded_hal_async::i2c::Operation::Read(bytes) => bytes.len() > 0,
                embedded_hal_async::i2c::Operation::Write(bytes) => bytes.len() > 0,
            })
            .peekable();
        while let Some(op) = ops.next() {
            // Stop bit only if this is the last op
            let end_condition = if ops.peek().is_none() {
                End::Stop
            } else {
                End::None
            };

            match op {
                embedded_hal_async::i2c::Operation::Read(data) => {
                    // First op or change from write to read: STart bit + addr
                    if first || !was_reading {
                        self.prepare_read(address, data.len(), end_condition)?;
                    }
                    was_reading = true;
                    self.inner_read(data, end_condition).await?;
                }
                embedded_hal_async::i2c::Operation::Write(data) => {
                    // First op or change from read to write: STart bit + addr
                    if first || was_reading {
                        self.prepare_write(address, data.len(), end_condition)?;
                    }
                    self.inner_write(data, end_condition).await?;
                    was_reading = false;
                }
            }
            first = false;
        }
        Ok(())
    }
}

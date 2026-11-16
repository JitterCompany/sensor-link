use core::{future::poll_fn, ops::Deref, ptr, task::Poll};

use cortex_m::interrupt;
pub use embedded_hal_async::spi::{Mode, Phase, Polarity};
use sensor_link_firmware::{
    monotonic_time::delay_us,
    traits::{self, Suspend as _},
    utils::num::div_ceil,
};
use stm32ral::{modify_reg, read_reg, spi};

use crate::{
    gpio,
    rcc::{self, Clocks},
};

mod waker {
    use super::BusID;
    pub use embassy_sync::waitqueue::AtomicWaker;

    static WAKERS: [AtomicWaker; 1] = [const { AtomicWaker::new() }; 1];

    #[inline]
    pub(super) fn for_bus(id: BusID) -> &'static AtomicWaker {
        &WAKERS[id as usize]
    }
}

/// SPI bus
///
/// Represents an SPI bus which can be used by one or more SPI devices.
pub struct Bus {
    peripheral: spi::Instance,
    enabled: bool,
    clock: gpio::Pin,
    mosi: gpio::Pin,
    _miso: gpio::Pin,
    af: u8,
    /// CPOL of the most recent `enable()` — used by `disable()` to park CLK
    /// at the peripheral's idle level, so the next `enable()` can promote
    /// CLK to `Alt(af)` without producing an SCK edge. `None` before the
    /// first `enable()`.
    last_cpol: Option<Polarity>,
}

/// SPI config - specific to the device on the SPI bus
pub struct Config {
    pub lsb_first: bool,
    pub baudrate_hz: u32,
    pub mode: Mode,
}

/// The three shared-bus pins plus the alternate-function number they're routed
/// through. Bundled so the Bus can switch them between `Alt(af)` (peripheral
/// driving) and `Output(Low)` (safely parked) without losing track of which AF
/// to restore.
pub struct SpiPins {
    pub clock: gpio::Pin,
    pub mosi: gpio::Pin,
    pub miso: gpio::Pin,
    pub af: u8,
}

/// What the caller wants the bus pins to do when the peripheral is disabled.
///
/// Only MOSI's idle level is caller-selectable; CLK is always parked at the
/// peripheral's CPOL-defined idle level (remembered from the last `enable()`)
/// so the next `enable()` can promote CLK to `Alt(af)` without producing
/// an SCK edge.
#[derive(Debug, Clone, Copy)]
pub enum IdleMode {
    /// MOSI driven high: matches a device with weakly pulled-up inputs,
    /// so there is no DC drive fight, and no
    /// transition glitch when the peripheral re-takes the pin (STM32 SPI
    /// peripheral idle is also high).
    MosiHigh,
    /// MOSI driven low: for devices that prefer or require a low idle
    /// (no pull-up, or pulled-down inputs).
    MosiLow,
    /// Device rail is going off: MOSI *and* CLK both switched to
    /// `Output(Low)` so the MCU can't back-feed through the device's input
    /// protection diodes into the unpowered supply rail.
    Off,
}

pub type SPIOperation<'a> = embedded_hal_async::spi::Operation<'a, u8>;

/// SCK level when the peripheral is idle, derived from SPI CPOL.
fn cpol_idle_state(polarity: Polarity) -> gpio::PinState {
    match polarity {
        Polarity::IdleHigh => gpio::PinState::High,
        Polarity::IdleLow => gpio::PinState::Low,
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(usize)]
pub enum BusID {
    /// Internal SPI bus to the onboard SPI flash
    B1,
}

impl BusID {
    #[inline]
    pub fn waker(self) -> &'static waker::AtomicWaker {
        waker::for_bus(self)
    }
}

impl Bus {
    pub fn new(peripheral: spi::Instance, pins: SpiPins) -> Self {
        let SpiPins {
            mut clock,
            mut mosi,
            miso,
            af,
        } = pins;

        // Park CLK + MOSI as GPIO-output-low until the first transaction.
        // MISO stays in the Alt mode the caller configured (it is driven by
        // the remote device, never by us).
        clock.mode(gpio::PinMode::Output(gpio::PinState::Low));
        mosi.mode(gpio::PinMode::Output(gpio::PinState::Low));

        let bus = Self {
            peripheral,
            enabled: false,
            clock,
            mosi,
            _miso: miso,
            af,
            last_cpol: None,
        };

        // Assert the bus id is valid
        let _id: BusID = bus.id();

        bus
    }

    /// ID of the SPI bus. Uniqely identifies one bus that may
    /// connect to multiple [Device]s.
    pub fn id(&self) -> BusID {
        match self.peripheral.deref() as *const _ {
            spi::SPI1 => BusID::B1,
            _ => panic!("SPI: bus must be 1"),
        }
    }

    pub fn enable(&mut self, config: &Config, crc_poly: Option<u8>) -> Result<(), Error> {
        if self.enabled {
            return Err(Error::Busy);
        }

        match self.id() {
            BusID::B1 => rcc::enable_rst_spi1(),
        }

        let pclk = Clocks::from_global().pclk2();

        // find the minimum prescaler, accounting for int rounting
        let prescaler = div_ceil(pclk.to_Hz(), config.baudrate_hz);

        // SPI clock divider options(field 'br')
        let br: u32 = match prescaler {
            0..=2 => 0b000,
            3..=4 => 0b001,
            5..=8 => 0b010,
            9..=16 => 0b011,
            17..=32 => 0b100,
            33..=64 => 0b101,
            65..=128 => 0b110,
            _ => 0b111,
        };

        modify_reg!(spi, self.peripheral, CR1,
            SSM: 1, // chip select under software control
            SSI: 1, // force internal cs high (single-master SPI bus)
            LSBFIRST: u32::from(config.lsb_first),
            BR: br,
            MSTR: 1, // master mode
            CPOL: u32::from(config.mode.polarity == Polarity::IdleHigh),
            CPHA: u32::from(config.mode.phase == Phase::CaptureOnSecondTransition)
        );

        modify_reg!(spi, self.peripheral, CR2,
            FRXTH: 1, // 1=RXNE after 8 bits
            DS: 0b0111 // 8-bit mode
        );

        if let Some(crc_poly) = crc_poly {
            modify_reg!(spi, self.peripheral, CRCPR, CRCPOLY: crc_poly as u32);
            modify_reg!(spi, self.peripheral, CR1, CRCEN: 1);
        }

        modify_reg!(spi, self.peripheral, CR1, SPE: 1);

        // Hand CLK to the peripheral now, while CS is still deasserted.
        // Any level change (from a boot Output(Low) default, or from a
        // previous transaction that used a different CPOL) happens with
        // CS high, so the target device ignores the edge per SPI spec.
        // `disable()` parks CLK at this config's CPOL-idle level, so the
        // common steady-state case of back-to-back transactions with the
        // same CPOL produces no edge at all.
        self.clock.mode(gpio::PinMode::Alt(self.af));
        self.last_cpol = Some(config.mode.polarity);

        // MOSI stays in `Output(...)` until the caller invokes
        // `engage_mosi()` — that way the peripheral-driven MOSI edge, which
        // *can't* be made glitch-free (MOSI idle level is governed by TX
        // shift register contents, not CPOL), lands after CS has asserted.
        // With the target device actively driving MISO at that point, the
        // MOSI→MISO coupling is overpowered and no MISO undershoot occurs.
        self.enabled = true;
        Ok(())
    }

    /// Promote MOSI to `Alt(af)` so the peripheral drives it.
    ///
    /// Must be called *after* CS is asserted on the target device — see
    /// `enable` for the rationale.
    pub fn engage_mosi(&mut self) {
        self.mosi.mode(gpio::PinMode::Alt(self.af));
    }

    /// Reset the CRC calculations by temporarily disabling the bus.
    ///
    /// Note: this is also implicitly done by a disable() + enable() sequence
    pub fn reset_crc_state(&mut self) {
        // RM0432 Rev 9 52.4.14 (P1876) describes this sequence:
        modify_reg!(spi, self.peripheral, CR1, SPE: 0);
        modify_reg!(spi, self.peripheral, CR1, CRCEN: 0);
        modify_reg!(spi, self.peripheral, CR1, CRCEN: 1);
        modify_reg!(spi, self.peripheral, CR1, SPE: 1);
    }

    /// Read RX CRC: CRC8 checksum of all received bytes since [Bus::enable()] or [Bus::reset_crc_state()]
    pub fn read_crc_rx(&self) -> u8 {
        (read_reg!(spi, self.peripheral, RXCRCR) & 0xFF) as u8
    }

    pub fn disable(&mut self, mode: IdleMode) {
        // TODO RM0432 Rev 9 P 1864: The correct disable procedure is (except when receive only mode is used):
        // 1.Wait until FTLVL[1:0] = 00 (no more data to transmit).
        // 2.Wait until BSY=0 (the last data frame is processed).
        // 3.Disable the SPI (SPE=0).
        // 4.Read data until FRLVL[1:0] = 00 (read all the received data).

        // Park bus pins *before* the RCC reset so there's no window where
        // the pin is in `Alt` mode pointing at a reset peripheral with
        // undefined output. See `IdleMode` for the per-mode rationale.
        let clk_idle = self.last_cpol.map_or(gpio::PinState::Low, cpol_idle_state);
        match mode {
            IdleMode::MosiHigh => {
                self.mosi.mode(gpio::PinMode::Output(gpio::PinState::High));
                self.clock.mode(gpio::PinMode::Output(clk_idle));
            }
            IdleMode::MosiLow => {
                self.mosi.mode(gpio::PinMode::Output(gpio::PinState::Low));
                self.clock.mode(gpio::PinMode::Output(clk_idle));
            }
            IdleMode::Off => {
                // Rail going off — drive both lines low to prevent
                // back-feed through the device's input protection diodes.
                // Accepts that the next `enable()` may produce one CLK edge
                // (Output(Low) → Alt at CPOL-idle), but that edge happens
                // during the device's power-on/NOT_READY window where the
                // device hasn't yet started observing the SPI protocol.
                self.mosi.mode(gpio::PinMode::Output(gpio::PinState::Low));
                self.clock.mode(gpio::PinMode::Output(gpio::PinState::Low));
            }
        }

        match self.id() {
            BusID::B1 => rcc::disable_rst_spi1(),
        }
        self.enabled = false;
    }

    pub async fn perform_operation(
        &mut self,
        operation: &mut SPIOperation<'_>,
    ) -> Result<(), Error> {
        let mut tx_index = 0;
        let mut rx_index = 0;

        loop {
            // 1. Wait for space in the TX FIFO
            self.untill(Event::TXEmpty).await?;

            // 2. Fill the TX FIFO as much as possible
            let done = loop {
                // Get next byte to send (if any)
                if let Some(tx_byte) = Buffer::transfer(operation, tx_index).await {
                    // Check the TX FIFO has space remaining *and* the response will fit in the rx FIFO.
                    //
                    // TX/RX FIFOs are both 32-bits (4 byte). As SPI can be quite fast, it may be able to keep up
                    // with the CPU so that fifo_level does not reach 0b11 after > 4 bytes, resulting in rx overflow.
                    if read_reg!(spi, self.peripheral, SR, FTLVL != 0b11)
                        && (tx_index - rx_index) < 4
                    {
                        // NB: DR register is special: it sends 1 or 2 bytes *per write access* depending on the
                        // width of the write. By casting to *u8, we always send one byte at a time.
                        {
                            let mut_dr_ptr = ptr::addr_of!(self.peripheral.DR) as *mut u8;
                            unsafe { ptr::write_volatile(mut_dr_ptr, tx_byte) };
                        }
                        tx_index += 1;
                    } else {
                        break false;
                    }

                // Done transmitting
                } else {
                    break true;
                }
            };

            // 3. Empty the RX FIFO
            while rx_index < tx_index {
                // Unfortunately the FIFO is quite limited, so we'll have one interrupt per byte.
                // If we ever want to optimize it we should use DMA.
                self.untill(Event::RXNotEmpty).await?;
                self.try_rx_byte(operation, &mut rx_index);
            }

            // 4. This operation is done if all data transmitted and rx_index == tx_index
            if done {
                break;
            }
        }

        Ok(())
    }

    fn try_rx_byte(&mut self, operation: &mut SPIOperation, rx_index: &mut usize) {
        if read_reg!(spi, self.peripheral, SR, FRLVL != 0) {
            // NB: DR register is special: it yields 1 or 2 bytes *per read access* depending on the
            // width of the read. read_reg!(octospi, self.peripheral, DR) would read a 2 bytes at a time
            // from the FIFO. This would make the logic a lot more complicated.
            // By casting to *u8, we always read one byte at a time.
            let rx = {
                let dr_ptr = ptr::addr_of!(self.peripheral.DR) as *const u8;
                unsafe { ptr::read_volatile(dr_ptr) }
            };

            //let rx = self.peripheral.DR.read() as u8;
            Buffer::store_received(operation, *rx_index, rx);
            *rx_index += 1;
        }
    }

    /// ISR function: must be called from the SPI1 Interrupt handler
    ///
    /// ## Safety
    ///
    /// This function is marked unsafe because it must be called from the correct interrupt handler only.
    pub unsafe fn isr_bus1() {
        // Disable interrupts and wake the waker.
        // The waker will re-enable interrupts if it wants to await them
        interrupt::free(|_| unsafe {
            modify_reg!(spi, SPI1, CR2,
                ERRIE: 0,
                TXEIE: 0,
                RXNEIE: 0
            )
        });
        BusID::B1.waker().wake();
    }

    async fn untill(&self, event: Event) -> Result<(), Error> {
        poll_fn(|cx| {
            self.id().waker().register(cx.waker());
            // 1. Return any unhandled error that has already occurred

            if read_reg!(spi, self.peripheral, SR, OVR != 0) {
                return Poll::Ready(Err(Error::Overrun));
            }
            if read_reg!(spi, self.peripheral, SR, MODF != 0) {
                return Poll::Ready(Err(Error::Mode));
            }

            // 2. Return if the even has already occurred
            match event {
                Event::TXEmpty => {
                    if read_reg!(spi, self.peripheral, SR, TXE != 0) {
                        return Poll::Ready(Ok(()));
                    }
                }
                Event::RXNotEmpty => {
                    if read_reg!(spi, self.peripheral, SR, RXNE != 0) {
                        return Poll::Ready(Ok(()));
                    }
                }
            }

            // 3. Setup IRQ to wake us when a relevant event hapens
            interrupt::free(|_| {
                modify_reg!(spi, self.peripheral, CR2,
                    ERRIE: 1,
                    TXEIE: u32::from(event == Event::TXEmpty),
                    RXNEIE: u32::from(event == Event::RXNotEmpty)
                )
            });
            Poll::Pending
        })
        .await
    }
}

#[derive(Debug)]
pub enum Error {
    /// Another device driver has claimed the SPI bus
    Busy,

    Overrun,

    Mode,
}

/// SPI Device connected via an SPI `Bus`
///
/// Represents one device on the SPI `Bus` with a dedicated `chip_select` pin.
/// Intended to be used via the `embedded_hal_async::spi::SpiDevice` trait
///
/// Note that the device can be initialized with a chip_select pin that is low to support a bus
/// which is powered down. Can later be powered up/down via the uspended trait.
pub struct Device {
    config: Config,
    bus: Bus,
    cs_pin: gpio::Pin,
    crc_poly: Option<u8>,
    rx_crc8: u8,
    enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Event {
    /// TXE flag: ready to transmit one or more bytes
    TXEmpty,

    /// RX FIFO contains some data
    RXNotEmpty,
}

impl Device {
    /// Create a new SPIDevice on the given bus.
    ///
    /// Currently the device takes ownership of the bus to keep things simple.
    /// But multiple devices on one bus could be supported in theory
    ///
    /// Note: the chip_select pin may be initialized low to support a bus which is powered
    /// down. In any case, [Self::resume()] drives it high (automatically called on the first
    /// transaction).
    pub fn new(spi_bus: Bus, chip_select: gpio::Pin, config: Config) -> Self {
        Self {
            bus: spi_bus,
            cs_pin: chip_select,
            config,
            crc_poly: None,
            rx_crc8: 0xFF, // initialize at unexpected value
            enabled: false,
        }
    }

    fn start_transaction(&mut self) -> Result<&mut Bus, Error> {
        // resume in case we were asleep
        self.resume();

        // Note: this resets & reconfigures the peripheral for each SPI transaction.
        // This might have some performance impact when doing lots of small transactions.
        // But for now this is probably good enough.
        // NOTE: this also resets the CRC state

        self.bus.enable(&self.config, self.crc_poly)?;
        // CS must assert *before* MOSI moves into Alt mode — the peripheral
        // idle level may differ from the parked GPIO level, so the
        // resulting edge should happen while the device is selected and
        // actively driving MISO. That overpowers any MOSI→MISO coupling
        // and prevents the undershoot that would occur if the edge
        // happened with MISO high-Z.
        self.cs_pin.set_low();
        self.bus.engage_mosi();
        Ok(&mut self.bus)
    }

    fn stop_transaction(&mut self) {
        self.cs_pin.set_high();
        self.rx_crc8 = self.bus.read_crc_rx();
        // The SPI flash on this bus tolerates MOSI-high at idle. If a future device needs low-idle, this
        // call site should grow per-device configuration.
        self.bus.disable(IdleMode::MosiHigh);
    }
}

type ErrorKind = embedded_hal_async::spi::ErrorKind;
impl embedded_hal_async::spi::Error for Error {
    fn kind(&self) -> ErrorKind {
        match self {
            Error::Busy => ErrorKind::Other,
            Error::Overrun => ErrorKind::Overrun,
            Error::Mode => ErrorKind::ModeFault,
        }
    }
}

impl embedded_hal_async::spi::ErrorType for Device {
    type Error = Error;
}

impl embedded_hal_async::spi::SpiDevice for Device {
    async fn transaction(
        &mut self,
        operations: &mut [SPIOperation<'_>],
    ) -> Result<(), Self::Error> {
        let use_crc = self.crc_poly.is_some();
        let bus = self.start_transaction()?;
        let mut result = Ok(());

        for op in operations {
            let op_res = bus.perform_operation(op).await;
            if op_res.is_err() {
                result = op_res;
            }
            // Bytes clocked in *while transmitting* a Write are not part of
            // the device's CRC-covered frame, so keep them out of the RX CRC.
            // E.g. the ADS114S08 mirrors status bits during the RDATA command
            // byte (nonzero whenever a flag is set); its actual frame,
            // STATUS + data + CRC as specified in datasheet 9.3.13, arrives in
            // the Read that follows and is fully covered by this CRC check.
            if use_crc && matches!(op, SPIOperation::Write(_)) {
                bus.reset_crc_state();
            }
        }
        self.stop_transaction();

        result
    }
}

impl traits::Suspend for Device {
    fn suspend(&mut self) {
        if self.enabled {
            self.enabled = false;

            // Park bus pins for power-down: CLK + MOSI both low. Re-enabled
            // on first transaction.
            self.bus.disable(IdleMode::Off);

            self.cs_pin.set_low();
        }
    }

    fn resume(&mut self) {
        if !self.enabled {
            self.enabled = true;
            self.cs_pin.set_high();
        }
    }

    fn is_suspended(&self) -> bool {
        !self.enabled
    }
}

#[derive(Debug)]
pub struct ErrPolynomialNotOdd;

impl traits::CRC8Receiver for Device {
    type Error = ErrPolynomialNotOdd;

    fn configure_polynomial(&mut self, poly: u8) -> Result<(), Self::Error> {
        if poly & 1 != 1 {
            return Err(ErrPolynomialNotOdd);
        }
        self.crc_poly = Some(poly);
        Ok(())
    }

    fn rx_crc8(&mut self) -> u8 {
        //crc8 of all rx data in last transaction
        self.rx_crc8
    }
}

/// Helper trait to make working with embedded_hal_async::spi::Operation manageable
trait Buffer {
    /// Yields the byte to be sent. None means no more data to transmit OR receive
    async fn transfer(&self, index: usize) -> Option<u8>;

    /// Stores the rx byte from previous transfer in the rx buffer (if any)
    fn store_received(&mut self, index: usize, byte: u8);
}

impl Buffer for embedded_hal_async::spi::Operation<'_, u8> {
    async fn transfer(&self, index: usize) -> Option<u8> {
        let buffer = match self {
            // Delay: await delay then the transfer is immediately complete
            embedded_hal_async::spi::Operation::DelayNs(delay) => {
                delay_us(*delay as u64 * 1000).await;
                return None;
            }

            // Read only: tx dummy byte
            embedded_hal_async::spi::Operation::Read(rx_buffer) => {
                return rx_buffer.get(index).map(|_| 0);
            }

            // Special Tx variant: tx dummy byte if tx buffer is shorter than rx buffer
            embedded_hal_async::spi::Operation::Transfer(rx_buffer, buffer) => {
                return match buffer.get(index) {
                    Some(tx) => Some(*tx),
                    None if index < rx_buffer.len() => Some(0),
                    None => None,
                };
            }

            // Tx variants: get the tx_buffer
            embedded_hal_async::spi::Operation::Write(buffer) => buffer,
            embedded_hal_async::spi::Operation::TransferInPlace(buffer) => buffer.deref(),
        };

        // Get byte to transmit (if any)
        buffer.get(index).map(|tx| *tx)
    }

    fn store_received(&mut self, index: usize, byte: u8) {
        let rx_buffer = match self {
            // Non-rx variants: NOP
            embedded_hal_async::spi::Operation::DelayNs(_) => {
                return;
            }
            embedded_hal_async::spi::Operation::Write(_) => {
                return;
            }

            // Rx variants: get rx buffer
            embedded_hal_async::spi::Operation::Read(buffer) => buffer,
            embedded_hal_async::spi::Operation::Transfer(buffer, _) => buffer,
            embedded_hal_async::spi::Operation::TransferInPlace(buffer) => buffer,
        };

        if index < rx_buffer.len() {
            rx_buffer[index] = byte;
        }
    }
}

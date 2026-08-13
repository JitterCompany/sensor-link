//! SPI Flash driver
//!
//! Platform-independent async SPI flash driver using the [SpiDevice](embedded_hal_async::spi::SpiDevice) trait.
//! This crate implements an SPI driver that is generic over any [SpiDevice](embedded_hal_async::spi::SpiDevice) implementation.
//!
//! See [Manufacturer] for SPI flash brands that have been tested to work with this driver. As SPI flash is fairly standardized,
//! most other brands are probably also compatible.

use core::ops::Deref;

pub use embedded_hal_async::spi;
use num_enum::TryFromPrimitive;

use crate::{
    monotonic_time::delay_us,
    traits::Suspend,
    utils::bitwise::{width8::Bit, Bitfield},
};

/// Describes the properties of the flash chip
///
/// Some properties could be auto-detected but for now must be known in advance.
/// Please look these up in the relevant datasheet for your flash chip.
/// Note: call `validate()` to obtain a [ValidFlashDescriptor] for constructing a [SPIFlash].
pub struct FlashDescriptor {
    pub page_size: usize,
    pub erase_size: usize,
    /// Also selects the addressing mode: chips >16MB are switched to 4-byte addressing
    pub total_size: usize,

    /// Maximum time the chip takes to program a page
    pub program_timeout_us: u32,

    /// Maximum time the chip takes to erase a sector
    pub erase_timeout_us: u32,

    /// Maximum time the chip takes to wakeup from sleep / enter sleep mode
    pub sleep_timeout_us: u32,
}
#[derive(Debug, Clone, PartialEq)]
pub enum DescriptorError {
    PageSizeInvalid,
    EraseSizeNotMultipleOfPage,
    TotalSizeNotMultipleOfErase,
    EraseSizeNotMultipleMaxEraseSize,
}

impl FlashDescriptor {
    pub fn validate<const MAX_ERASE_SIZE: usize>(
        self,
    ) -> Result<ValidFlashDescriptor<MAX_ERASE_SIZE>, DescriptorError> {
        if self.page_size < 1 || !self.page_size.is_power_of_two() {
            return Err(DescriptorError::PageSizeInvalid);
        }
        if !self.erase_size.is_multiple_of(self.page_size) {
            return Err(DescriptorError::EraseSizeNotMultipleOfPage);
        }

        if !self.total_size.is_multiple_of(self.erase_size) {
            return Err(DescriptorError::TotalSizeNotMultipleOfErase);
        }

        if !MAX_ERASE_SIZE.is_multiple_of(self.erase_size) {
            return Err(DescriptorError::EraseSizeNotMultipleMaxEraseSize);
        }

        Ok(ValidFlashDescriptor(self))
    }
}

/// Wrapper to enforce validation. Can be used as FlashDescriptor
pub struct ValidFlashDescriptor<const MAX_ERASE_SIZE: usize>(FlashDescriptor);
impl<const MAX_ERASE_SIZE: usize> Deref for ValidFlashDescriptor<MAX_ERASE_SIZE> {
    type Target = FlashDescriptor;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Supported SPI flash manufacturer codes
///
/// This driver is likely compatible with much more
/// manufacturers. But let's only whitelist them if they are known to work..
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, TryFromPrimitive)]
pub enum Manufacturer {
    /// Infineon (formerly Cypress/Spansion), e.g. S25FL064L
    InfineonCypress = 0x01,
    /// Winbond (formerly Nexcom?)
    WinBondNexcom = 0xEF,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// Flash chip / feature not supported
    UnSupported,

    /// SPI flash size does not match the descriptor
    SizeMismatch,

    /// Something went wrong with the SPI bus
    SPI,

    /// Read/write out of bounds attempted
    OutOfBounds,

    /// Address / length passed to some methods needs specific alignment
    Alignment,

    /// Write failed. Write protection may be active
    Write,

    /// Operation timed out: flash chip stays busy beyond the normal time limits
    Timeout,
}

#[derive(Debug, Clone)]
pub struct Status {
    bits: u8,
}

impl Status {
    pub fn is_busy(&self) -> bool {
        self.bits.bit(Bit::B0)
    }

    pub fn is_write_enable_set(&self) -> bool {
        self.bits.bit(Bit::B1)
    }
}

impl From<u8> for Status {
    fn from(value: u8) -> Self {
        Status { bits: value }
    }
}

/// Address in the SPI flash
///
/// Addresses a specific byte stored in SPI flash.
/// Can be created via [From\<u32\>](From)
pub struct Address(u32);

impl From<u32> for Address {
    fn from(value: u32) -> Self {
        Address(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    B24, // 24-bit address mode. Suitable up to 16MByte
    B32, // 32-bit address mode. Suitable up to 4GByte
}

/// SPI Flash instance. Represents a SPI flash device
pub struct SPIFlash<DEV, const MAX_ERASE_SIZE: usize> {
    flash: Sleepy<DEV>,
    cfg: ValidFlashDescriptor<MAX_ERASE_SIZE>,
    mode: Mode,
    stats: Stats,
}

#[derive(Debug, Default)]
struct Stats {
    read_bytes: u32,
    read_attempts: u32,
    reads: u32,
    erase_attempts: u32,
}

struct Sleepy<DEV> {
    device: DEV,
    awake: bool,
    sleep_delay_us: u32,
}

impl<DEV> Sleepy<DEV>
where
    DEV: spi::SpiDevice + Suspend,
{
    fn new(device: DEV, sleep_delay_us: u32) -> Self {
        Self {
            device,
            awake: false,
            sleep_delay_us,
        }
    }

    /// Wakeup the device if it is not already awake
    pub async fn wakeup(&mut self) -> Result<&mut DEV, Error> {
        if !self.awake {
            // resume
            {
                // Retry a few attempts if it fails
                let mut attempt = 0;
                loop {
                    // (In case sleep was called just before immediately waking up
                    // wait untill the chip is actually in sleep mode)
                    delay_us(self.sleep_delay_us as u64).await;

                    match self.device.send_command(Command::Wakeup).await {
                        Ok(_) => break,

                        Err(error) => {
                            attempt += 1;
                            if attempt >= 5 {
                                // Device non-responsive.
                                // Try to put it to sleep and report the original errorcode
                                self.sleep().await;
                                return Err(error);
                            }
                        }
                    }
                }

                // Give chip time to wakeup
                delay_us(self.sleep_delay_us as u64).await;
            }
            self.awake = true;
        }
        Ok(&mut self.device)
    }

    /// Put the device to sleep
    pub async fn sleep(&mut self) {
        self.awake = false;
        // sleep
        {
            // Best effort attempt: failure is not critical, just slightly higher power consumption
            for _ in 0..2 {
                if self.device.send_command(Command::Sleep).await.is_ok() {
                    // device should enter low power mode within self.sleep_delay_us
                    break;
                }
            }
        }
    }
}

trait LowlevelFlash {
    async fn program_page(
        &mut self,
        timeout_us: u32,
        cmd_addr: &[u8],
        bytes: &[u8],
    ) -> Result<(), Error>;

    async fn is_ready(&mut self, timeout_us: u32) -> Result<(), Error>;
    async fn status(&mut self) -> Result<Status, Error>;
    async fn write_enable(&mut self) -> Result<(), Error>;
    async fn read_reg(&mut self, cmd: Command, result: &mut [u8]) -> Result<(), Error>;
    async fn send_command(&mut self, cmd: Command) -> Result<(), Error>;
}

impl<DEV> LowlevelFlash for DEV
where
    DEV: spi::SpiDevice + Suspend,
{
    /// Program up to one page of data
    ///
    /// Note: this is a low-level API which assumes the parameters are validated and the page
    /// is already pre-erased.
    async fn program_page(
        &mut self,
        timeout_us: u32,
        cmd_addr: &[u8],
        bytes: &[u8],
    ) -> Result<(), Error> {
        self.write_enable().await?;

        self.transaction(&mut [
            spi::Operation::Write(cmd_addr),
            spi::Operation::Write(bytes),
        ])
        .await
        .map_err(|_| Error::SPI)?;

        self.is_ready(timeout_us).await
    }

    /// Await the completion of the last operation (device no longer busy)
    async fn is_ready(&mut self, timeout_us: u32) -> Result<(), Error> {
        // Split the timeout in 10 steps. This is an arbitrary tradeoff (more steps would be faster but also use more CPU = more power)
        let timeout_us = (timeout_us / 10) as u64;

        for _ in 0..10 {
            if !self.status().await?.is_busy() {
                return Ok(());
            }
            delay_us(timeout_us).await;
        }

        Err(Error::Timeout)
    }

    /// Read the flash chip status
    async fn status(&mut self) -> Result<Status, Error> {
        let mut status = [0];
        self.read_reg(Command::ReadStatus, &mut status).await?;
        Ok(Status::from(status[0]))
    }

    /// Set the Write Enable Latch (WEL).
    ///
    /// Must always be performend before each write/erase, as the WEL is reset after each write/erase
    async fn write_enable(&mut self) -> Result<(), Error> {
        self.send_command(Command::WriteEnable).await?;
        match self.status().await?.is_write_enable_set() {
            true => Ok(()),
            false => Err(Error::Write),
        }
    }

    async fn read_reg(&mut self, cmd: Command, result: &mut [u8]) -> Result<(), Error> {
        let tx = [cmd as u8];
        self.transaction(&mut [spi::Operation::Write(&tx), spi::Operation::Read(result)])
            .await
            .map_err(|_| Error::SPI)
    }

    async fn send_command(&mut self, cmd: Command) -> Result<(), Error> {
        self.write(&[cmd as u8]).await.map_err(|_| Error::SPI)
    }
}

impl<DEV, const MAX_ERASE_SIZE: usize> SPIFlash<DEV, MAX_ERASE_SIZE>
where
    DEV: spi::SpiDevice + Suspend,
{
    /// Instantiate a SPIFlash device
    ///
    /// requires an implementation of [SpiDevice](embedded_hal_async::spi::SpiDevice) and a validated descriptor.
    /// See [FlashDescriptor](FlashDescriptor::validate)
    pub fn new(spi_dev: DEV, descriptor: ValidFlashDescriptor<MAX_ERASE_SIZE>) -> Self {
        Self {
            flash: Sleepy::new(spi_dev, descriptor.sleep_timeout_us),
            cfg: descriptor,
            mode: Mode::B24,
            stats: Stats::default(),
        }
    }

    /// Returns a descriptor with meta-information about the SPI flash
    ///
    /// Note: this is the same descriptor that was provided to [SPIFlash::new()]
    pub fn descriptor(&self) -> &ValidFlashDescriptor<MAX_ERASE_SIZE> {
        &self.cfg
    }

    /// Detect the flash type and check if it is supported
    ///
    /// Try to detect the [Manufacturer] and validate the `total_size` from the [FlashDescriptor].
    /// If used with an unsupported flash chip, add it to [Manufacturer] or just don't call this method.
    pub async fn detect_type(&mut self) -> Result<Manufacturer, Error> {
        let mut result = [0_u8; 3];

        let spi = self.flash.wakeup().await?;
        if let Err(err) = spi.read_reg(Command::ReadJEDEC, &mut result).await {
            self.flash.sleep().await;
            return Err(err);
        }

        // Calculate & verify density is as expected
        {
            let density = result[2];
            if density > 31 {
                self.flash.sleep().await;
                return Err(Error::SizeMismatch);
            }
            let size = 1 << result[2];
            if size != self.cfg.0.total_size {
                self.flash.sleep().await;
                return Err(Error::SizeMismatch);
            }
        }

        let id = result[0];
        match Manufacturer::try_from(id) {
            Ok(manufacturer) => Ok(manufacturer),
            Err(_) => {
                self.flash.sleep().await;
                Err(Error::UnSupported)
            }
        }
    }

    /// Read data from the flash memory at a specific address
    ///
    /// Any slice of data can be read from the flash as long as it stays within the bounds of the flash total_size
    pub async fn read(&mut self, address: Address, bytes: &mut [u8]) -> Result<(), Error> {
        if self.stats.read_attempts.is_multiple_of(2048) {
            // log::debug!(target: "SPIFlash", "Stats: {} bytes read, {} erase attempts", self.stats.read_bytes, self.stats.erase_attempts);
            self.stats = Stats::default();
        }
        self.stats.read_attempts += 1;

        let (spi, addr, mode) = self.init_and_check_addr(address, bytes.len()).await?;

        let result = spi
            .transaction(&mut [
                spi::Operation::Write(&cmd_and_addr(Command::Read, addr, mode)),
                spi::Operation::Read(bytes),
            ])
            .await
            .map_err(|_| Error::SPI);

        self.stats.read_bytes += bytes.len() as u32;
        self.stats.reads += 1;
        self.flash.sleep().await;
        result
    }

    /// Erase a range of data from address..addres+len
    ///
    /// Note: both address and len must be a multiple of `erase_size`.
    /// See [descriptor](SPIFlash::descriptor())
    ///
    /// Returns an [ErasedRange] which can be used to write data to the flash chip.
    pub async fn erase(
        &mut self,
        address: Address,
        len: usize,
    ) -> Result<ErasedRange<'_, DEV, MAX_ERASE_SIZE>, Error> {
        let erase_size = self.cfg.0.erase_size;
        let erase_timeout_us = self.cfg.erase_timeout_us;

        self.stats.erase_attempts += 1;

        // Note: below this point, we must put spi to sleep before returning
        let (spi, addr, mode) = self.init_and_check_addr(address, len).await?;

        if !(addr as usize).is_multiple_of(erase_size) || !len.is_multiple_of(erase_size) {
            self.flash.sleep().await;
            return Err(Error::Alignment);
        }

        // In B32 mode, keep the explicitly-4-byte erase opcode
        let erase_cmd = match mode {
            Mode::B24 => Command::EraseSector,
            Mode::B32 => Command::EraseSector4B,
        };

        let mut error = Ok(());
        for offset in (0..len).step_by(erase_size) {
            if let Err(e) = spi.write_enable().await {
                error = Err(e);
                break;
            }

            if spi
                .write(&cmd_and_addr(erase_cmd, addr + offset as u32, mode))
                .await
                .is_err()
            {
                error = Err(Error::SPI);
                break;
            }

            if let Err(e) = spi.is_ready(erase_timeout_us).await {
                error = Err(e);
                break;
            }
        }

        // Put flash back to sleep before returning results
        self.flash.sleep().await;

        error?;
        Ok(ErasedRange::new(self, addr, len))
    }

    /// Program arbitrary amount of data
    ///
    /// Note: this is assumes the memory range to be programmed is pre-erased
    async fn program(&mut self, address: Address, bytes: &[u8]) -> Result<(), Error> {
        let page_timeout_us = self.cfg.program_timeout_us;

        // program block size is not necessarily the same as PAGE_SIZE (e.g. 4k PAGE_SIZE, 256 program_size)
        // SPI flash cannot be programmed across page boundaries,
        // so the write may have to be split into multiple chunks (depending on data.len())
        let program_size = self.descriptor().page_size;

        // Note: below this point, we must put spi to sleep before returning
        let (spi, addr, mode) = self.init_and_check_addr(address, bytes.len()).await?;

        let mut block_offset: usize = 0;
        let result = loop {
            // Find maximum remaining space in this block
            let block_address = addr + block_offset as u32;
            let block_max_len = program_size - (block_address as usize % program_size);

            let bytes_to_program = (bytes.len() - block_offset).min(block_max_len);
            if bytes_to_program == 0 {
                break Ok(());
            }

            if let Err(err) = spi
                .program_page(
                    page_timeout_us,
                    &cmd_and_addr(Command::ProgramPage, block_address, mode),
                    &bytes[block_offset..block_offset + bytes_to_program],
                )
                .await
            {
                break Err(err);
            }

            // Advance to next block boundary (or end of data)
            block_offset += bytes_to_program;
        };

        // put flash back to sleep before returning result
        self.flash.sleep().await;
        result
    }

    async fn init_and_check_addr(
        &mut self,
        addr: Address,
        len: usize,
    ) -> Result<(&mut DEV, u32, Mode), Error> {
        let addr = bounds_check(addr, len, 0, self.cfg.total_size)?;

        match self.mode {
            Mode::B32 => {}

            Mode::B24 => {
                // This chip should support more than 24 bits addressing
                if self.cfg.total_size > 0x1_00_00_00 {
                    log::debug!(target: "SPIFlash", "Switch to 32-bit mode");
                    if let Err(err) = self
                        .flash
                        .wakeup()
                        .await?
                        .send_command(Command::Enter4ByteMode)
                        .await
                    {
                        self.flash.sleep().await;
                        return Err(err);
                    }
                    self.mode = Mode::B32;
                }
            }
        }

        let mode = self.mode;
        Ok((self.flash.wakeup().await?, addr, mode))
    }
}

/// Command + address wire frame: 3 address bytes in [Mode::B24], 4 in [Mode::B32]
struct CmdAddr {
    buf: [u8; 5],
    len: usize,
}

impl Deref for CmdAddr {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

/// Helper function to combine command + address to byte array
fn cmd_and_addr(cmd: Command, addr: u32, mode: Mode) -> CmdAddr {
    let addr = addr.to_be_bytes();
    match mode {
        // Top address byte is dropped: bounds_check guarantees addr < total_size <= 16MB
        Mode::B24 => CmdAddr {
            buf: [cmd as u8, addr[1], addr[2], addr[3], 0],
            len: 4,
        },
        Mode::B32 => CmdAddr {
            buf: [cmd as u8, addr[0], addr[1], addr[2], addr[3]],
            len: 5,
        },
    }
}

fn bounds_check(address: Address, len: usize, min_addr: u32, max_len: usize) -> Result<u32, Error> {
    if address.0 < min_addr {
        return Err(Error::OutOfBounds);
    }

    if len > max_len {
        return Err(Error::OutOfBounds);
    }

    let offset = address.0 - min_addr;
    if offset > (max_len - len) as u32 {
        return Err(Error::OutOfBounds);
    }

    Ok(address.0)
}

/// Erased range of flash: ready for writing data
///
/// This represents part of the [SPIFlash] that is erased
/// and ready to be written to ('programmed').
///
/// Can only be obtained by calling [SPIFlash::erase()]
pub struct ErasedRange<'a, DEV, const MAX_ERASE_SIZE: usize> {
    flash: &'a mut SPIFlash<DEV, MAX_ERASE_SIZE>,
    start_addr: u32,
    max_len: usize,
}

impl<'a, DEV, const MAX_ERASE_SIZE: usize> ErasedRange<'a, DEV, MAX_ERASE_SIZE>
where
    DEV: spi::SpiDevice + Suspend,
{
    fn new(flash: &'a mut SPIFlash<DEV, MAX_ERASE_SIZE>, start_addr: u32, len: usize) -> Self {
        Self {
            flash,
            start_addr,
            max_len: len,
        }
    }

    /// Write a slice of data to the SPI flash
    ///
    /// Note that the Address is absolute and must be within this erased range.
    /// If any of the writes would fall outside the erased range, [Error::OutOfBounds] is returned.
    pub async fn write(&mut self, address: Address, bytes: &[u8]) -> Result<(), Error> {
        let addr = bounds_check(address, bytes.len(), self.start_addr, self.max_len)?;

        self.flash.program(Address::from(addr), bytes).await
    }

    /// Write a slice of data to the SPI flash
    ///
    /// Same as [ErasedRange::write()] but writes at an offset relative to the start of the erased range
    /// instead of an absolue flash address.
    pub async fn write_relative(
        &mut self,
        relative_offset: usize,
        bytes: &[u8],
    ) -> Result<(), Error> {
        if relative_offset >= self.max_len {
            return Err(Error::OutOfBounds);
        }

        let address = Address::from(relative_offset as u32 + self.start_addr);
        self.write(address, bytes).await
    }
}

#[allow(unused)]
#[derive(Clone, Copy)]
enum Command {
    Wakeup = 0xAB,
    Sleep = 0xB9,
    PowerDown = 0x79,

    ReadJEDEC = 0x9F,
    ReadStatus = 0x05,
    WriteStatus = 0x01,

    Enter4ByteMode = 0xB7,

    Read = 0x03,

    WriteEnable = 0x06,
    WriteDisable = 0x04,

    // Sector erase with 3-byte address (in B24 mode) / 4-byte address (in B32 mode)
    EraseSector = 0x20,
    // Sector erase with 4-byte address, regardless of mode
    EraseSector4B = 0x21,

    ProgramPage = 0x02,
}

impl embedded_storage::nor_flash::NorFlashError for Error {
    fn kind(&self) -> embedded_storage::nor_flash::NorFlashErrorKind {
        match self {
            Error::UnSupported => embedded_storage::nor_flash::NorFlashErrorKind::Other,
            Error::SizeMismatch => embedded_storage::nor_flash::NorFlashErrorKind::Other,
            Error::SPI => embedded_storage::nor_flash::NorFlashErrorKind::Other,
            Error::OutOfBounds => embedded_storage::nor_flash::NorFlashErrorKind::OutOfBounds,
            Error::Alignment => embedded_storage::nor_flash::NorFlashErrorKind::NotAligned,
            Error::Write => embedded_storage::nor_flash::NorFlashErrorKind::Other,
            Error::Timeout => embedded_storage::nor_flash::NorFlashErrorKind::Other,
        }
    }
}

impl<DEV, const MAX_ERASE_SIZE: usize> embedded_storage::nor_flash::ErrorType
    for SPIFlash<DEV, MAX_ERASE_SIZE>
where
    DEV: spi::SpiDevice + Suspend,
{
    type Error = Error;
}

impl<DEV, const MAX_ERASE_SIZE: usize> embedded_storage_async::nor_flash::ReadNorFlash
    for SPIFlash<DEV, MAX_ERASE_SIZE>
where
    DEV: spi::SpiDevice + Suspend,
{
    const READ_SIZE: usize = 1;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        (*self).read(Address::from(offset), bytes).await
    }

    fn capacity(&self) -> usize {
        self.descriptor().total_size
    }
}

impl<DEV, const MAX_ERASE_SIZE: usize> embedded_storage_async::nor_flash::NorFlash
    for SPIFlash<DEV, MAX_ERASE_SIZE>
where
    DEV: spi::SpiDevice + Suspend,
{
    const WRITE_SIZE: usize = 1;

    /// NB: actual erase size might be smaller but not known at compile time
    const ERASE_SIZE: usize = MAX_ERASE_SIZE;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let len = (to - from) as usize;

        (*self).erase(Address::from(from), len).await.map(|_| ())
    }

    async fn write(&mut self, address: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.program(Address::from(address), bytes).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::vec::Vec;

    /// Mock SpiDevice: records the written bytes of every transaction and
    /// answers Read operations from a scripted queue (default: 0x02, which
    /// reads as "WEL set, not busy" when polled as a status register).
    struct MockSpi {
        transactions: Vec<Vec<u8>>,
        read_responses: VecDeque<Vec<u8>>,
    }

    impl MockSpi {
        fn new() -> Self {
            Self {
                transactions: Vec::new(),
                read_responses: VecDeque::new(),
            }
        }

        fn respond(&mut self, bytes: &[u8]) {
            self.read_responses.push_back(bytes.to_vec());
        }

        /// All recorded transactions that start with `cmd`
        fn sent(&self, cmd: u8) -> Vec<&Vec<u8>> {
            self.transactions
                .iter()
                .filter(|t| t.first() == Some(&cmd))
                .collect()
        }
    }

    #[derive(Debug)]
    struct MockError;
    impl spi::Error for MockError {
        fn kind(&self) -> spi::ErrorKind {
            spi::ErrorKind::Other
        }
    }
    impl spi::ErrorType for MockSpi {
        type Error = MockError;
    }

    impl spi::SpiDevice for MockSpi {
        async fn transaction(
            &mut self,
            operations: &mut [spi::Operation<'_, u8>],
        ) -> Result<(), MockError> {
            let mut written = Vec::new();
            for op in operations.iter_mut() {
                match op {
                    spi::Operation::Write(bytes) => written.extend_from_slice(bytes),
                    spi::Operation::Read(buf) => {
                        let response = self.read_responses.pop_front().unwrap_or_default();
                        for (i, b) in buf.iter_mut().enumerate() {
                            *b = response.get(i).copied().unwrap_or(0x02);
                        }
                    }
                    _ => unimplemented!(),
                }
            }
            self.transactions.push(written);
            Ok(())
        }
    }

    impl Suspend for MockSpi {
        fn suspend(&mut self) {}
        fn resume(&mut self) {}
        fn is_suspended(&self) -> bool {
            false
        }
    }

    fn flash(total_size: usize) -> SPIFlash<MockSpi, 4096> {
        let descriptor = FlashDescriptor {
            page_size: 256,
            erase_size: 4096,
            total_size,
            program_timeout_us: 1_000,
            erase_timeout_us: 1_000,
            sleep_timeout_us: 1,
        }
        .validate()
        .expect("valid descriptor");
        SPIFlash::new(MockSpi::new(), descriptor)
    }

    const MB: usize = 1024 * 1024;

    #[tokio::test]
    async fn b24_read_sends_3_address_bytes() {
        let mut flash = flash(8 * MB);

        let mut buf = [0; 4];
        flash.read(0x123456.into(), &mut buf).await.unwrap();

        let spi = &flash.flash.device;
        assert_eq!(spi.sent(0x03), [&vec![0x03, 0x12, 0x34, 0x56]]);
        assert!(spi.sent(0xB7).is_empty(), "must not enter 4-byte mode");
    }

    #[tokio::test]
    async fn b24_erase_sends_0x20_with_3_address_bytes() {
        let mut flash = flash(8 * MB);

        flash.erase(0x5000.into(), 2 * 4096).await.unwrap();

        let spi = &flash.flash.device;
        assert_eq!(
            spi.sent(0x20),
            [&vec![0x20, 0x00, 0x50, 0x00], &vec![0x20, 0x00, 0x60, 0x00]]
        );
        assert!(spi.sent(0x21).is_empty());
    }

    #[tokio::test]
    async fn b24_program_sends_3_address_bytes() {
        let mut flash = flash(8 * MB);

        let mut range = flash.erase(0x5000.into(), 4096).await.unwrap();
        range
            .write(0x5000.into(), &[0xDE, 0xAD, 0xBE, 0xEF])
            .await
            .unwrap();

        let spi = &flash.flash.device;
        assert_eq!(
            spi.sent(0x02),
            [&vec![0x02, 0x00, 0x50, 0x00, 0xDE, 0xAD, 0xBE, 0xEF]]
        );
    }

    #[tokio::test]
    async fn b32_enters_4_byte_mode_and_sends_4_address_bytes() {
        let mut flash = flash(32 * MB);

        let mut buf = [0; 4];
        flash.read(0x0112_3456.into(), &mut buf).await.unwrap();
        flash.erase(0x0100_0000.into(), 4096).await.unwrap();

        let spi = &flash.flash.device;
        assert_eq!(spi.sent(0xB7).len(), 1, "enter 4-byte mode exactly once");
        assert_eq!(spi.sent(0x03), [&vec![0x03, 0x01, 0x12, 0x34, 0x56]]);
        assert_eq!(spi.sent(0x21), [&vec![0x21, 0x01, 0x00, 0x00, 0x00]]);
        assert!(spi.sent(0x20).is_empty());
    }

    #[tokio::test]
    async fn detect_type_recognizes_infineon() {
        let mut flash = flash(8 * MB);

        // JEDEC ID of the S25FL064L: manufacturer 0x01, density 2^0x17 = 8MB
        flash.flash.device.respond(&[0x01, 0x60, 0x17]);
        assert_eq!(flash.detect_type().await, Ok(Manufacturer::InfineonCypress));
    }
}

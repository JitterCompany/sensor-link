use cortex_m::interrupt;
use sensor_link_firmware::bootloader::AppFlashWriter;
use stm32ral::{self, flash, flash::SR, modify_reg, read_reg};

use super::address;

#[derive(Debug, Clone, Copy)]
pub enum EraseError {
    // given range is not correctly aligned to page boundaries (bug in linker script?)
    Misaligned,

    // requested range is not within bouds of the flash memory (as reported by the hardware)
    OutOfBounds,

    Failed,
}

pub struct Flash {
    peripheral: flash::Instance,
    stats_prog_chunks: u32,
}

impl Flash {
    /// Maximum erase page size. All erase operations must be aligned
    /// to a multiple of this size.
    ///
    /// (the actual flash page size may be smaller if configured for dual-bank mode)
    pub const ERASE_MAX_PAGE_SIZE: usize = 8 * 1024;

    pub fn new(peripheral: flash::Instance) -> Self {
        Self {
            peripheral,
            stats_prog_chunks: 0,
        }
    }

    /// Reset instruction + data caches
    fn reset_caches(&mut self) {
        let icache = read_reg!(flash, self.peripheral, ACR, ICEN == 1);
        let dcache = read_reg!(flash, self.peripheral, ACR, DCEN == 1);

        // Disable I/D caches
        modify_reg!(flash, self.peripheral, ACR, ICEN: 0, DCEN: 0);

        // Reset I/D caches
        modify_reg!(flash, self.peripheral, ACR, ICRST: 1, DCRST: 1);
        modify_reg!(flash, self.peripheral, ACR, ICRST: 0, DCRST: 0);

        // Restore I/D caches
        modify_reg!(flash, self.peripheral, ACR, ICEN: icache as u32, DCEN: dcache as u32);
    }

    /// (re-)Lock the flash. Prevents accidental write access
    fn lock(&mut self) {
        log::debug!("Programmed {} chunks", self.stats_prog_chunks);
        self.stats_prog_chunks = 0;

        // Clear PG if set by last programming action
        modify_reg!(flash, self.peripheral, CR, PG: 0);

        // Lock write/erase access
        modify_reg!(flash, self.peripheral, CR, LOCK: 1);

        // Reset caches (we may have erased/written data that invalidates them)
        self.reset_caches();
    }

    // Unlock the flash for erase/write access
    fn unlock(&mut self) {
        self.peripheral.KEYR.write(0x4567_0123);
        self.peripheral.KEYR.write(0xCDEF_89AB);

        while read_reg!(flash, self.peripheral, CR, LOCK == 1) {}
    }

    /// Read a Flash region as byte slice
    pub fn read<'a>(&'a self, region: address::Region) -> &'a [u8] {
        // The slice can be accessed as long as this driver (self) is borrowed.
        // While we currently don't need &self in the implementation, this guarantees
        // that the flash cannot be simultaneously erased/written (as that requires a &mut self)
        match region.ptr_at_byte_offset(0) {
            Ok(ptr) => unsafe { core::slice::from_raw_parts(ptr, region.length) },
            Err(error) => {
                log::error!("Attempt to read at invalid pointer: {error:?}");
                &[]
            }
        }
    }

    pub fn erase(&mut self, region: address::Region) -> Result<WriteableRegion<'_>, EraseError> {
        self.unlock();

        // Only regions with bounds on ERASE_MAX_PAGE_SIZE can be erased
        if !region.is_aligned_to(Self::ERASE_MAX_PAGE_SIZE) {
            return Err(EraseError::Misaligned);
        }
        if (region.length % Self::ERASE_MAX_PAGE_SIZE) != 0 {
            return Err(EraseError::Misaligned);
        }

        // Note: RM0432 Rev 9 3.3.5: flash erase/programming can only be done in VOS Range1
        // This is also the default so should be OK..

        let (n_banks, bank_size) = self.size_info();
        const MEGABYTE: usize = 1024 * 1024;

        // Bounds check: dual bank devices may have a hole in the memory map (bank_size < 1MiB)
        if n_banks > 1 {
            let hole_start = bank_size;
            let hole_end = MEGABYTE;
            if hole_end != hole_start {
                if region.flash_offset() < hole_end
                    && (region.flash_offset() + region.length) >= hole_start
                {
                    return Err(EraseError::OutOfBounds);
                }
            }
        }
        // Bounds check: end of flash
        if region.flash_offset() + region.length > bank_size * n_banks {
            return Err(EraseError::OutOfBounds);
        }

        // Erase all pages in the selected region
        let mut page_no_min = [u32::MAX, u32::MAX];
        let mut page_no_max = [u32::MIN, u32::MIN];
        let page_size = Self::ERASE_MAX_PAGE_SIZE / n_banks;
        for offset in (0..region.length).step_by(page_size) {
            let mut bank_id = 0;
            let mut page_offset = region.flash_offset() + offset;

            // Dual bank mode: offset past 1MiB is seconds bank (id 1)
            if n_banks > 1 && page_offset >= MEGABYTE {
                bank_id = 1;
                page_offset -= MEGABYTE;
            }
            let page_no = (page_offset / page_size) as u32;

            // Should not be possible. But let's check anyways as the impact
            // of overflow could be severe (erase bootloader would 'brick' device)
            if page_no > 255 {
                return Err(EraseError::OutOfBounds);
            }

            page_no_max[bank_id] = page_no_max[bank_id].max(page_no);
            page_no_min[bank_id] = page_no_min[bank_id].min(page_no);

            // RM0432 Rev 9 3.3.6
            // 1. wait untill not busy
            while read_reg!(flash, self.peripheral, SR, BSY == 1) {}

            interrupt::free(|_| {
                // 2. Check and clear all error programming flags due to a previous programming.
                modify_reg!(flash, self.peripheral, SR, FASTERR: 1, MISERR: 1, PGSERR: 1, SIZERR: 1, PGAERR: 1, WRPERR: 1, PROGERR: 1, OPERR: 1);

                // 3. set PERR, PNB=page, BKER=0
                modify_reg!(flash, self.peripheral, CR,
                    PG: 0,          // no programming
                    PER: 1,         // page erase,
                    MER1: 0,        // no mass erase
                    MER2: 0,        // no mass erase
                    PNB: page_no,
                    BKER: bank_id as u32,
                    START: 0,       // don't start yet
                    OPTSTRT: 0,     // don't modify options
                    FSTPG: 0,       // disable 'fast programming'
                    ERRIE: 1        // enable error flags
                );

                // 4. set START
                modify_reg!(flash, self.peripheral, CR, START: 1);

                // 5. wait untill not busy
                while read_reg!(flash, self.peripheral, SR, BSY == 1) {}

                // reset PERR=0
                modify_reg!(flash, self.peripheral, CR, PER: 0, PNB: 0, START: 0);
            });
        }

        let status = self.peripheral.SR.read();

        // check error flags
        if status
            & (SR::FASTERR::mask
                | SR::MISERR::mask
                | SR::PGSERR::mask
                | SR::WRPERR::mask
                | SR::PROGERR::mask
                | SR::OPERR::mask
                | SR::PGAERR::mask
                | SR::SIZERR::mask)
            != 0
        {
            return Err(EraseError::Failed);
        }

        Ok(WriteableRegion {
            flash: self,
            region,
            offset: 0,
            buffer: [0; Chunk::SIZE_BYTES],
            buffer_count: 0,
        })
    }

    /// Flash is split into 1 or 2 banks.
    /// The resulting size is reported as (n_banks, bytes_per_bank)
    fn size_info(&self) -> (usize, usize) {
        const FLASH_SIZE_PTR: *const u16 = 0x1FFF_75E0 as _;
        let total_kib = (unsafe { *FLASH_SIZE_PTR }) as usize;

        let is_dual_bank =
        // 1MB devices: bit 21
        if total_kib <= 1024 {
            (self.peripheral.OPTR.read() & (1 << 21)) != 0
        // 2MB devices: bit 22
        } else {
            (self.peripheral.OPTR.read() & (1 << 22)) != 0
        };
        let n_banks = 1 + is_dual_bank as usize;
        let bank_size = total_kib / (n_banks as usize) * 1024;

        (n_banks, bank_size)
    }

    fn program_chunk(
        &mut self,
        region: &address::Region,
        offset: usize,
        chunk: Chunk,
    ) -> Result<(), WriteError> {
        self.stats_prog_chunks += 1;
        // Validate and create a pointer to the correct flash address
        let flash_ptr = {
            if offset % Chunk::SIZE_BYTES != 0 {
                //Should not happen: bug in upper layer code?
                return Err(WriteError::Misaligned);
            }

            if !region.contains(offset, Chunk::SIZE_BYTES) {
                return Err(WriteError::OutOfBounds);
            }
            region
                .mut_ptr_at_byte_offset(offset)
                .map_err(|err| match err {
                    address::PointerError::OutOfBounds => WriteError::OutOfBounds,
                    address::PointerError::Unaligned => WriteError::Misaligned,
                })?
        };

        // RM0432 Rev 9 3.3.7
        // 1. wait untill not busy
        while read_reg!(flash, self.peripheral, SR, BSY == 1) {}

        // 2. Check and clear all error programming flags due to a previous programming.
        modify_reg!(flash, self.peripheral, SR, FASTERR: 1, MISERR: 1, PGSERR: 1, SIZERR: 1, PGAERR: 1, WRPERR: 1, PROGERR: 1, OPERR: 1);

        interrupt::free(|_| {
            // 3. set PG
            modify_reg!(flash, self.peripheral, CR,
                    PG: 1,      // programming
                    PER: 0,     // no page erase,
                    MER1: 0,    // no mass erase
                    MER2: 0,    // no mass erase
                    PNB: 0,     // page number not relevant(?)
                    BKER: 0,    // single-bank
                    START: 0,   // don't start yet
                    OPTSTRT: 0, // don't modify options
                    FSTPG: 0,   // disable 'fast programming'
                    ERRIE: 1,   // enable error flags
                    EOPIE: 1    // enable end-of-program flag
            );

            // 4. Write data (2 words, aligned at double word address)
            //for (i, word) in chunk.words.iter().enumerate() {
            unsafe {
                core::ptr::write_volatile(flash_ptr, chunk.words[0]);
                core::ptr::write_volatile(flash_ptr.add(1), chunk.words[1]);
            }
            //}

            // 5. wait untill not busy
            while read_reg!(flash, self.peripheral, SR, BSY == 1) {}
        });

        // 6. check + clear EOP flag
        while read_reg!(flash, self.peripheral, SR, EOP != 1) {
            // check for specific errors

            let status = read_reg!(flash, self.peripheral, SR);
            if status != 0 {
                return if status & SR::PGAERR::mask != 0 {
                    Err(WriteError::Misaligned)
                } else if status & SR::SIZERR::mask != 0 {
                    Err(WriteError::Size)
                } else if status & SR::WRPERR::mask != 0 {
                    Err(WriteError::Protected)
                } else {
                    log::error!("Programming failed: {status:08X?}");
                    // Generic error (there are more error flags we could translate into more specific errors)
                    Err(WriteError::Programming)
                };
            }
        }
        modify_reg!(flash, self.peripheral, SR, EOP: 0);

        // (7. clear PG is deferred to lock() (which is caled when dropping the WriteableRegion)

        Ok(())
    }
}

/// Represents a writeable (pre-erased) region of internal flash
pub struct WriteableRegion<'a> {
    flash: &'a mut Flash,
    region: address::Region,
    offset: usize,

    // buffer (in case multiple appends are not on Chunk::SIZE_BYTES boundaries)
    buffer: [u8; Chunk::SIZE_BYTES],
    buffer_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WriteError {
    OutOfBounds,
    Misaligned,
    Protected,
    Size,
    Programming,
}

impl<'a> WriteableRegion<'a> {
    /// Append data to this pre-erased region
    ///
    /// The WriteableRegion internally keeps track of the offset in the region
    /// On success, this offset (=amount of bytes to be written) is returned.
    ///
    /// Note to guarantee the (last few bytes of) data is written, call
    /// [finalize](method@Self::finalize()) after the last append().
    /// Alternatively, the write is auto-completed when this WriteableRegion
    /// is dropped.
    pub fn append(&mut self, data: &[u8]) -> Result<usize, WriteError> {
        let len = data.len();
        if len > (self.region.length - self.offset) {
            return Err(WriteError::OutOfBounds);
        }

        // Handle remainder from previous append (if any)
        let data = {
            let buff_count = self.buffer_count;

            // Remainder from last time? try to complete it
            if buff_count != 0 {
                let remaining = (Chunk::SIZE_BYTES - buff_count).min(len);
                self.buffer[buff_count..buff_count + remaining].copy_from_slice(&data[..remaining]);
                self.buffer_count += remaining;

                // Buffer not complete: nothing to do. Data will be saved on next append (or when WriteableRegion is dropped)
                if self.buffer_count != Chunk::SIZE_BYTES {
                    return Ok(self.offset);
                }

                // Program & reset buffer
                self.flash.program_chunk(
                    &self.region,
                    self.offset,
                    Chunk::from_slice(&self.buffer),
                )?;
                self.offset += Chunk::SIZE_BYTES;
                self.buffer_count = 0;
                self.buffer.fill(0xFF);

                &data[remaining..]
            } else {
                &data
            }
        };

        for chunk in data.chunks(Chunk::SIZE_BYTES) {
            let chunk_len = chunk.len();
            const INCOMPLETE: usize = Chunk::SIZE_BYTES - 1;
            match chunk_len {
                // Complete chunk: write
                Chunk::SIZE_BYTES => {
                    let chunk: &[u8; Chunk::SIZE_BYTES] = chunk.try_into().unwrap();

                    self.flash.program_chunk(
                        &self.region,
                        self.offset,
                        Chunk::from_slice(chunk),
                    )?;
                    self.offset += Chunk::SIZE_BYTES;
                }

                // Partial chunk: save for later (this is the last chunk)
                0..=INCOMPLETE => {
                    self.buffer.fill(0xFF);
                    self.buffer[0..chunk_len].copy_from_slice(chunk);
                    self.buffer_count = chunk_len;
                    break;
                }

                _ => unreachable!(),
            }
        }

        Ok(self.offset)
    }

    /// Finalize writing to this region
    ///
    /// Consumes the WriteableRegion so no further appends are possible.
    /// This also syncs the last internal buffers and returns the result
    /// (how many bytes were written or the write error, see [append](method@Self::append())).
    ///
    /// Note: this method is not strictly required. Simply dropping the WriteableRegion
    /// also syncs the buffers but you get no feedback if it failed or how many bytes were written
    pub fn finalize(mut self) -> Result<usize, WriteError> {
        self.finish_remainder()
    }

    fn finish_remainder(&mut self) -> Result<usize, WriteError> {
        let remainder = self.buffer_count;
        if remainder > 0 {
            log::debug!(
                "finish_remainder: {remainder} bytes: {:02X?} @{}",
                &self.buffer,
                self.offset
            );
            self.buffer_count = 0;
            let result = self.flash.program_chunk(
                &self.region,
                self.offset,
                Chunk::from_slice(&self.buffer),
            );
            log::debug!("finish_remainder: {result:?}");
            result?;
        }
        Ok(remainder)
    }
}

impl Drop for WriteableRegion<'_> {
    fn drop(&mut self) {
        if self.buffer_count > 0 {
            log::debug!(
                "dropping WriteableRegion: try sync last {} bytes..",
                self.buffer_count
            );
        }
        self.finish_remainder().ok();
        self.flash.lock();
    }
}

impl AppFlashWriter for WriteableRegion<'_> {
    type WriteError = WriteError;

    fn append(&mut self, data: &[u8]) -> Result<usize, Self::WriteError> {
        WriteableRegion::append(self, data)
    }

    fn finalize(self) -> Result<usize, Self::WriteError> {
        WriteableRegion::finalize(self)
    }
}

struct Chunk {
    words: [u32; 2],
}

impl Chunk {
    const SIZE_BYTES: usize = 8;

    pub fn from_slice(slice: &[u8; 8]) -> Self {
        Self {
            words: [
                u32::from_le_bytes(slice[..4].try_into().unwrap()),
                u32::from_le_bytes(slice[4..].try_into().unwrap()),
            ],
        }
    }
}

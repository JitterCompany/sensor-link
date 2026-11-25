//! Internal flash memory addresses as defined from the linker script
//!
//! Depending on the linker script, not all Regions may be available.
//! A region not being available is encoded by setting its length to zero.

use crate::linker;
use core::marker::PhantomData;

/// Represents a region of (internal) flash memory
#[derive(Clone)]
pub struct Region {
    address: u32,
    pub length: usize,

    _private: PhantomData<()>,
}

impl core::fmt::Debug for Region {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut suffix = "";
        let mut len = self.length;
        if len > 1024 && len % 1024 == 0 {
            len /= 1024;
            suffix = "Ki";
        }
        if len > 1024 && len % 1024 == 0 {
            len /= 1024;
            suffix = "Mi";
        }
        write!(f, "Flash @ 0x{:08X} ({:} {}B)", self.address, len, suffix)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum PointerError {
    OutOfBounds,
    Unaligned,
}

impl Region {
    pub fn flash_offset(&self) -> usize {
        (self.address.saturating_sub(total_address())) as usize
    }

    pub fn is_aligned_to(&self, alignment: usize) -> bool {
        (self.address % (alignment as u32)) == 0
    }

    fn address_at_byte_offset<T>(&self, byte_offset: usize) -> Result<u32, PointerError> {
        if byte_offset >= self.length {
            return Err(PointerError::OutOfBounds);
        }
        let addr = self.address + byte_offset as u32;
        let align = core::mem::align_of::<T>() as u32;
        if (addr % align) != 0 {
            return Err(PointerError::Unaligned);
        }

        Ok(addr)
    }

    pub fn mut_ptr_at_byte_offset<T>(&self, byte_offset: usize) -> Result<*mut T, PointerError> {
        self.address_at_byte_offset::<T>(byte_offset)
            .map(|addr| addr as *mut T)
    }

    pub fn ptr_at_byte_offset<T>(&self, byte_offset: usize) -> Result<*const T, PointerError> {
        self.address_at_byte_offset::<T>(byte_offset)
            .map(|addr| addr as *const T)
    }

    // Does the range defined by the given offset + length fit within this region?
    pub fn contains(&self, offset: usize, length: usize) -> bool {
        offset < self.length && length <= (self.length - offset)
    }

    fn new(address: u32, length: usize) -> Option<Self> {
        if address == 0 || address < total_address() || length == 0 {
            None
        } else {
            Some(Self {
                address,
                length,
                _private: PhantomData,
            })
        }
    }
}

fn total_address() -> u32 {
    unsafe { linker::symbol(&linker::__FLASH_TOTAL_START) }
}

/// Region covering all the flash memory on the microcontroller
pub fn total() -> Region {
    Region::new(total_address(), unsafe {
        linker::symbol(&linker::__FLASH_TOTAL_LENGTH) as usize
    })
    .expect("Linker script broken")
}

/// Flash memory region where the bootloader application is stored
///
/// This region is not available in all linker scripts.
/// If not available, None is returned.
pub fn bootloader() -> Option<Region> {
    unsafe {
        Region::new(
            linker::symbol(&linker::__FLASH_BOOTLOADER_START),
            linker::symbol(&linker::__FLASH_BOOTLOADER_LENGTH) as usize,
        )
    }
}

/// Flash memory region where the bootloader metadata is stored
///
/// If a bootloader is present, this section will contain metadata about the bootloader.
/// If not available, None is returned.
pub fn bootloader_meta() -> Option<Region> {
    unsafe {
        Region::new(
            linker::symbol(&linker::__FLASH_BOOTLOADER_META_START),
            linker::symbol(&linker::__FLASH_BOOTLOADER_META_LENGTH) as usize,
        )
    }
}

/// Flash memory region for config (configured at deploy)
pub fn config() -> Region {
    unsafe {
        Region::new(
            linker::symbol(&linker::__FLASH_CFG_START),
            linker::symbol(&linker::__FLASH_CFG_LENGTH) as usize,
        )
        .expect("Linker script broken")
    }
}

/// Application (code + metadata) is stored in this region
///
/// This region is not available in all linker scripts.
/// If not available, None is returned.
pub fn application_total() -> Option<Region> {
    unsafe {
        Region::new(
            linker::symbol(&linker::__FLASH_APP_TOTAL_START),
            linker::symbol(&linker::__FLASH_APP_TOTAL_LENGTH) as usize,
        )
    }
}

/// Application code (excl metadata) is stored in this region
///
/// This region is not available in all linker scripts.
/// If not available, None is returned.
pub fn application_code() -> Option<Region> {
    unsafe {
        Region::new(
            linker::symbol(&linker::__FLASH_APP_CODE_START),
            linker::symbol(&linker::__FLASH_APP_CODE_LENGTH) as usize,
        )
    }
}

/// Application metadata is stored in this region
///
/// This region is not available in all linker scripts.
/// If not available, an empty region is returned.
pub fn application_metadata() -> Option<Region> {
    unsafe {
        Region::new(
            linker::symbol(&linker::__FLASH_APP_META_START),
            linker::symbol(&linker::__FLASH_APP_META_LENGTH) as usize,
        )
    }
}

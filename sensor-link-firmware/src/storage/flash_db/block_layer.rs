//! # Block layer: low-level flash abstraction based on the concept of blocks + fragments
//!
//! All flash is divided into blocks which each contain one or more data fragments.
//!
//! After formatting a block, each of its data fragments can be accessed for read/write/delete.
//! An important limitation is that each fragment can only be written once. Writing it again is only
//! possible by re-formatting the containing block (this is because blocks directly map to underlying flash erase blocks).
//! Fragments can however be be marked 'deleted' which does not require erasing the block.
//!
//! Each block contains a small metadata header to support atomic block format / fragment write / fragment delete.
//! This means that a block will fit slightly less segments than you would expect.

use embedded_storage::nor_flash::NorFlashError;
use embedded_storage_async::nor_flash::{NorFlash, ReadNorFlash};

#[derive(Debug, Clone, Copy, PartialEq)]
struct BlockValid(bool);

/// Block identifier: each block has a unique id 0..N (where N is the flash capacity)
pub type BlockId = u32;

/// Each erase block begins with this header
///
/// Block structure:
//
// - Blockheader [BLOCK_HEADER_SIZE]
// - FragmentHeaders [fragment_count() * FRAG_HEADER_SIZE]
// - FragmentData [fragment_count() * fragment_size()]
// - (remaining data until end of block is unused)

#[derive(Debug, Clone)]
pub struct BlockHeader {
    /// object_id this block belongs to
    object_id: u16,

    /// fragment size (also defines the amount of fragments in the block)
    /// while this info is constant per object, storing it makes the block_layer
    /// implementation simpler as it doesn't have to know about the objects
    frag_size: u16,

    /// Sequence number of the first fragment in block.
    first_frag_no: u32,

    /// Version number of the object this block belongs to
    object_version: u8,

    /// unused. reserved for future compatibility
    _rfu: [u8; 3],

    /// block id: identifies which block on flash this header
    /// belongs to (not explicitly stored)
    id: u32,

    /// How many fragments in this block
    /// (not explicitly stored, calculated from)
    frag_count: usize,
}

fn parse_u16(slice_assumed_2bytes: &[u8]) -> u16 {
    u16::from_le_bytes(slice_assumed_2bytes.try_into().unwrap())
}
fn parse_u32(slice_assumed_4bytes: &[u8]) -> u32 {
    u32::from_le_bytes(slice_assumed_4bytes.try_into().unwrap())
}

impl BlockValid {
    fn from_bytes(bytes: [u8; 4]) -> Self {
        Self(match bytes {
            // '900D' marker, not followed by 'BAAD': valid
            [0x90, 0x0D, 0xFF, 0xFF] => true,

            // anything else: invalid
            _ => false,
        })
    }

    // Serialize to two-bytes that must be written
    // at offset=0 or offset=2 in a 4-byte field
    fn as_bytes(&self) -> (usize, [u8; 2]) {
        match self.0 {
            // valid: write 'good' at offset 0
            true => (0, [0x90, 0x0D]),

            // invalid: write 'baad' at offset 2
            false => (2, [0xBA, 0xAD]),
        }
    }
}

const BLOCK_HEADER_SIZE: usize = 16;
const FRAG_HEADER_SIZE: usize = 3;

/// Calculate how many fragments would fit in a block with given block size and fragment size
///
/// Utility function, handy for calculation/planning when a block is not formatted yet.
/// For valid blocks, you can also use [BlockHeader::fragment_count()](method@BlockHeader::fragment_count)
pub fn calculate_fragments_per_block(block_size: usize, fragment_size: usize) -> usize {
    (block_size - BLOCK_HEADER_SIZE) / (fragment_size + FRAG_HEADER_SIZE)
}

impl BlockHeader {
    /// Object the block belongs to
    pub fn object_id(&self) -> u16 {
        self.object_id
    }

    /// Version of the object this block belongs to
    /// Mismatch indicates the block may be obsolete
    pub fn object_version(&self) -> u8 {
        self.object_version
    }

    /// Block index
    pub fn block_id(&self) -> u32 {
        self.id
    }

    /// First fragment stored in this block
    ///
    /// Together with [fragment_count()](method@Self::fragment_count()) this gives
    /// the range of fragments that may be stored in this block
    pub fn first_fragment(&self) -> u32 {
        self.first_frag_no
    }

    /// Last fragment stored in this block
    pub fn last_fragment(&self) -> u32 {
        let offset = self.fragment_count().saturating_sub(1) as u32;
        self.first_frag_no.wrapping_add(offset)
    }

    /// Size of each fragment in this block
    pub fn fragment_size(&self) -> usize {
        self.frag_size as usize
    }

    /// Count of how many fragments fit in this block
    ///
    /// (multiply with [fragment_size()](method@Self::fragment_size) to find the total amount of data that can be stored in the block)
    pub fn fragment_count(&self) -> usize {
        self.frag_count
    }

    /// Try to create a BlockHeader. Fails if the given frag_size won't fit in the block
    fn try_new<const BLOCK_SIZE: usize>(
        block_id: BlockId,
        object_id: u16,
        object_version: u8,
        frag_size: u16,
        first_frag_no: u32,
    ) -> Result<Self, ()> {
        let frag_count = calculate_fragments_per_block(BLOCK_SIZE, frag_size as usize);

        // If no fragments fit in the block, it is invalid
        if frag_count == 0 {
            Err(())
        } else {
            Ok(Self {
                object_id,
                frag_size,
                first_frag_no,
                object_version,
                _rfu: [0xFF, 0xFF, 0xFF],
                id: block_id,
                frag_count,
            })
        }
    }

    /// Deserialize Block header from the binary block header (excl fragment headers)
    fn from_bytes<const BLOCK_SIZE: usize>(
        id: u32,
        bytes: [u8; BLOCK_HEADER_SIZE],
    ) -> Option<Self> {
        let first_frag_no = parse_u32(&bytes[8..12]);
        let object_version = bytes[12];
        let _rfu = [bytes[13], bytes[14], bytes[15]];

        let valid = BlockValid::from_bytes(bytes[0..4].try_into().unwrap());
        if valid.0 {
            BlockHeader::try_new::<BLOCK_SIZE>(
                id,
                parse_u16(&bytes[4..6]),
                object_version,
                parse_u16(&bytes[6..8]),
                first_frag_no,
            )
            .ok()
        } else {
            None
        }
    }

    fn as_bytes(&self) -> [u8; 16] {
        let mut result = [0xFF; 16];
        // Note: valid marker is not serialized by default. that is written separately
        // to make writing the header atomic.
        result[4..6].copy_from_slice(&self.object_id.to_le_bytes());
        result[6..8].copy_from_slice(&self.frag_size.to_le_bytes());
        result[8..12].copy_from_slice(&self.first_frag_no.to_le_bytes());
        result[12] = self.object_version;

        // RFU: write to 0xFF
        result[13] = 0xFF;
        result[14] = 0xFF;
        result[15] = 0xFF;
        result
    }

    /// total size of the block header including fragment headers
    ///
    /// This is the offset in the block where the data fragments begin.
    ///
    /// *Note: this is not necessarily the only block overhead. Do not use to calculate total amount of data to be stored.*
    fn headers_total_size(&self) -> usize {
        BLOCK_HEADER_SIZE + (self.fragment_count() * FRAG_HEADER_SIZE)
    }
}

/// Each erase block has n=erase_size/frag_size fragment headers
///
/// Each byte in the header is only written once. Together they can
/// be summarized into a [FragmentStatus]
pub type FragmentHeader = [u8; FRAG_HEADER_SIZE];

/// Status of a fragment
///
/// This metadata is available for each fragment in a block
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum FragmentStatus {
    Empty = 0xFF,
    Invalid = 0xD1,
    Valid = 0xDC,
    Obsolete = 0x55,
}

impl FragmentStatus {
    pub fn as_byte(self) -> Option<(usize, u8)> {
        match self {
            FragmentStatus::Empty => None,
            FragmentStatus::Invalid => Some((0, self as u8)),
            FragmentStatus::Valid => Some((1, self as u8)),
            FragmentStatus::Obsolete => Some((2, self as u8)),
        }
    }
}

impl From<FragmentHeader> for FragmentStatus {
    fn from(header: FragmentHeader) -> Self {
        match header {
            [0xFF, 0xFF, 0xFF] => FragmentStatus::Empty,
            [0xD1, 0xDC, 0xFF] => FragmentStatus::Valid,
            [_, _, 0x55] => FragmentStatus::Obsolete,
            _ => FragmentStatus::Invalid,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct BoundsError;

/// Error type returned by read-only block API
#[derive(Debug, Clone, Copy)]
pub enum FlashError<E: NorFlashError> {
    /// tried to access a block outside the bounds of the flash
    BlockOutOfBounds,

    /// Flash-level error from the NorFlash driver
    Flash(E),
}

/// Error type returned when reading fragments fails
#[derive(Debug, Clone, Copy)]
pub enum FlashReadError<E: NorFlashError> {
    /// Read logic error: see [FlashReadErrorKind] for the details
    Read(FlashReadErrorKind),

    /// Flash-level error from the NorFlash driver
    Flash(E),
}

impl<E: NorFlashError> FlashReadError<E> {
    pub fn kind(&self) -> FlashReadErrorKind {
        match self {
            FlashReadError::Read(err) => err.clone(),
            FlashReadError::Flash(_) => FlashReadErrorKind::Flash,
        }
    }
}

/// All possible reasons why the FlashReadError occurred
///
/// Implements PartialEq so can be easily compared
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FlashReadErrorKind {
    FragmentEmpty,
    FragmentInvalid,
    FragmentObsolete,
    FragmentNotFound,
    BlockInvalid,
    BlockOutOfBounds,
    Flash,
}

/// Error type returned when writing fails
#[derive(Debug, Clone, Copy)]
pub enum FlashWriteError<E: NorFlashError> {
    /// Read logic error: see [FlashReadErrorKind] for the details
    Write(FlashWriteErrorKind),

    /// Flash-level error from the NorFlash driver
    Flash(E),
}

/// All possible reasons why the FlashWriteError occurred
///
/// Implements PartialEq so can be easily compared
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FlashWriteErrorKind {
    /// Fragment not found in block
    FragmentNotFound,

    /// Fragment cannot be written: block itself is not valid
    BlockInvalid,

    /// Fragment cannot be written: already marked as invalid
    FragmentInvalid,

    /// Fragment cannot be written: already marked as obsolete
    FragmentObsolete,

    /// Fragment cannot be written: already marked as written
    FragmentExists,

    /// Cannot write this data: too large for the fragment
    FragmentTooLarge,

    /// Write failed to complete: readback failed
    VerifyFailed,

    /// Block address is out-of-bounds (does not fit in flash)
    BlockOutOfBounds,

    /// Error in the flash driver
    Flash,
}

impl<E: NorFlashError> FlashWriteError<E> {
    pub fn kind(&self) -> FlashWriteErrorKind {
        match self {
            FlashWriteError::Write(err) => err.clone(),
            FlashWriteError::Flash(_) => FlashWriteErrorKind::Flash,
        }
    }
}

impl<E: NorFlashError> From<BoundsError> for FlashError<E> {
    fn from(_: BoundsError) -> Self {
        FlashError::BlockOutOfBounds
    }
}
impl<E: NorFlashError> From<BoundsError> for FlashReadError<E> {
    fn from(_: BoundsError) -> Self {
        FlashReadError::Read(FlashReadErrorKind::BlockOutOfBounds)
    }
}
impl<E: NorFlashError> From<BoundsError> for FlashWriteError<E> {
    fn from(_: BoundsError) -> Self {
        FlashWriteError::Write(FlashWriteErrorKind::BlockOutOfBounds)
    }
}

impl<E: NorFlashError> From<E> for FlashError<E> {
    fn from(err: E) -> Self {
        FlashError::Flash(err)
    }
}
impl<E: NorFlashError> From<E> for FlashReadError<E> {
    fn from(err: E) -> Self {
        FlashReadError::Flash(err)
    }
}
impl<E: NorFlashError> From<E> for FlashWriteError<E> {
    fn from(err: E) -> Self {
        FlashWriteError::Flash(err)
    }
}

/// Read-only part of the block-layer API
pub trait Reader<const BLOCK_SIZE: usize> {
    type Error: NorFlashError;

    /// Read (part of) a fragment from a block, starting at an offset relative to the start-of-fragment
    ///
    /// A block header can be obtained via [block_header()](method@Self::block_header).
    /// If the block is valid and contains the requested fragment, the requested part of the fragment is written to the result buffer.
    /// The result buffer may be smaller or larger than the stored fragment.
    /// On success, the actual amount of bytes read is returned.
    async fn read_fragment_at_offset(
        &mut self,
        block: &BlockHeader,
        frag_no: u32,
        offset: usize,
        result: &mut [u8],
    ) -> Result<usize, FlashReadError<Self::Error>>;

    /// Read a fragment from a block
    ///
    /// A block header can be obtained via [block_header()](method@Self::block_header).
    /// If the block is valid and contains the requested fragment, it is written to the result buffer.
    /// The result buffer may be smaller or larger than the stored fragment.
    /// On success, the actual amount of bytes read is returned.
    #[inline]
    async fn read_fragment(
        &mut self,
        block: &BlockHeader,
        frag_no: u32,
        result: &mut [u8],
    ) -> Result<usize, FlashReadError<Self::Error>> {
        self.read_fragment_at_offset(block, frag_no, 0, result)
            .await
    }

    /// Verify a fragment against expected data
    ///
    /// Intended use: check if data is already up-to-date before attempting a write.
    /// Note: no need to call this after writing, as the write methods already perform
    /// a readback verification internally!
    async fn verify_fragment(
        &mut self,
        block: &BlockHeader,
        frag_no: u32,
        expected_data: &[u8],
    ) -> Result<bool, FlashWriteError<Self::Error>>;

    /// Read the status of a fragment
    async fn fragment_status(
        &mut self,
        block: &BlockHeader,
        frag_no: u32,
    ) -> Result<FragmentStatus, FlashReadError<Self::Error>>;

    /// Read the block header for the given block
    ///
    /// The header includes info about the validity and which segments it may contain.
    /// None is returned if the block is invalid (=needs to be formatted)
    async fn block_header(
        &mut self,
        block_id: BlockId,
    ) -> Result<Option<BlockHeader>, FlashError<Self::Error>>;

    /// check if all fragments in the block are empty
    ///
    /// Returns true if the block is valid & only contains empty fragments
    async fn block_is_empty(
        &mut self,
        block: &BlockHeader,
    ) -> Result<bool, FlashReadError<Self::Error>>;

    /// Check if any of the fragments in this block is valid (contains data)
    ///
    /// Returns the frag_no of the first valid fragment found, or None if no data in this block
    async fn block_find_valid_fragment(
        &mut self,
        block: &BlockHeader,
    ) -> Result<Option<u32>, FlashReadError<Self::Error>>;

    /// Check the validity of the given block
    ///
    /// This is an optimized shortcut for [read_fragment()](method@Self::read_fragment).is_some()
    /// (only reads a partial [BlockHeader] from flash)
    async fn block_is_valid(&mut self, block_id: BlockId) -> Result<bool, FlashError<Self::Error>>;
}

/// Writeable part of the block-layer API
pub trait Writer<const BLOCK_SIZE: usize>: Reader<BLOCK_SIZE> {
    /// Fully erases the block: all previous data stored on this block will be gone.
    /// If succesful, the block is formatted and ready to store fragments for the specified object
    ///
    /// The block must always be in exactly one of three states,
    /// even if erase is interrupted by e.g. reboot/power cycle:
    /// 1. as it was before starting the format (the format was effectively never applied)
    /// 2. block reads as invalid (the erase was aborted, block needs to be re-erased)
    /// 3. block reads as valid (formatting complete, block ready for writes)
    async fn format_block(
        &mut self,
        block_id: BlockId,
        object_id: u16,
        object_version: u8,
        frag_size: u16,
        first_frag_no: u32,
    ) -> Result<(), FlashWriteError<Self::Error>>;

    /// Try to write a fragment to the block
    ///
    /// Before writing the first fragment, the block must be formatted (see [format_block()](method@Self::format_block)).
    /// Each fragment can only be written once! After one write, the fragment can only be read or deleted.
    /// To re-write a fragment, the full block must be re-formatted.
    ///
    /// Each fragment must always be in exactly one of three states,
    /// even if erase is interrupted by e.g. reboot/power cycle:
    /// 1. as it was before starting the write (the write was effectively never applied)
    /// 2. block reads as invalid (write aborted: block needs to be re-formatted and re-written)
    /// 3. block reads as valid (write complete: fragment is stored + verified. fragment ready for reads)
    async fn write_fragment(
        &mut self,
        block: &BlockHeader,
        frag_no: u32,
        data: &[u8],
    ) -> Result<(), FlashWriteError<Self::Error>>;

    /// Try to delete a fragment from the block by marking it as obsolete
    ///
    /// If the fragment is already invalid or obsolete, no write is performed but this is not considered an error.
    /// Each fragment must always be in exactly one of two states,
    /// even if erase is interrupted by e.g. reboot/power cycle:
    /// 1. as it was before starting the delete (the delete effectively never happened)
    /// 2. block reads as invalid/obsolete (delete complete)
    ///
    /// *Note: this is a soft-delete: the original data could still be recoverable from flash untill
    /// the block is formatted (erased).*
    async fn delete_fragment(
        &mut self,
        block: &BlockHeader,
        frag_no: u32,
    ) -> Result<(), FlashWriteError<Self::Error>>;
}

/// Represents the storage device as a collection of blocks.
///
/// Implements [Reader] and [Writer] (if initialized as writeable)
///
/// To create a read-only instance, see [readonly_from()](method@Self::readonly_from).
/// To create a read/write instance, see [writeable_from()](method@Self::writeable_from).
///
/// Each block has a [BlockHeader] which defines the data fragments stored in it.
///
pub struct BlockDevice<FLASH, const BLOCK_SIZE: usize> {
    flash: FLASH,
    blocks_total: usize,
}

impl<F: NorFlash, const B: usize> BlockDevice<F, B> {
    /// Try to create a writeable Blockdevice. Can only fail if the given flash driver
    /// has an erase size that does not match B.
    /// (if `generic_const_exprs` becomes available this can be simplified)
    pub fn writeable_from(flash: F) -> Result<Self, ()> {
        if B != F::ERASE_SIZE {
            Err(())
        } else {
            let blocks_total = flash.capacity() / B;
            Ok(Self {
                flash,
                blocks_total,
            })
        }
    }

    /// Read fragment status, converting errors into appropriate write errors
    async fn fragment_status_pre_write(
        &mut self,
        block: &BlockHeader,
        frag_no: u32,
    ) -> Result<FragmentStatus, FlashWriteError<F::Error>> {
        match self.try_get_fragment_status(block, frag_no).await {
            Ok(Some(status)) => Ok(status),
            Ok(None) => Err(FlashWriteError::Write(
                FlashWriteErrorKind::FragmentNotFound,
            )),
            Err(error) => match error {
                FlashError::BlockOutOfBounds => Err(FlashWriteError::Write(
                    FlashWriteErrorKind::BlockOutOfBounds,
                )),
                FlashError::Flash(flash_err) => Err(FlashWriteError::Flash(flash_err)),
            },
        }
    }
}

impl<F: ReadNorFlash, const B: usize> BlockDevice<F, B> {
    /// Create a readonly Blockdevice.
    ///
    /// **NOTE: the erase size must match B**.
    /// (the ReadNorFlash trait does not expose the erase size)
    /// If using the wrong erase size, the blockdevice will most likely
    /// not recognize any valid blocks but depending on the data it may
    /// yield invalid data
    pub fn readonly_from(flash: F) -> Self {
        let blocks_total = flash.capacity() / B;
        Self {
            flash,
            blocks_total,
        }
    }

    /// Total amount of blocks on the device
    ///
    /// For example, 10 blocks means that the maximum `block_id` is 9
    pub fn blocks_total(&self) -> usize {
        self.blocks_total
    }

    /// Flash address of the given block (if in bounds)
    fn block_addr(&self, block_id: BlockId) -> Result<u32, BoundsError> {
        if block_id > self.blocks_total as u32 {
            Err(BoundsError)
        } else {
            Ok(block_id * B as u32)
        }
    }

    /// Try to get fragment status. Return None if fragment not in this block
    async fn try_get_fragment_status(
        &mut self,
        block: &BlockHeader,
        frag_no: u32,
    ) -> Result<Option<FragmentStatus>, FlashError<F::Error>> {
        // Find frag_no relative to first in block
        let rel_frag_no = frag_no.wrapping_sub(block.first_frag_no) as usize;
        if rel_frag_no >= block.fragment_count() {
            return Ok(None);
        }

        let block_addr = self.block_addr(block.id)?;

        // Block structure:
        //
        // - Blockheader [BLOCK_HEADER_SIZE]
        // - FragmentHeaders [fragment_count() * FRAG_HEADER_SIZE]
        // - FragmentData [fragment_count() * fragment_size()]
        let header_offset = BLOCK_HEADER_SIZE + (FRAG_HEADER_SIZE * rel_frag_no);
        let mut buffer: FragmentHeader = [0; FRAG_HEADER_SIZE];
        self.flash
            .read(block_addr + header_offset as u32, &mut buffer)
            .await?;
        Ok(Some(buffer.into()))
    }

    /// Read raw fragment contents
    ///
    /// This does not guarantee the fragment actually contains useful data. Please see [fragment_status()](method@Self::fragment_status)
    async fn read_raw_fragment(
        &mut self,
        block: &BlockHeader,
        frag_no: u32,
        offset: usize,
        result: &mut [u8],
    ) -> Result<usize, FlashReadError<F::Error>> {
        // Find frag_no relative to first in block
        let rel_frag_no = frag_no.wrapping_sub(block.first_frag_no) as usize;
        if rel_frag_no >= block.fragment_count() {
            return Err(FlashReadError::Read(FlashReadErrorKind::FragmentNotFound));
        }

        // cap result at fragment size
        let bytes_till_end_of_frag = block.fragment_size().saturating_sub(offset);
        let result_len = result.len().min(bytes_till_end_of_frag);
        let result = &mut result[..result_len];

        let data_offset = block.headers_total_size() + rel_frag_no * block.fragment_size() + offset;
        self.flash
            .read(self.block_addr(block.id)? + data_offset as u32, result)
            .await?;

        Ok(result.len())
    }
}

impl<F: ReadNorFlash, const B: usize> Reader<B> for BlockDevice<F, B> {
    type Error = F::Error;

    async fn read_fragment_at_offset(
        &mut self,
        block: &BlockHeader,
        frag_no: u32,
        offset: usize,
        result: &mut [u8],
    ) -> Result<usize, FlashReadError<Self::Error>> {
        match self.fragment_status(block, frag_no).await? {
            FragmentStatus::Empty => Err(FlashReadError::Read(FlashReadErrorKind::FragmentEmpty)),
            FragmentStatus::Invalid => {
                Err(FlashReadError::Read(FlashReadErrorKind::FragmentInvalid))
            }
            FragmentStatus::Obsolete => {
                Err(FlashReadError::Read(FlashReadErrorKind::FragmentObsolete))
            }
            FragmentStatus::Valid => self.read_raw_fragment(block, frag_no, offset, result).await,
        }
    }

    async fn verify_fragment(
        &mut self,
        block: &BlockHeader,
        frag_no: u32,
        expected_data: &[u8],
    ) -> Result<bool, FlashWriteError<F::Error>> {
        const CHUNK_SIZE: usize = 128;

        for (chunk_no, expected) in expected_data.chunks(CHUNK_SIZE).enumerate() {
            let offset = chunk_no * CHUNK_SIZE;
            let mut readback = [0_u8; CHUNK_SIZE];
            let readback_slice = &mut readback[0..expected.len()];
            let bytes_read = self
                .read_raw_fragment(block, frag_no, offset, readback_slice)
                .await
                .map_err(|r_err| match r_err {
                    FlashReadError::Read(_) => {
                        FlashWriteError::Write(FlashWriteErrorKind::VerifyFailed)
                    }
                    FlashReadError::Flash(flash) => FlashWriteError::Flash(flash),
                })?;
            if &readback_slice[..bytes_read] != expected {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn block_header(
        &mut self,
        block_id: BlockId,
    ) -> Result<Option<BlockHeader>, FlashError<Self::Error>> {
        if block_id > self.blocks_total as u32 {
            return Err(FlashError::BlockOutOfBounds);
        }

        // Read block header (excl fragments)
        let offset = block_id * B as u32;
        let mut buffer = [0; BLOCK_HEADER_SIZE];
        self.flash.read(offset, &mut buffer).await?;
        Ok(BlockHeader::from_bytes::<B>(block_id, buffer))
    }

    async fn block_is_empty(
        &mut self,
        block: &BlockHeader,
    ) -> Result<bool, FlashReadError<Self::Error>> {
        let frag_count = block.fragment_count();

        // Note: a potential optimization in case of many fragments:
        // read multiple fragment headers in one transaction instead of one 3-byte header at a time.
        // For now this is probably not worth the extra complexity
        let first_frag = block.first_fragment();
        for rel_frag in 0..frag_count {
            if FragmentStatus::Empty
                != self
                    .fragment_status(&block, first_frag + rel_frag as u32)
                    .await?
            {
                // non-empty fragment found: block empty
                return Ok(false);
            }
        }

        // All fragments empty: block empty
        Ok(true)
    }

    async fn block_find_valid_fragment(
        &mut self,
        block: &BlockHeader,
    ) -> Result<Option<u32>, FlashReadError<Self::Error>> {
        let frag_count = block.fragment_count();

        // Note: a potential optimization in case of many fragments:
        // read multiple fragment headers in one transaction instead of one 3-byte header at a time.
        // For now this is probably not worth the extra complexity
        let first_frag = block.first_fragment();
        for rel_frag in 0..frag_count {
            let frag_no = first_frag + rel_frag as u32;

            if FragmentStatus::Valid == self.fragment_status(&block, frag_no).await? {
                // valid fragment found
                return Ok(Some(frag_no));
            }
        }
        Ok(None)
    }

    async fn block_is_valid(&mut self, block_id: BlockId) -> Result<bool, FlashError<Self::Error>> {
        if block_id > self.blocks_total as u32 {
            return Err(FlashError::BlockOutOfBounds);
        }

        let offset = block_id * B as u32;
        let mut buffer = [0; 4];
        self.flash.read(offset, &mut buffer).await?;
        Ok(BlockValid::from_bytes(buffer).0)
    }

    async fn fragment_status(
        &mut self,
        block: &BlockHeader,
        frag_no: u32,
    ) -> Result<FragmentStatus, FlashReadError<Self::Error>> {
        match self.try_get_fragment_status(block, frag_no).await {
            Ok(Some(status)) => Ok(status),
            Ok(None) => Err(FlashReadError::Read(FlashReadErrorKind::FragmentNotFound)),
            Err(error) => match error {
                FlashError::BlockOutOfBounds => {
                    Err(FlashReadError::Read(FlashReadErrorKind::BlockOutOfBounds))
                }
                FlashError::Flash(flash_err) => Err(FlashReadError::Flash(flash_err)),
            },
        }
    }
}

impl<F: NorFlash, const B: usize> Writer<B> for BlockDevice<F, B> {
    async fn format_block(
        &mut self,
        block_id: BlockId,
        object_id: u16,
        object_version: u8,
        frag_size: u16,
        first_frag_no: u32,
    ) -> Result<(), FlashWriteError<Self::Error>> {
        let block_addr = self.block_addr(block_id)?;

        // 1. begin transaction: try to mark page as invalid.
        // If this fails, the error is ignored on purpose: if the block was never previously erased writing might
        // not be possible)
        let (offset, marker) = BlockValid(false).as_bytes();
        self.flash
            .write(block_addr + offset as u32, &marker)
            .await
            .ok();

        // 2. perform erase. (If interrupted it is highly unlikely to read as valid due to step 1)
        self.flash.erase(block_addr, block_addr + B as u32).await?;

        // 3. erase is finished: write header (exlc first 4 'valid marker' bytes)
        let header = BlockHeader::try_new::<{ B }>(
            block_id,
            object_id,
            object_version,
            frag_size,
            first_frag_no,
        )
        .map_err(|_| FlashWriteError::Write(FlashWriteErrorKind::FragmentTooLarge))?;
        self.flash
            .write(block_addr + 4, &header.as_bytes()[4..])
            .await?;

        // 4. finalize: mark page as valid
        let (offset, marker) = BlockValid(true).as_bytes();
        self.flash
            .write(block_addr + offset as u32, &marker)
            .await?;
        Ok(())
    }

    async fn write_fragment(
        &mut self,
        block: &BlockHeader,
        frag_no: u32,
        data: &[u8],
    ) -> Result<(), FlashWriteError<Self::Error>> {
        if data.len() > block.fragment_size() {
            return Err(FlashWriteError::Write(
                FlashWriteErrorKind::FragmentTooLarge,
            ));
        }

        // Calculate block flash address
        let block_addr = self.block_addr(block.id)?;

        // Calculate fragment relative to start of block
        let rel_frag_no = frag_no.wrapping_sub(block.first_frag_no) as usize;
        if rel_frag_no >= block.fragment_count() {
            return Err(FlashWriteError::Write(
                FlashWriteErrorKind::FragmentNotFound,
            ));
        }

        match self.fragment_status_pre_write(block, frag_no).await? {
            FragmentStatus::Invalid => {
                Err(FlashWriteError::Write(FlashWriteErrorKind::FragmentInvalid))
            }
            FragmentStatus::Obsolete => Err(FlashWriteError::Write(
                FlashWriteErrorKind::FragmentObsolete,
            )),
            FragmentStatus::Valid => {
                Err(FlashWriteError::Write(FlashWriteErrorKind::FragmentExists))
            }
            FragmentStatus::Empty => {
                // 1. mark fragment as incomplete (write-in-progress)
                let frag_header_addr =
                    block_addr + (BLOCK_HEADER_SIZE + (FRAG_HEADER_SIZE * rel_frag_no)) as u32;
                if let Some((marker_offset, marker)) = FragmentStatus::Invalid.as_byte() {
                    self.flash
                        .write(frag_header_addr + marker_offset as u32, &[marker])
                        .await?;
                }

                // 2. write data to segment
                let data_offset = block.headers_total_size() + rel_frag_no * block.fragment_size();
                self.flash
                    .write(block_addr + data_offset as u32, data)
                    .await?;

                // 3. readback data and verify
                if !self.verify_fragment(block, frag_no, data).await? {
                    return Err(FlashWriteError::Write(FlashWriteErrorKind::VerifyFailed));
                }

                // 4. mark fragment as valid (write complete)
                if let Some((marker_offset, marker)) = FragmentStatus::Valid.as_byte() {
                    self.flash
                        .write(frag_header_addr + marker_offset as u32, &[marker])
                        .await?;
                }

                Ok(())
            }
        }
    }

    async fn delete_fragment(
        &mut self,
        block: &BlockHeader,
        frag_no: u32,
    ) -> Result<(), FlashWriteError<Self::Error>> {
        // Calculate block flash address
        let block_addr = self.block_addr(block.id)?;

        // Calculate fragment relative to start of block
        let rel_frag_no = frag_no.wrapping_sub(block.first_frag_no) as usize;
        if rel_frag_no >= block.fragment_count() {
            return Err(FlashWriteError::Write(
                FlashWriteErrorKind::FragmentNotFound,
            ));
        }

        match self.fragment_status_pre_write(block, frag_no).await? {
            // Already invalid: no point in also marking it erased
            // (not guaranteed to be even possible if the obsolete marker was already written with invalid value)
            FragmentStatus::Invalid => {}

            // Already obsolete: nothing to do
            FragmentStatus::Obsolete => {}

            // Mark fragment as obsolete
            FragmentStatus::Empty | FragmentStatus::Valid => {
                let frag_header_addr =
                    block_addr + (BLOCK_HEADER_SIZE + (FRAG_HEADER_SIZE * rel_frag_no)) as u32;
                if let Some((marker_offset, marker)) = FragmentStatus::Obsolete.as_byte() {
                    self.flash
                        .write(frag_header_addr + marker_offset as u32, &[marker])
                        .await?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    use embedded_storage::nor_flash::NorFlashErrorKind;

    use super::*;
    use crate::tests::mock::mock_flash::{self, MockError, MockFlash};

    #[tokio::test]
    async fn test_read_unformatted_blocks_invalid() {
        let mut flash = mock_flash::new();
        let total_size = (&mut flash).capacity();

        let mut blockdev: BlockDevice<&mut MockFlash<4096>, 4096> =
            BlockDevice::writeable_from(&mut flash).unwrap();
        assert_eq!(total_size / 4096, blockdev.blocks_total());
        for block_id in 0..blockdev.blocks_total() as u32 {
            assert!(blockdev.block_header(block_id).await.unwrap().is_none());

            // also verify via shortcut API
            assert!(!blockdev.block_is_valid(block_id).await.unwrap(),);
        }
    }

    #[tokio::test]
    async fn test_read_hardcoded_block() {
        let mut flash = mock_flash::new();

        // Manually construct a blockheader. This is done manually so that the test catches regressions: we want the on-disk format to be stable!
        let offset = 1 * 512;
        flash.memory[offset..offset + 4].copy_from_slice(&[0x90, 0x0D, 0xFF, 0xFF]);
        flash.memory[offset + 4..offset + 6].copy_from_slice(&(123_u16).to_le_bytes());
        flash.memory[offset + 6..offset + 8].copy_from_slice(&(42_u16).to_le_bytes());
        flash.memory[offset + 8..offset + 12].copy_from_slice(&(0xABCD_0123_u32).to_le_bytes());

        // write content in third fragment of block 1
        flash.memory[offset + 16 + 2 * 3] = 0xD1;
        flash.memory[offset + 16 + 2 * 3 + 1] = 0xDC;
        let data_offset = offset + 16 + 11 * 3 + 2 * 42; // 16-byte header, 11 3-byte frag headers, third (offset 2) 42-byte fragment
        flash.memory[data_offset..data_offset + 42].fill(0xAB);

        let mut blockdev: BlockDevice<&mut MockFlash<512>, 512> =
            BlockDevice::writeable_from(&mut flash).unwrap();
        // blocks 0,2 are not initialized: should be invalid
        assert!(!blockdev.block_header(0).await.unwrap().is_some());
        assert!(!blockdev.block_header(2).await.unwrap().is_some());

        // block 1 is manually constructed in this test: header should match
        {
            let block = blockdev.block_header(1).await.unwrap().unwrap();
            assert_eq!(block.object_id, 123);
            assert_eq!(block.fragment_size(), 42);
            assert_eq!(block.fragment_count(), 11); // 11 * (42+3) + 16 = 511, just fits in a 512-byte block
            assert_eq!(block.first_fragment(), 0xABCD_0123);

            // also verify via shortcut API
            assert!(blockdev.block_is_valid(1).await.unwrap());

            // Verify fragments: only third fragment (0xABCD_0123 + 2) should exist and be filled with 0xAB
            let mut frag_data = [0xC0; 64];
            assert!(blockdev
                .read_fragment(&block, 0xABCD_0123, &mut frag_data)
                .await
                .is_err());
            assert!(blockdev
                .read_fragment(&block, 0xABCD_0124, &mut frag_data)
                .await
                .is_err());
            assert!(blockdev
                .read_fragment(&block, 0xABCD_0126, &mut frag_data)
                .await
                .is_err());
            assert!(blockdev
                .read_fragment(&block, 2, &mut frag_data)
                .await
                .is_err());

            // buffer should still be untouched
            assert!(frag_data.into_iter().all(|byte| byte == 0xC0));

            // this fragment exists: check it returns exactly 42 0xAB bytes (rest of the buffer stays untouched)
            assert_eq!(
                42,
                blockdev
                    .read_fragment(&block, 0xABCD_0125, &mut frag_data)
                    .await
                    .unwrap()
            );
            assert!(frag_data[..42].into_iter().all(|byte| *byte == 0xAB));
            assert!(frag_data[42..].into_iter().all(|byte| *byte == 0xC0));
        }
    }

    #[tokio::test]
    async fn test_format_block() {
        let mut flash = mock_flash::new();

        let object_id = 123;
        let object_version = 0xFF;
        let frag_size = 66_u16;
        let mut frag_no = 4567; // fragment numbers start at this offset
        let frags_per_block = calculate_fragments_per_block(4096, frag_size.into()) as u32;

        let mut blockdev: BlockDevice<&mut MockFlash<4096>, 4096> =
            BlockDevice::writeable_from(&mut flash).unwrap();

        for block_id in 0..blockdev.blocks_total() as u32 {
            // block should not be valid (not formatted yet)
            assert!(!blockdev.block_is_valid(block_id).await.unwrap());

            blockdev
                .format_block(block_id, object_id, object_version, frag_size, frag_no)
                .await
                .unwrap();
            frag_no += frags_per_block;

            // block should be valid after format
            assert!(blockdev.block_is_valid(block_id).await.unwrap());
        }
    }

    #[tokio::test]
    async fn test_format_block_is_atomic() {
        // Test behaviour under all possible combinations of write/erase fails.
        // The block must always be eiter completely formatted or readback as invalid.
        let mut success_count = 0;
        let mut fail_count = 0;
        for erase_fail in 0..2 {
            for write_fail in 0..5 {
                let mut flash = mock_flash::new();
                flash.trigger_erase_error_after(erase_fail, MockError(NorFlashErrorKind::Other));
                flash.trigger_write_error_after(write_fail, MockError(NorFlashErrorKind::Other));

                let block_id = 3;
                let object_id = 123;
                let object_version = 0xFF;
                let frag_size = 66_u16;
                let frag_no = 4567; // fragment numbers start at this offset
                let frags_per_block = calculate_fragments_per_block(4096, frag_size.into());

                let mut blockdev: BlockDevice<&mut MockFlash<4096>, 4096> =
                    BlockDevice::writeable_from(&mut flash).unwrap();

                // block should not be valid yet
                assert!(!blockdev.block_is_valid(block_id).await.unwrap());

                // attempt to format block. this will fail if affected by failed write/erase
                match blockdev
                    .format_block(block_id, object_id, object_version, frag_size, frag_no)
                    .await
                {
                    // Format claims to have succeeded, so the block *must* be valid and in the expected state
                    Ok(_) => {
                        let block = blockdev.block_header(block_id).await.unwrap().unwrap();
                        assert_eq!(block.object_id, object_id);
                        assert_eq!(block.fragment_size(), usize::from(frag_size));
                        assert_eq!(block.fragment_count(), frags_per_block);
                        assert_eq!(block.first_fragment(), frag_no);

                        success_count += 1;
                    }

                    // Format failed, so it *must* be invalid (or in the state it was before the format, which was also invalid)
                    Err(_) => {
                        assert!(blockdev.block_header(block_id).await.unwrap().is_none());
                        fail_count += 1;
                    }
                }
            }
        }

        // If these asserts fail, it means format_block() needs more than 5 writes + 2 erases to succeed.
        // If that is as intended in a future implementation, increase the erase_fail / write_fail loop limits
        assert!(success_count >= 1);
        assert!(fail_count >= 1);
    }

    #[tokio::test]
    async fn test_write_fragments() {
        let mut flash = mock_flash::new();

        let block_id = 3;
        let object_id = 123;
        let object_version = 0xFF;
        let frag_size = 66_u16;
        let frag_no = 4567;

        let mut blockdev: BlockDevice<&mut MockFlash<4096>, 4096> =
            BlockDevice::writeable_from(&mut flash).unwrap();

        blockdev
            .format_block(block_id, object_id, object_version, frag_size, frag_no)
            .await
            .unwrap();
        let block = blockdev.block_header(block_id).await.unwrap().unwrap();

        // fragments should not exist yet
        let mut buffer = [0xEE; 6];
        assert_eq!(
            FlashReadErrorKind::FragmentEmpty,
            blockdev
                .read_fragment(&block, block.first_fragment(), &mut buffer)
                .await
                .unwrap_err()
                .kind()
        );
        assert_eq!(
            FlashReadErrorKind::FragmentEmpty,
            blockdev
                .read_fragment(&block, block.first_fragment() + 1, &mut buffer)
                .await
                .unwrap_err()
                .kind()
        );

        // Write a fragment and read it back
        blockdev
            .write_fragment(&block, block.first_fragment(), &[5, 6, 7, 8, 9])
            .await
            .unwrap();
        assert_eq!(
            buffer.len(),
            blockdev
                .read_fragment(&block, block.first_fragment(), &mut buffer)
                .await
                .unwrap()
        );
        assert_eq!(buffer, [5, 6, 7, 8, 9, 0xFF]);

        // fragment next to it should still be empty
        assert_eq!(
            FlashReadErrorKind::FragmentEmpty,
            blockdev
                .read_fragment(&block, block.first_fragment() + 1, &mut buffer)
                .await
                .unwrap_err()
                .kind()
        );

        // Try to write a fragment again (must fail as it already exists)
        assert_eq!(
            FlashWriteErrorKind::FragmentExists,
            blockdev
                .write_fragment(&block, block.first_fragment(), &[33, 44, 55, 66, 77])
                .await
                .unwrap_err()
                .kind()
        );
    }

    #[tokio::test]
    async fn test_write_fragment_is_atomic() {
        let block_id = 3;
        let object_id = 123;
        let object_version = 0xFF;
        let frag_size = 66_u16;
        let frag_no = 4567;

        // Test behaviour under all possible combinations of write/erase fails.
        // The block must always be eiter completely formatted or readback as invalid.
        let mut success_count = 0;
        let mut fail_count = 0;
        for erase_fail in 0..2 {
            for write_fail in 0..4 {
                let mut flash = mock_flash::new();
                // Temporary scope: initialize and format the target block
                {
                    let mut blockdev: BlockDevice<&mut MockFlash<4096>, 4096> =
                        BlockDevice::writeable_from(&mut flash).unwrap();

                    blockdev
                        .format_block(block_id, object_id, object_version, frag_size, frag_no)
                        .await
                        .unwrap();
                }

                // initialize a new blockdevice, but now the flash storage will have has some errors injected
                flash.trigger_erase_error_after(erase_fail, MockError(NorFlashErrorKind::Other));
                flash.trigger_write_error_after(write_fail, MockError(NorFlashErrorKind::Other));
                let mut blockdev: BlockDevice<&mut MockFlash<4096>, 4096> =
                    BlockDevice::writeable_from(&mut flash).unwrap();
                let block = blockdev.block_header(block_id).await.unwrap().unwrap();

                // fragment should still be empty
                let mut buffer = [0xEE; 6];
                assert_eq!(
                    FlashReadErrorKind::FragmentEmpty,
                    blockdev
                        .read_fragment(&block, block.first_fragment(), &mut buffer)
                        .await
                        .unwrap_err()
                        .kind()
                );

                // Write a fragment and read it back
                match blockdev
                    .write_fragment(&block, block.first_fragment(), &[5, 6, 7, 8, 9])
                    .await
                {
                    // The write succeeded so the data *must* match
                    Ok(_) => {
                        assert_eq!(
                            buffer.len(),
                            blockdev
                                .read_fragment(&block, block.first_fragment(), &mut buffer)
                                .await
                                .unwrap()
                        );
                        assert_eq!(buffer, [5, 6, 7, 8, 9, 0xFF]);

                        success_count += 1;
                    }

                    // Write failed so the fragment *must* read as empty or invalid
                    Err(_) => {
                        match blockdev
                            .read_fragment(&block, block.first_fragment(), &mut buffer)
                            .await
                            .unwrap_err()
                            .kind()
                        {
                            FlashReadErrorKind::FragmentEmpty
                            | FlashReadErrorKind::FragmentInvalid => {}
                            unexpected_kind @ _ => {
                                panic!("unexpected error kind {unexpected_kind:?}")
                            }
                        }
                        fail_count += 1;
                    }
                }
            }
        }
        // If these asserts fail, it means write_fragment() needs more than 3 writes + 0 erases to succeed.
        // If that is as intended in a future implementation, increase the erase_fail / write_fail loop limits
        // and/or relax these assertions
        assert_eq!(success_count, 2);
        assert_eq!(fail_count, 6);
    }

    #[tokio::test]
    async fn test_delete_fragment() {
        let mut flash = mock_flash::new();

        let block_id = 3;
        let object_id = 123;
        let object_version = 0xFF;
        let frag_size = 66_u16;
        let frag_no = 4567;

        let mut blockdev: BlockDevice<&mut MockFlash<4096>, 4096> =
            BlockDevice::writeable_from(&mut flash).unwrap();

        blockdev
            .format_block(block_id, object_id, object_version, frag_size, frag_no)
            .await
            .unwrap();

        // Start with empty block
        let block = blockdev.block_header(block_id).await.unwrap().unwrap();
        let mut buffer = [0; 6];
        assert!(blockdev.block_is_empty(&block).await.unwrap());

        // Write three fragments
        for i in 0..3 {
            blockdev
                .write_fragment(&block, block.first_fragment() + i, &[5, 6, 7, 8, 9])
                .await
                .unwrap();
        }

        // Delete second fragment
        blockdev
            .delete_fragment(&block, block.first_fragment() + 1)
            .await
            .unwrap();

        // Readback: Second fragment must now be obsolete
        assert_eq!(
            FlashReadErrorKind::FragmentObsolete,
            blockdev
                .read_fragment(&block, block.first_fragment() + 1, &mut buffer)
                .await
                .unwrap_err()
                .kind()
        );

        // Readback first+third fragment (they must not be affected)
        for i in [0, 2] {
            assert_eq!(
                buffer.len(),
                blockdev
                    .read_fragment(&block, block.first_fragment() + i, &mut buffer)
                    .await
                    .unwrap()
            );
            assert_eq!(buffer, [5, 6, 7, 8, 9, 0xFF]);
        }
        assert!(!blockdev.block_is_empty(&block).await.unwrap());
    }
}

//! # Flash database
//!
//! Implements a very limited 'filesystem-like' storage database.
//!
//! ## Consistent write throughput
//!
//! Writes to any object should lead to a predictable amount of flash writes/erases so the firmware can have a well-defined upper bound on the write latency.
//!
//! ## Robustness
//!
//! Well-defined behaviour if the underlying flash write/erase is aborted (power failure, reboot, panic etc).
//! This is mainly handled in the [block layer](block_layer).
//!
//! ## Simplicity
//!
//! Dynamically creating/resizing files is not supported. No folders or filenames: the list of possible objects and their size
//! is known at compile-time. Resizing or creating new files can be supported but will probably look like a database migration.
//!
//! ## Embedded-specific storage
//!
//! Apart from [File] this will support [Circular] objects wihch are optimized for use as persistent queue.
//! By abstracting over the [block layer](block_layer) it may be possible to add new object types in the future if we want.
//!
//! ## Wear leveling
//!
//! The assumption is that an object as a whole is not rewritten more than the flash erase endurance.
//! The database assumes that writes are evenly spread over fragments within an object. This is guaranteed if using the circular buffer mode.
//! For example: a 1000-fragment circular store (on flash rated for 100K erase cycles) can handle 100M events before flash is worn.
//! It is up to the user to select suitable file sizes and guard against too many (re-)writes.
//!

use core::{marker::PhantomData, ops::Range};

use embedded_storage::nor_flash::NorFlashError;

use block_layer::{
    FlashError, FlashReadError, FlashWriteError, FlashWriteErrorKind, FragmentStatus,
};
mod arbiter;
pub mod block_layer;
mod circular_store;
mod file_store;
mod schema;

pub use circular_store::*;
pub use file_store::*;

use self::block_layer::BlockHeader;

/// Unique identifier for a specific file
pub type ObjectId = u16;

/// Each file needs at least one block extra to use as temporary storage
/// in case an interrupted / failed write needs to be retried
const RESERVED_BLOCKS_PER_FILE: usize = 1;
const MAX_FRAGMENTS_PER_OBJECT: usize = (u32::MAX / 2) as usize;

mod private {
    // Marker: traits not to be implemented outside this module
    pub trait Sealed<const _B: usize> {}
}

/// Object that can be stored in the [Database]
pub trait Object<const BLOCK_SIZE: usize>: Copy {
    /// Id must be unique per file
    fn id(&self) -> ObjectId;

    /// (maximum) fragment size
    fn fragment_size(&self) -> usize;

    /// All blocks assigned to this file
    ///
    /// Note:the range-end is exclusive.
    /// E.g. 1..3 means blocks 1 and 2 only
    fn flash_blocks(&self) -> Range<block_layer::BlockId>;

    /// Total storage capacity in bytes (e.g. size)
    ///
    /// Implementers may override this to define objects with
    /// a specif (e.g. non-multiple-of-fragment-size) size
    fn capacity(&self) -> usize {
        self.fragment_count() * self.fragment_size()
    }
}

/// Extension trait for Object (auto-implemented for all [Object]s)
///
/// This implementation is sealed on purpose to keep the potential
/// for errors in [Object] implementation as small as possible

pub trait ObjectExt<const BLOCK_SIZE: usize>:
    Object<BLOCK_SIZE> + private::Sealed<BLOCK_SIZE>
{
    fn block_count(&self) -> usize {
        let range = self.flash_blocks();
        (range.end - range.start) as usize
    }

    // The 'spare' block is a block not typically used for normal data storage
    // but for meta operations such as file locking
    fn spare_block(&self) -> u32 {
        self.flash_blocks().end.saturating_sub(1)
    }

    /// All blocks assigned to this file that store file data
    fn flash_blocks_except_spare(&self) -> Range<block_layer::BlockId> {
        let range = self.flash_blocks();
        range.start..range.end.saturating_sub(1)
    }

    /// How many fragments exist in each block
    fn fragments_per_block(&self) -> usize {
        block_layer::calculate_fragments_per_block(BLOCK_SIZE, self.fragment_size())
    }

    /// Total amount of fragments this file can store
    fn fragment_count(&self) -> usize {
        MAX_FRAGMENTS_PER_OBJECT
            .min(self.fragments_per_block() * (self.block_count() - RESERVED_BLOCKS_PER_FILE))
    }
}

// blanket implementations: all Objects are 'extended' by auto-implementing ObjectExt
impl<const B: usize, T: Object<B>> private::Sealed<B> for T {}
impl<const B: usize, T: Object<B>> ObjectExt<B> for T {}

/// Database: storage for different kinds of data objects
///
/// Most of the public API is available via traits:
/// - [FileStore] / [WriteableFileStore] for accessing [File]s
/// - [CircularStore] / [WriteableCircularStore] for accessing [Circular]s (circular buffers)
pub struct Database<BD, const BLOCK_SIZE: usize, F: File<BLOCK_SIZE>, C: Circular<BLOCK_SIZE>> {
    blockdev: BD,

    // Database does not directly depend on F, but this makes it easier to use the FileStore trait which does
    _file: PhantomData<F>,

    // Database does not directly depend on C, but this makes it easier to use the CircularStore trait which does
    _circular: PhantomData<C>,

    auto_format_circular: bool,
}

impl<
        BD: block_layer::Reader<BLOCK_SIZE>,
        const BLOCK_SIZE: usize,
        F: File<BLOCK_SIZE>,
        C: Circular<BLOCK_SIZE>,
    > Database<BD, BLOCK_SIZE, F, C>
{
    pub fn new(blockdevice: BD) -> Self {
        Self {
            blockdev: blockdevice,
            _file: PhantomData,
            _circular: PhantomData,

            auto_format_circular: false,
        }
    }

    /// Check if object is locked for reading
    async fn lock_status<OBJ: ObjectExt<BLOCK_SIZE>>(
        &mut self,
        object: &OBJ,
        mode: LockMode,
    ) -> Result<LockStatus, Error> {
        Ok(match mode {
            LockMode::LockSpare => {
                match self.blockdev.block_header(object.spare_block()).await? {
                    // Spare block invalid == locked
                    None => LockStatus::Locked,

                    // Spare block valid: locked unless first fragment empty
                    Some(spare) => {
                        if object.id() != spare.object_id() {
                            return Err(Error::Corrupt);
                        }
                        let locked = FragmentStatus::Empty
                            != self
                                .blockdev
                                .fragment_status(&spare, spare.first_fragment())
                                .await?;

                        match locked {
                            true => LockStatus::Locked,
                            false => LockStatus::Unlocked(spare.object_version()),
                        }
                    }
                }
            }
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub enum LockStatus {
    /// Object is unlocked (only blocks with this object_version)
    Unlocked(u8),

    /// Object is locked
    Locked,
}

#[derive(Debug, Clone, Copy)]
enum LockMode {
    // Use invalid/obsolete status of spare block as lock
    LockSpare,
}

#[allow(unused)]
#[derive(Debug, Clone, Copy, PartialEq)]
enum DeleteMode {
    // Delete + erase whole object except spare block (ignore locking)
    EraseSkipSpare,

    // Delete + erase whole object atomically (LockMode::LockSpare)
    EraseLockSpare,

    // Soft-delete: spare block is formatted with increased version number
    SoftVersion,
}

impl DeleteMode {
    pub fn lock_mode(&self) -> Option<LockMode> {
        match self {
            DeleteMode::EraseSkipSpare => None,
            DeleteMode::EraseLockSpare => Some(LockMode::LockSpare),
            DeleteMode::SoftVersion => None,
        }
    }
}

impl<
        BD: block_layer::Writer<BLOCK_SIZE>,
        const BLOCK_SIZE: usize,
        F: File<BLOCK_SIZE>,
        C: Circular<BLOCK_SIZE>,
    > Database<BD, BLOCK_SIZE, F, C>
{
    /// Lock the object
    ///
    /// While the object is locked it cannot be read.
    /// This allows for atomic delete/writes: readers see an invalid/empty object while we are busy deleting/writing its contents.
    /// Note that the lock is persistent, so if a write/delete is aborted the file stays locked untill it is manually erased.
    async fn lock<OBJ: ObjectExt<BLOCK_SIZE>>(
        &mut self,
        object: &OBJ,
        mode: LockMode,
    ) -> Result<(), Error> {
        let spare_header = self.blockdev.block_header(object.spare_block()).await?;
        match mode {
            LockMode::LockSpare => {
                if let Some(spare) = spare_header {
                    self.blockdev
                        .delete_fragment(&spare, spare.first_fragment())
                        .await?;
                }
            }
        }
        Ok(())
    }

    /// Unlock the object
    ///
    /// allows readers to read the object again.
    /// Assumes the contents is in a known good state (e.g. write/erase succeeded).
    /// If not, use delete instead.
    async fn unlock<OBJ: ObjectExt<BLOCK_SIZE>>(
        &mut self,
        object: &OBJ,
        mode: LockMode,
        object_version: u8,
    ) -> Result<(), Error> {
        // Skip unlock if already unlocked
        match self.lock_status(object, mode).await {
            Ok(LockStatus::Unlocked(_)) => {
                return Ok(());
            }
            _ => {}
        }

        match mode {
            LockMode::LockSpare => {
                self.blockdev
                    .format_block(
                        object.spare_block(),
                        object.id(),
                        object_version,
                        object.fragment_size() as u16,
                        0,
                    )
                    .await?;
            }
        }
        Ok(())
    }

    /// Delete the object
    async fn delete_object<OBJ: ObjectExt<BLOCK_SIZE>>(
        &mut self,
        object: &OBJ,
        mode: DeleteMode,
    ) -> Result<(), Error> {
        let mut format_count = 0;
        let total_frag_count = object.fragment_count() as u32;
        if total_frag_count == 0 {
            return Err(Error::InvalidObject);
        }

        // Try to find next version
        let first_block = object.flash_blocks().start;
        let spare_block = object.spare_block();
        let first_header = self.blockdev.block_header(first_block).await;
        let spare_header = self.blockdev.block_header(spare_block).await;
        let mut versions = [0xFF; 2];
        let mut erase_counts = [0; 2];
        for header in [first_header, spare_header] {
            header.ok().flatten().as_ref().map(|header| {
                erase_counts[0] = header.first_fragment() / total_frag_count;
                versions[0] = header.object_version();
            });
        }

        // because of wrapping, delta > 128 means the lower number is actually likely newer
        let existing_version = {
            let max_version = versions[0].max(versions[1]);
            let min_version = versions[0].min(versions[1]);
            if max_version - min_version > 128 {
                min_version
            } else {
                max_version
            }
        };
        let object_next_version = existing_version.wrapping_add(1);

        // Soft delete: only block 1 + spare are erased with new (incremented) version number
        if mode == DeleteMode::SoftVersion {
            for (i, block_to_erase) in [first_block, spare_block].iter().enumerate() {
                let first_frag_no_for_block = block_to_erase * object.fragments_per_block() as u32;

                self.blockdev
                    .format_block(
                        *block_to_erase,
                        object.id(),
                        object_next_version,
                        object.fragment_size() as u16,
                        first_frag_no_for_block
                            .wrapping_add((erase_counts[i] + 1) * total_frag_count),
                    )
                    .await?;
            }

            return Ok(());
        }

        for block in object.flash_blocks() {
            // Skip spare block (managing e.g. lock/unlock via spare block is responsibility of caller)
            if mode == DeleteMode::EraseSkipSpare && block == object.spare_block() {
                continue;
            }

            let first_frag_no_for_block = block * object.fragments_per_block() as u32;

            // Find out if the block actually needs to be erased or not.
            // This can give a big speedup (erasing is slow) and avoid unnecesary flash wear.
            let mut format = true;
            let mut erase_count = 0;
            if let Some(header) = self.blockdev.block_header(block).await? {
                // Must reformat if block belonged to a different object
                if header.object_id() == object.id() {
                    // block_layer fragment numbers keep incrementing forever (wrap on overflow).
                    // if a block is at N * object size, we assume it was erased N times
                    erase_count = header.first_fragment() / total_frag_count;

                    // block metadata up-to-date: only format block if not empty
                    if header.fragment_size() == object.fragment_size()
                        && ((header.first_fragment() % total_frag_count) == first_frag_no_for_block)
                    {
                        format = !self.blockdev.block_is_empty(&header).await?;
                    }
                }
            }

            if format {
                // 1. First time a block will be formatted: lock the object.
                if format_count == 0 {
                    if let Some(lock) = mode.lock_mode() {
                        self.lock(object, lock).await?;
                    }
                }

                // 2. erase this block
                // Note that the spare block is by definition the last block in the object:
                // upon erasing the last (spare) block the object is by defenition no longer marked obsolete.
                self.blockdev
                    .format_block(
                        block,
                        object.id(),
                        object_next_version,
                        object.fragment_size() as u16,
                        first_frag_no_for_block.wrapping_add((erase_count + 1) * total_frag_count),
                    )
                    .await?;

                format_count += 1;
            }
        }
        Ok(())
    }

    async fn prepare_writeable_block<OBJ: ObjectExt<BLOCK_SIZE>>(
        &mut self,
        object: &OBJ,
        block_no: u32,
        version: u8,
    ) -> Result<BlockHeader, Error> {
        let first_frag_no_for_block = block_no * object.fragments_per_block() as u32;
        let total_frag_count = object.fragment_count().max(1) as u32;
        let mut erase_count = 0;

        let is_writeable = |block: &BlockHeader| -> bool {
            object.fragment_size() == block.fragment_size() && block.object_version() == version
        };

        // 1. Read block header and check if it should be writeable
        if let Some(block) = self.blockdev.block_header(block_no).await? {
            // If the block format already matches and its version is up-to-date, no format is required
            if is_writeable(&block) {
                return Ok(block);
            }

            erase_count = block.first_fragment() / total_frag_count;
        }

        // 2. Not writeable: try to format the block
        self.blockdev
            .format_block(
                block_no,
                object.id(),
                version,
                object.fragment_size() as u16,
                first_frag_no_for_block.wrapping_add((erase_count + 1) * total_frag_count),
            )
            .await?;

        // 3. After formatting, the block should be OK
        if let Some(block) = self.blockdev.block_header(block_no).await? {
            // If the block format already matches and its version is up-to-date, no format is required
            if is_writeable(&block) {
                return Ok(block);
            }
        }

        // Readback after erase failed: should not happen (bug or flash glitch)
        return Err(Error::WriteFailedVerify);
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum InitStatus {
    /// A new, empty store was initialized (no previous persistent data found)
    New,

    /// Persistent store already existed (data may already be available for reading)
    Existing,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Error {
    /// Something went wrong in the underlying flash driver
    Flash,

    /// Given storage object has invalid properties
    InvalidObject,

    /// Tried to access address outside the flash area.
    /// This would mean the file definition is wrong (extends past end of flash)
    CorruptOutOfBounds,

    /// Data store has become corrupted (lost, probably need to re-format)
    Corrupt,

    /// Stream has become corrupted (probably needs to be deleted, other files/streams are not affected)
    CorruptStream,

    /// Cannot store fragment: ObjectExt full or fragment out of bounds
    NoSpaceAvailable,

    /// Requested object is not found
    NotFound,

    /// ObjectExt does not fit in the given buffer
    BufferTooSmall,

    /// Something internal to the library went wrong (bug)
    InternalError,

    /// File fragment already exists
    FragmentExists,

    /// File fragment exceeds maximum size
    FragmentTooLarge,

    /// File fragment does not exist
    FragmentNotFound,

    /// File fragment cannot be written (needs to be deleted first)
    FragmentNotWriteable,

    /// Flash write failed because readback mismatched
    WriteFailedVerify,

    /// File cannot be read: either locked for writing or has never been initialized
    FileNotReadable,

    FragmentNotReadable,
}

impl<E: NorFlashError> From<block_layer::FlashError<E>> for Error {
    fn from(error: block_layer::FlashError<E>) -> Self {
        match error {
            FlashError::BlockOutOfBounds => Error::CorruptOutOfBounds,
            FlashError::Flash(_) => Error::Flash,
        }
    }
}

impl<E: NorFlashError> From<block_layer::FlashReadError<E>> for Error {
    fn from(error: block_layer::FlashReadError<E>) -> Self {
        match error {
            FlashReadError::Flash(_) => Error::Flash,
            FlashReadError::Read(read_err) => match read_err {
                block_layer::FlashReadErrorKind::FragmentEmpty => Error::FragmentNotReadable,
                block_layer::FlashReadErrorKind::FragmentInvalid => Error::FragmentNotReadable,
                block_layer::FlashReadErrorKind::FragmentObsolete => Error::FragmentNotReadable,
                block_layer::FlashReadErrorKind::FragmentNotFound => Error::FragmentNotFound,
                block_layer::FlashReadErrorKind::BlockInvalid => Error::FragmentNotReadable,
                block_layer::FlashReadErrorKind::BlockOutOfBounds => Error::CorruptOutOfBounds,
                block_layer::FlashReadErrorKind::Flash => Error::Flash,
            },
        }
    }
}

impl<E: NorFlashError> From<FlashWriteError<E>> for Error {
    fn from(error: FlashWriteError<E>) -> Self {
        match error {
            FlashWriteError::Flash(_) => Error::Flash,
            FlashWriteError::Write(write_err) => match write_err {
                FlashWriteErrorKind::FragmentNotFound => Error::FragmentNotFound,
                FlashWriteErrorKind::BlockInvalid
                | FlashWriteErrorKind::FragmentInvalid
                | FlashWriteErrorKind::FragmentObsolete => Error::FragmentNotWriteable,
                FlashWriteErrorKind::FragmentExists => Error::FragmentNotWriteable,
                FlashWriteErrorKind::FragmentTooLarge => Error::FragmentTooLarge,
                FlashWriteErrorKind::VerifyFailed => Error::WriteFailedVerify,
                FlashWriteErrorKind::BlockOutOfBounds => Error::CorruptOutOfBounds,
                FlashWriteErrorKind::Flash => Error::Flash,
            },
        }
    }
}

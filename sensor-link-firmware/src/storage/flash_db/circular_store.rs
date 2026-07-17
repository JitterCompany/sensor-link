use super::{
    block_layer::{self, BlockHeader, FragmentStatus},
    Database, Error, File, InitStatus, Object, ObjectExt, MAX_FRAGMENTS_PER_OBJECT,
};

/// Circular trait: all circular buffers you want to store in the [Database] must implement this trait
///
/// These can be read from [CircularStore] and written to [WriteableCircularStore].
///
/// Implementers must be careful that all circular buffers have:
/// - an id that is at least unique-per-buffer
/// - flash block range must be globally unique (cannot overlap any other objects)
pub trait Circular<const BLOCK_SIZE: usize>: Object<BLOCK_SIZE> {
    fn overwrite_on_full(&self) -> bool;
}

/// Sequence number of each fragment (ever increasing with wraparound)
pub type SeqNo = u32;

/// Represents a range of fragments in the circular store
///
/// Note that the start/end may overflow. Compare via [fragment_gte]
#[derive(Debug, Clone, Default)]
pub struct CircularRange {
    // First valid fragment
    pub start: SeqNo,

    // First invalid fragment (= to be written)
    pub end: SeqNo,
}

impl CircularRange {
    /// The SeqNo representing the 'read pointer': start trying to read here
    pub fn read_at(&self) -> SeqNo {
        self.start
    }

    /// The SeqNo representing the 'write pointer': start trying to write here
    pub fn write_at(&self) -> SeqNo {
        self.end
    }

    /// Amount of fragments in the range
    pub fn len(&self) -> usize {
        self.end.wrapping_sub(self.start) as usize
    }
}

fn find_block_no_for_seq_no<const BS: usize, C: Circular<BS>>(circular: &C, seq_no: SeqNo) -> u32 {
    // Find out which block stores the fragment.
    // While `seq_no` increases 'forever', it is stored in a predictable location
    let rel_frag_no = seq_no % circular.fragment_count() as u32;
    let block_offset = rel_frag_no / circular.fragments_per_block() as u32;
    circular.flash_blocks().start + block_offset
}

enum FindFail {
    NotFound(block_layer::BlockHeader),
    Corrupt(block_layer::BlockHeader),
    Other(Error),
}

impl From<FindFail> for Error {
    fn from(value: FindFail) -> Self {
        match value {
            FindFail::NotFound(_) => Error::FragmentNotFound,
            FindFail::Corrupt(_) => Error::Corrupt,
            FindFail::Other(error) => error,
        }
    }
}

/// Try to find the block header for the fragment with given seq_no
///
/// - return Ok(None) if no valid block header can be found
/// - returns Ok(block) if fragment is found in that block
/// - returns Err if fragment not found or corrupt or other flash error
async fn find_block_for_seq_no<BD: block_layer::Reader<BS>, const BS: usize, C: Circular<BS>>(
    blockdev: &mut BD,
    circular: &C,
    seq_no: SeqNo,
) -> Result<Option<block_layer::BlockHeader>, FindFail> {
    let block_no = find_block_no_for_seq_no(circular, seq_no);
    let fragment_in_block = seq_no % circular.fragments_per_block() as u32;
    let block = blockdev
        .block_header(block_no)
        .await
        .map_err(|err| FindFail::Other(err.into()))?;

    block
        .map(|block| {
            // Block exists but with unexpected id or fragment size?
            // This likely means the filesystem has changed without proper migration / reformat
            if circular.id() != block.object_id() {
                return Err(FindFail::Corrupt(block));
            }
            if circular.fragment_size() != block.fragment_size() {
                return Err(FindFail::Corrupt(block));
            }

            // Check the expected fragment is contained in this block.
            // This may occur if the data is already overwritten N times with a newer version
            // (seq_no will be N * fragment_count())
            if seq_no != block.first_fragment().wrapping_add(fragment_in_block) {
                return Err(FindFail::NotFound(block));
            }
            Ok(block)
        })
        .transpose()
}

/// Read-only database containing [Circular] buffers
pub trait CircularStore<const BLOCK_SIZE: usize, C: Circular<BLOCK_SIZE>> {
    /// Initialize the store
    ///
    /// Required for correct operation. Other methods may fail if not initialized first.
    async fn initialize_circular_store(&mut self) -> Result<(), Error>;

    /// Find the range of sequence numbers that may have data stored
    ///
    /// The result can be used to determine which seq_no to read/write/delete.
    ///
    /// *Note: for best read/write throughput it is recommended to cache the result*
    async fn find_circular_range(&mut self, circular: C) -> Result<CircularRange, Error>;

    /// Read (part of) a specific circular fragment, starting at a specific offset
    ///
    /// Tries to fill the result buffer with data from the circular fragment.
    /// Result buffer may be larger or smaller than the fragment size.
    /// Returns the actual amount of data read.
    ///
    /// Offset is relative to the start of the fragment
    async fn read_circular_fragment_at_offset(
        &mut self,
        circular: C,
        fragment_no: SeqNo,
        bytes: &mut [u8],
        offset: usize,
    ) -> Result<usize, Error>;

    /// Read (part of) a specific circular fragment
    ///
    /// Tries to fill the result buffer with data from the circular fragment.
    /// Result buffer may be larger or smaller than the fragment size.
    /// Returns the actual amount of data read.
    #[inline]
    async fn read_circular_fragment(
        &mut self,
        circular: C,
        fragment_no: SeqNo,
        bytes: &mut [u8],
    ) -> Result<usize, Error> {
        self.read_circular_fragment_at_offset(circular, fragment_no, bytes, 0)
            .await
    }
}

/// Writeable database for storing [Circular]s
pub trait WriteableCircularStore<const BLOCK_SIZE: usize, C: Circular<BLOCK_SIZE>>:
    CircularStore<BLOCK_SIZE, C>
{
    /// Initialize the store
    ///
    /// Required for correct operation. Other methods may fail if not initialized first.
    async fn initialize_writeable_circular_store(
        &mut self,
        auto_format: bool,
    ) -> Result<InitStatus, Error>;

    /// # Write data to a circular fragment
    ///
    /// Try to write to a fragment. This is fail-safe: the resulting fragment is always either unreadable, or contains exactly the new data.
    /// If the fragment is the first fragment in a block, the block is auto-erased
    /// (unless it still contains data and overwrite-on-full is disabled).
    ///
    /// *Note: rewriting a fragment multiple times will fail with `Error::FragmentNotWriteable`.
    /// Find the first writeable SeqNo via [find_circular_range()](method@CircularStore::find_circular_range) or retry with incremented `fragment_no`*
    async fn write_circular_fragment(
        &mut self,
        circular: C,
        fragment_no: SeqNo,
        bytes: &[u8],
    ) -> Result<(), Error>;

    /// Delete the circular fragment
    ///
    /// This marks the fragment as deleted:
    /// - the fragment is no longer considered 'in range' (see [CircularStore::find_circular_range()](method@CircularStore::find_circular_range))
    /// - the fragment is no longer readable (data might technically be recoverable from the underlying flash as it is not immediately erased)
    /// - the fragment may be overwritten by future writes, independent of overwrite-on-full
    async fn delete_circular_fragment(
        &mut self,
        circular: C,
        fragment_no: SeqNo,
    ) -> Result<(), Error>;
}

/// returns true if `large` >= `small`
pub fn fragment_gte(large: SeqNo, small: SeqNo) -> bool {
    large.wrapping_sub(small) < (MAX_FRAGMENTS_PER_OBJECT as u32)
}

impl<BD, F, C, const BS: usize> CircularStore<BS, C> for Database<BD, BS, F, C>
where
    BD: block_layer::Reader<BS>,
    F: File<BS>,
    C: Circular<BS>,
{
    async fn initialize_circular_store(&mut self) -> Result<(), Error> {
        // TODO if we implement a FAT, we could load it here
        Ok(())
    }

    async fn find_circular_range(&mut self, circular: C) -> Result<CircularRange, Error> {
        // Track the range of blocks that may contain data (if any)
        let mut data_blocks: Option<(BlockHeader, BlockHeader)> = None;

        // Fallback in case no data is found: continue in first empty block
        let mut first_empty_block: Option<BlockHeader> = None;

        // Last fallback: if no empty blocks have beem found: continue with a new block after this one
        let mut highest_block: Option<BlockHeader> = None;

        // 1. Linear scan across all blocks.
        //
        // Simple & robust and probably fast enough
        // (16-byte + 2x3-byte read per block, assuming 16MB file with 4K blocks this reads 64K)
        // If needed the speed could be optimized a lot by performing a binary search
        // which should be O(log N) vs curent O(N)
        for block_id in circular.flash_blocks_except_spare() {
            if let Some(block) = self.blockdev.block_header(block_id).await? {
                let frag_no: SeqNo = block.first_fragment();

                // check if this block may contain data (assuming circular write/delete).
                let mut block_may_contain_data = false;
                for offset in 0..block.fragment_count() {
                    // Search fragments in reverse order untill we find a non-invalid fragment
                    let frag_in_block = block.last_fragment().wrapping_sub(offset as u32);
                    match self.blockdev.fragment_status(&block, frag_in_block).await? {
                        // Invalid fragment: skip
                        // (this should be rare, only occurs after aborted write / power interrupt)
                        FragmentStatus::Invalid => {
                            continue;
                        }

                        // Valid fragment: proof this block contains data
                        FragmentStatus::Valid => {
                            block_may_contain_data = true;
                            break;
                        }

                        // Last obsolete fragment: no data in block
                        // Assuming fragments are only marked obsolete (deleted) in increasing order (e.g. oldest first)
                        FragmentStatus::Obsolete => {
                            block_may_contain_data = false;
                            break;
                        }

                        // Empty fragment: block may be partially or fully empty.
                        // Assuming fragments are only written in increasing order (e.g. oldest first) we only have to check the first block.
                        FragmentStatus::Empty => {
                            let first_status = self
                                .blockdev
                                .fragment_status(&block, block.first_fragment())
                                .await?;

                            // First status empty would mean all blocks in the range are empty, thus no data.
                            // First status non-empty implies this block probably contains or contained data.
                            // Note: even if the block only contains invalid/obsolete data it still counts [as maybe containing data]
                            // but that is ok as there is no data in the queue in that case
                            let block_is_empty = first_status == FragmentStatus::Empty;
                            block_may_contain_data = !block_is_empty;

                            // Keep track of the lowest empty block
                            if block_is_empty {
                                let first_empty =
                                    first_empty_block.get_or_insert_with(|| block.clone());
                                if !fragment_gte(frag_no, first_empty.first_fragment()) {
                                    *first_empty = block.clone();
                                }
                            }

                            break;
                        }
                    }
                }

                if block_may_contain_data {
                    let (read, write) =
                        data_blocks.get_or_insert_with(|| (block.clone(), block.clone()));

                    // new block > current write pointer: update
                    if fragment_gte(frag_no, write.first_fragment()) {
                        *write = block.clone()
                    }

                    // new block < current read pointer: update
                    if !fragment_gte(frag_no, read.first_fragment()) {
                        *read = block.clone()
                    }
                }

                // Keep track of the highest block seen
                let highest = highest_block.get_or_insert_with(|| block.clone());
                if fragment_gte(frag_no, highest.first_fragment()) {
                    *highest = block.clone();
                }
            }
        }

        // 2. Within the read/write block, find the first/last fragment of data. This defines the exact boundary of the data range:
        let range = match (data_blocks, first_empty_block, highest_block) {
            // Found a range that contains (or once had contained) valid data fragments.
            // Still have to loop across the fragments in this block to find the exact read/write indices
            (Some((read, write)), _, _) => {
                // find first valid fragment (or empty which means no data available)
                let first_seq_no = {
                    let mut first_seq_no = read.first_fragment();
                    for _ in 0..read.fragment_count() {
                        match self.blockdev.fragment_status(&read, first_seq_no).await? {
                            FragmentStatus::Invalid | FragmentStatus::Obsolete => {
                                first_seq_no += 1;
                            }
                            FragmentStatus::Empty | FragmentStatus::Valid => break,
                        }
                    }
                    first_seq_no
                };

                // find first empty fragment if any (if none found the seq_no points to first seg in next block)
                let last_seq_no = {
                    let mut last_seq_no = write.first_fragment();
                    for _ in 0..write.fragment_count() {
                        match self.blockdev.fragment_status(&write, last_seq_no).await? {
                            FragmentStatus::Valid
                            | FragmentStatus::Invalid
                            | FragmentStatus::Obsolete => {
                                last_seq_no += 1;
                            }
                            FragmentStatus::Empty => break,
                        }
                    }
                    last_seq_no
                };

                CircularRange {
                    start: first_seq_no,
                    end: last_seq_no,
                }
            }

            // No data in store: continue at first empty fragment
            (None, Some(empty), _) => CircularRange {
                start: empty.first_fragment(),
                end: empty.first_fragment(),
            },

            // No data in store and no empty blocks: continue after the highest existing block
            (None, None, Some(highest)) => {
                let to_be_erased = highest
                    .first_fragment()
                    .wrapping_add(highest.fragment_count() as u32);

                // Note: this range points to a block with obsolete data.
                // The first write attempt will trigger a format of the block
                CircularRange {
                    start: to_be_erased,
                    end: to_be_erased,
                }
            }

            // No valid blocks found at all: assume store is initialized for first time
            // Only happens if the store is just formatted and no data was ever written to it
            (None, None, None) => {
                log::debug!("Found empty store: assuming default range");

                CircularRange::default()
            }
        };
        Ok(range)
    }

    async fn read_circular_fragment_at_offset(
        &mut self,
        circular: C,
        frag_no: SeqNo,
        bytes: &mut [u8],
        offset: usize,
    ) -> Result<usize, Error> {
        let block = find_block_for_seq_no(&mut self.blockdev, &circular, frag_no)
            .await?
            .ok_or(Error::FragmentNotReadable)?;

        let n_read = self
            .blockdev
            .read_fragment_at_offset(&block, frag_no, offset, bytes)
            .await?;
        Ok(n_read)
    }
}

impl<BD, F, C, const BS: usize> WriteableCircularStore<BS, C> for Database<BD, BS, F, C>
where
    BD: block_layer::Writer<BS>,
    F: File<BS>,
    C: Circular<BS>,
{
    async fn initialize_writeable_circular_store(
        &mut self,
        auto_format: bool,
    ) -> Result<InitStatus, Error> {
        self.auto_format_circular = auto_format;
        self.initialize_circular_store().await?;

        // TODO if we implement a FAT, we could load it here / write it if invalid && auto_format

        // TODO check spare each circular for interrupted writes (spare block) or delete (obsolete flags on first block?)

        Ok(InitStatus::Existing)
    }

    async fn write_circular_fragment(
        &mut self,
        circular: C,
        frag_no: SeqNo,
        bytes: &[u8],
    ) -> Result<(), Error> {
        for attempt in 0..2 {
            let block_header =
                match find_block_for_seq_no(&mut self.blockdev, &circular, frag_no).await {
                    // Fragment not found in block: this means the block already stores a different (old) fragment
                    Err(FindFail::NotFound(block)) => {
                        // Block must be erased to be able to write data to it.
                        // Only erase if overwrite_on_full is enabled or no data in the block
                        if circular.overwrite_on_full()
                            || self
                                .blockdev
                                .block_find_valid_fragment(&block)
                                .await?
                                .is_none()
                        {
                            Ok(None)

                        // No overwrite allowed for this circular: write fails as no more space is available.
                        } else {
                            Err(Error::NoSpaceAvailable)
                        }
                    }

                    // Block is corrupt: if auto_format enabled, consider the block invalid (it will be formatted).
                    Err(FindFail::Corrupt(_block)) if self.auto_format_circular => Ok(None),

                    // Succes or other errors: propagate
                    result => result.map_err(|err| err.into()),
                }?;

            match (attempt, block_header) {
                // First attempt, no valid block: format
                (0, None) => {
                    let block_id = find_block_no_for_seq_no(&circular, frag_no);

                    let rel_frag_no = frag_no % circular.fragments_per_block() as u32;
                    if rel_frag_no != 0 {
                        // We are trying to format a block to write frag_no. This means that frag_no
                        // will be the first fragment in the block, i.e. aligned to a block boundary.
                        // If not, this must be a bug and/or corruption in flash!
                        // Refuse to erase + write the block as it causes data loss and the to-be-written
                        // segment won't be readable at invalid address anyways.
                        return Err(Error::FragmentNotWriteable);
                    }
                    let first_frag_no = frag_no;

                    // Object versioning not used for Circular.
                    // Data written before the version field existed has version 0xFF so this is a safe dummy value
                    let object_version = 0xFF;
                    self.blockdev
                        .format_block(
                            block_id,
                            circular.id(),
                            object_version,
                            circular.fragment_size() as u16,
                            first_frag_no,
                        )
                        .await?;
                }

                // valid block: OK
                (_, Some(block)) => {
                    self.blockdev.write_fragment(&block, frag_no, bytes).await?;
                    return Ok(());
                }

                // After formatting the block is still invalid: give up
                (_, None) => {
                    break;
                }
            }
        }
        Err(Error::FragmentNotWriteable)
    }

    async fn delete_circular_fragment(&mut self, circular: C, frag_no: SeqNo) -> Result<(), Error> {
        if let Some(block) = find_block_for_seq_no(&mut self.blockdev, &circular, frag_no).await? {
            self.blockdev.delete_fragment(&block, frag_no).await?;
        }
        Ok(())
    }
}

#[allow(unused)]
pub mod circular_perf {

    //! # Performance specifications for circular store
    //!
    //! The circular store must have bounded and predictable performance. The upper bounds are defined
    //! in a number of constants within this module.
    //!
    //! As an example, assuming the following parameters:
    //! - a circular with 65-byte fragments
    //! - SPI flash with 4K blocksize (=60 fragments/block), t_erase < 400ms, t_program < 2.5ms
    //!
    //! The expected performance will be (readback time is assumed neglegible):
    //!
    //! Maximum latency: `t_erase + (MAX_WRITES_PER_ERASE + MAX_WRITES_PER_WRITE) * t_program = 415ms`
    //!
    //! Average latency: `(t_erase + MAX_WRITES_PER_ERASE * t_program)/60 + (MAX_WRITES_PER_WRITE + MAX_WRITES_PER_DELETE) * t_program = 16.8ms (= 59 fragments/sec throughput)`
    //!

    /// Maximum flash-write overhead per block erase (flash_db has some overhead to guarantee atomic erase)
    pub const MAX_WRITES_PER_ERASE: u64 = 3;

    /// Maximum flash-write overhead per fragment written (flash_db has some overhead to guarantee atomic writes)
    pub const MAX_WRITES_PER_WRITE: u64 = 3;

    /// Maximum flash-write overhead per fragment deleted (flash_db delete is a soft-delete)
    pub const MAX_WRITES_PER_DELETE: u64 = 1;
}

#[cfg(test)]
mod tests {

    use super::{circular_perf as perf, *};
    use crate::{
        storage::{flash_db, flash_db::block_layer::BlockDevice},
        tests::mock::mock_flash::{self, MockFlash},
    };

    #[derive(Debug, Clone, Copy)]
    #[repr(u16)]
    enum TestCircular {
        One,
        Two,
        Three,
    }

    impl Object<4096> for TestCircular {
        fn id(&self) -> flash_db::ObjectId {
            *self as flash_db::ObjectId
        }

        fn fragment_size(&self) -> usize {
            32
        }

        fn flash_blocks(&self) -> core::ops::Range<block_layer::BlockId> {
            match self {
                TestCircular::One => 2..4,
                TestCircular::Two => 4..6,
                TestCircular::Three => 6..10,
            }
        }
    }
    impl Circular<4096> for TestCircular {
        fn overwrite_on_full(&self) -> bool {
            match self {
                TestCircular::One => false,
                TestCircular::Two => true,
                TestCircular::Three => true,
            }
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum NoFile {}
    impl Object<4096> for NoFile {
        fn id(&self) -> flash_db::ObjectId {
            0
        }

        fn fragment_size(&self) -> usize {
            0
        }

        fn flash_blocks(&self) -> core::ops::Range<block_layer::BlockId> {
            0..0
        }
    }
    impl File<4096> for NoFile {}

    type TestDB<'a> =
        Database<BlockDevice<&'a mut MockFlash<4096>, 4096>, 4096, NoFile, TestCircular>;

    #[tokio::test]
    async fn test_write_circular_short() {
        simple_logger::init_with_level(log::Level::Debug).ok();

        let mut flash = mock_flash::new::<4096>();
        let mut db: TestDB = Database::new(BlockDevice::writeable_from(&mut flash).unwrap());
        db.initialize_writeable_circular_store(false).await.unwrap();

        // No circular data available yet
        let range = db.find_circular_range(TestCircular::One).await.unwrap();
        assert_eq!(0, range.read_at());
        assert_eq!(0, range.len());
        assert_eq!(
            0,
            db.find_circular_range(TestCircular::Two)
                .await
                .unwrap()
                .len()
        );
        assert_eq!(
            0,
            db.find_circular_range(TestCircular::Three)
                .await
                .unwrap()
                .len()
        );

        let mut buffer = [0; 25];
        assert_eq!(
            Error::FragmentNotReadable,
            db.read_circular_fragment(TestCircular::One, range.read_at(), &mut buffer)
                .await
                .unwrap_err()
        );

        // Write,readback,delete first item
        {
            db.write_circular_fragment(TestCircular::One, range.write_at(), b"Hello world")
                .await
                .unwrap();
            let range = db.find_circular_range(TestCircular::One).await.unwrap();
            assert_eq!(1, range.len());

            // (trying to delete 'Two'should have no effect on 'One')
            db.delete_circular_fragment(TestCircular::Two, range.read_at())
                .await
                .unwrap();

            db.read_circular_fragment(TestCircular::One, range.read_at(), &mut buffer)
                .await
                .unwrap();
            assert_eq!(b"Hello world", &buffer[..11]);

            db.delete_circular_fragment(TestCircular::One, range.read_at())
                .await
                .unwrap();
        }

        // Write,readback, delete second item
        {
            db.write_circular_fragment(TestCircular::One, range.write_at() + 1, b"Test 123")
                .await
                .unwrap();

            let range = db.find_circular_range(TestCircular::One).await.unwrap();
            assert_eq!(1, range.read_at());
            assert_eq!(1, range.len());

            db.read_circular_fragment(TestCircular::One, range.read_at(), &mut buffer)
                .await
                .unwrap();
            assert_eq!(b"Test 123", &buffer[..8]);

            db.delete_circular_fragment(TestCircular::One, range.read_at())
                .await
                .unwrap();
            let range = db.find_circular_range(TestCircular::One).await.unwrap();
            assert_eq!(2, range.read_at());
            assert_eq!(0, range.len());
        }

        // Integrity check: no data outside 2..10 should have been accessed
        drop(db);
        assert!(&flash.memory[..4096 * 2].iter().all(|b| *b == 0xFF));
        assert!(&flash.memory[4096 * 10..4096 * 20]
            .iter()
            .all(|b| *b == 0xFF));
    }

    #[tokio::test]
    /// find_circular_range should result in a range where the write pointer points
    /// to a writeable fragment. See issue #316: no data in queue does not mean range (0,0)!
    async fn test_find_circular_range_obsolete() {
        simple_logger::init_with_level(log::Level::Debug).ok();

        let mut flash = mock_flash::new::<4096>();
        let mut db: TestDB = Database::new(BlockDevice::writeable_from(&mut flash).unwrap());
        db.initialize_writeable_circular_store(false).await.unwrap();

        // No circular data available yet
        let range = db.find_circular_range(TestCircular::One).await.unwrap();
        assert_eq!(0, range.read_at());
        assert_eq!(0, range.len());

        // write and delete exactly all fragments in one block
        let frags_per_block = TestCircular::One.fragment_count();
        for i in 0..frags_per_block {
            db.write_circular_fragment(TestCircular::One, i as SeqNo, b"Hello world")
                .await
                .unwrap();
            db.delete_circular_fragment(TestCircular::One, i as SeqNo)
                .await
                .unwrap();
        }

        // Still no circular data available yet. but range should not be (0,0) !
        let range = db.find_circular_range(TestCircular::One).await.unwrap();
        log::debug!("Found range {range:?}");
        assert_eq!(0, range.len());
        assert_eq!(frags_per_block as u32, range.write_at());
        assert_eq!(frags_per_block as u32, range.read_at());

        // Should be able to write a fragment at write pointer
        db.write_circular_fragment(TestCircular::One, range.write_at(), b"Hello world")
            .await
            .unwrap();
        // Range should now contain one item
        let range = db.find_circular_range(TestCircular::One).await.unwrap();
        assert_eq!(1, range.len());
    }

    #[tokio::test]
    async fn test_write_circular_full() {
        simple_logger::init_with_level(log::Level::Debug).ok();

        let mut flash = mock_flash::new::<4096>();
        let mut db: TestDB = Database::new(BlockDevice::writeable_from(&mut flash).unwrap());
        db.initialize_writeable_circular_store(false).await.unwrap();

        // No circular data available yet
        assert_eq!(
            0,
            db.find_circular_range(TestCircular::One)
                .await
                .unwrap()
                .len()
        );

        // Write untill full
        let n_frags = TestCircular::One.fragment_count();
        assert_eq!(116, n_frags);
        for i in 0..n_frags {
            db.write_circular_fragment(TestCircular::One, i as u32, b"Hello world")
                .await
                .unwrap();
        }

        // Write one more fragment: should fail (TestCircular::One is not overwrite-on-full)
        db.write_circular_fragment(TestCircular::One, n_frags as u32, b"Hello world")
            .await
            .unwrap_err();
        let _range = db.find_circular_range(TestCircular::One).await.unwrap();
    }

    #[tokio::test]
    async fn test_write_circular_large_seqno() {
        simple_logger::init_with_level(log::Level::Debug).ok();

        let stream = TestCircular::Three; // three has overwrite-on-full enabled
        let first_seq = 25 * (stream.fragment_count() as u32);

        let mut flash = mock_flash::new::<4096>();
        let mut db: TestDB = Database::new(BlockDevice::writeable_from(&mut flash).unwrap());
        db.initialize_writeable_circular_store(false).await.unwrap();

        // No circular data available yet
        assert_eq!(0, db.find_circular_range(stream).await.unwrap().len());

        // Write untill full
        let n_frags = stream.fragment_count();
        let frags_per_block = stream.fragments_per_block();
        assert_eq!(348, n_frags);
        assert_eq!(116, frags_per_block);

        let mut seq = first_seq;
        for _i in 0..(n_frags + frags_per_block - 1) {
            db.write_circular_fragment(stream, seq, b"Hello world")
                .await
                .unwrap();
            seq += 1;
        }
        let range = db.find_circular_range(stream).await.unwrap();
        log::debug!("Range (full): {range:?}");
        db.initialize_writeable_circular_store(false).await.unwrap();

        // Write one more fragment: should succeed by overwriting (TestCircular::Three is overwrite-on-full)
        db.write_circular_fragment(stream, seq, b"Hello world")
            .await
            .unwrap();
        seq += 1;

        let range = db.find_circular_range(stream).await.unwrap();
        log::debug!(
            "Range (1 overwritten): {range:?} ({} frags/block)",
            frags_per_block
        );
        // 1 block erased then 1 fragment written
        assert_eq!(n_frags, range.len());
        assert_eq!(first_seq + frags_per_block as u32, range.start);
        assert_eq!(seq, range.end);
    }

    #[tokio::test]
    async fn test_write_circular_overwrite_on_full() {
        simple_logger::init_with_level(log::Level::Debug).ok();

        let mut flash = mock_flash::new::<4096>();
        let mut db: TestDB = Database::new(BlockDevice::writeable_from(&mut flash).unwrap());
        db.initialize_writeable_circular_store(false).await.unwrap();

        // No circular data available yet
        assert_eq!(
            0,
            db.find_circular_range(TestCircular::Two)
                .await
                .unwrap()
                .len()
        );

        // Write untill full
        let n_frags = TestCircular::Two.fragment_count();
        assert_eq!(116, n_frags);
        db.write_circular_fragment(TestCircular::Two, 0, b"First element")
            .await
            .unwrap();
        for i in 1..n_frags {
            db.write_circular_fragment(TestCircular::Two, i as u32, b"Hello world")
                .await
                .unwrap();
        }

        let mut buffer = [0; 32];
        db.read_circular_fragment(TestCircular::Two, 0, &mut buffer)
            .await
            .unwrap();
        assert_eq!(b"First element", &buffer[..13]);

        // Write one more fragment: should overwrite oldest valuefail (TestCircular::Two is overwrite-on-full)
        db.write_circular_fragment(TestCircular::Two, n_frags as u32, b"Last element")
            .await
            .unwrap();
        let _range = db.find_circular_range(TestCircular::Two).await.unwrap();
    }

    #[tokio::test]
    async fn test_perf_write_circular_overwrite() {
        simple_logger::init_with_level(log::Level::Debug).ok();

        let mut flash = mock_flash::new::<4096>();
        let stream = TestCircular::Three;

        // 0. do nothing but initialize. So far we don't expect any writes with auto_format=false
        {
            let mut db: TestDB = Database::new(BlockDevice::writeable_from(&mut flash).unwrap());
            db.initialize_writeable_circular_store(false).await.unwrap();
        }
        assert_eq!(flash.write_count, 0);
        assert_eq!(flash.erase_count, 0);

        // reset stats before test
        let init_read_count = flash.read_count;
        flash.reset_stats();

        // 1. Write untill full
        let n_frags = stream.fragment_count();
        {
            assert_eq!(348, n_frags);
            let mut db: TestDB = Database::new(BlockDevice::writeable_from(&mut flash).unwrap());
            db.initialize_writeable_circular_store(false).await.unwrap();
            let write_no = db.find_circular_range(stream).await.unwrap().write_at();
            assert_eq!(0, write_no);
            db.write_circular_fragment(stream, 0, b"First element")
                .await
                .unwrap();
            for i in 1..n_frags {
                db.write_circular_fragment(stream, i as u32, b"Hello world")
                    .await
                    .unwrap();
            }
        }

        // Expected performance: at most 1 erase / block, write amplification up to MAX_WRITES_PER_WRITE
        assert!(flash.erase_count <= (n_frags / stream.fragments_per_block()) as u64);
        assert!(
            flash.write_count
                <= perf::MAX_WRITES_PER_WRITE * (n_frags as u64)
                    + perf::MAX_WRITES_PER_ERASE * flash.erase_count
        );
        assert!(flash.read_count.saturating_sub(init_read_count) <= flash.write_count);

        // reset stats before next test
        flash.reset_stats();
        {
            let mut db: TestDB = Database::new(BlockDevice::writeable_from(&mut flash).unwrap());
            db.initialize_writeable_circular_store(false).await.unwrap();
            let _ = db.find_circular_range(stream).await.unwrap().write_at();
        }
        let init_read_count = flash.read_count;
        flash.reset_stats();

        // 2. Keep circular-overwriting a couple of times
        let n_frags = 3 * stream.fragment_count();
        {
            let mut db: TestDB = Database::new(BlockDevice::writeable_from(&mut flash).unwrap());
            db.initialize_writeable_circular_store(false).await.unwrap();
            let write_no = db.find_circular_range(stream).await.unwrap().write_at();

            for i in 0..n_frags {
                log::debug!("Overwrite {i}");
                db.write_circular_fragment(stream, write_no + i as u32, b"Hello world")
                    .await
                    .unwrap();
            }
        }

        // Expect performance to stay consistent: at most 1 erase / block, write amplification up to MAX_WRITES_PER_WRITE
        assert!(flash.erase_count <= (n_frags / stream.fragments_per_block()) as u64);
        assert!(
            flash.write_count
                <= perf::MAX_WRITES_PER_WRITE * (n_frags as u64)
                    + perf::MAX_WRITES_PER_ERASE * flash.erase_count
        );
        assert!(flash.read_count.saturating_sub(init_read_count) <= flash.write_count);
    }

    #[tokio::test]
    async fn test_perf_write_circular_with_deletes() {
        simple_logger::init_with_level(log::Level::Debug).ok();

        let mut flash = mock_flash::new::<4096>();
        let stream = TestCircular::Three;

        // 0. do nothing but initialize. So far we don't expect any writes with auto_format=false
        {
            let mut db: TestDB = Database::new(BlockDevice::writeable_from(&mut flash).unwrap());
            db.initialize_writeable_circular_store(false).await.unwrap();
            let _ = db.find_circular_range(stream).await.unwrap().write_at();
        }
        assert_eq!(flash.write_count, 0);
        assert_eq!(flash.erase_count, 0);

        // reset stats before test
        let init_read_count = flash.read_count;
        flash.reset_stats();

        // 1. Write 3 * capacity through the stream
        let n_frags = stream.fragment_count() * 3;
        {
            let mut db: TestDB = Database::new(BlockDevice::writeable_from(&mut flash).unwrap());
            db.initialize_writeable_circular_store(false).await.unwrap();
            //let write_no = db.find_circular_range(stream).await.unwrap().write_at();
            //assert_eq!(0, write_no);
            for i in 0..n_frags {
                db.write_circular_fragment(stream, i as u32, b"Hello world")
                    .await
                    .unwrap();

                db.delete_circular_fragment(stream, i as u32).await.unwrap();
            }
        }

        // Expected performance: at most 1 erase / block, write amplification up to MAX_WRITES_PER_WRITE + 1 per delete
        assert!(flash.erase_count <= (n_frags / stream.fragments_per_block()) as u64);
        assert!(
            flash.write_count
                <= (perf::MAX_WRITES_PER_WRITE + perf::MAX_WRITES_PER_DELETE) * (n_frags as u64)
                    + perf::MAX_WRITES_PER_ERASE * flash.erase_count
        );
        assert!(
            flash.read_count.saturating_sub(init_read_count) <= flash.write_count + n_frags as u64
        );
    }
}

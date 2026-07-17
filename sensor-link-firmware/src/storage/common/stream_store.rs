//! Generic stream store implementation.
//!
//! Provides persistent stream storage with queue-like semantics for a single stream,
//! built on top of the circular storage layer. The StreamStore manages variable-length
//! data by implementing length-prefixed storage internally.
//!
//! # Features
//!
//! - **Queue-like semantics**: FIFO ordering with enqueue/peek/confirm operations
//! - **Variable-length data**: Supports data of any size up to fragment capacity
//! - **Persistence**: Data survives power cycles and system restarts
//! - **Atomic operations**: Peek without commit allows retry on failure
//! - **Concurrency control**: Limited concurrent peeks with automatic retry
//! - **Generic backend**: Works with any storage backend implementing the required traits
//!
//! # Usage
//!
//! ```rust,ignore
//! // Create a StreamStore for a specific stream type
//! let mut stream_store = StreamStore::new(database, &confirm_channel);
//!
//! // Enqueue data
//! let seq_no = stream_store.enqueue(MyStream::Data, b"hello world").await?;
//!
//! // Peek data (atomic read without commit)
//! let mut buffer = [0u8; 64];
//! if let Some((len, handle)) = stream_store.peek_next(MyStream::Data, &mut buffer).await? {
//!     // Process the data
//!     process_data(&buffer[..len]);
//!
//!     // Confirm successful processing (removes from queue)
//!     handle.confirm();
//! }
//! ```
//!
//! # Implementation Details
//!
//! The StreamStore uses length-prefixed storage to handle variable-length data:
//! - Each stored item consists of: `[2-byte length][TODO 4-byte CRC][variable data]`
//! - The underlying circular storage operates on fixed-size fragments
//! - Length prefix allows fast retrieval of the original data size (size excludes CRC).
//! - CRC is to be used used for extra data integrity robustness.

use crc::Crc;
use heapless::Vec;

use crate::storage::{
    common::queue::{ConfirmChannel, ConfirmHandle, Queue, SeqNo},
    flash_db::{Circular, Error, ObjectExt, WriteableCircularStore},
};

/// CRC32 checksum for data integrity
const CRC_OVERHEAD_BYTES: usize = 4;
const CRC: Crc<u32> = Crc::<u32>::new(&crc::CRC_32_CKSUM);

/// Length prefix (BlockHeader in BlockDevice supports 16-bit fragment size, assuming BLOCK_SIZE is large enough)
const LEN_OVERHEAD_BYTES: usize = 2;

/// Total overhead bytes for a stream fragment
pub const STREAM_OVERHEAD_BYTES: usize = LEN_OVERHEAD_BYTES + CRC_OVERHEAD_BYTES;

pub const MAX_PEEKS: usize = 5;

/// A persistent stream store providing queue-like semantics for a single stream.
///
/// `StreamStore` provides FIFO (first-in, first-out) access to variable-length data
/// stored persistently in a circular storage backend. It supports atomic peek operations
/// with explicit confirmation, allowing for robust error handling and retry logic.
///
/// # Type Parameters
///
/// - `'a`: Lifetime of the confirm channel
/// - `DB`: Database backend implementing [`WriteableCircularStore`]
/// - `C`: Stream type implementing [`Circular`] trait
/// - `BLOCK_SIZE`: Block size of the underlying storage system
///
/// # Storage Format
///
/// Data is stored with a 2-byte length prefix + 4-byte CRC, followed by the actual data:
/// ```text
/// [length: u16 LE][CRC: u32 LE][data: variable length]
/// ```
///
/// This allows the store to handle variable-length data within fixed-size storage fragments.
///
/// # Concurrency
///
/// The store supports up to [`MAX_PEEKS`] concurrent peek operations. If this limit is
/// exceeded, [`peek_next`](StreamStore::peek_next) will return `None` until some handles
/// are confirmed or dropped.
///
/// # Example
///
/// ```rust,ignore
/// use frogwatch_core::storage::common::stream_store::StreamStore;
///
/// // Create a stream store
/// let mut stream_store = StreamStore::new(database, &confirm_channel);
///
/// // Enqueue some data
/// let data = b"Hello, world!";
/// let seq_no = stream_store.enqueue(MyStream::Messages, data).await?;
///
/// // Peek the data (non-destructive read)
/// let mut buffer = [0u8; 64];
/// if let Some((len, handle)) = stream_store.peek_next(MyStream::Messages, &mut buffer).await? {
///     println!("Read {} bytes: {:?}", len, &buffer[..len]);
///
///     // Confirm processing (removes from queue)
///     handle.confirm();
/// }
/// ```
pub struct StreamStore<'a, DB, C, const BLOCK_SIZE: usize> {
    /// Database backend where the stream data is stored
    store: DB,

    /// Queue metadata cache: tracks available items and manages peek operations with retries
    queue: Queue<'a, MAX_PEEKS>,

    /// Flag indicating whether the store has been initialized for the stream
    init_done: bool,

    /// Circular instance where the stream is stored
    circular: C,
}

impl<'a, DB, C, const BLOCK_SIZE: usize> StreamStore<'a, DB, C, BLOCK_SIZE>
where
    DB: WriteableCircularStore<{ BLOCK_SIZE }, C>,
    C: Circular<{ BLOCK_SIZE }> + core::fmt::Debug,
{
    pub fn new(store: DB, circular: C, confirm_channel: &'a ConfirmChannel<MAX_PEEKS>) -> Self {
        let queue = Queue::with_overwrite_support(
            confirm_channel,
            circular.fragment_count(),
            circular.fragments_per_block(),
        );
        Self {
            store,
            queue,
            init_done: false,
            circular,
        }
    }

    pub async fn initialize(&mut self) -> Result<(), Error> {
        if !self.init_done {
            let auto_format = false;

            let stream = self.circular;
            log::info!(target: "StreamStore", "initialize({stream:?})");
            self.store
                .initialize_writeable_circular_store(auto_format)
                .await?;

            // Initialize queue metadata from database
            let range = self.store.find_circular_range(stream).await?;
            log::debug!(target: "StreamStore", "stream {stream:?} range {range:?}");
            self.queue
                .reinit_with_existing_range((range.read_at(), range.write_at()));
            self.init_done = true;
        }

        Ok(())
    }

    pub async fn peek_next(
        &mut self,
        read_buffer: &mut [u8],
    ) -> Result<Option<(usize, ConfirmHandle<'a, MAX_PEEKS>)>, Error> {
        self.initialize().await?;

        let queue = &mut self.queue;

        let mut oldest_fragment = queue.existing_range().0;

        // Find which object to read next (if any).
        // Note: as a side-effect this updates existing_range()
        let peek_result = queue.peek_next();
        let stream = self.circular;

        // Cleanup: delete stream items that have been confirmed since last peek
        // so they won't be repeated
        let range = queue.existing_range();
        while oldest_fragment != range.0 {
            match self
                .store
                .delete_circular_fragment(stream, oldest_fragment)
                .await
            {
                // delete succesful
                Ok(_) => {}

                // oldest fragment(s) already deleted / erased?
                // this may happen if circular overwrite happened
                Err(Error::FragmentNotFound) => {}

                // unexpected error
                Err(err) => return Err(err),
            }

            oldest_fragment = oldest_fragment.wrapping_add(1);
        }

        match peek_result {
            Ok(handle) => {
                let seq_no = handle.seq_no();

                // Read the header (length+CRC prefix) first to determine actual data size
                let mut header_buffer = [0u8; STREAM_OVERHEAD_BYTES];
                match self
                    .store
                    .read_circular_fragment(stream, seq_no, &mut header_buffer)
                    .await
                {
                    Ok(_) => {
                        // Extract length & CRC from prefix
                        let data_len = u16::from_le_bytes(
                            header_buffer[..LEN_OVERHEAD_BYTES].try_into().unwrap(),
                        ) as usize;
                        let data_crc = u32::from_le_bytes(
                            header_buffer[LEN_OVERHEAD_BYTES..].try_into().unwrap(),
                        );

                        // Ensure the read buffer is large enough
                        if read_buffer.len() < data_len {
                            log::error!(target: "StreamStore", "Stream {stream:?}: buffer too small for #{seq_no}: need {data_len} bytes, have {}", read_buffer.len());
                            handle.confirm();
                            return Err(Error::BufferTooSmall);
                        }

                        // Read the data fragment itself
                        match self
                            .store
                            .read_circular_fragment_at_offset(
                                stream,
                                seq_no,
                                &mut read_buffer[..data_len],
                                STREAM_OVERHEAD_BYTES,
                            )
                            .await
                        {
                            Ok(len) => {
                                // Expect the read data to match the expected length and CRC
                                if data_len != len
                                    || data_crc != CRC.checksum(&read_buffer[..data_len])
                                {
                                    log::error!(target: "StreamStore", "Stream {stream:?}: failed to read data for #{seq_no}: length or CRC mismatch");
                                    handle.confirm();
                                    return Err(Error::FragmentNotReadable);
                                }
                                return Ok(Some((len, handle)));
                            }
                            Err(error) => {
                                log::error!(target: "StreamStore", "Stream {stream:?}: failed to read data for #{seq_no}: {error:?}");
                                handle.confirm();
                                return Err(error);
                            }
                        }
                    }
                    Err(error) => match error {
                        // Error at the flash driver layer or misconfigured
                        // confirming and trying to make progress wont help here
                        Error::Flash | Error::InvalidObject => Err(error),

                        // The requested item could not be read. Keeping retrying to read the same item
                        // has low probability of success. confirm so it is skipped next time
                        read_error => {
                            log::error!(target: "StreamStore", "Stream {stream:?}: skip #{seq_no}: {read_error:?}");
                            handle.confirm();
                            Err(read_error)
                        }
                    },
                }
            }
            // TODO depending on use we may want to be able to await this?
            // Currently two scenarios can exist that can unblock peek_next:
            // - new data being enqueued (cant await that as both need &mut self)
            // - MAX_PEEKS handles are 'in flight' and at least one is completed/canceled
            Err(blocking) => {
                log::debug!(target: "StreamStore", "Stream {stream:?} blocks: {blocking:?}");
                Ok(None)
            }
        }
    }

    /// Enqueue data to the stream
    ///
    /// If succesfull, the data is stored persistently and can be dequeued even after a power cycle.
    pub async fn enqueue(&mut self, data: &[u8]) -> Result<SeqNo, Error> {
        self.initialize().await?;

        let queue = &self.queue;
        let seq_no = queue.writable_seq_no();
        let stream = self.circular;

        // Create length-prefixed data: [length: u16][crc32: u32][data: variable]
        let len = data.len();
        if len > stream.fragment_size().saturating_sub(STREAM_OVERHEAD_BYTES) {
            return Err(Error::FragmentTooLarge);
        }

        let data_len = len as u16;
        let crc32: u32 = CRC.checksum(data);

        // Note: this buffer is likely much too large, but we don't know the size of the data at compile time.
        // If this would become an issue, write_circular_fragment() should be changed to accept multiple slices
        // that it would iterate over and write in sequence (keeping the write atomic!)
        let mut prefixed_data: Vec<u8, { BLOCK_SIZE }> = Vec::new();
        prefixed_data
            .extend_from_slice(&data_len.to_le_bytes())
            .map_err(|_| Error::FragmentTooLarge)?;
        prefixed_data
            .extend_from_slice(&crc32.to_le_bytes())
            .map_err(|_| Error::FragmentTooLarge)?;
        prefixed_data
            .extend_from_slice(data)
            .map_err(|_| Error::FragmentTooLarge)?;

        let write_result = self
            .store
            .write_circular_fragment(stream, seq_no, &prefixed_data)
            .await;

        let queue = &mut self.queue;
        match write_result {
            // Write succes: mark as enqueued (increment write ptr)
            Ok(_) => {
                queue
                    .enqueue()
                    .map(|_| ())
                    .map_err(|_| Error::NoSpaceAvailable)?;
                Ok(seq_no)
            }

            // Write failed for this specific fragment (was it written before?).
            // (still mark as 'enqueued' so next enqueue will use different seq_no)
            Err(err @ Error::FragmentNotWriteable) | Err(err @ Error::FragmentExists) => {
                queue.enqueue().ok();
                Err(err)
            }

            // Write failed for other reason (next enqueue will re-attempt using the same seq_no)
            Err(other) => Err(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{
        backend::InMemoryFlash,
        common::queue::ConfirmChannel,
        flash_db::{block_layer::BlockDevice, Circular, Database, File, Object, ObjectExt},
    };

    #[derive(Debug, Clone, Copy)]
    #[repr(u16)]
    enum TestCircular {
        StreamOne,
    }

    impl Object<4096> for TestCircular {
        fn id(&self) -> crate::storage::flash_db::ObjectId {
            *self as crate::storage::flash_db::ObjectId
        }

        fn fragment_size(&self) -> usize {
            64
        }

        fn flash_blocks(&self) -> core::ops::Range<crate::storage::flash_db::block_layer::BlockId> {
            match self {
                TestCircular::StreamOne => 2..5, // 3 blocks
            }
        }
    }

    impl Circular<4096> for TestCircular {
        fn overwrite_on_full(&self) -> bool {
            true
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum NoFile {}

    impl Object<4096> for NoFile {
        fn id(&self) -> crate::storage::flash_db::ObjectId {
            0
        }

        fn fragment_size(&self) -> usize {
            0
        }

        fn flash_blocks(&self) -> core::ops::Range<crate::storage::flash_db::block_layer::BlockId> {
            0..0
        }
    }

    impl File<4096> for NoFile {}

    type TestDB<'a> =
        Database<BlockDevice<&'a mut InMemoryFlash<4096>, 4096>, 4096, NoFile, TestCircular>;

    async fn setup_test() -> (
        Box<InMemoryFlash<4096>>,
        TestDB<'static>,
        ConfirmChannel<MAX_PEEKS>,
    ) {
        let flash = Box::new(InMemoryFlash::<4096>::new(4 * 1024 * 1024));

        // SAFETY: We're leaking memory here for test purposes only.
        // In real code, the flash would have a proper lifetime.
        let flash_ref: &'static mut InMemoryFlash<4096> = Box::leak(flash);

        let mut db: TestDB = Database::new(BlockDevice::writeable_from(flash_ref).unwrap());
        db.initialize_writeable_circular_store(false).await.unwrap();

        let confirm_channel = ConfirmChannel::new();

        // Create a new flash instance to return (we can't return the leaked one)
        let return_flash = Box::new(InMemoryFlash::<4096>::new(4 * 1024 * 1024));
        (return_flash, db, confirm_channel)
    }

    #[tokio::test]
    async fn test_stream_store_basic_enqueue_peek() {
        simple_logger::init_with_level(log::Level::Debug).ok();

        let (_flash, db, confirm_channel) = setup_test().await;
        let mut stream_store = StreamStore::new(db, TestCircular::StreamOne, &confirm_channel);

        // Test enqueuing variable-length data
        let data = b"Hello, StreamStore!";
        let seq_no = stream_store.enqueue(data).await.unwrap();
        assert_eq!(0, seq_no);

        // Test peeking the data
        let mut buffer = [0u8; 64];
        let (len, handle) = stream_store.peek_next(&mut buffer).await.unwrap().unwrap();

        assert_eq!(data.len(), len);
        assert_eq!(data, &buffer[..len]);
        assert_eq!(0, handle.seq_no());

        // Confirm the read
        handle.confirm();

        // Should be empty now
        assert!(stream_store.peek_next(&mut buffer).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_stream_store_multiple_enqueue_peek() {
        simple_logger::init_with_level(log::Level::Debug).ok();

        let (_flash, db, confirm_channel) = setup_test().await;
        let mut stream_store = StreamStore::new(db, TestCircular::StreamOne, &confirm_channel);

        // Enqueue multiple items
        let data1 = b"First message";
        let data2 = b"Second message";
        let data3 = b"Third message";

        let seq1 = stream_store.enqueue(data1).await.unwrap();
        let seq2 = stream_store.enqueue(data2).await.unwrap();
        let seq3 = stream_store.enqueue(data3).await.unwrap();

        assert_eq!(0, seq1);
        assert_eq!(1, seq2);
        assert_eq!(2, seq3);

        // Peek and confirm each in order
        let mut buffer = [0u8; 64];

        // First message
        let (len, handle) = stream_store.peek_next(&mut buffer).await.unwrap().unwrap();
        assert_eq!(data1.len(), len);
        assert_eq!(data1, &buffer[..len]);
        assert_eq!(0, handle.seq_no());
        handle.confirm();

        // Second message
        let (len, handle) = stream_store.peek_next(&mut buffer).await.unwrap().unwrap();
        assert_eq!(data2.len(), len);
        assert_eq!(data2, &buffer[..len]);
        assert_eq!(1, handle.seq_no());
        handle.confirm();

        // Third message
        let (len, handle) = stream_store.peek_next(&mut buffer).await.unwrap().unwrap();
        assert_eq!(data3.len(), len);
        assert_eq!(data3, &buffer[..len]);
        assert_eq!(2, handle.seq_no());
        handle.confirm();

        // Should be empty now
        assert!(stream_store.peek_next(&mut buffer).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_stream_store_overwrite_full() {
        simple_logger::init_with_level(log::Level::Debug).ok();

        let (_flash, db, confirm_channel) = setup_test().await;
        let stream = TestCircular::StreamOne;
        log::debug!("Stream: {} frags/block", stream.fragments_per_block());
        let max_frag_count = stream.fragment_count();
        let mut stream_store = StreamStore::new(db, stream, &confirm_channel);

        // write store full + 1 extra message (which triggers overwrite on first few messages)
        log::debug!("=== Writing stream ===");
        for i in 0..(max_frag_count + 1) {
            let data = format!("Message {}", i);
            let data = data.as_bytes();

            let seq_no = stream_store.enqueue(data).await.unwrap();
            log::debug!("Enqueued message {}", i);
            assert_eq!(i as u32, seq_no);
        }
        log::debug!("=== Done writing stream ===");

        // Try to readback the data
        // Note: first block was erased by overwrite, so messages 0..stream.fragments_per_block() will be missing!
        log::debug!("=== Reading stream ===");
        for i in stream.fragments_per_block()..(max_frag_count + 1) {
            // Peek and confirm each in order
            let mut buffer = [0u8; 64];
            let (len, handle) = stream_store.peek_next(&mut buffer).await.unwrap().unwrap();
            let expected = format!("Message {}", i);
            assert_eq!(expected, str::from_utf8(&buffer[..len]).unwrap());
            assert_eq!(i as u32, handle.seq_no());
            log::debug!("Peeked message {}", i);
            handle.confirm();
        }
        log::debug!("=== Done reading stream ===");

        // Should be empty now
        {
            let mut buffer = [0u8; 64];
            assert!(stream_store.peek_next(&mut buffer).await.unwrap().is_none());
        }
    }

    #[tokio::test]
    /// same as test_stream_store_overwrite_full but with peeks active while the overwrite happens
    async fn test_stream_store_overwrite_while_peeking() {
        simple_logger::init_with_level(log::Level::Debug).ok();

        let (_flash, db, confirm_channel) = setup_test().await;
        let stream = TestCircular::StreamOne;
        log::debug!("Stream: {} frags/block", stream.fragments_per_block());
        let max_frag_count = stream.fragment_count();
        let mut stream_store = StreamStore::new(db, stream, &confirm_channel);

        let mut peeks = std::vec::Vec::new();

        // write store full + 1 extra message (which triggers overwrite on first few messages)
        log::debug!("=== Writing stream ===");
        for i in 0..(max_frag_count + 1) {
            let data = format!("Message {}", i);
            let data = data.as_bytes();

            let seq_no = stream_store.enqueue(data).await.unwrap();
            log::debug!("Enqueued message {}", i);
            assert_eq!(i as u32, seq_no);

            // peeks
            {
                // start 3 peeks into data that will be overwritten while the ConfirmHandles are still alive
                if seq_no < 3 {
                    let mut buffer = [0u8; 64];
                    if let Some((_len, handle)) = stream_store.peek_next(&mut buffer).await.unwrap()
                    {
                        peeks.push(handle);
                    }
                }
            }
        }
        log::debug!("=== Done writing stream ===");

        // peeks into overwritten data: confirm 2 and mark one for retry.
        // since the underlying data must be assumed to be erased,
        // this should not have any negative side-effect
        peeks.pop().unwrap().confirm();
        drop(peeks.pop().unwrap());
        peeks.pop().unwrap().confirm();
        assert!(peeks.is_empty());

        // Try to readback the data
        // Note: first block was erased by overwrite, so messages 0..stream.fragments_per_block() will be missing!
        log::debug!("=== Reading stream ===");
        for i in stream.fragments_per_block()..(max_frag_count + 1) {
            // Peek and confirm each in order
            let mut buffer = [0u8; 64];
            // all outstanding peeks have been confirmed and/or dropped, so this should not block!
            let (len, handle) = stream_store.peek_next(&mut buffer).await.unwrap().unwrap();
            let expected = format!("Message {}", i);
            assert_eq!(expected, str::from_utf8(&buffer[..len]).unwrap());
            assert_eq!(i as u32, handle.seq_no());
            log::debug!("Peeked message {}", i);
            handle.confirm();
        }
        log::debug!("=== Done reading stream ===");

        // Should be empty now
        {
            let mut buffer = [0u8; 64];
            assert!(stream_store.peek_next(&mut buffer).await.unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn test_stream_store_peek_retry_on_drop() {
        simple_logger::init_with_level(log::Level::Debug).ok();

        let (_flash, db, confirm_channel) = setup_test().await;
        let mut stream_store = StreamStore::new(db, TestCircular::StreamOne, &confirm_channel);

        let data = b"Retry test data";
        stream_store.enqueue(data).await.unwrap();

        let mut buffer = [0u8; 64];

        // Peek but don't confirm (drop the handle)
        {
            let (len, _handle) = stream_store.peek_next(&mut buffer).await.unwrap().unwrap();
            assert_eq!(data.len(), len);
            assert_eq!(data, &buffer[..len]);
            // handle is dropped here, so read should be retried
        }

        // Should be able to peek the same data again
        let (len, handle) = stream_store.peek_next(&mut buffer).await.unwrap().unwrap();
        assert_eq!(data.len(), len);
        assert_eq!(data, &buffer[..len]);
        assert_eq!(0, handle.seq_no());

        // This time confirm it
        handle.confirm();

        // Should be empty now
        assert!(stream_store.peek_next(&mut buffer).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_stream_store_max_peeks() {
        simple_logger::init_with_level(log::Level::Debug).ok();

        let (_flash, db, confirm_channel) = setup_test().await;
        let mut stream_store = StreamStore::new(db, TestCircular::StreamOne, &confirm_channel);

        // Enqueue more items than MAX_PEEKS
        for i in 0..(MAX_PEEKS + 2) {
            let data = format!("Message {}", i);
            stream_store.enqueue(data.as_bytes()).await.unwrap();
        }

        let mut buffer = [0u8; 64];
        let mut handles: heapless::Vec<_, { MAX_PEEKS + 2 }> = Vec::new();

        // Peek up to MAX_PEEKS items without confirming
        for i in 0..MAX_PEEKS {
            let (len, handle) = stream_store.peek_next(&mut buffer).await.unwrap().unwrap();

            let expected_data = format!("Message {}", i);
            assert_eq!(expected_data.len(), len);
            assert_eq!(expected_data.as_bytes(), &buffer[..len]);
            assert_eq!(i as u32, handle.seq_no());
            handles.push(handle).unwrap();
        }

        // Next peek should return None (blocked)
        assert!(stream_store.peek_next(&mut buffer).await.unwrap().is_none());

        // Confirm first handle to unblock
        handles.remove(0).confirm();

        // Now should be able to peek the next item
        let (len, handle) = stream_store.peek_next(&mut buffer).await.unwrap().unwrap();

        let expected_data = format!("Message {}", MAX_PEEKS);
        assert_eq!(expected_data.len(), len);
        assert_eq!(expected_data.as_bytes(), &buffer[..len]);
        assert_eq!(MAX_PEEKS as u32, handle.seq_no());

        // Clean up remaining handles
        handle.confirm();
        for h in handles {
            h.confirm();
        }
    }

    #[tokio::test]
    async fn test_stream_store_large_data() {
        simple_logger::init_with_level(log::Level::Debug).ok();

        let (_flash, db, confirm_channel) = setup_test().await;
        let mut stream_store = StreamStore::new(db, TestCircular::StreamOne, &confirm_channel);

        // Test with data that fills the fragment
        let large_data = vec![0xAB; 64 - STREAM_OVERHEAD_BYTES];

        stream_store.enqueue(&large_data).await.unwrap();

        let mut buffer = [0u8; 64];
        let (len, handle) = stream_store.peek_next(&mut buffer).await.unwrap().unwrap();

        assert_eq!(large_data.len(), len);
        assert_eq!(large_data.as_slice(), &buffer[..len]);
        handle.confirm();
    }

    #[tokio::test]
    async fn test_stream_store_empty_data() {
        simple_logger::init_with_level(log::Level::Debug).ok();

        let (_flash, db, confirm_channel) = setup_test().await;
        let mut stream_store = StreamStore::new(db, TestCircular::StreamOne, &confirm_channel);

        // Test with empty data
        let empty_data = b"";
        stream_store.enqueue(empty_data).await.unwrap();

        let mut buffer = [0u8; 64];
        let (len, handle) = stream_store.peek_next(&mut buffer).await.unwrap().unwrap();

        assert_eq!(0, len);
        handle.confirm();
    }
}

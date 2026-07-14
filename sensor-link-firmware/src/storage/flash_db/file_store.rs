use super::{
    block_layer, Circular, Database, DeleteMode, Error, InitStatus, LockMode, Object, ObjectExt,
};

/// File trait: all files you want to store in the [Database] must implement this trait
///
/// These can be read from [FileStore] and written to [WriteableFileStore].
///
/// Implementers must be careful that all files has:
/// - an id that is at least unique-per-file
/// - flash block range must be globally unique (cannot overlap any other objects)
pub trait File<const BLOCK_SIZE: usize>: Object<BLOCK_SIZE> {}

/// Relative index of the file fragment (relative to start-of-file)
pub type FragIndex = u32;

#[derive(Debug)]
pub struct FileHandle<const BLOCK_SIZE: usize, F: File<BLOCK_SIZE>> {
    file: F,
    version: u8,
}

impl<const BLOCK_SIZE: usize, F: File<BLOCK_SIZE>> FileHandle<BLOCK_SIZE, F> {
    pub fn file(&self) -> &F {
        &self.file
    }

    pub fn object_version(&self) -> u8 {
        self.version
    }

    /// Only intended for use by implmentors of [FileStore]
    pub fn from_file_and_version(file: F, version: u8) -> Self {
        Self { file, version }
    }
}

#[derive(Debug)]
pub struct WriteableFileHandle<const BLOCK_SIZE: usize, F: File<BLOCK_SIZE>> {
    handle: FileHandle<BLOCK_SIZE, F>,
}

impl<const BLOCK_SIZE: usize, F: File<BLOCK_SIZE>> WriteableFileHandle<BLOCK_SIZE, F> {
    /// Only intended for use by implementors of [WriteableFileStore]
    pub fn from_handle(handle: FileHandle<BLOCK_SIZE, F>) -> Self {
        Self { handle }
    }
}

impl<const BLOCK_SIZE: usize, F: File<BLOCK_SIZE>> WriteableFileHandle<BLOCK_SIZE, F> {
    pub fn file(&self) -> &F {
        self.handle.file()
    }

    pub fn object_version(&self) -> u8 {
        self.handle.object_version()
    }
}

/// Read-only filesystem for storing [File]s
pub trait FileStore<const BLOCK_SIZE: usize, F: File<BLOCK_SIZE>> {
    /// Initialize the store
    ///
    /// Required for correct operation. Other methods may fail if not initialized first.
    async fn initialize(&mut self) -> Result<(), Error>;

    /// Read (part of) the whole file (data may exceed fragment length)
    ///
    /// Tries to fill the result buffer with data from the file.
    /// Result buffer may be larger or smaller than the file capacity.
    /// Returns the actual amount of data read.
    async fn read_file(&mut self, file: F, bytes: &mut [u8]) -> Result<usize, Error>;

    /// Get a file handle for use with [read_file_fragment](method@Self::read_file_fragment) or [write_file_fragment](method@WriteableFileStore::write_file_fragment)
    ///
    /// fail-safe guarantee: if locked (write or erase process is in progres / aborted)
    /// no valid file handle can be obtained
    async fn file_handle(&mut self, file: F) -> Result<FileHandle<BLOCK_SIZE, F>, Error>;

    /// Read (part of) a specific file fragment
    ///
    /// Tries to fill the result buffer with data from the file fragment.
    /// Result buffer may be larger or smaller than the fragment size.
    /// Returns the actual amount of data read.
    /// See [file_handle](method@FileStore::file_handle)
    async fn read_file_fragment(
        &mut self,
        filehandle: &FileHandle<BLOCK_SIZE, F>,
        fragment_no: FragIndex,
        bytes: &mut [u8],
    ) -> Result<usize, Error>;
}

/// Writeable filesystem for storing [File]s
pub trait WriteableFileStore<const BLOCK_SIZE: usize, F: File<BLOCK_SIZE>>:
    FileStore<BLOCK_SIZE, F>
{
    /// Initialize the store
    ///
    /// Required for correct operation. Other methods may fail if not initialized first.
    async fn initialize_writeable(&mut self, auto_format: bool) -> Result<InitStatus, Error>;

    /// # Overwrite the file with new data
    ///
    /// Writes to file with the following fail-safe guarantees:
    /// - readback via [read_file()](method@FileStore::read_file):
    ///   This will always observe either the previous version, empty file or the new version
    ///   of the file as a whole. Even if the write was interrupted or device power cycled.
    ///
    /// - readback via [read_file_fragment()](method@FileStore::read_file_fragment):
    ///   This only offers fail-safety at the fragment level
    ///   (e.g. it may observe a mix of old and new fragments
    ///   but the content within a fragment is always consistent)
    ///
    /// *This erases all non-erased blocks in the file, which may be slow.
    /// If this is a bottle-neck, consider pre-erasing the file, see [delete()](method@Self::delete)*
    async fn write_file(&mut self, file: F, bytes: &[u8]) -> Result<(), Error>;

    /// # Write data to a file fragment
    ///
    /// Writes data to a fragment if the fragment is empty.
    /// (each fragment can only be written once). To re-write a fragment,
    /// delete the file first. See [delete()](method@Self::delete).
    ///
    /// writing is fail-safe: the resulting block is always either empty, invalid, or contains the new data.
    ///
    /// *Always delete the file first before starting to write a new version
    /// to be sure all fragments are empty*
    ///
    /// See [file_handle](method@FileStore::file_handle)
    async fn write_file_fragment(
        &mut self,
        filehandle: &WriteableFileHandle<BLOCK_SIZE, F>,
        fragment_no: FragIndex,
        bytes: &[u8],
    ) -> Result<(), Error>;

    /// Delete the file
    ///
    /// If succesfull, the file can be re-written again via [write_file()](method@Self::write_file) or [write_file_fragment()](method@Self::write_file_fragment).
    ///
    /// *This erases all non-erased blocks in the file. This is usually quite a slow process, especially for large files*
    async fn delete(&mut self, file: F) -> Result<WriteableFileHandle<BLOCK_SIZE, F>, Error>;
}

impl<BD, F, C, const BS: usize> FileStore<BS, F> for Database<BD, BS, F, C>
where
    BD: block_layer::Reader<BS>,
    F: File<BS>,
    C: Circular<BS>,
{
    async fn initialize(&mut self) -> Result<(), Error> {
        // TODO if we implement a FAT, we could load it here
        Ok(())
    }

    async fn read_file(&mut self, file: F, bytes: &mut [u8]) -> Result<usize, Error> {
        let frag_size = file.fragment_size();
        let mut bytes_read = 0;

        // fail-safe guarantee: file handle only succeeds if the file is in a sane (non-locked) state
        let file_handle = self.file_handle(file).await?;

        // Limit to total file capacity
        let bytes = {
            let len = bytes.len();
            &mut bytes[..len.min(file.capacity())]
        };

        // read as many fragments as needed to fill the result buffer.
        //
        // there is some room for optimization as all chunks in a block are stored sequentially
        // but the performance difference is probably not worth the complexity
        let max_n_frags = file.fragment_count();
        for (frag_index, chunk) in bytes.chunks_mut(frag_size).enumerate() {
            // Don't try to read past end-of-file
            if frag_index >= max_n_frags {
                break;
            }
            let n = match self
                .read_file_fragment(&file_handle, frag_index as u32, chunk)
                .await
            {
                // Data written in the file may be smaller than the read buffer,
                // so empty fragments may exist. In this case we return bytes_read < bytes.len()
                Err(Error::FragmentNotReadable) => {
                    if bytes_read > 0 {
                        Ok(0)
                    } else {
                        Err(Error::FileNotReadable)
                    }
                }

                other => other,
            }?;

            bytes_read += n;

            // In case of empty fragment: don't read any further
            if n != chunk.len() {
                break;
            }
        }
        Ok(bytes_read)
    }

    async fn file_handle(&mut self, file: F) -> Result<FileHandle<BS, F>, Error> {
        match self.lock_status(&file, LockMode::LockSpare).await? {
            super::LockStatus::Unlocked(version) => {
                Ok(FileHandle::from_file_and_version(file, version))
            }
            super::LockStatus::Locked => Err(Error::FileNotReadable),
        }
    }

    async fn read_file_fragment(
        &mut self,
        filehandle: &FileHandle<BS, F>,
        fragment_index: FragIndex,
        bytes: &mut [u8],
    ) -> Result<usize, Error> {
        let file = &filehandle.file;
        if fragment_index >= file.fragment_count() as u32 {
            return Err(Error::NoSpaceAvailable);
        }

        // Limit to total object capacity (in case it is not fragment-aligned)
        let bytes = {
            let len = bytes.len();
            let max_capacity_remaining =
                file.capacity() - fragment_index as usize * file.fragment_size();
            &mut bytes[..len.min(max_capacity_remaining)]
        };

        let block_no =
            file.flash_blocks().start + (fragment_index / file.fragments_per_block() as u32);
        let fragment_in_block = fragment_index % file.fragments_per_block() as u32;
        let block = self
            .blockdev
            .block_header(block_no)
            .await?
            .ok_or(Error::FragmentNotReadable)?;

        // block exists, but for a previous version of the file.
        // This is effectively the same as if the block is not valid
        if block.object_version() != filehandle.version {
            return Err(Error::FragmentNotReadable);
        }
        let n_read = self
            .blockdev
            .read_fragment(&block, block.first_fragment() + fragment_in_block, bytes)
            .await?;
        Ok(n_read)
    }
}

impl<BD, F, C, const BS: usize> WriteableFileStore<BS, F> for Database<BD, BS, F, C>
where
    BD: block_layer::Writer<BS>,
    F: File<BS>,
    C: Circular<BS>,
{
    async fn initialize_writeable(&mut self, _auto_format: bool) -> Result<InitStatus, Error> {
        self.initialize().await?;

        // TODO if we implement a FAT, we could load it here / write it if invalid && auto_format

        // TODO check spare each file for interrupted writes (spare block) or delete (obsolete flags on first block?)

        Ok(InitStatus::Existing)
    }

    async fn write_file(&mut self, file: F, bytes: &[u8]) -> Result<(), Error> {
        // Check if this will actually fit
        if bytes.len() > file.capacity() {
            return Err(Error::NoSpaceAvailable);
        }

        // Lock file for writing & erase whole file
        self.lock(&file, LockMode::LockSpare).await?;
        let w_handle = self.delete(file).await?;

        let frag_size = file.fragment_size();

        // there is some room for optimization as all chunks in a block are stored sequentially.
        // For files with multiple small (< 256 byte) segments this could be a decent performance boost
        // (although it is probably not common to write such a file in one go)
        let max_n_frags = file.fragment_count();
        for (frag_index, chunk) in bytes.chunks(frag_size).enumerate() {
            // Don't try to write past end-of-file
            if frag_index >= max_n_frags {
                return Err(Error::NoSpaceAvailable);
            }

            self.write_file_fragment(&w_handle, frag_index as u32, chunk)
                .await?;
        }

        // Erase the spare block to release the file 'lock'
        self.unlock(&file, LockMode::LockSpare, w_handle.object_version())
            .await?;

        Ok(())
    }

    async fn write_file_fragment(
        &mut self,
        filehandle: &WriteableFileHandle<BS, F>,
        fragment_index: FragIndex,
        bytes: &[u8],
    ) -> Result<(), Error> {
        let file = filehandle.file();

        if fragment_index >= file.fragment_count() as u32 {
            return Err(Error::NoSpaceAvailable);
        }

        // Limit to total object capacity (in case it is not fragment-aligned)
        {
            let max_capacity_remaining =
                file.capacity() - fragment_index as usize * file.fragment_size();
            if bytes.len() > max_capacity_remaining {
                return Err(Error::NoSpaceAvailable);
            }
        }

        let block_no =
            file.flash_blocks().start + (fragment_index / file.fragments_per_block() as u32);

        let block = self
            .prepare_writeable_block(file, block_no, filehandle.object_version())
            .await?;

        let fragment_in_block = fragment_index % file.fragments_per_block() as u32;
        self.blockdev
            .write_fragment(&block, block.first_fragment() + fragment_in_block, bytes)
            .await?;
        Ok(())
    }

    async fn delete(&mut self, file: F) -> Result<WriteableFileHandle<BS, F>, Error> {
        self.delete_object(&file, DeleteMode::SoftVersion).await?;

        Ok(WriteableFileHandle {
            handle: self.file_handle(file).await?,
        })
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::{
        storage::flash_db::block_layer::BlockDevice,
        tests::mock::mock_flash::{self, MockFlash},
    };

    #[derive(Debug, Clone, Copy)]
    #[repr(u16)]
    enum TestFile {
        One,
        Two,
        Three,
    }

    impl Object<4096> for TestFile {
        fn id(&self) -> crate::storage::flash_db::ObjectId {
            *self as crate::storage::flash_db::ObjectId
        }

        fn fragment_size(&self) -> usize {
            32
        }

        fn flash_blocks(&self) -> core::ops::Range<block_layer::BlockId> {
            match self {
                TestFile::One => 11..13,
                TestFile::Two => 13..15,
                TestFile::Three => 15..19,
            }
        }
    }
    impl File<4096> for TestFile {}

    #[derive(Debug, Clone, Copy)]
    enum NoCircular {}
    impl Object<4096> for NoCircular {
        fn id(&self) -> crate::storage::flash_db::ObjectId {
            0
        }

        fn fragment_size(&self) -> usize {
            0
        }

        fn flash_blocks(&self) -> core::ops::Range<block_layer::BlockId> {
            0..0
        }
    }
    impl Circular<4096> for NoCircular {
        fn overwrite_on_full(&self) -> bool {
            false
        }
    }
    type TestDB<'a> =
        Database<BlockDevice<&'a mut MockFlash<4096>, 4096>, 4096, TestFile, NoCircular>;

    #[tokio::test]
    async fn test_write_file_short() {
        let mut flash = mock_flash::new::<4096>();
        let mut db: TestDB = Database::new(BlockDevice::writeable_from(&mut flash).unwrap());
        db.initialize_writeable(false).await.unwrap();

        // No files written yet: all should report as not readable
        let mut buffer = [0; 25];
        assert_eq!(
            Error::FileNotReadable,
            db.read_file(TestFile::One, &mut buffer).await.unwrap_err()
        );
        assert_eq!(
            Error::FileNotReadable,
            db.read_file(TestFile::Two, &mut buffer).await.unwrap_err()
        );
        assert_eq!(
            Error::FileNotReadable,
            db.read_file(TestFile::Three, &mut buffer)
                .await
                .unwrap_err()
        );

        // Write a small file and verify
        db.write_file(TestFile::One, b"hello world!").await.unwrap();
        let len = db.read_file(TestFile::One, &mut buffer).await.unwrap();
        assert_eq!(&buffer[..12], b"hello world!");
        assert!(buffer[12..len].iter().all(|b| *b == 0xFF));

        db.delete(TestFile::Two).await.unwrap();

        // Verify again after deleting an unrelated file
        let len = db.read_file(TestFile::One, &mut buffer).await.unwrap();
        assert_eq!(&buffer[..12], b"hello world!");
        assert!(buffer[12..len].iter().all(|b| *b == 0xFF));

        db.delete(TestFile::One).await.unwrap();

        // Verify yet again after deleting the file
        assert_eq!(
            Error::FileNotReadable,
            db.read_file(TestFile::One, &mut buffer).await.unwrap_err()
        );

        // Integrity check: no data outside 11..19 should have been accessed
        drop(db);
        assert!(&flash.memory[..4096 * 11].iter().all(|b| *b == 0xFF));
        assert!(&flash.memory[4096 * 19..4096 * 20]
            .iter()
            .all(|b| *b == 0xFF));
    }

    #[tokio::test]
    async fn test_write_file_long() {
        let mut flash = mock_flash::new::<4096>();
        let mut db: TestDB = Database::new(BlockDevice::writeable_from(&mut flash).unwrap());
        db.initialize_writeable(false).await.unwrap();

        // testdata: repeating pattern (that intentionally does not repeat at block bounds)
        let testdata = {
            let mut testdata = [0_u8; 10_000];
            for (i, byte) in testdata.iter_mut().enumerate() {
                *byte = (i % 127) as u8;
            }
            testdata
        };

        // Large enough file: should write + readback succesfully
        assert!(TestFile::Three.capacity() > testdata.len());
        db.write_file(TestFile::Three, &testdata).await.unwrap();
        let mut buffer = [0; 10_000];
        let len = db.read_file(TestFile::Three, &mut buffer).await.unwrap();
        assert_eq!(len, buffer.len());
        assert_eq!(&buffer, &testdata);

        // Too small file: should fail to write
        assert!(TestFile::One.capacity() < 10_000);
        db.write_file(TestFile::One, &testdata).await.unwrap_err();

        assert_eq!(
            Error::FileNotReadable,
            db.read_file(TestFile::One, &mut buffer).await.unwrap_err()
        );
        assert_eq!(
            Error::FileNotReadable,
            db.read_file(TestFile::Two, &mut buffer).await.unwrap_err()
        );

        // Integrity check: no data outside 11..19 should have been accessed
        drop(db);
        assert!(&flash.memory[..4096 * 11].iter().all(|b| *b == 0xFF));
        assert!(&flash.memory[4096 * 19..4096 * 20]
            .iter()
            .all(|b| *b == 0xFF));
    }

    #[tokio::test]
    async fn test_write_file_fragmented() {
        let mut flash = mock_flash::new::<4096>();
        let mut db: TestDB = Database::new(BlockDevice::writeable_from(&mut flash).unwrap());
        db.initialize_writeable(false).await.unwrap();

        // testdata: four full-sized fragments
        let testdata = {
            [
                b"this is the first 32-byte frag !",
                b"fragment number two: also 32byte",
                b"frag three: hello world 01234567",
                b"abcdefghijklmnopqrstuvwxyzABCDEF",
            ]
        };
        assert!(TestFile::One.capacity() > testdata.len());

        let mut buffer = [0; 64];

        // Data never written: can't even get a handle
        assert_eq!(
            Error::FileNotReadable,
            db.file_handle(TestFile::One).await.unwrap_err()
        );

        let write_file_one = db.delete(TestFile::One).await.unwrap();

        // File freshly deleted: fragment should be empty
        let file_one = db.file_handle(TestFile::One).await.unwrap();
        assert_eq!(
            Error::FragmentNotReadable,
            db.read_file_fragment(&file_one, 0, &mut buffer)
                .await
                .unwrap_err()
        );

        // Write fragments in 'random' order
        db.write_file_fragment(&write_file_one, 2, testdata[2])
            .await
            .unwrap();
        db.write_file_fragment(&write_file_one, 0, testdata[0])
            .await
            .unwrap();
        db.write_file_fragment(&write_file_one, 1, testdata[1])
            .await
            .unwrap();
        db.write_file_fragment(&write_file_one, 3, testdata[3])
            .await
            .unwrap();

        // Verify fragments
        for i in 0..4 {
            let len = db
                .read_file_fragment(&file_one, i as u32, &mut buffer)
                .await
                .unwrap();
            assert_eq!(&buffer[..len], testdata[i]);
        }

        // This fragment was never written
        assert_eq!(
            Error::FragmentNotReadable,
            db.read_file_fragment(&file_one, 4, &mut buffer)
                .await
                .unwrap_err()
        );

        // Integrity check: no data outside 11..19 should have been accessed
        drop(db);
        assert!(&flash.memory[..4096 * 11].iter().all(|b| *b == 0xFF));
        assert!(&flash.memory[4096 * 19..4096 * 20]
            .iter()
            .all(|b| *b == 0xFF));
    }

    #[tokio::test]
    /// Test writing a small amount of data, readback with much larger buffer: empty fragments should not be an error
    async fn test_read_file_large_buffer() {
        let _ = simple_logger::init();

        let mut flash = mock_flash::new::<4096>();
        let mut db: TestDB = Database::new(BlockDevice::writeable_from(&mut flash).unwrap());
        db.initialize_writeable(false).await.unwrap();

        // Write a small amount of data to file
        db.write_file(TestFile::One, b"hello world!").await.unwrap();

        // Readback using buffer that is too large (larger than fragment size). DB must handle reading nonexisting fragments correctly
        let mut buffer = [0; 100];
        assert!(TestFile::One.fragment_size() < buffer.len());

        let len = db.read_file(TestFile::One, &mut buffer).await.unwrap();
        assert_eq!(&buffer[..12], b"hello world!");
        assert!(buffer[12..len].iter().all(|b| *b == 0xFF));

        db.delete(TestFile::Two).await.unwrap();
    }

    /// Maximum flash-write overhead per erase (flash_db has some overhead to guarantee atomic erase)
    const PERF_MAX_WRITES_PER_ERASE: u64 = 3;

    /// Maximum flash-write overhead per fragment written (flash_db has some overhead to guarantee atomic writes)
    const PERF_MAX_WRITES_PER_WRITE: u64 = 3;

    #[tokio::test]
    async fn test_perf_write_small_file() {
        let mut flash = mock_flash::new::<4096>();

        // 0. do nothing but initialize. So far we don't expect any writes with auto_format=false
        {
            let mut db: TestDB = Database::new(BlockDevice::writeable_from(&mut flash).unwrap());
            db.initialize_writeable(false).await.unwrap();
        }
        assert_eq!(flash.write_count, 0);
        assert_eq!(flash.erase_count, 0);

        // 1. write small (single fragment) file: expect 2 erases + 1 fragment write
        flash.reset_stats();
        {
            let mut db: TestDB = Database::new(BlockDevice::writeable_from(&mut flash).unwrap());
            db.initialize_writeable(false).await.unwrap();

            // Write a small file (data fits in one fragment)
            db.write_file(TestFile::One, b"hello world!").await.unwrap();
        }
        assert!(flash.erase_count <= 2);
        assert!(flash.write_count <= 2 * PERF_MAX_WRITES_PER_ERASE + 1 * PERF_MAX_WRITES_PER_WRITE);

        // 2. delete the file: expect at most 2 erases
        flash.reset_stats();
        {
            let mut db: TestDB = Database::new(BlockDevice::writeable_from(&mut flash).unwrap());
            db.initialize_writeable(false).await.unwrap();
            db.delete(TestFile::One).await.unwrap();
        }
        assert!(flash.erase_count <= 2);
        assert!(flash.write_count == 2 * PERF_MAX_WRITES_PER_ERASE);
    }

    #[tokio::test]
    /// Tests the performance and correctness of deleting large file written / read in fragments
    async fn test_perf_fragmented_delete() {
        simple_logger::init_with_level(log::Level::Debug).ok();
        let mut flash = mock_flash::new::<4096>();

        // 1. write large file (N fragments spanning multiple blocks): expect one erase per block + N fragment writes
        let n_fragments = TestFile::Three.fragment_count() as u64;
        let n_blocks = TestFile::Three.block_count() as u64;

        log::debug!(
            "{n_fragments} frags in {n_blocks} blocks: {} frags/block",
            TestFile::Three.fragments_per_block()
        );

        // File needs to be 'large enough' to validate scaling of delete performance
        assert!(n_blocks >= 4);

        flash.reset_stats();
        {
            let mut db: TestDB = Database::new(BlockDevice::writeable_from(&mut flash).unwrap());
            db.initialize_writeable(false).await.unwrap();

            let file3_writer = db.delete(TestFile::Three).await.unwrap();
            log::debug!("initial write handle: {file3_writer:?}");

            for frag_no in 0..n_fragments as u32 {
                db.write_file_fragment(&file3_writer, frag_no, b"Hello World")
                    .await
                    .unwrap();
            }
        }
        assert_eq!(flash.erase_count, n_blocks);
        assert_eq!(
            flash.write_count,
            n_blocks * PERF_MAX_WRITES_PER_ERASE + n_fragments * PERF_MAX_WRITES_PER_WRITE
        );

        // 2. delete the file: expect at most 2 erases (even though more than 2 blocks have been 'deleted')
        flash.reset_stats();
        {
            let mut db: TestDB = Database::new(BlockDevice::writeable_from(&mut flash).unwrap());
            db.initialize_writeable(false).await.unwrap();
            let deleted_handle = db.delete(TestFile::Three).await.unwrap();
            log::debug!("write handle after delete: {deleted_handle:?}");
        }
        assert!(flash.erase_count <= 2);
        assert!(flash.write_count == 2 * PERF_MAX_WRITES_PER_ERASE);

        // 3. verify no file contents remains (this should not cause any erase or write activity).
        flash.reset_stats();
        {
            let mut db: TestDB = Database::new(BlockDevice::writeable_from(&mut flash).unwrap());
            db.initialize_writeable(false).await.unwrap();

            // file should be deleted: either we don't even get a file handle or we get a handle but all reads should fail
            if let Ok(handle) = db.file_handle(TestFile::Three).await {
                log::debug!("readback got handle: {handle:?}");
                for frag_no in 0..n_fragments as u32 {
                    let mut buffer = [0; 100];
                    let readback_result =
                        db.read_file_fragment(&handle, frag_no, &mut buffer).await;
                    match readback_result {
                        Err(Error::FileNotReadable) | Err(Error::FragmentNotReadable) => {
                            log::debug!("readback deleted frag {frag_no}: '{readback_result:?}'");
                        }
                        _ => {
                            panic!("Unexpected result for reading deleted frag {frag_no}: '{readback_result:?}'");
                        }
                    }
                }
            }
        }
        assert_eq!(flash.erase_count, 0);
        assert_eq!(flash.write_count, 0);
    }
}

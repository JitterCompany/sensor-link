use crate::storage::flash_db::{self, File, FileStore, ObjectId, WriteableFileHandle};

/// Mock file store. Only allows a single file to be stored.
///
/// Generic over the flash block size, so any consumer can drive it with its own
/// `BLOCK_SIZE`.
pub struct MockSingleFileStore {
    pub obj_id: Option<ObjectId>,
    pub buffer: Vec<u8>,
}

impl MockSingleFileStore {
    pub fn new() -> Self {
        Self {
            obj_id: None,
            buffer: Vec::new(),
        }
    }

    fn match_file<const BLOCK_SIZE: usize, F: File<BLOCK_SIZE>>(&self, file: F) -> bool {
        match self.obj_id {
            Some(id) => id == file.id(),
            None => false,
        }
    }
}

impl<const BLOCK_SIZE: usize, F: File<BLOCK_SIZE>> flash_db::FileStore<BLOCK_SIZE, F>
    for &mut MockSingleFileStore
{
    async fn initialize(&mut self) -> Result<(), flash_db::Error> {
        Ok(())
    }

    async fn read_file(&mut self, file: F, bytes: &mut [u8]) -> Result<usize, flash_db::Error> {
        if !self.match_file::<BLOCK_SIZE, F>(file) {
            return Err(flash_db::Error::NotFound);
        }

        // Length, limited by available data and file capacity
        let len = bytes.len().min(file.capacity()).min(self.buffer.len());
        bytes[..len].copy_from_slice(self.buffer.as_slice());
        Ok(len)
    }

    async fn file_handle(
        &mut self,
        file: F,
    ) -> Result<flash_db::FileHandle<BLOCK_SIZE, F>, flash_db::Error> {
        Ok(flash_db::FileHandle::from_file_and_version(file, 0))
    }

    async fn read_file_fragment(
        &mut self,
        _filehandle: &flash_db::FileHandle<BLOCK_SIZE, F>,
        _fragment_no: flash_db::FragIndex,
        _bytes: &mut [u8],
    ) -> Result<usize, flash_db::Error> {
        todo!()
    }
}

impl<const BLOCK_SIZE: usize, F: File<BLOCK_SIZE>> flash_db::WriteableFileStore<BLOCK_SIZE, F>
    for &mut MockSingleFileStore
{
    async fn initialize_writeable(
        &mut self,
        _auto_format: bool,
    ) -> Result<flash_db::InitStatus, flash_db::Error> {
        Ok(flash_db::InitStatus::New)
    }

    async fn write_file(&mut self, file: F, bytes: &[u8]) -> Result<(), flash_db::Error> {
        if self.obj_id.is_some() && !self.match_file::<BLOCK_SIZE, F>(file) {
            return Err(flash_db::Error::NotFound);
        }
        self.obj_id = Some(file.id());
        self.buffer = Vec::new();
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    async fn write_file_fragment(
        &mut self,
        _filehandle: &flash_db::WriteableFileHandle<BLOCK_SIZE, F>,
        _fragment_no: flash_db::FragIndex,
        _bytes: &[u8],
    ) -> Result<(), flash_db::Error> {
        todo!()
    }

    async fn delete(
        &mut self,
        file: F,
    ) -> Result<flash_db::WriteableFileHandle<BLOCK_SIZE, F>, flash_db::Error> {
        self.buffer = Vec::new();
        self.obj_id = None;

        Ok(WriteableFileHandle::from_handle(
            self.file_handle(file).await?,
        ))
    }
}

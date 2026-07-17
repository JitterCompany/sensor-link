use super::{
    block_layer, Circular, CircularStore, Database, File, FileStore, WriteableCircularStore,
    WriteableFileHandle, WriteableFileStore,
};
use crate::traits::Arbiter;

/// Wrapper: implement Filestore for `Arbiter<Database>`
impl<A, BD, F, C, const BS: usize> FileStore<BS, F> for &A
where
    A: Arbiter<Shared = Database<BD, BS, F, C>>,
    BD: block_layer::Reader<BS>,
    F: File<BS>,
    C: Circular<BS>,
{
    async fn initialize(&mut self) -> Result<(), super::Error> {
        self.access().await.initialize().await
    }

    async fn read_file(&mut self, file: F, bytes: &mut [u8]) -> Result<usize, super::Error> {
        self.access().await.read_file(file, bytes).await
    }

    async fn file_handle(&mut self, file: F) -> Result<super::FileHandle<BS, F>, super::Error> {
        self.access().await.file_handle(file).await
    }

    async fn read_file_fragment(
        &mut self,
        filehandle: &super::FileHandle<BS, F>,
        fragment_no: super::FragIndex,
        bytes: &mut [u8],
    ) -> Result<usize, super::Error> {
        self.access()
            .await
            .read_file_fragment(filehandle, fragment_no, bytes)
            .await
    }
}

/// Wrapper: implement WriteableFileStore for `Arbiter<Database>`
impl<A, BD, F, C, const BS: usize> WriteableFileStore<BS, F> for &A
where
    A: Arbiter<Shared = Database<BD, BS, F, C>>,
    BD: block_layer::Writer<BS>,
    F: File<BS>,
    C: Circular<BS>,
{
    async fn initialize_writeable(
        &mut self,
        auto_format: bool,
    ) -> Result<super::InitStatus, super::Error> {
        self.access().await.initialize_writeable(auto_format).await
    }

    async fn write_file(&mut self, file: F, bytes: &[u8]) -> Result<(), super::Error> {
        self.access().await.write_file(file, bytes).await
    }

    async fn write_file_fragment(
        &mut self,
        filehandle: &WriteableFileHandle<BS, F>,
        fragment_no: super::FragIndex,
        bytes: &[u8],
    ) -> Result<(), super::Error> {
        self.access()
            .await
            .write_file_fragment(filehandle, fragment_no, bytes)
            .await
    }

    async fn delete(&mut self, file: F) -> Result<WriteableFileHandle<BS, F>, super::Error> {
        self.access().await.delete(file).await
    }
}

/// Wrapper: implement CircularStore for `Arbiter<Database>`
impl<A, BD, F, C, const BS: usize> CircularStore<BS, C> for &A
where
    A: Arbiter<Shared = Database<BD, BS, F, C>>,
    BD: block_layer::Reader<BS>,
    F: File<BS>,
    C: Circular<BS>,
{
    async fn initialize_circular_store(&mut self) -> Result<(), super::Error> {
        self.access().await.initialize_circular_store().await
    }

    async fn find_circular_range(
        &mut self,
        circular: C,
    ) -> Result<super::CircularRange, super::Error> {
        self.access().await.find_circular_range(circular).await
    }

    async fn read_circular_fragment_at_offset(
        &mut self,
        circular: C,
        fragment_no: super::SeqNo,
        bytes: &mut [u8],
        offset: usize,
    ) -> Result<usize, super::Error> {
        self.access()
            .await
            .read_circular_fragment_at_offset(circular, fragment_no, bytes, offset)
            .await
    }
}

/// Wrapper: implement WriteableCircularStore for `Arbiter<Database>`
impl<A, BD, F, C, const BS: usize> WriteableCircularStore<BS, C> for &A
where
    A: Arbiter<Shared = Database<BD, BS, F, C>>,
    BD: block_layer::Writer<BS>,
    F: File<BS>,
    C: Circular<BS>,
{
    async fn initialize_writeable_circular_store(
        &mut self,
        auto_format: bool,
    ) -> Result<super::InitStatus, super::Error> {
        self.access()
            .await
            .initialize_writeable_circular_store(auto_format)
            .await
    }

    async fn write_circular_fragment(
        &mut self,
        circular: C,
        fragment_no: super::SeqNo,
        bytes: &[u8],
    ) -> Result<(), super::Error> {
        self.access()
            .await
            .write_circular_fragment(circular, fragment_no, bytes)
            .await
    }

    async fn delete_circular_fragment(
        &mut self,
        circular: C,
        fragment_no: super::SeqNo,
    ) -> Result<(), super::Error> {
        self.access()
            .await
            .delete_circular_fragment(circular, fragment_no)
            .await
    }
}

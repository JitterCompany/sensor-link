//! Persistent file store of the demo device

use crate::{
    logic::{
        orchestrator::{application_behavior, ProgramState},
        time_adjust::ClockCalibration,
    },
    storage::flash_db::{self, Error, WriteableFileHandle},
};

/// Block size for flashDB. Same for stream store as both use the same flashdb.
pub const BLOCK_SIZE: usize = super::streams::BLOCK_SIZE;

/// Files that can be stored
#[derive(Debug, Clone, Copy, PartialEq, strum::EnumCount)]
pub enum File {
    Firmware,
    Settings,
    ProgramState,
    ClockCalibration,
}

impl File {
    pub const fn max_fragment_size(&self) -> usize {
        match self {
            File::Firmware => 300,
            File::Settings => 512,
            File::ProgramState => 20,
            File::ClockCalibration => 32,
        }
    }

    /// Maximum serialized size of the file
    pub const fn max_file_size(&self) -> usize {
        match self {
            File::Firmware => 300,
            File::Settings => 512,
            File::ProgramState => 20,
            File::ClockCalibration => 32,
        }
    }
}

impl flash_db::Object<{ BLOCK_SIZE }> for File {
    fn id(&self) -> flash_db::ObjectId {
        *self as flash_db::ObjectId
    }

    fn fragment_size(&self) -> usize {
        self.max_fragment_size()
    }

    fn flash_blocks(&self) -> core::ops::Range<flash_db::block_layer::BlockId> {
        match self {
            // (reserve 0,1 for fs internal )
            File::Firmware => 2..541,

            File::ClockCalibration => 547..550,

            File::ProgramState => 560..563,

            // (..759) can be claimed for future files)
            File::Settings => 759..768,
        }
    }
}

impl flash_db::File<BLOCK_SIZE> for File {}

pub struct FileStore<DB: flash_db::WriteableFileStore<BLOCK_SIZE, File>> {
    pub db: DB,
    firmware_writer: Option<WriteableFileHandle<BLOCK_SIZE, File>>,
}

impl<DB: flash_db::WriteableFileStore<BLOCK_SIZE, File>> FileStore<DB> {
    pub fn new(db: DB) -> Self {
        Self {
            db,
            firmware_writer: None,
        }
    }
}

impl<DB: flash_db::WriteableFileStore<BLOCK_SIZE, File>> application_behavior::Store
    for FileStore<DB>
{
    type Error = flash_db::Error;

    async fn initialize_writeable(
        &mut self,
        auto_format: bool,
    ) -> Result<flash_db::InitStatus, Self::Error> {
        self.db.initialize_writeable(auto_format).await
    }

    async fn read_settings(&mut self) -> Result<demo_protocol::settings::Settings, Self::Error> {
        let mut bytes = [0u8; File::Settings.max_file_size()];
        self.db.read_file(File::Settings, &mut bytes).await?;
        let crc = crc::Crc::<u32>::new(&crc::CRC_32_CKSUM);
        let digest = crc.digest();
        postcard::from_bytes_crc32::<demo_protocol::settings::Settings>(&bytes, digest)
            .map_err(|_err| Error::InvalidObject)
    }

    async fn write_settings(
        &mut self,
        settings: &demo_protocol::settings::Settings,
    ) -> Result<(), Self::Error> {
        let mut bytes = [0u8; File::Settings.max_file_size()];
        let crc = crc::Crc::<u32>::new(&crc::CRC_32_CKSUM);
        let digest = crc.digest();
        if let Ok(serialized_bytes) = postcard::to_slice_crc32(settings, &mut bytes, digest) {
            self.db.write_file(File::Settings, serialized_bytes).await
        } else {
            Err(Error::InvalidObject)
        }
    }

    async fn delete_settings(&mut self) -> Result<(), Self::Error> {
        self.db.delete(File::Settings).await.map(|_| ())
    }

    async fn read_clock_calibration(
        &mut self,
    ) -> Result<crate::logic::time_adjust::ClockCalibration, Self::Error> {
        let mut bytes = [0u8; File::ClockCalibration.max_file_size()];
        self.db
            .read_file(File::ClockCalibration, &mut bytes)
            .await?;
        let crc = crc::Crc::<u32>::new(&crc::CRC_32_CKSUM);
        let digest = crc.digest();
        postcard::from_bytes_crc32::<ClockCalibration>(&bytes, digest)
            .map_err(|_err| Error::InvalidObject)
    }

    async fn write_clock_calibration(
        &mut self,
        cal: &crate::logic::time_adjust::ClockCalibration,
    ) -> Result<(), Self::Error> {
        // Skip rewriting if the calibration data is already very similar.
        // Helps reduce flash wear in case calibration updates are done very often
        if let Ok(prev_cal) = self.read_clock_calibration().await {
            if !cal.is_significantly_different(prev_cal) {
                return Ok(());
            }
        }
        let mut bytes = [0u8; File::ClockCalibration.max_file_size()];
        let crc = crc::Crc::<u32>::new(&crc::CRC_32_CKSUM);
        let digest = crc.digest();
        if let Ok(serialized_bytes) = postcard::to_slice_crc32(cal, &mut bytes, digest) {
            self.db
                .write_file(File::ClockCalibration, serialized_bytes)
                .await
        } else {
            Err(Error::InvalidObject)
        }
    }

    async fn read_program_state(
        &mut self,
    ) -> Result<crate::logic::orchestrator::ProgramState, Self::Error> {
        let mut bytes = [0u8; File::ProgramState.max_file_size()];
        self.db.read_file(File::ProgramState, &mut bytes).await?;
        let crc = crc::Crc::<u32>::new(&crc::CRC_32_CKSUM);
        let digest = crc.digest();
        postcard::from_bytes_crc32::<ProgramState>(&bytes, digest)
            .map_err(|_err| Error::InvalidObject)
    }

    async fn write_program_state(
        &mut self,
        state: &crate::logic::orchestrator::ProgramState,
    ) -> Result<(), Self::Error> {
        let mut bytes = [0u8; File::ProgramState.max_file_size()];
        let crc = crc::Crc::<u32>::new(&crc::CRC_32_CKSUM);
        let digest = crc.digest();
        if let Ok(serialized_bytes) = postcard::to_slice_crc32(state, &mut bytes, digest) {
            self.db
                .write_file(File::ProgramState, serialized_bytes)
                .await
        } else {
            Err(Error::InvalidObject)
        }
    }

    async fn write_firmware_fragment(
        &mut self,
        sequence: u32,
        data: &[u8],
    ) -> Result<(), Self::Error> {
        // use handle saved when deleting. If not deleted, fragments can't be overwritten.
        let handle = self
            .firmware_writer
            .as_ref()
            .ok_or(Error::FragmentNotWriteable)?;
        self.db.write_file_fragment(handle, sequence, data).await
    }

    async fn delete_firmware(&mut self) -> Result<(), Self::Error> {
        let handle = self.db.delete(File::Firmware).await?;

        // save write handle for writing in chunks
        self.firmware_writer = Some(handle);
        Ok(())
    }
}

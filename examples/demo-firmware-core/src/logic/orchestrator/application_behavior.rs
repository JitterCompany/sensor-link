//! Trait for application-specific behavior
//!
//! This trait is used to customize the behavior of the orchestrator for different applications.
//! This keeps the orchestrator generic (i.e. it does not have to know about application-specific file formats etc)

use demo_protocol::settings::Settings;

use crate::storage::flash_db::InitStatus;

use core::fmt::Debug;

/// Trait that abstracts application-specific behavior for the orchestrator.
///
/// This allows the orchestrator to work with different applications
/// by delegating application-specific logic to implementations of this trait.
pub trait ApplicationBehavior {
    /// Application- (and platform-?)specific store type
    type Store: Store;

    /// Access the application- (and platform-)specific store
    fn store(&mut self) -> &mut Self::Store;
}

/// Trait for common storage operations
///
/// The trait allows platform-specific implementations
pub trait Store {
    type Error: Debug + Clone;

    /// Initialize the storage for writing
    async fn initialize_writeable(&mut self, create: bool) -> Result<InitStatus, Self::Error>;

    /// Read settings from storage
    async fn read_settings(&mut self) -> Result<Settings, Self::Error>;

    /// Write settings to storage
    async fn write_settings(&mut self, settings: &Settings) -> Result<(), Self::Error>;

    /// Delete settings from storage
    async fn delete_settings(&mut self) -> Result<(), Self::Error>;

    /// Read clock calibration from storage
    async fn read_clock_calibration(
        &mut self,
    ) -> Result<crate::logic::time_adjust::ClockCalibration, Self::Error>;

    /// Write clock calibration to storage
    async fn write_clock_calibration(
        &mut self,
        cal: &crate::logic::time_adjust::ClockCalibration,
    ) -> Result<(), Self::Error>;

    /// Read program state from storage
    async fn read_program_state(
        &mut self,
    ) -> Result<crate::logic::orchestrator::ProgramState, Self::Error>;

    /// Write program state to storage
    async fn write_program_state(
        &mut self,
        state: &crate::logic::orchestrator::ProgramState,
    ) -> Result<(), Self::Error>;

    /// Write firmware fragment (for firmware updates)
    async fn write_firmware_fragment(
        &mut self,
        sequence: u32,
        data: &[u8],
    ) -> Result<(), Self::Error>;

    /// Delete last firmware update
    async fn delete_firmware(&mut self) -> Result<(), Self::Error>;
}

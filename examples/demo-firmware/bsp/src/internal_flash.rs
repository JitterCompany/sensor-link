//! Internal flash driver: read/write flash inside the microcontroller
//!
//! The API currently is blocking instead of async as erase/write to flash
//! halts the core anyways (when erasing/writing the same bank)

use demo_firmware_core::{
    bootloader::{AppFlashRead, AppFlashWrite},
    meta::{
        bootloader::{BootloaderMeta, BootloaderMetaError},
        config::{FirmwareConfig, FirmwareConfigError},
        firmware::{ApplicationHeader, ApplicationHeaderError, DeviceType},
    },
};

pub mod address;
mod driver;

pub use driver::{EraseError, WriteError, WriteableRegion};

/// Internal flash driver
///
/// Wraps internal flash and exposes a simple interface to read/write specific sections
pub struct InternalFlash {
    /// Lower level driver: access to all flash sections
    driver: driver::Flash,
}

impl InternalFlash {
    pub fn new(peripheral: stm32ral::flash::Instance) -> Self {
        Self {
            driver: driver::Flash::new(peripheral),
        }
    }

    /// Erase the application firmware
    ///
    /// This erases the whole application section (appliction code + metadata).
    /// If succesfull, the resulting WriteableRegion can be used to write new data to it.
    ///
    /// # Safety
    ///
    /// This is only safe if not called from the application itself.
    /// (although the firmware likely fails to build with a linker error if you try)
    pub unsafe fn erase_application(&mut self) -> Result<WriteableRegion<'_>, EraseError> {
        self.driver
            .erase(address::application_total().ok_or(EraseError::OutOfBounds)?)
    }

    /// Read application firmware metadata from internal flash
    pub fn application_metadata(&self) -> Result<ApplicationHeader, ApplicationHeaderError> {
        let region = address::application_metadata().ok_or(ApplicationHeaderError::TooFewBytes)?;
        let fl_offset = region.flash_offset();
        let bytes = self.driver.read(region);
        log::debug!("application meta: read {:?} @{}", &bytes[..16], fl_offset);
        ApplicationHeader::try_from_bytes(bytes)
    }

    /// Read application firmware code from internal flash
    pub fn application_code(&self) -> &[u8] {
        match address::application_code() {
            Some(region) => self.driver.read(region),
            None => &[],
        }
    }

    /// Read application firmware (meta + code) from internal flash
    pub fn application_total(&self) -> &[u8] {
        match address::application_total() {
            Some(region) => self.driver.read(region),
            None => &[],
        }
    }

    /// Read firmware config from internal flash, verifying it is for the expected device type.
    #[inline]
    pub fn config_for_device_type(
        &self,
        expected_device_type: DeviceType,
    ) -> Result<FirmwareConfig<'_, DeviceType>, FirmwareConfigError<DeviceType>> {
        let config = self.config()?;
        if config.device_type == expected_device_type {
            Ok(config)
        } else {
            Err(FirmwareConfigError::MismatchedDeviceType(
                config.device_type,
            ))
        }
    }

    /// Read firmware config from internal flash
    ///
    /// Note: this assumes latest FirmwareConfig format
    #[inline]
    pub fn config(
        &self,
    ) -> Result<FirmwareConfig<'_, DeviceType>, FirmwareConfigError<DeviceType>> {
        FirmwareConfig::try_from_bytes(self.driver.read(address::config()))
    }

    /// Read bootloader metadata from internal flash
    pub fn bootloader_metadata(&self) -> Result<BootloaderMeta<'_>, BootloaderMetaError> {
        let region = address::bootloader_meta().ok_or(BootloaderMetaError::TooFewBytes)?;

        BootloaderMeta::try_from_bytes(self.driver.read(region))
    }
}

// Bridge the concrete internal-flash driver to the generic update logic.
impl AppFlashRead for InternalFlash {
    fn application_total(&self) -> &[u8] {
        InternalFlash::application_total(self)
    }
}

impl AppFlashWrite for InternalFlash {
    type EraseError = EraseError;
    type WriteError = WriteError;
    type Writer<'a> = WriteableRegion<'a>;

    unsafe fn erase_application(&mut self) -> Result<Self::Writer<'_>, Self::EraseError> {
        unsafe { InternalFlash::erase_application(self) }
    }
}

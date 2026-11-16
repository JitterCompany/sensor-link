//! Firmware-image metadata for the demo device.
//!
//! Defines the device-type enum that is embedded in the application header, so
//! the bootloader can reject firmware built for a different device, and
//! re-exports the generic header and validator from sensor-link-firmware.
//!
//! The demo has no legacy V1 images in the field, so the plain generic
//! [`ApplicationHeaderV2`] is pinned to [`DeviceType`] directly.

use num_enum::TryFromPrimitive;
pub use sensor_link_firmware::meta::firmware::{
    ApplicationHeaderError, ApplicationHeaderV2, Security, SecurityLevel, SignError,
    ValidationError,
};

/// Identifies the application type a firmware image is built for.
///
/// The bootloader uses this to reject firmware meant for a different device.
/// When adding a new variant, assign the next free `u8` discriminant.
#[repr(u8)]
#[derive(TryFromPrimitive, Clone, Copy, Debug, PartialEq, PartialOrd)]
pub enum DeviceType {
    Demo = 0,
}

impl From<DeviceType> for demo_protocol::DeviceType {
    fn from(value: DeviceType) -> Self {
        match value {
            DeviceType::Demo => demo_protocol::DeviceType::Demo,
        }
    }
}

/// Concrete application header of the demo: the generic header pinned to
/// [`DeviceType`]. Exposes the inherent `BINARY_SIZE` / `placeholder` /
/// `try_from_bytes` / `as_bytes` API used by the build script and flash layer.
pub type ApplicationHeader = ApplicationHeaderV2<DeviceType>;

/// Validator for demo firmware images.
pub type Validator = sensor_link_firmware::meta::firmware::Validator<ApplicationHeader>;

impl sensor_link_firmware::meta::firmware::DeviceType for DeviceType {
    fn to_byte(self) -> u8 {
        self as u8
    }

    fn try_from_byte(byte: u8) -> Option<Self> {
        Self::try_from_primitive(byte).ok()
    }
}

/// Sign a firmware image. See [`Validator`].
///
/// The complete firmware has to be passed in as a slice of bytes.
pub fn sign(
    signing_key: &crate::utils::crypto::PrivKey,
    binary: &mut [u8],
) -> Result<(), SignError> {
    sensor_link_firmware::meta::firmware::sign::<ApplicationHeader>(signing_key, binary)
}

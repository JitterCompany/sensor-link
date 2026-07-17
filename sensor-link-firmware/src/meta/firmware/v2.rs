//! Firmware Binary Header V2 (device-type aware, no legacy V1 support).
use core::marker::PhantomData;

use crate::utils::crypto;

use super::{
    super::section_header::{header, Header},
    ApplicationHeaderError, DeviceType, Security, SecurityLevel,
};

// Sizes are independent of the device-type parameter, so they live as free
// consts (generic parameters may not appear in array-length / const positions).
const BINARY_SIZE_RAW: usize = 76;
const BINARY_SIZE_AUTH: usize = 5;
const BINARY_SIZE: usize = BINARY_SIZE_RAW + BINARY_SIZE_AUTH;

/// Application firmware metadata
///
/// This header is present at the start of bootloadable firmware
/// and contains metadata about the firmware image.
#[derive(Debug, Clone, PartialEq)]
pub struct ApplicationHeader<D: DeviceType> {
    pub(super) security: Security,

    /// downgrade Security: bootloader verifies new firmware has N >= old firmware.
    /// Increment to prevent (accidental?) downgrade to obsolete firmware versions.
    pub(super) anti_downgrade_version: u32,

    /// Device type enum used to validate that the firmware binary is for the correct device type
    pub(super) device_type: D,

    _private: PhantomData<()>,
}

impl<D: DeviceType> ApplicationHeader<D> {
    pub(super) const BINARY_SIZE_RAW: usize = BINARY_SIZE_RAW;
    pub(super) const BINARY_SIZE_AUTH: usize = BINARY_SIZE_AUTH;
    pub const BINARY_SIZE: usize = BINARY_SIZE;

    /// Generate placeholder
    ///
    /// The placeholder must be overwritten/patched in the elf/binary to be
    /// accepted by the bootloader.
    pub fn placeholder(device_type: D) -> Self {
        Self {
            security: Security::None,
            anti_downgrade_version: 0,
            device_type,
            _private: PhantomData,
        }
    }

    /// Get the security level applied to this image
    pub fn security_level(&self) -> SecurityLevel {
        self.security.level()
    }

    /// Get the total length of the firmware image
    ///
    /// Depending on the security level, the length may be unknown (None).
    /// If a length is returned, it is the total length including this metadata header itself
    pub fn length(&self) -> Option<usize> {
        match self.security {
            Security::None => None,
            Security::IntegrityOnly(len, _) => Some(len),
            Security::Signed(len, _) => Some(len),
        }
    }

    pub fn anti_downgrade_version(&self) -> u32 {
        self.anti_downgrade_version
    }

    /// Write a signature. Updates the security level to `SecurityLevel::Signed`
    ///
    /// The length must be the total length of the application firmware including metadata
    pub fn write_signature(
        &mut self,
        application_total_length: usize,
        signature: crypto::Signature,
    ) {
        self.security = Security::Signed(application_total_length, signature);
    }

    /// Serialize to bytes
    pub fn as_bytes(&self, bytes: &mut [u8]) -> Result<(), ApplicationHeaderError> {
        if bytes.len() < Self::BINARY_SIZE {
            return Err(ApplicationHeaderError::TooFewBytes);
        }
        let bytes = &mut bytes[..Self::BINARY_SIZE];

        // Initialize all zeroes
        bytes.fill(0);

        {
            // 4-byte Header
            bytes[0..4].copy_from_slice(&header(Header::AppMetaV2));
            let bytes = &mut bytes[4..];

            // 4-byte security level (byte 5..8 RFU)
            bytes[0] = self.security.level() as u8;
            let bytes = &mut bytes[4..];

            // 4-byte length field
            let length = match self.security {
                Security::None => 0,
                Security::IntegrityOnly(length, _) => length,
                Security::Signed(length, _) => length,
            } as u32;
            bytes[..4].copy_from_slice(&length.to_le_bytes());
            let bytes = &mut bytes[4..];

            // 64-byte security tag (signature / hash / ..) field
            match &self.security {
                Security::None => {}
                Security::IntegrityOnly(_, hash) => {
                    bytes[..crypto::Hash::BINARY_SIZE].copy_from_slice(&hash.to_bytes());
                }
                Security::Signed(_, signature) => {
                    bytes[..crypto::Signature::BINARY_SIZE].copy_from_slice(&signature.to_bytes());
                }
            }
            let bytes = &mut bytes[64..];
            let _ = bytes;
        }
        self.authenticated_fields_as_bytes(&mut bytes[Self::BINARY_SIZE_RAW..])
    }

    /// Try to parse from bytes. Expects at least Self::BINARY_SIZE bytes
    pub fn try_from_bytes(bytes: &[u8]) -> Result<Self, ApplicationHeaderError> {
        if bytes.len() < Self::BINARY_SIZE {
            return Err(ApplicationHeaderError::TooFewBytes);
        }

        // 4-byte Header
        if bytes[0..4] != header(Header::AppMetaV2) {
            return Err(ApplicationHeaderError::InvalidHeader);
        }
        let bytes = &bytes[4..];

        // 4-byte security level (byte 5..8 RFU)
        let security_level = SecurityLevel::try_from(bytes[0])
            .map_err(|_| ApplicationHeaderError::InvalidSecurityLevel)?;
        // Skip bytes 5..8 (RFU)
        let bytes = &bytes[4..];

        // 4-byte length field
        let length = u32::from_le_bytes(bytes[..4].try_into().unwrap());
        let length = if length > 0 { Some(length) } else { None };
        let bytes = &bytes[4..];

        // 64-byte security tag (signature / hash / ..) field
        let security = match (length, security_level) {
            (_, SecurityLevel::None) => Security::None,
            (None, _) => {
                return Err(ApplicationHeaderError::InvalidLength);
            }
            (Some(length), SecurityLevel::IntegrityOnly) => {
                let hash = crypto::Hash::try_from_bytes(&bytes[..64])
                    .map_err(|_| ApplicationHeaderError::InvalidSecurityTag)?;
                Security::IntegrityOnly(length as usize, hash)
            }
            (Some(length), SecurityLevel::Signed) => {
                let signature = crypto::Signature::try_from_bytes(&bytes[..64])
                    .map_err(|_| ApplicationHeaderError::InvalidSecurityTag)?;
                Security::Signed(length as usize, signature)
            }
        };
        let bytes = &bytes[64..];

        // 4-byte anti-downgrade field / build number
        let anti_downgrade_version = u32::from_le_bytes(bytes[..4].try_into().unwrap());
        let bytes = &bytes[4..];

        // 1-byte device type
        let device_type =
            D::try_from_byte(bytes[0]).ok_or(ApplicationHeaderError::InvalidDeviceType)?;
        let _bytes = &bytes[1..];

        Ok(ApplicationHeader {
            security,
            anti_downgrade_version,
            device_type,
            _private: PhantomData,
        })
    }

    /// Serialize the authenticated part of the metadata to bytes
    ///
    /// Intended to be used to 'authenticate' (e.g. hash or sign) part of the metadata
    pub(crate) fn authenticated_fields_as_bytes(
        &self,
        bytes: &mut [u8],
    ) -> Result<(), ApplicationHeaderError> {
        if bytes.len() < Self::BINARY_SIZE_AUTH {
            return Err(ApplicationHeaderError::TooFewBytes);
        }

        // 4-byte anti-downgrade field / build number
        bytes[..4].copy_from_slice(&self.anti_downgrade_version.to_le_bytes());

        // 1 byte device type
        bytes[4] = self.device_type.to_byte();

        Ok(())
    }
}

impl<D: DeviceType> super::ApplicationHeader for ApplicationHeader<D> {
    type DeviceType = D;
    type RawBuffer = [u8; BINARY_SIZE];

    const BINARY_SIZE: usize = BINARY_SIZE;
    const BINARY_SIZE_RAW: usize = BINARY_SIZE_RAW;
    const BINARY_SIZE_AUTH: usize = BINARY_SIZE_AUTH;

    fn new_raw_buffer() -> Self::RawBuffer {
        [0u8; BINARY_SIZE]
    }

    fn try_from_bytes(bytes: &[u8]) -> Result<Self, ApplicationHeaderError> {
        Self::try_from_bytes(bytes)
    }

    fn as_bytes(&self, bytes: &mut [u8]) -> Result<(), ApplicationHeaderError> {
        self.as_bytes(bytes)
    }

    fn authenticated_fields_as_bytes(
        &self,
        bytes: &mut [u8],
    ) -> Result<(), ApplicationHeaderError> {
        self.authenticated_fields_as_bytes(bytes)
    }

    fn write_signature(&mut self, application_total_length: usize, signature: crypto::Signature) {
        self.write_signature(application_total_length, signature)
    }

    fn security(&self) -> &Security {
        &self.security
    }

    fn device_type(&self) -> D {
        self.device_type
    }

    fn anti_downgrade_version(&self) -> u32 {
        self.anti_downgrade_version
    }
}

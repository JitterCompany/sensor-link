//! Provides access to metadata about the bootloader

use core::marker::PhantomData;

use super::section_header::{header, Header};

/// Bootloader metadata
///
/// This contains metadata about the bootloader
#[derive(Debug, Clone, PartialEq)]
pub struct BootloaderMeta<'meta> {
    pub version: &'meta str,
    pub git_revision: &'meta str,

    _private: PhantomData<()>,
}

/// Errors that may occur while serializing/deserializing an [BootloaderMeta](struct@BootloaderMeta)
#[derive(Debug, Clone, PartialEq)]
pub enum BootloaderMetaError {
    TooFewBytes,
    InvalidHeader,
    InvalidVersion,
    InvalidGitRevision,
}

fn str_from_zero_padded_slice(slice: &[u8]) -> Result<&str, core::str::Utf8Error> {
    let mut end = slice.len();

    // Find null terminator if any
    for i in 0..end {
        if slice[i] == 0 {
            end = i;
            break;
        }
    }

    core::str::from_utf8(&slice[..end])
}

const VERSION_LEN: usize = 16;

impl<'meta> BootloaderMeta<'meta> {
    pub const BINARY_SIZE: usize = 4 + (2 * VERSION_LEN);

    pub fn new(version: &'meta str, git_revision: &'meta str) -> Self {
        Self {
            version,
            git_revision,
            _private: PhantomData,
        }
    }

    /// Serialize to bytes
    pub fn as_bytes(&self, bytes: &mut [u8]) -> Result<(), BootloaderMetaError> {
        if bytes.len() < Self::BINARY_SIZE {
            return Err(BootloaderMetaError::TooFewBytes);
        }
        let bytes = &mut bytes[..Self::BINARY_SIZE];

        // Initialize all zeroes
        bytes.fill(0);

        {
            // 4-byte Header
            bytes[0..4].copy_from_slice(&header(Header::BootloaderMeta));
            let bytes = &mut bytes[4..];

            // 16-byte version string (0-padded)
            let version = self.version.as_bytes();
            let len = version.len().min(VERSION_LEN);
            bytes[..len].copy_from_slice(&version[..len]);
            let bytes = &mut bytes[VERSION_LEN..];

            // 16-byte git revision string (0-padded)
            let git_rev = self.git_revision.as_bytes();
            let len = git_rev.len().min(VERSION_LEN);
            bytes[..len].copy_from_slice(&git_rev[..len]);
            let _bytes = &mut bytes[VERSION_LEN..];
        }
        Ok(())
    }

    /// Try to parse from bytes. Expects at least Self::BINARY_SIZE bytes
    pub fn try_from_bytes(bytes: &'meta [u8]) -> Result<Self, BootloaderMetaError> {
        if bytes.len() < Self::BINARY_SIZE {
            return Err(BootloaderMetaError::TooFewBytes);
        }

        // 4-byte Header
        if bytes[0..4] != header(Header::BootloaderMeta) {
            return Err(BootloaderMetaError::InvalidHeader);
        }
        let bytes = &bytes[4..];

        // 16-byte version string
        let version = str_from_zero_padded_slice(&bytes[..VERSION_LEN])
            .map_err(|_| BootloaderMetaError::InvalidVersion)?;
        let bytes = &bytes[VERSION_LEN..];

        // 16-byte git revision
        let git_revision = str_from_zero_padded_slice(&bytes[..VERSION_LEN])
            .map_err(|_| BootloaderMetaError::InvalidGitRevision)?;
        let _bytes = &bytes[VERSION_LEN..];

        Ok(BootloaderMeta {
            version,
            git_revision,
            _private: PhantomData,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_meta_invalid() {
        let bytes = [0x11; BootloaderMeta::BINARY_SIZE];
        assert_eq!(
            BootloaderMeta::try_from_bytes(&bytes).unwrap_err(),
            BootloaderMetaError::InvalidHeader
        );
    }

    #[test]
    fn test_meta_serialize_deserialize_loopback() {
        let ref_meta = BootloaderMeta::new("12.13.14-abc", "0123456789abcdef");
        let mut bytes = [0x11; BootloaderMeta::BINARY_SIZE];
        ref_meta.as_bytes(&mut bytes).unwrap();
        let meta = BootloaderMeta::try_from_bytes(&bytes).unwrap();
        assert_eq!(ref_meta.version, meta.version);
        assert_eq!(ref_meta.git_revision, meta.git_revision);
    }

    #[test]
    fn test_meta_deserialize_compatibility() {
        // Hardcoded array: should be able to deserialize into valid BootloaderMeta
        // do not edit: this test is meant to test for backwards compatibility in case the BootloaderMeta ever changes
        let ref_bytes = [
            0x4A, 0x54, 0x52, 0x08, '4' as u8, '.' as u8, '1' as u8, '2' as u8, '3' as u8,
            '.' as u8, '9' as u8, '8' as u8, 0, 0, 0, 0, 0, 0, 0, 0, 'g' as u8, 'i' as u8,
            't' as u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];

        let meta = BootloaderMeta::try_from_bytes(&ref_bytes).unwrap();
        assert_eq!(meta.version, "4.123.98");
        assert_eq!(meta.git_revision, "git");
    }
}

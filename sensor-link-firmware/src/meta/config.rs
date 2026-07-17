//! Provides access to the provisioned config for a board.

use core::marker::PhantomData;

use crate::utils;

use super::{
    firmware::DeviceType,
    section_header::{header, Header},
};

#[derive(Debug, Clone, PartialEq)]
pub enum FirmwareConfigError<D: DeviceType> {
    TooFewBytes,
    InvalidHeader,
    InvalidSerial,
    InvalidClientCert,
    InvalidClientKey,
    InvalidDeviceType,
    MismatchedDeviceType(D),
}

const MAX_SERIAL_STRLEN: usize = sensor_link_protocol::MAX_UID_LEN;
pub type SerialString = heapless::String<MAX_SERIAL_STRLEN>;

// Sizes are independent of the device-type parameter, so they live as free
// consts (a generic `Self` may not appear in array-length / const positions).
const MAX_CERT_SIZE: usize = 1024;
const MAX_KEY_SIZE: usize = 512;
const MAX_SERIAL_SIZE_BYTES: usize = 16;
const BINARY_SIZE_COMMON: usize = 4 // Header bytes
    + MAX_SERIAL_SIZE_BYTES
    + 4 // length of client cert
    + MAX_CERT_SIZE
    + 4 // length of client key
    + MAX_KEY_SIZE;
const BINARY_SIZE: usize = BINARY_SIZE_COMMON + 1; // device type

#[derive(Clone)]
pub struct FirmwareConfig<'a, D: DeviceType> {
    pub serial: SerialString,
    /// Client certificate for TLS authentication
    pub client_cert: &'a [u8],
    /// Client private key for TLS authentication
    pub client_key: &'a [u8],

    pub device_type: D,

    _private: PhantomData<()>,
}

impl<'a, D: DeviceType> FirmwareConfig<'a, D> {
    /// Maximum size for the client certificate.
    pub const MAX_CERT_SIZE: usize = MAX_CERT_SIZE;
    /// Maximum size for the client private key.
    pub const MAX_KEY_SIZE: usize = MAX_KEY_SIZE;

    /// Maximum string length of the serial number
    pub const MAX_SERIAL_STRLEN: usize = MAX_SERIAL_STRLEN;

    /// Required size for the config section in flash.
    pub const BINARY_SIZE: usize = BINARY_SIZE;

    pub fn from_resources(
        serial: SerialString,
        device_type: D,
        client_cert: &'a [u8],
        client_key: &'a [u8],
    ) -> Self {
        Self {
            serial,
            client_cert,
            client_key,
            device_type,
            _private: PhantomData,
        }
    }

    /// Serialize to bytes
    pub fn as_bytes(&self, bytes: &mut [u8; BINARY_SIZE]) {
        // Initialize all zeroes
        bytes.fill(0);

        // 4-byte header
        bytes[0..4].copy_from_slice(&header(Header::Config));
        let bytes = &mut bytes[4..];

        // Serial number string
        {
            let serial_bytes = self.serial.as_bytes();
            let len = serial_bytes.len().min(MAX_SERIAL_SIZE_BYTES);
            bytes[0..len].copy_from_slice(&serial_bytes[0..len]);
        }
        let bytes = &mut bytes[MAX_SERIAL_SIZE_BYTES..];

        // Length of client cert
        let cert_len = self.client_cert.len();
        bytes[0..4].copy_from_slice(&(cert_len as u32).to_le_bytes());
        let bytes = &mut bytes[4..];

        // Client cert
        bytes[0..cert_len].copy_from_slice(self.client_cert);
        let bytes = &mut bytes[MAX_CERT_SIZE..];

        // Length of client key
        let key_len = self.client_key.len();
        bytes[0..4].copy_from_slice(&(key_len as u32).to_le_bytes());
        let bytes = &mut bytes[4..];

        // Client key
        bytes[0..key_len].copy_from_slice(self.client_key);
        let bytes = &mut bytes[MAX_KEY_SIZE..];

        // Device type
        bytes[0] = self.device_type.to_byte();

        let _ = bytes[1..];
    }

    /// Try to parse from bytes. Expects at least Self::BINARY_SIZE bytes
    pub fn try_from_bytes(bytes: &'a [u8]) -> Result<Self, FirmwareConfigError<D>> {
        if bytes.len() < BINARY_SIZE {
            return Err(FirmwareConfigError::TooFewBytes);
        }

        // 4-byte header
        if bytes[0..4] != header(Header::Config) {
            return Err(FirmwareConfigError::InvalidHeader);
        }
        let common = &bytes[4..];

        // serial number string
        let serial: SerialString = utils::try_string_from_bytes(&common[..MAX_SERIAL_SIZE_BYTES])
            .map_err(|_| FirmwareConfigError::InvalidSerial)?;
        let common = &common[MAX_SERIAL_SIZE_BYTES..];

        // Read 4 bytes length field for client cert
        let cert_len = u32::from_le_bytes(common[..4].try_into().unwrap()) as usize;
        let common = &common[4..];
        if cert_len > MAX_CERT_SIZE {
            return Err(FirmwareConfigError::InvalidClientCert);
        }
        let client_cert = &common[..cert_len];
        let common = &common[MAX_CERT_SIZE..];

        // Read 4 bytes length field for client key
        let key_len = u32::from_le_bytes(common[..4].try_into().unwrap()) as usize;
        let common = &common[4..];
        if key_len > MAX_KEY_SIZE {
            return Err(FirmwareConfigError::InvalidClientKey);
        }
        let client_key = &common[..key_len];
        let common = &common[MAX_KEY_SIZE..];

        // trailing device-type byte
        let device_type =
            D::try_from_byte(common[0]).ok_or(FirmwareConfigError::InvalidDeviceType)?;

        Ok(Self {
            serial,
            client_cert,
            client_key,
            device_type,
            _private: PhantomData,
        })
    }
}

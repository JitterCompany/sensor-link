//! The per-device config image: UID, device type, client certificate and
//! private key, in the layout `sensor-link-firmware` reads from flash.

use anyhow::{Result, bail};
use sensor_link_firmware::meta::{config::FirmwareConfig, firmware::DeviceType};

/// Device type taken from the profile; only the wire byte matters here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileDeviceType(pub u8);

impl DeviceType for ProfileDeviceType {
    fn to_byte(self) -> u8 {
        self.0
    }
    fn try_from_byte(byte: u8) -> Option<Self> {
        Some(Self(byte))
    }
}

type Config<'a> = FirmwareConfig<'a, ProfileDeviceType>;

pub const BINARY_SIZE: usize = Config::BINARY_SIZE;

/// Firmware's hard cap on the UID / serial length.
pub const MAX_UID_LEN: usize = Config::MAX_SERIAL_STRLEN;

pub fn build(uid: &str, device_type: u8, cert_pem: &[u8], key_pem: &[u8]) -> Result<Vec<u8>> {
    if cert_pem.len() > Config::MAX_CERT_SIZE {
        bail!(
            "certificate PEM is {} bytes, limit {}",
            cert_pem.len(),
            Config::MAX_CERT_SIZE
        );
    }
    if key_pem.len() > Config::MAX_KEY_SIZE {
        bail!(
            "private key PEM is {} bytes, limit {}",
            key_pem.len(),
            Config::MAX_KEY_SIZE
        );
    }
    let serial = sensor_link_firmware::meta::config::SerialString::try_from(uid)
        .map_err(|_| anyhow::anyhow!("UID '{uid}' exceeds {} bytes", Config::MAX_SERIAL_STRLEN))?;
    let config = Config::from_resources(serial, ProfileDeviceType(device_type), cert_pem, key_pem);
    let mut buf = [0u8; BINARY_SIZE];
    config.as_bytes(&mut buf);
    Ok(buf.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_provision_tool() {
        // Same serialisation provision-tool performs: 4-byte header, 16-byte
        // serial, u32 cert len + 1024 cert bytes, u32 key len + 512 key bytes,
        // device type byte.
        let cert = b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";
        let key = b"-----BEGIN EC PRIVATE KEY-----\nBBBB\n-----END EC PRIVATE KEY-----\n";
        let out = build("ABC123DEF", 7, cert, key).unwrap();
        assert_eq!(out.len(), 1565);
        assert_eq!(&out[4..13], b"ABC123DEF");
        assert_eq!(out[13..20], [0; 7]);
        let cert_len = u32::from_le_bytes(out[20..24].try_into().unwrap()) as usize;
        assert_eq!(cert_len, cert.len());
        assert_eq!(&out[24..24 + cert_len], cert);
        let key_off = 24 + 1024;
        let key_len = u32::from_le_bytes(out[key_off..key_off + 4].try_into().unwrap()) as usize;
        assert_eq!(key_len, key.len());
        assert_eq!(&out[key_off + 4..key_off + 4 + key_len], key);
        assert_eq!(out[1564], 7);
        let parsed = Config::try_from_bytes(&out).unwrap();
        assert_eq!(parsed.serial.as_str(), "ABC123DEF");
        assert_eq!(parsed.device_type, ProfileDeviceType(7));
    }

    #[test]
    fn rejects_oversize() {
        let big = vec![b'A'; 1025];
        assert!(build("X", 1, &big, b"k").is_err());
        assert!(build("X", 1, b"c", &vec![b'A'; 513]).is_err());
        assert!(build("ABCDEFGHIJKLMNOPQ", 1, b"c", b"k").is_err());
    }
}

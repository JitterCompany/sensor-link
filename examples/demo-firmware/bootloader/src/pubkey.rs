//! Access the application public key

use demo_firmware_core::utils::crypto::PubKey;

/// The key as raw bytes. Serialization from PEM format is done by build.rs,
/// which needs demo_firmware_core with `alloc`.
#[allow(unsafe_code)]
#[used]
static PUBKEY_BYTES: [u8; PubKey::BINARY_SIZE] =
    *include_bytes!(concat!(env!("OUT_DIR"), "/app_pubkey.bin"));

/// Access the public key for verification of application firmware
pub fn app_pubkey() -> PubKey {
    // Should never fail as the file is generated from a valid key by build.rs..
    PubKey::try_from_bytes(&PUBKEY_BYTES).expect("invalid pubkey!")
}

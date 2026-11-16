//! Pre-initialized contents for specific flash sections.
//!
//! Placed by the linker script and read through the flash API, never
//! referenced from code.

use demo_firmware_core::meta::firmware::ApplicationHeader;

/// Placeholder application header: the signer fills in length and signature
#[allow(unsafe_code)]
#[link_section = ".flash_app_meta"]
#[used]
static FLASH_APP_META: [u8; ApplicationHeader::BINARY_SIZE] =
    *include_bytes!(concat!(env!("OUT_DIR"), "/flash_app_meta.bin"));

//! Bootloader metadata is stored in appropriate section

use demo_firmware_core::meta::bootloader::BootloaderMeta;
#[allow(unsafe_code)]
#[link_section = ".flash_bootloader_meta"]
#[used]
static META_BYTES: [u8; BootloaderMeta::BINARY_SIZE] =
    *include_bytes!(concat!(env!("OUT_DIR"), "/bootloader_meta.bin"));

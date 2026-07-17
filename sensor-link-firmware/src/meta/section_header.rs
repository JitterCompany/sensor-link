//! Defines unique headers used to recognize some internal firmware sections rom the binary

#[derive(Debug, Clone, Copy)]
pub enum Header {
    AppMetaV2,
    Config,
    BootloaderMeta,
}

const HEADER_MASK: u8 = 0x55;

// Headers are intended to be unique in the firmware binary.
// Their definitions are obfuscated a bit so the definition itself
// does not cause a duplicate.
const M_HEADER_IMG_V2: [u8; 4] = [
    0x4A ^ HEADER_MASK,
    0x54 ^ HEADER_MASK,
    0x52 ^ HEADER_MASK,
    0x02 ^ HEADER_MASK, // 0x02 = V2 application metadata (only accepted by newer bootloaders)
];

const M_HEADER_CFG: [u8; 4] = [
    0x4A ^ HEADER_MASK,
    0x54 ^ HEADER_MASK,
    0x52 ^ HEADER_MASK,
    0x10 ^ HEADER_MASK,
];
const M_HEADER_BOOT_META: [u8; 4] = [
    0x4A ^ HEADER_MASK,
    0x54 ^ HEADER_MASK,
    0x52 ^ HEADER_MASK,
    0x08 ^ HEADER_MASK,
];

pub fn header(header: Header) -> [u8; 4] {
    let header_bytes = match header {
        Header::AppMetaV2 => M_HEADER_IMG_V2,
        Header::Config => M_HEADER_CFG,
        Header::BootloaderMeta => M_HEADER_BOOT_META,
    };
    let mut result = [HEADER_MASK; 4];

    // 'decrypt' the obfuscated header.
    // black_box hints the compiler not to optimize this out
    for (i, b) in header_bytes.iter().enumerate() {
        result[i] = core::hint::black_box(result[i]) ^ b;
    }

    result
}

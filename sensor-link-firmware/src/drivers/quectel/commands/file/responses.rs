use atat::{atat_derive::AtatResp, heapless_bytes::Bytes, serde_at::HexStr};

use super::Filename;

#[derive(Clone, Debug, AtatResp)]
pub struct FileOpen {
    pub handle: u32,
}

/// Response format +QFLST: <filename>,<file_size>
#[derive(Clone, Debug, AtatResp)]
pub struct FileList {
    /// Name of the file.
    #[allow(dead_code)]
    pub filename: Filename,
    /// File size in bytes,
    pub file_size: u32,
}

/// Response format +QFLDS: <free_size>,<total_size>
#[derive(Clone, Debug, AtatResp)]
#[allow(dead_code)]
pub struct StorageSpace {
    pub free_size: u32,
    pub total_size: u32,
}

#[derive(Clone, Debug, AtatResp)]
pub struct FileRead {
    pub bytes: Bytes<1024>,
}

/// Response format +QFUPL: <upload_size>,<checksum>
/// +QFUPL: 683,f6e
#[derive(Clone, Debug, AtatResp)]
pub struct Upload {
    pub size: u32,

    #[allow(dead_code)]
    pub checksum: HexStr<u16>,
}

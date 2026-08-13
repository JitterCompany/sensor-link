//! File commands
//!
//! Note that file commands only need about 3-5 seconds after boot until they work.
//! Probably the filesystem is still loading internally in the modem.
//!

pub mod responses;

use atat::{atat_derive::AtatCmd, heapless_bytes::Bytes};
use heapless::String;
use nom::{
    bytes::complete::tag,
    character::complete::{digit1, line_ending},
    sequence::tuple,
    IResult,
};

use crate::drivers::quectel::NoResponse;

use responses::*;

pub type Filename = String<20>;

/// The command lists the information of a single file or all files in the specified storage.
/// NOTE: only single file listing is supported currently. Do not use wildcare patterns.
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QFLST", FileList, timeout_ms = 10000, termination = "\r")]
pub struct List {
    pub pattern: Filename,
}

/// The command lists the information of a single file or all files in the specified storage.
/// NOTE: only single file listing is supported currently. Do not use wildcare patterns.
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QFLDS", StorageSpace, timeout_ms = 10000, termination = "\r")]
pub struct Space {
    /// Storage name "UFS", "RAM", "SD"
    pub pattern: Filename,
}

/// The command opens a file and gets the file handle to be used in commands such as AT+QFREAD,
/// AT+QFWRITE, AT+QFSEEK, AT+QFPOSITION, AT+QFTUCAT and AT+QFCLOSE.
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QFOPEN", FileOpen, timeout_ms = 10000, termination = "\r")]
pub struct Open {
    pub filename: Filename,

    /// The open mode of the file.
    ///
    /// 0 If the file does not exist, it will be created. If the file exists, it will be directly
    /// opened. And both of them can be read and written.
    ///
    /// 1 If the file does not exist, it will be created. If the file exists, it will be overwritten
    /// and cleared. And both of them can be read and written.
    ///
    /// 2 If the file exists, open it and it can be read only. When the file does not exist, it
    /// will respond an error.
    pub mode: u8,
}

/// The command closes a file and ends the operation to the file. After that,
/// the file handle is released and should not be used again,
/// unless the file is opened again by AT+QFOPEN.
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QFCLOSE", NoResponse, timeout_ms = 10000, termination = "\r")]
pub struct Close {
    pub handle: u32,
}

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QFSEEK", NoResponse, timeout_ms = 10000, termination = "\r")]
pub struct Seek {
    pub handle: u32,
    /// The number of bytes of the file pointer movement.
    pub offset: u32,
    /// Pointer movement mode.
    ///
    /// 0 The beginning of the file.
    ///
    /// 1 The current position of the pointer.
    ///
    /// 2 The end of the file.
    pub position: u8,
}

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QFREAD", FileRead, timeout_ms = 10000, parse=parse_read, termination="\r")]
pub struct Read {
    pub handle: u32,
    /// Number of bytes to read from the pointer.
    pub len: u32,
}

fn parse_read(bytes: &[u8]) -> Result<FileRead, ()> {
    // Parser for +QFREAD response
    #[allow(clippy::type_complexity)]
    fn parser(i: &[u8]) -> IResult<&[u8], (&[u8], &[u8], &[u8])> {
        tuple((tag("CONNECT "), digit1, line_ending))(i)
    }

    let (leftover_bytes, (_tag, size_u8, _nl)) = parser(bytes).map_err(|_| ())?;

    // Parse bytes in size to a usize
    let size = core::str::from_utf8(size_u8)
        .map_err(|_| ())
        .and_then(|s| s.parse::<usize>().map_err(|_| ()))?;

    let bytes = &leftover_bytes[..size.min(leftover_bytes.len())];

    if leftover_bytes.len() + 1 >= size {
        Ok(FileRead {
            bytes: Bytes::from_slice(bytes).map_err(|_| ())?,
        })
    } else {
        // If the length difference is 1 we assume it is a newline at the end of the line.
        // If the difference is more something else went wrong.
        Err(())
    }
}

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QFUPL", NoResponse, timeout_ms = 10000, termination = "\r")]
pub struct UploadInit {
    pub filename: Filename,
    pub file_size: u32,
    pub timeout: u32,
}

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QFDEL", NoResponse, timeout_ms = 10000, termination = "\r")]
pub struct Delete {
    pub filename: Filename,
}

#[cfg(test)]
mod test {

    use super::*;

    #[test]
    fn parse_file_read() {
        let buf: &[u8] = &[
            // "CONNECT "
            67, 79, 78, 78, 69, 67, 84, 32, // 1 5
            49, 53, // line_ending
            13, 10, // Bytes
            45, 45, 45, 45, 45, 66, 69, 71, 73, 78, 32, 67, 69, 82, 84,
        ];

        let res = parse_read(buf).unwrap();

        let expected: [u8; 15] = [45, 45, 45, 45, 45, 66, 69, 71, 73, 78, 32, 67, 69, 82, 84];
        assert_eq!(res.bytes.as_slice(), expected.as_slice());
    }
}

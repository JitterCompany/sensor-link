//! Useful utility functions in pure Rust.

use core::str::Utf8Error;

pub mod bitwise;
#[cfg(any(test, feature = "use-std"))]
pub mod channels;
pub mod crypto;
#[cfg(feature = "use-std")]
pub mod file;
pub mod float;
pub mod lut;
pub mod num;
pub mod select;
#[cfg(any(test, feature = "use-std"))]
pub mod sync;
pub mod time;
#[cfg(feature = "use-std")]
pub mod x509;

/// Try to convert up to N bytes from the given slice into a String.
///
/// The slice is processed untill one of these conditions is met:
/// - end of the slice is reached
/// - MAX_STR_LEN bytes limit is reached
/// - a zero byte is found (C-style termination)
///
/// This can only fail if the bytes cannot be converted to UTF8.
pub fn try_string_from_bytes<const MAX_STR_LEN: usize>(
    bytes: &[u8],
) -> Result<heapless::String<MAX_STR_LEN>, Utf8Error> {
    // Find C-style zero termination (if any)
    let max_len = bytes.len().min(MAX_STR_LEN);
    let length = match bytes[..max_len]
        .iter()
        .enumerate()
        .find(|(_index, byte)| **byte == 0_u8)
    {
        Some((index, _)) => index,
        None => MAX_STR_LEN,
    };

    let str_ref = core::str::from_utf8(&bytes[..length])?;

    // length <= MAX_STR_LEN so unwrap will never fail
    Ok(heapless::String::<MAX_STR_LEN>::try_from(str_ref).unwrap())
}

/// Wrapper for a byte-slice that formats it as a string if possible and as
/// bytes otherwise.
pub struct LossyStr<'a>(pub &'a [u8]);

impl<'a> core::fmt::Debug for LossyStr<'a> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match core::str::from_utf8(self.0) {
            Ok(s) => write!(f, "{s:?}"),
            Err(_) => write!(f, "{:?}", self.0),
        }
    }
}

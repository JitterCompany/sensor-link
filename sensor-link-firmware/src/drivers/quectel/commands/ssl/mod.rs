use super::{super::types::ContextId, NoResponse};
use atat::{atat_derive::AtatCmd, serde_at::HexStr};
use heapless::String;

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QSSLCFG", NoResponse, timeout_ms = 300, termination = "\r")]
pub struct SSLCFGFile {
    #[at_arg(position = 0)]
    param: String<12>, // should be "cacert", "clientcert", or "clientkey",

    #[at_arg(position = 1)]
    pub ssl_ctx: ContextId,

    #[at_arg(position = 2)]
    path: String<20>,
}

impl SSLCFGFile {
    pub fn cacert(ssl_ctx: ContextId, path: String<20>) -> Self {
        Self {
            param: String::try_from("cacert").unwrap(),
            ssl_ctx,
            path,
        }
    }
    pub fn clientcert(ssl_ctx: ContextId, path: String<20>) -> Self {
        Self {
            param: String::try_from("clientcert").unwrap(),
            ssl_ctx,
            path,
        }
    }
    pub fn clientkey(ssl_ctx: ContextId, path: String<20>) -> Self {
        Self {
            param: String::try_from("clientkey").unwrap(),
            ssl_ctx,
            path,
        }
    }
}

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QSSLCFG", NoResponse, timeout_ms = 300, termination = "\r")]
pub struct SSLCFG {
    #[at_arg(position = 0)]
    /// should be "sslversion" or "seclevel" or "ignorelocaltime"
    param: String<16>,

    #[at_arg(position = 1)]
    ssl_ctx: ContextId,

    #[at_arg(position = 2)]
    arg: u16,
}

impl SSLCFG {
    /// Numeric type. SSL Version.
    /// 0 SSL3.0
    /// 1 TLS1.0
    /// 2 TLS1.1
    /// 3 TLS1.2
    /// 4 All
    pub fn sslversion(ssl_ctx: ContextId, sslversion: u8) -> Self {
        Self {
            param: String::try_from("sslversion").unwrap(),
            ssl_ctx,
            arg: sslversion as u16,
        }
    }

    /// Numeric format. The authentication mode.
    /// 0 No authentication
    /// 1 Manage server authentication
    /// 2 Manage server and client authentication if
    /// requested by the remoteserver
    pub fn seclevel(ssl_ctx: ContextId, seclevel: u8) -> Self {
        Self {
            param: String::try_from("seclevel").unwrap(),
            ssl_ctx,
            arg: seclevel as u16,
        }
    }

    /// How to deal with expired certificate.
    /// 0 Care about validity check for certification
    /// 1 Ignore validity check for certification
    pub fn ignorelocaltime(ssl_ctx: ContextId, ignorelocaltime: u8) -> Self {
        Self {
            param: String::try_from("ignorelocaltime").unwrap(),
            ssl_ctx,
            arg: ignorelocaltime as u16,
        }
    }
}

/// NOTE: this command cannot be used until https://github.com/BlackbirdHQ/atat/issues/166
/// is implemented.
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QSSLCFG", NoResponse, timeout_ms = 300, termination = "\r")]
#[allow(dead_code)] // unused command, kept for reference (see NOTE above)
pub struct SSLCFGCiphersuite {
    #[at_arg(position = 0)]
    /// should be "ciphersuite"
    param: String<16>,

    #[at_arg(position = 1)]
    ssl_ctx: ContextId,

    #[at_arg(position = 2)]
    arg: HexStr<u16>,
}

impl SSLCFGCiphersuite {
    pub fn _new(ssl_ctx: ContextId, ciphersuite: u16) -> Self {
        let ciphercode = HexStr {
            val: ciphersuite,
            add_0x_with_encoding: true,
            hex_in_caps: true,
            delimiter: ' ',
            delimiter_after_nibble_count: 0,
            skip_last_0_values: true,
        };

        Self {
            param: String::try_from("ciphersuite").unwrap(),
            ssl_ctx,
            arg: ciphercode,
        }
    }
}

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd(
    "+QSSLCFG=\"ciphersuite\",2,0xFFFF",
    NoResponse,
    timeout_ms = 300,
    termination = "\r"
)]
pub struct SSLCFGCiphersuiteFixed;

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd(
    "+QSSLCFG=\"ciphersuite\",1,0xFFFF",
    NoResponse,
    timeout_ms = 300,
    termination = "\r"
)]
pub struct SSLCFGCiphersuiteFixed1;

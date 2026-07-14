pub mod responses;

use atat::atat_derive::AtatCmd;
use heapless::String;

use crate::drivers::ec2x::ContextId;

use super::{file::Filename, NoResponse};

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QHTTPCFG", NoResponse, timeout_ms = 300, termination = "\r")]
pub struct ConfigContext {
    #[at_arg(position = 0)]
    param: String<15>, // should be "contextid" or "sslctxid",
    pub context_id: ContextId,
}

impl ConfigContext {
    pub fn pdp(context_id: impl Into<ContextId>) -> Self {
        Self {
            param: String::try_from("contextid").unwrap(),
            context_id: context_id.into(),
        }
    }
    pub fn ssl(context_id: impl Into<ContextId>) -> Self {
        Self {
            param: String::try_from("sslctxid").unwrap(),
            context_id: context_id.into(),
        }
    }
}

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QHTTPCFG", NoResponse, timeout_ms = 300, termination = "\r")]
pub struct ConfigResponseHeader {
    #[at_arg(position = 0)]
    param: String<15>, // should be "responseheader",
    enabled: u8,
}

impl ConfigResponseHeader {
    pub fn new(enabled: bool) -> Self {
        Self {
            param: String::try_from("responseheader").unwrap(),
            enabled: enabled as u8,
        }
    }
}

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QHTTPURL", NoResponse, timeout_ms = 300, termination = "\r")]
pub struct URL {
    #[at_arg(position = 0)]
    /// The length of URL. The range is 1-2048. Unit: byte.
    pub url_length: u16,
    /// The maximum time for inputting URL.
    /// The range is 1-65535, and the default value is 60. Unit: second.
    pub timeout: u16,
}

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QHTTPGET", NoResponse, timeout_ms = 300, termination = "\r")]
pub struct GET {
    /// The range is 1-65535, and the default value is 60. Unit: second.
    /// It is used to configure the timeout for the HTTP(S) GET response
    /// "+QHTTPGET: <err>[,<httprspcode>,<content_length>]" to be outputted after "OK" is returned.
    pub timeout: u16,
}

/// Write GET response to a file on the modem.
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QHTTPREADFILE", NoResponse, timeout_ms = 300, termination = "\r")]
pub struct ReadFile {
    /// File name. The maximum length of the file name is 80 bytes.
    pub file_name: Filename,
    /// The maximum interval time between receiving two packets of data.
    /// The range is 1-65535, and the default value is 60. Unit: second.
    pub wait_time: u16,
}

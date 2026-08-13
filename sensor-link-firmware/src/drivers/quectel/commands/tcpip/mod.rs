//! TCP IP Commands
//! Quectel_LTE_Standard_TCP(IP)_Application_Note_V1.1
//! Chapter 2
//!

pub mod urc;

use atat::atat_derive::AtatCmd;
use heapless::String;

// use responses::*;
use super::{super::types::ContextId, NoResponse};

/// 2.1.2 Activate a PDP Context activation.
///
/// Although the range of <contextID> is 1-16, the module supports **maximum three** PDP contexts activated simultaneously.
/// Depending on the network, it may take at most 150 seconds to return OK or ERROR after executing AT+QIACT.
/// Before the response is returned, other AT commands cannot be executed.
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QIACT", NoResponse, timeout_ms = 150_000, termination = "\r")]
pub struct ActivateContext {
    #[at_arg(position = 0)]
    pub cid: ContextId,
}

/// 2.1.3 Deactivate a PDP Context activation.
///
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QIDEACT", NoResponse, timeout_ms = 40_000, termination = "\r")]
pub struct DeactivateContext {
    #[at_arg(position = 0)]
    pub cid: ContextId,
}

/// 2.1.11 Ping a remote server.
///
/// The command is used to test the Internet protocol reachability of a host.
///
/// Before using ping tools, the host should activate the context corresponding to <contextID> via AT+QIACT first.
/// It will return the result within <timeout> and the default value of <timeout> is 4 seconds.
///
/// Format: AT+QPING=1,"8.8.8.8",5,5
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QPING", NoResponse, termination = "\r")]
#[allow(dead_code)] // unused command, kept for reference
pub struct Ping {
    #[at_arg(position = 0)]
    pub cid: ContextId,

    #[at_arg(position = 1)]
    pub ip_address: String<15>,

    #[at_arg(position = 2)]
    pub timeout: u8,

    #[at_arg(position = 3)]
    pub num: u8,
}

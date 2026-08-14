//! Packet Domain Commands
//! Quectel_EC25&EC21_AT_Commands_Manual_V1.3
//! Chapter 10
//!

mod responses;

use atat::atat_derive::AtatCmd;
use heapless::String;

use super::{super::types::ContextId, NoResponse};

/// 10.2 Define PDP Context
///
/// The command specifies PDP context parameters for a specific context <cid>.
/// A special form of the Write Command (AT+CGDCONT=cid) causes the values
/// for context <cid> to become undefined.
/// It is not allowed to change the definition of an already activated context.
/// The Read Command returns the current settings for each defined PDP context.
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+CGDCONT", NoResponse, termination = "\r")]
pub struct SetPDPContextDefinition {
    #[at_arg(position = 0)]
    pub cid: ContextId,
    #[at_arg(position = 1)]
    pub pdp_type: String<6>,
    #[at_arg(position = 2)]
    pub apn: String<15>,
}

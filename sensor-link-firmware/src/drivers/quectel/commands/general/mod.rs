//! General Commands can be found in
//! Quectel_EC25&EC21_AT_Commands_Manual_V1.3
//! Chapter 2
//!

mod responses;
pub mod urc;

use responses::*;

use super::NoResponse;
use atat::atat_derive::AtatCmd;

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("", NoResponse, timeout_ms = 1000, termination = "\r")]
pub struct AT;

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+CPIN?", CPINResponse, timeout_ms = 5000, termination = "\r")]
pub struct CPIN;

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+CIMI", CIMIResponse, timeout_ms = 300, termination = "\r")]
pub struct CIMI;

/// Read Network Registration Service
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+CREG?", Registration, timeout_ms = 300, termination = "\r")]
pub struct CREGQuery;

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+CREG", NoResponse, timeout_ms = 300, termination = "\r")]
pub struct CREG {
    #[at_arg(position = 0)]
    n: u8,
}

#[allow(unused)]
impl CREG {
    /// Enable network registration URC +CREG: <stat>
    pub fn enable_urc() -> Self {
        Self { n: 1 }
    }

    /// Disable network registration URC +CREG: <stat>
    pub fn disable_urc() -> Self {
        Self { n: 0 }
    }
}

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+CEREG?", Registration, timeout_ms = 300, termination = "\r")]
pub struct CEREGQuery;

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+CEREG", NoResponse, timeout_ms = 300, termination = "\r")]
pub struct CEREG {
    #[at_arg(position = 0)]
    n: u8,
}

#[allow(unused)]
impl CEREG {
    /// Enable EPS network registration status URC +CEREG: <stat>
    pub fn enable_urc() -> Self {
        Self { n: 1 }
    }

    /// Disable EPS network registration URC +CEREG: <stat>
    pub fn disable_urc() -> Self {
        Self { n: 0 }
    }
}

/// 2.1 The command delivers a product information text.
///
/// Returns some module information as the module type number and some details
/// about the firmware version.
///
/// **Notes:**
/// - The information text response of ATI9 contains the modem version and the
///   application version of the module.
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd(
    "I",
    IdentificationInformationResponse,
    value_sep = false,
    termination = "\r"
)]
#[allow(dead_code)] // unused command, kept for reference
pub struct IdentificationInformation;

/// 2.5 Manufacturer identification +CGMI
///
/// Text string identifying the manufacturer.
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+CGMI", ManufacturerId, termination = "\r")]
#[allow(dead_code)] // unused command, kept for reference
pub struct GetManufacturerId;

/// 2.6 Model identification +CGMM
///
/// Read a text string that identifies the device model.
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+CGMM", ModelId, termination = "\r")]
pub struct GetModelId;

/// 2.7 Software version identification +CGMR
///
/// Read a text string that identifies the software version of the module
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+CGMR", SoftwareVersion, termination = "\r")]
pub struct GetSoftwareVersion;

/// Set TE-TA local flow control: `AT+IFC=2,2` = hardware RTS/CTS in both
/// directions (used by the PPP driver together with the UART's flow control).
#[derive(Clone, AtatCmd)]
#[at_cmd("+IFC", NoResponse, timeout_ms = 300, termination = "\r")]
pub struct SetFlowControl {
    pub dce_by_dte: u8,
    pub dte_by_dce: u8,
}

/// 2.16 Set Command Echo Mode
///
/// value
/// * 0 Echo mode OFF
/// * 1 Echo mode ON
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("E", NoResponse, value_sep = false, termination = "\r")]
pub struct ATE {
    #[at_arg(position = 0)]
    pub value: u8,
}

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QPOWD", NoResponse, timeout_ms = 300, termination = "\r")]
pub struct PowerDown {
    #[at_arg(position = 0)]
    /// 0 = immediate power down, 1 = normal power down
    n: u8,
}
impl PowerDown {
    pub fn new() -> Self {
        Self { n: 1 }
    }
}

/// 6.3 AT+CSQ Signal Quality Report
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+CSQ", SignalQualityReport, timeout_ms = 300, termination = "\r")]
pub struct GetSignalQuality;

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+IPR", NoResponse, timeout_ms = 300, termination = "\r")]
pub struct SetBaudRate {
    pub rate: u32,
}

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("&W", NoResponse, timeout_ms = 300, termination = "\r")]
pub struct StoreBaudrate;

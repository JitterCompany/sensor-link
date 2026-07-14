//! Responses for General Commands

use atat::{atat_derive::AtatResp, heapless_bytes::Bytes};
use heapless::String;

#[derive(Debug, Clone, AtatResp)]
#[allow(dead_code)]
pub struct CPINResponse {
    pub code: heapless::String<16>,
}

#[derive(Debug, Clone, AtatResp)]
pub struct CIMIResponse {
    // IMSI is a 15-digit numeric string without quotes
    imsi: Bytes<15>,
}

impl CIMIResponse {
    /// Parse the response into a string.
    /// (Atat cannot do this directly, it does not like unquoted numerical strings)
    pub fn imsi_str(&self) -> Result<&str, atat::Error> {
        core::str::from_utf8(&self.imsi).map_err(|_| atat::Error::Parse)
    }
}

/// Identification information I
///
/// Returns some module information as the module type number and some details
/// about the firmware version.
///
/// Example:
/// Quectel
/// EC25
/// Revision: EC25EFAR06A09M4G
#[derive(Clone, Debug, AtatResp)]
#[allow(dead_code)]
pub struct IdentificationInformationResponse {
    pub app_ver: Bytes<64>,
}

/// Manufacturer identification
/// Text string identifying the manufacturer.
#[derive(Clone, Debug, AtatResp)]
#[allow(dead_code)]
pub struct ManufacturerId {
    pub id: String<64>,
    // pub id: Bytes<64>,
}

/// Model identification
/// Text string identifying the manufacturer.
#[derive(Clone, Debug, AtatResp)]
pub struct ModelId {
    pub id: String<64>,
}

/// Software version identification
/// Read a text string that identifies the software version of the module.
#[derive(Clone, Debug, AtatResp)]
#[allow(dead_code)]
pub struct SoftwareVersion {
    pub id: String<64>,
}

/// Network Registration Status for CREG, CGREG and CEREG
#[derive(Clone, Debug, AtatResp)]
pub struct Registration {
    /// Whether URC is enabled
    #[allow(dead_code)]
    pub enabled: u8,
    /// Registration network status.
    /// 0 Not registered. ME is not currently searching a new operator to register to
    /// 1 Registered, home network
    /// 2 Not registered, but ME is currently searching a new operator to register to
    /// 3 Registration denied
    /// 4 Unknown
    /// 5 Registered, roaming
    pub stat: u8,
}

/// 6.3 AT+CSQ Signal Quality Report
///
/// This command indicates the received signal strength `<rssi>` and the channel bit error rate `<ber>`.
#[derive(Clone, Debug, AtatResp)]
pub struct SignalQualityReport {
    pub rssi: i16,

    #[allow(dead_code)]
    pub ber: u8,
}

impl SignalQualityReport {
    /// Signal strength mapped to a percentage
    pub fn signal_strength(&self) -> i16 {
        let dbm = rssi_to_dbm(self.rssi);
        dbm.map(|dbm| dbm_to_percentage(dbm)).unwrap_or(-2)
    }
}

fn rssi_to_dbm(rssi: i16) -> Option<i16> {
    match rssi {
        0 => Some(-113),                                  // 0: -113 dBm or less
        1 => Some(-111),                                  // 1: -111 dBm
        2..=30 => Some(-109 + (rssi - 2) * 56 / 28),      // -109 dBm to -53 dBm
        31 => Some(-51),                                  // 31: -51 dBm or greater
        99 => None,                                       // Not known or not detectable
        100 => Some(-116),                                // 100: -116 dBm or less
        101 => Some(-115),                                // 101: -115 dBm
        102..=190 => Some(-114 + (rssi - 102) * 88 / 88), // -114 dBm to -26 dBm
        191 => Some(-25),                                 // 191: -25 dBm or greater
        199 => None,                                      // Not known or not detectable
        _ => None,                                        // Out of range
    }
}

/// Map dBm value linearly to a percentage.
/// -25 dBm = 100%
/// -116 dBm = 0%
fn dbm_to_percentage(dbm: i16) -> i16 {
    let percentage = if dbm < -116 {
        0 // Less than -116 dBm
    } else if dbm > -25 {
        100 // Greater than -25 dBm
    } else {
        // Linear mapping from -116 dBm to -25 dBm
        ((dbm + 116) * 100) / 91 // -116 to -25 is a range of 91 dBm
    };

    percentage as i16
}

#[cfg(test)]
mod test {

    use super::*;

    #[test]
    fn test_dbm_to_percentage() {
        assert_eq!(dbm_to_percentage(-25), 100);
        assert_eq!(dbm_to_percentage(-116), 0);

        let halfway = (-25 - 116) / 2;
        assert_eq!(dbm_to_percentage(halfway), 50);
    }

    #[test]
    fn test_signal_quality_report() {
        assert_eq!(rssi_to_dbm(0), Some(-113));
        assert_eq!(rssi_to_dbm(1), Some(-111));
        assert_eq!(rssi_to_dbm(25), Some(-53 - 2 * 5));
        assert_eq!(rssi_to_dbm(100), Some(-116));
        assert_eq!(rssi_to_dbm(199), None);
        assert_eq!(rssi_to_dbm(99), None);
        assert_eq!(rssi_to_dbm(200), None);
    }
}

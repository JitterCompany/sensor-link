//! Responses for General Commands

use atat::{atat_derive::AtatResp, heapless_bytes::Bytes};
use heapless::String;
use sensor_link_protocol::event::{CellInfo, Rat};

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
        dbm.map(dbm_to_percentage).unwrap_or(-2)
    }
}

/// AT+QENG="servingcell" response.
///
/// `cell` is `None` while the modem is still searching for a cell.
#[derive(Clone, Debug, AtatResp)]
pub struct ServingCell {
    pub cell: Option<CellInfo>,
}

/// Parse `+QENG: "servingcell",<state>,<rat>,...`.
///
/// The field layout depends on the RAT. CellID, LAC and TAC are unquoted hex.
pub fn parse_serving_cell(bytes: &[u8]) -> Result<ServingCell, ()> {
    let text = core::str::from_utf8(bytes).map_err(|_| ())?;
    let line = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("+QENG:"))
        .unwrap_or(text);
    let mut fields = line.split(',').map(|field| field.trim().trim_matches('"'));

    if fields.next() != Some("servingcell") {
        return Err(());
    }
    let _state = fields.next().ok_or(())?;
    let Some(rat) = fields.next() else {
        return Ok(ServingCell { cell: None });
    };

    let mut next = || fields.next().ok_or(());
    let cell = match rat {
        "GSM" | "WCDMA" => {
            let rat = if rat == "GSM" { Rat::Gsm } else { Rat::Umts };
            let mcc = parse_dec(next()?)?;
            let mnc = parse_dec(next()?)?;
            let tac = parse_hex(next()?)?;
            let ci = parse_hex(next()?)?;
            CellInfo {
                rat,
                mcc,
                mnc,
                tac,
                ci,
                rsrp: None,
            }
        }
        "LTE" | "CAT-M" | "eMTC" | "CAT-NB" | "NBIoT" => {
            let rat = match rat {
                "LTE" => Rat::Lte,
                "CAT-M" | "eMTC" => Rat::LteM,
                _ => Rat::NbIot,
            };
            let _is_tdd = next()?;
            let mcc = parse_dec(next()?)?;
            let mnc = parse_dec(next()?)?;
            let ci = parse_hex(next()?)?;
            // PCID, EARFCN, band, UL bandwidth, DL bandwidth
            for _ in 0..5 {
                next()?;
            }
            let tac = parse_hex(next()?)?;
            let rsrp = next().ok().and_then(|rsrp| rsrp.parse().ok());
            CellInfo {
                rat,
                mcc,
                mnc,
                tac,
                ci,
                rsrp,
            }
        }
        _ => return Err(()),
    };
    Ok(ServingCell { cell: Some(cell) })
}

fn parse_dec(field: &str) -> Result<u16, ()> {
    field.parse().map_err(|_| ())
}

fn parse_hex(field: &str) -> Result<u32, ()> {
    u32::from_str_radix(field, 16).map_err(|_| ())
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
    if dbm < -116 {
        0 // Less than -116 dBm
    } else if dbm > -25 {
        100 // Greater than -25 dBm
    } else {
        // Linear mapping from -116 dBm to -25 dBm
        ((dbm + 116) * 100) / 91 // -116 to -25 is a range of 91 dBm
    }
}

#[cfg(test)]
mod test {

    use super::*;

    // Example lines from the EC2x AT manual, not captured from a modem.
    #[test]
    fn serving_cell_lte() {
        let line = br#"+QENG: "servingcell","NOCONN","LTE","FDD",204,08,1A2D001,123,6300,20,3,3,2B11,-95,-10,-65,15,40"#;
        let cell = parse_serving_cell(line).unwrap().cell.unwrap();
        assert_eq!(
            cell,
            CellInfo {
                rat: Rat::Lte,
                mcc: 204,
                mnc: 8,
                tac: 0x2B11,
                ci: 0x1A2D001,
                rsrp: Some(-95),
            }
        );
    }

    #[test]
    fn serving_cell_gsm() {
        let line = br#"+QENG: "servingcell","NOCONN","GSM",204,16,4B1,7D3F,23,62,0,-61,255,255,255,67,67,1,-,-,-,-,-,-,-,-,-,-"#;
        let cell = parse_serving_cell(line).unwrap().cell.unwrap();
        assert_eq!(
            cell,
            CellInfo {
                rat: Rat::Gsm,
                mcc: 204,
                mnc: 16,
                tac: 0x4B1,
                ci: 0x7D3F,
                rsrp: None,
            }
        );
    }

    #[test]
    fn serving_cell_searching() {
        let line = br#"+QENG: "servingcell","SEARCH""#;
        assert!(parse_serving_cell(line).unwrap().cell.is_none());
    }

    #[test]
    fn serving_cell_without_prefix() {
        let line = br#""servingcell","CONNECT","LTE","FDD",204,08,1A2D001,123,6300,20,3,3,2B11,-95,-10,-65,15,40"#;
        assert!(parse_serving_cell(line).unwrap().cell.is_some());
    }

    #[test]
    fn serving_cell_malformed() {
        assert!(parse_serving_cell(br#"+QENG: "servingcell","NOCONN","LTE","FDD",204"#).is_err());
        assert!(parse_serving_cell(br#"+QENG: "neighbourcell intra","LTE""#).is_err());
        assert!(parse_serving_cell(br#"+QENG: "servingcell","NOCONN","NR5G-SA""#).is_err());
    }

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

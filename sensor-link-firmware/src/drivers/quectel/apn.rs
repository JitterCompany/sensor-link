pub type APN = &'static str;

pub const FALLBACK: APN = "internet";

// Table mapping PLMN ID to APN
//
// see https://android.googlesource.com/device/sample/+/main/etc/apns-full-conf.xml
//
// Note: PLMN ID is a combination of MCC and MNC.
// This is the first 5 or 6 digits of the IMSI (MNC can be 2 or 3 digits)
const TABLE: &[(&str, APN)] = &[
    ("20408", "internet"), // KPN Mobiel Internet
    ("20412", "internet"), // KPN Mobiel Internet
    ("20469", "internet"), // KPN Lab Internet
    ("23201", "Data0575"), // A1 via Simhuis
];

pub fn lookup(imsi: &str) -> Result<APN, APN> {
    for (plmn_id, apn) in TABLE.iter() {
        if imsi.starts_with(plmn_id) {
            log::debug!("IMSI {imsi} matched {plmn_id}: select APN '{apn}'");
            return Ok(*apn);
        }
    }
    log::warn!("IMSI {imsi} did not match any APN");
    Err(FALLBACK)
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn lookup_known_imsi() {
        // PLMN for this IMSI is in the table, should match
        assert_eq!("internet", lookup("204080000000000").unwrap());
    }

    #[test]
    fn lookup_unknown_imsi() {
        // This IMSI is not in the table, should fallback
        assert_eq!(FALLBACK, lookup("012340123456789").unwrap_err());
    }
}

//! Operator input checks. All of these are advisory: the UI warns and lets
//! the operator override, mirroring the shell flow.

/// Trim and uppercase a scanned UID (scanners sometimes deliver lowercase).
pub fn normalize_uid(raw: &str) -> String {
    raw.trim().to_ascii_uppercase()
}

/// `None` when the UID has the expected length, otherwise a warning text.
pub fn uid_warning(uid: &str, min: usize, max: usize) -> Option<String> {
    let n = uid.chars().count();
    (n < min || n > max).then(|| format!("UID '{uid}' is {n} characters (expected {min}-{max})."))
}

/// SIM ICCID: 20 digits with a valid Luhn-10 check digit.
pub fn iccid_valid(icc: &str) -> bool {
    let icc = icc.trim();
    if icc.len() != 20 || !icc.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let sum: u32 = icc
        .bytes()
        .rev()
        .enumerate()
        .map(|(i, b)| {
            let d = u32::from(b - b'0');
            if i % 2 == 1 {
                let dd = d * 2;
                if dd > 9 { dd - 9 } else { dd }
            } else {
                d
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

pub const ICCID_WARNING: &str = "The SIM ICCID failed validation (expected 20 digits with a valid \
Luhn-10 checksum). A wrong ICCID means the SIM may not get activated and the device-to-SIM \
record in the log will be incorrect, which later blocks stopping or upgrading this SIM's \
subscription. Only override if you are certain the scanner mis-read a known-good card.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luhn() {
        // 19 digits + Luhn check digit.
        assert!(iccid_valid("89310410106543789305"));
        assert!(!iccid_valid("89310410106543789306"));
        assert!(!iccid_valid("8931041010654378930"));
        assert!(!iccid_valid("8931041010654378930a"));
        assert!(iccid_valid(" 89310410106543789305 "));
    }

    #[test]
    fn uid() {
        assert_eq!(normalize_uid(" abc123def\n"), "ABC123DEF");
        assert!(uid_warning("ABC123DEF", 5, 9).is_none());
        assert!(uid_warning("ABCDE", 5, 9).is_none());
        assert!(uid_warning("ABCD", 5, 9).is_some());
        assert!(uid_warning("ABC1234567", 5, 9).is_some());
    }
}

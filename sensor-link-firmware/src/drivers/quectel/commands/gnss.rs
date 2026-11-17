//! GNSS commands (EC2x GNSS application note)
//!
//! NMEA output goes to the USB NMEA port by default (`AT+QGPSCFG="outport"`),
//! so it never reaches our UART.

use atat::atat_derive::{AtatCmd, AtatResp};
use sensor_link_protocol::event::GnssFix;

use super::NoResponse;

/// AT+QGPS? GNSS on or off
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QGPS?", GnssState, timeout_ms = 300, termination = "\r")]
pub struct GetGnssState;

#[derive(Clone, Debug, AtatResp)]
pub struct GnssState {
    /// 0 off, 1 on
    pub on: u8,
}

/// AT+QGPS=1 Turn GNSS on in standalone mode.
/// Fails with `+CME ERROR: 504` when it is already on.
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QGPS=1", NoResponse, timeout_ms = 300, termination = "\r")]
pub struct GnssStart;

/// AT+QGPSEND Turn GNSS off.
/// Fails with `+CME ERROR: 505` when it is already off.
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QGPSEND", NoResponse, timeout_ms = 300, termination = "\r")]
pub struct GnssEnd;

/// AT+QGPSLOC=2 Position in decimal degrees.
/// Fails with `+CME ERROR: 516` until there is a fix.
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd(
    "+QGPSLOC=2",
    GnssLocation,
    timeout_ms = 300,
    parse = parse_gnss_location,
    termination = "\r"
)]
pub struct GetGnssLocation;

#[derive(Clone, Debug, AtatResp)]
pub struct GnssLocation {
    pub fix: GnssFix,
}

/// Parse `+QGPSLOC: <UTC>,<lat>,<lon>,<HDOP>,<alt>,<fix>,<COG>,<spkm>,<spkn>,<date>,<nsat>`.
pub fn parse_gnss_location(bytes: &[u8]) -> Result<GnssLocation, ()> {
    let text = core::str::from_utf8(bytes).map_err(|_| ())?;
    let line = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("+QGPSLOC:"))
        .unwrap_or(text);
    let mut fields = line.split(',').map(str::trim);
    let mut next = || fields.next().ok_or(());

    let _utc = next()?;
    let lat = next()?.parse().map_err(|_| ())?;
    let lon = next()?.parse().map_err(|_| ())?;
    let hdop = next()?.parse().map_err(|_| ())?;
    let alt = next()?.parse().map_err(|_| ())?;
    // fix mode, course, speed km/h, speed knots, date
    for _ in 0..5 {
        next()?;
    }
    let sat = next()?.parse().map_err(|_| ())?;
    Ok(GnssLocation {
        fix: GnssFix {
            lat,
            lon,
            alt,
            hdop,
            sat,
        },
    })
}

#[cfg(test)]
mod test {
    use super::*;

    // Example from the EC2x GNSS application note, not captured from a modem.
    #[test]
    fn location_from_manual() {
        let fix = parse_gnss_location(
            b"+QGPSLOC: 061951.000,31.16855,121.39223,1.0,15.0,2,0.00,0.0,0.0,110513,09",
        )
        .unwrap()
        .fix;
        assert_eq!(
            fix,
            GnssFix {
                lat: 31.16855,
                lon: 121.39223,
                alt: 15.0,
                hdop: 1.0,
                sat: 9,
            }
        );
    }

    #[test]
    fn location_south_west() {
        let fix = parse_gnss_location(
            b"+QGPSLOC: 235959.000,-33.86882,-151.20929,0.7,-2.5,3,123.45,10.5,5.7,311226,12",
        )
        .unwrap()
        .fix;
        assert_eq!(fix.lat, -33.86882);
        assert_eq!(fix.lon, -151.20929);
        assert_eq!(fix.alt, -2.5);
        assert_eq!(fix.sat, 12);
    }

    #[test]
    fn location_rejects_short_line() {
        assert!(parse_gnss_location(b"+QGPSLOC: 061951.000,31.16855,121.39223").is_err());
        assert!(parse_gnss_location(b"").is_err());
    }
}

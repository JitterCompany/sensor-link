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

/// AT+QGPSCFG="nmeasrc",1 Allow reading NMEA sentences with AT+QGPSGNMEA.
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd(
    "+QGPSCFG=\"nmeasrc\",1",
    NoResponse,
    timeout_ms = 300,
    termination = "\r"
)]
pub struct EnableNmeaSource;

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

/// AT+QGPSGNMEA="GSV" Satellites in view, as NMEA GSV sentences.
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd(
    "+QGPSGNMEA=\"GSV\"",
    SatellitesInView,
    timeout_ms = 300,
    parse = parse_satellites_in_view,
    termination = "\r"
)]
pub struct GetSatellitesInView;

/// Summary of the GSV sentences of all constellations.
#[derive(Clone, Debug, Default, PartialEq, Eq, AtatResp)]
pub struct SatellitesInView {
    /// Satellites in view, all constellations
    pub count: u8,
    /// Satellites with a signal (SNR reported)
    pub tracked: u8,
    /// Highest SNR in dB-Hz, 0 when none
    pub best_snr: u8,
}

/// Parse `+QGPSGNMEA: $xxGSV,<msgs>,<msg>,<in view>{,<prn>,<elev>,<az>,<snr>}*hh` lines.
///
/// The "in view" field repeats in every sentence of a constellation, so it is
/// counted from the first sentence (`<msg>` = 1) only. A reply without any
/// GSV sentence is an error, so 0 in view means the modem reported 0.
pub fn parse_satellites_in_view(bytes: &[u8]) -> Result<SatellitesInView, ()> {
    let text = core::str::from_utf8(bytes).map_err(|_| ())?;
    let mut summary = SatellitesInView::default();
    let mut sentences = 0;
    for line in text.lines() {
        let line = line.trim();
        let sentence = line.strip_prefix("+QGPSGNMEA:").unwrap_or(line).trim();
        let Some(sentence) = sentence.strip_prefix('$') else {
            continue;
        };
        let body = sentence.split('*').next().unwrap_or("");
        let mut fields = body.split(',');
        if !fields.next().is_some_and(|tag| tag.ends_with("GSV")) {
            continue;
        }
        sentences += 1;
        let _msgs = fields.next();
        let msg = fields.next();
        let in_view: u8 = fields.next().and_then(|f| f.parse().ok()).unwrap_or(0);
        if msg == Some("1") {
            summary.count = summary.count.saturating_add(in_view);
        }
        let blocks: heapless::Vec<&str, 20> = fields.collect();
        for block in blocks.chunks(4) {
            if let Some(snr) = block.get(3).and_then(|f| f.parse::<u8>().ok()) {
                summary.tracked = summary.tracked.saturating_add(1);
                summary.best_snr = summary.best_snr.max(snr);
            }
        }
    }
    if sentences == 0 {
        let head = text.get(..text.len().min(80)).unwrap_or(text);
        log::warn!(target: "quectel", "No GSV sentence in {} bytes: {head:?}", text.len());
        return Err(());
    }
    Ok(summary)
}

#[cfg(test)]
mod test {
    use super::*;

    // Built from the GSV format in the NMEA 0183 spec, not captured from a modem.
    #[test]
    fn satellites_in_view() {
        let lines =
            b"+QGPSGNMEA: $GPGSV,2,1,07,02,45,123,32,05,30,200,,12,60,045,41,15,10,300,*7A\r\n\
+QGPSGNMEA: $GPGSV,2,2,07,20,20,090,25,24,05,180,,29,70,010,*4B\r\n\
+QGPSGNMEA: $GLGSV,1,1,02,65,33,100,,66,40,250,*60\r\n";
        assert_eq!(
            parse_satellites_in_view(lines).unwrap(),
            SatellitesInView {
                count: 9,
                tracked: 3,
                best_snr: 41,
            }
        );
    }

    #[test]
    fn satellites_none_in_view() {
        let lines = b"+QGPSGNMEA: $GPGSV,1,1,00*79\r\n";
        assert_eq!(
            parse_satellites_in_view(lines).unwrap(),
            SatellitesInView::default()
        );
    }

    #[test]
    fn satellites_without_prefix() {
        let lines = b"$GPGSV,1,1,02,02,45,123,32,05,30,200,*7A\r\n";
        assert_eq!(
            parse_satellites_in_view(lines).unwrap(),
            SatellitesInView {
                count: 2,
                tracked: 1,
                best_snr: 32,
            }
        );
    }

    #[test]
    fn satellites_without_gsv_is_an_error() {
        assert!(parse_satellites_in_view(b"").is_err());
        assert!(parse_satellites_in_view(b"+QGPSGNMEA: $GPGGA,,,,,,0,,,,,,,,*66\r\n").is_err());
    }

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

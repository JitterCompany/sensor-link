use chrono::{DateTime, NaiveDateTime, TimeZone, Timelike, Utc};
/// Convert to chrono UTC date with fallback to MIN_DATE
#[inline]
pub fn convert_ms_to_utc_datetime(timestamp_ms: i64) -> DateTime<Utc> {
    datetime_from_millis(timestamp_ms)
        .map_err(|e| tracing::error!("{}. Using default date.", e))
        .unwrap_or(Utc.from_utc_datetime(&NaiveDateTime::MIN))
}

#[inline]
pub fn datetime_from_millis(millis: i64) -> anyhow::Result<DateTime<Utc>> {
    let ts_secs = millis / 1000;
    let ts_ns = ((millis % 1000) as u32) * 1_000_000;
    DateTime::from_timestamp(ts_secs, ts_ns).ok_or(anyhow::anyhow!("invalid timestamp {millis}"))
}

#[inline]
pub fn datetime_from_micros(micros: i64) -> anyhow::Result<DateTime<Utc>> {
    let ts_secs = micros / 1_000_000;
    let ts_ns = ((micros % 1_000_000) as u32) * 1_000;
    DateTime::from_timestamp(ts_secs, ts_ns).ok_or(anyhow::anyhow!("invalid timestamp {micros}"))
}

pub fn datetime_from_micros_vec(t: &[i64]) -> anyhow::Result<Vec<DateTime<Utc>>> {
    t.iter().map(|t| datetime_from_micros(*t)).collect()
}

/// Normalize a license_start timestamp to noon UTC of the same month.
/// The frontend DatePicker sends midnight local time, which can shift to the previous
/// month in UTC (e.g. March 1 00:00 CET = Feb 28 23:00 UTC). By normalizing to noon
/// on the 1st of the month, we avoid timezone boundary issues.
/// Normalize a license_start timestamp to noon UTC on the 1st of that month.
/// The frontend DatePicker sends midnight local time, which can shift to the previous
/// month in UTC (e.g. March 1 00:00 CET = Feb 28 23:00 UTC). By normalizing to noon
/// on the 1st of the month, we avoid timezone boundary issues.
pub fn normalize_license_start(millis: i64) -> anyhow::Result<i64> {
    use chrono::{Datelike, NaiveDate, NaiveTime};

    let dt = datetime_from_millis(millis)?;
    // The timestamp might have shifted to the previous month due to timezone.
    // Heuristic: if the time is in the last hours of a month (>= 21:00 UTC)
    // on the last days, it was likely meant to be the 1st of the next month at
    // midnight in a timezone east of UTC. Shift forward to get the intended month.
    let corrected = if dt.hour() >= 21 && dt.day() >= 28 {
        dt + chrono::Duration::hours(4)
    } else {
        dt
    };

    let noon = NaiveDate::from_ymd_opt(corrected.year(), corrected.month(), 1)
        .ok_or_else(|| anyhow::anyhow!("invalid date"))?
        .and_time(NaiveTime::from_hms_opt(12, 0, 0).unwrap());
    Ok(Utc.from_utc_datetime(&noon).timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, NaiveDate, NaiveTime};

    /// Helper: millis for a date/time in UTC.
    fn millis_at(year: i32, month: u32, day: u32, hour: u32) -> i64 {
        let dt = NaiveDate::from_ymd_opt(year, month, day)
            .unwrap()
            .and_time(NaiveTime::from_hms_opt(hour, 0, 0).unwrap());
        Utc.from_utc_datetime(&dt).timestamp_millis()
    }

    /// Expected result: noon UTC on the 1st of the given month/year.
    fn noon_utc(year: i32, month: u32) -> i64 {
        millis_at(year, month, 1, 12)
    }

    #[test]
    fn normalize_noon_utc_is_idempotent() {
        // Already noon UTC on the 1st — should not change
        let ts = noon_utc(2025, 3);
        assert_eq!(normalize_license_start(ts).unwrap(), ts);
    }

    #[test]
    fn normalize_midnight_utc_same_month() {
        // March 1 00:00 UTC — should normalize to March 1 12:00 UTC
        let ts = millis_at(2025, 3, 1, 0);
        let result = normalize_license_start(ts).unwrap();
        assert_eq!(result, noon_utc(2025, 3));
    }

    #[test]
    fn normalize_midnight_cet_march_shifted_to_feb_utc() {
        // March 1 00:00 CET = Feb 28 23:00 UTC
        let ts = millis_at(2025, 2, 28, 23);
        let result = normalize_license_start(ts).unwrap();
        let dt = datetime_from_millis(result).unwrap();
        assert_eq!(dt.month(), 3, "should be corrected to March");
        assert_eq!(result, noon_utc(2025, 3));
    }

    #[test]
    fn normalize_midnight_cet_january_shifted_to_dec_utc() {
        // Jan 1 00:00 CET = Dec 31 23:00 UTC (previous year!)
        let ts = millis_at(2024, 12, 31, 23);
        let result = normalize_license_start(ts).unwrap();
        let dt = datetime_from_millis(result).unwrap();
        assert_eq!(dt.month(), 1, "should be corrected to January");
        assert_eq!(dt.year(), 2025, "should be corrected to next year");
        assert_eq!(result, noon_utc(2025, 1));
    }

    #[test]
    fn normalize_midnight_cest_july_shifted_to_june_utc() {
        // July 1 00:00 CEST (UTC+2) = June 30 22:00 UTC
        let ts = millis_at(2025, 6, 30, 22);
        let result = normalize_license_start(ts).unwrap();
        let dt = datetime_from_millis(result).unwrap();
        assert_eq!(dt.month(), 7, "should be corrected to July");
        assert_eq!(result, noon_utc(2025, 7));
    }

    #[test]
    fn normalize_mid_month_date_stays_same_month() {
        // March 15 14:00 UTC — clearly March, should stay March
        let ts = millis_at(2025, 3, 15, 14);
        let result = normalize_license_start(ts).unwrap();
        assert_eq!(result, noon_utc(2025, 3));
    }

    #[test]
    fn normalize_early_evening_end_of_month_not_shifted() {
        // Feb 28 20:00 UTC — this is genuinely February (before the 21:00 threshold)
        let ts = millis_at(2025, 2, 28, 20);
        let result = normalize_license_start(ts).unwrap();
        assert_eq!(result, noon_utc(2025, 2));
    }
}

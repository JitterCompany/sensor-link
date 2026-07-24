use chrono::{DateTime, NaiveDateTime};

pub enum TimeError {
    InvalidTimestamp(i64),
}

#[inline]
pub fn datetime_from_millis(millis: i64) -> Result<NaiveDateTime, TimeError> {
    let ts_secs = millis / 1000;
    let ts_ns = ((millis % 1000) as u32) * 1_000_000;
    DateTime::from_timestamp(ts_secs, ts_ns)
        .map(|dt| dt.naive_utc())
        .ok_or(TimeError::InvalidTimestamp(millis))
}

#[inline]
pub fn datetime_from_micros(micros: i64) -> Result<NaiveDateTime, TimeError> {
    let ts_secs = micros / 1000000;
    let ts_ns = ((micros % 1000000) as u32) * 1_000;
    DateTime::from_timestamp(ts_secs, ts_ns)
        .map(|dt| dt.naive_utc())
        .ok_or(TimeError::InvalidTimestamp(micros))
}

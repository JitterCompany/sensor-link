use chrono::NaiveDateTime;

#[inline]
pub fn datetime_from_millis(millis: i64) -> Result<NaiveDateTime, ()> {
    let ts_secs = millis / 1000;
    let ts_ns = ((millis % 1000) as u32) * 1_000_000;
    NaiveDateTime::from_timestamp_opt(ts_secs, ts_ns).ok_or(())
}

#[inline]
pub fn datetime_from_micros(micros: i64) -> Result<NaiveDateTime, ()> {
    let ts_secs = micros / 1000000;
    let ts_ns = ((micros % 1000000) as u32) * 1_000;
    NaiveDateTime::from_timestamp_opt(ts_secs, ts_ns).ok_or(())
}

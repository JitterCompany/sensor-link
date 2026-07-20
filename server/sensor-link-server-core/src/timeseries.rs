use std::cmp::Ordering;

use crate::{
    sensor_data::{MPSensorData, TimeResolution},
    store_traits::{Result, SensorDataStore},
    DataKind, DataStoreId, TimeRange,
};

#[derive(Debug, Clone)]
pub enum DownsampleBucketMethod {
    MaxPoints(usize),
    BucketWidthMS(i64),
}

#[derive(Debug, Clone)]
pub enum DownsamplePreference {
    Percentage,
    Average,
}

#[derive(Debug, Clone)]
pub struct TimeseriesQuery<'q, DC: DataKind> {
    pub channel: DC,
    pub measuring_point_id: &'q DataStoreId,
    pub time_range: TimeRange,
    pub sort: Option<i32>,
    pub limit: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct DownsampleOptions {
    pub method: DownsampleBucketMethod,
    pub preference: DownsamplePreference,
}

/// Maximum number of samples that can be returned in a downsampled result.
pub const MAX_NUM_SAMPLES: usize = 5000;

/// Represents a time bucket width with associated helper methods.
/// Bucket widths are predefined to ensure consistent and intuitive time intervals
/// ranging from 1ms to 30 days.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BucketWidth(i64);

impl BucketWidth {
    /// All available bucket widths in milliseconds, from smallest to largest
    pub const ALL: [BucketWidth; 22] = [
        BucketWidth(1),             // 1ms
        BucketWidth(5),             // 5ms
        BucketWidth(10),            // 10ms
        BucketWidth(50),            // 50ms
        BucketWidth(100),           // 100ms
        BucketWidth(500),           // 500ms
        BucketWidth(1_000),         // 1s
        BucketWidth(5_000),         // 5s
        BucketWidth(10_000),        // 10s
        BucketWidth(30_000),        // 30s
        BucketWidth(60_000),        // 1m
        BucketWidth(300_000),       // 5m
        BucketWidth(600_000),       // 10m
        BucketWidth(900_000),       // 15m
        BucketWidth(1_800_000),     // 30m
        BucketWidth(3_600_000),     // 1h
        BucketWidth(10_800_000),    // 3h
        BucketWidth(21_600_000),    // 6h
        BucketWidth(43_200_000),    // 12h
        BucketWidth(86_400_000),    // 1d
        BucketWidth(604_800_000),   // 1w
        BucketWidth(2_592_000_000), // 30d
    ];

    /// Returns the smallest bucket width (1ms)
    pub const fn smallest() -> Self {
        Self::ALL[0]
    }

    /// Returns the largest bucket width (30d)
    pub const fn largest() -> Self {
        Self::ALL[Self::ALL.len() - 1]
    }

    /// Returns the millisecond value
    pub const fn as_millis(&self) -> i64 {
        self.0
    }

    /// Find the smallest bucket width that's >= the given width in milliseconds
    pub fn find_next_width(width_ms: i64) -> Self {
        Self::ALL
            .iter()
            .find(|&&bucket| bucket.0 >= width_ms)
            .copied()
            .unwrap_or_else(Self::largest)
    }

    /// Returns a human-readable description of the bucket width
    pub fn description(&self) -> &'static str {
        match self.0 {
            1 => "1ms",
            5 => "5ms",
            10 => "10ms",
            50 => "50ms",
            100 => "100ms",
            500 => "500ms",
            1_000 => "1s",
            5_000 => "5s",
            10_000 => "10s",
            30_000 => "30s",
            60_000 => "1m",
            300_000 => "5m",
            600_000 => "10m",
            900_000 => "15m",
            1_800_000 => "30m",
            3_600_000 => "1h",
            10_800_000 => "3h",
            21_600_000 => "6h",
            43_200_000 => "12h",
            86_400_000 => "1d",
            604_800_000 => "1w",
            2_592_000_000 => "30d",
            _ => "custom",
        }
    }
}

/// Calculate the ideal bucket width based on the desired number of samples.
///
/// The function follows these rules:
/// 1. For empty or single-point input, returns the smallest bucket width (1ms)
/// 2. For max_num_samples = 1, returns the full time range as a single bucket
/// 3. Otherwise, finds the smallest predefined bucket width that:
///    - Is large enough to ensure we don't exceed max_num_samples
///    - Provides intuitive time intervals (e.g., 1m, 5m, 1h)
///
/// # Arguments
/// * `t` - Slice of timestamps in milliseconds
/// * `max_num_samples` - Maximum number of samples desired in the result
///
/// # Returns
/// The calculated bucket width in milliseconds
pub fn calc_bucket_width(t: &[i64], max_num_samples: usize) -> i64 {
    if t.len() <= 1 {
        return BucketWidth::smallest().as_millis();
    }

    let start_time_ms = t[0];
    let end_time_ms = t[t.len() - 1];
    let time_range = end_time_ms - start_time_ms;

    if max_num_samples == 1 {
        return time_range;
    }

    // Calculate the ideal bucket width based on the desired number of samples
    let ideal_width = (time_range as f64 / (max_num_samples - 1) as f64).ceil() as i64;

    // Calculate the minimum interval in the source data
    let min_interval = t
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|&i| i > 0)
        .min()
        .unwrap_or(1);

    // Ensure the bucket width is not smaller than the minimum interval
    let adjusted_width = ideal_width.max(min_interval);

    // Find the smallest predefined bucket width that's >= our adjusted width
    BucketWidth::find_next_width(adjusted_width).as_millis()
}

/// Get timeseries data for a device and channel
pub async fn get_timeseries<'q, DS, DC, DT>(
    datastore: &DS,
    query: TimeseriesQuery<'q, DC>,
    options: Option<DownsampleOptions>,
) -> Result<MPSensorData>
where
    DS: SensorDataStore<DataChannel = DC, DataType = DT> + ?Sized,
    DC: DataKind,
    DT: DataKind,
{
    // Get the raw data
    let columndata = datastore
        .sensor_data_for_measuring_point_with_options(
            query.channel,
            if options.is_some() {
                None
            } else {
                Some(TimeResolution::Native)
            },
            query.measuring_point_id,
            &query.time_range,
            true,
            query.sort,
            query.limit,
        )
        .await?;

    // Downsample if requested
    if let Some(options) = options {
        Ok(downsample_to_bucket_width_by_percentage_or_average(
            columndata, options,
        ))
    } else {
        Ok(columndata)
    }
}

/// Downsample the data to the specified bucket width
/// Since buckets are aligned to the bucket width, we may end up with one extra bucket.
fn downsample_to_bucket_width_by_percentage_or_average(
    series: MPSensorData,
    options: DownsampleOptions,
) -> MPSensorData {
    if series.time.is_empty() {
        return series;
    }

    let method = options.method;
    let preference = options.preference;

    let t = &series.time;
    let v = &series.values;
    let n = t.len();
    let start_time_ms = *t.first().unwrap();
    let end_time_ms = *t.last().unwrap();

    let (bucket_width_ms, max_num_samples) = match method {
        DownsampleBucketMethod::MaxPoints(n) => (calc_bucket_width(t, n), n),
        DownsampleBucketMethod::BucketWidthMS(width_ms) => {
            // Check that given bucket width does not result in too many samples
            let mut num_samples = ((end_time_ms - start_time_ms) / width_ms) as usize;
            if num_samples > MAX_NUM_SAMPLES {
                num_samples = MAX_NUM_SAMPLES;
                let adjusted_width_ms = (end_time_ms - start_time_ms) / num_samples as i64;
                (adjusted_width_ms, num_samples)
            } else {
                (width_ms, num_samples)
            }
        }
    };

    if n <= max_num_samples {
        return series;
    }

    tracing::debug!("Using bucket width: {bucket_width_ms} ms");

    let mut downsampled = MPSensorData {
        measuring_point_id: series.measuring_point_id,
        time: Vec::with_capacity(max_num_samples),
        values: Vec::with_capacity(max_num_samples),
        min: Vec::with_capacity(max_num_samples),
        max: Vec::with_capacity(max_num_samples),
        freq: series
            .freq
            .as_ref()
            .map(|_| Vec::with_capacity(max_num_samples)),
        perc: series
            .perc
            .as_ref()
            .map(|_| Vec::with_capacity(max_num_samples)),
        seconds_per_sample: bucket_width_ms as f32 / 1000.,
    };

    let (mut start_idx, last_idx) = match method {
        // Buckets for downsampling should start at timestamp of which modulo the bucket width (which will be based on the configured static interval) gives 0.
        // The input data may not start precisely on such timestamp and so incomplete static interval (buckets) at the start and the end
        // of the data series may be ignored.
        DownsampleBucketMethod::BucketWidthMS(width_ms) => {
            // Find the first timestamp for which *t % width_ms is smaller then the previous one.
            // This should be the start of the first full bucket.
            let mut prev_modulo = 0;
            let start_idx = t
                .iter()
                .position(|t| {
                    let modulo = *t % width_ms;
                    if modulo < prev_modulo {
                        true
                    } else {
                        prev_modulo = modulo;
                        false
                    }
                })
                .unwrap_or(0);

            // Find last timestamp of last full bucket
            let mut last_modulo = width_ms;
            let end_idx = t
                .iter()
                .rposition(|t| {
                    let modulo = *t % width_ms;
                    if modulo > last_modulo {
                        true
                    } else {
                        last_modulo = modulo;
                        false
                    }
                })
                .unwrap_or(n - 1);

            (start_idx, end_idx)
        }
        _ => (0, n),
    };

    while start_idx < last_idx {
        let bucket = Bucket::new(t, start_idx, bucket_width_ms);
        let end_idx = bucket.end_idx;

        let mean = v.get(bucket.range()).map(average);
        if let Some(mean) = mean {
            let min = series
                .min
                .get(bucket.range())
                .and_then(|min_bucket| min_bucket.iter().cloned().reduce(f32::min))
                .unwrap_or(mean);
            let max = series
                .max
                .get(bucket.range())
                .and_then(|max_bucket| max_bucket.iter().cloned().reduce(f32::max))
                .unwrap_or(mean);
            let freq = series
                .freq
                .as_deref()
                .and_then(|freq| freq.get(bucket.range()))
                .map(average);
            let perc = series
                .perc
                .as_deref()
                .and_then(|perc| perc.get(bucket.range()))
                .map(average);
            let perc_bucket = series
                .perc
                .as_ref()
                .and_then(|perc| perc.get(bucket.range()));
            match (
                &preference,
                perc_bucket.and_then(|perc| {
                    series
                        .max
                        .get(bucket.range())
                        .and_then(|max_values| find_sample_with_max_percentage(perc, max_values))
                }),
            ) {
                (DownsamplePreference::Percentage, Some((max_index, max_perc))) => {
                    downsampled.add_sample(
                        bucket.ts,
                        mean,
                        perc_bucket
                            .and_then(|percs| {
                                series.min.get(bucket.range()).and_then(|min_values| {
                                    find_sample_with_min_percentage(percs, min_values)
                                })
                            })
                            .and_then(|(min_index, _)| {
                                series.min.get(start_idx + min_index).copied()
                            })
                            .unwrap_or(min),
                        series
                            .max
                            .get(start_idx + max_index)
                            .copied()
                            .unwrap_or(max),
                        series
                            .freq
                            .as_ref()
                            .and_then(|freq| freq.get(start_idx + max_index).copied()),
                        Some(max_perc),
                    );
                }
                _ => {
                    downsampled.add_sample(bucket.ts, mean, min, max, freq, perc);
                }
            }
        }

        start_idx = end_idx;
    }

    downsampled
}

struct Bucket {
    ts: i64,
    start_idx: usize,
    end_idx: usize,
}

impl Bucket {
    fn new(t: &[i64], start_idx: usize, bucket_width_ms: i64) -> Bucket {
        // Round down time to start of bucket
        let bucket_start_ms: i64 = t[start_idx] - (t[start_idx] % bucket_width_ms);
        let bucket_end_ms: i64 = bucket_start_ms + bucket_width_ms;
        // Find the first index outside the bucket
        let end_idx = t.iter().position(|&t| t > bucket_end_ms).unwrap_or(t.len());

        Bucket {
            ts: bucket_start_ms,
            start_idx,
            end_idx,
        }
    }
    fn range(&self) -> std::ops::Range<usize> {
        self.start_idx..self.end_idx
    }
}

fn average(v: &[f32]) -> f32 {
    v.iter().sum::<f32>() / v.len() as f32
}

fn find_sample_with_max_percentage(perc: &[f32], max_values: &[f32]) -> Option<(usize, f32)> {
    perc.iter()
        .cloned()
        .enumerate()
        .max_by(
            |(i1, value1), (i2, value2)| match value1.partial_cmp(value2) {
                Some(Ordering::Equal) | None => max_values[*i1]
                    .partial_cmp(&max_values[*i2])
                    .unwrap_or(Ordering::Equal),
                Some(ordering) => ordering,
            },
        )
}

fn find_sample_with_min_percentage(perc: &[f32], min_values: &[f32]) -> Option<(usize, f32)> {
    perc.iter()
        .cloned()
        .enumerate()
        .min_by(
            |(i1, value1), (i2, value2)| match value1.partial_cmp(value2) {
                Some(Ordering::Equal) | None => min_values[*i1]
                    .partial_cmp(&min_values[*i2])
                    .unwrap_or(Ordering::Equal),
                Some(ordering) => ordering,
            },
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bucket_width_description() {
        assert_eq!(BucketWidth(1_000).description(), "1s");
        assert_eq!(BucketWidth(300_000).description(), "5m");
        assert_eq!(BucketWidth(86_400_000).description(), "1d");
    }

    #[test]
    fn test_bucket_width_selection() {
        // Test smallest bucket
        assert_eq!(BucketWidth::find_next_width(1).as_millis(), 1);
        // Test exact match
        assert_eq!(BucketWidth::find_next_width(10_000).as_millis(), 10_000);
        // Test in-between value rounds up
        assert_eq!(BucketWidth::find_next_width(11_000).as_millis(), 30_000);
        // Test largest bucket
        assert_eq!(
            BucketWidth::find_next_width(9_999_999_999).as_millis(),
            2_592_000_000
        );
    }

    #[test]
    fn test_downsample_by_percentage() {
        let series = MPSensorData {
            time: vec![0, 1, 2],
            values: vec![4., 6., 5.],
            min: vec![0., 2., 1.],
            max: vec![9., 8., 9.],
            freq: Some(vec![5., 6., 7.]),
            perc: Some(vec![2., 3., 1.]),
            ..Default::default()
        };

        let ds = downsample_to_bucket_width_by_percentage_or_average(
            series,
            DownsampleOptions {
                method: DownsampleBucketMethod::MaxPoints(1),
                preference: DownsamplePreference::Percentage,
            },
        );
        assert_eq!(ds.time.len(), 1);
        assert_eq!(ds.values.len(), 1);
        assert_eq!(ds.min.len(), 1);
        assert_eq!(ds.max.len(), 1);
        assert!(ds.freq.is_some());
        assert!(ds.perc.is_some());
        let freq = ds.freq.unwrap();
        let perc = ds.perc.unwrap();
        assert_eq!(freq.len(), 1);
        assert_eq!(perc.len(), 1);

        assert_eq!(ds.time[0], 0);
        assert_eq!(ds.values[0], 5.);
        assert_eq!(ds.min[0], 1.);
        assert_eq!(freq[0], 6.);
        assert_eq!(perc[0], 3.);
        assert_eq!(ds.max[0], 8.);
    }

    #[test]
    fn test_downsample_by_percentage_all_zero_percentages() {
        let series = MPSensorData {
            time: vec![1, 2, 3],
            values: vec![4., 6., 5.],
            min: vec![0., 2., 1.],
            max: vec![8., 9., 8.],
            freq: None,
            perc: Some(vec![0., 0., 0.]),
            ..Default::default()
        };
        let ds = downsample_to_bucket_width_by_percentage_or_average(
            series,
            DownsampleOptions {
                method: DownsampleBucketMethod::MaxPoints(1),
                preference: DownsamplePreference::Percentage,
            },
        );

        // We can have up to 1 bucket extra because of realignment of the timestamps.
        let expected_len: usize = 1 + 1;
        assert!(ds.time.len() <= expected_len);
        assert_eq!(ds.values.len(), expected_len);
        assert_eq!(ds.min.len(), expected_len);
        assert_eq!(ds.max.len(), expected_len);
        assert!(ds.freq.is_none());
        assert!(ds.perc.is_some());
        let perc = ds.perc.unwrap();
        assert_eq!(perc.len(), expected_len);

        assert_eq!(ds.time[0], 0); // realigned timestamp
        assert_eq!(ds.values[0], 5.);
        assert_eq!(ds.min[0], 0.);
        assert_eq!(perc[0], 0.);
        assert_eq!(ds.max[0], 9.);
    }

    #[test]
    fn test_bucket_width_alignment() {
        // Create test data with timestamps not perfectly aligned to 10-minute intervals
        // Starting at 2024-01-01 10:03:45
        let base_time = 1704103425000; // 2024-01-01 10:03:45
        let ten_minutes_ms = 10 * 60 * 1000;

        let series = MPSensorData {
            measuring_point_id: "test".to_string(),
            time: vec![
                base_time,                  // 10:03:45
                base_time + 2 * 60 * 1000,  // 10:05:45
                base_time + 5 * 60 * 1000, // 10:08:45 - end bucket 10:00:00 (missing start, so rejected)
                base_time + 8 * 60 * 1000, // 10:11:45 - start bucket at 10:10:00
                base_time + 12 * 60 * 1000, // 10:15:45
                base_time + 18 * 60 * 1000, // 10:21:45 - start bucket at 10:20:00
                base_time + 25 * 60 * 1000, // 10:28:45
                base_time + 32 * 60 * 1000, // 10:35:45 - start bucket at 10:30:00
                base_time + 38 * 60 * 1000, // 10:41:45 - start bucket at 10:40:00
                base_time + 42 * 60 * 1000, // 10:45:45
                base_time + 48 * 60 * 1000, // 10:51:45 - start bucket at 10:50:00 (not completed)
            ],
            values: vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0],
            min: vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0],
            max: vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0],
            freq: None,
            perc: None,
            seconds_per_sample: 1.0,
        };

        let ds = downsample_to_bucket_width_by_percentage_or_average(
            series,
            DownsampleOptions {
                method: DownsampleBucketMethod::BucketWidthMS(ten_minutes_ms),
                preference: DownsamplePreference::Average,
            },
        );

        // We expect timestamps to be aligned to 10-minute intervals
        // First timestamp should be at 10:10:00 (rounded down)
        let expected_first_ts = ((base_time + ten_minutes_ms) / ten_minutes_ms) * ten_minutes_ms;

        // Verify all timestamps are properly aligned
        for ts in ds.time.clone() {
            assert_eq!(
                ts % ten_minutes_ms,
                0,
                "Timestamp {} is not aligned to {}-minute interval",
                ts,
                ten_minutes_ms / (60 * 1000)
            );
        }

        // Verify we got the expected number of buckets
        assert!(
            ds.time.len() >= 4,
            "Expected at least 4 buckets for the time span"
        );

        // Verify first timestamp is correctly aligned
        assert_eq!(
            ds.time[0], expected_first_ts,
            "First timestamp should be aligned to 10-minute interval"
        );
    }

    #[test]
    fn test_calc_bucket_width() {
        // Test empty input
        assert_eq!(calc_bucket_width(&[], 100), 1); // Should return smallest bucket

        // Test single point
        assert_eq!(calc_bucket_width(&[1000], 100), 1); // Should return smallest bucket

        // Test max_num_samples = 1
        assert_eq!(calc_bucket_width(&[0, 1000], 1), 1000); // Should return time range

        // Test exact division
        // 1 hour range with 60 samples should give 1-minute buckets
        let one_hour = (0..3600000).step_by(60000).collect::<Vec<_>>();
        assert_eq!(calc_bucket_width(&one_hour, 60), 60000); // 1 minute

        // Test rounding up to next bucket
        // 70 minutes with 60 samples would ideally need 70 second buckets
        // Should round up to 5 minute buckets
        let seventy_min = (0..4200000).step_by(70000).collect::<Vec<_>>();
        assert_eq!(calc_bucket_width(&seventy_min, 60), 300000); // 5 minutes

        // Test very large range
        // 2 days with 2 samples would need 2 day buckets (172800000ms)
        // Should round up to 1 week since that's the next available bucket
        let points = vec![0, 172800000];
        assert_eq!(calc_bucket_width(&points, 2), 604800000); // 1 week

        // Test very small range
        // 100ms with 10 samples should give 10ms buckets
        let points = (0..100).step_by(10).collect::<Vec<_>>();
        assert_eq!(calc_bucket_width(&points, 10), 10); // 10ms

        // Test max_points larger than number of points
        let one_min = (0..60000).step_by(1000).collect::<Vec<_>>();
        assert_eq!(calc_bucket_width(&one_min, 100), 1000); // 1 minute
    }

    #[test]
    fn test_calc_bucket_width_with_nonzero_intervals() {
        // Timestamps with zero intervals and nonzero intervals
        let timestamps = vec![1000, 1000, 1900, 3000, 3000, 4000, 5000];
        let max_num_samples = 5;

        // Expected to use the smallest nonzero interval, which is 1000ms
        let bucket_width = calc_bucket_width(&timestamps, max_num_samples);
        assert_eq!(bucket_width, 1000);
    }

    #[test]
    fn test_calc_bucket_width_for_real_irregular_sbr_b_data() {
        let series = MPSensorData {
            measuring_point_id: "test".to_string(),
            time: vec![1776676005336, 1776676035361, 1776676065383, 1776676095409],
            values: vec![1.0, 2.0, 3.0, 4.0],
            ..Default::default()
        };

        let ds = downsample_to_bucket_width_by_percentage_or_average(
            series,
            DownsampleOptions {
                method: DownsampleBucketMethod::MaxPoints(1800),
                preference: DownsamplePreference::Percentage,
            },
        );
        assert_eq!(ds.time.len(), 4);
        assert_eq!(ds.values.len(), 4);
    }
}

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use strum::EnumIter;

use crate::{data_kind::DataKind, DataStoreId};

#[derive(Debug, Clone, Copy, EnumIter, Deserialize)]
pub enum TimeResolution {
    /// Highest possible time resolution (no downsampling)
    Native,
    Seconds,
    Minutes,
    Hours,
}

impl TimeResolution {
    pub fn finer_resolution(self) -> Option<Self> {
        match self {
            TimeResolution::Native => None,
            TimeResolution::Seconds => Some(TimeResolution::Native),
            TimeResolution::Minutes => Some(TimeResolution::Seconds),
            TimeResolution::Hours => Some(TimeResolution::Minutes),
        }
    }

    pub fn coarser_resolution(self) -> Option<Self> {
        match self {
            TimeResolution::Native => Some(TimeResolution::Seconds),
            TimeResolution::Seconds => Some(TimeResolution::Minutes),
            TimeResolution::Minutes => Some(TimeResolution::Hours),
            TimeResolution::Hours => None,
        }
    }

    pub fn seconds_per_sample(&self) -> f32 {
        match self {
            TimeResolution::Native => 0.,
            TimeResolution::Seconds => 1.,
            TimeResolution::Minutes => 60.,
            TimeResolution::Hours => 3600.,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MetaData<C: DataKind> {
    pub device_id: Option<String>,
    pub measuring_point_id: Option<DataStoreId>,
    pub data_set_id: Option<DataStoreId>,
    pub data_channel: Option<C>,
}

/// Timeseries data point from the database
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SensorData<C: DataKind> {
    pub metadata: MetaData<C>,
    pub time: DateTime<Utc>,
    pub value: f32,
    pub min: Option<f32>,
    pub max: Option<f32>,
    pub percentage: Option<f32>,
    pub frequency: Option<f32>,
}

impl<C: DataKind> SensorData<C> {
    pub fn device_id(&self) -> Option<&String> {
        self.metadata.device_id.as_ref()
    }
    pub fn timestamp(&self) -> chrono::DateTime<chrono::Utc> {
        self.time
    }
    pub fn value(&self) -> f32 {
        self.value
    }
    pub fn measuring_point_id(&self) -> Option<&DataStoreId> {
        self.metadata.measuring_point_id.as_ref()
    }
    pub fn data_set_id(&self) -> &DataStoreId {
        self.metadata.data_set_id.as_ref().unwrap()
    }
}

/// Sensor data for a MeasuringPoint in column format
#[derive(Debug, Serialize, Default)]
pub struct MPSensorData {
    pub measuring_point_id: String,
    pub time: Vec<i64>,
    pub values: Vec<f32>,
    pub min: Vec<f32>,
    pub max: Vec<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub freq: Option<Vec<f32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub perc: Option<Vec<f32>>,
    pub seconds_per_sample: f32,
}

impl MPSensorData {
    pub fn empty(measuring_point_id: String) -> Self {
        Self {
            measuring_point_id,
            time: vec![],
            values: vec![],
            min: vec![],
            max: vec![],
            freq: None,
            perc: None,
            seconds_per_sample: 1.0,
        }
    }

    pub fn add_sample(
        &mut self,
        t: i64,
        v: f32,
        min: f32,
        max: f32,
        freq: Option<f32>,
        perc: Option<f32>,
    ) {
        self.time.push(t);
        self.values.push(v);
        self.min.push(min);
        self.max.push(max);
        if let (Some(f), Some(freqs)) = (freq, &mut self.freq) {
            freqs.push(f);
        }
        if let (Some(p), Some(percs)) = (perc, &mut self.perc) {
            percs.push(p);
        }
    }
}

pub struct MPSensorDataChannels {
    data: Vec<MPSensorData>,
}

impl MPSensorDataChannels {
    pub fn new() -> Self {
        Self { data: Vec::new() }
    }

    pub fn push(&mut self, data: MPSensorData) {
        self.data.push(data);
    }

    /// True if no channel has any rows in this batch.
    pub fn is_empty(&self) -> bool {
        self.data.first().is_none_or(|d| d.time.is_empty())
    }

    /// True if any channel actually carries frequency samples.
    pub fn has_freq(&self) -> bool {
        self.data
            .iter()
            .any(|d| d.freq.as_ref().is_some_and(|f| !f.is_empty()))
    }

    /// True if any channel actually carries percentage samples.
    pub fn has_perc(&self) -> bool {
        self.data
            .iter()
            .any(|d| d.perc.as_ref().is_some_and(|p| !p.is_empty()))
    }

    /// Render the batch as CSV rows. `include_freq`/`include_perc` gate the freq
    /// and percentage column blocks (appended after the value columns), so every
    /// batch stays aligned with a header decided once for the whole export.
    pub fn as_csv(&self, include_freq: bool, include_perc: bool) -> String {
        if self.data.is_empty() {
            return String::new();
        }
        self.data[0]
            .time
            .iter()
            .enumerate()
            .map(|(index, ts)| {
                let value_cols = self
                    .data
                    .iter()
                    .map(|data| data.values.get(index).unwrap_or(&f32::NAN).to_string());
                let freq_cols = include_freq
                    .then(|| {
                        self.data.iter().map(move |data| {
                            data.freq
                                .as_ref()
                                .and_then(|f| f.get(index))
                                .copied()
                                .unwrap_or(f32::NAN)
                                .to_string()
                        })
                    })
                    .into_iter()
                    .flatten();
                // Stored perc is a fraction (0..1, "frac"); ×100 for actual percentage.
                let perc_cols = include_perc
                    .then(|| {
                        self.data.iter().map(move |data| {
                            (data
                                .perc
                                .as_ref()
                                .and_then(|p| p.get(index))
                                .copied()
                                .unwrap_or(f32::NAN)
                                * 100.0)
                                .to_string()
                        })
                    })
                    .into_iter()
                    .flatten();
                let cols = value_cols
                    .chain(freq_cols)
                    .chain(perc_cols)
                    .collect::<Vec<_>>()
                    .join(",");
                format!("{ts},{cols}\n")
            })
            .collect()
    }
}

impl Default for MPSensorDataChannels {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct StatsPerBin {
    pub time: i64,
    pub mean: f32,
    pub std_dev: f32,
    pub max: f32,
    pub min: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(values: Vec<f32>, freq: Option<Vec<f32>>, perc: Option<Vec<f32>>) -> MPSensorData {
        MPSensorData {
            measuring_point_id: "mp".to_string(),
            time: (0..values.len() as i64).collect(),
            values,
            min: vec![],
            max: vec![],
            freq,
            perc,
            seconds_per_sample: 1.0,
        }
    }

    #[test]
    fn as_csv_without_freq_or_perc_is_legacy_format() {
        let mut channels = MPSensorDataChannels::new();
        channels.push(channel(vec![1.0, 2.0], None, None));
        channels.push(channel(vec![3.0, 4.0], None, None));

        assert_eq!(channels.as_csv(false, false), "0,1,3\n1,2,4\n");
    }

    #[test]
    fn as_csv_appends_freq_then_perc_blocks_with_perc_scaled() {
        // Two SBR-A-like channels carrying both freq and perc (perc stored as fraction).
        let mut channels = MPSensorDataChannels::new();
        channels.push(channel(
            vec![1.0, 2.0],
            Some(vec![10.0, 11.0]),
            Some(vec![0.25, 0.5]),
        ));
        channels.push(channel(
            vec![3.0, 4.0],
            Some(vec![12.0, 13.0]),
            Some(vec![0.75, 1.0]),
        ));

        // value cols (v1,v2), then freq cols (f1,f2), then perc cols (p1*100,p2*100).
        assert_eq!(
            channels.as_csv(true, true),
            "0,1,3,10,12,25,75\n1,2,4,11,13,50,100\n"
        );
    }

    #[test]
    fn as_csv_flags_gate_columns_not_data_presence() {
        // Data carries freq/perc, but flags are off → legacy format.
        let mut channels = MPSensorDataChannels::new();
        channels.push(channel(vec![1.0], Some(vec![10.0]), Some(vec![0.5])));
        channels.push(channel(vec![3.0], Some(vec![12.0]), Some(vec![0.25])));

        assert_eq!(channels.as_csv(false, false), "0,1,3\n");
    }

    #[test]
    fn presence_accessors_reflect_data() {
        let empty = MPSensorDataChannels::new();
        assert!(empty.is_empty());

        let mut with_freq = MPSensorDataChannels::new();
        with_freq.push(channel(vec![1.0], Some(vec![10.0]), Some(vec![])));
        assert!(!with_freq.is_empty());
        assert!(with_freq.has_freq());
        assert!(!with_freq.has_perc());

        let mut without = MPSensorDataChannels::new();
        without.push(channel(vec![1.0], Some(vec![]), None));
        assert!(!without.has_freq());
        assert!(!without.has_perc());
    }
}

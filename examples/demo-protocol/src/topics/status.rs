//! Information about the device that changes regularly.
//!

use serde::{Deserialize, Serialize};

use crate::{types::SensorType, NUM_CH_DEMO};

pub use sensor_link_protocol::Status;

/// Error states for sensor channels
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq)]
pub enum ChannelError {
    /// Sensor was expected but not found
    NoSensor,

    /// Measured value is out of range (clipping)
    OutOfRange,

    /// Faulty or unsupported hardware detected.
    Faulty,
}

/// Status of sensor channels
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq)]
pub enum ChannelStatus {
    Nothing,
    Ok(SensorType),
    Error(ChannelError),
}

/// Status for demo devices
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "lowercase")]
pub struct DeviceStatus {
    #[serde(default)]
    pub settings_hash: u64,
    pub status: Status,
    pub signal_strength: i32, // TODO: making this optional might be better
    pub temperature: f32,

    #[serde(skip_serializing_if = "Option::is_none", default, rename = "ch_stat")]
    pub channel_status: Option<[ChannelStatus; NUM_CH_DEMO]>,
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn serialize_empty_status() {
        let status = DeviceStatus::default();
        let serialized: heapless::String<1024> = serde_json_core::to_string(&status).unwrap();
        assert_eq!(
            serialized,
            r#"{"settings_hash":0,"status":"unknown","signal_strength":0,"temperature":0.0}"#
        );
    }
}

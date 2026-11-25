//! Device settings - settings that can change on-the-fly.

use serde::{Deserialize, Serialize};
#[cfg(feature = "use-std")]
use std::hash::{Hash, Hasher};
#[cfg(feature = "use-std")]
use xxhash_rust::xxh3::Xxh3;

use sensor_link_protocol::{TopicPayloadSerialize, MAX_MESSAGE_LEN};

/// Output DataRate
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
#[cfg_attr(feature = "use-std", derive(Hash))]
#[serde(rename_all = "lowercase")]
pub enum DataRate {
    /// 1 sample per second
    _1SPS,
    #[default]
    /// 1 sample per minute
    _1SPM,
    /// 6 samples per hour
    _6SPH,
    /// 2 samples per hour
    _2SPH,
    /// 1 sample per hour
    _1SPH,
}

/// Device operational settings.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[cfg_attr(feature = "use-std", derive(Hash))]
#[serde(rename_all = "lowercase")]
pub struct Settings {
    /// Data rate for all enabled channels
    pub data_rate: DataRate,

    /// Sync interval in seconds; The maximum interval between status updates.
    #[serde(rename = "sync")]
    pub sync_interval_s: i32,

    /// Hash of the settings. Used to detect changes.
    #[serde(rename = "h")]
    pub hash: u64,
}

impl TopicPayloadSerialize<MAX_MESSAGE_LEN> for Settings {}

#[cfg(feature = "use-std")]
impl Settings {
    /// Calculate hash for this struct.
    pub fn calculate_hash(&self) -> u64 {
        let mut hasher = Xxh3::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sensor_link_protocol::parse_json_payload;

    #[test]
    fn settings_encode_decode_json() {
        let settings = Settings {
            data_rate: DataRate::_1SPS,
            sync_interval_s: 900,
            hash: 42,
        };

        let encoded = settings.serialize_topic_payload().unwrap();
        let decoded: Settings = parse_json_payload(encoded.as_slice()).unwrap();

        assert_eq!(settings, decoded);
    }
}

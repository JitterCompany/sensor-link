use serde_with::{DeserializeFromStr, SerializeDisplay};
use std::{fmt::Display, str::FromStr};

/// The supported sensor data types
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, strum::EnumIter, DeserializeFromStr, SerializeDisplay,
)]
pub enum DataType {
    Measurement,
}

impl Display for DataType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for DataType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "measurement" => Ok(DataType::Measurement),
            _ => Err(format!("Unknown data type: {}", s)),
        }
    }
}

impl TryFrom<String> for DataType {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        DataType::from_str(&value)
    }
}

impl DataType {
    pub fn as_str(&self) -> &str {
        match self {
            DataType::Measurement => "measurement",
        }
    }

    /// Returns all data types as a list
    pub fn all_as_list() -> impl Iterator<Item = DataType> {
        <Self as strum::IntoEnumIterator>::iter()
    }

    /// Returns all channels for all data types
    pub fn all_channels() -> Vec<DataChannel> {
        DataType::all_as_list()
            .flat_map(|dt| dt.channels())
            .collect()
    }

    /// Converts the datatype to a channel
    pub fn as_channel(&self) -> DataChannel {
        match self {
            DataType::Measurement => DataChannel::Measurement,
        }
    }

    /// Returns all available channels for this data type
    pub fn channels(&self) -> Vec<DataChannel> {
        match self {
            DataType::Measurement => vec![DataChannel::Measurement],
        }
    }

    pub fn downsampling(&self) -> bool {
        match self {
            DataType::Measurement => true,
        }
    }
}

impl TryFrom<demo_protocol::TopicFromDevice> for DataType {
    type Error = demo_protocol::TopicFromDevice;
    fn try_from(topic: demo_protocol::TopicFromDevice) -> Result<Self, Self::Error> {
        use demo_protocol::TopicFromDevice as T;
        match topic {
            T::Data => Ok(DataType::Measurement),
            _ => Err(topic),
        }
    }
}

/// The supported sensor data channels
#[derive(Debug, Clone, Copy, SerializeDisplay, DeserializeFromStr, PartialEq, Eq, Hash)]
pub enum DataChannel {
    Measurement,
}

impl Display for DataChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for DataChannel {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.split('.');
        match parts.next() {
            Some("measurement") => Ok(DataChannel::Measurement),
            _ => Err(anyhow::anyhow!("Unknown channel: {}", s)),
        }
    }
}

impl DataChannel {
    /// Get the data type of the channel.
    pub fn datatype(&self) -> DataType {
        match self {
            DataChannel::Measurement => DataType::Measurement,
        }
    }

    pub fn downsampling(&self) -> bool {
        self.datatype().downsampling()
    }

    pub fn as_str(&self) -> &str {
        match self {
            DataChannel::Measurement => "measurement",
        }
    }

    /// Stable SMALLINT encoding for storage in TimescaleDB.
    ///
    /// The mapping is part of the on-disk schema — never renumber an existing
    /// variant. Gaps are reserved for future axes / variants.
    pub fn to_i16(self) -> i16 {
        match self {
            DataChannel::Measurement => 2,
        }
    }

    pub fn try_from_i16(n: i16) -> anyhow::Result<Self> {
        Ok(match n {
            2 => DataChannel::Measurement,
            _ => anyhow::bail!("Unknown DataChannel code: {}", n),
        })
    }
}

// ── DataKind implementations ──────────────────────────────────────────────────

impl sensor_link_server_core::DataKind for DataType {
    fn downsampling(&self) -> bool {
        DataType::downsampling(self)
    }
}

impl sensor_link_server_core::DataKind for DataChannel {
    fn downsampling(&self) -> bool {
        DataChannel::downsampling(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_datatype_string_consistency() {
        for data_type in DataType::all_as_list() {
            let string_repr = data_type.as_str();
            let parsed: DataType = string_repr.to_string().try_into().unwrap();
            assert_eq!(
                data_type, parsed,
                "DataType string conversion failed for {:?}",
                data_type
            );
        }
    }

    #[test]
    fn test_datatype_serde_consistency() {
        for data_type in DataType::all_as_list() {
            let json_repr = serde_json::to_string(&data_type).unwrap();
            let parsed: DataType = serde_json::from_str(&json_repr).unwrap();
            assert_eq!(
                data_type, parsed,
                "DataType serde conversion failed for {:?}",
                data_type
            );
        }
    }

    #[test]
    fn test_datachannel_string_consistency() {
        for data_type in DataType::all_as_list() {
            for channel in data_type.channels() {
                let string_repr = channel.as_str();
                let parsed: DataChannel = string_repr.parse().unwrap();
                assert_eq!(
                    channel, parsed,
                    "DataChannel string conversion failed for {:?}",
                    channel
                );
            }
        }
    }

    #[test]
    fn test_datachannel_i16_roundtrip() {
        // Every channel produced by `DataType::all_as_list().channels()` must
        // round-trip through `to_i16` / `try_from_i16`.
        let all: Vec<DataChannel> = DataType::all_as_list()
            .flat_map(|dt| dt.channels())
            .collect();

        let mut seen_codes: Vec<i16> = Vec::new();
        for channel in all {
            let code = channel.to_i16();
            assert!(
                !seen_codes.contains(&code),
                "Duplicate code {} for {:?}",
                code,
                channel
            );
            seen_codes.push(code);

            let parsed = DataChannel::try_from_i16(code).unwrap();
            assert_eq!(
                channel, parsed,
                "DataChannel i16 round-trip failed for {:?} (code {})",
                channel, code
            );
        }

        assert!(DataChannel::try_from_i16(0).is_err());
        assert!(DataChannel::try_from_i16(999).is_err());
    }
}

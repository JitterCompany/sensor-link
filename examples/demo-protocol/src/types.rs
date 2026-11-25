use sensor_link_protocol::samples::UniformSamples;
use serde::{Deserialize, Serialize};
use strum::EnumIter;

pub use sensor_link_protocol::types::*;

/// Channel layout of the demo data stream (`f/<client-id>/data`): the index of
/// each channel in the serialized samples.
#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemoChannel {
    /// Board temperature, °C
    BoardTemperature,
    /// Charger input bus voltage, V
    Vbus,
    /// Charger input bus current, A
    Ibus,
    /// System rail voltage, V
    Vsys,
}

impl DemoChannel {
    /// All channels in stream order
    pub const ALL: [Self; 4] = [Self::BoardTemperature, Self::Vbus, Self::Ibus, Self::Vsys];

    pub const fn index(self) -> usize {
        self as usize
    }

    /// The sensor behind this channel
    pub const fn sensor(self) -> SensorType {
        match self {
            Self::BoardTemperature => SensorType::BoardTemperature,
            Self::Vbus | Self::Ibus | Self::Vsys => SensorType::Charger,
        }
    }
}

pub const NUM_CH_DEMO: usize = DemoChannel::ALL.len();
pub type DemoData<const M: usize> = UniformSamples<NUM_CH_DEMO, M>;

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, EnumIter, Eq, Hash)]
pub enum DeviceType {
    #[default]
    Demo,
}

/// Sensors behind the demo channels
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq)]
pub enum SensorType {
    /// Analog temperature sensor on the board
    BoardTemperature,
    /// ADC of the battery charger
    Charger,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_indices_follow_stream_order() {
        for (i, ch) in DemoChannel::ALL.iter().enumerate() {
            assert_eq!(ch.index(), i);
        }
    }
}

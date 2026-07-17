use super::TopicPayloadSerialize;
use crate::MAX_MESSAGE_LEN;
use serde::{Deserialize, Serialize};

/// Operational status of a sensor device, published on the `status` topic.
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Unknown,
    Startup,
    Inactive,
    Active,
    TryActive,
    Idle,
    Waiting,
    NoSensor,
    ConfigOutdated,
    NoConfig,
    Error,
}

/// Charging / power-supply status of a battery-backed sensor device.
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]
pub enum ChargerStatus {
    #[default]
    Unknown,
    /// Something is wrong, e.g. overvoltage
    Error,
    /// Power adapter and battery, charging enabled
    Charging,
    /// No battery, thus only power adapter
    NoBattery,
    /// Power adapter and battery, charging disabled/finished
    Idle,
    /// Internal battery, no power adapter
    NoAdapter,
}

impl TopicPayloadSerialize<MAX_MESSAGE_LEN> for Status {}

impl TopicPayloadSerialize<MAX_MESSAGE_LEN> for ChargerStatus {}

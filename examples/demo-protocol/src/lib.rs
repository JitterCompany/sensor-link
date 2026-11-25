//! demo-protocol defines the demo device's protocol on top of the shared
//! `sensor-link-protocol`: settings, device status, channels, events and the
//! sensor-data topic.
#![cfg_attr(not(any(test, feature = "use-std")), no_std)]
#![allow(async_fn_in_trait)]

mod topics;
pub mod types;

#[cfg(feature = "use-std")]
pub mod server;

pub use topics::*;
pub use types::{DemoChannel, DemoData, DeviceType, SensorType, NUM_CH_DEMO};

pub use sensor_link_protocol::{
    cmd, fwupdate, info, online, parse_json_payload, parse_system_topic, samples, server_status,
    time, Error, Fraction, Microseconds, Milliseconds, SystemTopic, TestPayload, Threshold,
    TopicFromDevice as CommonTopicFromDevice, TopicParseError, TopicParts, TopicPayloadSerialize,
    TopicSerializeError, TopicString, MAX_EVENT_LEN, MAX_FILE_CHUNK_LEN, MAX_MESSAGE_LEN,
    MAX_TOPIC_LEN, MAX_UID_LEN,
};

#[cfg(feature = "use-std")]
pub use sensor_link_protocol::{sms, to_string};

pub type DeviceInfoV3 = sensor_link_protocol::info::DeviceInfoV3<DeviceType>;

/// Event payload (event + timestamp) for the demo [event::Event] type.
pub type EventPayload = event::EventPayload<event::Event>;

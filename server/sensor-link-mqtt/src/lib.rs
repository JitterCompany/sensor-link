#![allow(async_fn_in_trait)]

pub mod client;
pub mod metrics;

pub use client::*;

use sensor_link_protocol::{
    cmd::CommandPayload,
    event::{Event, EventPayload},
    fwupdate::FWAnnounce,
    info::DeviceInfoV2,
    online::Online,
    samples::NChannelSamples,
    sms::{SMSRequest, SMSResponse},
    TopicParseError, TopicParts,
};

/// Application-provided data type classifier for incoming MQTT data.
pub trait MqttDataType: Send + Sync + Clone + 'static {}

impl<T> MqttDataType for T where T: Send + Sync + Clone + 'static {}

pub enum ProcessingIn {
    UniformSamples1D(NChannelSamples<1>),
    UniformSamples2D(NChannelSamples<2>),
    UniformSamples3D(NChannelSamples<3>),
}

pub enum DeviceControlOut {
    DeviceCommand(CommandPayload),
    FWUpdateAnnounce(FWAnnounce),
    Time,
    SMSRequest(SMSRequest),
}

pub enum DeviceControlIn<D, S, EV = Event> {
    DeviceOnline(Online),
    DeviceInfo(DeviceInfoV2<D>),
    DeviceStatus(S),
    Event(EventPayload<EV>),
}

pub enum SystemMessageIn {
    SMSResponse(SMSResponse),
}

pub struct ProcessingMessage<DT: MqttDataType, P = ProcessingIn> {
    pub datatype: DT,
    pub device_id: String,
    pub data: P,
}

pub struct ControlMessageIn<D, S, EV = Event> {
    pub device_id: String,
    pub payload: DeviceControlIn<D, S, EV>,
}

pub struct ControlMessageOut<CO = DeviceControlOut> {
    pub device_id: String,
    pub payload: CO,
}

pub enum ParsedMqttIn<DT: MqttDataType, D, S, EV = Event, P = ProcessingIn> {
    Data(ProcessingMessage<DT, P>),
    Control(ControlMessageIn<D, S, EV>),
    System(SystemMessageIn),
}

/// The [`ParsedMqttIn`] variant produced by a given [`TopicCodec`] `C`,
/// with all of the codec's associated payload types filled in.
pub type ParsedMqttInFor<C> = ParsedMqttIn<
    <C as TopicCodec>::DT,
    <C as TopicCodec>::D,
    <C as TopicCodec>::S,
    <C as TopicCodec>::EV,
    <C as TopicCodec>::P,
>;

/// Codec for a manufacturer's MQTT topic/payload scheme.
///
/// `sensor-mqtt`'s connection/event-loop machinery is generic over this trait
/// and monomorphized once per implementation, so each manufacturer's topic
/// parsing/encoding lives behind a single zero-sized type.
pub trait TopicCodec: Send + Sync + 'static {
    type DT: MqttDataType;
    type D: Send + 'static;
    type S: Send + 'static;
    type EV: Send + 'static;
    type P: Send + 'static;
    type TopicFromDevice: Copy + core::fmt::Debug;
    type ControlOut: Send + 'static;

    fn parse_topic_from_device(
        topic: &str,
    ) -> Result<TopicParts<Self::TopicFromDevice>, TopicParseError>;

    fn parse_payload(
        payload: &[u8],
        parts: TopicParts<Self::TopicFromDevice>,
    ) -> Result<ParsedMqttInFor<Self>, String>;

    /// Returns the full topic string and serialized payload to publish, or an
    /// error description if encoding failed.
    fn encode_control_out(
        device_id: &str,
        out: Self::ControlOut,
    ) -> Result<(String, String), String>;

    /// Returns the sensor-server-log type under which this event should be
    /// recorded, or `None` if it shouldn't be logged.
    fn event_log_type(_event: &Self::EV) -> Option<&'static str> {
        None
    }
}

impl From<sensor_link_server_core::status_handler::ControlMessageOut> for ControlMessageOut {
    fn from(value: sensor_link_server_core::status_handler::ControlMessageOut) -> Self {
        ControlMessageOut {
            device_id: value.device_id,
            payload: value.payload.into(),
        }
    }
}

impl From<sensor_link_server_core::status_handler::DeviceControlOut> for DeviceControlOut {
    fn from(value: sensor_link_server_core::status_handler::DeviceControlOut) -> Self {
        use sensor_link_server_core::status_handler::DeviceControlOut::*;
        match value {
            DeviceCommand(cmd) => DeviceControlOut::DeviceCommand(cmd),
            FWUpdateAnnounce(announce) => DeviceControlOut::FWUpdateAnnounce(announce),
            Time => DeviceControlOut::Time,
        }
    }
}

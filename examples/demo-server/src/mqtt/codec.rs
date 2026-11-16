use demo_protocol::{
    event::Event, server::parse_data, status::DeviceStatus, DeviceType, TopicFromDevice,
    TopicParseError, TopicParts, TopicToDevice,
};
use sensor_link_mqtt::{JitterTopicCodec, ParsedMqttIn, ProcessingMessage, TopicCodec};

use crate::{
    mqtt::{control_out::DemoControlOut, processing::DemoProcessingIn},
    DataType,
};

/// The generic [JitterTopicCodec] specialized to the demo payload types.
/// [DemoTopicCodec] delegates its shared `Common(..)` topics to this.
type CommonCodec = JitterTopicCodec<DataType, DeviceType, DeviceStatus, Event>;

/// [TopicCodec] for demo devices, composing the generic Jitter topic
/// vocabulary (via [CommonCodec]) with demo-specific topics.
pub struct DemoTopicCodec;

impl TopicCodec for DemoTopicCodec {
    type DT = DataType;
    type D = DeviceType;
    type S = DeviceStatus;
    type EV = Event;
    type P = DemoProcessingIn;
    type TopicFromDevice = TopicFromDevice;
    type ControlOut = DemoControlOut;

    fn parse_topic_from_device(
        topic: &str,
    ) -> Result<TopicParts<Self::TopicFromDevice>, TopicParseError> {
        demo_protocol::parse_topic_from_device(topic)
    }

    fn parse_payload(
        payload: &[u8],
        parts: TopicParts<Self::TopicFromDevice>,
    ) -> Result<ParsedMqttIn<Self::DT, Self::D, Self::S, Self::EV, Self::P>, String> {
        let TopicParts {
            prefix,
            device_id,
            topic,
        } = parts;
        match topic {
            TopicFromDevice::Common(jitter_topic) => {
                let parsed = CommonCodec::parse_payload(
                    payload,
                    TopicParts {
                        prefix,
                        device_id,
                        topic: jitter_topic,
                    },
                )?;
                match parsed {
                    ParsedMqttIn::Data(ProcessingMessage {
                        datatype,
                        device_id,
                        data,
                    }) => Ok(ParsedMqttIn::Data(ProcessingMessage {
                        datatype,
                        device_id,
                        data: data.into(),
                    })),
                    ParsedMqttIn::Control(msg) => Ok(ParsedMqttIn::Control(msg)),
                    ParsedMqttIn::System(msg) => Ok(ParsedMqttIn::System(msg)),
                }
            }
            TopicFromDevice::Data => {
                let Ok(datatype) = DataType::try_from(topic) else {
                    return Err(format!("Cannot match topic {topic:?} to datatype"));
                };
                let data = parse_data(payload)
                    .map_err(|err| format!("Data topic payload parse error: {err:?}"))?;
                Ok(ParsedMqttIn::Data(ProcessingMessage {
                    datatype,
                    device_id,
                    data: DemoProcessingIn::Data(data),
                }))
            }
        }
    }

    fn encode_control_out(
        device_id: &str,
        out: Self::ControlOut,
    ) -> Result<(String, String), String> {
        match out {
            DemoControlOut::Common(inner) => CommonCodec::encode_control_out(device_id, inner),
            DemoControlOut::Settings(data) => {
                let payload = serde_json::to_string(&data)
                    .map_err(|err| format!("Serialize Settings: {err}"))?;
                let topic = TopicToDevice::Settings
                    .to_topic_string(device_id)
                    .map_err(|err| format!("Encode topic: {err:?}"))?;
                Ok((demo_protocol::to_string(topic), payload))
            }
        }
    }

    fn event_log_type(_event: &Self::EV) -> Option<&'static str> {
        None
    }
}

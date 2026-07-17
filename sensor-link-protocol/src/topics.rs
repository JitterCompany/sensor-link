pub mod cmd;
pub mod event;
pub mod fwupdate;
pub mod info;
pub mod online;
pub mod server_status;
#[cfg(feature = "use-std")]
pub mod sms;
pub mod status;
pub mod time;

pub use status::{ChargerStatus, Status};

use core::fmt::Write;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::MAX_TOPIC_LEN;

pub type TopicString = heapless::String<MAX_TOPIC_LEN>;

#[cfg(feature = "use-std")]
pub fn to_string(topic_str: TopicString) -> std::string::String {
    std::string::String::from(topic_str.as_str())
}

/// Bound shared by every topic type usable as a wire-format header by [Sendable]/[SerializedSendable].
pub trait Topic: Serialize + DeserializeOwned + Clone + Send + Sync + core::fmt::Debug {
    fn to_topic_string(&self, uid: &str) -> Result<TopicString, TopicSerializeError>;
}

/// Trait to define (default) serialization for topic payload types.
///
/// `N` is the size of the output buffer and lets each payload cap its serialized
/// length at its own maximum (e.g. [MAX_EVENT_LEN](crate::MAX_EVENT_LEN) for events,
/// [MAX_MESSAGE_LEN](crate::MAX_MESSAGE_LEN) for the general case). Implementors only
/// need to pick `N`; the default implementations cover JSON encoding.
pub trait TopicPayloadSerialize<const N: usize>: Serialize {
    fn serialize_topic_payload_to_slice(&self, buf: &mut [u8]) -> Result<usize, crate::Error<()>> {
        serde_json_core::to_slice(self, buf).map_err(|_| crate::Error::Serialize)
    }

    fn serialize_topic_payload(&self) -> Result<heapless::Vec<u8, N>, crate::Error<()>> {
        let mut result = heapless::Vec::<u8, N>::new();
        result
            .resize_default(N)
            .map_err(|_| crate::Error::Serialize)?;
        let len = self.serialize_topic_payload_to_slice(result.as_mut())?;
        result.truncate(len);
        Ok(result)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, strum::EnumIter)]
pub enum SystemTopic {
    SMSRequest,
    SMSResponse,
    ServerStatus,
}

impl SystemTopic {
    pub fn as_str(&self) -> &'static str {
        match self {
            SystemTopic::SMSRequest => "sms/req",
            SystemTopic::SMSResponse => "sms/resp",
            SystemTopic::ServerStatus => "server/status",
        }
    }
}

pub fn parse_system_topic(value: &str) -> Option<SystemTopic> {
    SystemTopic::try_from(value).ok()
}

impl TryFrom<&str> for SystemTopic {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "sms/req" => Ok(SystemTopic::SMSRequest),
            "sms/resp" => Ok(SystemTopic::SMSResponse),
            "server/status" => Ok(SystemTopic::ServerStatus),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, strum::EnumIter, Serialize, Deserialize)]
pub enum TopicFromDevice {
    Online,
    // Meta Topics
    Event,
    FWStatus,
    DeviceInfoV2,
    Status,

    // Test Topics
    BenchmarkEvent,
    BenchmarkData,

    ChargerStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, strum::EnumIter)]
pub enum TopicToDevice {
    Command,
    FWUpdateAnnounce,
    Time,
}

impl Topic for TopicFromDevice {
    fn to_topic_string(&self, uid: &str) -> Result<TopicString, TopicSerializeError> {
        (*self).to_topic_string(uid)
    }
}

impl TopicFromDevice {
    pub fn topic_name(&self) -> &'static str {
        match self {
            TopicFromDevice::Online => "online",
            TopicFromDevice::Event => "events",
            TopicFromDevice::FWStatus => "fw_update/status",
            TopicFromDevice::DeviceInfoV2 => "info_v2",
            TopicFromDevice::Status => "status",
            TopicFromDevice::ChargerStatus => "charger_status",
            TopicFromDevice::BenchmarkEvent => "benchmark_event",
            TopicFromDevice::BenchmarkData => "benchmark_data",
        }
    }

    /// Convert to a full topic string in the "f/uid/topic" format.
    pub fn to_topic_string(self, uid: impl AsRef<str>) -> Result<TopicString, TopicSerializeError> {
        self.to_topic_string_with_prefix("f", uid)
    }

    /// Convert to a full topic string with custom prefix in the format "prefix/uid/topic".
    ///
    /// In most cases you'll want to use [to_topic_string](method@Self::to_topic_string) instead.
    pub fn to_topic_string_with_prefix(
        self,
        prefix: impl AsRef<str>,
        uid: impl AsRef<str>,
    ) -> Result<TopicString, TopicSerializeError> {
        build_topic_string(prefix, uid, self.topic_name())
    }
}

impl TopicToDevice {
    pub fn topic_name(&self) -> &'static str {
        match self {
            TopicToDevice::Command => "commands",
            TopicToDevice::FWUpdateAnnounce => "fw_update/meta",
            TopicToDevice::Time => "time",
        }
    }

    /// Convert to a full topic string in the "t/uid/topic" format.
    pub fn to_topic_string(self, uid: impl AsRef<str>) -> Result<TopicString, TopicSerializeError> {
        self.to_topic_string_with_prefix("t", uid)
    }

    /// Convert to a full topic string with custom prefix in the format "prefix/uid/topic".
    ///
    /// In most cases you'll want to use [to_topic_string](method@Self::to_topic_string) instead.
    pub fn to_topic_string_with_prefix(
        self,
        prefix: impl AsRef<str>,
        uid: impl AsRef<str>,
    ) -> Result<TopicString, TopicSerializeError> {
        build_topic_string(prefix, uid, self.topic_name())
    }
}

pub fn validate_prefix(prefix: &str) -> Result<&str, TopicSerializeError> {
    if prefix.len() <= crate::MAX_TOPIC_PREFIX_LEN {
        Ok(prefix)
    } else {
        Err(TopicSerializeError::PrefixLength)
    }
}

pub fn validate_uid(prefix: &str) -> Result<&str, TopicSerializeError> {
    if prefix.len() <= crate::MAX_UID_LEN {
        Ok(prefix)
    } else {
        Err(TopicSerializeError::UIDLength)
    }
}

/// Build a full topic string in the "prefix/uid/topic_part" format, validating prefix and uid
/// lengths.
pub fn build_topic_string(
    prefix: impl AsRef<str>,
    uid: impl AsRef<str>,
    topic_part: &str,
) -> Result<TopicString, TopicSerializeError> {
    let prefix_str = validate_prefix(prefix.as_ref())?;
    let uid_str = validate_uid(uid.as_ref())?;

    let mut output = TopicString::new();
    write!(output, "{}/{}/{}", prefix_str, uid_str, topic_part)
        .map_err(|_| TopicSerializeError::TotalLength)?;

    Ok(output)
}

/// write a topic with dynamic suffix into output
pub fn topic_fmt_f32<'out>(
    output: &'out mut TopicString,
    topic_name: &str,
    suffix: f32,
) -> Result<&'out str, TopicSerializeError> {
    write!(output, "{}/{}", topic_name, suffix).map_err(|_| TopicSerializeError::SuffixLength)?;
    Ok(output.as_str())
}

#[derive(Debug, Clone, Copy)]
pub enum TopicParseError {
    NoMatch,
    MissingPrefix,
    SuffixParse,
}

#[derive(Debug, Clone, Copy)]
pub enum TopicSerializeError {
    /// Prefix is too long
    PrefixLength,

    /// UID is too long
    UIDLength,

    /// Suffix is too long
    SuffixLength,

    /// Total topic string is too long
    TotalLength,

    Other,
}

impl<T: core::fmt::Debug> From<TopicSerializeError> for crate::Error<T> {
    fn from(err: TopicSerializeError) -> Self {
        crate::Error::SerializeTopic(err)
    }
}

#[cfg(feature = "use-std")]
#[derive(Debug, Clone)]
pub struct TopicParts<TOPIC> {
    pub prefix: String,
    pub device_id: String,
    pub topic: TOPIC,
}
#[cfg(not(feature = "use-std"))]
#[derive(Debug)]
pub struct TopicParts<'a, TOPIC> {
    pub prefix: &'a str,
    pub device_id: &'a str,
    pub topic: TOPIC,
}

/// Parse TopicFromDevice from string
///
/// Topics have the format "<direction | manufacturer>/<client_id>/<topic_name>",
/// where
/// - the first part can be "f" (from client) or "t" (to client) as Jitter manufacturer proprietary values
///   or any specific value for other manufacturers.
/// - the second part specifies the device id
/// - the last part defines the topic_name itself (which may include subtopics)
///
/// Note: the prefix and device_id are not validated. The caller may use them for further parsing / verification.
pub fn parse_topic_from_device(
    topic: &str,
) -> Result<TopicParts<'_, TopicFromDevice>, TopicParseError> {
    // split into 3 parts, skipping th first 2 and keeping the remainder
    let mut parts = topic.splitn(3, '/');
    let prefix = parts.next().ok_or(TopicParseError::MissingPrefix)?;
    let device_id = parts.next().ok_or(TopicParseError::MissingPrefix)?;
    let topic_name = parts.next().ok_or(TopicParseError::MissingPrefix)?;

    let topic = match topic_name {
        "online" => TopicFromDevice::Online,
        "benchmark_data" => TopicFromDevice::BenchmarkData,
        "benchmark_event" => TopicFromDevice::BenchmarkEvent,
        "events" => TopicFromDevice::Event,
        "status" => TopicFromDevice::Status,
        "charger_status" => TopicFromDevice::ChargerStatus,
        "info_v2" => TopicFromDevice::DeviceInfoV2,
        "fw_update/status" => TopicFromDevice::FWStatus,
        _ => return Err(TopicParseError::NoMatch),
    };

    // already returned an error at this point when there are less than 3 parts
    Ok(TopicParts {
        #[allow(clippy::useless_conversion)]
        prefix: prefix.into(),
        #[allow(clippy::useless_conversion)]
        device_id: device_id.into(),
        topic,
    })
}

/// Parse TopicToDevice from string
///
/// Topics have the format "<direction | manufacturer>/<client_id>/<topic_name>",
/// where
/// - the first part can be "f" (from client) or "t" (to client) as Jitter manufacturer proprietary values
///   or any specific value for other manufacturers.
/// - the second part specifies the device id
/// - the last part defines the topic_name itself (which may include subtopics)
///
/// Note: the prefix and device_id are not validated. The caller may use them for further parsing / verification.
pub fn parse_topic_to_device(
    topic: &str,
) -> Result<TopicParts<'_, TopicToDevice>, TopicParseError> {
    // split into 3 parts, skipping th first 2 and keeping the remainder
    let mut parts = topic.splitn(3, '/');
    let prefix = parts.next().ok_or(TopicParseError::MissingPrefix)?;
    let device_id = parts.next().ok_or(TopicParseError::MissingPrefix)?;
    let topic_name = parts.next().ok_or(TopicParseError::MissingPrefix)?;

    let topic = match topic_name {
        "commands" => TopicToDevice::Command,
        "time" => TopicToDevice::Time,
        "fw_update/meta" => TopicToDevice::FWUpdateAnnounce,
        _ => return Err(TopicParseError::NoMatch),
    };

    // already returned an error at this point when there are less than 3 parts
    Ok(TopicParts {
        #[allow(clippy::useless_conversion)]
        prefix: prefix.into(),
        #[allow(clippy::useless_conversion)]
        device_id: device_id.into(),
        topic,
    })
}

/// Removes outer quotes and parses as json
pub fn parse_json_payload<'a, T>(bytes: &'a [u8]) -> Option<T>
where
    T: Deserialize<'a>,
{
    if bytes.len() <= 1 {
        return None;
    }

    match serde_json_core::from_slice::<T>(bytes) {
        Ok((val, _len)) => Some(val),
        Err(err) => {
            log::error!("Error parsing json: {err:?}");
            None
        }
    }
}

/// Defines the json data structure to transceive over the cmd topic
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "lowercase")]
pub struct TestPayload {
    /// Timestamp in milliseconds since epoch
    pub ts: i64,
    pub values: heapless::Vec<f32, 128>,
}

#[cfg(test)]
mod test {

    use super::*;
    use crate::{MAX_TOPIC_LEN, MAX_TOPIC_PREFIX_LEN, MAX_UID_LEN};
    use strum::IntoEnumIterator;

    const TEST_UID: &str = "1234-5678";
    const TEST_PREFIX: &str = "prefix-1";

    #[test]
    fn test_length_validation() {
        assert_eq!(TEST_UID.len(), MAX_UID_LEN);
        validate_uid(TEST_UID).unwrap();

        assert_eq!(TEST_PREFIX.len(), MAX_TOPIC_PREFIX_LEN);
        validate_prefix(TEST_PREFIX).unwrap();
    }

    #[test]
    fn test_parse_topic() {
        let full_topic: heapless::String<MAX_TOPIC_LEN> =
            heapless::String::try_from("t/test/commands").unwrap();
        let topic = parse_topic_to_device(full_topic.as_str()).unwrap();
        assert_eq!(topic.prefix, "t");
        assert_eq!(topic.device_id, "test");
        assert_eq!(topic.topic, TopicToDevice::Command);

        let full_topic: heapless::String<MAX_TOPIC_LEN> =
            heapless::String::try_from("t/mock/time").unwrap();
        let topic = parse_topic_to_device(full_topic.as_str()).unwrap();
        assert_eq!(topic.prefix, "t");
        assert_eq!(topic.device_id, "mock");
        assert_eq!(topic.topic, TopicToDevice::Time);
    }

    /// Check all topics serialize/deserialize strings match
    #[test]
    fn test_all_from_topic_names_serialize() {
        for topic in TopicFromDevice::iter() {
            let serialized = topic
                .to_topic_string_with_prefix(TEST_PREFIX, TEST_UID)
                .unwrap();
            assert!(serialized.len() <= MAX_TOPIC_LEN);

            let deserialized = parse_topic_from_device(serialized.as_str()).unwrap_or_else(|_| {
                panic!("Topic '{topic:?}' serialized to '{serialized}' but failed to deserialize")
            });
            assert_eq!(deserialized.prefix, TEST_PREFIX);
            assert_eq!(deserialized.device_id, TEST_UID);
            assert_eq!(topic, deserialized.topic);
        }
    }

    /// Check all topics serialize/deserialize strings match
    #[test]
    fn test_all_to_topic_names_serialize() {
        for topic in TopicToDevice::iter() {
            let serialized = topic
                .to_topic_string_with_prefix(TEST_PREFIX, TEST_UID)
                .unwrap();
            assert!(serialized.len() <= MAX_TOPIC_LEN);

            let deserialized = parse_topic_to_device(serialized.as_str()).unwrap_or_else(|_| {
                panic!("Topic '{topic:?}' serialized to '{serialized}' but failed to deserialize")
            });
            assert_eq!(deserialized.prefix, TEST_PREFIX);
            assert_eq!(deserialized.device_id, TEST_UID);
            assert_eq!(topic, deserialized.topic);
        }
    }

    #[test]
    #[cfg(feature = "use-std")]
    fn test_parse_system_topics() {
        for topic in SystemTopic::iter() {
            let topic_str = topic.as_str();
            let parsed_topic = parse_system_topic(topic_str);
            assert_eq!(topic, parsed_topic.unwrap());
        }
    }
}

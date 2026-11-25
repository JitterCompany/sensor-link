pub mod data;
pub mod event;
pub mod settings;
pub mod status;

use serde::{Deserialize, Serialize};

use sensor_link_protocol::{
    build_topic_string, Topic, TopicParseError, TopicParts, TopicSerializeError, TopicString,
};

/// Demo-specific topics published by the device, plus every topic shared with all
/// Jitter devices (wrapped in [TopicFromDevice::Common]).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TopicFromDevice {
    Data,

    /// Topic shared with all Jitter devices, see [sensor_link_protocol::TopicFromDevice]
    Common(sensor_link_protocol::TopicFromDevice),
}

/// Demo-specific topics sent to the device, plus every topic shared with all
/// Jitter devices (wrapped in [TopicToDevice::Common]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TopicToDevice {
    Settings,

    /// Topic shared with all Jitter devices, see [sensor_link_protocol::TopicToDevice]
    Common(sensor_link_protocol::TopicToDevice),
}

/// Wrap a manufacturer-generic Jitter topic as the corresponding device topic.
impl From<sensor_link_protocol::TopicFromDevice> for TopicFromDevice {
    fn from(topic: sensor_link_protocol::TopicFromDevice) -> Self {
        TopicFromDevice::Common(topic)
    }
}

impl From<sensor_link_protocol::TopicToDevice> for TopicToDevice {
    fn from(topic: sensor_link_protocol::TopicToDevice) -> Self {
        TopicToDevice::Common(topic)
    }
}

impl TopicFromDevice {
    /// Convert to a full topic string in the "f/uid/topic" format.
    pub fn to_topic_string(self, uid: impl AsRef<str>) -> Result<TopicString, TopicSerializeError> {
        self.to_topic_string_with_prefix("f", uid)
    }

    /// Convert to a full topic string with custom prefix in the format "prefix/uid/topic".
    pub fn to_topic_string_with_prefix(
        self,
        prefix: impl AsRef<str>,
        uid: impl AsRef<str>,
    ) -> Result<TopicString, TopicSerializeError> {
        if let TopicFromDevice::Common(inner) = self {
            return inner.to_topic_string_with_prefix(prefix, uid);
        }

        let topic_str = match self {
            TopicFromDevice::Common(c) => c.topic_name(),
            TopicFromDevice::Data => "data",
        };

        build_topic_string(prefix, uid, topic_str)
    }
}

impl Topic for TopicFromDevice {
    fn to_topic_string(&self, uid: &str) -> Result<TopicString, TopicSerializeError> {
        (*self).to_topic_string(uid)
    }
}

impl TopicToDevice {
    /// Iterate over every variant (device-specific topics followed by the shared Jitter topics).
    pub fn iter() -> impl Iterator<Item = Self> {
        use strum::IntoEnumIterator;
        [TopicToDevice::Settings]
            .into_iter()
            .chain(sensor_link_protocol::TopicToDevice::iter().map(TopicToDevice::Common))
    }

    /// Convert to a full topic string in the "t/uid/topic" format.
    pub fn to_topic_string(self, uid: impl AsRef<str>) -> Result<TopicString, TopicSerializeError> {
        self.to_topic_string_with_prefix("t", uid)
    }

    /// Convert to a full topic string with custom prefix in the format "prefix/uid/topic".
    pub fn to_topic_string_with_prefix(
        self,
        prefix: impl AsRef<str>,
        uid: impl AsRef<str>,
    ) -> Result<TopicString, TopicSerializeError> {
        if let TopicToDevice::Common(inner) = self {
            return inner.to_topic_string_with_prefix(prefix, uid);
        }

        let topic_str = match self {
            TopicToDevice::Common(_) => unreachable!("handled above"),
            TopicToDevice::Settings => "settings",
        };

        build_topic_string(prefix, uid, topic_str)
    }
}

/// Parse [TopicFromDevice] from a "prefix/uid/topic_name" string.
/// Note: `mismatched_lifetime_syntaxes` is allowed because only the "not use-std" feature return type requires a lifetime parameter.
#[allow(mismatched_lifetime_syntaxes)]
pub fn parse_topic_from_device(
    topic: &str,
) -> Result<TopicParts<TopicFromDevice>, TopicParseError> {
    if let Ok(parts) = sensor_link_protocol::parse_topic_from_device(topic) {
        return Ok(TopicParts {
            prefix: parts.prefix,
            device_id: parts.device_id,
            topic: TopicFromDevice::Common(parts.topic),
        });
    }

    let mut parts = topic.splitn(3, '/');
    let prefix = parts.next().ok_or(TopicParseError::MissingPrefix)?;
    let device_id = parts.next().ok_or(TopicParseError::MissingPrefix)?;
    let topic_name = parts.next().ok_or(TopicParseError::MissingPrefix)?;

    let topic = match topic_name {
        "data" => TopicFromDevice::Data,
        _ => return Err(TopicParseError::NoMatch),
    };

    Ok(TopicParts {
        prefix: prefix.into(),
        device_id: device_id.into(),
        topic,
    })
}

/// Parse [TopicToDevice] from a "prefix/uid/topic_name" string.
/// Note: `mismatched_lifetime_syntaxes` is allowed because only the "not use-std" feature return type requires a lifetime parameter.
#[allow(mismatched_lifetime_syntaxes)]
pub fn parse_topic_to_device(topic: &str) -> Result<TopicParts<TopicToDevice>, TopicParseError> {
    if let Ok(parts) = sensor_link_protocol::parse_topic_to_device(topic) {
        return Ok(TopicParts {
            prefix: parts.prefix,
            device_id: parts.device_id,
            topic: TopicToDevice::Common(parts.topic),
        });
    }

    let mut parts = topic.splitn(3, '/');
    let prefix = parts.next().ok_or(TopicParseError::MissingPrefix)?;
    let device_id = parts.next().ok_or(TopicParseError::MissingPrefix)?;
    let topic_name = parts.next().ok_or(TopicParseError::MissingPrefix)?;

    let topic = match topic_name {
        "settings" => TopicToDevice::Settings,
        _ => return Err(TopicParseError::NoMatch),
    };

    Ok(TopicParts {
        prefix: prefix.into(),
        device_id: device_id.into(),
        topic,
    })
}

#[cfg(test)]
mod test {
    use super::*;
    use sensor_link_protocol::MAX_TOPIC_LEN;

    #[test]
    fn test_parse_topic_roundtrips() {
        for topic in TopicToDevice::iter() {
            let serialized = topic.to_topic_string_with_prefix("t", "1234-5678").unwrap();
            assert!(serialized.len() <= MAX_TOPIC_LEN);
            let deserialized = parse_topic_to_device(serialized.as_str()).unwrap_or_else(|_| {
                panic!("Topic '{topic:?}' serialized to '{serialized}' but failed to deserialize")
            });
            assert_eq!(topic, deserialized.topic);
        }
    }
}

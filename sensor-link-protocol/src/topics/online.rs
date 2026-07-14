use serde::{Deserialize, Serialize};

use super::TopicPayloadSerialize;
use crate::{Milliseconds, MAX_ONLINE_PAYLOAD_LEN};

/// Online status of a device.
/// This is used to indicate whether a device is online or offline.
/// since is the timestamp that indicates when the device last came online.
/// If the device is offline, since is None.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Online {
    /// Timestamp that indicates when the device last came online.
    /// If the device is offline, since is None.
    #[serde(skip_serializing_if = "Option::is_none")]
    since: Option<Milliseconds>,
    /// Canary: if this flag is missing the device disconnected ungracefully
    #[serde(skip_serializing_if = "Option::is_none")]
    c: Option<i8>,
}

impl Online {
    /// Returns an Online object representing an offline device.
    pub fn no() -> Self {
        Self {
            since: None,
            c: Some(1),
        }
    }

    /// Returns an Online object representing an online device.
    /// `since`` is the timestamp that indicates when the device last came online.
    pub fn yes(since: Milliseconds) -> Self {
        Self {
            since: Some(since),
            c: None,
        }
    }

    /// Returns an Online object representing an offline device.
    /// This is used to indicate that the device disconnected ungracefully.
    pub fn will() -> Self {
        Self {
            since: None,
            c: None,
        }
    }

    /// Returns the time since the device was last online.
    /// If the device is offline, returns None.
    pub fn since(&self) -> Option<Milliseconds> {
        self.since.clone()
    }

    /// Returns true if this message means that the device disconnected ungracefully.
    pub fn ungraceful_offline(&self) -> bool {
        self.since.is_none() && self.c.is_none()
    }
}

impl TopicPayloadSerialize<MAX_ONLINE_PAYLOAD_LEN> for Online {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialization() {
        let online = Online::yes(Milliseconds(12345678901234));
        let serialized = online.serialize_topic_payload().unwrap();
        assert_eq!(
            std::str::from_utf8(&serialized).unwrap(),
            r#"{"since":12345678901234}"#
        );

        let offline = Online::no();
        let serialized = offline.serialize_topic_payload().unwrap();
        assert_eq!(std::str::from_utf8(&serialized).unwrap(), r#"{"c":1}"#);

        let offline_unexpected = Online::will();
        let serialized = offline_unexpected.serialize_topic_payload().unwrap();
        assert_eq!(std::str::from_utf8(&serialized).unwrap(), r#"{}"#);

        let online = Online::yes(Milliseconds(17491303460000));
        let serialized = online.serialize_topic_payload().unwrap();
        assert_eq!(
            std::str::from_utf8(&serialized).unwrap(),
            r#"{"since":17491303460000}"#
        );
    }
}

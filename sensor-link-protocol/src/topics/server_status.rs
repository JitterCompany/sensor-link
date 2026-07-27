use super::TopicPayloadSerialize;
use crate::MAX_MESSAGE_LEN;
use serde::{Deserialize, Serialize};

#[derive(thiserror_no_std::Error, Debug)]
pub enum Error {
    #[error("{0}")]
    JsonDeserializeError(#[from] serde_json_core::de::Error),
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ServerStatus {
    /// Server online
    Ok,
    /// Server offline,
    Offline,
    /// Means do not send any data that may not get lost
    Maintenance,
}

impl ServerStatus {
    pub fn from_slice(slice: &[u8]) -> Result<Self, Error> {
        Ok(serde_json_core::from_slice(slice).map(|(val, _len)| val)?)
    }

    #[cfg(feature = "use-std")]
    pub fn serialize(&self) -> String {
        serde_json::to_string(self).unwrap()
    }
}

impl TopicPayloadSerialize<MAX_MESSAGE_LEN> for ServerStatus {}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_from_slice() {
        assert_eq!(
            ServerStatus::from_slice(b"\"ok\"").unwrap(),
            ServerStatus::Ok
        );
        assert_eq!(
            ServerStatus::from_slice(b"\"offline\"").unwrap(),
            ServerStatus::Offline
        );
        assert_eq!(
            ServerStatus::from_slice(b"\"maintenance\"").unwrap(),
            ServerStatus::Maintenance
        );
        assert!(ServerStatus::from_slice(b"\"unknown\"").is_err());
    }

    #[cfg(feature = "use-std")]
    #[test]
    fn test_serialize() {
        assert_eq!(ServerStatus::Ok.serialize(), "\"ok\"");
        assert_eq!(ServerStatus::Offline.serialize(), "\"offline\"");
        assert_eq!(ServerStatus::Maintenance.serialize(), "\"maintenance\"");
    }

    #[cfg(feature = "use-std")]
    #[test]
    fn test_serializ_deserialize() {
        let ok = ServerStatus::Ok;
        let ok_serialized = ok.serialize();
        let ok_deserialized = ServerStatus::from_slice(ok_serialized.as_bytes()).unwrap();
        assert_eq!(ok, ok_deserialized);

        let offline = ServerStatus::Offline;
        let offline_serialized = offline.serialize();
        let offline_deserialized = ServerStatus::from_slice(offline_serialized.as_bytes()).unwrap();
        assert_eq!(offline, offline_deserialized);

        let maintenance = ServerStatus::Maintenance;
        let maintenance_serialized = maintenance.serialize();
        let maintenance_deserialized =
            ServerStatus::from_slice(maintenance_serialized.as_bytes()).unwrap();
        assert_eq!(maintenance, maintenance_deserialized);
    }
}

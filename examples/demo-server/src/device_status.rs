use demo_protocol::status::{DeviceStatus as ProtocolDeviceStatus, Status as BaseStatus};
use sensor_link_server_core::device::DeviceStatusLike;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Default, strum::Display)]
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
    Connected,
    NothingConnected,
    NoSensorError,
    OutOfRangeError,
    FaultyChannelError,
}

impl From<BaseStatus> for Status {
    fn from(s: BaseStatus) -> Self {
        match s {
            BaseStatus::Unknown => Status::Unknown,
            BaseStatus::Startup => Status::Startup,
            BaseStatus::Inactive => Status::Inactive,
            BaseStatus::Active => Status::Active,
            BaseStatus::TryActive => Status::TryActive,
            BaseStatus::Idle => Status::Idle,
            BaseStatus::Waiting => Status::Waiting,
            BaseStatus::NoSensor => Status::NoSensor,
            BaseStatus::ConfigOutdated => Status::ConfigOutdated,
            BaseStatus::NoConfig => Status::NoConfig,
            BaseStatus::Error => Status::Error,
        }
    }
}

/// Server-side device status.
///
/// Wraps [`ProtocolDeviceStatus`] rather than re-declaring its fields, so the protocol payload has
/// a single source of truth and can't silently drift out of sync. The extended [`Status`] is
/// *derived* from `inner` (extending the protocol status with server-only states) and stored so
/// [`DeviceStatusLike::status`] can hand out a reference to it.
///
/// (De)serialization keeps only the protocol payload on the wire: `inner` is flattened (so the
/// persisted form is exactly [`ProtocolDeviceStatus`], keeping the original base status as ground
/// truth) and the derived `status` is skipped. Deserialization routes through
/// [`From<ProtocolDeviceStatus>`] so the derived status is recomputed from the protocol fields
/// rather than defaulting — the derivation is the single place that logic lives.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(from = "ProtocolDeviceStatus")]
pub struct DeviceStatus {
    #[serde(flatten)]
    pub inner: ProtocolDeviceStatus,
    #[serde(skip)]
    pub status: Status,
}

impl From<ProtocolDeviceStatus> for DeviceStatus {
    fn from(inner: ProtocolDeviceStatus) -> Self {
        let status = Status::from(inner.status.clone());
        Self { inner, status }
    }
}

impl DeviceStatusLike for DeviceStatus {
    type Status = Status;

    fn status(&self) -> &Status {
        &self.status
    }

    fn is_active_or_idle(&self) -> bool {
        matches!(self.status, Status::Active | Status::Idle)
    }

    fn is_inactive(&self) -> bool {
        matches!(self.status, Status::Inactive)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_as_protocol_payload_with_base_status() {
        let ds = DeviceStatus::from(ProtocolDeviceStatus {
            status: BaseStatus::Active,
            signal_strength: -42,
            ..Default::default()
        });

        let json = serde_json::to_value(&ds).unwrap();
        // Flat protocol shape: base status on the wire, derived `status` not persisted separately.
        assert_eq!(json["status"], "active");
        assert_eq!(json["signal_strength"], -42);
        let keys = json.as_object().unwrap();
        assert_eq!(keys.keys().filter(|k| *k == "status").count(), 1);
        assert!(!keys.contains_key("inner"));
    }

    #[test]
    fn deserialize_rederives_extended_status_from_protocol_fields() {
        // Only the protocol payload is on the wire; the extended status must be recomputed.
        let json = r#"{"settings_hash":7,"status":"idle","signal_strength":0,"temperature":0.0}"#;
        let ds: DeviceStatus = serde_json::from_str(json).unwrap();

        assert_eq!(ds.inner.settings_hash, 7);
        assert_eq!(ds.inner.status, BaseStatus::Idle);
        // Derived rather than left at Default::default() (== Unknown).
        assert_eq!(ds.status, Status::Idle);
    }

    #[test]
    fn round_trips_through_protocol_payload() {
        let ds = DeviceStatus::from(ProtocolDeviceStatus {
            status: BaseStatus::Waiting,
            ..Default::default()
        });

        let json = serde_json::to_string(&ds).unwrap();
        let back: DeviceStatus = serde_json::from_str(&json).unwrap();

        assert_eq!(back.inner.status, BaseStatus::Waiting);
        assert_eq!(back.status, Status::Waiting);
    }
}

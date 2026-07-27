use chrono::{DateTime, Utc};
use field_types::{FieldName, FieldType};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{fmt::Display, str::FromStr};

use crate::{DataStoreId, MeteorId};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, Default)]
pub enum Command {
    #[default]
    None,
    Start,
    Stop,
    Reboot,
    Format,
}

pub enum CommandIntoError {
    /// No command, no need to send (this is not an error)
    NoCommand,
    /// Trying to convert an unsupported command.
    Unsupported,
}

impl TryFrom<Command> for sensor_link_protocol::cmd::Cmd {
    type Error = CommandIntoError;

    fn try_from(value: Command) -> std::result::Result<Self, Self::Error> {
        match value {
            Command::None => Err(CommandIntoError::NoCommand),
            Command::Start => Ok(sensor_link_protocol::cmd::Cmd::Start),
            Command::Stop => Ok(sensor_link_protocol::cmd::Cmd::Stop),
            Command::Reboot => Ok(sensor_link_protocol::cmd::Cmd::Reboot),
            Command::Format => Err(CommandIntoError::Unsupported),
        }
    }
}

impl Display for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::None => "",
                Self::Start => "start",
                Self::Stop => "stop",
                Self::Reboot => "reboot",
                Self::Format => "force format",
            }
        )
    }
}

impl FromStr for Command {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "" => Ok(Self::None),
            "start" => Ok(Self::Start),
            "stop" => Ok(Self::Stop),
            "reboot" => Ok(Self::Reboot),
            "format" | "force format" => Ok(Self::Format),
            _ => Err(format!("Unknown command: {}", s)),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(rename_all(serialize = "lowercase"))]
pub enum CommandForFrontend {
    #[default]
    #[serde(rename(serialize = ""))]
    None,
    Start,
    Stop,
    Reboot,
    #[serde(rename(serialize = "force format"))]
    Format,
}

pub trait DeviceStatusLike:
    Default + Clone + Serialize + DeserializeOwned + Send + Sync + 'static
{
    type Status: PartialEq + Clone;
    fn status(&self) -> &Self::Status;
    fn is_active_or_idle(&self) -> bool;
    fn is_inactive(&self) -> bool;
}

impl DeviceStatusLike for () {
    type Status = ();
    fn status(&self) -> &() {
        self
    }
    fn is_active_or_idle(&self) -> bool {
        false
    }
    fn is_inactive(&self) -> bool {
        false
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, Default)]
pub struct Version {
    pub firmware: String,
    pub hardware: String,
    pub bootloader: String,
    pub api: String,
    #[serde(default)]
    pub modem_model: String,
    #[serde(default)]
    pub modem_firmware_version: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Location {
    pub lat: f32,
    pub lng: f32,
}

#[derive(Clone, Debug, Deserialize, Serialize, FieldName, FieldType)]
#[serde(bound(
    serialize = "DT: Serialize, DS: Serialize",
    deserialize = "DT: DeserializeOwned, DS: DeserializeOwned"
))]
pub struct Device<DT, DS: DeviceStatusLike> {
    #[serde(rename = "_id")]
    #[field_name(skip)]
    #[field_types(skip)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[field_name(skip)]
    pub device_type: DT,
    pub hub_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub group_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_contact: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub version: Option<Version>,
    #[serde(default)]
    pub marked_for_update: Option<DataStoreId>,
    #[serde(default)]
    pub command: Command,
    #[serde(default)]
    pub sync_interval_min: Option<f32>,
    #[serde(default)]
    pub waiting_for_new_mp: bool,
    #[serde(default)]
    pub config_confirmed: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub config_last_sent_at: Option<i64>,
    pub calibration_date: Option<i64>,
    #[serde(default)]
    pub documents: Vec<String>,
    #[serde(default)]
    pub device_status: DS,
    #[serde(default)]
    pub status_since: i64,
    #[serde(default)]
    pub register_time: i64,
    #[serde(default)]
    pub baseline_values: Vec<f32>,
    #[serde(default)]
    pub baseline_date: Option<i64>,
    #[serde(default)]
    pub sim_iccid: String,
    #[serde(default)]
    pub license_start: Option<i64>,
    #[serde(default)]
    pub online_since: Option<DateTime<Utc>>,
}

impl<DT: Default, DS: DeviceStatusLike> Default for Device<DT, DS> {
    fn default() -> Self {
        Self {
            id: Default::default(),
            name: Default::default(),
            device_type: DT::default(),
            group_id: Default::default(),
            last_contact: Default::default(),
            version: Default::default(),
            marked_for_update: Default::default(),
            command: Default::default(),
            sync_interval_min: Default::default(),
            waiting_for_new_mp: Default::default(),
            config_confirmed: Default::default(),
            config_last_sent_at: Default::default(),
            calibration_date: Default::default(),
            documents: Default::default(),
            device_status: Default::default(),
            status_since: Default::default(),
            register_time: Default::default(),
            baseline_values: Default::default(),
            baseline_date: Default::default(),
            hub_id: Default::default(),
            sim_iccid: Default::default(),
            license_start: Default::default(),
            online_since: Default::default(),
        }
    }
}

impl<DT, DS: DeviceStatusLike> DeviceFieldType<DT, DS> {
    pub fn field_name(&self) -> &'static str {
        match self {
            DeviceFieldType::DeviceType(_) => "device_type",
            DeviceFieldType::GroupId(_) => DeviceFieldName::GroupId.name(),
            DeviceFieldType::Name(_) => DeviceFieldName::Name.name(),
            DeviceFieldType::LastContact(_) => DeviceFieldName::LastContact.name(),
            DeviceFieldType::Version(_) => DeviceFieldName::Version.name(),
            DeviceFieldType::MarkedForUpdate(_) => DeviceFieldName::MarkedForUpdate.name(),
            DeviceFieldType::Command(_) => DeviceFieldName::Command.name(),
            DeviceFieldType::SyncIntervalMin(_) => DeviceFieldName::SyncIntervalMin.name(),
            DeviceFieldType::WaitingForNewMp(_) => DeviceFieldName::WaitingForNewMp.name(),
            DeviceFieldType::ConfigConfirmed(_) => DeviceFieldName::ConfigConfirmed.name(),
            DeviceFieldType::ConfigLastSentAt(_) => DeviceFieldName::ConfigLastSentAt.name(),
            DeviceFieldType::CalibrationDate(_) => DeviceFieldName::CalibrationDate.name(),
            DeviceFieldType::Documents(_) => DeviceFieldName::Documents.name(),
            DeviceFieldType::DeviceStatus(_) => DeviceFieldName::DeviceStatus.name(),
            DeviceFieldType::StatusSince(_) => DeviceFieldName::StatusSince.name(),
            DeviceFieldType::RegisterTime(_) => DeviceFieldName::RegisterTime.name(),
            DeviceFieldType::BaselineValues(_) => DeviceFieldName::BaselineValues.name(),
            DeviceFieldType::BaselineDate(_) => DeviceFieldName::BaselineDate.name(),
            DeviceFieldType::HubId(_) => DeviceFieldName::HubId.name(),
            DeviceFieldType::SimIccid(_) => DeviceFieldName::SimIccid.name(),
            DeviceFieldType::LicenseStart(_) => DeviceFieldName::LicenseStart.name(),
            DeviceFieldType::OnlineSince(_) => DeviceFieldName::OnlineSince.name(),
        }
    }
}

/// Device definition for HTTP API
#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceExt<DT, DS: Default> {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub device_type: DT,
    pub hub_id: Option<String>,
    pub cluster_id: Option<String>,
    #[serde(default)]
    pub mp_id: Option<String>,
    #[serde(default)]
    pub mp_name: String,
    #[serde(default)]
    pub project_id: MeteorId,
    #[serde(default)]
    pub last_contact: i64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub group_id: Option<String>,
    #[serde(default)]
    pub version: Version,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub marked_for_update: Option<String>,
    #[serde(default)]
    pub command: CommandForFrontend,
    #[serde(default)]
    pub sync_interval_min: Option<f32>,
    #[serde(default)]
    pub waiting_for_new_mp: bool,
    #[serde(default)]
    pub config_confirmed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calibration_date: Option<i64>,
    #[serde(default)]
    pub documents: Vec<String>,
    #[serde(default)]
    pub device_status: DS,
    #[serde(default)]
    pub status_since: i64,
    #[serde(default)]
    pub active_start: Option<DateTime<Utc>>,
    #[serde(default)]
    pub active_end: Option<DateTime<Utc>>,
    #[serde(default)]
    pub location: Option<Location>,
    #[serde(default)]
    pub register_time: Option<i64>,
    #[serde(default)]
    pub monitoring_enabled: bool,
    #[serde(default)]
    pub registration_enabled: bool,
    #[serde(default)]
    pub baseline_values: Vec<f32>,
    #[serde(default)]
    pub baseline_date: Option<i64>,
    #[serde(default)]
    pub sim_iccid: String,
    #[serde(default)]
    pub license_start: Option<i64>,
    #[serde(default)]
    pub online_since: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_sync_interval_min: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_calibration: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_scheduled_command: Option<ClusterScheduledCommand>,
}

/// Subset of a cluster's scheduled command needed by status views.
#[derive(Debug, Serialize, Deserialize)]
pub struct ClusterScheduledCommand {
    pub command: String,
    #[serde(default)]
    pub completed: bool,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct DeviceQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<MeteorId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_ids: Option<Vec<MeteorId>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster_id: Option<MeteorId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sensor_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_types: Option<Vec<String>>,
}

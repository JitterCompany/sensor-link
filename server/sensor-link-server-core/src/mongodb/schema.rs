use bson::{oid::ObjectId, DateTime};
use serde::{Deserialize, Serialize};

use crate::{
    data_export::DataExportStatus,
    data_kind::DataKind,
    data_set::{DataSet as ModelDataSet, NewDataSet as ModelNewDataSet},
    event::{
        Comment, Event as ModelEvent, EventStats, EventType,
        EventsWithStats as ModelEventsWithStats, NewEvent as ModelNewEvent, ProjectHourStats,
        SendStatus,
    },
    firmware::{Firmware as ModelFirmware, NewFirmware as ModelNewFirmware},
    sensor_data::{MetaData as ModelMetaData, SensorData as ModelSensorData},
    sensor_server_log::SensorServerLog as ModelSensorServerLog,
    store_traits::{DataStoreError, Result},
    MeteorId, TimeRange,
};

// ── Collection name constants ─────────────────────────────────────────────────

pub const DEVICE_COLL_NAME: &str = "devices";
pub const FIRMWARE_COLL_NAME: &str = "firmware";
pub const SENSOR_SERVER_LOG_COLL_NAME: &str = "sensorServerLog";
pub const EVENTS_COLL_NAME: &str = "events";
pub const S2D_EVENTS_COLL_NAME: &str = "s2d/s.events";
pub const TRACE_EVENT_COLL_NAME: &str = "traceEvents";
pub const DATA_SET_COLL_NAME: &str = "dataSets";
pub const DATA_EXPORT_COLL_NAME: &str = "dataExports";
pub const MEAS_POINT_COLL_NAME: &str = "measurementPoints";
pub const MEAS_POINT_SHADOW_COLL_NAME: &str = "measurementPointShadows";
pub const EVENT_DESCRIPTORS_COLL_NAME: &str = "eventDescriptors";
pub const S2D_CONFIGURATION_COLL_NAME: &str = "s2d/s.config";
pub const CLUSTER_COLL_NAME: &str = "clusters";
pub const CLUSTER_SHADOW_COLL_NAME: &str = "clusterShadows";
pub const PROJECT_COLL_NAME: &str = "projects";
pub const GROUP_COLL_NAME: &str = "groups";

// ── Firmware ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize)]
#[allow(non_snake_case)]
pub struct NewFirmware<D> {
    version: String,
    description: String,
    date: DateTime,
    v2BinID: String,
    recommended: bool,
    device_type: D,
}

impl<D> TryFrom<ModelNewFirmware<D>> for NewFirmware<D> {
    type Error = DataStoreError;

    fn try_from(value: ModelNewFirmware<D>) -> Result<Self> {
        Ok(Self {
            version: value.version,
            description: value.description,
            date: value.date.into(),
            v2BinID: value.v2BinID,
            recommended: value.recommended,
            device_type: value.device_type,
        })
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[allow(non_snake_case)]
pub struct Firmware<D> {
    #[serde(rename = "_id")]
    pub id: ObjectId,
    version: String,
    description: String,
    date: DateTime,
    pub v2BinID: String,
    recommended: bool,
    device_type: D,
}

impl<D> From<Firmware<D>> for ModelFirmware<D> {
    fn from(value: Firmware<D>) -> Self {
        Self {
            id: value.id.to_hex(),
            version: value.version,
            description: value.description,
            date: value.date.into(),
            v2BinID: value.v2BinID,
            recommended: value.recommended,
            device_type: value.device_type,
        }
    }
}

// ── Sensor Data ───────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(bound(
    serialize = "C: serde::Serialize",
    deserialize = "C: serde::de::DeserializeOwned"
))]
pub struct SensorData<C: DataKind> {
    pub metadata: MetaData<C>,
    pub time: DateTime,
    pub value: f32,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub min: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub max: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub perc: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub freq: Option<f32>,
}

impl<C: DataKind> From<ModelSensorData<C>> for SensorData<C> {
    fn from(value: ModelSensorData<C>) -> Self {
        Self {
            metadata: value.metadata.into(),
            time: value.time.into(),
            value: value.value,
            min: value.min,
            max: value.max,
            perc: value.percentage,
            freq: value.frequency,
        }
    }
}

impl<C: DataKind> From<SensorData<C>> for ModelSensorData<C> {
    fn from(value: SensorData<C>) -> Self {
        Self {
            metadata: value.metadata.into(),
            time: value.time.into(),
            value: value.value,
            min: value.min,
            max: value.max,
            percentage: value.perc,
            frequency: value.freq,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(bound(
    serialize = "C: serde::Serialize",
    deserialize = "C: serde::de::DeserializeOwned"
))]
pub struct MetaData<C: DataKind> {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub measuring_point_id: Option<ObjectId>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub data_set_id: Option<ObjectId>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub data_channel: Option<C>,
}

impl<C: DataKind> From<ModelMetaData<C>> for MetaData<C> {
    fn from(value: ModelMetaData<C>) -> Self {
        let measuring_point_id = value
            .measuring_point_id
            .map(|s| ObjectId::parse_str(&s))
            .transpose()
            .ok()
            .flatten();
        let data_set_id = value
            .data_set_id
            .map(|s| ObjectId::parse_str(&s))
            .transpose()
            .ok()
            .flatten();
        Self {
            device_id: value.device_id,
            measuring_point_id,
            data_set_id,
            data_channel: value.data_channel,
        }
    }
}

impl<C: DataKind> From<MetaData<C>> for ModelMetaData<C> {
    fn from(value: MetaData<C>) -> Self {
        Self {
            device_id: value.device_id,
            measuring_point_id: value.measuring_point_id.map(|id| id.to_hex()),
            data_set_id: value.data_set_id.map(|id| id.to_hex()),
            data_channel: value.data_channel,
        }
    }
}

// ── DataSet ───────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize)]
pub struct NewDataSet {
    #[serde(rename = "_id", skip_serializing_if = "Option::is_none", default)]
    pub id: Option<ObjectId>,
    pub sensor_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub measuring_point_id: Option<ObjectId>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub configuration_id: Option<ObjectId>,
    pub start: i64,
}

impl TryFrom<ModelNewDataSet> for NewDataSet {
    type Error = DataStoreError;

    fn try_from(value: ModelNewDataSet) -> Result<Self> {
        Ok(Self {
            id: value.id.map(|id| ObjectId::parse_str(&id)).transpose()?,
            sensor_id: value.sensor_id,
            measuring_point_id: value
                .measuring_point_id
                .map(|id| ObjectId::parse_str(&id))
                .transpose()?,
            configuration_id: value
                .configuration_id
                .map(|id| ObjectId::parse_str(&id))
                .transpose()?,
            start: value.start,
        })
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DataSet {
    #[serde(rename = "_id")]
    pub id: ObjectId,
    pub sensor_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub measuring_point_id: Option<ObjectId>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub configuration_id: Option<ObjectId>,
    pub start: i64,
}

impl From<DataSet> for ModelDataSet {
    fn from(value: DataSet) -> Self {
        Self {
            id: value.id.to_hex(),
            sensor_id: value.sensor_id,
            measuring_point_id: value.measuring_point_id.map(|id| id.to_hex()),
            configuration_id: value.configuration_id.map(|id| id.to_hex()),
            start: value.start,
        }
    }
}

// ── DataExport ────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[serde(bound(
    serialize = "DT: serde::Serialize",
    deserialize = "DT: serde::de::DeserializeOwned"
))]
pub struct DataExport<DT: DataKind> {
    #[serde(rename = "_id", skip_serializing_if = "Option::is_none")]
    pub id: Option<ObjectId>,
    pub timestamp: DateTime,
    pub measuring_point_id: ObjectId,
    #[serde(alias = "data_channel")]
    pub data_type: DT,
    pub project_id: MeteorId,
    pub time_range: TimeRange,
    pub file_id: Option<ObjectId>,
    pub file_size: Option<u64>,
    pub status: DataExportStatus,
    pub trigger_id: Option<i64>,
}

impl<DT: DataKind> From<DataExport<DT>> for crate::data_export::DataExport<DT> {
    fn from(val: DataExport<DT>) -> Self {
        crate::data_export::DataExport {
            id: val.id.map(|id| id.to_hex()),
            measuring_point_id: val.measuring_point_id.to_hex(),
            data_type: val.data_type,
            project_id: val.project_id,
            time_range: val.time_range,
            file_id: val.file_id.map(|id| id.to_hex()),
            file_size: val.file_size,
            status: val.status,
            trigger_id: val.trigger_id,
        }
    }
}

// ── SensorServerLog ───────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct SensorServerLog {
    #[serde(rename = "_id", skip_serializing_if = "Option::is_none")]
    id: Option<ObjectId>,
    #[serde(rename = "sensorID")]
    sensor_id: String,
    #[serde(rename = "groupID")]
    group_id: Option<String>,
    #[serde(rename = "userID")]
    user_id: String,
    timestamp: DateTime,
    duration: u32,
    #[serde(rename = "type")]
    _type: String,
    header: String,
    body: Vec<String>,
}

impl From<ModelSensorServerLog> for SensorServerLog {
    fn from(value: ModelSensorServerLog) -> Self {
        Self {
            id: value.id.and_then(|id| ObjectId::parse_str(&id).ok()),
            sensor_id: value.sensor_id,
            group_id: value.group_id,
            user_id: value.user_id,
            timestamp: value.timestamp.into(),
            duration: value.duration,
            _type: value._type,
            header: value.header,
            body: value.body,
        }
    }
}

impl From<SensorServerLog> for ModelSensorServerLog {
    fn from(value: SensorServerLog) -> Self {
        Self {
            id: value.id.map(|id| id.to_hex()),
            sensor_id: value.sensor_id,
            group_id: value.group_id,
            user_id: value.user_id,
            timestamp: value.timestamp.into(),
            duration: value.duration,
            _type: value._type,
            header: value.header,
            body: value.body,
        }
    }
}

// ── Events ────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct Event<C, D> {
    #[serde(rename = "_id", skip_serializing_if = "Option::is_none", default)]
    pub id: Option<ObjectId>,
    #[serde(default)]
    pub sent: SendStatus,
    #[serde(rename = "type")]
    pub _type: EventType,
    pub code: u32,
    pub timestamp: DateTime,
    pub project_id: Option<MeteorId>,
    pub group_id: Option<MeteorId>,
    #[serde(default)]
    pub has_server_time: bool,
    pub contact_details: Option<C>,
    pub message: Option<String>,
    pub msg: Option<String>,
    pub msg_en: Option<String>,
    #[serde(default)]
    pub comments: Vec<Comment>,
    pub device_id: Option<String>,
    pub measuring_point_id: Option<ObjectId>,
    pub project_name: Option<String>,
    pub cluster_name: Option<String>,
    pub measuring_point_name: Option<String>,
    pub device_name: Option<String>,
    pub group_name: Option<String>,
    #[serde(flatten)]
    pub event_details: Option<D>,
}

impl<C, D, ModelC, ModelD> From<ModelNewEvent<ModelC, ModelD>> for Event<C, D>
where
    C: From<ModelC>,
    D: From<ModelD>,
{
    fn from(value: ModelNewEvent<ModelC, ModelD>) -> Self {
        Self {
            id: None,
            sent: SendStatus::NotSent,
            _type: value._type,
            code: value.code,
            timestamp: value.timestamp.into(),
            message: value.message,
            msg: None,
            msg_en: None,
            project_id: value.project_id,
            group_id: value.group_id,
            has_server_time: value.has_server_time,
            contact_details: value.contact_details.map(|cd| cd.into()),
            comments: Default::default(),
            device_id: value.device_id,
            measuring_point_id: value
                .measuring_point_id
                .map(|s| ObjectId::parse_str(&s))
                .transpose()
                .ok()
                .flatten(),
            project_name: None,
            cluster_name: None,
            measuring_point_name: None,
            device_name: None,
            group_name: None,
            event_details: value.event_details.map(|ed| ed.into()),
        }
    }
}

impl<C, D, ModelC, ModelD> From<Event<C, D>> for ModelEvent<ModelC, ModelD>
where
    C: Into<ModelC>,
    D: Into<ModelD>,
{
    fn from(value: Event<C, D>) -> Self {
        Self {
            id: value.id.map(|v| v.to_hex()).unwrap_or_default(),
            _type: value._type,
            code: value.code,
            timestamp: value.timestamp.into(),
            sent: Some(value.sent),
            message: value.message,
            msg: value.msg,
            msg_en: value.msg_en,
            project_id: value.project_id,
            group_id: value.group_id,
            has_server_time: value.has_server_time,
            contact_details: value.contact_details.map(|cd| cd.into()),
            device_id: value.device_id,
            measuring_point_id: value.measuring_point_id.map(|id| id.to_hex()),
            project_name: value.project_name,
            cluster_name: value.cluster_name,
            measuring_point_name: value.measuring_point_name,
            device_name: value.device_name,
            group_name: value.group_name,
            comments: value.comments,
            event_details: value.event_details.map(|ed| ed.into()),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EventsWithStats<C, D> {
    pub events: Vec<Event<C, D>>,
    pub code_stats: Vec<EventStats>,
    pub type_stats: Vec<EventStats>,
    pub hour_stats: Vec<ProjectHourStats>,
}

impl<C, D, ModelC, ModelD> From<EventsWithStats<C, D>> for ModelEventsWithStats<ModelC, ModelD>
where
    C: Into<ModelC>,
    D: Into<ModelD>,
{
    fn from(value: EventsWithStats<C, D>) -> Self {
        Self {
            events: value.events.into_iter().map(Into::into).collect(),
            code_stats: value.code_stats,
            type_stats: value.type_stats,
            hour_stats: value.hour_stats,
        }
    }
}

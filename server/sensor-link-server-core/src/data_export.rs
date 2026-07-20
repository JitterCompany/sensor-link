use serde::{Deserialize, Serialize};

use crate::{data_kind::DataKind, DataStoreId, TimeRange};

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub enum DataExportStatus {
    InProgress,
    Success,
    Error,
}

#[derive(Deserialize, Serialize)]
#[serde(bound(
    serialize = "DT: serde::Serialize",
    deserialize = "DT: serde::de::DeserializeOwned"
))]
pub struct DataExport<DT: DataKind> {
    #[serde(rename = "_id")]
    pub id: Option<DataStoreId>,
    pub measuring_point_id: DataStoreId,
    pub data_type: DT,
    pub project_id: DataStoreId,
    pub time_range: TimeRange,
    pub file_id: Option<DataStoreId>,
    pub file_size: Option<u64>,
    pub status: DataExportStatus,
    pub trigger_id: Option<i64>,
}

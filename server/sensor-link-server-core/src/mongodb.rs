pub mod schema;

use std::{
    collections::{BTreeSet, HashMap},
    future::IntoFuture,
    time::Duration,
};

use async_trait::async_trait;
use bson::{doc, oid::ObjectId, to_bson, to_document, Binary, Bson, DateTime, Document};
use chrono::{NaiveDate, Utc};
use futures::{AsyncWriteExt, StreamExt, TryFutureExt, TryStreamExt};
use itertools::Itertools;
use mongodb::{
    options::{CountOptions, GridFsBucketOptions},
    Client, ClientSession, Collection, Database,
};
use serde::{de::DeserializeOwned, Serialize};
use strum::IntoEnumIterator;
use tokio::sync::mpsc;

use crate::{
    data_export::{DataExport, DataExportStatus},
    data_kind::DataKind,
    data_set::{DataSet, NewDataSet},
    device::{Device, DeviceExt, DeviceFieldType, DeviceQuery, DeviceStatusLike},
    event::{EventQuery, EventType, EventsWithStats, NewEvent, SendStatus},
    firmware::{Firmware, NewFirmware},
    materialized_views::MatViewMsg,
    sensor_data::{MPSensorData, MPSensorDataChannels, SensorData, TimeResolution},
    sensor_server_log::SensorServerLog,
    store_traits::{
        DataStoreError, DeviceStore, EventStore, FirmwareStore, Result, SensorDataOptions,
        SensorDataStore, TransactionDataStore,
    },
    utils::datetime::datetime_from_millis,
    DataStoreId, MeteorId, TimeRange,
};

use schema::{
    DATA_EXPORT_COLL_NAME, DATA_SET_COLL_NAME, DEVICE_COLL_NAME, EVENTS_COLL_NAME,
    EVENT_DESCRIPTORS_COLL_NAME, FIRMWARE_COLL_NAME, MEAS_POINT_COLL_NAME,
    MEAS_POINT_SHADOW_COLL_NAME, S2D_EVENTS_COLL_NAME, SENSOR_SERVER_LOG_COLL_NAME,
    TRACE_EVENT_COLL_NAME,
};

// ── SensorChannelStorage ──────────────────────────────────────────────────────

const DEFAULT_DATA_COLL_NAME_SUFFIX: &str = "";
const SEC_DATA_COLL_NAME_SUFFIX: &str = ".sec";
const MIN_DATA_COLL_NAME_SUFFIX: &str = ".min";
const HOUR_DATA_COLL_NAME_SUFFIX: &str = ".hour";

fn coll_name_suffix(resolution: TimeResolution) -> &'static str {
    match resolution {
        TimeResolution::Native => DEFAULT_DATA_COLL_NAME_SUFFIX,
        TimeResolution::Seconds => SEC_DATA_COLL_NAME_SUFFIX,
        TimeResolution::Minutes => MIN_DATA_COLL_NAME_SUFFIX,
        TimeResolution::Hours => HOUR_DATA_COLL_NAME_SUFFIX,
    }
}

/// Wrapper for multi resolution storage of sensor data
pub struct SensorChannelStorage<DC: DataKind> {
    channel: DC,
    resolution: TimeResolution,
}

impl<DC: DataKind> SensorChannelStorage<DC> {
    pub fn new(channel: DC) -> Self {
        Self {
            channel,
            resolution: TimeResolution::Native,
        }
    }

    pub fn new_with_range(channel: DC, range: &TimeRange) -> Self {
        let resolution = match range.duration() {
            d if d < Duration::from_secs(18) => TimeResolution::Native,
            d if d < Duration::from_secs(1800) => TimeResolution::Seconds,
            d if d < Duration::from_secs(1800 * 60) => TimeResolution::Minutes,
            _ => TimeResolution::Hours,
        };
        Self {
            channel,
            resolution,
        }
    }

    pub fn new_with_resolution(channel: DC, resolution: TimeResolution) -> Self {
        Self {
            channel,
            resolution,
        }
    }

    pub fn base_col_name(&self) -> String {
        self.channel.to_string()
    }

    pub fn collection_name(&self) -> String {
        format!(
            "{}{}",
            self.base_col_name(),
            coll_name_suffix(self.resolution)
        )
    }

    pub fn collection_name_for_resolution(&self, resolution: TimeResolution) -> String {
        format!("{}{}", self.base_col_name(), coll_name_suffix(resolution))
    }

    pub fn collection_suffix(&self) -> &str {
        coll_name_suffix(self.resolution)
    }

    pub fn collection_resolution(&self) -> TimeResolution {
        self.resolution
    }

    /// Try to select a finer resolution collection.
    /// Returns false if we're already at the highest res.
    pub fn select_finer_resolution(&mut self) -> bool {
        match self.resolution.finer_resolution() {
            Some(res) => {
                self.resolution = res;
                true
            }
            None => false,
        }
    }
}

// ── MongoDb ───────────────────────────────────────────────────────────────────

pub struct MongoDb<DC: DataKind, C, D, DT: DataKind, T, DevSt> {
    db_name: String,
    pub(crate) client: Client,
    pub tx_to_mat_view_task: mpsc::Sender<MatViewMsg<DC>>,
    _phantom: std::marker::PhantomData<(C, D, DT, T, DevSt)>,
}

impl<DC: DataKind, C, D, DT: DataKind, T, DevSt> Clone for MongoDb<DC, C, D, DT, T, DevSt> {
    fn clone(&self) -> Self {
        MongoDb {
            db_name: self.db_name.clone(),
            client: self.client.clone(),
            tx_to_mat_view_task: self.tx_to_mat_view_task.clone(),
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<DC: DataKind, C, D, DT: DataKind, T, DevSt> MongoDb<DC, C, D, DT, T, DevSt> {
    pub fn new(
        db_name: String,
        client: Client,
        tx_to_mat_view_task: mpsc::Sender<MatViewMsg<DC>>,
    ) -> Self {
        Self {
            db_name,
            client,
            tx_to_mat_view_task,
            _phantom: std::marker::PhantomData,
        }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn db_name(&self) -> &str {
        &self.db_name
    }

    pub fn db(&self) -> Database {
        self.client.database(&self.db_name)
    }

    pub fn collection<Doc: Send + Sync>(&self, coll_name: &str) -> Collection<Doc> {
        self.db().collection(coll_name)
    }

    pub async fn data_set_ids_for_measuring_points(
        &self,
        mp_ids: &[ObjectId],
    ) -> Result<Vec<ObjectId>> {
        Ok(self
            .collection::<schema::DataSet>(DATA_SET_COLL_NAME)
            .find(doc! { "measuring_point_id": { "$in": mp_ids } })
            .await?
            .try_collect::<Vec<_>>()
            .await?
            .into_iter()
            .map(|ds| ds.id)
            .collect())
    }
}

impl<DC: DataKind, C, D, DT: DataKind, T, DevSt> MongoDb<DC, C, D, DT, T, DevSt>
where
    DC: DeserializeOwned,
{
    async fn find_latest_timestamp_for_dataset_and_resolution(
        &self,
        data_channel: DC,
        data_set_id: ObjectId,
        res: TimeResolution,
    ) -> anyhow::Result<Option<i64>> {
        let collection_name =
            SensorChannelStorage::new_with_resolution(data_channel, res).collection_name();
        let collection: Collection<schema::SensorData<DC>> = self.collection(&collection_name);

        let datapoint = collection
            .find_one(doc! { "metadata.data_set_id": data_set_id })
            .sort(doc! { "time": -1 })
            .await?;

        Ok(datapoint.map(|d| d.time.timestamp_millis()))
    }

    async fn build_materialized_view_average(
        &self,
        source_col: &Collection<schema::SensorData<DC>>,
        dest_col: &str,
        match_doc: Document,
        date_group_str: &str,
    ) -> anyhow::Result<()> {
        source_col
            .aggregate(vec![
                match_doc,
                doc! { "$sort": { "perc": -1, "max": -1, "value": -1 } },
                doc! { "$group": {
                    "_id": {
                        "time": { "$dateToString": { "format": date_group_str, "date": "$time" }},
                        "metadata": "$metadata"
                    },
                    "value": { "$avg": "$value" },
                    "min": { "$min": { "$ifNull": ["$min", "$value"] } },
                    "max": { "$first": { "$ifNull": ["$max", "$value"] } },
                    "perc": { "$first": { "$ifNull": ["$perc", 0 ] } },
                    "freq": { "$first": { "$ifNull": ["$freq", 0 ] } },
                }},
                doc! { "$project": {
                    "time": { "$dateFromString": {
                        "dateString": "$_id.time",
                        "format": date_group_str
                    }},
                    "metadata": "$_id.metadata",
                    "value": 1,
                    "min": 1,
                    "max": 1,
                    "perc": 1,
                    "freq": 1,
                    "_id": 1,
                }},
                doc! { "$merge": { "into": dest_col, "whenMatched": "replace" } },
            ])
            .max_time(Duration::from_secs(60 * 1000))
            .await?;
        Ok(())
    }

    async fn get_sensor_data(
        &self,
        collection_name: String,
        collection_resolution: TimeResolution,
        meas_point_id: &ObjectId,
        data_set_ids: &[ObjectId],
        timerange: &TimeRange,
        inclusive_range: bool,
        limit: u64,
        sort: Option<i32>,
    ) -> Result<MPSensorData> {
        let query_count = self
            .collection::<schema::SensorData<DC>>(&collection_name)
            .count_documents(doc! {
                "metadata.data_set_id": { "$in": data_set_ids },
                "time": if inclusive_range {
                    doc! { "$gte": timerange.from, "$lte": timerange.until }
                } else {
                    doc! { "$gte": timerange.from, "$lt": timerange.until }
                },
            })
            .await
            .map_err(|err| DataStoreError::DatabaseReadFailed(err.to_string()))?;
        if query_count > limit {
            return Err(DataStoreError::TooMuchData);
        }
        let cursor = self
            .collection::<schema::SensorData<DC>>(&collection_name)
            .find(doc! {
                "metadata.data_set_id": { "$in": data_set_ids },
                "time": if inclusive_range {
                    doc! { "$gte": timerange.from, "$lte": timerange.until }
                } else {
                    doc! { "$gte": timerange.from, "$lt": timerange.until }
                },
            })
            .sort(match sort {
                Some(sort) => doc! { "time": sort },
                None => doc! { "time": 1 },
            })
            .limit(limit as i64)
            .await
            .map_err(|err| DataStoreError::DatabaseReadFailed(err.to_string()))?;

        let sensor_data: Vec<schema::SensorData<DC>> = cursor
            .try_collect()
            .await
            .map_err(|err| DataStoreError::DatabaseReadFailed(err.to_string()))?;

        let mut result = MPSensorData {
            measuring_point_id: meas_point_id.to_hex(),
            time: Vec::with_capacity(sensor_data.len()),
            values: Vec::with_capacity(sensor_data.len()),
            min: Vec::with_capacity(sensor_data.len()),
            max: Vec::with_capacity(sensor_data.len()),
            freq: Some(Vec::with_capacity(sensor_data.len())),
            perc: Some(Vec::with_capacity(sensor_data.len())),
            seconds_per_sample: collection_resolution.seconds_per_sample(),
        };

        for doc in sensor_data {
            result.add_sample(
                doc.time.timestamp_millis(),
                doc.value,
                doc.min.unwrap_or(doc.value),
                doc.max.unwrap_or(doc.value),
                doc.freq,
                doc.perc,
            );
        }

        Ok(result)
    }

    async fn find_datapoint_for_mp_sorted(
        &self,
        data_channel: DC,
        meas_point_id: &DataStoreId,
        sort: i32,
        until: Option<chrono::DateTime<Utc>>,
    ) -> Result<Option<(i64, f32)>> {
        let collection: Collection<schema::SensorData<DC>> =
            self.collection(&data_channel.to_string());
        let id = ObjectId::parse_str(meas_point_id)?;
        let data_set_ids = self.data_set_ids_for_measuring_points(&[id]).await?;
        if data_set_ids.is_empty() {
            return Ok(None);
        }
        for data_set_id in data_set_ids
            .iter()
            .sorted_by_key(|ds| ds.timestamp().timestamp_millis() * sort as i64)
        {
            let mut filter = doc! { "metadata.data_set_id": data_set_id };
            if let Some(until) = until {
                filter.insert("time", doc! { "$lte": until });
            }
            let datapoint = collection
                .find_one(filter)
                .sort(doc! { "time": sort })
                .await?;
            if let Some(datapoint) = datapoint {
                return Ok(Some((datapoint.time.timestamp_millis(), datapoint.value)));
            }
        }
        Ok(None)
    }
}

impl<DC: DataKind, C, D, DT: DataKind, T, DevSt> MongoDb<DC, C, D, DT, T, DevSt> {
    async fn sensor_data_for_measuring_point_query(
        &self,
        meas_point_id: ObjectId,
        timerange: &TimeRange,
    ) -> anyhow::Result<(Vec<ObjectId>, Document)> {
        let data_set_ids = self
            .data_set_ids_for_measuring_points(&[meas_point_id])
            .await?;
        let findquery = doc! {
            "metadata.data_set_id": { "$in": &data_set_ids },
            "time": { "$gte": timerange.from, "$lte": timerange.until }
        };
        Ok((data_set_ids, findquery))
    }

    async fn set_device_field_for_query(
        &self,
        query_doc: Document,
        value: DeviceFieldType<T, DevSt>,
        session: Option<&mut ClientSession>,
    ) -> Result<()>
    where
        T: Serialize,
        DevSt: DeviceStatusLike,
    {
        let collection = self.collection::<Document>(DEVICE_COLL_NAME);
        let field_name = value.field_name();
        let mut query = collection.update_many(
            query_doc,
            doc! { "$set": { field_name: bson::Bson::try_from(value)? } },
        );
        if let Some(session) = session {
            query = query.session(session);
        }
        query.await?;
        Ok(())
    }

    async fn insert_event_impl(
        &self,
        event: NewEvent<C, D>,
        session: Option<&mut ClientSession>,
    ) -> Result<DataStoreId>
    where
        C: Send + Sync + Serialize,
        D: Send + Sync + Serialize,
    {
        let event: schema::Event<C, D> = event.into();
        let collection = self.collection::<schema::Event<C, D>>(EVENTS_COLL_NAME);
        let mut query = collection.insert_one(event);
        if let Some(session) = session {
            query = query.session(session);
        }
        let res = query.await?;
        res.inserted_id
            .as_object_id()
            .map(|obid| obid.to_string())
            .ok_or(DataStoreError::Database(
                "Failed to insert event or invalid ID".to_string(),
            ))
    }
}

// ── TryFrom<DeviceFieldType> for Bson ─────────────────────────────────────────

impl<DT: Serialize, DS: DeviceStatusLike> TryFrom<DeviceFieldType<DT, DS>> for Bson {
    type Error = DataStoreError;

    fn try_from(field_type: DeviceFieldType<DT, DS>) -> Result<Self> {
        match field_type {
            DeviceFieldType::DeviceType(v) => to_bson(&v),
            DeviceFieldType::GroupId(v) => to_bson(&v),
            DeviceFieldType::Name(v) => to_bson(&v),
            DeviceFieldType::LastContact(v) => to_bson(&v),
            DeviceFieldType::Version(v) => to_bson(&v),
            DeviceFieldType::MarkedForUpdate(v) => to_bson(&v),
            DeviceFieldType::Command(v) => to_bson(&v),
            DeviceFieldType::SyncIntervalMin(v) => to_bson(&v),
            DeviceFieldType::WaitingForNewMp(v) => to_bson(&v),
            DeviceFieldType::ConfigConfirmed(v) => to_bson(&v),
            DeviceFieldType::ConfigLastSentAt(v) => to_bson(&v),
            DeviceFieldType::CalibrationDate(v) => to_bson(&v),
            DeviceFieldType::Documents(v) => to_bson(&v),
            DeviceFieldType::DeviceStatus(v) => to_bson(&v),
            DeviceFieldType::StatusSince(v) => to_bson(&v),
            DeviceFieldType::RegisterTime(v) => to_bson(&v),
            DeviceFieldType::BaselineValues(v) => to_bson(&v),
            DeviceFieldType::BaselineDate(v) => to_bson(&v),
            DeviceFieldType::HubId(v) => to_bson(&v),
            DeviceFieldType::SimIccid(v) => to_bson(&v),
            DeviceFieldType::LicenseStart(v) => to_bson(&v),
            DeviceFieldType::OnlineSince(v) => to_bson(&v),
        }
        .map_err(|err| err.into())
    }
}

impl From<mongodb::error::Error> for DataStoreError {
    fn from(error: mongodb::error::Error) -> Self {
        DataStoreError::Database(error.to_string())
    }
}

impl From<mongodb::bson::ser::Error> for DataStoreError {
    fn from(error: mongodb::bson::ser::Error) -> Self {
        DataStoreError::Database(error.to_string())
    }
}

impl From<bson::oid::Error> for DataStoreError {
    fn from(error: bson::oid::Error) -> Self {
        DataStoreError::InvalidDatabaseId(error.to_string())
    }
}

// ── MongoTransaction ──────────────────────────────────────────────────────────

struct MongoTransaction<DC: DataKind, C, D, DT: DataKind, T, DevSt> {
    mongo: MongoDb<DC, C, D, DT, T, DevSt>,
    session: ClientSession,
}

#[async_trait]
impl<DC: DataKind, C, D, DT: DataKind, T, DevSt> TransactionDataStore
    for MongoTransaction<DC, C, D, DT, T, DevSt>
where
    C: Serialize + DeserializeOwned + Send + Sync + 'static,
    D: Serialize + DeserializeOwned + Send + Sync + 'static,
    T: Serialize + Send + Sync + 'static,
    DevSt: DeviceStatusLike,
{
    type ContactData = C;
    type EventData = D;
    type DeviceType = T;
    type DeviceStatus = DevSt;

    async fn set_device_field(
        &mut self,
        device_id: &str,
        value: DeviceFieldType<T, DevSt>,
    ) -> Result<()> {
        self.mongo
            .set_device_field_for_query(doc! { "_id": device_id }, value, Some(&mut self.session))
            .await
    }

    async fn set_device_field_for_groups(
        &mut self,
        device_id: &str,
        group_ids: Vec<String>,
        value: DeviceFieldType<T, DevSt>,
    ) -> Result<()> {
        let query_doc = doc! { "_id": device_id, "group_id": { "$in": group_ids } };
        self.mongo
            .set_device_field_for_query(query_doc, value, Some(&mut self.session))
            .await
    }

    async fn insert_event(&mut self, event: NewEvent<C, D>) -> Result<DataStoreId> {
        self.mongo
            .insert_event_impl(event, Some(&mut self.session))
            .await
    }

    async fn commit(&mut self) -> Result<()> {
        self.session.commit_transaction().await?;
        Ok(())
    }
}

// ── impl DeviceStore ──────────────────────────────────────────────────────────

#[async_trait]
impl<DC: DataKind, C, D, DT: DataKind, T, DevSt> DeviceStore for MongoDb<DC, C, D, DT, T, DevSt>
where
    C: Serialize + DeserializeOwned + Send + Sync + 'static,
    D: Serialize + DeserializeOwned + Send + Sync + 'static,
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    DevSt: DeviceStatusLike,
{
    type TxContactData = C;
    type TxEventData = D;
    type DeviceType = T;
    type DeviceStatus = DevSt;

    async fn start_transaction(
        &self,
    ) -> Result<
        Box<
            dyn TransactionDataStore<
                ContactData = Self::TxContactData,
                EventData = Self::TxEventData,
                DeviceType = Self::DeviceType,
                DeviceStatus = Self::DeviceStatus,
            >,
        >,
    > {
        let mut session = self.client.start_session().await?;
        session.start_transaction().await?;
        Ok(Box::new(MongoTransaction {
            mongo: self.clone(),
            session,
        }))
    }

    async fn get_devices(&self, query: DeviceQuery) -> Result<Vec<DeviceExt<T, DevSt>>> {
        let mut filter = to_document(&query)?;
        filter.remove("project_ids");
        filter.remove("cluster_id");
        if let Some(sensor_ids) = query.sensor_ids {
            filter.remove("sensor_ids");
            filter.insert("_id", doc! { "$in": sensor_ids });
        }
        if let Some(device_types) = query.device_types {
            filter.remove("device_types");
            filter.insert("device_type", doc! { "$in": device_types });
        }
        self.collection::<Device<T, DevSt>>(DEVICE_COLL_NAME)
            .aggregate(
                vec![
                    doc! { "$match": filter },
                    doc! { "$lookup": {
                        "from": MEAS_POINT_COLL_NAME,
                        "localField": "_id",
                        "foreignField": "sensor_id",
                        "as": "measuring_point"
                    }},
                    doc! { "$unwind": { "path": "$measuring_point", "preserveNullAndEmptyArrays": true } },
                    doc! { "$lookup": {
                        "from": MEAS_POINT_SHADOW_COLL_NAME,
                        "localField": "_id",
                        "foreignField": "sensor_id",
                        "as": "measuring_point_shadow"
                    }},
                    doc! { "$unwind": { "path": "$measuring_point_shadow", "preserveNullAndEmptyArrays": true } },
                    doc! { "$lookup": {
                        "from": DEVICE_COLL_NAME,
                        "localField": "hub_id",
                        "foreignField": "hub_id",
                        "as": "hub_meas_point",
                        "pipeline": [
                            doc! { "$match": { "hub_id": { "$exists": true } } },
                            doc! { "$lookup": {
                                "from": MEAS_POINT_COLL_NAME,
                                "localField": "_id",
                                "foreignField": "sensor_id",
                                "as": "meas_point"
                            } },
                            doc! { "$replaceRoot": { "newRoot": { "$mergeObjects": "$meas_point" } } },
                        ],
                    }},
                    doc! { "$addFields": { "hub_meas_point": { "$mergeObjects": "$hub_meas_point" } } },
                    doc! { "$addFields": { "cluster_id": { "$ifNull": [
                        { "$toString": "$hub_meas_point.cluster_id" },
                        { "$toString": "$measuring_point.cluster_id" },
                        "$measuring_point_shadow.cluster_id"
                    ] } } },
                    doc! { "$project": { "hub_meas_point": 0 } },
                    doc! { "$lookup": {
                        "from": DEVICE_COLL_NAME,
                        "localField": "_id",
                        "foreignField": "hub_id",
                        "as": "hub_meas_point",
                        "pipeline": [
                            doc! { "$match": { "hub_id": { "$exists": true } } },
                            doc! { "$lookup": {
                                "from": MEAS_POINT_COLL_NAME,
                                "localField": "_id",
                                "foreignField": "sensor_id",
                                "as": "meas_point"
                            } },
                            doc! { "$replaceRoot": { "newRoot": { "$mergeObjects": "$meas_point" } } },
                        ],
                    }},
                    doc! { "$addFields": { "hub_meas_point": { "$mergeObjects": "$hub_meas_point" } } },
                    doc! { "$addFields": { "hub_project_id": "$hub_meas_point.project_id" } },
                    doc! { "$lookup": {
                        "from": crate::mongodb::schema::S2D_CONFIGURATION_COLL_NAME,
                        "localField": "measuring_point.configuration_id",
                        "foreignField": "_id",
                        "as": "configuration"
                    }},
                    doc! { "$unwind": { "path": "$configuration", "preserveNullAndEmptyArrays": true } },
                    doc! { "$addFields": {
                        "mp_id": { "$ifNull": [{ "$toString": "$measuring_point._id" }, "$measuring_point_shadow._id"] },
                        "mp_name": { "$ifNull": ["$measuring_point.name", "$measuring_point_shadow.name"] },
                        "project_id": { "$ifNull": ["$measuring_point.project_id", "$measuring_point_shadow.project_id", "$hub_project_id"] },
                        "active_start": { "$ifNull": ["$measuring_point.active_start", "$measuring_point_shadow.active_start"] },
                        "active_end": { "$ifNull": ["$measuring_point.active_end", "$measuring_point_shadow.active_end"] },
                        "location": { "$ifNull": ["$measuring_point.location", "$measuring_point_shadow.location"] },
                        "monitoring_enabled": { "$ifNull": ["$measuring_point.monitoring_enabled", "$measuring_point_shadow.monitoring_enabled"] },
                        "registration_enabled": { "$ifNull": ["$configuration.registration_enabled", false] },
                    }},
                    doc! { "$project": {
                        "measuring_point": 0,
                        "measuring_point_shadow": 0,
                        "hub_meas_point": 0,
                        "hub_project_id": 0,
                    }},
                    // Join cluster (V3) or cluster shadow (V2) and project so status
                    // views don't need to fetch full project structures separately.
                    doc! { "$lookup": {
                        "from": schema::CLUSTER_COLL_NAME,
                        "let": { "cid": "$cluster_id" },
                        "pipeline": [
                            doc! { "$match": { "$expr": { "$eq": [ { "$toString": "$_id" }, "$$cid" ] } } },
                        ],
                        "as": "cluster"
                    }},
                    doc! { "$unwind": { "path": "$cluster", "preserveNullAndEmptyArrays": true } },
                    doc! { "$lookup": {
                        "from": schema::CLUSTER_SHADOW_COLL_NAME,
                        "localField": "cluster_id",
                        "foreignField": "_id",
                        "as": "cluster_shadow"
                    }},
                    doc! { "$unwind": { "path": "$cluster_shadow", "preserveNullAndEmptyArrays": true } },
                    doc! { "$lookup": {
                        "from": schema::PROJECT_COLL_NAME,
                        "localField": "project_id",
                        "foreignField": "project_id",
                        "as": "project"
                    }},
                    doc! { "$unwind": { "path": "$project", "preserveNullAndEmptyArrays": true } },
                    doc! { "$addFields": {
                        "project_name": "$project.name",
                        "cluster_sync_interval_min": { "$ifNull": [
                            "$cluster.operationsConfig.sync_interval_min",
                            "$cluster_shadow.operationsConfig.sync_interval_min"
                        ] },
                        "cluster_calibration": { "$ifNull": [
                            "$cluster.operationsConfig.calibration",
                            "$cluster_shadow.operationsConfig.calibration"
                        ] },
                        "cluster_scheduled_command": { "$ifNull": [
                            "$cluster.scheduledCommand",
                            "$cluster_shadow.scheduledCommand"
                        ] },
                    }},
                    doc! { "$project": {
                        "cluster": 0,
                        "cluster_shadow": 0,
                        "project": 0,
                    }},
                    if let Some(project_ids) = query.project_ids {
                        doc! { "$match": { "project_id": { "$in": project_ids } } }
                    } else {
                        doc! { "$skip": 0 }
                    },
                    if let Some(cluster_id) = query.cluster_id {
                        doc! { "$match": { "cluster_id": cluster_id } }
                    } else {
                        doc! { "$skip": 0 }
                    },
                ],
            )
            .await?
            .map(|result| {
                result.and_then(|doc| {
                    bson::from_bson::<DeviceExt<T, DevSt>>(bson::Bson::Document(doc))
                        .map_err(Into::into)
                })
            })
            .try_collect::<Vec<_>>()
            .await
            .map_err(Into::into)
    }

    async fn get_devices_simple(&self, query: DeviceQuery) -> Result<Vec<Device<T, DevSt>>> {
        let mut filter = to_document(&query)?;
        filter.remove("project_ids");
        filter.remove("cluster_id");
        if let Some(sensor_ids) = query.sensor_ids {
            filter.remove("sensor_ids");
            filter.insert("_id", doc! { "$in": sensor_ids });
        }
        if let Some(device_types) = query.device_types {
            filter.remove("device_types");
            filter.insert("device_type", doc! { "$in": device_types });
        }
        self.collection::<Device<T, DevSt>>(DEVICE_COLL_NAME)
            .find(filter)
            .await?
            .try_collect::<Vec<_>>()
            .await
            .map_err(Into::into)
    }

    async fn device_by_id(&self, id: &str) -> Result<Option<Device<T, DevSt>>> {
        Ok(self
            .collection::<Device<T, DevSt>>(DEVICE_COLL_NAME)
            .find_one(doc! { "_id": id })
            .max_time(Duration::from_secs(10))
            .await?)
    }

    async fn get_device_ids_for_hub(&self, hub_id: &DataStoreId) -> Result<Vec<DataStoreId>> {
        Ok(self
            .collection::<Device<T, DevSt>>(DEVICE_COLL_NAME)
            .find(doc! { "hub_id": hub_id })
            .await?
            .try_collect::<Vec<_>>()
            .await?
            .into_iter()
            .map(|d| d.id)
            .collect())
    }

    async fn remove_device(&self, device_id: &DataStoreId) -> Result<()> {
        self.collection::<Device<T, DevSt>>(DEVICE_COLL_NAME)
            .delete_one(doc! { "_id": device_id })
            .await?;
        Ok(())
    }

    async fn upsert_devices_for_datatype(
        &self,
        device_type: Self::DeviceType,
        device_ids: &[String],
    ) -> Result<Vec<String>> {
        let device_collection: Collection<Document> = self.collection(DEVICE_COLL_NAME);
        let mut new_device_ids: Vec<String> = Vec::new();

        for device_id in device_ids.iter() {
            match device_collection
                .update_one(
                    doc! { "_id": device_id },
                    doc! { "$setOnInsert": { "device_type": to_bson(&device_type)? } },
                )
                .upsert(true)
                .await
            {
                Ok(update_result) => {
                    if let Some(new_device_id) = update_result
                        .upserted_id
                        .as_ref()
                        .and_then(|id| id.as_str())
                    {
                        new_device_ids.push(new_device_id.to_string());
                    }
                }
                Err(err) => tracing::error!("Error inserting device {device_id}: {err}"),
            }
        }
        Ok(new_device_ids)
    }

    async fn set_device_field(
        &self,
        device_id: &str,
        value: DeviceFieldType<T, DevSt>,
    ) -> Result<()> {
        self.set_device_field_for_query(doc! { "_id": device_id }, value, None)
            .await
    }

    async fn set_device_field_for_groups(
        &self,
        device_id: &str,
        group_ids: Vec<String>,
        value: DeviceFieldType<T, DevSt>,
    ) -> Result<()> {
        let query_doc = doc! { "_id": device_id, "group_id": { "$in": group_ids } };
        self.set_device_field_for_query(query_doc, value, None)
            .await
    }

    async fn set_field_for_devices(
        &self,
        device_ids: &[String],
        value: DeviceFieldType<T, DevSt>,
    ) -> Result<()> {
        let query_doc = doc! { "_id": { "$in": device_ids } };
        self.set_device_field_for_query(query_doc, value, None)
            .await
    }

    async fn insert_sensor_server_log(&self, log: SensorServerLog) -> anyhow::Result<()> {
        let log: schema::SensorServerLog = log.into();
        self.collection::<schema::SensorServerLog>(SENSOR_SERVER_LOG_COLL_NAME)
            .insert_one(log)
            .await?;
        Ok(())
    }

    async fn get_sensor_server_log(
        &self,
        device_id: &DataStoreId,
        limit: u32,
    ) -> Result<Vec<SensorServerLog>> {
        Ok(self
            .collection::<schema::SensorServerLog>(SENSOR_SERVER_LOG_COLL_NAME)
            .find(doc! { "sensorID": device_id })
            .sort(doc! { "timestamp": -1 })
            .limit(limit.into())
            .await?
            .try_collect::<Vec<_>>()
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    async fn unlink_sensor_from_measuring_points(&self, sensor_id: &DataStoreId) -> Result<()> {
        self.collection::<bson::Document>(MEAS_POINT_COLL_NAME)
            .update_many(
                doc! { "sensor_id": sensor_id },
                doc! { "$unset": { "sensor_id": "" } },
            )
            .await?;
        Ok(())
    }

    async fn unlink_sensor_from_measuring_point_shadows(
        &self,
        sensor_id: &DataStoreId,
    ) -> Result<()> {
        self.collection::<bson::Document>(MEAS_POINT_SHADOW_COLL_NAME)
            .update_many(
                doc! { "sensor_id": sensor_id },
                doc! { "$unset": { "sensor_id": "" } },
            )
            .await?;
        Ok(())
    }
}

// ── impl FirmwareStore ────────────────────────────────────────────────────────

#[async_trait]
impl<DC: DataKind, C, D, DT: DataKind, T, DevSt> FirmwareStore for MongoDb<DC, C, D, DT, T, DevSt>
where
    C: Send + Sync + 'static,
    D: Send + Sync + 'static,
    T: Send + Sync + 'static + Serialize + DeserializeOwned,
    DevSt: Send + Sync + 'static,
{
    type DeviceType = T;

    async fn insert_firmware(&self, firmware: NewFirmware<Self::DeviceType>) -> Result<()> {
        let firmware: schema::NewFirmware<Self::DeviceType> = firmware.try_into()?;
        self.collection::<schema::NewFirmware<Self::DeviceType>>(FIRMWARE_COLL_NAME)
            .insert_one(firmware)
            .await?;
        Ok(())
    }

    async fn get_firmwares(
        &self,
        device_type: &Self::DeviceType,
    ) -> Result<Vec<Firmware<Self::DeviceType>>> {
        self.collection::<schema::Firmware<Self::DeviceType>>(FIRMWARE_COLL_NAME)
            .find(doc! { "device_type": to_bson(device_type)? })
            .await?
            .try_collect::<Vec<_>>()
            .await
            .map(|firmwares| firmwares.into_iter().map(Into::into).collect())
            .map_err(Into::into)
    }

    async fn firmware_by_id(&self, id: &DataStoreId) -> Result<Option<Firmware<Self::DeviceType>>> {
        let id = ObjectId::parse_str(id)?;
        Ok(self
            .collection::<schema::Firmware<Self::DeviceType>>(FIRMWARE_COLL_NAME)
            .find_one(doc! { "_id": id })
            .await?
            .map(Into::into))
    }

    async fn remove_firmware(&self, id: &DataStoreId) -> Result<()> {
        let id = ObjectId::parse_str(id)?;
        self.collection::<schema::Firmware<Self::DeviceType>>(FIRMWARE_COLL_NAME)
            .delete_one(doc! { "_id": id })
            .await?;
        Ok(())
    }

    async fn recommend_firmware(&self, id: &DataStoreId) -> Result<()> {
        let id = ObjectId::parse_str(id)?;
        self.collection::<schema::Firmware<Self::DeviceType>>(FIRMWARE_COLL_NAME)
            .update_one(doc! { "_id": id }, doc! { "$set": { "recommended": true } })
            .await?;
        Ok(())
    }

    async fn unrecommend_other_firmwares_for_same_device_type(
        &self,
        id: &DataStoreId,
    ) -> Result<()> {
        let device_type = self
            .firmware_by_id(id)
            .await?
            .ok_or(DataStoreError::NotFound("firmware"))?
            .device_type;
        let id = ObjectId::parse_str(id)?;
        self.collection::<schema::Firmware<Self::DeviceType>>(FIRMWARE_COLL_NAME)
            .update_many(
                doc! { "_id": { "$ne": id }, "device_type": to_bson(&device_type)? },
                doc! { "$set": { "recommended": false } },
            )
            .await?;
        Ok(())
    }
}

// ── impl SensorDataStore ──────────────────────────────────────────────────────

#[async_trait]
impl<DC, C, D, DT, T, DevSt> SensorDataStore for MongoDb<DC, C, D, DT, T, DevSt>
where
    DC: DataKind + DeserializeOwned,
    DT: DataKind,
    C: Send + Sync + 'static,
    D: Send + Sync + 'static,
    T: Send + Sync + 'static,
    DevSt: Send + Sync + 'static,
{
    type DataChannel = DC;
    type DataType = DT;

    fn clone_dyn(&self) -> Box<dyn SensorDataStore<DataChannel = DC, DataType = DT>> {
        Box::new(self.clone())
    }

    async fn data_set_ids_for_measuring_point(
        &self,
        mp_id: &DataStoreId,
    ) -> Result<Vec<DataStoreId>> {
        let mp_id = ObjectId::parse_str(mp_id)?;
        Ok(self
            .data_set_ids_for_measuring_points(&[mp_id])
            .await?
            .into_iter()
            .map(|id| id.to_hex())
            .collect())
    }

    async fn insert_sensordata<'a>(
        &'a self,
        data_channel: DC,
        sensor_data: Vec<SensorData<DC>>,
    ) -> anyhow::Result<()> {
        let unique_dataset_ids: Vec<String> = sensor_data
            .iter()
            .filter_map(|s| s.metadata.data_set_id.clone())
            .unique()
            .collect();

        let sensor_data: Vec<schema::SensorData<DC>> =
            sensor_data.into_iter().map(Into::into).collect();

        // No time limit as this insert is the most important
        self.collection::<schema::SensorData<DC>>(&data_channel.to_string())
            .insert_many(sensor_data)
            .await?;

        if data_channel.downsampling() {
            for data_set_id in unique_dataset_ids {
                if let Err(err) = self
                    .tx_to_mat_view_task
                    .send(MatViewMsg {
                        data_channel,
                        data_set_id,
                    })
                    .await
                {
                    tracing::error!("tx_to_mat_view_task error: {err:?}");
                }
            }
        }
        Ok(())
    }

    async fn clear_materialized_views(
        &self,
        channels: &[DC],
        meas_point_id: &DataStoreId,
        from: chrono::DateTime<Utc>,
        res: TimeResolution,
    ) -> Result<()> {
        let meas_point_id = ObjectId::parse_str(meas_point_id)?;

        let data_set_ids = self
            .data_set_ids_for_measuring_points(&[meas_point_id])
            .await?;

        let match_doc = doc! {
            "metadata.data_set_id": { "$in": data_set_ids },
            "time": { "$gte": from }
        };

        for &channel in channels {
            let collection_name =
                SensorChannelStorage::new_with_resolution(channel, res).collection_name();
            let collection: Collection<Document> = self.collection(&collection_name);
            tracing::info!(
                "Deleting data for mp {meas_point_id} from {collection_name} from {from}"
            );
            collection.delete_many(match_doc.clone()).await?;
        }

        Ok(())
    }

    async fn update_materialized_views<'a>(
        &'a self,
        data_channel: DC,
        data_set_id: &'a str,
    ) -> anyhow::Result<()> {
        let data_set_id = ObjectId::parse_str(data_set_id)?;

        let from_resolution = TimeResolution::Seconds;
        let mut source_resolution = TimeResolution::Native;
        let mut resolution = match from_resolution {
            TimeResolution::Native => Some(TimeResolution::Seconds),
            _ => Some(from_resolution),
        };

        let channel_storage = SensorChannelStorage::new(data_channel);

        while let Some(dest_resolution) = resolution {
            let timestamp_ms = self
                .find_latest_timestamp_for_dataset_and_resolution(
                    data_channel,
                    data_set_id,
                    dest_resolution,
                )
                .await?
                .unwrap_or(0);
            let date = DateTime::from_millis(timestamp_ms);

            let match_doc = doc! {
                "$match": {
                    "metadata.data_set_id": data_set_id,
                    "time": { "$gte": date }
                }
            };

            let dest_coll_name = channel_storage.collection_name_for_resolution(dest_resolution);
            let source_coll_name =
                channel_storage.collection_name_for_resolution(source_resolution);
            let source_col: Collection<schema::SensorData<DC>> = self.collection(&source_coll_name);

            let date_group_str = match dest_resolution {
                TimeResolution::Native => {
                    return Err(anyhow::anyhow!("Invalid destination resolution"))
                }
                TimeResolution::Seconds => "%Y-%m-%dT%H:%M:%S",
                TimeResolution::Minutes => "%Y-%m-%dT%H:%M",
                TimeResolution::Hours => "%Y-%m-%dT%H",
            };

            self.build_materialized_view_average(
                &source_col,
                &dest_coll_name,
                match_doc.clone(),
                date_group_str,
            )
            .await?;

            source_resolution = dest_resolution;
            resolution = dest_resolution.coarser_resolution();
        }

        Ok(())
    }

    async fn find_first_timestamp_for_mp<'a>(
        &'a self,
        data_channel: DC,
        meas_point_id: &'a DataStoreId,
    ) -> Result<Option<i64>> {
        self.find_datapoint_for_mp_sorted(data_channel, meas_point_id, 1, None)
            .await
            .map(|opt| opt.map(|(ts, _)| ts))
    }

    async fn find_latest_timestamp_for_mp<'a>(
        &'a self,
        data_channel: DC,
        meas_point_id: &'a DataStoreId,
    ) -> Result<Option<i64>> {
        self.find_datapoint_for_mp_sorted(data_channel, meas_point_id, -1, None)
            .await
            .map(|opt| opt.map(|(ts, _)| ts))
    }

    async fn find_latest_datapoint_for_mp(
        &self,
        data_channel: DC,
        meas_point_id: &DataStoreId,
        until: chrono::DateTime<Utc>,
    ) -> Result<Option<(i64, f32)>> {
        self.find_datapoint_for_mp_sorted(data_channel, meas_point_id, -1, Some(until))
            .await
    }

    async fn count_sensor_data_for_measuring_point(
        &self,
        data_channel: DC,
        resolution: TimeResolution,
        meas_point_id: &DataStoreId,
        timerange: &TimeRange,
        limit: u64,
    ) -> anyhow::Result<u64> {
        let meas_point_id = ObjectId::parse_str(meas_point_id)?;

        let (_, findquery) = self
            .sensor_data_for_measuring_point_query(meas_point_id, timerange)
            .await?;

        let coll_name =
            SensorChannelStorage::new_with_resolution(data_channel, resolution).collection_name();
        self.collection::<schema::SensorData<DC>>(&coll_name)
            .count_documents(findquery)
            .max_time(Duration::from_secs(10))
            .limit(limit)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to count sensor data: {e}"))
    }

    async fn sensor_data_for_measuring_point(
        &self,
        data_channel: DC,
        resolution: Option<TimeResolution>,
        meas_point_id: &DataStoreId,
        timerange: &TimeRange,
        inclusive_range: bool,
    ) -> Result<MPSensorData> {
        self.sensor_data_for_measuring_point_with_options(
            data_channel,
            resolution,
            meas_point_id,
            timerange,
            inclusive_range,
            SensorDataOptions::default(),
        )
        .await
    }

    async fn sensor_data_for_measuring_point_with_options(
        &self,
        data_channel: DC,
        resolution: Option<TimeResolution>,
        meas_point_id: &DataStoreId,
        timerange: &TimeRange,
        inclusive_range: bool,
        options: SensorDataOptions,
    ) -> Result<MPSensorData> {
        let SensorDataOptions { sort, limit } = options;

        let meas_point_id = ObjectId::parse_str(meas_point_id)?;

        let (data_set_ids, findquery) = self
            .sensor_data_for_measuring_point_query(meas_point_id, timerange)
            .await
            .map_err(|e| DataStoreError::DatabaseReadFailed(e.to_string()))?;

        let col_set = if let Some(resolution) = resolution {
            SensorChannelStorage::new_with_resolution(data_channel, resolution)
        } else {
            let mut col_set = SensorChannelStorage::new_with_range(data_channel, timerange);

            let options = CountOptions::builder()
                .max_time(Duration::from_secs(30))
                .limit(1800)
                .build();

            loop {
                let coll_name = col_set.collection_name();

                match self
                    .collection::<schema::SensorData<DC>>(&coll_name)
                    .count_documents(findquery.clone())
                    .with_options(options.clone())
                    .await
                {
                    Ok(num_docs) => {
                        tracing::trace!("{coll_name}: num docs: {num_docs}");
                        if num_docs >= 1800 {
                            break;
                        } else if col_set.select_finer_resolution() {
                            continue;
                        } else {
                            break;
                        }
                    }
                    Err(err) => return Err(DataStoreError::DatabaseReadFailed(err.to_string())),
                }
            }

            col_set
        };

        tracing::debug!("Using collection {:?}", col_set.collection_name());

        self.get_sensor_data(
            col_set.collection_name(),
            col_set.collection_resolution(),
            &meas_point_id,
            &data_set_ids,
            timerange,
            inclusive_range,
            limit.map(|l| 180_000.min(l)).unwrap_or(180_000),
            sort,
        )
        .await
    }

    async fn get_highest_values(
        &self,
        data_channels: &[DC],
        meas_point_id: &DataStoreId,
        timerange: &TimeRange,
        nr_values: u32,
    ) -> Result<Vec<SensorData<DC>>> {
        let Some(first_data_channel) = data_channels.first() else {
            return Err(DataStoreError::NotFound("No data channels provided"));
        };
        let id = ObjectId::parse_str(meas_point_id)?;
        let data_set_ids = self.data_set_ids_for_measuring_points(&[id]).await?;
        let first_coll_name = first_data_channel.to_string();
        let collection = self.collection::<schema::SensorData<DC>>(&first_coll_name);
        let data_channel_query = vec![
            doc! { "$match": {
                "metadata.data_set_id": { "$in": data_set_ids },
                "time": { "$gte": timerange.from, "$lt": timerange.until }
            }},
            doc! { "$sort": { "perc": -1, "value": -1 } },
            doc! { "$limit": nr_values },
        ];

        let mut pipeline = data_channel_query.clone();
        pipeline.push(
            doc! { "$addFields": { "metadata.data_channel": first_data_channel.to_string() } },
        );
        for data_channel in data_channels.iter().skip(1) {
            let coll_name = data_channel.to_string();
            let data_channel_pipeline = [
                &data_channel_query[..],
                &[doc! { "$addFields": { "metadata.data_channel": coll_name.clone() } }],
            ]
            .concat();
            pipeline.push(doc! { "$unionWith": {
                "coll": coll_name,
                "pipeline": data_channel_pipeline
            }});
        }
        pipeline.push(doc! { "$sort": { "perc": -1, "value": -1 } });
        pipeline.push(doc! { "$limit": nr_values });

        let cursor = collection.aggregate(pipeline).await?;
        let sensor_data: Vec<SensorData<DC>> = cursor
            .map(|result| {
                result.and_then(|doc| {
                    bson::from_bson::<schema::SensorData<DC>>(bson::Bson::Document(doc))
                        .map(Into::into)
                        .map_err(Into::into)
                })
            })
            .try_collect()
            .await?;

        Ok(sensor_data)
    }

    async fn active_days_for_data_sets(
        &self,
        data_channels: &[DC],
        data_set_ids: &[DataStoreId],
        timerange: &TimeRange,
    ) -> Result<HashMap<DataStoreId, BTreeSet<NaiveDate>>> {
        let mut result: HashMap<DataStoreId, BTreeSet<NaiveDate>> = HashMap::new();
        if data_channels.is_empty() || data_set_ids.is_empty() {
            return Ok(result);
        }

        let data_set_oids: Vec<ObjectId> = data_set_ids
            .iter()
            .filter_map(|id| ObjectId::parse_str(id).ok())
            .collect();
        if data_set_oids.is_empty() {
            return Ok(result);
        }

        let match_stage = doc! { "$match": {
            "metadata.data_set_id": { "$in": &data_set_oids },
            "time": { "$gte": timerange.from, "$lt": timerange.until }
        }};
        let project_stage = doc! { "$project": {
            "_id": 0,
            "data_set_id": "$metadata.data_set_id",
            "day": { "$dateToString": {
                "format": "%Y-%m-%d",
                "date": "$time",
                "timezone": "UTC"
            }}
        }};

        let first_channel = data_channels[0];
        let first_coll_name =
            SensorChannelStorage::new_with_resolution(first_channel, TimeResolution::Hours)
                .collection_name();
        let collection = self.collection::<Document>(&first_coll_name);

        let mut pipeline = vec![match_stage.clone(), project_stage.clone()];
        for &channel in data_channels.iter().skip(1) {
            let coll_name =
                SensorChannelStorage::new_with_resolution(channel, TimeResolution::Hours)
                    .collection_name();
            pipeline.push(doc! { "$unionWith": {
                "coll": coll_name,
                "pipeline": [match_stage.clone(), project_stage.clone()]
            }});
        }
        pipeline.push(doc! { "$group": {
            "_id": { "data_set_id": "$data_set_id", "day": "$day" }
        }});

        let mut cursor = collection.aggregate(pipeline).await?;
        while let Some(doc) = cursor.try_next().await? {
            let Ok(key) = doc.get_document("_id") else {
                continue;
            };
            let Ok(oid) = key.get_object_id("data_set_id") else {
                continue;
            };
            let Ok(day_str) = key.get_str("day") else {
                continue;
            };
            let Ok(date) = NaiveDate::parse_from_str(day_str, "%Y-%m-%d") else {
                continue;
            };
            result.entry(oid.to_hex()).or_default().insert(date);
        }

        Ok(result)
    }

    async fn create_data_export(
        &self,
        meas_point_id: &DataStoreId,
        timerange: &TimeRange,
        data_type: DT,
        project_id: &DataStoreId,
        trigger_id: i64,
    ) -> anyhow::Result<String> {
        let collection = self.collection::<schema::DataExport<DT>>(DATA_EXPORT_COLL_NAME);
        let data_export = schema::DataExport {
            id: None,
            timestamp: Utc::now().into(),
            measuring_point_id: ObjectId::parse_str(meas_point_id)?,
            data_type,
            project_id: project_id.clone(),
            time_range: timerange.clone(),
            file_id: None,
            file_size: None,
            status: DataExportStatus::InProgress,
            trigger_id: Some(trigger_id),
        };
        let insert_res = collection.insert_one(data_export).await?;
        insert_res
            .inserted_id
            .as_object_id()
            .map(|id| id.to_string())
            .ok_or(anyhow::anyhow!("Invalid ID"))
    }

    async fn update_data_export_status(
        &self,
        export_id: &DataStoreId,
        status: DataExportStatus,
    ) -> anyhow::Result<()> {
        self.collection::<schema::DataExport<DT>>(DATA_EXPORT_COLL_NAME)
            .update_one(
                doc! { "_id": ObjectId::parse_str(export_id)? },
                doc! { "$set": { "status": to_bson(&status)? } },
            )
            .await?;
        Ok(())
    }

    async fn write_data_export(
        &self,
        data_channels: &[DC],
        meas_point_id: &DataStoreId,
        timerange: &TimeRange,
        time_resolution: TimeResolution,
        export_id: &DataStoreId,
        csv_header: &str,
        clamp_to_data_range: bool,
    ) -> anyhow::Result<()> {
        let bucket = self.db().gridfs_bucket(
            GridFsBucketOptions::builder()
                .bucket_name(DATA_EXPORT_COLL_NAME.to_string())
                .build(),
        );
        let mut upload_stream = bucket.open_upload_stream("example.txt").await?;

        let mut mp_time_range = TimeRange {
            from: timerange.from,
            until: timerange.until,
        };
        if clamp_to_data_range {
            let first_timestamp = if let Some(data_channel) = data_channels.first() {
                self.find_first_timestamp_for_mp(*data_channel, meas_point_id)
                    .await?
                    .and_then(|ts| datetime_from_millis(ts).ok())
            } else {
                None
            };
            let last_timestamp = if let Some(data_channel) = data_channels.first() {
                self.find_latest_timestamp_for_mp(*data_channel, meas_point_id)
                    .await?
                    .and_then(|ts| datetime_from_millis(ts).ok())
            } else {
                None
            };
            if let Some(first_timestamp) = first_timestamp {
                if first_timestamp > mp_time_range.from {
                    mp_time_range.from = first_timestamp;
                }
            }
            if let Some(last_timestamp) = last_timestamp {
                if last_timestamp < mp_time_range.until {
                    mp_time_range.until = last_timestamp;
                }
            }
        }

        // The freq/perc columns are decided from the first non-empty batch: a field
        // is included only if the data actually carries it. The header is therefore
        // written lazily, once that decision can be made.
        let mut header_written = false;
        let mut include_freq = false;
        let mut include_perc = false;

        for day_range in mp_time_range.iter_hours() {
            let mut timeseries = MPSensorDataChannels::new();
            for &data_channel in data_channels.iter() {
                let data = self
                    .sensor_data_for_measuring_point(
                        data_channel,
                        Some(time_resolution),
                        meas_point_id,
                        &day_range,
                        day_range.until == mp_time_range.until,
                    )
                    .await?;
                timeseries.push(data);
            }

            if !header_written && !timeseries.is_empty() {
                include_freq = timeseries.has_freq();
                include_perc = timeseries.has_perc();
                let mut header = csv_header.to_string();
                if include_freq {
                    for dc in data_channels.iter() {
                        header.push_str(&format!(",{dc}_freq"));
                    }
                }
                if include_perc {
                    for dc in data_channels.iter() {
                        header.push_str(&format!(",{dc}_perc"));
                    }
                }
                header.push('\n');
                upload_stream.write_all(header.as_bytes()).await?;
                header_written = true;
            }

            upload_stream
                .write_all(timeseries.as_csv(include_freq, include_perc).as_bytes())
                .await?;
        }

        // No data at all in range → still emit the base header row.
        if !header_written {
            upload_stream
                .write_all(format!("{csv_header}\n").as_bytes())
                .await?;
        }

        upload_stream.close().await?;

        let file_size = self
            .collection::<Document>(&(DATA_EXPORT_COLL_NAME.to_string() + ".files"))
            .find_one(doc! { "_id": upload_stream.id() })
            .await?
            .and_then(|doc| doc.get_i64("length").ok());

        self.collection::<schema::DataExport<DT>>(DATA_EXPORT_COLL_NAME)
            .update_one(
                doc! { "_id": ObjectId::parse_str(export_id)? },
                doc! { "$set": { "file_id": upload_stream.id(), "file_size": file_size } },
            )
            .await?;
        Ok(())
    }

    async fn get_data_exports(&self, project_id: &MeteorId) -> Result<Vec<DataExport<DT>>>
    where
        DT: DeserializeOwned,
    {
        Ok(self
            .collection::<schema::DataExport<DT>>(DATA_EXPORT_COLL_NAME)
            .find(doc! { "project_id": project_id })
            .await?
            .try_collect::<Vec<_>>()
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    async fn get_data_export(&self, export_id: &DataStoreId) -> Result<Option<DataExport<DT>>>
    where
        DT: DeserializeOwned,
    {
        Ok(self
            .collection::<schema::DataExport<DT>>(DATA_EXPORT_COLL_NAME)
            .find_one(doc! { "_id": ObjectId::parse_str(export_id)? })
            .await?
            .map(Into::into))
    }

    async fn get_data_export_chunk(
        &self,
        file_id: &DataStoreId,
        chunk_index: u32,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        tracing::debug!("Getting chunk {} of file {}", chunk_index, file_id);
        let Some(document) = self
            .collection::<Document>(&format!("{DATA_EXPORT_COLL_NAME}.chunks"))
            .find_one(doc! {
                "files_id": ObjectId::parse_str(file_id)?,
                "n": chunk_index,
            })
            .projection(doc! { "data": 1 })
            .await?
        else {
            return Ok(None);
        };
        let data = document
            .get("data")
            .ok_or(anyhow::anyhow!("No data found"))?;
        let Bson::Binary(Binary { bytes, .. }) = data else {
            return Err(anyhow::anyhow!("Data is not binary"));
        };
        Ok(Some(bytes.to_vec()))
    }

    async fn delete_data_export(&self, export_id: &DataStoreId) -> Result<()>
    where
        DT: DeserializeOwned,
    {
        let export: DataExport<DT> = self
            .get_data_export(export_id)
            .await?
            .ok_or(DataStoreError::NotFound("data export"))?;
        match export.file_id {
            Some(file_id) => {
                let bucket = self.db().gridfs_bucket(
                    GridFsBucketOptions::builder()
                        .bucket_name(DATA_EXPORT_COLL_NAME.to_string())
                        .build(),
                );
                bucket.delete(ObjectId::parse_str(&file_id)?.into()).await?;
            }
            None => {
                tracing::warn!("No file_id found for data export {}", export_id);
            }
        }
        self.collection::<schema::DataExport<DT>>(DATA_EXPORT_COLL_NAME)
            .delete_one(doc! { "_id": ObjectId::parse_str(export_id)? })
            .await?;
        Ok(())
    }

    async fn cleanup_data_exports(&self) -> anyhow::Result<()>
    where
        DT: DeserializeOwned,
    {
        let three_days_ago = Utc::now() - chrono::Duration::days(3);
        let filter = doc! { "timestamp": { "$lt": three_days_ago } };
        let mut cursor = self
            .collection::<schema::DataExport<DT>>(DATA_EXPORT_COLL_NAME)
            .find(filter)
            .await?;
        while let Some(data_export) = cursor.next().await {
            if let Ok(schema::DataExport { id: Some(id), .. }) = data_export {
                let _ = self.delete_data_export(&id.to_hex()).await.map_err(|err| {
                    tracing::error!("Error deleting data export: {err}");
                });
            }
        }
        Ok(())
    }

    async fn data_sets_for_sensor_ids(
        &self,
        sensor_ids: &[String],
        max_timestamp: i64,
    ) -> Result<Vec<DataSet>> {
        let filter =
            doc! { "sensor_id": { "$in": sensor_ids }, "start": { "$lte": max_timestamp } };
        self.collection::<schema::DataSet>(DATA_SET_COLL_NAME)
            .find(filter)
            .into_future()
            .and_then(|cursor| cursor.try_collect::<Vec<_>>())
            .await
            .map(|data_sets| data_sets.into_iter().map(Into::into).collect())
            .map_err(Into::into)
    }

    async fn latest_data_set_for_measuring_point(
        &self,
        meas_point_id: &DataStoreId,
        max_timestamp: i64,
    ) -> Result<Option<DataSet>> {
        let filter = doc! { "measuring_point_id": ObjectId::parse_str(meas_point_id)?, "start": { "$lt": max_timestamp } };
        Ok(self
            .collection::<schema::DataSet>(DATA_SET_COLL_NAME)
            .find_one(filter)
            .sort(doc! { "start": -1 })
            .await?
            .map(Into::into))
    }

    async fn all_data_set_mp_ids(&self) -> anyhow::Result<Vec<DataStoreId>> {
        self.collection::<schema::DataSet>(DATA_SET_COLL_NAME)
            .distinct("measuring_point_id", doc! {})
            .await
            .map_err(Into::into)
            .map(|ids| ids.into_iter().map(|id| id.to_string()).collect())
    }

    async fn data_sets_for_measuring_point(
        &self,
        meas_point_id: &DataStoreId,
    ) -> Result<Vec<DataSet>> {
        let meas_point_id = ObjectId::parse_str(meas_point_id)?;
        self.collection::<schema::DataSet>(DATA_SET_COLL_NAME)
            .find(doc! { "measuring_point_id": meas_point_id })
            .await?
            .try_collect::<Vec<_>>()
            .await
            .map(|data_sets| data_sets.into_iter().map(Into::into).collect())
            .map_err(Into::into)
    }

    async fn upsert_data_set(&self, data_set: NewDataSet) -> Result<()> {
        let data_set: schema::NewDataSet = data_set.try_into()?;
        self.collection::<schema::NewDataSet>(DATA_SET_COLL_NAME)
            .replace_one(doc! { "_id": data_set.id.unwrap_or_default() }, data_set)
            .upsert(true)
            .await?;
        Ok(())
    }

    async fn trigger_building_resolutions(
        &self,
        channel: DC,
        data_set_id: &DataStoreId,
    ) -> anyhow::Result<()> {
        let msg = MatViewMsg {
            data_channel: channel,
            data_set_id: data_set_id.clone(),
        };
        self.tx_to_mat_view_task.send(msg).await?;
        Ok(())
    }

    async fn delete_old_sensor_data(
        &self,
        channels: &[DC],
        days_to_keep: u64,
    ) -> anyhow::Result<()> {
        let cutoff_time = Utc::now() - chrono::Duration::days(days_to_keep as i64);
        let cutoff_datetime = DateTime::from_chrono(cutoff_time);

        tracing::info!(
            "Deleting sensor data older than {} days (before {})",
            days_to_keep,
            cutoff_time
        );

        let mut total_deleted = 0u64;

        for &channel in channels {
            let channel_storage = SensorChannelStorage::new(channel);

            for resolution in TimeResolution::iter() {
                let col_name = channel_storage.collection_name_for_resolution(resolution);
                let collection = self.collection::<Document>(&col_name);

                match collection
                    .delete_many(doc! { "time": { "$lt": cutoff_datetime } })
                    .await
                {
                    Ok(result) => {
                        if result.deleted_count > 0 {
                            tracing::info!(
                                "Deleted {} documents from collection {}",
                                result.deleted_count,
                                col_name
                            );
                            total_deleted += result.deleted_count;
                        }
                    }
                    Err(err) => {
                        tracing::error!("Failed to delete old data from {}: {}", col_name, err);
                    }
                }
            }
        }

        tracing::info!("Total deleted documents: {}", total_deleted);
        Ok(())
    }
}

// ── impl EventStore ───────────────────────────────────────────────────────────

#[async_trait]
impl<DC: DataKind, C, D, DT: DataKind, T, DevSt> EventStore for MongoDb<DC, C, D, DT, T, DevSt>
where
    C: Serialize + DeserializeOwned + Send + Sync + 'static,
    D: Serialize + DeserializeOwned + Send + Sync + 'static,
    T: Send + Sync + 'static,
    DevSt: Send + Sync + 'static,
{
    type ContactData = C;
    type EventData = D;

    async fn insert_event(&self, event: NewEvent<C, D>) -> Result<DataStoreId> {
        self.insert_event_impl(event, None).await
    }

    async fn query_events(&self, query: EventQuery) -> Result<EventsWithStats<C, D>> {
        let mut filter = to_document(&query.params)?;
        if let Some(time_range) = query.params.time_range {
            filter.insert(
                "timestamp",
                doc! { "$gte": time_range.from, "$lte": time_range.until },
            );
            filter.remove("from");
            filter.remove("until");
        }
        let types_filter = if let Some(types) = query.params.types {
            let bson_types = types
                .into_iter()
                .filter_map(|t| to_bson(&t).ok())
                .collect::<Vec<_>>();
            filter.remove("types");
            Some(doc! { "$in": bson_types })
        } else {
            None
        };
        if let Some(codes) = query.code {
            filter.insert("code", doc! { "$in": codes });
        }
        if let Some(project_ids) = query.params.project_ids {
            filter.remove("project_id");
            filter.remove("project_ids");
            filter.insert("project_id", doc! { "$in": project_ids });
        }
        if let Some(meas_point_ids) = query.params.measuring_point_ids {
            let mp_oids = meas_point_ids
                .into_iter()
                .filter_map(|t| ObjectId::parse_str(&t).ok())
                .collect::<Vec<_>>();
            filter.insert("measuring_point_id", doc! { "$in": mp_oids });
            filter.remove("measuring_point_ids");
        }
        if let Some(true) = query.params.has_contact_details {
            filter.insert("contact_details", doc! { "$exists": true });
            filter.remove("has_contact_details");
        }
        // Resolve group_id → project_ids so all three event collections share a
        // project-led $match. s2d has no group_id; events are project-scoped.
        if !filter.contains_key("project_id") {
            if let Some(group_id) = query.params.group_id.as_deref() {
                let project_ids: Vec<MeteorId> = self
                    .collection::<Document>(schema::PROJECT_COLL_NAME)
                    .find(doc! { "groupID": group_id })
                    .projection(doc! { "project_id": 1_i32 })
                    .await?
                    .try_collect::<Vec<_>>()
                    .await?
                    .into_iter()
                    .filter_map(|d| d.get_str("project_id").ok().map(str::to_string))
                    .collect();
                filter.insert("project_id", doc! { "$in": project_ids });
            }
        }
        // After adding project_id fields, we no longer need group_id.
        // Also s2d.events don't set group_id so we must not query on it
        filter.remove("group_id");

        let events_field_pipeline = if query.only_stats {
            vec![doc! { "$limit": 1 }, doc! { "$skip": 1 }]
        } else {
            vec![
                doc! { "$lookup": {
                    "from": MEAS_POINT_COLL_NAME,
                    "localField": "measuring_point_id",
                    "foreignField": "_id",
                    "as": "measuring_point"
                }},
                doc! { "$unwind": { "path": "$measuring_point", "preserveNullAndEmptyArrays": true } },
                doc! { "$lookup": {
                    "from": MEAS_POINT_SHADOW_COLL_NAME,
                    "let": { "meas_point_id": { "$toString": "$measuring_point_id" } },
                    "pipeline": [
                        doc! { "$match": { "$expr": { "$eq": [ "$_id", "$$meas_point_id" ] } } },
                    ],
                    "as": "measuring_point_shadow"
                }},
                doc! { "$unwind": { "path": "$measuring_point_shadow", "preserveNullAndEmptyArrays": true } },
                doc! { "$lookup": {
                    "from": schema::CLUSTER_COLL_NAME,
                    "localField": "measuring_point.cluster_id",
                    "foreignField": "_id",
                    "as": "cluster"
                }},
                doc! { "$unwind": { "path": "$cluster", "preserveNullAndEmptyArrays": true } },
                doc! { "$lookup": {
                    "from": schema::CLUSTER_SHADOW_COLL_NAME,
                    "localField": "measuring_point_shadow.cluster_id",
                    "foreignField": "_id",
                    "as": "cluster_shadow"
                }},
                doc! { "$unwind": { "path": "$cluster_shadow", "preserveNullAndEmptyArrays": true } },
                doc! { "$lookup": {
                    "from": schema::PROJECT_COLL_NAME,
                    "localField": "project_id",
                    "foreignField": "project_id",
                    "as": "project"
                }},
                doc! { "$unwind": { "path": "$project", "preserveNullAndEmptyArrays": true } },
                doc! { "$lookup": {
                    "from": DEVICE_COLL_NAME,
                    "localField": "device_id",
                    "foreignField": "_id",
                    "as": "device"
                }},
                doc! { "$unwind": { "path": "$device", "preserveNullAndEmptyArrays": true } },
                doc! { "$lookup": {
                    "from": schema::GROUP_COLL_NAME,
                    "localField": "group_id",
                    "foreignField": "group_id",
                    "as": "group"
                }},
                doc! { "$unwind": { "path": "$group", "preserveNullAndEmptyArrays": true } },
                doc! { "$addFields": {
                    "project_name": "$project.name",
                    "cluster_name": { "$ifNull": ["$cluster.name", "$cluster_shadow.name"] },
                    "measuring_point_name": { "$ifNull": ["$measuring_point.name", "$measuring_point_shadow.name"] },
                    "device_name": { "$ifNull": ["$device.name", "$device._id"] },
                    "group_name": "$group.name",
                    "type": "$desc.type",
                    "msg": "$desc.nl",
                    "msg_en": "$desc.msg",
                }},
                doc! { "$project": {
                    "project": 0,
                    "cluster": 0,
                    "cluster_shadow": 0,
                    "measuring_point": 0,
                    "measuring_point_shadow": 0,
                    "device": 0,
                    "group": 0,
                    "desc": 0,
                }},
            ]
        };

        // Only do the eventDescriptors $lookup before $facet when downstream actually needs
        // it per row: the events output (non-stats path) or a types filter that gates the
        // union by desc.type. For the stats-only / no-types path, we let code_stats and
        // type_stats look up the ~10 distinct codes inside the facet instead of paying the
        // $lookup once per input row.
        let pre_facet_desc_lookup = !query.only_stats || types_filter.is_some();

        let mut pipeline = vec![
            doc! { "$match": &filter },
            doc! { "$unionWith": {
                "coll": TRACE_EVENT_COLL_NAME,
                "pipeline": [doc! { "$match": &filter }]
            }},
            doc! { "$unionWith": {
                "coll": S2D_EVENTS_COLL_NAME,
                "pipeline": [doc! { "$match": &filter }]
            }},
        ];
        if pre_facet_desc_lookup {
            pipeline.push(doc! { "$lookup": {
                "from": EVENT_DESCRIPTORS_COLL_NAME,
                "localField": "code",
                "foreignField": "code",
                "as": "desc"
            }});
        }
        if let Some(types_filter) = types_filter {
            pipeline.push(doc! { "$match": { "desc.type": types_filter } });
        }
        // Sort / skip / limit only affect the events facet branch — skip them entirely for
        // stats-only requests, where they'd just force materialising the full union.
        if !query.only_stats {
            if let Some(sort_max_perc) = query.sort_max_perc {
                pipeline.push(
                    doc! { "$sort": { "max_percentage": sort_max_perc, "timestamp": query.sort } },
                );
            } else {
                pipeline.push(doc! { "$sort": { "timestamp": query.sort } });
            }
            if let Some(skip) = query.skip {
                if skip > 0 {
                    pipeline.push(doc! { "$skip": skip });
                }
            }
            if let Some(limit) = query.limit {
                pipeline.push(doc! { "$limit": limit });
            }
        }
        if pre_facet_desc_lookup {
            pipeline
                .push(doc! { "$unwind": { "path": "$desc", "preserveNullAndEmptyArrays": true } });
        } else {
            // Slim docs to just what the stats facets read. Cuts per-doc work through the
            // three $group stages now that nothing downstream needs the full event.
            pipeline.push(doc! { "$project": {
                "project_id": 1_i32,
                "timestamp": 1_i32,
                "code": 1_i32,
            }});
        }
        let (code_stats, type_stats) = if pre_facet_desc_lookup {
            (
                vec![
                    doc! { "$group": { "_id": "$code", "count": { "$sum": 1 }, "code": { "$first": "$code" }, "type": { "$first": "$desc.type" }, "msg": { "$first": "$desc.nl" }, "msg_en": { "$first": "$desc.msg" } } },
                ],
                vec![
                    doc! { "$group": { "_id": "$desc.type", "count": { "$sum": 1 }, "type": { "$first": "$desc.type" } } },
                ],
            )
        } else {
            // Group first, then look up the ~10 distinct codes instead of every row.
            (
                vec![
                    doc! { "$group": { "_id": "$code", "count": { "$sum": 1 } } },
                    doc! { "$lookup": { "from": EVENT_DESCRIPTORS_COLL_NAME, "localField": "_id", "foreignField": "code", "as": "desc" } },
                    doc! { "$unwind": { "path": "$desc", "preserveNullAndEmptyArrays": true } },
                    doc! { "$addFields": { "code": "$_id", "type": "$desc.type", "msg": "$desc.nl", "msg_en": "$desc.msg" } },
                    doc! { "$project": { "desc": 0_i32 } },
                ],
                vec![
                    doc! { "$group": { "_id": "$code", "count": { "$sum": 1 } } },
                    doc! { "$lookup": { "from": EVENT_DESCRIPTORS_COLL_NAME, "localField": "_id", "foreignField": "code", "as": "desc" } },
                    doc! { "$unwind": { "path": "$desc", "preserveNullAndEmptyArrays": true } },
                    doc! { "$group": { "_id": "$desc.type", "count": { "$sum": "$count" }, "type": { "$first": "$desc.type" } } },
                ],
            )
        };
        pipeline.push(doc! { "$facet": {
            "code_stats": code_stats,
            "type_stats": type_stats,
            "hour_stats": [
                doc! { "$match": { "project_id": { "$ne": bson::Bson::Null } } },
                doc! { "$group": { "_id": { "projectID": "$project_id", "hour": { "$hour": "$timestamp" } }, "count": { "$sum": 1 } } },
                doc! { "$group": { "_id": "$_id.projectID", "hour_stats": { "$push": { "hour": "$_id.hour", "count": "$count" } } } },
                doc! { "$addFields": { "hour_stats": { "$sortArray": { "input": "$hour_stats", "sortBy": { "hour": 1 } } } } },
                doc! { "$lookup": { "from": "projects", "localField": "_id", "foreignField": "project_id", "as": "project" } },
                doc! { "$unwind": { "path": "$project", "preserveNullAndEmptyArrays": true } },
                doc! { "$project": { "projectId": { "$toString": "$_id" }, "projectName": "$project.name", "hourStats": "$hour_stats" } },
            ],
            "events": events_field_pipeline,
        }});

        self.collection::<schema::Event<C, D>>(EVENTS_COLL_NAME)
            .aggregate(pipeline)
            .await?
            .map(|result| {
                result.and_then(|doc| {
                    bson::from_bson::<schema::EventsWithStats<C, D>>(bson::Bson::Document(doc))
                        .map_err(Into::into)
                })
            })
            .try_collect::<Vec<_>>()
            .await?
            .pop()
            .map(Into::into)
            .ok_or(DataStoreError::NotFound("events"))
    }

    async fn add_event_comment(
        &self,
        event_id: DataStoreId,
        comment: crate::event::Comment,
    ) -> Result<()> {
        let id = ObjectId::parse_str(&event_id)?;
        self.collection::<bson::Document>(EVENTS_COLL_NAME)
            .update_one(
                doc! { "_id": id },
                doc! { "$push": { "comments": to_document(&comment)? } },
            )
            .await?;
        self.collection::<bson::Document>(S2D_EVENTS_COLL_NAME)
            .update_one(
                doc! { "_id": id },
                doc! { "$push": { "comments": to_document(&comment)? } },
            )
            .await?;
        Ok(())
    }

    async fn edit_event_comment(
        &self,
        event_id: DataStoreId,
        comment_id: String,
        user_id: MeteorId,
        comment: String,
    ) -> Result<()> {
        let id = ObjectId::parse_str(&event_id)?;
        tracing::debug!("edit_event_comment: {event_id} {comment_id} {user_id}");
        self.collection::<bson::Document>(EVENTS_COLL_NAME)
            .update_one(
                doc! { "_id": id, "comments._id": &comment_id, "comments.userID": &user_id },
                doc! { "$set": { "comments.$[element].msg": &comment, "comments.$[element].edited": Utc::now().timestamp() } },
            )
            .array_filters(vec![doc! { "element._id": &comment_id, "element.userID": &user_id }])
            .await?;
        self.collection::<bson::Document>(S2D_EVENTS_COLL_NAME)
            .update_one(
                doc! { "_id": id, "comments._id": &comment_id, "comments.userID": &user_id },
                doc! { "$set": { "comments.$[element].msg": comment, "comments.$[element].edited": Utc::now().timestamp() } },
            )
            .array_filters(vec![doc! { "element._id": comment_id, "element.userID": user_id }])
            .await?;
        Ok(())
    }

    async fn event_mark_sent(
        &self,
        event_id: &DataStoreId,
        send_status: SendStatus,
    ) -> anyhow::Result<()> {
        let id = ObjectId::parse_str(event_id)?;
        let collection = self.collection::<bson::Document>(EVENTS_COLL_NAME);
        let result = collection
            .update_one(
                doc! { "_id": id },
                doc! { "$set": { "sent": bson::to_bson(&send_status)? } },
            )
            .await?;

        if result.modified_count != 1 {
            return Err(anyhow::anyhow!(
                "event_mark_sent: Unexpected modified_count: {}",
                result.modified_count
            ));
        }
        Ok(())
    }

    async fn event_update_code(
        &self,
        event_id: &DataStoreId,
        event_code: u32,
        event_type: EventType,
    ) -> anyhow::Result<()> {
        let id = ObjectId::parse_str(event_id)?;
        let collection = self.collection::<bson::Document>(EVENTS_COLL_NAME);
        let result = collection
            .update_one(
                doc! { "_id": id },
                doc! { "$set": { "type": to_bson(&event_type)?, "code": event_code } },
            )
            .await?;

        if result.modified_count != 1 {
            return Err(anyhow::anyhow!(
                "event_update_code: Unexpected modified_count: {}",
                result.modified_count
            ));
        }
        Ok(())
    }
}

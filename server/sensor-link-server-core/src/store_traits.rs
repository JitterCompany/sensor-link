use std::collections::{BTreeSet, HashMap};

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;

use crate::{
    data_export::{DataExport, DataExportStatus},
    data_kind::DataKind,
    data_set::{DataSet, NewDataSet},
    device::{Device, DeviceExt, DeviceFieldType, DeviceQuery, DeviceStatusLike},
    event::{EventQuery, EventType, EventsWithStats, NewEvent, SendStatus},
    firmware::{Firmware, NewFirmware},
    sensor_data::{MPSensorData, SensorData, TimeResolution},
    sensor_server_log::SensorServerLog,
    DataStoreId, MeteorId, TimeRange,
};

/// Optional query parameters for [`SensorDataStore::sensor_data_for_measuring_point_with_options`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SensorDataOptions {
    pub sort: Option<i32>,
    pub limit: Option<u64>,
}

#[derive(Debug, Error)]
pub enum DataStoreError {
    #[error("Database error: {0}")]
    Database(String),

    #[error("Database read failed: {0}")]
    DatabaseReadFailed(String),

    #[error("Database write failed: {0}")]
    DatabaseWriteFailed(String),

    #[error("Invalid database ID: {0}")]
    InvalidDatabaseId(String),

    #[error("Too much data requested")]
    TooMuchData,

    #[error("Not found: {0}")]
    NotFound(&'static str),
}

pub type Result<T> = anyhow::Result<T, DataStoreError>;

#[cfg(feature = "actix")]
impl actix_web::ResponseError for DataStoreError {
    fn status_code(&self) -> actix_web::http::StatusCode {
        use actix_web::http::StatusCode;
        match self {
            DataStoreError::Database(_)
            | DataStoreError::DatabaseReadFailed(_)
            | DataStoreError::DatabaseWriteFailed(_) => StatusCode::INTERNAL_SERVER_ERROR,
            DataStoreError::InvalidDatabaseId(_) | DataStoreError::TooMuchData => {
                StatusCode::BAD_REQUEST
            }
            DataStoreError::NotFound(_) => StatusCode::NOT_FOUND,
        }
    }
}

#[async_trait]
pub trait TransactionDataStore {
    type ContactData;
    type EventData;
    type DeviceType;
    type DeviceStatus: DeviceStatusLike;

    async fn set_device_field(
        &mut self,
        device_id: &str,
        value: DeviceFieldType<Self::DeviceType, Self::DeviceStatus>,
    ) -> Result<()>;

    async fn set_device_field_for_groups(
        &mut self,
        device_id: &str,
        group_ids: Vec<String>,
        value: DeviceFieldType<Self::DeviceType, Self::DeviceStatus>,
    ) -> Result<()>;

    async fn insert_event(
        &mut self,
        event: NewEvent<Self::ContactData, Self::EventData>,
    ) -> Result<DataStoreId>;

    async fn commit(&mut self) -> Result<()>;
}

#[async_trait]
pub trait DeviceStore: Send + Sync + 'static {
    type TxContactData;
    type TxEventData;
    type DeviceType: Serialize + DeserializeOwned;
    type DeviceStatus: DeviceStatusLike + Serialize + DeserializeOwned + Default;

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
    >;

    async fn get_devices(
        &self,
        query: DeviceQuery,
    ) -> Result<Vec<DeviceExt<Self::DeviceType, Self::DeviceStatus>>>;

    async fn get_devices_simple(
        &self,
        query: DeviceQuery,
    ) -> Result<Vec<Device<Self::DeviceType, Self::DeviceStatus>>>;

    async fn device_by_id(
        &self,
        id: &str,
    ) -> Result<Option<Device<Self::DeviceType, Self::DeviceStatus>>>;

    async fn get_device_ids_for_hub(&self, hub_id: &DataStoreId) -> Result<Vec<DataStoreId>>;

    async fn remove_device(&self, device_id: &DataStoreId) -> Result<()>;

    async fn upsert_devices_for_datatype(
        &self,
        device_type: Self::DeviceType,
        device_ids: &[String],
    ) -> Result<Vec<String>>;

    async fn set_device_field(
        &self,
        device_id: &str,
        value: DeviceFieldType<Self::DeviceType, Self::DeviceStatus>,
    ) -> Result<()>;

    async fn set_device_field_for_groups(
        &self,
        device_id: &str,
        group_ids: Vec<String>,
        value: DeviceFieldType<Self::DeviceType, Self::DeviceStatus>,
    ) -> Result<()>;

    async fn set_field_for_devices(
        &self,
        device_ids: &[String],
        value: DeviceFieldType<Self::DeviceType, Self::DeviceStatus>,
    ) -> Result<()>;

    async fn insert_sensor_server_log(&self, log: SensorServerLog) -> anyhow::Result<()>;

    async fn get_sensor_server_log(
        &self,
        device_id: &DataStoreId,
        limit: u32,
    ) -> Result<Vec<SensorServerLog>>;

    async fn unlink_sensor_from_measuring_points(&self, sensor_id: &DataStoreId) -> Result<()>;

    async fn unlink_sensor_from_measuring_point_shadows(
        &self,
        sensor_id: &DataStoreId,
    ) -> Result<()>;
}

#[async_trait]
pub trait FirmwareStore: Send + Sync + 'static {
    type DeviceType: Serialize;

    async fn insert_firmware(&self, firmware: NewFirmware<Self::DeviceType>) -> Result<()>;

    async fn get_firmwares(
        &self,
        device_type: &Self::DeviceType,
    ) -> Result<Vec<Firmware<Self::DeviceType>>>;

    async fn firmware_by_id(&self, id: &DataStoreId) -> Result<Option<Firmware<Self::DeviceType>>>;

    async fn remove_firmware(&self, id: &DataStoreId) -> Result<()>;

    async fn recommend_firmware(&self, id: &DataStoreId) -> Result<()>;

    async fn unrecommend_other_firmwares_for_same_device_type(
        &self,
        id: &DataStoreId,
    ) -> Result<()>;
}

#[async_trait]
pub trait SensorDataStore: Send + Sync + 'static {
    type DataChannel: DataKind;
    type DataType: DataKind;

    fn clone_dyn(
        &self,
    ) -> Box<dyn SensorDataStore<DataChannel = Self::DataChannel, DataType = Self::DataType>>;

    async fn insert_sensordata<'a>(
        &'a self,
        data_channel: Self::DataChannel,
        sensor_data: Vec<SensorData<Self::DataChannel>>,
    ) -> anyhow::Result<()>;

    async fn data_set_ids_for_measuring_point(
        &self,
        meas_point_ids: &DataStoreId,
    ) -> Result<Vec<DataStoreId>>;

    async fn find_latest_datapoint_for_mp(
        &self,
        data_channel: Self::DataChannel,
        meas_point_id: &DataStoreId,
        until: DateTime<Utc>,
    ) -> Result<Option<(i64, f32)>>;

    async fn find_latest_timestamp_for_mp<'a>(
        &'a self,
        data_channel: Self::DataChannel,
        meas_point_id: &'a DataStoreId,
    ) -> Result<Option<i64>>;

    async fn find_first_timestamp_for_mp<'a>(
        &'a self,
        data_channel: Self::DataChannel,
        meas_point_id: &'a DataStoreId,
    ) -> Result<Option<i64>>;

    /// Clear materialized-view collections for the given channels and measuring point.
    ///
    /// `channels` should contain every channel whose downsampled collections
    /// need to be wiped (i.e. all channels where `DataKind::downsampling` is true).
    async fn clear_materialized_views(
        &self,
        channels: &[Self::DataChannel],
        meas_point_id: &DataStoreId,
        from: DateTime<Utc>,
        res: TimeResolution,
    ) -> Result<()>;

    async fn update_materialized_views<'a>(
        &'a self,
        data_channel: Self::DataChannel,
        data_set_id: &'a str,
    ) -> anyhow::Result<()>;

    async fn count_sensor_data_for_measuring_point(
        &self,
        data_channel: Self::DataChannel,
        resolution: TimeResolution,
        meas_point_id: &DataStoreId,
        timerange: &TimeRange,
        limit: u64,
    ) -> anyhow::Result<u64>;

    async fn sensor_data_for_measuring_point(
        &self,
        data_channel: Self::DataChannel,
        resolution: Option<TimeResolution>,
        meas_point_id: &DataStoreId,
        timerange: &TimeRange,
        inclusive_range: bool,
    ) -> Result<MPSensorData>;

    async fn sensor_data_for_measuring_point_with_options(
        &self,
        data_channel: Self::DataChannel,
        resolution: Option<TimeResolution>,
        meas_point_id: &DataStoreId,
        timerange: &TimeRange,
        inclusive_range: bool,
        options: SensorDataOptions,
    ) -> Result<MPSensorData>;

    async fn get_highest_values(
        &self,
        data_channels: &[Self::DataChannel],
        meas_point_id: &DataStoreId,
        timerange: &TimeRange,
        nr_values: u32,
    ) -> Result<Vec<SensorData<Self::DataChannel>>>;

    /// For each given `data_set_id`, returns the set of UTC dates on which
    /// at least one hourly-aggregate row exists across any of the supplied
    /// `data_channels`, within `[timerange.from, timerange.until)`.
    /// Only data sets that have at least one active day are present in the
    /// result map.
    async fn active_days_for_data_sets(
        &self,
        data_channels: &[Self::DataChannel],
        data_set_ids: &[DataStoreId],
        timerange: &TimeRange,
    ) -> Result<HashMap<DataStoreId, BTreeSet<NaiveDate>>>;

    async fn create_data_export(
        &self,
        meas_point_id: &DataStoreId,
        timerange: &TimeRange,
        data_type: Self::DataType,
        project_id: &DataStoreId,
        trigger_id: i64,
    ) -> anyhow::Result<String>;

    async fn update_data_export_status(
        &self,
        export_id: &DataStoreId,
        status: DataExportStatus,
    ) -> anyhow::Result<()>;

    async fn write_data_export(
        &self,
        data_channels: &[Self::DataChannel],
        meas_point_id: &DataStoreId,
        timerange: &TimeRange,
        time_resolution: TimeResolution,
        export_id: &DataStoreId,
        csv_header: &str,
    ) -> anyhow::Result<()>;

    async fn get_data_exports(
        &self,
        project_id: &MeteorId,
    ) -> Result<Vec<DataExport<Self::DataType>>>
    where
        Self::DataType: serde::de::DeserializeOwned;

    async fn get_data_export(
        &self,
        export_id: &DataStoreId,
    ) -> Result<Option<DataExport<Self::DataType>>>
    where
        Self::DataType: serde::de::DeserializeOwned;

    async fn get_data_export_chunk(
        &self,
        file_id: &DataStoreId,
        chunk_index: u32,
    ) -> anyhow::Result<Option<Vec<u8>>>;

    async fn delete_data_export(&self, export_id: &DataStoreId) -> Result<()>
    where
        Self::DataType: serde::de::DeserializeOwned;

    async fn cleanup_data_exports(&self) -> anyhow::Result<()>
    where
        Self::DataType: serde::de::DeserializeOwned;

    /// Delete sensor data older than `days_to_keep` days for all provided channels.
    async fn delete_old_sensor_data(
        &self,
        channels: &[Self::DataChannel],
        days_to_keep: u64,
    ) -> anyhow::Result<()>;

    async fn all_data_set_mp_ids(&self) -> anyhow::Result<Vec<DataStoreId>>;

    async fn data_sets_for_sensor_ids(
        &self,
        sensor_ids: &[String],
        max_timestamp: i64,
    ) -> Result<Vec<DataSet>>;

    async fn latest_data_set_for_measuring_point(
        &self,
        meas_point_id: &DataStoreId,
        max_timestamp: i64,
    ) -> Result<Option<DataSet>>;

    async fn data_sets_for_measuring_point(
        &self,
        meas_point_id: &DataStoreId,
    ) -> Result<Vec<DataSet>>;

    async fn upsert_data_set(&self, data_set: NewDataSet) -> Result<()>;

    async fn trigger_building_resolutions(
        &self,
        channel: Self::DataChannel,
        data_set_id: &DataStoreId,
    ) -> anyhow::Result<()>;
}

#[async_trait]
pub trait EventStore: Send + Sync + 'static {
    type ContactData;
    type EventData;

    async fn insert_event(
        &self,
        event: NewEvent<Self::ContactData, Self::EventData>,
    ) -> Result<DataStoreId>;

    async fn query_events(
        &self,
        query: EventQuery,
    ) -> Result<EventsWithStats<Self::ContactData, Self::EventData>>;

    async fn add_event_comment(
        &self,
        event_id: DataStoreId,
        comment: crate::event::Comment,
    ) -> Result<()>;

    async fn edit_event_comment(
        &self,
        event_id: DataStoreId,
        comment_id: String,
        user_id: MeteorId,
        comment: String,
    ) -> Result<()>;

    async fn event_mark_sent(
        &self,
        event_id: &DataStoreId,
        send_status: SendStatus,
    ) -> anyhow::Result<()>;

    async fn event_update_code(
        &self,
        event_id: &DataStoreId,
        event_code: u32,
        event_type: EventType,
    ) -> anyhow::Result<()>;
}

/// Test utilities: mock store implementations for unit tests that do not require a live database.
#[cfg(test)]
pub(crate) mod test {
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::{DeviceStore, FirmwareStore, Result, TransactionDataStore};
    use crate::{
        device::{Device, DeviceExt, DeviceFieldType, DeviceQuery},
        firmware::{Firmware, NewFirmware},
        sensor_server_log::SensorServerLog,
        DataStoreId,
    };

    /// Mock implementation of [`DeviceStore`], [`FirmwareStore`], and [`EventStore`].
    ///
    /// Set closure fields for the methods under test; any unconfigured method panics
    /// with `unimplemented!()` if called.
    ///
    /// ```ignore
    /// let store = MockStore {
    ///     set_device_field: Some(Arc::new(|_, _| Ok(()))),
    ///     ..Default::default()
    /// };
    /// ```
    #[derive(Default)]
    pub struct MockStore {
        pub set_device_field:
            Option<Arc<dyn Fn(&str, DeviceFieldType<(), ()>) -> Result<()> + Send + Sync>>,
        pub firmware_by_id:
            Option<Arc<dyn Fn(&DataStoreId) -> Result<Option<Firmware<String>>> + Send + Sync>>,
    }

    #[async_trait]
    impl DeviceStore for MockStore {
        type TxContactData = ();
        type TxEventData = ();
        type DeviceType = ();
        type DeviceStatus = ();

        async fn set_device_field(
            &self,
            device_id: &str,
            value: DeviceFieldType<(), ()>,
        ) -> Result<()> {
            self.set_device_field
                .as_ref()
                .expect("set_device_field called but not configured on MockStore")(
                device_id, value
            )
        }

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
            unimplemented!()
        }
        async fn get_devices(
            &self,
            _: DeviceQuery,
        ) -> Result<Vec<DeviceExt<Self::DeviceType, Self::DeviceStatus>>> {
            unimplemented!()
        }
        async fn get_devices_simple(
            &self,
            _: DeviceQuery,
        ) -> Result<Vec<Device<Self::DeviceType, Self::DeviceStatus>>> {
            unimplemented!()
        }
        async fn device_by_id(
            &self,
            _: &str,
        ) -> Result<Option<Device<Self::DeviceType, Self::DeviceStatus>>> {
            unimplemented!()
        }
        async fn get_device_ids_for_hub(&self, _: &DataStoreId) -> Result<Vec<DataStoreId>> {
            unimplemented!()
        }
        async fn remove_device(&self, _: &DataStoreId) -> Result<()> {
            unimplemented!()
        }
        async fn upsert_devices_for_datatype(
            &self,
            _: Self::DeviceType,
            _: &[String],
        ) -> Result<Vec<String>> {
            unimplemented!()
        }
        async fn set_device_field_for_groups(
            &self,
            _: &str,
            _: Vec<String>,
            _: DeviceFieldType<(), ()>,
        ) -> Result<()> {
            unimplemented!()
        }
        async fn set_field_for_devices(
            &self,
            _: &[String],
            _: DeviceFieldType<(), ()>,
        ) -> Result<()> {
            unimplemented!()
        }
        async fn insert_sensor_server_log(&self, _: SensorServerLog) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn get_sensor_server_log(
            &self,
            _: &DataStoreId,
            _: u32,
        ) -> Result<Vec<SensorServerLog>> {
            unimplemented!()
        }
        async fn unlink_sensor_from_measuring_points(&self, _: &DataStoreId) -> Result<()> {
            unimplemented!()
        }
        async fn unlink_sensor_from_measuring_point_shadows(&self, _: &DataStoreId) -> Result<()> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl FirmwareStore for MockStore {
        type DeviceType = String;

        async fn firmware_by_id(
            &self,
            id: &DataStoreId,
        ) -> Result<Option<Firmware<Self::DeviceType>>> {
            self.firmware_by_id
                .as_ref()
                .expect("firmware_by_id called but not configured on MockStore")(id)
        }

        async fn insert_firmware(&self, _: NewFirmware<Self::DeviceType>) -> Result<()> {
            unimplemented!()
        }
        async fn get_firmwares(
            &self,
            _: &Self::DeviceType,
        ) -> Result<Vec<Firmware<Self::DeviceType>>> {
            unimplemented!()
        }
        async fn remove_firmware(&self, _: &DataStoreId) -> Result<()> {
            unimplemented!()
        }
        async fn recommend_firmware(&self, _: &DataStoreId) -> Result<()> {
            unimplemented!()
        }
        async fn unrecommend_other_firmwares_for_same_device_type(
            &self,
            _: &DataStoreId,
        ) -> Result<()> {
            unimplemented!()
        }
    }
}

use chrono::{DateTime, Utc};
use demo_protocol::{
    data::std::Data,
    settings::{DataRate, Settings},
    status::{DeviceStatus as ProtocolDeviceStatus, Status as BaseStatus},
    DeviceType, EventPayload,
};
use diesel::prelude::*;
use sensor_link_server_core::{
    device::{Command, Device as DomainDevice},
    sensor_server_log::SensorServerLog,
    DataStoreId,
};
use serde::{de::DeserializeOwned, Serialize};

use super::schema::{
    device_events, device_settings, device_status, devices, firmware, measurement_data,
    sensor_server_log,
};
use crate::{
    device::{Firmware, NewFirmware},
    device_status::DeviceStatus,
};

/// Row model for the `device_status` time-series table, used for both reads and
/// inserts.
///
/// The `devices` table holds no status; the latest row here is what fills in a
/// device's live status when reading a device back (see the joins in
/// `TimescaleDb::device_by_id` / `get_devices_simple`). Column order mirrors the
/// [`device_status`](super::schema::device_status) schema.
#[derive(Debug, Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = device_status)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub(crate) struct DeviceStatusRow {
    pub time: DateTime<Utc>,
    pub device_id: String,
    pub settings_hash: i64,
    /// The *base* protocol status (ground truth); the extended `Status` is not
    /// stored, only re-derived (see the `From` impls below).
    #[diesel(serialize_as = String, deserialize_as = String)]
    pub status: DbText<BaseStatus>,
    pub signal_strength: i32,
    pub temperature: f32,
}

impl From<DeviceStatus> for DeviceStatusRow {
    /// Build an insertable row from a status, stamped with the current time.
    ///
    /// A [`DeviceStatus`] carries no device id, so `device_id` is left empty for
    /// the caller to fill in (typically via struct-update syntax). Only the base
    /// protocol status is persisted (as its serde name) — the derived extended
    /// `Status` is *not* stored (it is `#[serde(skip)]` on [`DeviceStatus`]) and
    /// gets re-derived by the reverse [`From<DeviceStatusRow>`]. Channel-status
    /// fields have no columns and are dropped.
    fn from(status: DeviceStatus) -> Self {
        Self {
            time: Utc::now(),
            device_id: String::new(),
            settings_hash: status.inner.settings_hash as i64,
            status: DbText(status.inner.status),
            signal_strength: status.inner.signal_strength,
            temperature: status.inner.temperature,
        }
    }
}

impl From<DeviceStatusRow> for DeviceStatus {
    /// Reconstruct the server-side [`DeviceStatus`] from a persisted row.
    ///
    /// The row stores the *base* protocol status (ground truth); rebuild a
    /// [`ProtocolDeviceStatus`] from the columns and let [`DeviceStatus::from`]
    /// re-derive the extended `Status`, keeping that derivation the single source
    /// of truth (channel-status fields aren't persisted, so they default).
    fn from(row: DeviceStatusRow) -> Self {
        DeviceStatus::from(ProtocolDeviceStatus {
            settings_hash: row.settings_hash as u64,
            status: row.status.0,
            signal_strength: row.signal_strength,
            temperature: row.temperature,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_status::Status;

    #[test]
    fn device_status_row_persists_base_and_rederives_extended() {
        let status = DeviceStatus::from(ProtocolDeviceStatus {
            status: BaseStatus::Idle,
            settings_hash: 7,
            signal_strength: -42,
            temperature: 21.5,
            ..Default::default()
        });

        let row: DeviceStatusRow = status.into();
        // The base protocol status is stored as its serde name, not the extended one.
        let stored: String = row.status.clone().into();
        assert_eq!(stored, "idle");

        let back = DeviceStatus::from(row);
        assert_eq!(back.inner.status, BaseStatus::Idle);
        assert_eq!(back.inner.settings_hash, 7);
        assert_eq!(back.inner.signal_strength, -42);
        assert_eq!(back.inner.temperature, 21.5);
        // Extended status is re-derived from the base, not read from storage.
        assert_eq!(back.status, Status::Idle);
    }

    /// Payload of `n_samples` samples, one second apart from t = 1 s, whose
    /// channels are filled with a distinct value per channel and sample.
    fn data_with(n_samples: usize) -> Data {
        let channel = |offset: f32| (0..n_samples).map(|i| offset + i as f32).collect();
        Data {
            t: (0..n_samples).map(|i| (i as i64 + 1) * 1_000_000).collect(),
            fs: 1.0,
            board_temperature: channel(10.0),
            vbus: channel(20.0),
            ibus: channel(30.0),
            vsys: channel(40.0),
        }
    }

    #[test]
    fn measurement_rows_fan_out_one_row_per_sample() {
        let rows = MeasurementDataRow::rows_from_data("dev-1", &data_with(2));

        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[1],
            MeasurementDataRow {
                time: DateTime::from_timestamp_micros(2_000_000).unwrap(),
                device_id: "dev-1".to_string(),
                fs: 1.0,
                board_temperature: Some(11.0),
                vbus: Some(21.0),
                ibus: Some(31.0),
                vsys: Some(41.0),
            }
        );
    }

    #[test]
    fn measurement_rows_skip_samples_a_channel_is_short_of() {
        // A malformed payload: `vsys` stops one sample early, so the last
        // sample has no complete row and is dropped rather than filled in.
        let mut data = data_with(3);
        data.vsys.pop();

        let rows = MeasurementDataRow::rows_from_data("dev-1", &data);

        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows.last().unwrap().time,
            DateTime::from_timestamp_micros(2_000_000).unwrap()
        );
    }

    #[test]
    fn measurement_rows_store_channels_without_data_as_null() {
        // A device sends a non-finite value for a channel it has no
        // measurement for; that is stored as NULL, not as NaN.
        let mut data = data_with(1);
        data.vbus = vec![f32::NAN];
        data.ibus = vec![f32::INFINITY];

        let rows = MeasurementDataRow::rows_from_data("dev-1", &data);

        assert_eq!(rows[0].board_temperature, Some(10.0));
        assert_eq!(rows[0].vbus, None);
        assert_eq!(rows[0].ibus, None);
    }

    #[test]
    fn unknown_stored_status_decodes_to_default() {
        let decoded: DbText<BaseStatus> = "not-a-status".to_owned().into();
        assert_eq!(decoded.0, BaseStatus::Unknown);
    }
}

/// Row model for the `devices` table (one row per device, current/latest state).
///
/// Mirrors the [`devices`](super::schema::devices) schema. The `Text`-backed
/// enums go through the generic [`DbText`] proxy via
/// `serialize_as`/`deserialize_as = String`: diesel maps the column as a plain
/// `String`, and the proxy encodes the enum via its `serde` derives. Conversion
/// to the domain `Device` lives in the [`From`] impl below.
#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = devices)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub(crate) struct Device {
    pub id: String,
    pub name: String,
    #[diesel(serialize_as = String, deserialize_as = String)]
    pub device_type: DbText<DeviceType>,
    pub last_contact: Option<i64>,
    pub version: Option<serde_json::Value>,
    /// Id of the firmware this device is marked to update to (a [`firmware`]
    /// row's `id`, resolved via `FirmwareStore::firmware_by_id`). Plain `TEXT`
    /// copy of that id with no FK enforcement.
    ///
    /// [`firmware`]: super::schema::firmware
    pub marked_for_update: Option<DataStoreId>,
    #[diesel(serialize_as = String, deserialize_as = String)]
    pub command: DbText<Command>,
    pub config_confirmed: bool,
    pub config_last_sent_at: Option<i64>,
    pub status_since: i64,
    pub register_time: i64,
    pub sim_iccid: String,
    pub online_since: Option<DateTime<Utc>>,
}

impl Device {
    /// Build a freshly-registered device row of the given `device_type`, with
    /// all other fields at their defaults.
    pub fn new_for_type(id: String, device_type: DeviceType, register_time: i64) -> Self {
        Self {
            id,
            name: String::new(),
            device_type: DbText(device_type),
            last_contact: None,
            version: None,
            marked_for_update: None,
            command: DbText(Command::default()),
            config_confirmed: false,
            config_last_sent_at: None,
            status_since: register_time,
            register_time,
            sim_iccid: String::new(),
            online_since: None,
        }
    }
}

impl From<Device> for DomainDevice<DeviceType, DeviceStatus> {
    /// Convert a `devices` row into the domain [`Device`](DomainDevice). Fields
    /// with no column in this schema (groups, measuring-point/baseline data,
    /// the sync interval, live status, …) are left at their defaults.
    fn from(row: Device) -> Self {
        Self {
            id: row.id,
            name: row.name,
            device_type: row.device_type.0,
            last_contact: row.last_contact,
            version: row.version.and_then(|v| serde_json::from_value(v).ok()),
            marked_for_update: row.marked_for_update,
            command: row.command.0,
            config_confirmed: row.config_confirmed,
            config_last_sent_at: row.config_last_sent_at,
            status_since: row.status_since,
            register_time: row.register_time,
            sim_iccid: row.sim_iccid,
            online_since: row.online_since,
            ..Default::default()
        }
    }
}

/// Joined read model: a `devices` row plus its latest `device_status` row (if
/// any), as produced by the device reads' left join.
///
/// Both sides are `#[diesel(embed)]`ed so the query loads this composite in one
/// step (`DeviceWithStatus::as_select()`); the [`From`] impl then folds it into
/// the domain [`Device`](DomainDevice), so callers never touch the join tuple.
#[derive(Debug, Clone, Queryable, Selectable)]
pub(crate) struct DeviceWithStatus {
    #[diesel(embed)]
    pub device: Device,
    #[diesel(embed)]
    pub status: Option<DeviceStatusRow>,
}

impl From<DeviceWithStatus> for DomainDevice<DeviceType, DeviceStatus> {
    fn from(joined: DeviceWithStatus) -> Self {
        let mut device = DomainDevice::from(joined.device);
        if let Some(status_row) = joined.status {
            device.device_status = status_row.into();
        }
        device
    }
}

/// Read model for the `device_settings` table (one row per device, holding its
/// current operational settings).
///
/// Mirrors the [`device_settings`](super::schema::device_settings) schema, which
/// flattens [`Settings`]: `data_rate` goes through the same [`DbText`] `String`
/// proxy as the other enum-valued columns. The settings `hash` has no column: it
/// is derived from the settings and is calculated dynamically
/// ([`Settings::calculate_hash`]) rather than stored. Read-only: settings are
/// written by whatever owns the device configuration, not from this model.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = device_settings)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub(crate) struct DeviceSettingsRow {
    /// FK into `devices`; selected with the row but not carried into
    /// [`Settings`], hence unread until a caller needs it.
    #[allow(dead_code)]
    pub device_id: String,
    #[diesel(deserialize_as = String)]
    pub data_rate: DbText<DataRate>,
    pub sync_interval_s: i32,
}

impl From<DeviceSettingsRow> for Settings {
    /// Rebuild the protocol [`Settings`] from a persisted row. The `device_id`
    /// has no counterpart in [`Settings`] and is dropped; `hash` is left at `0`
    /// for the caller to fill in via [`Settings::calculate_hash`], which needs a
    /// nonce this row does not carry.
    fn from(row: DeviceSettingsRow) -> Self {
        Self {
            data_rate: row.data_rate.0,
            sync_interval_s: row.sync_interval_s,
            hash: 0,
        }
    }
}

/// Generic `Text`/`String` proxy for any type whose `serde` derives encode it as
/// a JSON *string* (the usual case for unit enums, e.g. [`DeviceType`],
/// [`Command`], or [`Status`](demo_protocol::status::Status)).
///
/// This reuses a type's existing `Serialize`/`Deserialize` instead of requiring
/// a bespoke `Display`/`FromStr` (or an upstream change) just to persist it: use
/// it via `#[diesel(serialize_as = String, deserialize_as = String)]` on a field
/// of type `DbText<T>`. An unrepresentable or unknown value decodes to
/// `T::default()`.
#[derive(Debug, Clone)]
pub(crate) struct DbText<T>(pub(crate) T);

impl<T: Serialize> From<DbText<T>> for String {
    fn from(value: DbText<T>) -> Self {
        match serde_json::to_value(&value.0) {
            Ok(serde_json::Value::String(name)) => name,
            _ => String::new(),
        }
    }
}

impl<T: DeserializeOwned + Default> From<String> for DbText<T> {
    fn from(value: String) -> Self {
        DbText(serde_json::from_value(serde_json::Value::String(value)).unwrap_or_default())
    }
}

/// Row model for the `firmware` table (one row per firmware image).
///
/// Mirrors the [`firmware`](super::schema::firmware) schema. `device_type` goes
/// through the same [`DbText`] `String` proxy as [`Device`]. The domain
/// [`Firmware`] renames two fields (`id` ⇒ `_id`, `v2_bin_id` ⇒ `v2BinID`); the
/// [`From`] impl below maps the columns onto them.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = firmware)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub(crate) struct FirmwareRow {
    pub id: String,
    pub version: String,
    pub description: String,
    pub date: DateTime<Utc>,
    pub v2_bin_id: String,
    pub recommended: bool,
    #[diesel(deserialize_as = String)]
    pub device_type: DbText<DeviceType>,
}

impl From<FirmwareRow> for Firmware {
    fn from(row: FirmwareRow) -> Self {
        Self {
            id: row.id,
            version: row.version,
            description: row.description,
            date: row.date,
            v2BinID: row.v2_bin_id,
            recommended: row.recommended,
            device_type: row.device_type.0,
        }
    }
}

/// Insertable model for the `firmware` table.
///
/// `id` is intentionally absent so Postgres fills it from the column `DEFAULT`
/// (`gen_random_uuid()::text`); add `.returning(firmware::id)` at the call site
/// if the generated id is needed.
#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = firmware)]
pub(crate) struct NewFirmwareRow {
    pub version: String,
    pub description: String,
    pub date: DateTime<Utc>,
    pub v2_bin_id: String,
    pub recommended: bool,
    #[diesel(serialize_as = String)]
    pub device_type: DbText<DeviceType>,
}

impl From<NewFirmware> for NewFirmwareRow {
    fn from(fw: NewFirmware) -> Self {
        Self {
            version: fw.version,
            description: fw.description,
            date: fw.date,
            v2_bin_id: fw.v2BinID,
            recommended: fw.recommended,
            device_type: DbText(fw.device_type),
        }
    }
}

/// Row model for the `measurement_data` time-series table, used for both reads
/// and inserts.
///
/// Mirrors the [`measurement_data`](super::schema::measurement_data) schema: one
/// row per *sample*, with a column per measurement, whereas the protocol's
/// [`Data`] carries a whole set of samples in column format. Use
/// [`rows_from_data`](Self::rows_from_data) to fan a received payload out into
/// rows.
#[derive(Debug, Clone, PartialEq, Queryable, Selectable, Insertable)]
#[diesel(table_name = measurement_data)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub(crate) struct MeasurementDataRow {
    pub time: DateTime<Utc>,
    pub device_id: String,
    /// Sampling frequency (Hz) of the set this sample came from; the same value
    /// repeats on every row of that set.
    pub fs: f32,
    /// Channel values, see [`demo_protocol::DemoChannel`]. `None` for a
    /// channel the device had no measurement for: the payload carries those as
    /// NaN, which would poison any aggregate over the column.
    pub board_temperature: Option<f32>,
    pub vbus: Option<f32>,
    pub ibus: Option<f32>,
    pub vsys: Option<f32>,
}

/// A channel value as stored: non-finite (NaN, +/-inf) means "no measurement".
fn measured(value: f32) -> Option<f32> {
    value.is_finite().then_some(value)
}

impl MeasurementDataRow {
    /// Fan a received [`Data`] payload out into one row per sample.
    ///
    /// The payload's timestamps are microseconds since the epoch. A sample is
    /// skipped when its timestamp is not a representable date, or when any
    /// channel is short of it: a payload whose channels disagree in length is
    /// malformed, and a partially-filled row would be indistinguishable from a
    /// real measurement. Callers can spot this by comparing the row count to
    /// `data.t.len()`.
    pub fn rows_from_data(device_id: &str, data: &Data) -> Vec<Self> {
        data.t
            .iter()
            .enumerate()
            .filter_map(|(i, &t_us)| {
                Some(Self {
                    time: DateTime::from_timestamp_micros(t_us)?,
                    device_id: device_id.to_string(),
                    fs: data.fs,
                    board_temperature: measured(*data.board_temperature.get(i)?),
                    vbus: measured(*data.vbus.get(i)?),
                    ibus: measured(*data.ibus.get(i)?),
                    vsys: measured(*data.vsys.get(i)?),
                })
            })
            .collect()
    }
}

/// Row model for the `sensor_server_log` time-series table, used for both reads
/// and inserts.
///
/// Mirrors the [`sensor_server_log`](super::schema::sensor_server_log) schema.
/// The domain [`SensorServerLog`] carries a `group_id` and `duration` that this
/// schema deliberately drops (see the `From` impls below); on read-back they are
/// filled with their empty/default values, and `id` (a MongoDB-only artifact) is
/// left `None`.
#[derive(Debug, Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = sensor_server_log)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub(crate) struct SensorServerLogRow {
    pub time: DateTime<Utc>,
    pub sensor_id: String,
    pub user_id: String,
    pub log_type: String,
    pub header: String,
    pub body: Vec<String>,
}

impl From<SensorServerLog> for SensorServerLogRow {
    /// Build an insertable row from a log. The `group_id` and `duration` fields
    /// have no column in this schema and are dropped; `id` is server-managed and
    /// not persisted here.
    fn from(log: SensorServerLog) -> Self {
        Self {
            time: log.timestamp,
            sensor_id: log.sensor_id,
            user_id: log.user_id,
            log_type: log._type,
            header: log.header,
            body: log.body,
        }
    }
}

impl From<SensorServerLogRow> for SensorServerLog {
    /// Reconstruct the domain [`SensorServerLog`] from a persisted row. Fields
    /// with no column in this schema are restored to their defaults: `id` is
    /// `None`, `group_id` is `None`, and `duration` is `0`.
    fn from(row: SensorServerLogRow) -> Self {
        Self {
            id: None,
            sensor_id: row.sensor_id,
            group_id: None,
            user_id: row.user_id,
            timestamp: row.time,
            duration: 0,
            _type: row.log_type,
            header: row.header,
            body: row.body,
        }
    }
}

/// Row model for the `device_events` table: one row per received event.
#[derive(Debug, Clone, PartialEq, Queryable, Selectable, Insertable)]
#[diesel(table_name = device_events)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub(crate) struct DeviceEventRow {
    pub time: DateTime<Utc>,
    pub device_id: String,
    pub event: String,
    pub message: Option<String>,
    pub received_at: DateTime<Utc>,
}

impl DeviceEventRow {
    /// Build a row from a received event, stamped as received now.
    ///
    /// Name and message are taken from the event's JSON form, so every variant
    /// is stored without a per-variant mapping: `"blink"` has no message, an
    /// object such as `{"sensorerror":{"m":"..."}}` has `m`, and a string
    /// value is the message itself. `None` when the timestamp is not a
    /// representable date.
    pub fn from_payload(device_id: &str, payload: &EventPayload) -> Option<Self> {
        let (event, message) = match serde_json::to_value(&payload.event).ok()? {
            serde_json::Value::String(name) => (name, None),
            serde_json::Value::Object(map) => {
                let (name, value) = map.into_iter().next()?;
                let message = match value {
                    serde_json::Value::String(s) => Some(s),
                    serde_json::Value::Object(fields) => {
                        fields.get("m").and_then(|m| m.as_str()).map(str::to_owned)
                    }
                    _ => None,
                };
                (name, message)
            }
            _ => return None,
        };
        Some(Self {
            time: DateTime::from_timestamp_millis(payload.ts)?,
            device_id: device_id.to_owned(),
            event,
            message,
            received_at: Utc::now(),
        })
    }
}

#[cfg(test)]
mod device_event_tests {
    use demo_protocol::event::{CommonEvent, Event};

    use super::*;

    fn row(event: Event) -> DeviceEventRow {
        let payload = EventPayload {
            event,
            ts: 1_727_000_000_000,
        };
        DeviceEventRow::from_payload("DEMO-0002", &payload).unwrap()
    }

    #[test]
    fn demo_event_keeps_its_wire_name() {
        let row = row(Event::ButtonPressed);
        assert_eq!(row.event, "button_pressed");
        assert_eq!(row.message, None);
        assert_eq!(row.device_id, "DEMO-0002");
        assert_eq!(row.time.timestamp_millis(), 1_727_000_000_000);
    }

    #[test]
    fn common_events_keep_their_wire_name() {
        let booted = row(Event::booted("PowerOn"));
        assert_eq!(booted.event, "booted");
        assert_eq!(booted.message.as_deref(), Some("PowerOn"));

        let blink = row(Event::Common(CommonEvent::Blink));
        assert_eq!(blink.event, "blink");
        assert_eq!(blink.message, None);
    }

    #[test]
    fn sensor_error_stores_its_message() {
        let row = row(Event::sensor_error("Charger I2C"));
        assert_eq!(row.event, "sensorerror");
        assert_eq!(row.message.as_deref(), Some("Charger I2C"));
    }
}

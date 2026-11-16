//! TimescaleDB connection pool (diesel-async + deadpool) and health-check task.
//!
//! TLS is a config-only difference: libpq-style `sslmode`/`sslrootcert`/
//! `sslcert`/`sslkey` query params in TIMESCALEDB_URL select between NoTls
//! and a rustls connector, on a shared code path.

pub mod models;
pub mod schema;

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use chrono::Utc;
use demo_protocol::{data::std::Data, settings::Settings, DeviceType, EventPayload};
use diesel::{ExpressionMethods, JoinOnDsl, OptionalExtension, QueryDsl, SelectableHelper};
use diesel_async::{
    async_connection_wrapper::AsyncConnectionWrapper,
    pooled_connection::{deadpool::Pool, AsyncDieselConnectionManager, ManagerConfig},
    AsyncPgConnection, RunQueryDsl,
};
use diesel_migrations::MigrationHarness;
use sensor_link_server_core::{
    device::{Command, Device, DeviceExt, DeviceFieldType, DeviceQuery},
    event::{Comment, EventQuery, EventType, EventsWithStats, NewEvent, SendStatus},
    sensor_server_log::SensorServerLog,
    store_traits::{
        DataStoreError, DeviceStore, EventStore, FirmwareStore, Result, TransactionDataStore,
    },
    DataStoreId,
};
use task_supervisor::{get_crate_relative_function_path, Handle, PanicCallback};
use tokio::sync::watch;

use crate::{
    db::schema::{
        device_events::dsl as de, device_settings::dsl as dsett, device_status::dsl as ds,
        devices::dsl as d, firmware::dsl as f, measurement_data::dsl as md,
        sensor_server_log::dsl as ssl,
    },
    device::{Firmware, NewFirmware},
    device_status::DeviceStatus,
    DeviceEventStore, DeviceSettingsStore, MeasurementDataStore,
};

pub type TimescalePool = Pool<AsyncPgConnection>;

pub const MIGRATIONS: diesel_migrations::EmbeddedMigrations =
    diesel_migrations::embed_migrations!("migrations");

/// `devices.command` value that asks the device to blink its LED, see
/// [`DeviceSettingsStore::take_blink_request`].
const BLINK_COMMAND: &str = "Blink";

const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(10);

/// Rows per `measurement_data` INSERT. A row binds 9 parameters, so this stays
/// an order of magnitude below Postgres' 65535-parameter limit per statement.
const MEASUREMENT_INSERT_CHUNK: usize = 4096;

enum TimescaleTls {
    NoTls,
    Rustls(tokio_postgres_rustls::MakeRustlsConnect),
}

struct TimescaleConfig {
    pg_config: tokio_postgres::Config,
    tls: TimescaleTls,
}

impl TimescaleConfig {
    fn from_url(raw_url: &str) -> anyhow::Result<Self> {
        use rustls::{
            pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer},
            ClientConfig, RootCertStore,
        };
        use std::sync::Once;
        use tokio_postgres_rustls::MakeRustlsConnect;
        use url::Url;

        static CRYPTO_INIT: Once = Once::new();
        CRYPTO_INIT.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });

        let mut url = Url::parse(raw_url)?;

        let mut sslcert: Option<String> = None;
        let mut sslkey: Option<String> = None;
        let mut sslrootcert: Option<String> = None;
        let mut sslmode: Option<String> = None;
        let kept: Vec<(String, String)> = url
            .query_pairs()
            .filter_map(|(k, v)| match k.as_ref() {
                "sslcert" => {
                    sslcert = Some(v.into_owned());
                    None
                }
                "sslkey" => {
                    sslkey = Some(v.into_owned());
                    None
                }
                "sslrootcert" => {
                    sslrootcert = Some(v.into_owned());
                    None
                }
                "sslmode" => {
                    sslmode = Some(v.into_owned());
                    None
                }
                _ => Some((k.into_owned(), v.into_owned())),
            })
            .collect();

        url.set_query(None);
        if !kept.is_empty() {
            let mut qp = url.query_pairs_mut();
            for (k, v) in &kept {
                qp.append_pair(k, v);
            }
        }

        let mut pg_config: tokio_postgres::Config = url.as_str().parse()?;

        let want_disable = sslmode.as_deref() == Some("disable");
        let need_tls =
            !want_disable && (sslrootcert.is_some() || sslcert.is_some() || sslmode.is_some());

        let tls = if need_tls {
            pg_config.ssl_mode(tokio_postgres::config::SslMode::Require);

            let mut roots = RootCertStore::empty();
            if let Some(ca_path) = sslrootcert.as_deref() {
                for cert in CertificateDer::pem_file_iter(ca_path)? {
                    roots.add(cert?)?;
                }
            }
            let builder = ClientConfig::builder().with_root_certificates(roots);
            let client_config = match (sslcert.as_deref(), sslkey.as_deref()) {
                (Some(cert_path), Some(key_path)) => {
                    let chain: Vec<CertificateDer<'static>> =
                        CertificateDer::pem_file_iter(cert_path)?
                            .collect::<std::result::Result<_, _>>()?;
                    let key = PrivateKeyDer::from_pem_file(key_path)?;
                    builder.with_client_auth_cert(chain, key)?
                }
                _ => builder.with_no_client_auth(),
            };
            TimescaleTls::Rustls(MakeRustlsConnect::new(client_config))
        } else {
            pg_config.ssl_mode(tokio_postgres::config::SslMode::Disable);
            TimescaleTls::NoTls
        };

        Ok(Self { pg_config, tls })
    }

    async fn establish(&self) -> diesel::ConnectionResult<AsyncPgConnection> {
        use diesel::ConnectionError;

        let client = match &self.tls {
            TimescaleTls::Rustls(connector) => {
                let (client, conn) = self
                    .pg_config
                    .connect(connector.clone())
                    .await
                    .map_err(|e| ConnectionError::BadConnection(e.to_string()))?;
                tokio::spawn(async move {
                    if let Err(e) = conn.await {
                        tracing::debug!("TimescaleDB connection task ended: {e}");
                    }
                });
                client
            }
            TimescaleTls::NoTls => {
                let (client, conn) = self
                    .pg_config
                    .connect(tokio_postgres::NoTls)
                    .await
                    .map_err(|e| ConnectionError::BadConnection(e.to_string()))?;
                tokio::spawn(async move {
                    if let Err(e) = conn.await {
                        tracing::debug!("TimescaleDB connection task ended: {e}");
                    }
                });
                client
            }
        };
        AsyncPgConnection::try_from(client).await
    }
}

pub async fn connect_timescaledb() -> Option<TimescalePool> {
    let url = match std::env::var("TIMESCALEDB_URL") {
        Ok(url) => url,
        Err(_) => {
            tracing::error!("TIMESCALEDB_URL not set, skipping TimescaleDB connection");
            return None;
        }
    };

    let cfg = match TimescaleConfig::from_url(&url) {
        Ok(c) => Arc::new(c),
        Err(err) => {
            tracing::error!("Failed to parse TIMESCALEDB_URL: {err}");
            return None;
        }
    };

    let mut mgr_config = ManagerConfig::<AsyncPgConnection>::default();
    mgr_config.custom_setup = Box::new({
        let cfg = cfg.clone();
        move |_| {
            let cfg = cfg.clone();
            Box::pin(async move { cfg.establish().await })
        }
    });
    let manager =
        AsyncDieselConnectionManager::<AsyncPgConnection>::new_with_config(url, mgr_config);
    let max_size = std::env::var("TIMESCALEDB_POOL_SIZE")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(10);
    let pool = match Pool::builder(manager).max_size(max_size).build() {
        Ok(pool) => pool,
        Err(err) => {
            tracing::error!("Failed to build TimescaleDB pool: {err}");
            return None;
        }
    };

    let migration_conn = match cfg.establish().await {
        Ok(c) => c,
        Err(err) => {
            tracing::error!("Failed to connect for TimescaleDB migrations: {err}");
            return None;
        }
    };

    let skip_migrations = std::env::var("SKIP_MIGRATIONS").as_deref() == Ok("true");
    if !skip_migrations {
        let migration_result = tokio::task::spawn_blocking(move || {
            let mut wrapped: AsyncConnectionWrapper<AsyncPgConnection> =
                AsyncConnectionWrapper::from(migration_conn);
            wrapped
                .run_pending_migrations(MIGRATIONS)
                .map(|_| ())
                .map_err(|err| format!("run migrations: {err}"))
        })
        .await;

        match migration_result {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tracing::error!("Failed to run TimescaleDB migrations: {err}");
                return None;
            }
            Err(err) => {
                tracing::error!("Migration task panicked: {err}");
                return None;
            }
        }
    }

    tracing::info!("Connected to TimescaleDB");
    Some(pool)
}

/// Periodically runs `SELECT 1` on the pool and reports the result on `status_tx`.
pub fn start_health_check_task(
    pool: TimescalePool,
    status_tx: watch::Sender<bool>,
    on_panic: PanicCallback,
) -> Handle {
    let task_function = health_check_task;
    Handle::new(
        move |shutdown_rx| task_function(shutdown_rx, pool.clone(), status_tx.clone()),
        get_crate_relative_function_path(task_function),
        on_panic,
    )
}

async fn health_check_task(
    mut shutdown_rx: watch::Receiver<()>,
    pool: TimescalePool,
    status_tx: watch::Sender<bool>,
) {
    let mut interval = tokio::time::interval(HEALTH_CHECK_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => break,
            _ = interval.tick() => {
                let healthy = check_health(&pool).await;
                if healthy != *status_tx.borrow() {
                    match healthy {
                        true => tracing::info!("TimescaleDB connection OK"),
                        false => tracing::warn!("TimescaleDB connection lost"),
                    }
                }
                let _ = status_tx.send(healthy);
            }
        }
    }
}

async fn check_health(pool: &TimescalePool) -> bool {
    let mut conn = match pool.get().await {
        Ok(conn) => conn,
        Err(err) => {
            tracing::debug!("TimescaleDB health check: pool.get failed: {err}");
            return false;
        }
    };
    match diesel::sql_query("SELECT 1").execute(&mut conn).await {
        Ok(_) => true,
        Err(err) => {
            tracing::debug!("TimescaleDB health check: SELECT 1 failed: {err}");
            false
        }
    }
}

#[derive(Clone)]
pub struct TimescaleDb {
    pub pool: TimescalePool,
}

impl TimescaleDb {
    pub fn new(pool: TimescalePool) -> Self {
        Self { pool }
    }

    /// Update a single column (selected by `value`) for every device in `device_ids`.
    ///
    /// [`DeviceFieldType::DeviceStatus`] is special: the `devices` table holds no
    /// status, so it inserts a new row into the `device_status` time-series table
    /// (one per device) rather than updating a column. Device fields that have no
    /// corresponding column in the `devices` table (groups, measuring-point and
    /// baseline bookkeeping, the sync interval, …) are logged and fail with an
    /// error.
    async fn update_field(
        &self,
        device_ids: &[String],
        value: DeviceFieldType<DeviceType, DeviceStatus>,
    ) -> Result<()> {
        if device_ids.is_empty() {
            return Ok(());
        }

        let mut conn = self.pool.get().await.map_err(db_err)?;
        let devices = d::devices.filter(d::id.eq_any(device_ids.to_vec()));

        let written = match value {
            DeviceFieldType::DeviceType(dt) => {
                diesel::update(devices)
                    .set(d::device_type.eq(String::from(models::DbText(dt))))
                    .execute(&mut conn)
                    .await
            }
            DeviceFieldType::Name(name) => {
                diesel::update(devices)
                    .set(d::name.eq(name))
                    .execute(&mut conn)
                    .await
            }
            DeviceFieldType::LastContact(v) => {
                diesel::update(devices)
                    .set(d::last_contact.eq(v))
                    .execute(&mut conn)
                    .await
            }
            DeviceFieldType::Version(v) => {
                let json = v.map(serde_json::to_value).transpose().map_err(write_err)?;
                diesel::update(devices)
                    .set(d::version.eq(json))
                    .execute(&mut conn)
                    .await
            }
            DeviceFieldType::MarkedForUpdate(v) => {
                diesel::update(devices)
                    .set(d::marked_for_update.eq(v))
                    .execute(&mut conn)
                    .await
            }
            // The serde name ("Start"), which the row model reads back;
            // `Command`'s `Display` gives a lowercase spelling it would not.
            DeviceFieldType::Command(c) => {
                diesel::update(devices)
                    .set(d::command.eq(String::from(models::DbText(c))))
                    .execute(&mut conn)
                    .await
            }
            DeviceFieldType::ConfigConfirmed(b) => {
                diesel::update(devices)
                    .set(d::config_confirmed.eq(b))
                    .execute(&mut conn)
                    .await
            }
            DeviceFieldType::ConfigLastSentAt(v) => {
                diesel::update(devices)
                    .set(d::config_last_sent_at.eq(v))
                    .execute(&mut conn)
                    .await
            }
            DeviceFieldType::StatusSince(t) => {
                diesel::update(devices)
                    .set(d::status_since.eq(t))
                    .execute(&mut conn)
                    .await
            }
            DeviceFieldType::RegisterTime(t) => {
                diesel::update(devices)
                    .set(d::register_time.eq(t))
                    .execute(&mut conn)
                    .await
            }
            DeviceFieldType::SimIccid(s) => {
                diesel::update(devices)
                    .set(d::sim_iccid.eq(s))
                    .execute(&mut conn)
                    .await
            }
            DeviceFieldType::OnlineSince(v) => {
                diesel::update(devices)
                    .set(d::online_since.eq(v))
                    .execute(&mut conn)
                    .await
            }
            DeviceFieldType::DeviceStatus(status) => {
                // The `devices` table holds no status; append a row per device to
                // the `device_status` time-series table instead. Built by value:
                // the `status` column's `serialize_as` proxy consumes each row
                // rather than borrowing it.
                let rows: Vec<models::DeviceStatusRow> = device_ids
                    .iter()
                    .map(|id| models::DeviceStatusRow {
                        device_id: id.clone(),
                        ..status.clone().into()
                    })
                    .collect();
                diesel::insert_into(ds::device_status)
                    .values(rows)
                    .execute(&mut conn)
                    .await
            }
            // The configured interval lives in `device_settings.sync_interval_s`;
            // a second copy here could only drift from it.
            DeviceFieldType::SyncIntervalMin(_) => {
                tracing::error!(
                    "Sync interval is not stored per device row, devices {:?}",
                    device_ids
                );
                return Err(DataStoreError::Database(
                    "Sync interval is not stored on the device row, see device_settings"
                        .to_string(),
                ));
            }
            other => {
                tracing::error!(
                    "[Not implemented!] Setting unsupported device field '{}' for devices {:?}",
                    other.field_name(),
                    device_ids
                );
                return Err(DataStoreError::Database(format!(
                    "Setting unsupported device field '{}'",
                    other.field_name()
                )));
            }
        };
        written.map_err(write_err)?;

        Ok(())
    }

    /// Set a single field on one device. Shared by the [`DeviceStore`] and
    /// [`TransactionDataStore`] trait implementations, which must call this
    /// inherent method explicitly (e.g. `TimescaleDb::set_device_field(self, …)`)
    /// to avoid resolving back to the same-named trait method (self-recursion).
    async fn set_device_field(
        &self,
        device_id: &str,
        value: DeviceFieldType<DeviceType, DeviceStatus>,
    ) -> Result<()> {
        self.update_field(&[device_id.to_string()], value).await
    }

    /// Groups have no representation in the `devices` schema, so this is a
    /// logged no-op. Shared by both trait implementations; call explicitly
    /// (e.g. `TimescaleDb::set_device_field_for_groups(self, …)`) to avoid
    /// self-recursion into the same-named trait method.
    async fn set_device_field_for_groups(
        &self,
        device_id: &str,
        group_ids: Vec<String>,
        _value: DeviceFieldType<DeviceType, DeviceStatus>,
    ) -> Result<()> {
        // Not applicable for this server application, so do nothing and report error as it should never be used.
        tracing::error!(
            "[Not implemented!] Setting device field for device {} for groups {:?} \
             (device groups are not supported by this schema)",
            device_id,
            group_ids
        );
        Err(DataStoreError::Database(
            "Device groups are not supported by this schema".to_string(),
        ))
    }

    /// Insert a new event. Shared by the [`EventStore`] and
    /// [`TransactionDataStore`] trait implementations, which must call this
    /// inherent method explicitly (e.g. `TimescaleDb::insert_event(self, …)`)
    /// to avoid resolving back to the same-named trait method (self-recursion).
    fn insert_event(&self, event: NewEvent<(), ()>) -> Result<DataStoreId> {
        tracing::error!("[Not implemented!] Inserting new event: {:?}", event);
        Ok(DataStoreId::new())
    }
}

fn db_err(err: impl std::fmt::Display) -> DataStoreError {
    DataStoreError::Database(err.to_string())
}

fn read_err(err: impl std::fmt::Display) -> DataStoreError {
    DataStoreError::DatabaseReadFailed(err.to_string())
}

fn write_err(err: impl std::fmt::Display) -> DataStoreError {
    DataStoreError::DatabaseWriteFailed(err.to_string())
}

#[async_trait]
impl TransactionDataStore for TimescaleDb {
    type ContactData = ();
    type EventData = ();
    type DeviceType = DeviceType;
    type DeviceStatus = DeviceStatus;

    async fn set_device_field(
        &mut self,
        device_id: &str,
        value: DeviceFieldType<Self::DeviceType, Self::DeviceStatus>,
    ) -> Result<()> {
        TimescaleDb::set_device_field(self, device_id, value).await
    }

    async fn set_device_field_for_groups(
        &mut self,
        device_id: &str,
        group_ids: Vec<String>,
        value: DeviceFieldType<Self::DeviceType, Self::DeviceStatus>,
    ) -> Result<()> {
        TimescaleDb::set_device_field_for_groups(self, device_id, group_ids, value).await
    }

    async fn insert_event(
        &mut self,
        event: NewEvent<Self::ContactData, Self::EventData>,
    ) -> Result<DataStoreId> {
        TimescaleDb::insert_event(self, event)
    }

    async fn commit(&mut self) -> Result<()> {
        tracing::debug!("Committing transaction (noop)");
        Ok(())
    }
}

#[async_trait]
impl DeviceStore for TimescaleDb {
    type DeviceType = DeviceType;
    type DeviceStatus = DeviceStatus;
    type TxContactData = ();
    type TxEventData = ();

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
        tracing::debug!("Starting transaction");
        Ok(Box::new(self.clone()))
    }

    async fn get_devices(
        &self,
        query: DeviceQuery,
    ) -> Result<Vec<DeviceExt<Self::DeviceType, Self::DeviceStatus>>> {
        // Not applicable for this server application, so do nothing and report error as it should never be used.
        tracing::error!("[Not implemented!] Getting devices with query: {:?}", query);
        Err(DataStoreError::Database(
            "Extended devices query is not supported by this schema".to_string(),
        ))
    }

    /// Load device rows matching `query`.
    ///
    /// Only the filters backed by a `devices` column are applied (`sensor_ids`
    /// → `id`, `device_types` → `device_type`); group/project/cluster filters
    /// have no representation in this schema and are ignored.
    async fn get_devices_simple(
        &self,
        query: DeviceQuery,
    ) -> Result<Vec<Device<Self::DeviceType, Self::DeviceStatus>>> {
        let mut conn = self.pool.get().await.map_err(db_err)?;

        // The `devices` table holds no status; left-join each device to its
        // latest `device_status` row. `DISTINCT ON (devices.id)` with a matching
        // leading `ORDER BY devices.id, device_status.time DESC` keeps only the
        // newest status per device (or NULL when it has none).
        let mut q = d::devices
            .left_join(ds::device_status.on(ds::device_id.eq(d::id)))
            .distinct_on(d::id)
            .order((d::id, ds::time.desc()))
            .select(models::DeviceWithStatus::as_select())
            .into_boxed();
        if let Some(sensor_ids) = &query.sensor_ids {
            q = q.filter(d::id.eq_any(sensor_ids.clone()));
        }
        if let Some(device_types) = &query.device_types {
            q = q.filter(d::device_type.eq_any(device_types.clone()));
        }
        let rows: Vec<models::DeviceWithStatus> = q.load(&mut conn).await.map_err(read_err)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn device_by_id(
        &self,
        id: &str,
    ) -> Result<Option<Device<Self::DeviceType, Self::DeviceStatus>>> {
        let mut conn = self.pool.get().await.map_err(db_err)?;

        // The `devices` table holds no status; left-join to the device's latest
        // `device_status` row (see `get_devices_simple` for the DISTINCT ON idea).
        let joined: Option<models::DeviceWithStatus> = d::devices
            .left_join(ds::device_status.on(ds::device_id.eq(d::id)))
            .filter(d::id.eq(id.to_string()))
            .distinct_on(d::id)
            .order((d::id, ds::time.desc()))
            .select(models::DeviceWithStatus::as_select())
            .first(&mut conn)
            .await
            .optional()
            .map_err(read_err)?;
        Ok(joined.map(Into::into))
    }

    async fn get_device_ids_for_hub(&self, gateway_id: &DataStoreId) -> Result<Vec<DataStoreId>> {
        // Not applicable for this server application, so do nothing and report error as it should never be used.
        tracing::error!("[Not implemented!] Getting device ids for gateway: {gateway_id}");
        Err(DataStoreError::Database(
            "Gateways are not supported by this schema".to_string(),
        ))
    }

    async fn remove_device(&self, device_id: &DataStoreId) -> Result<()> {
        let mut conn = self.pool.get().await.map_err(db_err)?;
        diesel::delete(d::devices.find(device_id.to_string()))
            .execute(&mut conn)
            .await
            .map_err(write_err)?;
        Ok(())
    }

    async fn upsert_devices_for_datatype(
        &self,
        device_type: Self::DeviceType,
        device_ids: &[String],
    ) -> Result<Vec<String>> {
        if device_ids.is_empty() {
            return Ok(vec![]);
        }

        let mut conn = self.pool.get().await.map_err(db_err)?;
        let register_time = Utc::now().timestamp_millis();
        let rows: Vec<models::Device> = device_ids
            .iter()
            .map(|id| models::Device::new_for_type(id.clone(), device_type, register_time))
            .collect();

        // Insert only devices that don't exist yet; return the ids actually inserted.
        // `values` takes the rows by value: `command`'s `serialize_as` proxy
        // requires owned rows rather than a borrow.
        let inserted: Vec<String> = diesel::insert_into(d::devices)
            .values(rows)
            .on_conflict(d::id)
            .do_nothing()
            .returning(d::id)
            .get_results(&mut conn)
            .await
            .map_err(write_err)?;
        Ok(inserted)
    }

    async fn set_device_field(
        &self,
        device_id: &str,
        value: DeviceFieldType<Self::DeviceType, Self::DeviceStatus>,
    ) -> Result<()> {
        TimescaleDb::set_device_field(self, device_id, value).await
    }

    async fn set_device_field_for_groups(
        &self,
        device_id: &str,
        group_ids: Vec<String>,
        value: DeviceFieldType<Self::DeviceType, Self::DeviceStatus>,
    ) -> Result<()> {
        TimescaleDb::set_device_field_for_groups(self, device_id, group_ids, value).await
    }

    async fn set_field_for_devices(
        &self,
        device_ids: &[String],
        value: DeviceFieldType<Self::DeviceType, Self::DeviceStatus>,
    ) -> Result<()> {
        self.update_field(device_ids, value).await
    }

    async fn insert_sensor_server_log(&self, log: SensorServerLog) -> anyhow::Result<()> {
        let mut conn = self.pool.get().await?;
        diesel::insert_into(ssl::sensor_server_log)
            .values(models::SensorServerLogRow::from(log))
            .execute(&mut conn)
            .await?;
        Ok(())
    }

    /// Return the most recent `limit` log rows for `device_id`, newest first.
    async fn get_sensor_server_log(
        &self,
        device_id: &DataStoreId,
        limit: u32,
    ) -> Result<Vec<SensorServerLog>> {
        let mut conn = self.pool.get().await.map_err(db_err)?;
        let rows: Vec<models::SensorServerLogRow> = ssl::sensor_server_log
            .filter(ssl::sensor_id.eq(device_id.to_string()))
            .order(ssl::time.desc())
            .limit(limit.into())
            .select(models::SensorServerLogRow::as_select())
            .load(&mut conn)
            .await
            .map_err(read_err)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn unlink_sensor_from_measuring_points(&self, sensor_id: &DataStoreId) -> Result<()> {
        // Not applicable for this server application, so do nothing and report error as it should never be used.
        tracing::error!(
            "[Not implemented!] Unlinking sensor from measuring points: {}",
            sensor_id
        );
        Err(DataStoreError::Database(
            "Unlinking sensor from measuring points is not supported by this schema".to_string(),
        ))
    }

    async fn unlink_sensor_from_measuring_point_shadows(
        &self,
        sensor_id: &DataStoreId,
    ) -> Result<()> {
        // Not applicable for this server application, so do nothing and report error as it should never be used.
        tracing::error!(
            "[Not implemented!] Unlinking sensor from measuring point shadows: {}",
            sensor_id
        );
        Err(DataStoreError::Database(
            "Unlinking sensor from measuring point shadows is not supported by this schema"
                .to_string(),
        ))
    }
}

#[async_trait]
impl DeviceSettingsStore for TimescaleDb {
    async fn settings_by_device_id(&self, device_id: &str) -> Result<Option<Settings>> {
        let mut conn = self.pool.get().await.map_err(db_err)?;
        let row: Option<models::DeviceSettingsRow> = dsett::device_settings
            .find(device_id.to_string())
            .select(models::DeviceSettingsRow::as_select())
            .first(&mut conn)
            .await
            .optional()
            .map_err(read_err)?;
        Ok(row.map(Into::into))
    }

    async fn take_blink_request(&self, device_id: &str) -> Result<bool> {
        let mut conn = self.pool.get().await.map_err(db_err)?;
        // One conditional UPDATE, so a request written meanwhile is not lost.
        let cleared = diesel::update(
            d::devices
                .filter(d::id.eq(device_id.to_string()))
                .filter(d::command.eq(BLINK_COMMAND)),
        )
        .set(d::command.eq(String::from(models::DbText(Command::None))))
        .execute(&mut conn)
        .await
        .map_err(write_err)?;
        Ok(cleared > 0)
    }
}

#[async_trait]
impl MeasurementDataStore for TimescaleDb {
    async fn insert_measurement_data(&self, device_id: &str, data: &Data) -> Result<()> {
        let rows = models::MeasurementDataRow::rows_from_data(device_id, data);
        if rows.len() < data.t.len() {
            tracing::warn!(
                "Dropping {} of {} samples from {device_id}: unrepresentable timestamp or short channel",
                data.t.len() - rows.len(),
                data.t.len()
            );
        }
        if rows.is_empty() {
            return Ok(());
        }

        let mut conn = self.pool.get().await.map_err(db_err)?;
        for chunk in rows.chunks(MEASUREMENT_INSERT_CHUNK) {
            diesel::insert_into(md::measurement_data)
                .values(chunk)
                .on_conflict((md::device_id, md::time))
                .do_nothing()
                .execute(&mut conn)
                .await
                .map_err(write_err)?;
        }
        Ok(())
    }
}

#[async_trait]
impl DeviceEventStore for TimescaleDb {
    async fn insert_device_event(&self, device_id: &str, payload: &EventPayload) -> Result<()> {
        let Some(row) = models::DeviceEventRow::from_payload(device_id, payload) else {
            tracing::warn!("Dropping event from {device_id}: unrepresentable {payload:?}");
            return Ok(());
        };
        let mut conn = self.pool.get().await.map_err(db_err)?;
        diesel::insert_into(de::device_events)
            .values(&row)
            .on_conflict((de::device_id, de::time, de::event))
            .do_nothing()
            .execute(&mut conn)
            .await
            .map_err(write_err)?;
        Ok(())
    }
}

#[async_trait]
impl EventStore for TimescaleDb {
    type ContactData = ();
    type EventData = ();

    async fn insert_event(
        &self,
        event: NewEvent<Self::ContactData, Self::EventData>,
    ) -> Result<DataStoreId> {
        TimescaleDb::insert_event(self, event)
    }

    async fn query_events(
        &self,
        query: EventQuery,
    ) -> Result<EventsWithStats<Self::ContactData, Self::EventData>> {
        tracing::error!("[Not implemented!] Querying events with query: {:?}", query);
        Ok(EventsWithStats {
            events: vec![],
            code_stats: vec![],
            type_stats: vec![],
            hour_stats: vec![],
        })
    }

    async fn add_event_comment(&self, event_id: DataStoreId, comment: Comment) -> Result<()> {
        tracing::error!(
            "[Not implemented!] Adding event comment for event: {} with comment: {:?}",
            event_id,
            comment
        );
        Ok(())
    }

    async fn edit_event_comment(
        &self,
        event_id: DataStoreId,
        comment_id: DataStoreId,
        user_id: DataStoreId,
        comment: String,
    ) -> Result<()> {
        tracing::error!(
            "[Not implemented!] Editing event comment (with ID: {}) from user {} for event: {} with comment: {:?}",
            comment_id,
            user_id,
            event_id,
            comment
        );
        Ok(())
    }

    async fn event_mark_sent(
        &self,
        event_id: &DataStoreId,
        send_status: SendStatus,
    ) -> anyhow::Result<()> {
        tracing::error!(
            "[Not implemented!] Marking event as sent for event: {} with status: {:?}",
            event_id,
            send_status
        );
        Ok(())
    }

    async fn event_update_code(
        &self,
        event_id: &DataStoreId,
        event_code: u32,
        event_type: EventType,
    ) -> anyhow::Result<()> {
        tracing::error!(
            "[Not implemented!] Updating event code for event: {} with code: {} and type: {:?}",
            event_id,
            event_code,
            event_type
        );
        Ok(())
    }
}

#[async_trait]
impl FirmwareStore for TimescaleDb {
    type DeviceType = DeviceType;

    async fn insert_firmware(&self, firmware: NewFirmware) -> Result<()> {
        let mut conn = self.pool.get().await.map_err(db_err)?;
        // `id` is filled by the column DEFAULT (`gen_random_uuid()::text`); the
        // `device_type` proxy's `serialize_as` requires an owned row.
        diesel::insert_into(f::firmware)
            .values(models::NewFirmwareRow::from(firmware))
            .execute(&mut conn)
            .await
            .map_err(write_err)?;
        Ok(())
    }

    async fn get_firmwares(&self, device_type: &Self::DeviceType) -> Result<Vec<Firmware>> {
        let mut conn = self.pool.get().await.map_err(db_err)?;
        let rows: Vec<models::FirmwareRow> = f::firmware
            .filter(f::device_type.eq(String::from(models::DbText(*device_type))))
            .select(models::FirmwareRow::as_select())
            .load(&mut conn)
            .await
            .map_err(read_err)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn firmware_by_id(&self, id: &DataStoreId) -> Result<Option<Firmware>> {
        let mut conn = self.pool.get().await.map_err(db_err)?;
        let row: Option<models::FirmwareRow> = f::firmware
            .find(id.to_string())
            .select(models::FirmwareRow::as_select())
            .first(&mut conn)
            .await
            .optional()
            .map_err(read_err)?;
        Ok(row.map(Into::into))
    }

    async fn remove_firmware(&self, id: &DataStoreId) -> Result<()> {
        let mut conn = self.pool.get().await.map_err(db_err)?;
        diesel::delete(f::firmware.find(id.to_string()))
            .execute(&mut conn)
            .await
            .map_err(write_err)?;
        Ok(())
    }

    async fn recommend_firmware(&self, id: &DataStoreId) -> Result<()> {
        let mut conn = self.pool.get().await.map_err(db_err)?;
        diesel::update(f::firmware.find(id.to_string()))
            .set(f::recommended.eq(true))
            .execute(&mut conn)
            .await
            .map_err(write_err)?;
        Ok(())
    }

    async fn unrecommend_other_firmwares_for_same_device_type(
        &self,
        id: &DataStoreId,
    ) -> Result<()> {
        // Clear `recommended` on every firmware sharing this one's device type,
        // except `id` itself. Paired with `recommend_firmware` by the upstream
        // `recommend_firmware` helper so exactly one firmware per device type
        // stays recommended. No-op if the target firmware no longer exists.
        let mut conn = self.pool.get().await.map_err(db_err)?;
        let device_type: Option<String> = f::firmware
            .find(id.to_string())
            .select(f::device_type)
            .first(&mut conn)
            .await
            .optional()
            .map_err(read_err)?;
        let Some(device_type) = device_type else {
            return Ok(());
        };
        diesel::update(
            f::firmware
                .filter(f::device_type.eq(device_type))
                .filter(f::id.ne(id.to_string())),
        )
        .set(f::recommended.eq(false))
        .execute(&mut conn)
        .await
        .map_err(write_err)?;
        Ok(())
    }
}

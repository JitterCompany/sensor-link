use async_trait::async_trait;
use demo_protocol::{data::std::Data, settings::Settings, DeviceType, EventPayload};
use sensor_link_server_core::store_traits::{DeviceStore, EventStore, Result};

mod data_types;
pub mod db;
pub mod device;
mod device_status;
pub mod firmware;
pub mod http;
pub mod mqtt;
#[cfg(test)]
pub mod test_support;

pub use data_types::DataType;

use crate::device_status::DeviceStatus;

pub const BUILD_VERSION: Option<&str> = option_env!("BUILD_VERSION");
pub const GIT_REV: Option<&str> = option_env!("GIT_REV");
pub const BUILD_DATE: Option<&str> = option_env!("BUILD_DATE");
pub const CARGO_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn get_mqtt_broker_host_name() -> String {
    std::env::var("MQTT_BROKER_HOST_NAME").unwrap_or("localhost".to_string())
}

pub fn get_mqtt_broker_port() -> u16 {
    std::env::var("MQTT_BROKER_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(1884)
}

pub fn get_http_bind_addr() -> String {
    std::env::var("HTTP_BIND_ADDR").unwrap_or("127.0.0.1:8087".to_string())
}

/// Directory holding the uploaded firmware images, watched by [`firmware`].
pub fn get_firmware_dir() -> std::path::PathBuf {
    std::env::var("FIRMWARE_DIR")
        .unwrap_or("/firmwares".to_string())
        .into()
}

/// Base URL the firmware images in [`get_firmware_dir`] are served at; a file
/// name is appended to it to form the download URL stored per firmware.
pub fn get_firmware_base_url() -> String {
    std::env::var("FIRMWARE_BASE_URL").unwrap_or("https://mqtt.demo.example/fw".to_string())
}

/// Supertrait alias for `EventStore` fixed to the demo contact and event detail types.
/// Use as a bound (`DS: AppEventStore`) instead of the verbose associated-type form.
pub trait AppEventStore: EventStore<ContactData = (), EventData = ()> {}
impl<T: EventStore<ContactData = (), EventData = ()>> AppEventStore for T {}

/// Supertrait alias for `DeviceStore` fixed to the demo device types.
/// Use as a bound (`DS: AppDeviceStore`) instead of the verbose associated-type form.
pub trait AppDeviceStore:
    DeviceStore<
    TxContactData = (),
    TxEventData = (),
    DeviceType = DeviceType,
    DeviceStatus = DeviceStatus,
>
{
}
impl<T> AppDeviceStore for T where
    T: DeviceStore<
        TxContactData = (),
        TxEventData = (),
        DeviceType = DeviceType,
        DeviceStatus = DeviceStatus,
    >
{
}

/// [`AppDeviceStore`] extended with the demo-specific device settings storage.
///
/// Settings have no place in the upstream `DeviceStore` trait, but modules that
/// need them shouldn't have to name the concrete store either: they take
/// `DB: DeviceSettingsStore` and can be tested against a mock store.
#[async_trait]
pub trait DeviceSettingsStore: AppDeviceStore {
    /// Load the operational settings of one device.
    ///
    /// `Ok(None)` means no settings are stored for the device (yet), which is
    /// also what an unknown device id yields, since there is nothing to
    /// distinguish the two here.
    async fn settings_by_device_id(&self, device_id: &str) -> Result<Option<Settings>>;

    /// Clear a pending `Blink` in `devices.command`; `true` if there was one.
    ///
    /// The upstream `Command` has no blink variant, so the core status handler
    /// reads a stored `Blink` as no command and the demo server sends it itself.
    async fn take_blink_request(&self, device_id: &str) -> Result<bool>;
}

/// Storage for the measurement data devices publish on their `data` topic: a
/// received payload is stored as-is, keyed by the device that sent it.
///
/// Modules that persist measurements take `DB: MeasurementDataStore` rather than
/// the concrete store, so they can be tested against a mock store.
#[async_trait]
pub trait MeasurementDataStore: Send + Sync {
    /// Store every sample of a received payload for `device_id`.
    ///
    /// Samples already stored for that device and timestamp are left untouched,
    /// so a device re-sending a set of samples does not fail the insert.
    async fn insert_measurement_data(&self, device_id: &str, data: &Data) -> Result<()>;
}

/// Storage for the events devices publish on their `events` topic.
#[async_trait]
pub trait DeviceEventStore: Send + Sync {
    /// Store a received event for `device_id`.
    ///
    /// An event already stored for that device, timestamp and name is left
    /// untouched, so a device re-sending a batch does not fail the insert.
    async fn insert_device_event(&self, device_id: &str, payload: &EventPayload) -> Result<()>;
}

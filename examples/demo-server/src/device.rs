use std::{marker::Send, sync::Arc};

use chrono::Utc;
use demo_protocol::{fwupdate::FWStatus, online::Online, DeviceType};
use sensor_link_mqtt::DeviceControlIn;
use sensor_link_server_core::{
    store_traits::{DeviceStore, FirmwareStore, Result},
    utils::datetime::datetime_from_millis,
};
use task_supervisor::{get_crate_relative_function_path, Handle, PanicCallback};
use tokio::sync::{mpsc, Mutex};

use crate::{
    db::TimescaleDb,
    device_status::DeviceStatus,
    mqtt::{ControlMessageIn, ControlMessageOut},
    AppDeviceStore, DeviceEventStore,
};

// Type aliases binding the demo concrete types to generic sensor-link-server-core structs
pub type Device = sensor_link_server_core::device::Device<DeviceType, DeviceStatus>;
pub type DeviceExt = sensor_link_server_core::device::DeviceExt<DeviceType, DeviceStatus>;
pub type DeviceFieldType =
    sensor_link_server_core::device::DeviceFieldType<DeviceType, DeviceStatus>;
pub type Firmware = sensor_link_server_core::firmware::Firmware<DeviceType>;
pub type NewFirmware = sensor_link_server_core::firmware::NewFirmware<DeviceType>;

pub(crate) mod info_handler;
pub(crate) mod settings_sync;
mod status_handler;

use info_handler::InfoHandler;
use status_handler::DeviceStatusHandler;

pub fn start_task(
    db: TimescaleDb,
    tx_to_mqtt: mpsc::Sender<ControlMessageOut>,
    rx_from_mqtt: mpsc::Receiver<ControlMessageIn>,
    on_panic: PanicCallback,
) -> Handle
where
    <TimescaleDb as FirmwareStore>::DeviceType: Send,
{
    let task_function = device_sync_task;
    let rx_from_mqtt = Arc::new(Mutex::new(rx_from_mqtt));
    Handle::new(
        move |_| task_function(db.clone(), tx_to_mqtt.clone(), rx_from_mqtt.clone()),
        get_crate_relative_function_path(task_function),
        on_panic,
    )
}

async fn device_sync_task(
    db: TimescaleDb,
    tx_to_mqtt: mpsc::Sender<ControlMessageOut>,
    rx_from_mqtt: Arc<Mutex<mpsc::Receiver<ControlMessageIn>>>,
) {
    let status_handler = DeviceStatusHandler::new(&db, &tx_to_mqtt);

    let info_handler = InfoHandler::new(&db);

    while let Some(msg) = rx_from_mqtt
        .try_lock()
        .expect("MQTT receiver seems to be locked by another task then device sync task")
        .recv()
        .await
    {
        match msg.payload {
            DeviceControlIn::DeviceOnline(online) => {
                if let Err(err) = process_device_online(&msg.device_id, &db, online).await {
                    tracing::error!("Error processing device online: {:?}", err);
                }
            }
            DeviceControlIn::DeviceInfo(device_info) => {
                if let Err(err) = info_handler.process_info(&msg.device_id, device_info).await {
                    tracing::error!("Error processing device info: {:?}", err);
                }
            }
            DeviceControlIn::Event(payload) => {
                tracing::info!("Event from {}: {:?}", msg.device_id, payload.event);
                if let Err(err) = db.insert_device_event(&msg.device_id, &payload).await {
                    tracing::error!("Error storing device event: {:?}", err);
                }
            }
            DeviceControlIn::DeviceStatus(status) => {
                if let Err(err) = status_handler.handle_status(&msg.device_id, status).await {
                    tracing::error!("Error handling device status: {:?}", err);
                }
            }
            DeviceControlIn::FirmwareUpdateStatus(status) => match status {
                FWStatus::Failed => {
                    tracing::error!("Firmware update for device {} failed", msg.device_id);
                }
                FWStatus::Start | FWStatus::Received | FWStatus::Complete => {
                    tracing::debug!(
                        "Firmware update status for device {}: {:?}",
                        msg.device_id,
                        status
                    );
                }
            },
        }
    }
}

pub async fn process_device_online(
    device_id: &String,
    db: &TimescaleDb,
    payload: Online,
) -> anyhow::Result<()> {
    if let Some(since) = payload.since() {
        tracing::debug!("Device {} is online since {since:?}", device_id);
    } else if payload.ungraceful_offline() {
        tracing::debug!("Device {} is offline (ungraceful)", device_id);
    } else {
        tracing::debug!("Device {} is offline", device_id);
    }

    let ts = payload
        .since()
        .map(|s| datetime_from_millis(s.milliseconds()).unwrap_or(Utc::now()));

    db.set_device_field(device_id, DeviceFieldType::OnlineSince(ts))
        .await?;
    Ok(())
}

pub async fn register_devices<DS>(
    db: &DS,
    device_type: DeviceType,
    sensor_ids: &[String],
) -> Result<()>
where
    DS: AppDeviceStore + ?Sized,
{
    let new_device_ids = db
        .upsert_devices_for_datatype(device_type, sensor_ids)
        .await?;

    if new_device_ids.is_empty() {
        return Ok(());
    }

    tracing::info!(
        device_ids = ?new_device_ids,
        device_type = ?device_type,
        "Registering devices: {:?}",
        new_device_ids
    );
    for new_device_id in new_device_ids {
        db.set_device_field(
            &new_device_id,
            DeviceFieldType::RegisterTime(Utc::now().timestamp_millis()),
        )
        .await?;
    }
    Ok(())
}

pub async fn deregister<DB: AppDeviceStore + ?Sized>(db: &DB, device_id: &String) -> Result<()> {
    db.remove_device(device_id).await
}

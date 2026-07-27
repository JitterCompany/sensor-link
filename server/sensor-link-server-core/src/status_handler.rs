mod command_handler;
mod firmware_handler;

use chrono::Utc;
use sensor_link_protocol::{cmd::CommandPayload, fwupdate::FWAnnounce};

use crate::{
    device::{Device, DeviceFieldType, DeviceStatusLike},
    store_traits::{DeviceStore, FirmwareStore},
    DataStoreId,
};

use command_handler::CommandHandler;
use firmware_handler::FirmwareHandler;

pub struct ControlMessageOut {
    pub device_id: String,
    pub payload: DeviceControlOut,
}

pub enum DeviceControlOut {
    DeviceCommand(CommandPayload),
    FWUpdateAnnounce(FWAnnounce),
    Time,
}

/// Core status handler requiring only DeviceStore, EventStore, SensorDataStore, and FirmwareStore.
/// Structured to be movable to sensor-core once those traits are defined there.
pub struct StatusHandler<'a, DS: DeviceStore> {
    db: &'a DS,
    command_handler: CommandHandler<'a, DS>,
    firmware_handler: FirmwareHandler<'a, DS>,
}

impl<'a, DS: DeviceStore> StatusHandler<'a, DS> {
    pub fn new(db: &'a DS) -> Self {
        Self {
            db,
            command_handler: CommandHandler::new(db),
            firmware_handler: FirmwareHandler::new(db),
        }
    }

    /// Handles a device status update using only the core store traits.
    /// Covers contact time, command dispatch, time sync, config-change detection,
    /// device status persistence (including TimescaleDB), and firmware update checks.
    pub async fn handle_status(
        &self,
        device: &Device<<DS as DeviceStore>::DeviceType, <DS as DeviceStore>::DeviceStatus>,
        status: &<DS as DeviceStore>::DeviceStatus,
    ) -> anyhow::Result<Vec<ControlMessageOut>>
    where
        DS: FirmwareStore,
    {
        let mut messages = Vec::new();
        self.update_contact_time(device).await;
        if let Some(command_message) = self.handle_command(device, status).await? {
            messages.push(command_message);
        }
        messages.push(self.send_time_sync(&device.id));
        self.update_device_status(device, status).await?;
        if let Some(firmware_update_message) = self.check_firmware_update(device).await? {
            messages.push(firmware_update_message);
        }
        Ok(messages)
    }

    pub async fn update_contact_time(&self, device: &Device<DS::DeviceType, DS::DeviceStatus>) {
        self.update_contact_time_for_sensors(vec![&device.id]).await;
    }

    pub async fn update_contact_time_for_sensors(&self, sensor_ids: Vec<&str>) {
        for sensor_id in sensor_ids {
            if let Err(err) = self
                .db
                .set_device_field(
                    sensor_id,
                    DeviceFieldType::LastContact(Some(Utc::now().timestamp_millis())),
                )
                .await
            {
                tracing::error!("Error setting last contact for sensor ID {sensor_id}: {err}");
            }
        }
    }

    pub async fn handle_command(
        &self,
        device: &Device<DS::DeviceType, DS::DeviceStatus>,
        status: &DS::DeviceStatus,
    ) -> anyhow::Result<Option<ControlMessageOut>> {
        self.command_handler
            .check_send_command(device, status)
            .await
            .map_err(|err| anyhow::anyhow!("Error sending command: {:?}", err))
    }

    pub fn send_time_sync(&self, device_id: &str) -> ControlMessageOut {
        ControlMessageOut {
            device_id: device_id.to_string(),
            payload: DeviceControlOut::Time,
        }
    }

    pub async fn update_device_status(
        &self,
        device: &Device<DS::DeviceType, DS::DeviceStatus>,
        status: &DS::DeviceStatus,
    ) -> anyhow::Result<()> {
        let previous_status = device.device_status.clone();
        update_status(
            self.db,
            &device.id,
            status,
            &previous_status,
            device.waiting_for_new_mp,
        )
        .await;
        Ok(())
    }

    pub async fn check_firmware_update(
        &self,
        device: &Device<<DS as DeviceStore>::DeviceType, <DS as DeviceStore>::DeviceStatus>,
    ) -> anyhow::Result<Option<ControlMessageOut>>
    where
        DS: FirmwareStore,
    {
        self.firmware_handler
            .check_and_announce_update(device)
            .await
            .map_err(|err| anyhow::anyhow!("Error checking for firmware update: {err:?}"))
    }
}

pub async fn update_status<DS: DeviceStore>(
    db: &DS,
    device_id: &DataStoreId,
    device_status: &DS::DeviceStatus,
    previous_device_status: &DS::DeviceStatus,
    waiting_for_new_mp: bool,
) {
    if device_status.status() != previous_device_status.status() || waiting_for_new_mp {
        let _ = db
            .set_device_field(
                device_id,
                DeviceFieldType::StatusSince(Utc::now().timestamp_millis()),
            )
            .await
            .map_err(|err| tracing::error!("Error setting status_since: {:?}", err));
    }
    let _ = db
        .set_device_field(
            device_id,
            DeviceFieldType::DeviceStatus(device_status.clone()),
        )
        .await
        .map_err(|err| tracing::error!("Error setting status: {:?}", err));
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::store_traits::test::MockStore;

    use super::*;

    fn make_store_with_capture() -> (MockStore, Arc<Mutex<Vec<DeviceFieldType<(), ()>>>>) {
        let calls: Arc<Mutex<Vec<DeviceFieldType<(), ()>>>> = Arc::new(Mutex::new(vec![]));
        let calls_clone = calls.clone();
        let store = MockStore {
            set_device_field: Some(Arc::new(move |_, value| {
                calls_clone.lock().unwrap().push(value);
                Ok(())
            })),
            firmware_by_id: Some(Arc::new(|_| Ok(None))),
        };
        (store, calls)
    }

    #[tokio::test]
    async fn test_update_contact_time_sets_last_contact() {
        let (store, calls) = make_store_with_capture();
        let device: Device<(), ()> = Device {
            id: "VibDev1".to_string(),
            ..Default::default()
        };
        StatusHandler::new(&store)
            .handle_status(&device, &())
            .await
            .unwrap();
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .any(|f| matches!(f, DeviceFieldType::LastContact(Some(_)))),
            "Expected set_device_field to be called with LastContact"
        );
    }

    #[tokio::test]
    async fn test_update_device_status_sets_device_status_field() {
        let (store, calls) = make_store_with_capture();
        let device: Device<(), ()> = Device {
            id: "StatusDev1".to_string(),
            ..Default::default()
        };
        StatusHandler::new(&store)
            .handle_status(&device, &())
            .await
            .unwrap();
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .any(|f| matches!(f, DeviceFieldType::DeviceStatus(_))),
            "Expected set_device_field to be called with DeviceStatus"
        );
    }
}

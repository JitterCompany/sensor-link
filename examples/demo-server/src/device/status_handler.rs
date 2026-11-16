use crate::{
    db::TimescaleDb,
    device::{settings_sync::SettingsSyncHandler, Device},
    mqtt::{control_out::from_status_handler_control_message_out, ControlMessageOut},
    DeviceSettingsStore,
};
use demo_protocol::cmd::{Cmd, CommandPayload};
use sensor_link_mqtt::{ControlMessageOut as MqttControlMessageOut, DeviceControlOut};
use sensor_link_server_core::{
    status_handler::StatusHandler as CoreDeviceStatusHandler, store_traits::DeviceStore,
};
use tokio::sync::mpsc;

pub struct DeviceStatusHandler<'a> {
    db: &'a TimescaleDb,
    tx_to_mqtt: &'a mpsc::Sender<ControlMessageOut>,
    core: CoreDeviceStatusHandler<'a, TimescaleDb>,
    settings_sync_handler: SettingsSyncHandler<'a, TimescaleDb>,
}

impl<'a> DeviceStatusHandler<'a> {
    pub(crate) fn new(
        db: &'a TimescaleDb,
        tx_to_mqtt: &'a mpsc::Sender<ControlMessageOut>,
    ) -> Self {
        Self {
            db,
            tx_to_mqtt,
            core: CoreDeviceStatusHandler::new(db),
            settings_sync_handler: SettingsSyncHandler::new(db, tx_to_mqtt),
        }
    }

    /// Main entry point for handling device status updates
    pub(crate) async fn handle_status(
        &self,
        device_id: &str,
        status: demo_protocol::status::DeviceStatus,
    ) -> anyhow::Result<()> {
        let device = self
            .db
            .device_by_id(device_id)
            .await?
            .ok_or(anyhow::anyhow!("Device not found"))?;

        // Orchestrate the handling in clear steps
        for msg in self
            .core
            .handle_status(&device, &status.clone().into())
            .await?
        {
            self.tx_to_mqtt
                .send(from_status_handler_control_message_out(msg))
                .await?;
        }
        self.send_blink_request(&device).await?;
        self.update_contact_time(&device).await;
        self.handle_settings_change(&device, &status).await?;
        self.sync_settings(&device, &status).await?;

        Ok(())
    }

    pub async fn update_contact_time(&self, device: &Device) {
        self.core
            .update_contact_time_for_sensors(vec![device.id.as_str()])
            .await;
    }

    /// Forward a `Blink` stored in `devices.command`, which the core command
    /// handler cannot represent.
    async fn send_blink_request(&self, device: &Device) -> anyhow::Result<()> {
        if self.db.take_blink_request(&device.id).await? {
            tracing::info!("Sending blink command to {}", device.id);
            self.tx_to_mqtt
                .send(MqttControlMessageOut {
                    device_id: device.id.clone(),
                    payload: DeviceControlOut::DeviceCommand(CommandPayload { cmd: Cmd::Blink })
                        .into(),
                })
                .await?;
        }
        Ok(())
    }

    async fn handle_settings_change(
        &self,
        device: &Device,
        status: &demo_protocol::status::DeviceStatus,
    ) -> anyhow::Result<()> {
        let previous_status = device.device_status.clone();
        if previous_status.inner.settings_hash != status.settings_hash {
            // TODO: could emit Event SettingsUpdated here.
        }
        Ok(())
    }

    async fn sync_settings(
        &self,
        device: &Device,
        status: &demo_protocol::status::DeviceStatus,
    ) -> anyhow::Result<()> {
        self.settings_sync_handler.sync(device, status).await
    }
}

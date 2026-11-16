use chrono::Utc;
use demo_protocol::DeviceInfoV3;
use sensor_link_server_core::device::{Command, DeviceFieldType, Version};

use crate::{device::register_devices, AppDeviceStore};

pub struct InfoHandler<'a, DB: AppDeviceStore> {
    db: &'a DB,
}

impl<'a, DB: AppDeviceStore> InfoHandler<'a, DB> {
    pub(crate) fn new(db: &'a DB) -> Self {
        Self { db }
    }

    pub async fn process_info(
        &self,
        device_id: &str,
        device_info: DeviceInfoV3,
    ) -> anyhow::Result<()> {
        let device_type = device_info.device_type;
        let device_ids = vec![device_id.to_string()];
        register_devices(self.db, device_type, &device_ids).await?;
        self.db
            .set_field_for_devices(
                &device_ids,
                DeviceFieldType::LastContact(Some(Utc::now().timestamp_millis())),
            )
            .await?;
        let version = Version {
            firmware: format!("{}-{}", device_info.fw_version, device_info.fw_rev),
            bootloader: device_info.bootloader_version.to_string(),
            modem_firmware_version: device_info.modem_fw_version.to_string(),
            modem_model: device_info.modem_model.to_string(),
            hardware: device_info.hw_rev.to_string(),
            ..Default::default()
        };
        self.db
            .set_device_field(device_id, DeviceFieldType::Version(Some(version)))
            .await?;
        if self
            .db
            .device_by_id(device_id)
            .await?
            .ok_or(anyhow::anyhow!("Device not found"))?
            .command
            == Command::Reboot
        {
            self.db
                .set_device_field(device_id, DeviceFieldType::Command(Command::None))
                .await
                .map_err(|err| anyhow::anyhow!("Error resetting command: {err:?}"))?;
        }

        Ok(())
    }
}

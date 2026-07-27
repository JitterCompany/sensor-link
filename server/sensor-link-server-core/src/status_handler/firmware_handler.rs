use std::str::FromStr;

use sensor_link_protocol::fwupdate::FWAnnounce;

use crate::{
    device::{Device, DeviceFieldType},
    status_handler::{ControlMessageOut, DeviceControlOut},
    store_traits::{DeviceStore, FirmwareStore},
};

pub struct FirmwareHandler<'a, DS> {
    db: &'a DS,
}

impl<'a, DS> FirmwareHandler<'a, DS> {
    pub fn new(db: &'a DS) -> Self {
        Self { db }
    }
}

impl<'a, DS: DeviceStore + FirmwareStore> FirmwareHandler<'a, DS> {
    /// Check if device needs firmware update and send announcement
    pub async fn check_and_announce_update(
        &self,
        device: &Device<<DS as DeviceStore>::DeviceType, <DS as DeviceStore>::DeviceStatus>,
    ) -> anyhow::Result<Option<ControlMessageOut>> {
        if let Some(firmware) = self
            .db
            .firmware_by_id(
                device
                    .marked_for_update
                    .as_ref()
                    .unwrap_or(&Default::default()),
            )
            .await
            .ok()
            .flatten()
        {
            if device
                .version
                .as_ref()
                .map(|v| v.firmware == firmware.version)
                .unwrap_or(false)
            {
                self.db
                    .set_device_field(&device.id, DeviceFieldType::MarkedForUpdate(None))
                    .await
                    .map_err(|err| anyhow::anyhow!("Error resetting marked_for_update: {err:?}"))?;
            } else {
                return Ok(Some(ControlMessageOut {
                    payload: DeviceControlOut::FWUpdateAnnounce(FWAnnounce {
                        url: heapless::String::from_str(&firmware.v2BinID).map_err(|err| {
                            anyhow::anyhow!("Error converting firmware ID: {err:?}")
                        })?,
                        timestamp: None,
                    }),
                    device_id: device.id.clone(),
                }));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use chrono::Utc;

    use crate::{device::Version, firmware::Firmware, store_traits::test::MockStore};

    use super::*;

    #[tokio::test]
    async fn test_no_firmware_marked_for_update_is_noop() {
        let store = MockStore {
            firmware_by_id: Some(Arc::new(|_| Ok(None))),
            ..Default::default()
        };
        let device: Device<(), ()> = Device {
            id: "fw_dev_none".to_string(),
            marked_for_update: None,
            ..Default::default()
        };
        let msg = FirmwareHandler::new(&store)
            .check_and_announce_update(&device)
            .await
            .unwrap();
        assert!(msg.is_none());
    }

    #[tokio::test]
    async fn test_firmware_marked_but_not_found_in_db_is_noop() {
        let store = MockStore {
            firmware_by_id: Some(Arc::new(|_| Ok(None))),
            ..Default::default()
        };
        let device: Device<(), ()> = Device {
            id: "fw_dev_notfound".to_string(),
            marked_for_update: Some("000000000000000000000001".to_string()),
            ..Default::default()
        };
        let msg = FirmwareHandler::new(&store)
            .check_and_announce_update(&device)
            .await
            .unwrap();
        assert!(msg.is_none());
    }

    #[tokio::test]
    async fn test_firmware_already_at_target_version_clears_marked_for_update() {
        let captured: Arc<Mutex<Option<DeviceFieldType<(), ()>>>> = Arc::new(Mutex::new(None));
        let captured_clone = captured.clone();
        let store = MockStore {
            firmware_by_id: Some(Arc::new(|_| {
                Ok(Some(Firmware {
                    id: "fw_id".to_string(),
                    version: "2.0.0".to_string(),
                    description: "test".to_string(),
                    date: Utc::now(),
                    v2BinID: "binfile123".to_string(),
                    recommended: true,
                    device_type: "frogwatch2vibration".to_string(),
                }))
            })),
            set_device_field: Some(Arc::new(move |_, value| {
                *captured_clone.lock().unwrap() = Some(value);
                Ok(())
            })),
            ..Default::default()
        };
        let device: Device<(), ()> = Device {
            id: "fw_dev_same_version".to_string(),
            marked_for_update: Some("fw_id".to_string()),
            version: Some(Version {
                firmware: "2.0.0".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let msg = FirmwareHandler::new(&store)
            .check_and_announce_update(&device)
            .await
            .unwrap();
        assert!(msg.is_none());
        assert!(matches!(
            *captured.lock().unwrap(),
            Some(DeviceFieldType::MarkedForUpdate(None))
        ));
    }

    #[tokio::test]
    async fn test_firmware_different_version_sends_fw_update_announce() {
        let store = MockStore {
            firmware_by_id: Some(Arc::new(|_| {
                Ok(Some(Firmware {
                    id: "fw_id".to_string(),
                    version: "3.0.0".to_string(),
                    description: "update".to_string(),
                    date: Utc::now(),
                    v2BinID: "v3_firmware_bin".to_string(),
                    recommended: true,
                    device_type: "frogwatch2vibration".to_string(),
                }))
            })),
            ..Default::default()
        };
        let device: Device<(), ()> = Device {
            id: "fw_dev_outdated".to_string(),
            marked_for_update: Some("fw_id".to_string()),
            version: Some(Version {
                firmware: "1.0.0".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let msg = FirmwareHandler::new(&store)
            .check_and_announce_update(&device)
            .await
            .unwrap();
        assert!(msg.is_some());
        let msg = msg.unwrap();
        assert_eq!(msg.device_id, device.id);
        match msg.payload {
            DeviceControlOut::FWUpdateAnnounce(announce) => {
                assert_eq!(announce.url.as_str(), "v3_firmware_bin");
            }
            _ => panic!("Expected FWUpdateAnnounce"),
        }
    }
}

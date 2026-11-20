use chrono::Utc;
use demo_protocol::{settings::Settings, status::DeviceStatus};
use tokio::sync::mpsc;

use crate::{
    device::{Device, DeviceFieldType},
    mqtt::{control_out::DemoControlOut, ControlMessageOut},
    DeviceSettingsStore,
};

const CONFIG_RESEND_COOLDOWN_MS: i64 = 60_000;

pub struct SettingsSyncHandler<'a, DB: DeviceSettingsStore> {
    db: &'a DB,
    tx_to_mqtt: &'a mpsc::Sender<ControlMessageOut>,
}

impl<'a, DB: DeviceSettingsStore> SettingsSyncHandler<'a, DB> {
    pub(crate) fn new(db: &'a DB, tx_to_mqtt: &'a mpsc::Sender<ControlMessageOut>) -> Self {
        Self { db, tx_to_mqtt }
    }
}

impl<'a, DB: DeviceSettingsStore> SettingsSyncHandler<'a, DB> {
    /// Synchronize device configuration
    pub(crate) async fn sync(&self, device: &Device, status: &DeviceStatus) -> anyhow::Result<()> {
        let should_send = device
            .config_last_sent_at
            .map(|t| Utc::now().timestamp_millis() - t > CONFIG_RESEND_COOLDOWN_MS)
            .unwrap_or(true);
        let mut settings_confirmed = true;
        let settings = self.db.settings_by_device_id(&device.id).await?;
        if let Some(settings) = settings {
            settings_confirmed &= self
                .sync_settings(status, device, settings, should_send)
                .await;
        } else {
            tracing::info!(
                "No settings found for device {}. Skip sending update.",
                device.id
            );
            settings_confirmed = true;
        };
        if settings_confirmed {
            if device.config_last_sent_at.is_some() {
                let _ = self
                    .db
                    .set_device_field(&device.id, DeviceFieldType::ConfigLastSentAt(None))
                    .await;
            }
        } else if should_send {
            let _ = self
                .db
                .set_device_field(
                    &device.id,
                    DeviceFieldType::ConfigLastSentAt(Some(Utc::now().timestamp_millis())),
                )
                .await;
        }
        if settings_confirmed {
            if !device.config_confirmed {
                self.db
                    .set_device_field(&device.id, DeviceFieldType::ConfigConfirmed(true))
                    .await?;
            }
        } else {
            if device.config_confirmed {
                self.db
                    .set_device_field(&device.id, DeviceFieldType::ConfigConfirmed(false))
                    .await?;
            }
        }
        Ok(())
    }

    /// Check and send updated settings
    async fn sync_settings(
        &self,
        status: &DeviceStatus,
        device: &Device,
        mut settings: Settings,
        should_send: bool,
    ) -> bool {
        let current_settings_hash = settings.calculate_hash();
        let settings_confirmed = status.settings_hash == current_settings_hash;
        if !settings_confirmed && should_send {
            settings.hash = current_settings_hash;
            let _ = self
                .tx_to_mqtt
                .try_send(ControlMessageOut {
                    device_id: device.id.clone(),
                    payload: DemoControlOut::Settings(settings),
                })
                .map_err(|err| tracing::error!("Error sending settings: {:?}", err));
        }

        settings_confirmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{MockDeviceStore, RecordedField, StoreCall};

    const DEVICE_ID: &str = "dev-1";

    fn settings() -> Settings {
        Settings {
            data_rate: Default::default(),
            sync_interval_s: 900,
            hash: 0,
        }
    }

    fn device() -> Device {
        Device {
            id: DEVICE_ID.to_string(),
            ..Default::default()
        }
    }

    fn status(settings_hash: u64) -> DeviceStatus {
        DeviceStatus {
            settings_hash,
            ..Default::default()
        }
    }

    /// Runs the sync against a mock store, returning the store and every
    /// control message the handler queued.
    async fn sync(
        db: MockDeviceStore,
        device: &Device,
        status: &DeviceStatus,
    ) -> (MockDeviceStore, Vec<ControlMessageOut>) {
        let (tx, mut rx) = mpsc::channel(4);
        SettingsSyncHandler::new(&db, &tx)
            .sync(device, status)
            .await
            .unwrap();
        drop(tx);
        let mut sent = vec![];
        while let Ok(msg) = rx.try_recv() {
            sent.push(msg);
        }
        (db, sent)
    }

    #[tokio::test]
    async fn matching_hash_confirms_settings_without_sending() {
        let settings = settings();
        let db = MockDeviceStore::new().with_settings(settings.clone());

        let (db, sent) = sync(db, &device(), &status(settings.calculate_hash())).await;

        assert!(sent.is_empty());
        db.assert_calls(&[StoreCall::SetDeviceField {
            device_id: DEVICE_ID.to_string(),
            field: RecordedField::ConfigConfirmed(true),
        }]);
    }

    #[tokio::test]
    async fn already_confirmed_device_is_not_written_again() {
        let settings = settings();
        let db = MockDeviceStore::new().with_settings(settings.clone());
        let device = Device {
            config_confirmed: true,
            ..device()
        };

        let (db, sent) = sync(db, &device, &status(settings.calculate_hash())).await;

        assert!(sent.is_empty());
        db.assert_calls(&[]);
    }

    #[tokio::test]
    async fn differing_hash_sends_settings_stamped_with_their_hash() {
        let settings = settings();
        let db = MockDeviceStore::new().with_settings(settings.clone());
        let device = Device {
            config_confirmed: true,
            ..device()
        };

        let (db, sent) = sync(db, &device, &status(settings.calculate_hash() + 1)).await;

        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].device_id, DEVICE_ID);
        let DemoControlOut::Settings(sent_settings) = &sent[0].payload else {
            panic!("expected a settings message, got a common control message");
        };
        assert_eq!(
            sent_settings,
            &Settings {
                hash: settings.calculate_hash(),
                ..settings
            }
        );

        let calls = db.calls();
        assert!(
            matches!(
                &calls[0],
                StoreCall::SetDeviceField {
                    device_id,
                    field: RecordedField::ConfigLastSentAt(Some(_)),
                } if device_id == DEVICE_ID
            ),
            "expected the send to be timestamped, got {:?}",
            calls[0]
        );
        assert_eq!(
            calls[1],
            StoreCall::SetDeviceField {
                device_id: DEVICE_ID.to_string(),
                field: RecordedField::ConfigConfirmed(false),
            }
        );
        assert_eq!(calls.len(), 2);
    }

    #[tokio::test]
    async fn resend_is_suppressed_within_the_cooldown() {
        let settings = settings();
        let db = MockDeviceStore::new().with_settings(settings.clone());
        let device = Device {
            config_last_sent_at: Some(Utc::now().timestamp_millis()),
            ..device()
        };

        let (db, sent) = sync(db, &device, &status(settings.calculate_hash() + 1)).await;

        assert!(sent.is_empty());
        // Neither sent nor confirmed, so nothing is written: the timestamp stays
        // put until the cooldown expires.
        db.assert_calls(&[]);
    }

    #[tokio::test]
    async fn resend_happens_once_the_cooldown_expired() {
        let settings = settings();
        let db = MockDeviceStore::new().with_settings(settings.clone());
        let device = Device {
            config_last_sent_at: Some(
                Utc::now().timestamp_millis() - CONFIG_RESEND_COOLDOWN_MS - 1,
            ),
            ..device()
        };

        let (_db, sent) = sync(db, &device, &status(settings.calculate_hash() + 1)).await;

        assert_eq!(sent.len(), 1);
    }

    #[tokio::test]
    async fn device_without_stored_settings_counts_as_confirmed() {
        let db = MockDeviceStore::new();

        let (db, sent) = sync(db, &device(), &status(42)).await;

        assert!(sent.is_empty());
        db.assert_calls(&[StoreCall::SetDeviceField {
            device_id: DEVICE_ID.to_string(),
            field: RecordedField::ConfigConfirmed(true),
        }]);
    }

    #[tokio::test]
    async fn confirmation_clears_the_last_sent_timestamp() {
        let settings = settings();
        let db = MockDeviceStore::new().with_settings(settings.clone());
        let device = Device {
            config_confirmed: true,
            config_last_sent_at: Some(Utc::now().timestamp_millis()),
            ..device()
        };

        let (db, sent) = sync(db, &device, &status(settings.calculate_hash())).await;

        assert!(sent.is_empty());
        db.assert_calls(&[StoreCall::SetDeviceField {
            device_id: DEVICE_ID.to_string(),
            field: RecordedField::ConfigLastSentAt(None),
        }]);
    }
}

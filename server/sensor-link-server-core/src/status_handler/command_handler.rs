use sensor_link_protocol::cmd::{Cmd, CommandPayload};

use crate::{
    device::{Command, CommandIntoError, Device, DeviceFieldType, DeviceStatusLike},
    status_handler::{ControlMessageOut, DeviceControlOut},
    store_traits::DeviceStore,
};

pub struct CommandHandler<'a, DS: DeviceStore> {
    db: &'a DS,
}

impl<'a, DS: DeviceStore> CommandHandler<'a, DS> {
    pub fn new(db: &'a DS) -> Self {
        Self { db }
    }
}

impl<'a, DS: DeviceStore> CommandHandler<'a, DS> {
    pub async fn check_send_command<DT, DevStatus: DeviceStatusLike>(
        &self,
        device: &Device<DT, DevStatus>,
        device_status: &DevStatus,
    ) -> anyhow::Result<Option<ControlMessageOut>> {
        let cmd: Cmd = match device.command.clone().try_into() {
            Ok(cmd) => cmd,
            Err(CommandIntoError::NoCommand) => return Ok(None),
            Err(CommandIntoError::Unsupported) => {
                return Err(anyhow::anyhow!("Unsupported command"))
            }
        };
        let skip = match cmd.clone() {
            Cmd::Start => device_status.is_active_or_idle(),
            Cmd::Stop => device_status.is_inactive(),
            _ => false,
        };
        if skip {
            self.db
                .set_device_field(&device.id, DeviceFieldType::Command(Command::None))
                .await?;
            Ok(None)
        } else {
            Ok(Some(ControlMessageOut {
                device_id: device.id.clone(),
                payload: DeviceControlOut::DeviceCommand(CommandPayload { cmd }),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde::{Deserialize, Serialize};

    use crate::{device::DeviceStatusLike, store_traits::test::MockStore};

    use super::*;

    /// Minimal mock status for command handler tests.
    #[derive(Clone, Default, Serialize, Deserialize)]
    struct MockStatus {
        active_or_idle: bool,
        inactive: bool,
    }

    impl DeviceStatusLike for MockStatus {
        type Status = bool;
        fn status(&self) -> &bool {
            &self.active_or_idle
        }
        fn is_active_or_idle(&self) -> bool {
            self.active_or_idle
        }
        fn is_inactive(&self) -> bool {
            self.inactive
        }
    }

    #[tokio::test]
    async fn test_no_command_returns_ok_without_sending() {
        let store = MockStore::default();
        let device: Device<(), ()> = Device {
            id: "dev_no_cmd".to_string(),
            command: Command::None,
            ..Default::default()
        };
        let msg = CommandHandler::new(&store)
            .check_send_command(&device, &())
            .await
            .unwrap();
        assert!(msg.is_none());
    }

    #[tokio::test]
    async fn test_start_when_already_active_clears_command_in_db() {
        let captured: Arc<Mutex<Option<DeviceFieldType<(), ()>>>> = Arc::new(Mutex::new(None));
        let captured_clone = captured.clone();
        let store = MockStore {
            set_device_field: Some(Arc::new(move |_, value| {
                *captured_clone.lock().unwrap() = Some(value);
                Ok(())
            })),
            ..Default::default()
        };
        let device: Device<(), MockStatus> = Device {
            id: "dev_start_active".to_string(),
            command: Command::Start,
            ..Default::default()
        };
        let msg = CommandHandler::new(&store)
            .check_send_command(
                &device,
                &MockStatus {
                    active_or_idle: true,
                    inactive: false,
                },
            )
            .await
            .unwrap();
        assert!(msg.is_none());
        assert!(matches!(
            *captured.lock().unwrap(),
            Some(DeviceFieldType::Command(Command::None))
        ));
    }

    #[tokio::test]
    async fn test_start_when_already_idle_clears_command_in_db() {
        let captured: Arc<Mutex<Option<DeviceFieldType<(), ()>>>> = Arc::new(Mutex::new(None));
        let captured_clone = captured.clone();
        let store = MockStore {
            set_device_field: Some(Arc::new(move |_, value| {
                *captured_clone.lock().unwrap() = Some(value);
                Ok(())
            })),
            ..Default::default()
        };
        let device: Device<(), MockStatus> = Device {
            id: "dev_start_idle".to_string(),
            command: Command::Start,
            ..Default::default()
        };
        let msg = CommandHandler::new(&store)
            .check_send_command(
                &device,
                &MockStatus {
                    active_or_idle: true,
                    inactive: false,
                },
            )
            .await
            .unwrap();
        assert!(msg.is_none());
        assert!(matches!(
            *captured.lock().unwrap(),
            Some(DeviceFieldType::Command(Command::None))
        ));
    }

    #[tokio::test]
    async fn test_start_when_inactive_sends_start_to_mqtt() {
        let store = MockStore::default();
        let device: Device<(), ()> = Device {
            id: "dev_start_inactive".to_string(),
            command: Command::Start,
            ..Default::default()
        };
        let msg = CommandHandler::new(&store)
            .check_send_command(&device, &())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(msg.device_id, device.id);
        match msg.payload {
            DeviceControlOut::DeviceCommand(cmd) => assert_eq!(cmd.cmd, Cmd::Start),
            _ => panic!("Expected DeviceCommand(Start)"),
        }
    }

    #[tokio::test]
    async fn test_stop_when_already_inactive_clears_command_in_db() {
        let captured: Arc<Mutex<Option<DeviceFieldType<(), ()>>>> = Arc::new(Mutex::new(None));
        let captured_clone = captured.clone();
        let store = MockStore {
            set_device_field: Some(Arc::new(move |_, value| {
                *captured_clone.lock().unwrap() = Some(value);
                Ok(())
            })),
            ..Default::default()
        };
        let device: Device<(), MockStatus> = Device {
            id: "dev_stop_inactive".to_string(),
            command: Command::Stop,
            ..Default::default()
        };
        let msg = CommandHandler::new(&store)
            .check_send_command(
                &device,
                &MockStatus {
                    active_or_idle: false,
                    inactive: true,
                },
            )
            .await
            .unwrap();
        assert!(msg.is_none());
        assert!(matches!(
            *captured.lock().unwrap(),
            Some(DeviceFieldType::Command(Command::None))
        ));
    }

    #[tokio::test]
    async fn test_stop_when_active_sends_stop_to_mqtt() {
        let store = MockStore::default();
        let device: Device<(), ()> = Device {
            id: "dev_stop_active".to_string(),
            command: Command::Stop,
            ..Default::default()
        };
        let msg = CommandHandler::new(&store)
            .check_send_command(&device, &())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(msg.device_id, device.id);
        match msg.payload {
            DeviceControlOut::DeviceCommand(cmd) => assert_eq!(cmd.cmd, Cmd::Stop),
            _ => panic!("Expected DeviceCommand(Stop)"),
        }
    }

    #[tokio::test]
    async fn test_reboot_always_sends_regardless_of_status() {
        let store = MockStore::default();
        let device: Device<(), ()> = Device {
            id: "dev_reboot".to_string(),
            command: Command::Reboot,
            ..Default::default()
        };
        let msg = CommandHandler::new(&store)
            .check_send_command(&device, &())
            .await
            .unwrap()
            .unwrap();
        match msg.payload {
            DeviceControlOut::DeviceCommand(cmd) => assert_eq!(cmd.cmd, Cmd::Reboot),
            _ => panic!("Expected DeviceCommand(Reboot)"),
        }
    }
}

//! Provides access to (meta-)data about the current application/sensor

use demo_protocol::DeviceType;

pub mod firmware;
pub use sensor_link_firmware::meta::{bootloader, config, DeviceMetaDataProvider};

pub fn fw_version() -> &'static str {
    const PROD_VERSION: Option<&str> = option_env!("BUILD_VERSION");
    const DEV_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "-dev");
    PROD_VERSION.unwrap_or(DEV_VERSION)
}

const GIT_REV: Option<&str> = option_env!("GIT_REV");

pub fn git_rev() -> &'static str {
    GIT_REV.unwrap_or("dev")
}

pub struct DeviceDescriptor {
    bootloader_version: &'static str,
    device_type: DeviceType,
    hw_rev: &'static str,
}

impl DeviceDescriptor {
    pub fn new(
        bootloader_version: &'static str,
        device_type: DeviceType,
        hw_rev: &'static str,
    ) -> Self {
        Self {
            bootloader_version,
            device_type,
            hw_rev,
        }
    }
}

impl DeviceMetaDataProvider for DeviceDescriptor {
    type DeviceType = DeviceType;

    fn device_type(&self) -> DeviceType {
        self.device_type
    }

    fn bootloader_version(&self) -> &'static str {
        self.bootloader_version
    }

    fn git_rev() -> &'static str {
        git_rev()
    }

    fn fw_version() -> &'static str {
        fw_version()
    }

    fn hw_rev(&self) -> &'static str {
        self.hw_rev
    }
}

pub fn print_boot_message(device_type: firmware::DeviceType, label: &str) {
    let build_version = fw_version();
    let git_revision = git_rev();
    log::info!(target: "FW", "******");
    log::info!(target: "FW", "# Starting {label} {device_type:?}");
    log::info!(target: "FW", "Firmware v{build_version} ({git_revision})");
    log::info!(target: "FW", "******\n");
}

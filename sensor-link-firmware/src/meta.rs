//! Access to (meta-)data about the current application/sensor.

pub mod bootloader;
pub mod config;
pub mod firmware;
pub mod section_header;

pub use firmware::DeviceType;

use sensor_link_protocol::info::{DeviceInfoV2, VersionString};

pub trait DeviceMetaDataProvider {
    /// Wire representation of the device type. Generic so this trait stays sensor-agnostic;
    /// an implementation pins this to its own device-type enum.
    type DeviceType;

    fn device_type(&self) -> Self::DeviceType;
    fn bootloader_version(&self) -> &'static str;
    fn git_rev() -> &'static str;
    fn fw_version() -> &'static str;
    fn device_info(
        &self,
        modem_model: VersionString,
        modem_fw_version: VersionString,
    ) -> DeviceInfoV2<Self::DeviceType> {
        DeviceInfoV2 {
            device_type: self.device_type(),
            fw_version: heapless::String::try_from(Self::fw_version()).unwrap(),
            fw_rev: heapless::String::try_from(Self::git_rev()).unwrap(),
            bootloader_version: heapless::String::try_from(self.bootloader_version()).unwrap(),
            modem_model,
            modem_fw_version,
        }
    }
}

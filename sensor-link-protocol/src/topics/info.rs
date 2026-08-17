//! Information about the device that changes rarely.
//!

use super::TopicPayloadSerialize;
use crate::MAX_MESSAGE_LEN;
use serde::{Deserialize, Serialize};

pub const VERSION_STRING_MAX_LEN: usize = 16;
/// Firmware Version string.
/// Probably in semver format with buildnumber: `<MAJOR>.<MINOR>.<PATCH>-<BUILD>`
/// But can also contain other postfixes such as 'rc-2' or 'beta-1'.
pub type VersionString = heapless::String<VERSION_STRING_MAX_LEN>;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub struct DeviceInfoV3<D> {
    pub device_type: D,
    /// Firmware version of the device
    pub fw_version: VersionString,
    /// Short git commit hash for the firmware
    pub fw_rev: VersionString,
    /// Bootloader version of the device
    pub bootloader_version: VersionString,
    /// Hardware revision of the device
    pub hw_rev: VersionString,

    /// Modem model
    pub modem_model: VersionString,
    /// Modem firmware version
    pub modem_fw_version: VersionString,
}

impl<D: Serialize> TopicPayloadSerialize<MAX_MESSAGE_LEN> for DeviceInfoV3<D> {}

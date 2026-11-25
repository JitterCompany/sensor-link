//! Network logic.
//!
//! The generic network task and protocol-client interface live in
//! `sensor_link_firmware::logic::network`. The demo upload items (Sendable
//! plumbing) are defined here.

pub use sensor_link_firmware::logic::network::{
    network_task, FirmwareDownloadStart, NetworkClient, ReadOutcome, DEFAULT_TIMEOUT_S,
    INFINITE_TIMEOUT_S, NETWORK_DISCONNECTED_TIMEOUT_MS,
};

pub mod upload;
pub mod upload_items;

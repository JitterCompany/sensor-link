//! Dispatch: buffering and serializing the demo data stream for upload.
//!
//! The generic dispatch pipeline (task and helpers) lives in sensor-link-firmware.
//! Only the demo data buffer and its serializer are defined here.

pub mod demo_buffer;
pub(crate) mod demo_data;
mod serializer_wrappers;

use sensor_link_firmware::logic::dispatch::{buffer, drain};

pub use sensor_link_firmware::logic::dispatch::{
    barrier::DispatchBarrier, confirmable, dispatch_task, serialization, SerializedEvent,
};

pub use crate::storage::dispatch_store::{ConfirmHandle, DispatchStore};

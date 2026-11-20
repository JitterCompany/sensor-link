#![cfg_attr(not(any(test, feature = "use-std")), no_std)]
#![allow(async_fn_in_trait)]
//! demo-firmware-core implements the hardware-independent logic of the demo
//! device on top of `sensor-link-firmware`, and re-exports it. The firmware and
//! the mock device both run this crate, so the mock behaves like the real board.

mod adapters;
pub mod dsp;
pub mod logic;
pub mod meta;
pub mod storage;
pub mod utils;

pub use sensor_link_firmware::{define_pool, pool};

#[cfg(any(test, feature = "use-std"))]
pub use sensor_link_firmware::std_monotonic_driver;

// Re-exported so the application crates need no direct dependency on them
pub use demo_protocol;
use demo_protocol::TopicFromDevice;
pub use embassy_sync;
pub use embedded_hal;
pub use embedded_hal_async;
pub use embedded_io_async;
pub use futures;
pub use heapless;
pub use log;
pub use paste;
pub use sensor_link_firmware::{
    bootloader, drivers, init_monotonic, meta::firmware::ApplicationHeader, monotonic_time, mqtt,
    sync, traits,
};
pub use static_cell;

pub(crate) type SerializedSendable<const N: usize> =
    sensor_link_firmware::serialize::SerializedSendable<N, TopicFromDevice>;
pub(crate) type Sendable = dyn sensor_link_firmware::serialize::Sendable<TopicFromDevice>;

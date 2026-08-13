//! Hardware-independent drivers.
mod atclient;

pub mod ads124s08;
pub mod boot_stats;
pub mod bq25672;
pub mod quectel;
pub mod rtd;
pub mod spi_flash;
pub mod time;
pub mod timer_queue;

pub use atclient::ATClient;
pub use bq25672::{BatteryCharger, BQ25672};

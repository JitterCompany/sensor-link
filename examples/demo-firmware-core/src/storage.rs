//! Flash storage of the demo device: settings and calibration files, and the
//! upload queue streams.

pub use sensor_link_firmware::storage::{
    backend,
    common::{queue, stream_store},
    dispatch_store, flash_db,
};

pub mod files;
pub mod streams;

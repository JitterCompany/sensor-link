//! Memory pools for the upload queue.
#![allow(non_upper_case_globals)]

use demo_firmware_core::{
    define_pool,
    logic::upload_items::{SerEvent, SerSensorData},
};

// Pool definitions with their minimum capacity
define_pool!(SensorDataPool, SerSensorData, 5);
define_pool!(EventPool, SerEvent, 5);

//! Pools backing the upload queue: one each for sensor data, events and log
//! records.

#![allow(non_upper_case_globals)]

use sensor_link_firmware::define_pool;

use crate::device::upload::{SerEvent, SerLog, SerSensorData};

define_pool!(SensorDataPool, SerSensorData, 5);
define_pool!(EventPool, SerEvent, 5);
define_pool!(LogPool, SerLog, 5);

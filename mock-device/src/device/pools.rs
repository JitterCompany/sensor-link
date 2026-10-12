//! Pools backing the upload queue: one each for sensor data and events.

#![allow(non_upper_case_globals)]

use sensor_link_firmware::define_pool;

use crate::device::upload::{SerEvent, SerSensorData};

define_pool!(SensorDataPool, SerSensorData, 5);
define_pool!(EventPool, SerEvent, 5);

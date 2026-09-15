//! Pools backing the upload queue.
//!
//! Ported from `mock-device`'s `device::zonneboiler::pools`, with a third pool
//! for the log records the dispatch pipeline now carries.

#![allow(non_upper_case_globals)]

use sensor_link_firmware::define_pool;

use crate::device::upload::{SerEvent, SerLog, SerSensorData};

define_pool!(SensorDataPool, SerSensorData, 5);
define_pool!(EventPool, SerEvent, 5);
define_pool!(LogPool, SerLog, 5);

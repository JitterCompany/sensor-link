//! This module contains the data set structs and their implementations.
//!
//! The data set structs are used to represent data sets to assign incoming data to.
//!
//! ```
use serde::{Deserialize, Serialize};

use crate::DataStoreId;

#[derive(Debug, Deserialize, Serialize)]
pub struct NewDataSet {
    pub id: Option<DataStoreId>,
    pub sensor_id: String,
    pub measuring_point_id: Option<DataStoreId>,
    pub start: i64,
    pub configuration_id: Option<String>,
}

impl NewDataSet {
    pub fn new(sensor_id: String, measuring_point_id: Option<DataStoreId>) -> Self {
        Self {
            id: None,
            sensor_id,
            measuring_point_id,
            configuration_id: None,
            start: i64::MIN,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DataSet {
    pub id: DataStoreId,
    pub sensor_id: String,
    pub measuring_point_id: Option<DataStoreId>,
    pub configuration_id: Option<String>,
    pub start: i64,
}

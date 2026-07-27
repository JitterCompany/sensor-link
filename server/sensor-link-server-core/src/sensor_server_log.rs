use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct SensorServerLog {
    #[serde(rename = "_id")]
    pub id: Option<String>,
    #[serde(rename = "sensorID")]
    pub sensor_id: String,
    #[serde(rename = "groupID")]
    pub group_id: Option<String>,
    #[serde(rename = "userID")]
    pub user_id: String,
    pub timestamp: DateTime<Utc>,
    pub duration: u32,
    #[serde(rename = "type")]
    pub _type: String,
    pub header: String,
    pub body: Vec<String>,
}

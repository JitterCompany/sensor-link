use super::TopicPayloadSerialize;
use crate::MAX_MESSAGE_LEN;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "lowercase")]
pub struct Timestamp {
    pub time: i64,
}

impl TopicPayloadSerialize<MAX_MESSAGE_LEN> for Timestamp {}

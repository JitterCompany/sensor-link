use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{DataStoreId, MeteorId, TimeRange};

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[allow(non_camel_case_types)]
pub enum EventType {
    info = 1,
    warning,
    error,
    sms,
    email,
    alarm,
    debug,
    diagnostic,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone, Copy, Default)]
pub enum SendStatus {
    #[default]
    NotSent,
    Sending,
    Sent,
    Failed,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Comment {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(rename = "userID")]
    pub user_id: String,
    #[serde(with = "chrono::serde::ts_seconds")]
    pub time: DateTime<Utc>,
    pub msg: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        with = "chrono::serde::ts_seconds_option",
        default
    )]
    pub edited: Option<DateTime<Utc>>,
}

/// Generic event stored in the database and returned by the API.
///
/// `C` is the contact details type; use `()` if not needed.
/// `D` is the event details type; use `()` if not needed.
#[derive(Debug, Serialize)]
pub struct Event<C, D> {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(rename = "type")]
    pub _type: EventType,
    pub code: u32,
    pub timestamp: DateTime<Utc>,
    pub sent: Option<SendStatus>,
    #[serde(rename = "projectID")]
    pub project_id: Option<String>,
    #[serde(rename = "groupID")]
    pub group_id: Option<String>,
    #[serde(rename = "hasServerTime")]
    pub has_server_time: bool,
    #[serde(rename = "contactDetails")]
    pub contact_details: Option<C>,
    #[serde(rename = "sensorID")]
    pub device_id: Option<String>,
    #[serde(rename = "measuringPointID")]
    pub measuring_point_id: Option<String>,
    #[serde(rename = "projectName")]
    pub project_name: Option<String>,
    #[serde(rename = "clusterName")]
    pub cluster_name: Option<String>,
    #[serde(rename = "pointName")]
    pub measuring_point_name: Option<String>,
    #[serde(rename = "sensorName")]
    pub device_name: Option<String>,
    #[serde(rename = "groupName")]
    pub group_name: Option<String>,
    pub message: Option<String>,
    pub msg: Option<String>,
    #[serde(rename = "msgEn")]
    pub msg_en: Option<String>,
    pub comments: Vec<Comment>,
    #[serde(rename = "eventDetails", flatten)]
    pub event_details: Option<D>,
}

/// Generic event ready to be written to the database.
///
/// `C` is the contact details type; use `()` if not needed.
/// `D` is the event details type; use `()` if not needed.
#[derive(Debug)]
pub struct NewEvent<C, D> {
    pub _type: EventType,
    pub code: u32,
    pub timestamp: DateTime<Utc>,
    pub project_id: Option<String>,
    pub group_id: Option<String>,
    pub has_server_time: bool,
    pub contact_details: Option<C>,
    pub message: Option<String>,
    pub device_id: Option<String>,
    pub measuring_point_id: Option<String>,
    pub event_details: Option<D>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EventStats {
    pub(crate) count: i32,
    pub(crate) code: Option<u32>,
    #[serde(rename = "type")]
    _type: EventType,
    msg: Option<String>,
    msg_en: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HourStat {
    pub hour: i32,
    pub count: i32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectHourStats {
    pub project_id: String,
    pub project_name: Option<String>,
    pub hour_stats: Vec<HourStat>,
}

#[derive(Debug, Serialize)]
pub struct EventsWithStats<C, D> {
    pub events: Vec<Event<C, D>>,
    pub code_stats: Vec<EventStats>,
    pub type_stats: Vec<EventStats>,
    pub hour_stats: Vec<ProjectHourStats>,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct EventQuery {
    #[serde(flatten)]
    pub params: EventQueryParams,
    /// [EventQuery::code] field separate since `#[serde(flatten)]` doesn't work with `Option<Vec<u32>>`. It expects `String` instead of `u32`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<Vec<u32>>,
    /// sort direction: 1 for ascending, -1 for descending
    pub sort: i32,
    pub limit: Option<i64>,
    pub skip: Option<i64>,
    pub sort_max_perc: Option<i32>,
    #[serde(default)]
    pub only_stats: bool,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct EventQueryParams {
    #[serde(
        rename(deserialize = "timeRange"),
        flatten,
        skip_serializing_if = "Option::is_none"
    )]
    pub time_range: Option<TimeRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub types: Option<Vec<EventType>>,
    #[serde(
        rename(deserialize = "groupID"),
        skip_serializing_if = "Option::is_none"
    )]
    pub group_id: Option<MeteorId>,
    #[serde(
        rename(deserialize = "projectID"),
        skip_serializing_if = "Option::is_none"
    )]
    pub project_id: Option<MeteorId>,
    #[serde(
        rename(deserialize = "projectIDs"),
        skip_serializing_if = "Option::is_none"
    )]
    pub project_ids: Option<Vec<MeteorId>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measuring_point_ids: Option<Vec<DataStoreId>>,
    #[serde(
        rename(deserialize = "sensorID"),
        skip_serializing_if = "Option::is_none"
    )]
    pub device_id: Option<String>,
    #[serde(
        rename(deserialize = "contactID"),
        skip_serializing_if = "Option::is_none"
    )]
    pub contact_id: Option<DataStoreId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sent: Option<SendStatus>,

    /// To filter events that have contact details
    pub has_contact_details: Option<bool>,
}

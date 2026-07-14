use super::TopicPayloadSerialize;
use crate::{Microseconds, MAX_EVENT_LEN};
use serde::{Deserialize, Serialize};

const MAX_DESC_MSG_LEN: usize = 16;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Desc {
    #[serde(rename = "m")]
    pub msg: heapless::String<MAX_DESC_MSG_LEN>,
}

impl Desc {
    pub fn empty() -> Self {
        Self {
            msg: heapless::String::new(),
        }
    }
}

impl core::fmt::Write for Desc {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.msg.write_str(s)
    }
}

/// All manufacturer-agnostic events that can be sent from device to the server.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "lowercase")]
pub enum Event {
    Booted(Desc),
    Started(Desc),
    Stopped(Desc),
    Blink,

    NetworkConnected,
    NetworkDisconnected,

    /// Failed to connect to the server (typically after multiple failed attempts)
    ContactTimeout(Desc),

    FirmwareUpdateStarted,
    FirmwareUpdateFailed,
    FirmwareUpdateReceived,

    /// Missing calibration data: measurement is possible but with suboptimal accuracy
    NotCalibrated,

    /// Sensor is not mounted level: measurement still possible but with suboptimal accuracy
    NotLevel(Desc),

    /// Self test failed: no sensor detected
    SelfTestNoSensor(Desc),

    /// Self test failed: sensor is probably defect (self-test did not meet spec)
    SelfTestSensorDefect(Desc),

    /// Self test completed: no critical errors
    SelfTestComplete(Desc),

    /// Something is wrong with the sensor (usually fatal)
    SensorError(Desc),

    /// Unexpected but non-critical condition
    ProcessingWarning(Desc),
}

impl Event {
    pub const fn is_urgent(&self) -> bool {
        match self {
            Event::Booted(_) => false,
            Event::Blink => false,

            Event::Started(_) => true,
            Event::Stopped(_) => true,

            Event::NetworkConnected => false,
            Event::NetworkDisconnected => false,
            Event::ContactTimeout(_) => false,

            Event::FirmwareUpdateStarted => true,
            Event::FirmwareUpdateFailed => true,
            Event::FirmwareUpdateReceived => true,

            Event::SensorError(_) => true,
            Event::ProcessingWarning(_) => true,
            Event::NotCalibrated => true,
            Event::NotLevel(_) => true,
            Event::SelfTestNoSensor(_) => true,
            Event::SelfTestSensorDefect(_) => true,
            Event::SelfTestComplete(_) => true,
        }
    }

    pub fn contact_timeout(message: &str) -> Self {
        Event::ContactTimeout(desc_from_str(message))
    }

    pub fn started(message: &str) -> Self {
        Event::Started(desc_from_str(message))
    }

    pub fn stopped(message: &str) -> Self {
        Event::Stopped(desc_from_str(message))
    }

    pub fn booted(message: &str) -> Self {
        Event::Booted(desc_from_str(message))
    }

    pub fn sensor_error(message: &str) -> Self {
        Event::SensorError(desc_from_str(message))
    }

    pub fn processing_warning(message: &str) -> Self {
        Event::ProcessingWarning(desc_from_str(message))
    }

    pub fn selftest_no_sensor(message: &str) -> Self {
        Event::SelfTestNoSensor(desc_from_str(message))
    }

    pub fn selftest_defect(message: &str) -> Self {
        Event::SelfTestSensorDefect(desc_from_str(message))
    }

    pub fn not_level(message: &str) -> Self {
        Event::NotLevel(desc_from_str(message))
    }
}

fn desc_from_str(message: &str) -> Desc {
    let n = message.len().min(MAX_DESC_MSG_LEN);
    Desc {
        msg: heapless::String::try_from(&message[..n]).unwrap(),
    }
}

/// Generic event payload: wraps any event type with a millisecond timestamp.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "lowercase")]
pub struct EventPayload<E> {
    #[serde(rename = "e")]
    pub event: E,
    /// Timestamp in milliseconds since epoch
    #[serde(rename = "t")]
    pub ts: i64,
}

impl<E> EventPayload<E> {
    pub fn from_event_at(event: E, timestamp: Microseconds) -> Self {
        Self {
            event,
            ts: timestamp.milliseconds(),
        }
    }
}

impl<E: Serialize> TopicPayloadSerialize<MAX_EVENT_LEN> for EventPayload<E> {}

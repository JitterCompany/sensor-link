use core::fmt::Write;

use serde::{Deserialize, Serialize};

use super::TopicPayloadSerialize;
use crate::{MAX_LOG_LEN, MAX_LOG_MSG_LEN, MAX_LOG_TARGET_LEN};

/// Severity of a [`LogMessage`], mirroring the levels of the `log` crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
    }
}

impl From<log::Level> for LogLevel {
    fn from(level: log::Level) -> Self {
        match level {
            log::Level::Error => LogLevel::Error,
            log::Level::Warn => LogLevel::Warn,
            log::Level::Info => LogLevel::Info,
            log::Level::Debug => LogLevel::Debug,
            log::Level::Trace => LogLevel::Trace,
        }
    }
}

/// A single log line, published on [`TopicFromDevice::Log`](crate::TopicFromDevice::Log).
///
/// Both the target and the message body are capped
/// ([`MAX_LOG_TARGET_LEN`] / [`MAX_LOG_MSG_LEN`]) so the serialized payload
/// always fits [`MAX_LOG_LEN`]; anything longer is truncated rather than
/// dropped, since a shortened log line is more useful than none.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogMessage {
    /// Severity of the log record.
    #[serde(rename = "l")]
    pub level: LogLevel,

    /// Target (usually the module path) the log record came from.
    #[serde(rename = "tg")]
    pub target: heapless::String<MAX_LOG_TARGET_LEN>,

    /// Formatted message body.
    #[serde(rename = "m")]
    pub msg: heapless::String<MAX_LOG_MSG_LEN>,

    /// Timestamp in milliseconds since epoch
    #[serde(rename = "t")]
    pub ts: i64,
}

impl LogMessage {
    /// Build a log message, truncating `target` and `message` to what fits.
    pub fn new(level: LogLevel, target: &str, message: &str, ts: i64) -> Self {
        Self::from_args(level, target, format_args!("{message}"), ts)
    }

    /// Build a log message from the (unformatted) arguments of a log record,
    /// truncating `target` and the formatted body to what fits.
    pub fn from_args(level: LogLevel, target: &str, args: core::fmt::Arguments, ts: i64) -> Self {
        let mut msg = heapless::String::new();
        // Writing into a `Truncating` never fails.
        let _ = Truncating(&mut msg).write_fmt(args);

        Self {
            level,
            target: truncated(target),
            msg,
            ts,
        }
    }
}

impl TopicPayloadSerialize<MAX_LOG_LEN> for LogMessage {}

fn truncated<const N: usize>(text: &str) -> heapless::String<N> {
    let mut output = heapless::String::new();
    // Writing into a `Truncating` never fails.
    let _ = Truncating(&mut output).write_str(text);
    output
}

/// [`core::fmt::Write`] adapter that appends whatever still fits in the target
/// string and silently drops the rest.
struct Truncating<'a, const N: usize>(&'a mut heapless::String<N>);

impl<const N: usize> Write for Truncating<'_, N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for c in s.chars() {
            if self.0.push(c).is_err() {
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Size of the topic header the firmware's serializer prepends to the
    /// payload within `MAX_LOG_LEN` (`sensor_link_firmware::serialize`).
    const TOPIC_HEADER_SIZE: usize = 8;

    #[test]
    fn test_serialization() {
        let message = LogMessage::new(LogLevel::Warn, "Network", "Connect Error", 17491303460000);
        let serialized = message.serialize_topic_payload().unwrap();
        assert_eq!(
            std::str::from_utf8(&serialized).unwrap(),
            r#"{"l":"warn","tg":"Network","m":"Connect Error","t":17491303460000}"#
        );
    }

    #[test]
    fn test_truncation() {
        let long_target = "a".repeat(MAX_LOG_TARGET_LEN + 10);
        let long_message = "b".repeat(MAX_LOG_MSG_LEN + 10);
        let message = LogMessage::new(LogLevel::Error, &long_target, &long_message, 0);

        assert_eq!(message.target.len(), MAX_LOG_TARGET_LEN);
        assert_eq!(message.msg.len(), MAX_LOG_MSG_LEN);
    }

    /// A truncated message must never split a multi-byte character.
    #[test]
    fn test_truncation_on_char_boundary() {
        // One ASCII character followed by 3-byte characters, so the capacity
        // boundary falls in the middle of the last character that would fit.
        let target = std::format!("x{}", "€".repeat(MAX_LOG_TARGET_LEN));
        let message = LogMessage::new(LogLevel::Info, &target, "", 0);

        let expected_len = 1 + ((MAX_LOG_TARGET_LEN - 1) / 3) * 3;
        assert_eq!(message.target.len(), expected_len);
        assert!(expected_len < MAX_LOG_TARGET_LEN);
    }

    /// The worst-case message must still fit the payload buffer.
    ///
    /// Note that `MAX_LOG_LEN` covers the topic header too, so the buffer a
    /// record is serialized into is smaller than `MAX_LOG_LEN` by the header
    /// size the firmware's serializer prepends.
    #[test]
    fn test_max_size_fits() {
        let message = LogMessage::new(
            LogLevel::Error,
            &"a".repeat(MAX_LOG_TARGET_LEN),
            &"b".repeat(MAX_LOG_MSG_LEN),
            i64::MIN,
        );

        let mut payload = [0u8; MAX_LOG_LEN - TOPIC_HEADER_SIZE];
        assert!(message
            .serialize_topic_payload_to_slice(&mut payload)
            .is_ok());
    }

    /// A message with more escaped characters than the headroom on
    /// [`MAX_LOG_LEN`] does not fit, and is reported as such rather than
    /// silently producing a short payload.
    #[test]
    fn test_escape_heavy_message_does_not_fit() {
        let message = LogMessage::new(
            LogLevel::Error,
            &"a".repeat(MAX_LOG_TARGET_LEN),
            &"\"".repeat(MAX_LOG_MSG_LEN),
            i64::MIN,
        );

        let mut payload = [0u8; MAX_LOG_LEN - TOPIC_HEADER_SIZE];
        assert!(message
            .serialize_topic_payload_to_slice(&mut payload)
            .is_err());
    }
}

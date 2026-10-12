#![cfg_attr(not(any(test, feature = "use-std")), no_std)]
#![allow(async_fn_in_trait)]

use sensor_link_protocol::{
    device_log::LogMessage, event::EventPayload, Topic, TopicFromDevice, TopicPayloadSerialize,
    MAX_EVENT_LEN, MAX_LOG_LEN,
};
use serde::Serialize;

// Re-exported at the crate root so the `define_pool!` macro (which expands to
// `$crate::heapless`/`$crate::paste` paths) resolves in any downstream crate.
pub use heapless;
pub use paste;
pub use sensor_link_protocol;

pub mod bootloader;
pub mod drivers;
pub mod logic;
pub mod meta;
pub mod monotonic_time;
pub mod mqtt;
pub mod pool;
pub mod serialize;
#[cfg(any(test, feature = "use-std"))]
pub mod std_monotonic_driver;
pub mod storage;
pub mod sync;
pub mod traits;
pub mod utils;

// Exposed (not just `cfg(test)`) so downstream sensor test-suites can reuse these
// fixtures via the `test-mono` feature.
// Exposed (not just `cfg(test)`) so downstream sensor test-suites can reuse these
// fixtures via the `test-mono` feature.
#[cfg(any(test, feature = "test-mono"))]
pub mod tests {
    pub mod mock {
        pub mod mock_filestore;
        pub mod mock_timeout;
        pub mod mock_timer;
        pub mod mock_trigger;
        // jitter-internal only (depends on a cfg(test) MockError).
        #[cfg(test)]
        pub mod mock_flash;
    }
}

use crate::serialize::{AsSendable, SerializedSendable};

/// Serialize an event for any device topic type that can express the shared Jitter event topic.
///
/// The output topic is generic: the impl builds the manufacturer-generic
/// [`TopicFromDevice::Event`] and converts it into the caller's topic type `T`, so a frogwatch
/// device (whose topic wraps it as `Common(Event)`) gets the same payload addressed to its own
/// topic enum. This keeps the dispatch pipeline generic over the wire topic.
impl<E: Serialize, T: Topic + From<TopicFromDevice>> serialize::AsSendable<MAX_EVENT_LEN, T>
    for EventPayload<E>
{
    type Error = serialize::BuildError;
    const MAX_SENDABLE_LENGTH: usize = MAX_EVENT_LEN;

    #[inline]
    fn as_sendable(&self) -> Result<SerializedSendable<{ MAX_EVENT_LEN }, T>, Self::Error> {
        let mut builder = serialize::BuilderWithTopic::new(T::from(TopicFromDevice::Event));
        let len = self
            .serialize_topic_payload_to_slice(builder.payload_buffer())
            .map_err(|_| serialize::BuildError::PayloadTooLong)?;
        builder.create_with_payload_length(len)
    }
}

/// Serialize a log message for any device topic type that can express the shared Jitter log topic.
///
/// Generic over the output topic for the same reason as the [`EventPayload`] impl above: the
/// dispatch pipeline carries log records addressed to the caller's own topic type.
impl<T: Topic + From<TopicFromDevice>> AsSendable<MAX_LOG_LEN, T> for LogMessage {
    type Error = serialize::BuildError;
    const MAX_SENDABLE_LENGTH: usize = MAX_LOG_LEN;

    #[inline]
    fn as_sendable(&self) -> Result<SerializedSendable<{ MAX_LOG_LEN }, T>, Self::Error> {
        let mut builder = serialize::BuilderWithTopic::new(T::from(TopicFromDevice::Log));
        let len = self
            .serialize_topic_payload_to_slice(builder.payload_buffer())
            .map_err(|_| serialize::BuildError::PayloadTooLong)?;
        builder.create_with_payload_length(len)
    }
}

#[cfg(test)]
mod sendable_tests {
    use sensor_link_protocol::{
        device_log::LogLevel, MAX_LOG_MSG_LEN, MAX_LOG_TARGET_LEN, MAX_TOPIC_LEN,
    };

    use super::*;

    /// A log message must serialize onto the device's log topic.
    #[test]
    fn test_log_message_as_sendable() {
        let message = LogMessage::new(LogLevel::Warn, "Network", "Connect Error", 17491303460000);

        let sendable: SerializedSendable<MAX_LOG_LEN, TopicFromDevice> =
            message.as_sendable().unwrap();

        assert_eq!(sendable.topic().unwrap(), TopicFromDevice::Log);
        assert_eq!(
            core::str::from_utf8(sendable.payload_bytes()).unwrap(),
            r#"{"l":"warn","tg":"Network","m":"Connect Error","t":17491303460000}"#
        );
    }

    /// The worst-case log message must still fit the sendable's payload buffer.
    #[test]
    fn test_max_size_log_message_as_sendable() {
        let message = LogMessage::new(
            LogLevel::Error,
            &"a".repeat(MAX_LOG_TARGET_LEN),
            &"b".repeat(MAX_LOG_MSG_LEN),
            i64::MIN,
        );

        AsSendable::<MAX_LOG_LEN, TopicFromDevice>::as_sendable(&message).unwrap();
        assert!(MAX_LOG_LEN <= MAX_TOPIC_LEN + sensor_link_protocol::MAX_MESSAGE_LEN);
    }
}

#![cfg_attr(not(any(test, feature = "use-std")), no_std)]
#![allow(async_fn_in_trait)]

use sensor_link_protocol::{
    event::EventPayload, Topic, TopicFromDevice, TopicPayloadSerialize, MAX_EVENT_LEN,
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

use crate::serialize::SerializedSendable;

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

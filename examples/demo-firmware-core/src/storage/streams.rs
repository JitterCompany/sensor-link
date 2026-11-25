//! Demo configuration of the generic dispatch stream store.
//!
//! The store machinery itself is generic and lives in [`crate::storage::dispatch_store`]. This
//! module only provides the demo configuration: the [`Stream`] flash layout and the
//! concrete type aliases that pin the generic store to `demo_protocol::TopicFromDevice`.

use demo_protocol::TopicFromDevice;

use crate::storage::{
    dispatch_store::{DispatchStreams, StaticStreamPairStore, StreamPairStore},
    flash_db,
    stream_store::STREAM_OVERHEAD_BYTES,
};

pub use crate::storage::dispatch_store::ConfirmChannels;

pub const BLOCK_SIZE: usize = 4096;

/// Maximum serialized message length
/// tradeoff between storage and network efficiency, see `test::test_stream_efficiency`
pub const MAX_SENSOR_DATA_LEN: usize = 1008;

// Quectel modems limit MQTT payloads to 1500 bytes (MAX_MESSAGE_LEN)
const _: () = assert!(MAX_SENSOR_DATA_LEN <= demo_protocol::MAX_MESSAGE_LEN);

pub const MAX_SERIALIZED_EVENT_LEN: usize = demo_protocol::MAX_EVENT_LEN;

/// Streams that can be stored. Each stream acts like a persistent circular buffer that stores multiple data segments in FIFO order.
#[derive(Debug, Clone, Copy, PartialEq, strum::EnumCount)]
#[repr(u16)]
pub enum Stream {
    Event,
    ProcessingResult,
}

impl Stream {
    pub const fn max_fragment_size(&self) -> usize {
        match self {
            Stream::Event => MAX_SERIALIZED_EVENT_LEN + STREAM_OVERHEAD_BYTES,
            Stream::ProcessingResult => MAX_SENSOR_DATA_LEN + STREAM_OVERHEAD_BYTES,
        }
    }
}

impl flash_db::Object<{ BLOCK_SIZE }> for Stream {
    fn id(&self) -> flash_db::ObjectId {
        *self as flash_db::ObjectId
    }

    fn fragment_size(&self) -> usize {
        self.max_fragment_size()
    }

    fn flash_blocks(&self) -> core::ops::Range<flash_db::block_layer::BlockId> {
        match self {
            Stream::Event => 768..1024, // 256 blocks = ca 1MB
            // 1024 blocks = 4MB. The W25Q256 on this board has room for more
            // (32MB), but the bootloader shares this layout, so keep it stable.
            Stream::ProcessingResult => 1024..2048,
        }
    }
}
impl flash_db::Circular<{ BLOCK_SIZE }> for Stream {
    fn overwrite_on_full(&self) -> bool {
        match self {
            // If the storage for events is full, oldest events are erased to make space for new ones.
            Self::Event => true,

            // If the storage for proocessing results is full, oldest are erased to make space for new ones.
            Self::ProcessingResult => true,
        }
    }
}

impl DispatchStreams<{ BLOCK_SIZE }> for Stream {
    fn event() -> Self {
        Stream::Event
    }
    fn sensor_data() -> Self {
        Stream::ProcessingResult
    }
}

/// Persistent storage for events and sensor data, configured for the demo device.
///
/// Thin alias over the generic [`StreamPairStore`], pinned to the demo [`Stream`] layout and
/// `demo_protocol::TopicFromDevice`.
pub type DemoStore<'a, DB> =
    StreamPairStore<'a, DB, Stream, BLOCK_SIZE, MAX_SENSOR_DATA_LEN, TopicFromDevice>;

/// `'static` wrapper around [`DemoStore`]. Used by the dispatch task.
pub type StaticDemoStore<DB> =
    StaticStreamPairStore<DB, Stream, BLOCK_SIZE, MAX_SENSOR_DATA_LEN, TopicFromDevice>;

#[cfg(test)]
mod test {

    use crate::storage::flash_db::ObjectExt;

    use super::*;

    #[test]
    fn test_sensor_stream_efficiency() {
        let _ = simple_logger::init_with_level(log::Level::Debug).ok();

        let net_bytes_per_frag = MAX_SENSOR_DATA_LEN;
        assert_eq!(
            net_bytes_per_frag + STREAM_OVERHEAD_BYTES,
            Stream::ProcessingResult.max_fragment_size()
        );

        let max_fragments = Stream::ProcessingResult.fragments_per_block();
        let max_bytes = net_bytes_per_frag * max_fragments;
        let overhead = BLOCK_SIZE - max_bytes;
        let overhead_pct = overhead as f32 * 100.0 / BLOCK_SIZE as f32;
        log::info!("Stream store: {max_bytes} bytes per {BLOCK_SIZE}-byte block: {overhead} bytes ({overhead_pct:.2}%) overhead");

        // This threshold is arbitrary, the main purpose of this test is to notice if something changes that makes the storage (much) less efficient.
        // For example, a slight increase in `MAX_MESSAGE_LEN` could mean one less fragment per block can be stored, which can significantly increase the overhead.
        // In that case you might as well increase the message length even further..
        // Note: this overhead is excluding the overhead of the (demo-protocol) serialization format
        assert!(overhead_pct < 2.0);
    }

    #[test]
    fn test_event_stream_efficiency() {
        let _ = simple_logger::init_with_level(log::Level::Debug).ok();

        let net_bytes_per_frag = MAX_SERIALIZED_EVENT_LEN;
        assert_eq!(
            net_bytes_per_frag + STREAM_OVERHEAD_BYTES,
            Stream::Event.max_fragment_size()
        );

        let max_fragments = Stream::Event.fragments_per_block();
        let max_bytes = net_bytes_per_frag * max_fragments;
        let overhead = BLOCK_SIZE - max_bytes;
        let overhead_pct = overhead as f32 * 100.0 / BLOCK_SIZE as f32;
        log::info!("Stream store: {max_bytes} bytes per {BLOCK_SIZE}-byte block in {max_fragments} fragments: {overhead} bytes ({overhead_pct:.2}%) overhead");

        // This threshold is arbitrary, the main purpose of this test is to notice if something changes that makes the storage (much) less efficient.
        // For example, a slight increase in `MAX_MESSAGE_LEN` could mean one less fragment per block can be stored, which can significantly increase the overhead.
        // In that case you might as well increase the message length even further..
        // Note: this overhead is excluding the overhead of the (demo-protocol) serialization format
        assert!(overhead_pct < 6.5);
    }

    #[test]
    fn test_stream_count() {
        use crate::storage::flash_db::Object;

        assert_eq!(Stream::ProcessingResult.fragment_count(), 4092);
        assert_eq!(Stream::ProcessingResult.max_fragment_size(), 1014);
        assert_eq!(Stream::ProcessingResult.fragments_per_block(), 4);
        assert_eq!(Stream::ProcessingResult.flash_blocks().start, 1024);
        assert_eq!(Stream::ProcessingResult.flash_blocks().end, 2048);
    }
}

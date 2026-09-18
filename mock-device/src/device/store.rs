//! Mock-device configuration for the generic dispatch stream store.
//!
//! The store machinery itself is generic and lives in
//! [`sensor_link_firmware::storage::dispatch_store`]. This module only provides
//! the mock-specific configuration: the [`Stream`] flash layout and the concrete
//! type aliases that pin the generic store to
//! [`TopicFromDevice`](sensor_link_protocol::TopicFromDevice).
//!
//! The flash layout matches that of a real device: the mock runs the store
//! against an in-memory flash of the same size, so the same block ranges apply.

use sensor_link_firmware::{
    sensor_link_protocol::{TopicFromDevice, MAX_EVENT_LEN, MAX_LOG_LEN, MAX_MESSAGE_LEN},
    storage::{
        common::stream_store::STREAM_OVERHEAD_BYTES,
        dispatch_store::{DispatchStreams, StaticStreamSetStore, StreamSetStore},
        flash_db,
    },
};

pub use sensor_link_firmware::storage::dispatch_store::ConfirmChannels;

pub const BLOCK_SIZE: usize = 4096;

/// Maximum serialized message length: a tradeoff between storage and network
/// efficiency.
pub const MAX_SENSOR_DATA_LEN: usize = 1008;

// Quectel modems limit MQTT payloads to 1500 bytes (MAX_MESSAGE_LEN)
const _: () = assert!(MAX_SENSOR_DATA_LEN <= MAX_MESSAGE_LEN);

pub const MAX_SERIALIZED_EVENT_LEN: usize = MAX_EVENT_LEN;

/// Files that can be stored.
///
/// The mock's dispatch pipeline stores no files; the flash database is generic
/// over a file type regardless, so this declares the one slot a device
/// realistically needs. Its blocks sit below every stream's range.
// Declared for the flash layout, not constructed: the mock stores no files.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u16)]
pub enum File {
    Firmware,
}

impl flash_db::Object<{ BLOCK_SIZE }> for File {
    fn id(&self) -> flash_db::ObjectId {
        *self as flash_db::ObjectId
    }

    fn fragment_size(&self) -> usize {
        300
    }

    fn flash_blocks(&self) -> core::ops::Range<flash_db::block_layer::BlockId> {
        // Blocks 0 and 1 are reserved for the filesystem itself.
        match self {
            File::Firmware => 2..512,
        }
    }
}

impl flash_db::File<{ BLOCK_SIZE }> for File {}

/// Streams that can be stored. Each stream acts like a persistent circular
/// buffer that stores multiple data segments in FIFO order.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u16)]
pub enum Stream {
    Event,
    ProcessingResult,
    Log,
}

impl Stream {
    pub const fn max_fragment_size(&self) -> usize {
        match self {
            Stream::Event => MAX_SERIALIZED_EVENT_LEN + STREAM_OVERHEAD_BYTES,
            Stream::ProcessingResult => MAX_SENSOR_DATA_LEN + STREAM_OVERHEAD_BYTES,
            Stream::Log => MAX_LOG_LEN + STREAM_OVERHEAD_BYTES,
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
            // 1024 blocks = 4MB. Must stay below block 2048: the flash this
            // layout was written for (S25FL064L) is 8MB total.
            Stream::ProcessingResult => 1024..2048,
            Stream::Log => 512..768, // 256 blocks = ca 1MB
        }
    }
}

impl flash_db::Circular<{ BLOCK_SIZE }> for Stream {
    fn overwrite_on_full(&self) -> bool {
        // Every stream drops its oldest entries to make room for new ones: a
        // device that cannot reach the server keeps reporting the present
        // rather than stalling on the past.
        true
    }
}

impl DispatchStreams<{ BLOCK_SIZE }> for Stream {
    fn event() -> Self {
        Stream::Event
    }
    fn sensor_data() -> Self {
        Stream::ProcessingResult
    }
    fn log() -> Self {
        Stream::Log
    }
}

/// Persistent storage for events, sensor data and log records.
///
/// Thin alias over the generic [`StreamSetStore`], pinned to the mock [`Stream`]
/// layout and [`TopicFromDevice`].
#[allow(dead_code)]
pub type MockStore<'a, DB> =
    StreamSetStore<'a, DB, Stream, BLOCK_SIZE, MAX_SENSOR_DATA_LEN, TopicFromDevice>;

/// `'static` wrapper around [`MockStore`] (see ADR-0003). Used by the dispatch task.
pub type StaticMockStore<DB> =
    StaticStreamSetStore<DB, Stream, BLOCK_SIZE, MAX_SENSOR_DATA_LEN, TopicFromDevice>;

#[cfg(test)]
mod test {
    use super::*;
    use sensor_link_firmware::storage::flash_db::ObjectExt;

    /// A stream must not waste much of each flash block on fragment padding.
    ///
    /// The threshold is arbitrary; the point is to notice when a change (say a
    /// slightly larger `MAX_MESSAGE_LEN` costing one fragment per block) makes
    /// the storage much less efficient.
    fn assert_efficient(stream: Stream, net_bytes_per_frag: usize, max_overhead_pct: f32) {
        assert_eq!(
            net_bytes_per_frag + STREAM_OVERHEAD_BYTES,
            stream.max_fragment_size()
        );

        let max_bytes = net_bytes_per_frag * stream.fragments_per_block();
        let overhead = BLOCK_SIZE - max_bytes;
        let overhead_pct = overhead as f32 * 100.0 / BLOCK_SIZE as f32;

        assert!(
            overhead_pct < max_overhead_pct,
            "{stream:?}: {overhead} bytes ({overhead_pct:.2}%) overhead per block"
        );
    }

    #[test]
    fn test_sensor_stream_efficiency() {
        assert_efficient(Stream::ProcessingResult, MAX_SENSOR_DATA_LEN, 2.0);
    }

    #[test]
    fn test_event_stream_efficiency() {
        assert_efficient(Stream::Event, MAX_SERIALIZED_EVENT_LEN, 6.5);
    }

    #[test]
    fn test_stream_count() {
        use sensor_link_firmware::storage::flash_db::Object;

        assert_eq!(Stream::ProcessingResult.fragment_count(), 4092);
        assert_eq!(Stream::ProcessingResult.max_fragment_size(), 1014);
        assert_eq!(Stream::ProcessingResult.fragments_per_block(), 4);
        assert_eq!(Stream::ProcessingResult.flash_blocks().start, 1024);
        assert_eq!(Stream::ProcessingResult.flash_blocks().end, 2048);
    }

    /// The streams must not overlap: one stream writing into another's blocks
    /// would corrupt it.
    #[test]
    fn test_streams_do_not_overlap() {
        use sensor_link_firmware::storage::flash_db::Object;

        let mut ranges = [
            <File as Object<BLOCK_SIZE>>::flash_blocks(&File::Firmware),
            Stream::Log.flash_blocks(),
            Stream::Event.flash_blocks(),
            Stream::ProcessingResult.flash_blocks(),
        ];
        ranges.sort_by_key(|range| range.start);

        for pair in ranges.windows(2) {
            assert!(
                pair[0].end <= pair[1].start,
                "overlapping flash ranges: {:?} and {:?}",
                pair[0],
                pair[1]
            );
        }
    }
}

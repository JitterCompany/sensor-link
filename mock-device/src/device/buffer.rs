//! Buffering and serialization of the mock's sensor data.
//!
//! Ported from `btb-firmware-core`'s `logic::dispatch::zonneboiler_buffer`. The
//! buffering machinery it builds on ([`BufferManager`], [`DrainManager`],
//! [`LatencyControlledSerializer`]) is generic and comes from
//! `sensor-link-firmware`; what was zonneboiler-specific is replaced here by the
//! mock's own channel count and wire format.
//!
//! All of the mock's measurements share a single timebase, so they are buffered
//! together as one multi-measurement data stream.
//!
//! # Data flow
//!
//! 1. **Data reception**: receives [`MockResults`] from the measuring task
//! 2. **Buffer management**: the buffer manages capacity and timing
//! 3. **Serialization**: the buffer serializes when full, on timeout, or during
//!    a drain
//! 4. **Packet generation**: returns serialized packets ready for upload
//!
//! When a receive times out, the buffer drains what it has accumulated rather
//! than letting it go stale. Since all measurements live in a single buffer, a
//! drain yields at most one packet, after which the drain is complete.

use sensor_link_firmware::{
    logic::{
        dispatch::{
            buffer::{BufferManager, BufferResult, BufferSerializer},
            drain::DrainManager,
            serialization::LatencyControlledSerializer,
        },
        serializer::{SerializeError, UniformSampleSerializer},
        ReceiveChannel,
    },
    monotonic_time::FutureTimeout,
    sensor_link_protocol::{samples::UniformSamples, TopicFromDevice},
    serialize::SerializedSendable,
};

pub use sensor_link_firmware::logic::dispatch::buffer::SampleData;

use crate::device::store::MAX_SENSOR_DATA_LEN;

/// Number of measurement channels the mock reports.
pub const NUM_CH_MOCK: usize = 4;

const MAX_MESSAGE_LEN: usize = MAX_SENSOR_DATA_LEN;

/// Serializer producing the mock's wire format.
///
/// The zonneboiler had its own `ZonneboilerDataSerializer` in `btb-protocol`;
/// the mock uses the manufacturer-generic uniform-sample format instead, so
/// there is nothing product-specific left to define here.
type DataSerializer = UniformSampleSerializer<NUM_CH_MOCK, MAX_MESSAGE_LEN>;

/// Maximum number of samples (per measurement) in a single result from the
/// measuring task.
///
/// Measuring emits one sample per measurement at its (low) output rate, so a
/// small capacity suffices here: samples are accumulated by the buffer until a
/// full packet can be serialized.
pub const MAX_INPUT_SIZE: usize = 10;

/// A single result may never exceed what the buffer (and therefore a single
/// message) can hold.
const _: () = assert!(MAX_INPUT_SIZE <= DataSerializer::MAX_INPUT_LEN);

/// Topic the mock publishes its sensor data on.
///
/// A product device declares a `Data` topic of its own; the mock has no place
/// in any product's topic enum, so it reports on the protocol's test topic.
const DATA_TOPIC: TopicFromDevice = TopicFromDevice::BenchmarkData;

/// Adapts the generic sample serializer to the buffer's [`BufferSerializer`]
/// interface, which is what keeps wire-format knowledge out of the buffer.
pub struct MockDataSerializer;

impl<const MAX_INPUT_LEN: usize, const M: usize> BufferSerializer<NUM_CH_MOCK, MAX_INPUT_LEN, M>
    for MockDataSerializer
{
    type Error = SerializeError;
    type Topic = TopicFromDevice;

    fn serialize(
        &self,
        samples: &UniformSamples<NUM_CH_MOCK, MAX_INPUT_LEN>,
    ) -> Result<SerializedSendable<M, Self::Topic>, Self::Error> {
        UniformSampleSerializer::<NUM_CH_MOCK, M>::serialize(samples, DATA_TOPIC)
    }
}

/// Results the measuring task hands to the dispatch pipeline.
#[derive(Debug)]
pub enum MockResults {
    Data(SampleData<NUM_CH_MOCK, MAX_INPUT_SIZE>),
}

#[derive(Debug)]
pub enum Error<T: core::fmt::Debug> {
    BufferSerializeFailed,
    Receive(T),
}

type Packet = SerializedSendable<MAX_MESSAGE_LEN, TopicFromDevice>;

pub struct MockBuffer<RX: ReceiveChannel<MockResults>> {
    data: BufferManager<
        NUM_CH_MOCK,
        { DataSerializer::MAX_INPUT_LEN },
        MAX_MESSAGE_LEN,
        MockDataSerializer,
    >,
    receive_channel: RX,
    timeout_ms: u32,
    /// Sampling frequency [Hz] the current buffer was created for
    fs: f32,
    /// Maximum latency due to buffering [microseconds], see [`MockBuffer::new`]
    buffer_timeout_us: i64,
    /// Set while the buffer still has to be serialized after a timeout occurred
    timeout_drain_pending: bool,
    /// Packet that was serialized while another one was already being returned
    pending_packet: Option<Packet>,
}

impl<RX: ReceiveChannel<MockResults>> DrainManager for MockBuffer<RX> {
    type Item = Packet;

    fn start_drain(&mut self) {
        self.timeout_drain_pending = true;
    }

    fn continue_drain(&mut self) -> Option<Self::Item> {
        self.continue_timeout_drain()
    }

    fn is_draining(&self) -> bool {
        self.timeout_drain_pending
    }
}

impl<RX: ReceiveChannel<MockResults>> MockBuffer<RX> {
    pub const DEFAULT_TIMEOUT_MS: u32 = 2_000;
    pub const DEFAULT_MAX_LATENCY_MS: u32 = 30_000;

    /// Sampling frequency assumed until the first data (which carries its own
    /// `fs`) arrives.
    ///
    /// Only relevant while the buffer is still empty, so the exact value does
    /// not matter.
    const INITIAL_FS: f32 = 1.0;

    /// Create a buffer for all outgoing data streams, with default timing.
    ///
    /// See [`Self::new`] for more configuration options.
    pub fn with_default_timing(rx: RX) -> Self {
        Self::new(rx, Self::DEFAULT_TIMEOUT_MS, Self::DEFAULT_MAX_LATENCY_MS)
    }

    /// Create a buffer which buffers all outgoing data streams.
    ///
    /// The sampling frequency is not configured here: it is taken from the
    /// incoming data, see [`SampleData::fs`].
    ///
    /// * `timeout_ms`: Maximum expected time between results from `RX`. Buffers
    ///   are flushed/serialized when exceeded.
    /// * `max_latency_ms`: Maximum latency due to buffering: buffers are
    ///   flushed/serialized when the timerange (last - first sample) exceeds
    ///   this value. This enforces an upper limit to the data latency, in case
    ///   the buffer capacity is relatively large relative to the sampling
    ///   frequency.
    pub fn new(rx: RX, timeout_ms: u32, max_latency_ms: u32) -> Self {
        let buffer_timeout_us = (max_latency_ms * 1000) as i64;
        Self {
            data: BufferManager::new(Self::INITIAL_FS, MockDataSerializer, buffer_timeout_us),
            receive_channel: rx,
            timeout_ms,
            fs: Self::INITIAL_FS,
            buffer_timeout_us,
            timeout_drain_pending: false,
            pending_packet: None,
        }
    }

    pub async fn wait_for_data(&mut self) -> Result<Option<Packet>, Error<RX::Error>> {
        // A previously serialized packet still has to be returned before receiving new data
        if let Some(packet) = self.pending_packet.take() {
            return Ok(Some(packet));
        }

        // If we're in timeout drain mode, drain the buffer before receiving new data
        if self.is_draining() {
            if let Some(sendable) = self.continue_drain() {
                return Ok(Some(sendable));
            }
        }

        // Apply timeout to the receive operation
        let res = match self
            .receive_channel
            .recv()
            .with_timeout_ms(self.timeout_ms)
            .await
        {
            Some(res) => res,
            None => {
                // Timeout occurred, start draining the buffer
                self.start_drain();
                return Ok(self.continue_drain());
            }
        };

        match res {
            Ok(MockResults::Data(data)) => {
                // Data at a new rate: the buffered samples belong to the old rate, so they are
                // serialized (with that rate) before the buffer is re-created for the new one.
                let flushed = self.set_fs(data.fs);

                let packet = match self.data.push_data(&data) {
                    BufferResult::Serialized(packet) => Some(packet),
                    BufferResult::DataAdded => None,
                    BufferResult::SerializationFailed => return Err(Error::BufferSerializeFailed),
                };

                // At most one packet can be returned now, the other one is returned next call
                match flushed {
                    Some(flushed) => {
                        self.pending_packet = packet;
                        Ok(Some(flushed))
                    }
                    None => Ok(packet),
                }
            }
            // Something wrong with the receive queue(-adapter)
            Err(ch_err) => Err(Error::Receive(ch_err)),
        }
    }

    /// Update the sampling frequency the buffered data is serialized with.
    ///
    /// Any data buffered at the previous frequency is serialized (and returned)
    /// first, because the frequency applies to a buffer as a whole.
    fn set_fs(&mut self, fs: f32) -> Option<Packet> {
        if fs == self.fs {
            return None;
        }
        log::info!(target: "Dispatch", "Sample frequency changed: {} -> {fs} Hz", self.fs);

        let flushed = self.data.force_serialize();
        self.fs = fs;
        self.data = BufferManager::new(fs, MockDataSerializer, self.buffer_timeout_us);

        flushed
    }

    /// Drain the buffer when in timeout state.
    ///
    /// There is only a single buffer, so one call completes the drain.
    fn continue_timeout_drain(&mut self) -> Option<Packet> {
        if !self.timeout_drain_pending {
            return None;
        }
        self.timeout_drain_pending = false;

        self.data.force_serialize()
    }

    fn set_buffer_timeout(&mut self, timeout_us: i64) {
        self.buffer_timeout_us = timeout_us;
        self.data.set_timeout(timeout_us);
    }

    fn set_timeout(&mut self, timeout_ms: u32) {
        self.timeout_ms = timeout_ms;
    }
}

impl<RX: ReceiveChannel<MockResults>> LatencyControlledSerializer<MAX_MESSAGE_LEN>
    for MockBuffer<RX>
{
    type Error = Error<RX::Error>;
    type Topic = TopicFromDevice;

    async fn next_packet(&mut self) -> Result<Option<Packet>, Self::Error> {
        self.wait_for_data().await
    }

    fn set_timeout(&mut self, timeout_ms: u32) {
        self.set_timeout(timeout_ms);
    }

    fn set_buffer_timeout(&mut self, timeout_ms: u32) {
        self.set_buffer_timeout(i64::from(timeout_ms) * 1000);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_LEN: usize = 3;

    /// The buffer's receive timeout runs on the monotonic timer, which the
    /// application normally starts at boot.
    fn init_monotonic() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(sensor_link_firmware::std_monotonic_driver::start);
    }

    fn test_samples() -> SampleData<NUM_CH_MOCK, TEST_LEN> {
        SampleData {
            t: 1000,
            len: TEST_LEN,
            samples: [[1.0; TEST_LEN]; NUM_CH_MOCK],
            t_last: 4000,
            fs: 1000.0,
        }
    }

    /// A receiver that never yields data, so every receive times out.
    struct MockReceiver;

    impl ReceiveChannel<MockResults> for MockReceiver {
        type Error = ();
        async fn recv(&mut self) -> Result<MockResults, Self::Error> {
            core::future::pending().await
        }
        fn try_recv(&mut self) -> Result<MockResults, Self::Error> {
            Err(())
        }
    }

    /// Buffered data must be serialized (not dropped) when a receive times out.
    #[tokio::test]
    async fn test_timeout_drains_buffer() {
        init_monotonic();
        let mut buffer = MockBuffer::new(
            MockReceiver,
            1,
            MockBuffer::<MockReceiver>::DEFAULT_MAX_LATENCY_MS,
        );

        assert!(matches!(
            buffer.data.push_data(&test_samples()),
            BufferResult::DataAdded
        ));

        let packet = buffer.wait_for_data().await.unwrap();
        assert!(packet.is_some(), "timeout must drain the buffered samples");

        // The drain is complete: there is only one buffer.
        assert!(!buffer.is_draining());
    }

    /// An empty buffer has nothing to drain, so a timeout yields no packet.
    #[tokio::test]
    async fn test_timeout_on_empty_buffer() {
        init_monotonic();
        let mut buffer = MockBuffer::new(
            MockReceiver,
            1,
            MockBuffer::<MockReceiver>::DEFAULT_MAX_LATENCY_MS,
        );

        assert!(buffer.wait_for_data().await.unwrap().is_none());
    }
}

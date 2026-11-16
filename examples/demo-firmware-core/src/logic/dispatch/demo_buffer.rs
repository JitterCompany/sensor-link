//! Buffer implementation for the demo data stream.
//!
//! This module provides a concrete implementation of the buffering system for
//! the demo device. All its channels share a single timebase, so they are
//! buffered together as one multi-channel data stream.
//!
//! # Architecture
//!
//! The [`DemoBuffer`] manages:
//! - **1 Data Buffer**: holding [`NUM_CH_DEMO`] measurements which share
//!   the same start timestamp and sampling frequency
//! - **Timeout Management**: draining the buffer when receive operations timeout
//!
//! # Data Flow
//!
//! 1. **Data Reception**: Receives [`DemoResults`] from processing pipeline
//! 2. **Buffer Management**: The buffer manages capacity and timing
//! 3. **Serialization**: The buffer serializes when full, on timeout, or during drain operations
//! 4. **Packet Generation**: Returns serialized packets ready for network transmission
//!
//! # Timeout Handling
//!
//! When receive operations timeout, the buffer drains its accumulated data to prevent
//! data loss. Since all measurements live in a single buffer, a drain yields at most one
//! packet, after which the drain is complete.
//!
//! # Integration
//!
//! This module implements both [`DrainManager`] and [`LatencyControlledSerializer`]
//! traits, making it compatible with the broader dispatch architecture while providing
//! demo-specific optimizations.

use demo_protocol::{TopicFromDevice, NUM_CH_DEMO};

use crate::{
    logic::ReceiveChannel, monotonic_time::FutureTimeout, storage::streams::MAX_SENSOR_DATA_LEN,
    SerializedSendable,
};

use super::{
    buffer::{BufferManager, BufferResult},
    demo_data::DemoDataSerializer,
    drain::DrainManager,
    serialization::LatencyControlledSerializer,
    serializer_wrappers::DemoDataSerializerWrapper,
};

pub use super::buffer::SampleData;

const MAX_MESSAGE_LEN: usize = MAX_SENSOR_DATA_LEN;

/// Maximum number of samples (per measurement) in a single result from the processing task.
///
/// Processing emits one sample per measurement at its (low) output rate, so a small capacity
/// suffices here: samples are accumulated by the buffer until a full packet can be serialized.
const MAX_INPUT_SIZE: usize = 10;

/// A single result may never exceed what the buffer (and therefore a single message) can hold.
const _: () = assert!(MAX_INPUT_SIZE <= DemoDataSerializer::<MAX_MESSAGE_LEN>::MAX_INPUT_LEN);

#[derive(Debug)]
pub enum DemoResults {
    Data(SampleData<NUM_CH_DEMO, MAX_INPUT_SIZE>),
}

#[derive(Debug)]
pub enum Error<T: core::fmt::Debug> {
    BufferSerializeFailed,
    Receive(T),
}

pub struct DemoBuffer<RX: ReceiveChannel<DemoResults>> {
    data: BufferManager<
        NUM_CH_DEMO,
        { DemoDataSerializer::<MAX_MESSAGE_LEN>::MAX_INPUT_LEN },
        MAX_MESSAGE_LEN,
        DemoDataSerializerWrapper,
    >,
    receive_channel: RX,
    timeout_ms: u32,
    /// Sampling frequency [Hz] the current buffer was created for
    fs: f32,
    /// Maximum latency due to buffering [microseconds], see [DemoBuffer::new]
    buffer_timeout_us: i64,
    /// Set while the buffer still has to be serialized after a timeout occurred
    timeout_drain_pending: bool,
    /// Packet that was serialized while another one was already being returned
    pending_packet: Option<SerializedSendable<MAX_MESSAGE_LEN>>,
}

impl<RX: ReceiveChannel<DemoResults>> DrainManager for DemoBuffer<RX> {
    type Item = SerializedSendable<MAX_MESSAGE_LEN>;

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

impl<RX: ReceiveChannel<DemoResults>> DemoBuffer<RX> {
    pub const DEFAULT_TIMEOUT_MS: u32 = 2_000;
    pub const DEFAULT_MAX_LATENCY_MS: u32 = 30_000;

    /// Sampling frequency assumed until the first data (which carries its own `fs`) arrives.
    ///
    /// Only relevant while the buffer is still empty, so the exact value does not matter.
    const INITIAL_FS: f32 = 1.0;

    /// Create a buffer which buffers all outgoing data-streams
    ///
    /// * `rx`: receive channel for incoming data
    ///
    /// See [Self::new] for more configuration options.
    pub fn with_default_timing(rx: RX) -> Self {
        Self::new(rx, Self::DEFAULT_TIMEOUT_MS, Self::DEFAULT_MAX_LATENCY_MS)
    }

    /// Create a buffer which buffers all outgoing data-streams
    ///
    /// The sampling frequency is not configured here: it is taken from the incoming data,
    /// see [SampleData::fs].
    ///
    /// * `timeout_ms`: Maximum expected time between results from `RX`. Buffers are flushed/serialized when exceeded.
    /// * `max_latency_ms`: Maximum latency due to buffering: buffers are flushed/serialized when the timerange
    ///   (last - first sample) exceeds this value. This enforces an upper limit to the data latency,
    ///   in case the buffer capacity is relatively large relative to the sampling frequency.
    pub fn new(rx: RX, timeout_ms: u32, max_latency_ms: u32) -> Self {
        let buffer_timeout_us = (max_latency_ms * 1000) as i64;
        Self {
            data: BufferManager::new(
                Self::INITIAL_FS,
                DemoDataSerializerWrapper {},
                buffer_timeout_us,
            ),
            receive_channel: rx,
            timeout_ms,
            fs: Self::INITIAL_FS,
            buffer_timeout_us,
            timeout_drain_pending: false,
            pending_packet: None,
        }
    }

    pub async fn wait_for_data(
        &mut self,
    ) -> Result<Option<SerializedSendable<MAX_MESSAGE_LEN>>, Error<RX::Error>> {
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

        // TODO what if the caller cancels the future and retries later? I think the timeout should continue
        // instead of being restarted every time (e.g. if the caller applies its own timeout or we are polled
        // in a select() statement, the timeout condition might never be met)
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
            Ok(DemoResults::Data(data)) => {
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
    /// Any data buffered at the previous frequency is serialized (and returned) first,
    /// because the frequency applies to a buffer as a whole.
    fn set_fs(&mut self, fs: f32) -> Option<SerializedSendable<MAX_MESSAGE_LEN>> {
        if fs == self.fs {
            return None;
        }
        log::info!(target: "dispatch", "Sample frequency changed: {} -> {fs} Hz", self.fs);

        let flushed = self.data.force_serialize();
        self.fs = fs;
        self.data = BufferManager::new(fs, DemoDataSerializerWrapper {}, self.buffer_timeout_us);

        flushed
    }

    /// Drain the buffer when in timeout state
    ///
    /// There is only a single buffer, so one call completes the drain.
    fn continue_timeout_drain(&mut self) -> Option<SerializedSendable<MAX_MESSAGE_LEN>> {
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

impl<RX: ReceiveChannel<DemoResults>> LatencyControlledSerializer<MAX_MESSAGE_LEN>
    for DemoBuffer<RX>
{
    type Error = Error<RX::Error>;
    type Topic = TopicFromDevice;

    async fn next_packet(
        &mut self,
    ) -> Result<Option<SerializedSendable<MAX_MESSAGE_LEN>>, Self::Error> {
        self.wait_for_data().await
    }

    fn set_timeout(&mut self, timeout_ms: u32) {
        self.set_timeout(timeout_ms);
    }

    fn set_buffer_timeout(&mut self, timeout_ms: u32) {
        self.set_buffer_timeout(i64::from(timeout_ms) * 1000);
    }

    fn flush(&mut self) -> Option<SerializedSendable<MAX_MESSAGE_LEN>> {
        self.pending_packet
            .take()
            .or_else(|| self.data.force_serialize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logic::ReceiveChannel;

    const TEST_LEN: usize = 3;

    fn test_samples() -> SampleData<NUM_CH_DEMO, TEST_LEN> {
        SampleData {
            t: 1000,
            len: TEST_LEN,
            samples: [[1.0; TEST_LEN]; NUM_CH_DEMO],
            t_last: 4000,
            fs: 1000.0,
        }
    }

    // Create a simple mock receiver for testing
    struct MockReceiver;
    impl ReceiveChannel<DemoResults> for MockReceiver {
        type Error = ();
        async fn recv(&mut self) -> Result<DemoResults, Self::Error> {
            // Never return data - will always timeout
            Err(())
        }
        fn try_recv(&mut self) -> Result<DemoResults, Self::Error> {
            Err(())
        }
    }

    #[test]
    fn test_timeout_drain_state_initialization() {
        // Test that DemoBuffer initializes with no timeout drain state
        let mock_rx = MockReceiver;
        let buffer = DemoBuffer::new(mock_rx, 100, 10_000);

        // Should have no timeout drain state initially
        assert!(!buffer.timeout_drain_pending);
    }

    #[test]
    fn test_continue_timeout_drain_no_data() {
        // Test draining when the buffer has no data
        let mock_rx = MockReceiver;
        let mut buffer = DemoBuffer::new(mock_rx, 100, 10_000);

        // Manually set timeout drain state
        buffer.timeout_drain_pending = true;

        // Should return None and clear drain state when no data
        let result = buffer.continue_timeout_drain();
        assert!(result.is_none());
        assert!(!buffer.timeout_drain_pending);
    }

    #[test]
    fn test_continue_timeout_drain_with_data() {
        // Test draining when the buffer has data
        let mock_rx = MockReceiver;
        let mut buffer = DemoBuffer::new(mock_rx, 100, 10_000);

        // Manually add data to buffer (simulate data that was added before timeout)
        buffer.data.push_data(&test_samples());

        buffer.timeout_drain_pending = true;

        // Should return serialized data and complete the drain
        let result = buffer.continue_timeout_drain();
        assert!(result.is_some());
        assert!(!buffer.timeout_drain_pending);

        // Buffer should now be empty
        assert_eq!(buffer.data.len(), 0);
    }

    #[test]
    fn test_continue_timeout_drain_when_not_draining() {
        // Draining is only allowed after start_drain(): buffered data is kept otherwise
        let mock_rx = MockReceiver;
        let mut buffer = DemoBuffer::new(mock_rx, 100, 10_000);

        buffer.data.push_data(&test_samples());

        assert!(buffer.continue_timeout_drain().is_none());
        assert_eq!(buffer.data.len(), TEST_LEN);
    }

    #[test]
    fn test_drain_manager_trait() {
        // Test the DrainManager trait implementation
        let mock_rx = MockReceiver;
        let mut buffer = DemoBuffer::new(mock_rx, 100, 10_000);

        // Initially not draining
        assert!(!buffer.is_draining());

        buffer.data.push_data(&test_samples());

        // Start drain
        buffer.start_drain();
        assert!(buffer.is_draining());

        // Continue drain should return the single buffer and complete
        let mut drained_count = 0;
        while buffer.is_draining() {
            let result = buffer.continue_drain();
            if result.is_some() {
                drained_count += 1;
            }
        }

        assert_eq!(drained_count, 1); // Single buffer holding all measurements
        assert!(!buffer.is_draining()); // Done draining

        // No more items when not draining
        let result = buffer.continue_drain();
        assert!(result.is_none());
    }

    #[test]
    fn test_set_fs_unchanged() {
        // The buffer is kept as-is (nothing to serialize) while the frequency stays the same
        let mock_rx = MockReceiver;
        let mut buffer = DemoBuffer::new(mock_rx, 100, 10_000);

        buffer.set_fs(2.0);
        buffer.data.push_data(&test_samples());

        assert!(buffer.set_fs(2.0).is_none());
        assert_eq!(buffer.data.len(), TEST_LEN);
    }

    #[test]
    fn test_set_fs_flushes_previous_data() {
        // Buffered samples belong to the previous frequency: they are serialized before the
        // buffer switches over to the new one.
        let mock_rx = MockReceiver;
        let mut buffer = DemoBuffer::new(mock_rx, 100, 10_000);

        buffer.data.push_data(&test_samples());

        assert!(buffer.set_fs(2.0).is_some());
        assert_eq!(buffer.fs, 2.0);
        assert_eq!(buffer.data.len(), 0);

        // An empty buffer has nothing to serialize
        assert!(buffer.set_fs(4.0).is_none());
        assert_eq!(buffer.fs, 4.0);
    }
}

//! Core buffer types and logic for collecting and serializing samples.
//!
//! This module provides the foundation for sensor-agnostic data buffering in the dispatch
//! architecture. It implements a generic buffering system that can collect samples from
//! different sensor types, manage timing constraints, and trigger serialization based on
//! various conditions.
//!
//! # Key Components
//!
//! - [`SerializingBuffer`]: A generic buffer that accumulates samples with timestamp validation
//! - [`BufferManager`]: High-level buffer management with automatic serialization
//! - [`SampleData`]: Container for sample data with timing information
//! - [`BufferSerializer`]: Trait for implementing different serialization strategies
//!
//! # Buffer Lifecycle
//!
//! 1. **Data Accumulation**: Samples are added to the buffer with timestamp validation
//! 2. **Contiguity Checking**: Ensures temporal continuity between sample batches
//! 3. **Trigger Conditions**: Serialization is triggered by:
//!    - Buffer capacity limits
//!    - Timeout expiration
//!    - Non-contiguous timestamps
//! 4. **Serialization**: Buffer contents are serialized using the configured strategy
//! 5. **Reset**: Buffer is cleared and ready for new data
//!

use crate::serialize::SerializedSendable;
use sensor_link_protocol::{samples::UniformSamples, Topic};

/// A set of samples to be buffered.
///
/// Sensor-agnostic: it carries only samples and timing. Any channel/stream identification is the
/// responsibility of the serializer and of the caller's own result types, so this generic
/// buffering layer never depends on a concrete channel type.
#[derive(Debug)]
pub struct SampleData<const N_AXES: usize, const CAPACITY: usize> {
    /// Timestamp of the first sample in the set.
    pub t: i64,
    pub len: usize,
    pub samples: [[f32; CAPACITY]; N_AXES],
    /// Timestamp of the last sample in the set.
    pub t_last: i64,
    /// Sampling frequency [Hz] the samples were taken at.
    pub fs: f32,
}

impl<const N_AXES: usize, const CAPACITY: usize> SampleData<N_AXES, CAPACITY> {
    pub const N_AXES: usize = Self::validate_n_axes(N_AXES);
    pub const CAPACITY: usize = CAPACITY;

    const fn validate_n_axes(n_axes: usize) -> usize {
        assert!(n_axes > 0);
        n_axes
    }

    /// Create an empty set of samples with a define start timestamp.
    ///
    /// The samples stay marked as empty, the `len`, `samples` and `t_last` must be updated later
    /// See [from_slices](method@Self::from_slices) to create a fully initialized struct
    #[inline]
    pub fn empty_at(t_start: i64, sampling_frequency: f32) -> Self {
        Self::from_slices(t_start, sampling_frequency, [&[]; N_AXES])
    }

    /// Create SampleData from an array of slices
    ///
    /// Each slice is assumed to be at most [Self::CAPACITY] long
    /// and must have the same length (any samples beyond the shortest slice are truncated)
    ///
    /// t_start is in microseconds
    /// sampling_frequency is in Hz
    ///
    #[inline]
    pub fn from_slices(t_start: i64, sampling_frequency: f32, samples: [&[f32]; N_AXES]) -> Self {
        // Find the minimum length of all axes in case they are different
        // Note: unwrap cannot panick because validate_n_axes() asserts N_AXES > 0.
        let len: usize = samples.iter().map(|slice| slice.len()).min().unwrap();
        let dt_per_sample = 1e6 / sampling_frequency;
        let dt = (len.saturating_sub(1) as f32 * dt_per_sample) as i64;
        let t_last = t_start + dt;

        Self {
            t: t_start,
            t_last,
            len,

            samples: core::array::from_fn(|i| {
                let mut buf = [core::f32::NAN; CAPACITY];
                buf[..len].copy_from_slice(&samples[i][..len]);
                buf
            }),
            fs: sampling_frequency,
        }
    }
}

#[derive(Debug)]
pub enum BufferStatus {
    Ok,
    NotContiguous,
    NotEnoughSpace,
    NotEnoughSpaceForNext,
    Timeout,
}

/// A buffer for accumulating samples
///
/// # Type Parameters
///
/// * `N_AXES`: Number of axes in the data stream
/// * `MAX_INPUT_SIZE`: Maximum number of samples that can be stored in the buffer
#[derive(Debug)]
pub struct SerializingBuffer<const N_AXES: usize, const MAX_INPUT_SIZE: usize> {
    sample_buffer: UniformSamples<N_AXES, MAX_INPUT_SIZE>,
    last_timestamp: i64,
    t_delta_us: i64,
    timeout_us: i64,
}

impl<const N_AXES: usize, const MAX_INPUT_SIZE: usize> SerializingBuffer<N_AXES, MAX_INPUT_SIZE> {
    /// Create an empty SerializingBuffer, assuming future samples will be at approximately `fs` Hz.
    ///
    /// * `fs`: Expected sampling frequency of the data stream. Used to detect discontinuities.
    ///     If successive sample timestamps are more than two sample intervals apart, the buffer will be serialized
    ///     to prevent inaccurate timestamps after deserialization.
    /// * `timeout_us`: triggers `BufferStatus::Timeout` if the timerange of buffered data exceeds the timeout (in microseconds)
    ///     Intended as a heuristic to trigger serialization with a well-defined maximum data latency.
    pub fn empty(fs: f32, timeout_us: i64) -> Self {
        Self {
            sample_buffer: UniformSamples::empty(fs),
            last_timestamp: 0,
            t_delta_us: (1e6 / fs) as i64 * 2, // allow some slack for rounding errors
            timeout_us,
        }
    }

    /// Change the timeout for the buffer.
    ///
    /// This will trigger `BufferStatus::Timeout` if the timerange of buffered data exceeds the timeout (in microseconds)
    /// Intended as a heuristic to trigger serialization with a well-defined maximum data latency.
    pub fn set_timeout(&mut self, timeout_us: i64) {
        self.timeout_us = timeout_us;
    }

    pub fn len(&self) -> usize {
        self.sample_buffer.len()
    }

    pub fn is_full(&self) -> bool {
        self.len() == MAX_INPUT_SIZE
    }

    pub fn clear(&mut self) {
        self.sample_buffer = UniformSamples::empty(self.sample_buffer.fs);
    }

    pub fn push<const L: usize>(&mut self, data: &SampleData<N_AXES, L>) -> BufferStatus {
        // compile time assert that L < N
        const { assert!(L <= MAX_INPUT_SIZE) }

        if self.len() + data.len > MAX_INPUT_SIZE {
            return BufferStatus::NotEnoughSpace;
        }

        if self.len() == 0 {
            self.sample_buffer.t = data.t;
            self.last_timestamp = data.t_last;
        }

        // Check if the new data is contiguous with the last data.
        // use fs to calculate the expected timestamp of the first sample
        if data.t > self.last_timestamp + self.t_delta_us {
            return BufferStatus::NotContiguous;
        }

        for i in 0..N_AXES {
            let slice = &data.samples[i][..data.len];
            let result = self.sample_buffer.ch[i].extend_from_slice(slice);
            if result.is_err() {
                return BufferStatus::NotEnoughSpace;
            }
        }

        // Update the last timestamp
        self.last_timestamp = data.t_last;

        // Check time since first timestamp. If it exceeds the timeout, return timeout
        let first_timestamp = self.sample_buffer.t;
        let dt_us = self.last_timestamp - first_timestamp;
        if dt_us > self.timeout_us {
            return BufferStatus::Timeout;
        }

        // Check if there would be space for another similar-sized addition
        // This is a heuristic to trigger serialization before completely filling the buffer
        if self.len() + data.len > MAX_INPUT_SIZE {
            return BufferStatus::NotEnoughSpaceForNext;
        }

        BufferStatus::Ok
    }
}

/// Trait for serialization strategies
pub trait BufferSerializer<
    const N_AXES: usize,
    const MAX_INPUT_LEN: usize,
    const MAX_OUTPUT_LEN: usize,
>
{
    type Error: core::fmt::Debug;
    type Topic: Topic;

    fn serialize(
        &self,
        samples: &UniformSamples<N_AXES, MAX_INPUT_LEN>,
    ) -> Result<SerializedSendable<MAX_OUTPUT_LEN, Self::Topic>, Self::Error>;
}

/// Result of attempting to add data to the buffer
#[derive(Debug)]
pub enum BufferResult<const M: usize, T: Topic> {
    /// Data added successfully, no serialization needed yet
    DataAdded,
    /// Buffer was serialized and cleared, returning the packet
    Serialized(SerializedSendable<M, T>),
    /// Serialization failed, buffer was cleared
    SerializationFailed,
}

/// Manages buffering and serialization for a single data stream
///
/// # Type Parameters
///
/// * `N_AXES`: Number of axes in the data stream
/// * `MAX_INPUT_SIZE`: Maximum number of samples that can be stored in the buffer
/// * `MAX_OUTPUT_SIZE`: Maximum size of the serialized output
/// * `S`: Type of the serializer (must implement BufferSerializer)
pub struct BufferManager<
    const N_AXES: usize,
    const MAX_INPUT_SIZE: usize,
    const MAX_OUTPUT_SIZE: usize,
    S,
> where
    S: BufferSerializer<N_AXES, MAX_INPUT_SIZE, MAX_OUTPUT_SIZE>,
{
    /// The internal buffer for accumulating samples
    buffer: SerializingBuffer<N_AXES, MAX_INPUT_SIZE>,
    /// Strategy for serializing the buffer contents
    serializer: S,
}

impl<const N_AXES: usize, const MAX_INPUT_SIZE: usize, const MAX_OUTPUT_SIZE: usize, S>
    BufferManager<N_AXES, MAX_INPUT_SIZE, MAX_OUTPUT_SIZE, S>
where
    S: BufferSerializer<N_AXES, MAX_INPUT_SIZE, MAX_OUTPUT_SIZE>,
{
    /// Create an BufferManager managing an empty SerializingBuffer.
    ///
    /// * `fs` - Expected sampling frequency of the data stream. Used to detect discontinuities.
    ///     If successive sample timestamps are more than two sample intervals apart, the buffer will be serialized
    ///     to prevent inaccurate timestamps after deserialization.
    /// * `timeout_us` - maximum latency of buffered data [microseconds].
    ///     This guarantees a maximum data latency by forcing serialization of the buffer
    ///     if the timerange of buffered data (newest-oldest sample) exceeds the timeout.
    pub fn new(fs: f32, serializer: S, timeout_us: i64) -> Self {
        Self {
            buffer: SerializingBuffer::empty(fs, timeout_us),
            serializer,
        }
    }

    /// Add data to buffer, handling overflow and serialization automatically
    pub fn push_data<const L: usize>(
        &mut self,
        data: &SampleData<N_AXES, L>,
    ) -> BufferResult<MAX_OUTPUT_SIZE, S::Topic> {
        const { assert!(L <= MAX_INPUT_SIZE) }
        let mut data_inserted = false;
        // Since L <= MAX_INPUT_SIZE, we can only serialize max once per push_data call
        let mut serialized_packet: Option<SerializedSendable<MAX_OUTPUT_SIZE, S::Topic>> = None;
        let mut num_tries = 0;
        while !data_inserted {
            let should_serialize = match self.buffer.push(data) {
                BufferStatus::Ok => {
                    data_inserted = true;
                    self.buffer.is_full()
                }
                BufferStatus::NotContiguous | BufferStatus::NotEnoughSpace => {
                    // Data is not stored. First end the current buffer. Then insert again.
                    data_inserted = false;
                    true
                }
                BufferStatus::NotEnoughSpaceForNext | BufferStatus::Timeout => {
                    data_inserted = true;
                    // We might as well end the buffer here.
                    // But only if we haven't serialized yet, or we will lose a packet.
                    serialized_packet.is_none()
                }
            };

            if should_serialize {
                match self.serializer.serialize(&self.buffer.sample_buffer) {
                    Ok(serialized) => {
                        self.buffer.clear();
                        // Store the serialized packet
                        serialized_packet = Some(serialized);
                        // If data was already inserted, return immediately
                        if data_inserted {
                            return BufferResult::Serialized(serialized_packet.unwrap());
                        }
                        // Continue the loop to retry data insertion after clearing buffer
                    }
                    Err(err) => {
                        log::error!("Failed to serialize buffer: {err:?}");
                        self.buffer.clear();
                        if num_tries >= 1 {
                            return BufferResult::SerializationFailed;
                        }
                        num_tries += 1;
                    }
                }
            }
        }

        // If we serialized during retry, return the serialized result
        if let Some(packet) = serialized_packet {
            BufferResult::Serialized(packet)
        } else {
            BufferResult::DataAdded
        }
    }

    /// Force serialization of current buffer contents (for timeouts)
    pub fn force_serialize(&mut self) -> Option<SerializedSendable<MAX_OUTPUT_SIZE, S::Topic>> {
        if self.buffer.len() > 0 {
            match self.serializer.serialize(&self.buffer.sample_buffer) {
                Ok(serialized) => {
                    self.buffer.clear();
                    Some(serialized)
                }
                Err(_err) => {
                    log::error!("Failed to force serialize buffer");
                    self.buffer.clear();
                    None
                }
            }
        } else {
            None
        }
    }

    /// Check if buffer is full
    #[allow(dead_code)]
    pub fn is_full(&self) -> bool {
        self.buffer.is_full()
    }

    /// Get current buffer length
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Set the timeout for the buffer [microseconds].
    ///
    /// This guarantees a maximum data latency by forcing serialization of the buffer
    /// if the timerange of buffered data (newest-oldest sample) exceeds the timeout.
    pub fn set_timeout(&mut self, timeout_us: i64) {
        self.buffer.set_timeout(timeout_us);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serialize::{Builder, TOPIC_HEADER_SIZE};
    use sensor_link_protocol::{Topic, TopicSerializeError, TopicString};
    use serde::{Deserialize, Serialize};

    /// Minimal topic so the tests are topic-agnostic: the buffering logic never inspects the topic,
    /// it only needs *some* `Topic` to produce a `SerializedSendable`.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct TestTopic;
    impl Topic for TestTopic {
        fn to_topic_string(&self, _uid: &str) -> Result<TopicString, TopicSerializeError> {
            // Not exercised by the buffering tests.
            Ok(TopicString::new())
        }
    }

    // Mock serializer for testing
    struct MockSerializer {
        should_fail: bool,
    }

    impl<const N_AXES: usize, const N: usize, const MAX_OUTPUT_SIZE: usize>
        BufferSerializer<N_AXES, N, MAX_OUTPUT_SIZE> for MockSerializer
    {
        type Error = &'static str;
        type Topic = TestTopic;

        fn serialize(
            &self,
            _samples: &UniformSamples<N_AXES, N>,
        ) -> Result<SerializedSendable<MAX_OUTPUT_SIZE, Self::Topic>, Self::Error> {
            if self.should_fail {
                return Err("Mock serialization failed");
            }

            // The buffering logic never inspects the serialized content, so build a minimal dummy
            // packet for an arbitrary topic. (`create_with_total_length` is generic over the topic
            // and only validates length, so no concrete domain topic is needed.)
            let dummy = Builder::<MAX_OUTPUT_SIZE>::new()
                .create_with_total_length(TOPIC_HEADER_SIZE)
                .unwrap();

            Ok(dummy)
        }
    }

    #[test]
    fn from_slices_1axis() {
        const FS: f32 = 1000.0;
        const N_AXES: usize = 1;
        const L: usize = 3;
        let sample_data = SampleData::<N_AXES, L> {
            t: 1000,
            len: L,
            samples: [[1.0; L]],
            t_last: 3000,
            fs: FS,
        };

        let from_slices = SampleData::from_slices(1000, FS, [&[1.0; L]]);
        assert_eq!(sample_data.t, from_slices.t);
        assert_eq!(sample_data.len, from_slices.len);
        assert_eq!(sample_data.samples, from_slices.samples);
        assert_eq!(sample_data.t_last, from_slices.t_last);
        assert_eq!(sample_data.fs, from_slices.fs);
    }

    #[test]
    fn from_slices_2axes() {
        const FS: f32 = 1000.0;
        const N_AXES: usize = 2;
        const L: usize = 7;

        // two slices of unequal length: expect result to be truncated to the shorter one (l=5)
        let from_slices: SampleData<N_AXES, L> =
            SampleData::from_slices(12_345, FS, [&[1.0; 9], &[3.14; 5]]);
        assert_eq!(from_slices.t, 12_345);
        assert_eq!(from_slices.len, 5);
        assert_eq!(from_slices.samples[0][0..5], [1.0, 1.0, 1.0, 1.0, 1.0]);
        assert_eq!(from_slices.samples[1][0..5], [3.14, 3.14, 3.14, 3.14, 3.14]);
        // samples out of bound are NaN
        assert!(from_slices.samples[0][5..].iter().all(|&x| x.is_nan()));
        assert!(from_slices.samples[1][5..].iter().all(|&x| x.is_nan()));
        assert_eq!(from_slices.t_last, 16_345);
    }

    #[test]
    fn test_buffer_manager_data_added() {
        const MAX_INPUT_SIZE: usize = 10; // How many samples can fit in the buffer
        const L: usize = 3; // How many samples we are adding each time

        let serializer = MockSerializer { should_fail: false };
        let mut manager: BufferManager<1, MAX_INPUT_SIZE, 100, _> =
            BufferManager::new(1000.0, serializer, 1_000_000);

        let sample_data = SampleData::<1, L> {
            t: 1000, // Non-zero timestamp
            len: L,
            samples: [[1.0; L]],
            t_last: 3000,
            fs: 1000.0,
        };

        // Should add data without serializing (buffer not full)
        match manager.push_data(&sample_data) {
            BufferResult::DataAdded => {}
            other => panic!("Expected DataAdded, got {:?}", other),
        }

        assert_eq!(manager.len(), L);
        assert!(!manager.is_full());
    }

    #[test]
    fn test_buffer_manager_serialized_when_full() {
        const MAX_INPUT_SIZE: usize = 10;
        const L: usize = 3;

        let serializer = MockSerializer { should_fail: false };
        let mut manager: BufferManager<1, MAX_INPUT_SIZE, 100, _> =
            BufferManager::new(1000.0, serializer, 1_000_000);

        let sample_data = SampleData::<1, L> {
            t: 1000, // Non-zero timestamp
            len: L,
            samples: [[1.0; L]],
            t_last: 5000,
            fs: 1000.0,
        };

        // Should add data without serializing (buffer not full yet)
        match manager.push_data(&sample_data) {
            BufferResult::DataAdded => {}
            other => panic!("Expected DataAdded, got {:?}", other),
        }

        // Buffer should have the data but not be full
        assert_eq!(manager.len(), L);

        // Add more data to fill the buffer (3 + 7 = 10, which fills the buffer)
        let sample_data2 = SampleData::<1, 7> {
            t: 5000,
            len: 7,
            samples: [[2.0; 7]],
            t_last: 12000,
            fs: 1000.0,
        };

        // Should serialize when buffer becomes full
        match manager.push_data(&sample_data2) {
            BufferResult::Serialized(_packet) => {}
            other => panic!("Expected Serialized, got {:?}", other),
        }

        // Buffer should be cleared after serialization
        assert_eq!(manager.len(), 0);
        assert!(!manager.is_full());
    }

    #[test]
    fn test_buffer_manager_serialized_when_almost_full() {
        const MAX_INPUT_SIZE: usize = 10;
        const L: usize = 6;

        let serializer = MockSerializer { should_fail: false };
        let mut manager: BufferManager<1, MAX_INPUT_SIZE, 100, _> =
            BufferManager::new(1000.0, serializer, 1_000_000);

        // Fill buffer partially
        let sample_data1 = SampleData::<1, L> {
            t: 1000, // Non-zero timestamp
            len: L,
            samples: [[1.0; L]],
            t_last: 4000,
            fs: 1000.0,
        };

        match manager.push_data(&sample_data1) {
            BufferResult::Serialized(_packet) => {}
            other => panic!("Expected Serialized due to overflow, got {:?}", other),
        }

        assert_eq!(manager.len(), 0);
    }

    #[test]
    fn test_buffer_manager_retry_on_overflow() {
        const MAX_INPUT_SIZE: usize = 10;
        const L: usize = 4;

        let serializer = MockSerializer { should_fail: false };
        let mut manager: BufferManager<1, MAX_INPUT_SIZE, 100, _> =
            BufferManager::new(1000.0, serializer, 1_000_000);

        // Fill buffer partially
        let sample_data1 = SampleData::<1, L> {
            t: 1000, // Non-zero timestamp
            len: L,
            samples: [[1.0; L]],
            t_last: 4000,
            fs: 1000.0,
        };

        const L2: usize = 3;

        // Fill buffer partially
        let sample_data2 = SampleData::<1, L2> {
            t: 1000, // Non-zero timestamp
            len: L2,
            samples: [[1.0; L2]],
            t_last: 4000,
            fs: 1000.0,
        };

        manager.push_data(&sample_data1);
        assert_eq!(manager.len(), L);
        manager.push_data(&sample_data2);
        assert_eq!(manager.len(), L + L2);

        // Try to add more data than fits - should trigger serialization and retry
        let sample_data3 = SampleData::<1, L> {
            t: 10000, // Non-contiguous timestamp to force overflow
            len: L,
            samples: [[2.0; L]],
            t_last: 14000,
            fs: 1000.0,
        };

        match manager.push_data(&sample_data3) {
            BufferResult::Serialized(_packet) => {}
            other => panic!("Expected Serialized due to overflow, got {:?}", other),
        }

        // After retry, the buffer should contain the second data batch
        assert_eq!(manager.len(), L);
    }

    #[test]
    fn test_buffer_manager_serialization_failed() {
        const N: usize = 6;
        const L: usize = 3;

        let serializer = MockSerializer { should_fail: true };
        let mut manager: BufferManager<1, N, 100, _> =
            BufferManager::new(1000.0, serializer, 1_000_000);

        let sample_data = SampleData::<1, L> {
            t: 1000, // Non-zero timestamp
            len: L,
            samples: [[1.0; L]],
            t_last: 5000,
            fs: 1000.0,
        };

        // Add data first
        match manager.push_data(&sample_data) {
            BufferResult::DataAdded => {}
            other => panic!("Expected DataAdded, got {:?}", other),
        }

        // Add one more to trigger serialization (which will fail)
        // Too much data so we will first try to serialize.
        // This will fail, It will clear the buffer and add the sampels.
        // Try to serialize again. Will fail and will trigger the error.
        let sample_data2 = SampleData::<1, { 2 * L }> {
            t: 5000,
            len: 2 * L,
            samples: [[2.0; 2 * L]],
            t_last: 6000,
            fs: 1000.0,
        };

        // Should return SerializationFailed when serializer fails
        match manager.push_data(&sample_data2) {
            BufferResult::SerializationFailed => {}
            other => panic!("Expected SerializationFailed, got {:?}", other),
        }

        // Buffer should be cleared even after failed serialization
        assert_eq!(manager.len(), 0);
    }

    #[test]
    fn test_force_serialize() {
        const N: usize = 10;
        const L: usize = 3;

        let serializer = MockSerializer { should_fail: false };
        let mut manager: BufferManager<1, N, 100, _> =
            BufferManager::new(1000.0, serializer, 1_000_000);

        let sample_data = SampleData::<1, L> {
            t: 1000, // Non-zero timestamp
            len: L,
            samples: [[1.0; L]],
            t_last: 3000,
            fs: 1000.0,
        };

        // Add some data
        manager.push_data(&sample_data);
        assert_eq!(manager.len(), L);

        // Force serialize
        let result = manager.force_serialize();
        assert!(result.is_some());
        assert_eq!(manager.len(), 0); // Buffer should be cleared
    }

    #[test]
    fn test_force_serialize_empty_buffer() {
        let serializer = MockSerializer { should_fail: false };
        let mut manager = BufferManager::<1, 10, 100, _>::new(1000.0, serializer, 1_000_000);

        // Force serialize empty buffer should return None
        let result = manager.force_serialize();
        assert!(result.is_none());
    }

    #[test]
    fn test_serializing_buffer_directly() {
        const N: usize = 10;
        const L: usize = 3;

        let mut buffer: SerializingBuffer<1, N> = SerializingBuffer::empty(1000.0, 1_000_000);

        let sample_data = SampleData::<1, L> {
            t: 1000, // Non-zero timestamp
            len: L,
            samples: [[1.0; L]],
            t_last: 3000,
            fs: 1000.0,
        };

        let result = buffer.push(&sample_data);

        assert!(matches!(result, BufferStatus::Ok));
        assert_eq!(buffer.len(), L);
    }

    #[test]
    fn test_serializing_buffer_noncontiguous() {
        const N: usize = 10;
        const L: usize = 3;

        let mut buffer: SerializingBuffer<1, N> = SerializingBuffer::empty(1000.0, 1_000_000);

        let sample_data = SampleData::<1, L> {
            t: 1000, // Non-zero timestamp
            len: L,
            samples: [[1.0; L]],
            t_last: 3000,
            fs: 1000.0,
        };

        let result = buffer.push(&sample_data);

        assert!(matches!(result, BufferStatus::Ok));
        assert_eq!(buffer.len(), L);

        let sample_data = SampleData::<1, L> {
            t: 1_000_000, // Non-contiguous timestamp
            len: L,
            samples: [[1.0; L]],
            t_last: 3000,
            fs: 1000.0,
        };

        let result = buffer.push(&sample_data);

        assert!(matches!(result, BufferStatus::NotContiguous));
        assert_eq!(buffer.len(), L);
    }

    #[test]
    fn test_push_compile_time_assert_valid() {
        // Test that L <= N assertion passes when L is less than N
        const N: usize = 100;
        const L: usize = 50;

        let mut buffer: SerializingBuffer<3, N> = SerializingBuffer::empty(1000.0, 1_000_000);

        let sample_data = SampleData::<3, L> {
            t: 1000, // Non-zero timestamp
            len: L,
            samples: [[1.0; L], [2.0; L], [3.0; L]],
            t_last: 3,
            fs: 1000.0,
        };

        // This should compile and work without panicking
        assert!(matches!(buffer.push(&sample_data), BufferStatus::Ok));
    }

    #[test]
    fn test_buffer_timeout() {
        const MAX_INPUT_SIZE: usize = 100;
        const L: usize = 3;
        const TIMEOUT_US: i64 = 5000; // 5ms timeout

        let serializer = MockSerializer { should_fail: false };
        let mut manager: BufferManager<1, MAX_INPUT_SIZE, 100, _> =
            BufferManager::new(1000.0, serializer, TIMEOUT_US);

        // Add first data batch
        let sample_data1 = SampleData::<1, L> {
            t: 1000,
            len: L,
            samples: [[1.0; L]],
            t_last: 3000,
            fs: 1000.0,
        };

        match manager.push_data(&sample_data1) {
            BufferResult::DataAdded => {}
            other => panic!("Expected DataAdded, got {:?}", other),
        }

        assert_eq!(manager.len(), L);

        // Add second data batch that exceeds timeout
        let sample_data2 = SampleData::<1, L> {
            t: 3000, // Contiguous with first batch
            len: L,
            samples: [[2.0; L]],
            t_last: 1000 + TIMEOUT_US + 1000, // Exceeds timeout from first timestamp
            fs: 1000.0,
        };

        // Should trigger serialization due to timeout
        match manager.push_data(&sample_data2) {
            BufferResult::Serialized(_packet) => {}
            other => panic!("Expected Serialized due to timeout, got {:?}", other),
        }

        // Buffer should be cleared after timeout-triggered serialization
        assert_eq!(manager.len(), 0);
    }

    #[test]
    fn test_set_timeout() {
        const MAX_INPUT_SIZE: usize = 10;
        const INITIAL_TIMEOUT_US: i64 = 1000;
        const NEW_TIMEOUT_US: i64 = 5000;

        let serializer = MockSerializer { should_fail: false };
        let mut manager: BufferManager<1, MAX_INPUT_SIZE, 100, _> =
            BufferManager::new(1000.0, serializer, INITIAL_TIMEOUT_US);

        // Update timeout
        manager.set_timeout(NEW_TIMEOUT_US);

        // Verify timeout was updated by testing behavior
        let sample_data = SampleData::<1, 2> {
            t: 1000,
            len: 2,
            samples: [[1.0; 2]],
            t_last: 1000 + INITIAL_TIMEOUT_US + 1000, // Would exceed old timeout but not new one
            fs: 1000.0,
        };

        // Should not timeout with new timeout value
        match manager.push_data(&sample_data) {
            BufferResult::DataAdded => {}
            other => panic!("Expected DataAdded, got {:?}", other),
        }

        assert_eq!(manager.len(), 2);
    }

    #[test]
    fn test_non_contiguous_timestamp() {
        const MAX_INPUT_SIZE: usize = 100;
        const L: usize = 3;

        let serializer = MockSerializer { should_fail: false };
        let mut manager: BufferManager<1, MAX_INPUT_SIZE, 100, _> =
            BufferManager::new(1000.0, serializer, 1_000_000); // 1000 Hz, so t_delta_us = 2000

        // Add first data batch
        let sample_data1 = SampleData::<1, L> {
            t: 1000,
            len: L,
            samples: [[1.0; L]],
            t_last: 4000, // Ends at 4000
            fs: 1000.0,
        };

        match manager.push_data(&sample_data1) {
            BufferResult::DataAdded => {}
            other => panic!("Expected DataAdded, got {:?}", other),
        }

        assert_eq!(manager.len(), L);

        // Add second data batch with non-contiguous timestamp
        // Gap is larger than t_delta_us (2000), so should trigger serialization
        let sample_data2 = SampleData::<1, L> {
            t: 7000, // Gap of 3000 us from last timestamp (4000), exceeds t_delta_us of 2000
            len: L,
            samples: [[2.0; L]],
            t_last: 10000,
            fs: 1000.0,
        };

        // Should trigger serialization due to non-contiguous timestamps
        match manager.push_data(&sample_data2) {
            BufferResult::Serialized(_packet) => {}
            other => panic!(
                "Expected Serialized due to non-contiguous timestamps, got {:?}",
                other
            ),
        }

        // Buffer should now contain only the second batch after serialization and retry
        assert_eq!(manager.len(), L);
    }

    #[test]
    fn test_contiguous_timestamp() {
        const MAX_INPUT_SIZE: usize = 100;
        const L: usize = 3;

        let serializer = MockSerializer { should_fail: false };
        let mut manager: BufferManager<1, MAX_INPUT_SIZE, 100, _> =
            BufferManager::new(1000.0, serializer, 1_000_000); // 1000 Hz, so t_delta_us = 2000

        // Add first data batch (L=3 samples)
        let sample_data1 = SampleData::<1, L> {
            t: 1000,
            len: L,
            samples: [[1.0; L]],
            t_last: 4000,
            fs: 1000.0,
        };

        match manager.push_data(&sample_data1) {
            BufferResult::DataAdded => {}
            other => panic!("Expected DataAdded, got {:?}", other),
        }

        assert_eq!(manager.len(), L);

        // Add second data batch with contiguous timestamp (within t_delta_us tolerance)
        let sample_data2 = SampleData::<1, L> {
            t: 5000, // Gap of 1000 us from last timestamp (4000), within t_delta_us of 2000
            len: L,
            samples: [[2.0; L]],
            t_last: 8000,
            fs: 1000.0,
        };

        // Should add data without serialization since timestamps are contiguous
        match manager.push_data(&sample_data2) {
            BufferResult::DataAdded => {}
            other => panic!(
                "Expected DataAdded for contiguous timestamps, got {:?}",
                other
            ),
        }

        // Buffer should now contain both batches
        assert_eq!(manager.len(), L + L);
    }
}

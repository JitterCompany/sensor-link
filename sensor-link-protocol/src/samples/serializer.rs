use crate::{
    samples::{q15xl, UniformSamples},
    serialize::{self, SerializedSendable, Topic, TOPIC_HEADER_SIZE},
};

#[derive(Debug, Clone, PartialEq)]
pub enum SerializeError {
    /// Buffer provided is smaller than the minimum required size
    BufferTooSmall {
        /// Size of the buffer that was provided
        provided: usize,
        /// Minimum size required for the operation
        required: usize,
    },
    /// Failed to serialize the topic header
    TopicSerializationFailed {
        /// Size of buffer available for topic header
        available: usize,
    },
    /// Failed to extend data into the channel
    ChannelExtendFailed {
        /// Channel index that failed (0 for values, 1 for fractions)
        channel_index: usize,
        /// Number of samples attempted to add
        samples_attempted: usize,
        /// Current capacity of the channel
        channel_capacity: usize,
    },
    /// Failed to encode data as Q15 format
    Q15EncodingFailed {
        /// Number of bytes available for Q15 data
        available_bytes: usize,
        /// Number of samples attempted to encode
        sample_count: usize,
    },
}

impl SerializeError {
    /// Creates a new BufferTooSmall error with size information
    pub fn buffer_too_small(provided: usize, required: usize) -> Self {
        Self::BufferTooSmall { provided, required }
    }

    /// Creates a new TopicSerializationFailed error
    pub fn topic_failed(available: usize) -> Self {
        Self::TopicSerializationFailed { available }
    }

    /// Creates a new ChannelExtendFailed error
    pub fn channel_extend_failed(
        channel_index: usize,
        samples_attempted: usize,
        channel_capacity: usize,
    ) -> Self {
        Self::ChannelExtendFailed {
            channel_index,
            samples_attempted,
            channel_capacity,
        }
    }

    /// Creates a new Q15EncodingFailed error
    pub fn q15_failed(available_bytes: usize, sample_count: usize) -> Self {
        Self::Q15EncodingFailed {
            available_bytes,
            sample_count,
        }
    }
}

pub struct UniformSampleSerializer<const N_CH: usize, const MAX_OUTPUT_LEN: usize>;

impl<const N_CH: usize, const MAX_OUTPUT_LEN: usize> UniformSampleSerializer<N_CH, MAX_OUTPUT_LEN> {
    /// Maximum number of samples that can be serialized into a size `MAX_OUTPUT_LEN`
    pub const MAX_INPUT_LEN: usize = Self::calc_max_input_len();

    const fn calc_max_input_len() -> usize {
        const DUMMY: usize = 0;
        assert!(MAX_OUTPUT_LEN > TOPIC_HEADER_SIZE);
        q15xl::Uniform::<q15xl::Q15XL<N_CH, DUMMY>>::max_num_input_samples(
            MAX_OUTPUT_LEN - TOPIC_HEADER_SIZE,
        )
    }

    pub fn serialize<'buffer, const M: usize, T: Topic>(
        input: &UniformSamples<N_CH, M>,
        topic: T,
    ) -> Result<SerializedSendable<MAX_OUTPUT_LEN, T>, SerializeError> {
        // Compile-time check that M doesn't exceed MAX_INPUT_LEN
        const { assert!(M <= Self::MAX_INPUT_LEN) }

        let mut serialized = serialize::Builder::with_topic(topic);

        // For dynamic sample counts, check if it exceeds what we can fit
        if input.len() > Self::MAX_INPUT_LEN {
            return Err(SerializeError::buffer_too_small(
                input.len(),
                Self::MAX_INPUT_LEN,
            ));
        }

        // Convert to Q15XL format first
        let q15_input = q15xl::Uniform::<q15xl::Q15XL<N_CH, M>>::from_uniform(input);

        // Serialize Q15XL directly into the payload buffer
        let q15_bytes = q15_input
            .as_topic_data(serialized.payload_buffer())
            .map_err(|_| {
                SerializeError::q15_failed(
                    SerializedSendable::<MAX_OUTPUT_LEN, T>::MAX_PAYLOAD_LEN,
                    input.len(),
                )
            })?;
        let payload_len = q15_bytes.len();

        serialized.create_with_payload_length(payload_len).map_err(
            |build_error| match build_error {
                serialize::BuildError::TopicSerializeFailed => {
                    SerializeError::topic_failed(TOPIC_HEADER_SIZE)
                }
                serialize::BuildError::PayloadTooLong => SerializeError::buffer_too_small(
                    SerializedSendable::<MAX_OUTPUT_LEN, T>::MAX_PAYLOAD_LEN,
                    payload_len,
                ),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samples::q15xl;

    #[test]
    fn test_calc_max_input_len_formula() {
        // Test specific cases with const values to verify the calculation formula

        // Test case 1: 1 channel, 256 byte buffer
        {
            const MAX_OUTPUT_LEN: usize = 256;
            const N_CH: usize = 1;
            let calculated = UniformSampleSerializer::<N_CH, MAX_OUTPUT_LEN>::calc_max_input_len();
            let expected = calc_expected_max_input_len(N_CH, MAX_OUTPUT_LEN);
            assert_eq!(calculated, expected, "1 channel, 256 byte buffer");
        }

        // Test case 2: 2 channels, 512 byte buffer
        {
            const MAX_OUTPUT_LEN: usize = 512;
            const N_CH: usize = 2;
            let calculated = UniformSampleSerializer::<N_CH, MAX_OUTPUT_LEN>::calc_max_input_len();
            let expected = calc_expected_max_input_len(N_CH, MAX_OUTPUT_LEN);
            assert_eq!(calculated, expected, "2 channels, 512 byte buffer");
        }

        // Test case 3: 3 channels, 1024 byte buffer
        {
            const MAX_OUTPUT_LEN: usize = 1024;
            const N_CH: usize = 3;
            let calculated = UniformSampleSerializer::<N_CH, MAX_OUTPUT_LEN>::calc_max_input_len();
            let expected = calc_expected_max_input_len(N_CH, MAX_OUTPUT_LEN);
            assert_eq!(calculated, expected, "3 channels, 1024 byte buffer");
        }

        // Test case 4: 1 channel, 1024 byte buffer
        {
            const MAX_OUTPUT_LEN: usize = 1024;
            const N_CH: usize = 1;
            let calculated = UniformSampleSerializer::<N_CH, MAX_OUTPUT_LEN>::calc_max_input_len();
            let expected = calc_expected_max_input_len(N_CH, MAX_OUTPUT_LEN);
            assert_eq!(calculated, expected, "1 channel, 1024 byte buffer");
        }
    }

    fn calc_expected_max_input_len(n_ch: usize, max_output_len: usize) -> usize {
        if max_output_len <= TOPIC_HEADER_SIZE {
            0
        } else {
            let available_for_payload = max_output_len - TOPIC_HEADER_SIZE;
            let required_overhead = q15xl::UNIFORM_HEADER_OVERHEAD + q15xl::Q15_OVERHEAD_PER_PACKET;

            if available_for_payload < required_overhead || n_ch == 0 {
                0
            } else {
                let available_bytes = available_for_payload - required_overhead;
                let required_per_channel =
                    q15xl::Q15_OVERHEAD_PER_CHANNEL + q15xl::Q15_BYTES_PER_SAMPLE;

                if available_bytes < n_ch * required_per_channel {
                    0
                } else {
                    let available_bytes_per_channel = available_bytes / n_ch;
                    (available_bytes_per_channel - q15xl::Q15_OVERHEAD_PER_CHANNEL)
                        / q15xl::Q15_BYTES_PER_SAMPLE
                }
            }
        }
    }

    #[test]
    fn test_calc_max_input_len_edge_cases() {
        // Test edge cases that should return 0

        // Note: We can't test buffers smaller than TOPIC_HEADER_SIZE because of the assertion
        // in calc_max_input_len(), but we can test the runtime logic with our helper function
        assert_eq!(0, calc_expected_max_input_len(1, 7));
        assert_eq!(0, calc_expected_max_input_len(3, 7));
        assert_eq!(0, calc_expected_max_input_len(1, TOPIC_HEADER_SIZE));
        assert_eq!(0, calc_expected_max_input_len(3, TOPIC_HEADER_SIZE));

        // Buffer barely larger than topic header but too small for any samples
        const TINY_BUFFER: usize =
            TOPIC_HEADER_SIZE + q15xl::UNIFORM_HEADER_OVERHEAD + q15xl::Q15_OVERHEAD_PER_PACKET - 1;
        assert_eq!(
            0,
            UniformSampleSerializer::<1, { TINY_BUFFER }>::calc_max_input_len()
        );
        assert_eq!(
            0,
            UniformSampleSerializer::<3, { TINY_BUFFER }>::calc_max_input_len()
        );

        // Buffer with enough space for headers but not enough for even one sample per channel
        const MIN_OVERHEAD: usize =
            TOPIC_HEADER_SIZE + q15xl::UNIFORM_HEADER_OVERHEAD + q15xl::Q15_OVERHEAD_PER_PACKET;
        const INSUFFICIENT_BUFFER: usize =
            MIN_OVERHEAD + 3 * (q15xl::Q15_OVERHEAD_PER_CHANNEL + q15xl::Q15_BYTES_PER_SAMPLE) - 1;
        assert_eq!(
            0,
            UniformSampleSerializer::<3, { INSUFFICIENT_BUFFER }>::calc_max_input_len()
        );

        // Buffer with exactly enough space for one sample per channel
        const EXACTLY_ONE_SAMPLE: usize =
            MIN_OVERHEAD + 3 * (q15xl::Q15_OVERHEAD_PER_CHANNEL + q15xl::Q15_BYTES_PER_SAMPLE);
        assert_eq!(
            1,
            UniformSampleSerializer::<3, { EXACTLY_ONE_SAMPLE }>::calc_max_input_len()
        );

        // Test that the manual calculation matches for these cases
        assert_eq!(
            UniformSampleSerializer::<1, { TINY_BUFFER }>::calc_max_input_len(),
            calc_expected_max_input_len(1, TINY_BUFFER)
        );
        assert_eq!(
            UniformSampleSerializer::<3, { INSUFFICIENT_BUFFER }>::calc_max_input_len(),
            calc_expected_max_input_len(3, INSUFFICIENT_BUFFER)
        );
        assert_eq!(
            UniformSampleSerializer::<3, { EXACTLY_ONE_SAMPLE }>::calc_max_input_len(),
            calc_expected_max_input_len(3, EXACTLY_ONE_SAMPLE)
        );
    }

    #[test]
    fn test_calc_max_input_len_consistency() {
        // Verify that the calculation is consistent across different usage patterns

        // Test that MAX_INPUT_LEN constant equals calc_max_input_len() function
        assert_eq!(
            UniformSampleSerializer::<1, 256>::MAX_INPUT_LEN,
            UniformSampleSerializer::<1, 256>::calc_max_input_len()
        );

        assert_eq!(
            UniformSampleSerializer::<2, 512>::MAX_INPUT_LEN,
            UniformSampleSerializer::<2, 512>::calc_max_input_len()
        );

        assert_eq!(
            UniformSampleSerializer::<3, 1024>::MAX_INPUT_LEN,
            UniformSampleSerializer::<3, 1024>::calc_max_input_len()
        );
    }

    #[test]
    fn test_calc_max_input_len_monotonic_properties() {
        // Test that more buffer space allows for more samples (monotonic property)
        let small_buffer = UniformSampleSerializer::<2, 256>::calc_max_input_len();
        let medium_buffer = UniformSampleSerializer::<2, 512>::calc_max_input_len();
        let large_buffer = UniformSampleSerializer::<2, 1024>::calc_max_input_len();

        assert!(small_buffer <= medium_buffer);
        assert!(medium_buffer <= large_buffer);

        // Test that fewer channels allow for more samples per channel (with same buffer size)
        let one_channel = UniformSampleSerializer::<1, 512>::calc_max_input_len();
        let two_channels = UniformSampleSerializer::<2, 512>::calc_max_input_len();
        let three_channels = UniformSampleSerializer::<3, 512>::calc_max_input_len();

        assert!(one_channel >= two_channels);
        assert!(two_channels >= three_channels);
    }
}

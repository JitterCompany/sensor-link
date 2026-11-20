use crate::{SerializedSendable, TopicFromDevice};
#[cfg(test)]
use demo_protocol::DemoChannel;
use demo_protocol::{DemoData, NUM_CH_DEMO};
use sensor_link_firmware::logic::serializer::{SerializeError, UniformSampleSerializer};

const NUM_CH: usize = NUM_CH_DEMO;
pub struct DemoDataSerializer<const MAX_OUTPUT_LEN: usize>;

impl<const MAX_OUTPUT_LEN: usize> DemoDataSerializer<MAX_OUTPUT_LEN> {
    /// Maximum number of samples that can be serialized into a size `MAX_OUTPUT_LEN`
    pub const MAX_INPUT_LEN: usize =
        UniformSampleSerializer::<NUM_CH, MAX_OUTPUT_LEN>::MAX_INPUT_LEN;

    pub fn serialize<const M: usize>(
        input: &DemoData<M>,
    ) -> Result<SerializedSendable<MAX_OUTPUT_LEN>, SerializeError> {
        let topic = TopicFromDevice::Data;
        UniformSampleSerializer::<NUM_CH, MAX_OUTPUT_LEN>::serialize(input, topic)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sensor_link_firmware::serialize::TOPIC_HEADER_SIZE;

    /// Maximum number of samples (per measurement) fitting in a `MAX_OUTPUT_LEN`-sized message
    const MAX_LEN: usize = DemoDataSerializer::<MAX_OUTPUT_LEN>::MAX_INPUT_LEN;
    const MAX_OUTPUT_LEN: usize = 1024;

    pub type DemoFixed = DemoData<MAX_LEN>;

    fn empty_data(t: i64, fs: f32) -> DemoFixed {
        DemoFixed {
            t,
            fs,
            ch: core::array::from_fn(|_| heapless::Vec::new()),
        }
    }

    /// All measurements of the parsed data, in [`DemoChannel`] order
    #[cfg(feature = "use-std")]
    fn parsed_channels(parsed: &demo_protocol::data::std::Data) -> [&Vec<f32>; NUM_CH] {
        DemoChannel::ALL.map(|ch| match ch {
            DemoChannel::BoardTemperature => &parsed.board_temperature,
            DemoChannel::Vbus => &parsed.vbus,
            DemoChannel::Ibus => &parsed.ibus,
            DemoChannel::Vsys => &parsed.vsys,
        })
    }

    /// Test value for measurement `ch`, sample `i`: a sine wave, phase-shifted per measurement
    fn test_value(ch: usize, i: usize) -> f32 {
        ((i + ch) as f32 * 0.1).sin() * 0.5
    }

    #[test]
    #[cfg(feature = "use-std")]
    fn test_serialize_deserialize_roundtrip() {
        use demo_protocol::server::parse_data;

        // Test with various sample counts
        const TEST_CASES: &[usize] = &[1, 5, 10, 50, MAX_LEN];

        for &num_samples in TEST_CASES {
            let timestamp = 1_234_567_890;
            let sample_rate = 500.0;

            let mut data = empty_data(timestamp, sample_rate);

            // Fill every measurement with test values
            for (ch, samples) in data.ch.iter_mut().enumerate() {
                for i in 0..num_samples {
                    samples.push(test_value(ch, i)).unwrap();
                }
            }

            // Serialize using DemoDataSerializer
            let serialized = DemoDataSerializer::<MAX_OUTPUT_LEN>::serialize(&data)
                .expect("Serialization should succeed");

            // The serialized data includes a topic header, skip it for parse_data
            assert!(
                serialized.header_bytes().len() == TOPIC_HEADER_SIZE,
                "Serialized data too small"
            );
            let q15_data = serialized.payload_bytes();

            // Deserialize using parse_data
            let parsed = parse_data(q15_data).expect("Deserialization should succeed");

            // Verify the results
            assert_eq!(
                parsed.fs, sample_rate,
                "Sample rate mismatch for {} samples",
                num_samples
            );
            assert_eq!(parsed.t.len(), num_samples, "Number of timestamps mismatch");

            // Verify timestamps are correct
            let expected_interval_us = (1_000_000.0 / sample_rate) as i64;
            for i in 0..num_samples {
                let expected_timestamp = timestamp + (i as i64) * expected_interval_us;
                assert_eq!(
                    parsed.t[i], expected_timestamp,
                    "Timestamp mismatch at index {} for {} samples",
                    i, num_samples
                );
            }

            // Verify values are approximately correct (allowing for Q15 quantization error)
            for (ch, samples) in parsed_channels(&parsed).into_iter().enumerate() {
                assert_eq!(
                    samples.len(),
                    num_samples,
                    "Number of samples mismatch for ch {ch}"
                );

                for (i, &actual) in samples.iter().enumerate() {
                    let expected = test_value(ch, i);
                    let error = (actual - expected).abs();

                    // Q15 has ~15 bits of precision, so we allow for some quantization error
                    // The error should be less than 0.001 for values in range [-1, 1]
                    assert!(
                        error < 0.001,
                        "Value mismatch for ch {} at index {} for {} samples: expected {}, got {}, error {}",
                        ch,
                        i,
                        num_samples,
                        expected,
                        actual,
                        error
                    );
                }
            }
        }
    }

    #[test]
    #[cfg(feature = "use-std")]
    fn test_serialize_deserialize_edge_cases() {
        use demo_protocol::server::parse_data;

        // Test with empty data (0 samples)
        {
            let data = empty_data(1_000_000, 100.0);

            let serialized = DemoDataSerializer::<MAX_OUTPUT_LEN>::serialize(&data)
                .expect("Empty serialization should succeed");

            let q15_data = serialized.payload_bytes();
            let parsed = parse_data(q15_data).expect("Empty deserialization should succeed");

            assert_eq!(parsed.t.len(), 0);
            assert_eq!(parsed.fs, 100.0);
            for samples in parsed_channels(&parsed) {
                assert_eq!(samples.len(), 0);
            }
        }

        // Test with single sample
        {
            let mut data = empty_data(2_000_000, 250.0);
            for (ch, samples) in data.ch.iter_mut().enumerate() {
                samples.push(0.42 - ch as f32 * 0.1).unwrap();
            }

            let serialized = DemoDataSerializer::<MAX_OUTPUT_LEN>::serialize(&data)
                .expect("Single sample serialization should succeed");

            let q15_data = serialized.payload_bytes();
            let parsed =
                parse_data(q15_data).expect("Single sample deserialization should succeed");

            assert_eq!(parsed.t.len(), 1);
            assert_eq!(parsed.t[0], 2_000_000);
            for (ch, samples) in parsed_channels(&parsed).into_iter().enumerate() {
                assert!((samples[0] - (0.42 - ch as f32 * 0.1)).abs() < 0.001);
            }
        }

        // Test with extreme values
        {
            let mut data = empty_data(3_000_000, 1000.0);

            // Test values near the limits of Q15 representation
            let test_values = [0.99, -0.99, 0.0, 0.001, -0.001];
            for samples in data.ch.iter_mut() {
                for &val in &test_values {
                    samples.push(val).unwrap();
                }
            }

            let serialized = DemoDataSerializer::<MAX_OUTPUT_LEN>::serialize(&data)
                .expect("Extreme values serialization should succeed");

            let q15_data = serialized.payload_bytes();
            let parsed =
                parse_data(q15_data).expect("Extreme values deserialization should succeed");

            for (ch, samples) in parsed_channels(&parsed).into_iter().enumerate() {
                assert_eq!(samples.len(), test_values.len());
                for (i, &expected) in test_values.iter().enumerate() {
                    let actual = samples[i];
                    let error = (actual - expected).abs();
                    assert!(
                        error < 0.001,
                        "Extreme value mismatch for ch {} at index {}: expected {}, got {}",
                        ch,
                        i,
                        expected,
                        actual
                    );
                }
            }
        }
    }
}

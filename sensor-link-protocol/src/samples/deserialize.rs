use super::{q15, q15xl, ParseError};

/// Parse Q15 encoded data directly into NChannelSamples (std only)
///
/// This function parses Q15 compressed uniform samples without requiring
/// a compile-time MAX_N_SAMPLES constant, using dynamic allocation instead.
pub fn parse_q15_to_nchannel<const N_CH: usize>(
    bytes: &[u8],
) -> Result<super::NChannelSamples<N_CH>, ParseError> {
    use super::NChannelSamples;

    // Minimum size: timestamp (8) + fs (4)
    if bytes.len() < q15::UNIFORM_HEADER_OVERHEAD {
        return Err(ParseError::Deserialize(
            "Buffer too small for header".into(),
        ));
    }

    // Parse header
    let t = i64::from_le_bytes(
        bytes[0..8]
            .try_into()
            .map_err(|e: core::array::TryFromSliceError| ParseError::Deserialize(e.to_string()))?,
    );
    let fs = f32::from_le_bytes(
        bytes[8..12]
            .try_into()
            .map_err(|e: core::array::TryFromSliceError| ParseError::Deserialize(e.to_string()))?,
    );

    // Verify header values
    if t <= 0 {
        return Err(ParseError::Invalid);
    }
    if fs <= 0.0 || fs.is_nan() {
        return Err(ParseError::Invalid);
    }

    // Parse Q15 data
    let q15_bytes = &bytes[12..];
    if q15_bytes.is_empty() {
        // Empty data - return empty NChannelSamples
        return Ok(NChannelSamples {
            t: Vec::new(),
            fs,
            ch: core::array::from_fn(|_| Vec::new()),
        });
    }

    // Get number of samples
    let num_samples = q15_bytes[0] as usize;
    if num_samples == 0 {
        // No samples
        return Ok(NChannelSamples {
            t: Vec::new(),
            fs,
            ch: core::array::from_fn(|_| Vec::new()),
        });
    }

    // Calculate expected size
    let bytes_per_channel =
        q15::Q15_OVERHEAD_PER_CHANNEL + (q15::Q15_BYTES_PER_SAMPLE * num_samples);
    let expected_size = q15::Q15_OVERHEAD_PER_PACKET + (N_CH * bytes_per_channel);

    if q15_bytes.len() < expected_size {
        return Err(ParseError::Deserialize(format!(
            "Buffer too small: expected {} bytes, got {}",
            expected_size,
            q15_bytes.len()
        )));
    }

    // Parse channel data
    let mut channels: [Vec<f32>; N_CH] = core::array::from_fn(|_| Vec::with_capacity(num_samples));

    let data_bytes = &q15_bytes[1..]; // Skip num_samples byte

    for ch_idx in 0..N_CH {
        let ch_start = ch_idx * bytes_per_channel;
        let ch_bytes = &data_bytes[ch_start..ch_start + bytes_per_channel];

        // Parse exponent
        let exponent = ch_bytes[0] as i8;
        let scale_factor = libm::powf(2.0, exponent as f32) / (i16::MAX as f32);

        // Parse samples
        let sample_bytes = &ch_bytes[1..];
        for sample_idx in 0..num_samples {
            let byte_idx = sample_idx * 2;
            let sample_i16 =
                i16::from_le_bytes([sample_bytes[byte_idx], sample_bytes[byte_idx + 1]]);

            // Convert to f32 (handling NaN)
            let value = if sample_i16 == i16::MIN {
                f32::NAN
            } else {
                (sample_i16 as f32) * scale_factor
            };

            // Verify the value
            if value.is_nan() {
                return Err(ParseError::Invalid);
            }

            channels[ch_idx].push(value);
        }
    }

    // Generate timestamps
    let sample_interval_us = (1_000_000.0 / fs) as i64;
    let timestamps: Vec<i64> = (0..num_samples)
        .map(|i| t + (i as i64) * sample_interval_us)
        .collect();

    Ok(NChannelSamples {
        t: timestamps,
        fs,
        ch: channels,
    })
}

/// Parse Q15XL encoded data directly into NChannelSamples (std only)
///
/// This function parses Q15XL compressed uniform samples without requiring
/// a compile-time MAX_N_SAMPLES constant, using dynamic allocation instead.
pub fn parse_q15xl_to_nchannel<const N_CH: usize>(
    bytes: &[u8],
) -> Result<super::NChannelSamples<N_CH>, ParseError> {
    parse_q15xl_to_nchannel_impl::<N_CH>(bytes, false)
}

/// Parse Q15XL encoded data directly into NChannelSamples (std only)
///
/// Same as [`parse_q15xl_to_nchannel`], but samples encoded as NaN are kept as
/// [`f32::NAN`] instead of rejecting the message.
pub fn parse_q15xl_to_nchannel_allow_nan<const N_CH: usize>(
    bytes: &[u8],
) -> Result<super::NChannelSamples<N_CH>, ParseError> {
    parse_q15xl_to_nchannel_impl::<N_CH>(bytes, true)
}

fn parse_q15xl_to_nchannel_impl<const N_CH: usize>(
    bytes: &[u8],
    allow_nan: bool,
) -> Result<super::NChannelSamples<N_CH>, ParseError> {
    use super::NChannelSamples;

    // Minimum size: timestamp (8) + fs (4)
    if bytes.len() < q15xl::UNIFORM_HEADER_OVERHEAD {
        return Err(ParseError::Deserialize(
            "Buffer too small for header".into(),
        ));
    }

    // Parse header
    let t = i64::from_le_bytes(
        bytes[0..8]
            .try_into()
            .map_err(|e: core::array::TryFromSliceError| ParseError::Deserialize(e.to_string()))?,
    );
    let fs = f32::from_le_bytes(
        bytes[8..12]
            .try_into()
            .map_err(|e: core::array::TryFromSliceError| ParseError::Deserialize(e.to_string()))?,
    );

    // Verify header values
    if t <= 0 {
        return Err(ParseError::Invalid);
    }
    if fs <= 0.0 || fs.is_nan() {
        return Err(ParseError::Invalid);
    }

    // Parse Q15XL data
    let q15xl_bytes = &bytes[12..];
    if q15xl_bytes.len() < 2 {
        // Empty data or insufficient data for length field - return empty NChannelSamples
        return Ok(NChannelSamples {
            t: Vec::new(),
            fs,
            ch: core::array::from_fn(|_| Vec::new()),
        });
    }

    // Get number of samples (Q15XL uses u16 length field)
    let num_samples = u16::from_le_bytes([q15xl_bytes[0], q15xl_bytes[1]]) as usize;
    if num_samples == 0 {
        // No samples
        return Ok(NChannelSamples {
            t: Vec::new(),
            fs,
            ch: core::array::from_fn(|_| Vec::new()),
        });
    }

    // Calculate expected size
    let bytes_per_channel =
        q15xl::Q15_OVERHEAD_PER_CHANNEL + (q15xl::Q15_BYTES_PER_SAMPLE * num_samples);
    let expected_size = q15xl::Q15_OVERHEAD_PER_PACKET + (N_CH * bytes_per_channel);

    if q15xl_bytes.len() < expected_size {
        return Err(ParseError::Deserialize(format!(
            "Buffer too small: expected {} bytes, got {}",
            expected_size,
            q15xl_bytes.len()
        )));
    }

    // Parse channel data
    let mut channels: [Vec<f32>; N_CH] = core::array::from_fn(|_| Vec::with_capacity(num_samples));

    let data_bytes = &q15xl_bytes[2..]; // Skip 2-byte num_samples field

    for ch_idx in 0..N_CH {
        let ch_start = ch_idx * bytes_per_channel;
        let ch_bytes = &data_bytes[ch_start..ch_start + bytes_per_channel];

        // Parse exponent
        let exponent = ch_bytes[0] as i8;
        let scale_factor = libm::powf(2.0, exponent as f32) / (i16::MAX as f32);

        // Parse samples
        let sample_bytes = &ch_bytes[1..];
        for sample_idx in 0..num_samples {
            let byte_idx = sample_idx * 2;
            let sample_i16 =
                i16::from_le_bytes([sample_bytes[byte_idx], sample_bytes[byte_idx + 1]]);

            // Convert to f32 (handling NaN)
            let value = if sample_i16 == i16::MIN {
                f32::NAN
            } else {
                (sample_i16 as f32) * scale_factor
            };

            // Verify the value
            if value.is_nan() && !allow_nan {
                return Err(ParseError::Invalid);
            }

            channels[ch_idx].push(value);
        }
    }

    // Generate timestamps
    let sample_interval_us = (1_000_000.0 / fs) as i64;
    let timestamps: Vec<i64> = (0..num_samples)
        .map(|i| t + (i as i64) * sample_interval_us)
        .collect();

    Ok(NChannelSamples {
        t: timestamps,
        fs,
        ch: channels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samples::{q15, q15xl, UniformSamples};

    #[test]
    fn test_parse_q15_to_nchannel() {
        use q15::{Uniform, Q15};
        // Test the new std-only parsing function that doesn't require MAX_SAMPLES_PER_MESSAGE

        // Create test data with varying number of samples
        const N_CH: usize = 3;
        // Calculate max samples that fit in MAX_MESSAGE_LEN (1500 bytes)
        // Header: 12 bytes (timestamp + fs) + 1 byte (num_samples)
        // Per channel: 1 byte (exponent) + 2 bytes per sample
        // Max samples = (1500 - 12 - 1 - 3*1) / (3*2) = 1484/6 = 247
        let test_cases = vec![
            10,  // Small number of samples
            100, // Typical number of samples
            240, // Current MAX_SAMPLES_PER_MESSAGE
            245, // Close to maximum that fits in 1500 bytes
        ];

        for num_samples in test_cases {
            // Create uniform samples with the specific number of samples (using 255 as max)
            let mut samples = UniformSamples::<N_CH, 255>::empty_at(1_000_000, 1000.0);

            // Add test data
            for i in 0..num_samples {
                for ch in 0..N_CH {
                    samples.ch[ch]
                        .push((i as f32 + ch as f32 * 0.1) / 100.0)
                        .unwrap();
                }
            }

            // Convert to Q15 format
            let q15 = Uniform::<Q15<N_CH, 255>>::from_uniform(&samples);

            // Serialize to bytes
            let mut buffer = [0u8; 4096];
            let bytes = q15.as_topic_data(&mut buffer).unwrap();

            // Parse using the new function (no MAX_SAMPLES_PER_MESSAGE required)
            let result = parse_q15_to_nchannel::<N_CH>(bytes).unwrap();

            // Verify the results
            assert_eq!(result.fs, 1000.0);
            assert_eq!(result.t.len(), num_samples);
            assert_eq!(result.ch[0].len(), num_samples);

            // Verify timestamps are correct
            let expected_interval_us = 1000; // 1000Hz = 1000us interval
            for i in 0..num_samples {
                assert_eq!(result.t[i], 1_000_000 + (i as i64) * expected_interval_us);
            }

            // Verify values are approximately correct (allowing for Q15 quantization)
            for i in 0..num_samples {
                for ch in 0..N_CH {
                    let expected = (i as f32 + ch as f32 * 0.1) / 100.0;
                    let actual = result.ch[ch][i];
                    assert!(
                        (actual - expected).abs() < 0.001,
                        "Sample {}, channel {}: expected {}, got {}",
                        i,
                        ch,
                        expected,
                        actual
                    );
                }
            }
        }
    }

    #[test]
    fn test_parse_q15_to_nchannel_edge_cases() {
        use q15::{Uniform, Q15};
        // Test with zero samples (but valid header)
        {
            // Manually create the minimal valid Q15 message with 0 samples
            let mut buffer = vec![0u8; 13];
            // Timestamp: 1_000_000 as i64 little-endian
            buffer[0..8].copy_from_slice(&1_000_000i64.to_le_bytes());
            // Sample frequency: 500.0 as f32 little-endian
            buffer[8..12].copy_from_slice(&500.0f32.to_le_bytes());
            // Number of samples: 0
            buffer[12] = 0;

            let result = parse_q15_to_nchannel::<2>(&buffer).unwrap();
            assert_eq!(result.t.len(), 0);
            assert_eq!(result.ch[0].len(), 0);
            assert_eq!(result.ch[1].len(), 0);
            assert_eq!(result.fs, 500.0);
        }

        // Test single sample
        {
            let mut samples = UniformSamples::<1, 100>::empty_at(2_000_000, 100.0);
            samples.ch[0].push(0.123).unwrap();

            let q15 = Uniform::<Q15<1, 100>>::from_uniform(&samples);
            let mut buffer = [0u8; 256];
            let bytes = q15.as_topic_data(&mut buffer).unwrap();

            let result = parse_q15_to_nchannel::<1>(bytes).unwrap();
            assert_eq!(result.t.len(), 1);
            assert_eq!(result.t[0], 2_000_000);
            assert!((result.ch[0][0] - 0.123).abs() < 0.001);
        }
    }

    #[test]
    fn test_parse_q15xl_to_nchannel() {
        use q15xl::{Uniform, Q15XL};
        // Test the new std-only parsing function that doesn't require MAX_SAMPLES_PER_MESSAGE

        // Create test data with varying number of samples
        const N_CH: usize = 3;
        // Calculate max samples that fit in MAX_MESSAGE_LEN (1500 bytes)
        // Header: 12 bytes (timestamp + fs) + 2 byte (num_samples)
        // Per channel: 1 byte (exponent) + 2 bytes per sample
        // Max samples = (1500 - 12 - 2 - 3*1) / (3*2) = 1483/6 = 247
        let test_cases = vec![
            10,  // Small number of samples
            100, // Typical number of samples
            240, // Current MAX_SAMPLES_PER_MESSAGE
            245, // Close to maximum that fits in 1500 bytes
        ];

        for num_samples in test_cases {
            // Create uniform samples with the specific number of samples (using 255 as max)
            let mut samples = UniformSamples::<N_CH, 255>::empty_at(1_000_000, 1000.0);

            // Add test data
            for i in 0..num_samples {
                for ch in 0..N_CH {
                    samples.ch[ch]
                        .push((i as f32 + ch as f32 * 0.1) / 100.0)
                        .unwrap();
                }
            }

            // Convert to Q15XL format
            let q15xl = Uniform::<Q15XL<N_CH, 255>>::from_uniform(&samples);

            // Serialize to bytes
            let mut buffer = [0u8; 4096];
            let bytes = q15xl.as_topic_data(&mut buffer).unwrap();

            // Parse using the new function (no MAX_SAMPLES_PER_MESSAGE required)
            let result = parse_q15xl_to_nchannel::<N_CH>(bytes).unwrap();

            // Verify the results
            assert_eq!(result.fs, 1000.0);
            assert_eq!(result.t.len(), num_samples);
            assert_eq!(result.ch[0].len(), num_samples);

            // Verify timestamps are correct
            let expected_interval_us = 1000; // 1000Hz = 1000us interval
            for i in 0..num_samples {
                assert_eq!(result.t[i], 1_000_000 + (i as i64) * expected_interval_us);
            }

            // Verify values are approximately correct (allowing for Q15 quantization)
            for i in 0..num_samples {
                for ch in 0..N_CH {
                    let expected = (i as f32 + ch as f32 * 0.1) / 100.0;
                    let actual = result.ch[ch][i];
                    assert!(
                        (actual - expected).abs() < 0.001,
                        "Sample {}, channel {}: expected {}, got {}",
                        i,
                        ch,
                        expected,
                        actual
                    );
                }
            }
        }
    }

    #[test]
    fn test_parse_q15xl_to_nchannel_edge_cases() {
        use q15xl::{Uniform, Q15XL};
        // Test with zero samples (but valid header)
        {
            // Manually create the minimal valid Q15XL message with 0 samples
            let mut buffer = vec![0u8; 14];
            // Timestamp: 1_000_000 as i64 little-endian
            buffer[0..8].copy_from_slice(&1_000_000i64.to_le_bytes());
            // Sample frequency: 500.0 as f32 little-endian
            buffer[8..12].copy_from_slice(&500.0f32.to_le_bytes());
            // Number of samples: 0 as u16 little-endian
            buffer[12..14].copy_from_slice(&0u16.to_le_bytes());

            let result = parse_q15xl_to_nchannel::<2>(&buffer).unwrap();
            assert_eq!(result.t.len(), 0);
            assert_eq!(result.ch[0].len(), 0);
            assert_eq!(result.ch[1].len(), 0);
            assert_eq!(result.fs, 500.0);
        }

        // Test single sample
        {
            let mut samples = UniformSamples::<1, 100>::empty_at(2_000_000, 100.0);
            samples.ch[0].push(0.123).unwrap();

            let q15xl = Uniform::<Q15XL<1, 100>>::from_uniform(&samples);
            let mut buffer = [0u8; 256];
            let bytes = q15xl.as_topic_data(&mut buffer).unwrap();

            let result = parse_q15xl_to_nchannel::<1>(bytes).unwrap();
            assert_eq!(result.t.len(), 1);
            assert_eq!(result.t[0], 2_000_000);
            assert!((result.ch[0][0] - 0.123).abs() < 0.001);
        }
    }
}

#![allow(rustdoc::private_intra_doc_links)]

use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[cfg(feature = "use-std")]
pub mod deserialize;

pub mod q15;
pub mod q15xl;

/// UniformSamples: a set of up to `MAX_N_CHANNELS` samples for `N_CH` channels
///
/// The samples are assumed to be taken at a uniform sampling frequency `fs`.
///
/// Note: the actual serialized format in the topic data is slightly different.
/// See [from_topic_data](method@Self::from_topic_data) / [as_topic_data](method@Self::as_topic_data)
#[serde_as]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UniformSamples<const N_CH: usize, const MAX_N_SAMPLES: usize = 100> {
    /// Timestamp of the first sample in the set (microseconds)
    pub t: i64,

    /// Sample frequency used in this set (Hz)
    pub fs: f32,

    #[serde_as(as = "[_; N_CH]")]
    pub ch: [heapless::Vec<f32, MAX_N_SAMPLES>; N_CH],
}

impl<const N_CH: usize, const MAX_N_SAMPLES: usize> UniformSamples<N_CH, MAX_N_SAMPLES> {
    pub const N_CH: usize = N_CH;
    pub const MAX_N_SAMPLES: usize = MAX_N_SAMPLES;

    /// Create an empty set of samples.
    ///
    /// The samples stay marked as empty untill the timestamp is set via [clear](method@Self::clear).
    /// See [empty_at](method@Self::empty_at) to create an empty set which is immediately ready to accept samples
    pub fn empty(sampling_frequency: f32) -> Self {
        Self::empty_at(0, sampling_frequency)
    }

    pub fn empty_at(t_start: i64, sampling_frequency: f32) -> Self {
        Self {
            t: t_start,
            fs: sampling_frequency,
            ch: [const { heapless::Vec::<f32, MAX_N_SAMPLES>::new() }; N_CH],
        }
    }

    /// Length of the UniformSamples: how many samples are available for each channel
    pub fn len(&self) -> usize {
        if self.t == 0 {
            return 0;
        }
        let min_len = self
            .ch
            .iter()
            .fold(MAX_N_SAMPLES, |acc, vec| acc.min(vec.len()));

        min_len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.t == 0
    }

    /// How many samples can be accepted by all channels
    pub fn free_space(&self) -> usize {
        let max_len = self.ch.iter().fold(0, |acc, vec| acc.max(vec.len()));

        MAX_N_SAMPLES - max_len
    }

    /// Clear the sample buffer and set the timestamp of first sample
    pub fn clear(&mut self, t_start: i64) {
        for ch in &mut self.ch {
            ch.clear()
        }
        self.t = t_start
    }

    /// Verify that the data is valid
    ///
    /// Data is invalid if it contains NaN or if timestamp/fs is zero
    pub fn verify(&self) -> bool {
        if self.t <= 0 {
            return false;
        }
        if self.fs <= 0.0 {
            return false;
        }

        #[inline]
        fn invalid(f: &f32) -> bool {
            f.is_nan()
        }

        for ch in &self.ch {
            if ch.iter().any(invalid) {
                return false;
            }
        }

        true
    }

    /// Deserialize from topic data
    #[allow(dead_code)]
    #[cfg(feature = "use-std")]
    pub fn from_topic_data(bytes: &[u8]) -> Result<Self, ParseError> {
        // This is essentially what postcard::from_bytes(bytes) does but via the SerdeHelper struct.
        let mut deserializer = postcard::Deserializer::from_bytes(bytes);
        let samples: Self = Self::deserialize(&mut deserializer)
            .map_err(|err| ParseError::Deserialize(err.to_string()))?;

        if samples.verify() {
            Ok(samples)
        } else {
            Err(ParseError::Invalid)
        }
    }

    /// Serialize to topic data
    pub fn as_topic_data<'buffer>(
        &self,
        bytes: &'buffer mut [u8],
    ) -> postcard::Result<&'buffer [u8]> {
        postcard::to_slice(&self, bytes).map(|slice| &*slice)
    }
}

#[cfg(feature = "use-std")]
#[derive(Debug, Clone)]
pub enum ParseError {
    Deserialize(String),
    Invalid,
}

#[cfg(feature = "use-std")]
mod std {
    use super::*;
    #[derive(Debug, Clone, PartialEq)]
    pub struct NChannelSamples<const N_CH: usize> {
        /// Timestamps (microseconds)
        pub t: Vec<i64>,

        /// Sampling frequency (Hz)
        pub fs: f32,

        /// Channel data
        pub ch: [Vec<f32>; N_CH],
    }

    impl<const N_CH: usize, const LEN: usize> From<UniformSamples<N_CH, LEN>>
        for NChannelSamples<N_CH>
    {
        fn from(value: UniformSamples<N_CH, LEN>) -> Self {
            // Calculate timestamps based on first timestamp and sampling frequency
            let sample_count = value.len();
            let sample_interval_us = (1_000_000.0 / value.fs) as i64;
            let t = (0..sample_count)
                .map(|i| value.t + i as i64 * sample_interval_us)
                .collect();

            // Convert channel data
            let ch = core::array::from_fn(|i| value.ch[i].iter().copied().collect());

            Self {
                t,
                fs: value.fs,
                ch,
            }
        }
    }
}

#[cfg(feature = "use-std")]
pub use std::NChannelSamples;

#[cfg(all(test, feature = "use-std"))]
mod tests {

    use super::*;

    #[test]
    fn serde() {
        let samples = UniformSamples::<3>::empty_at(1_000_000, 1000.0);

        // frogwatchlink-specific ser/de
        {
            let mut buffer = [0; 2000];
            let serialized = samples.as_topic_data(&mut buffer).unwrap();
            assert_eq!(serialized.len(), 10);
            let restored: UniformSamples<3> =
                UniformSamples::<3>::from_topic_data(serialized).unwrap();
            assert_eq!(restored.fs, 1000.0);
        }

        // direct use of ser/de
        {
            let mut buffer = [0; 2000];
            let serialized = postcard::to_slice(&samples, &mut buffer).unwrap();
            let restored: UniformSamples<3> = postcard::from_bytes(&serialized).unwrap();
            assert_eq!(restored.fs, 1000.0);
            assert_eq!(serialized.len(), 10);
        }
    }

    #[cfg(feature = "use-std")]
    mod nchannel_samples_tests {
        use super::*;

        #[test]
        fn from_uniform_samples() {
            // Create test data
            const N_CH: usize = 2;
            const MAX_SAMPLES: usize = 10;
            let start_time = 1_000_000;
            let fs = 1000.0; // 1kHz
            let mut samples = UniformSamples::<N_CH, MAX_SAMPLES>::empty_at(start_time, fs);

            // Add some data to each channel
            let _ = samples.ch[0].extend_from_slice(&[1.0, 2.0, 3.0]);
            let _ = samples.ch[1].extend_from_slice(&[4.0, 5.0, 6.0]);

            // Convert to NChannelSamples
            let nchannel = NChannelSamples::from(samples);

            // Verify conversion
            assert_eq!(nchannel.fs, fs);
            assert_eq!(nchannel.t.len(), 3); // 3 samples

            // Verify timestamps are calculated correctly
            let expected_interval_us = (1_000_000.0 / fs) as i64; // 1000 microseconds
            assert_eq!(nchannel.t[0], start_time);
            assert_eq!(nchannel.t[1], start_time + expected_interval_us);
            assert_eq!(nchannel.t[2], start_time + 2 * expected_interval_us);

            // Verify channel data
            assert_eq!(nchannel.ch[0], vec![1.0, 2.0, 3.0]);
            assert_eq!(nchannel.ch[1], vec![4.0, 5.0, 6.0]);
        }
    }
}

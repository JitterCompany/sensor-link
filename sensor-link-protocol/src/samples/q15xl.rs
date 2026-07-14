use serde::Deserialize;

use crate::MAX_MESSAGE_LEN;

use super::UniformSamples;

#[cfg(feature = "use-std")]
use super::ParseError;

pub type UniformQ15<const N_CH: usize, const MAX_N_SAMPLES: usize = 100> =
    Uniform<Q15XL<N_CH, MAX_N_SAMPLES>>;

pub const Q15_BYTES_PER_SAMPLE: usize = I16::BYTES_PER_SAMPLE;
pub const Q15_OVERHEAD_PER_PACKET: usize = 2; // u16: num_samples
pub const Q15_OVERHEAD_PER_CHANNEL: usize = 1; // i8: exponent
pub const UNIFORM_HEADER_OVERHEAD: usize = 8 + 4; // i64: timestamp + f32: sample frequency

/// Uniform: a set of up to `MAX_N_CHANNELS` samples for `N_CH` channels
///
/// The samples are assumed to be taken at a uniform sampling frequency `fs`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Uniform<D> {
    /// Timestamp of the first sample in the set (microseconds)
    #[serde(with = "postcard::fixint::le")]
    pub t: i64,

    /// Sample frequency used in this set (Hz)
    pub fs: f32,

    /// Sample data
    pub data: D,
}

/// This is a space-optimized variant of `Uniformsamples` which stores the data internally
/// as 16-bit fixed-point with a shared scale factor for each channel.
#[derive(Debug, Clone, PartialEq)]
pub struct Q15XL<const N_CH: usize, const MAX_N_SAMPLES: usize> {
    /// Number of valid samples in packet
    num_samples: u16,

    /// Multiple channels of Q15XL-formatted data
    /// (each containing `num_samples` samples)
    pub(crate) ch: [Q15Channel<MAX_N_SAMPLES>; N_CH],
}

impl<const N_CH: usize, const MAX_N_SAMPLES: usize> Q15XL<N_CH, MAX_N_SAMPLES> {
    pub const MAX_SERIALIZED_SIZE: usize = Self::_serialized_size(MAX_N_SAMPLES);

    pub const BYTES_PER_SAMPLE: usize = I16::BYTES_PER_SAMPLE;

    #[inline]
    const fn _serialized_size(n_samples: usize) -> usize {
        if N_CH == 0 {
            0
        } else {
            // compile-time assert: q15xl::Q15 only works for MAX_N_SAMPLES <= 65535
            assert!(n_samples <= u16::MAX as usize);
            if n_samples == 0 {
                0
            } else {
                Q15_OVERHEAD_PER_PACKET + N_CH * Self::_serialized_size_per_channel(n_samples)
            }
        }
    }

    #[inline]
    const fn _serialized_size_per_channel(n_samples: usize) -> usize {
        if n_samples == 0 {
            0
        } else {
            // compile-time assert: q15xl::Q15 only works for MAX_N_SAMPLES <= 65535
            assert!(n_samples <= u16::MAX as usize);
            Q15_OVERHEAD_PER_CHANNEL // i8: exponent
            + Self::BYTES_PER_SAMPLE * n_samples // i16 samples
        }
    }

    #[inline]
    pub fn serialized_size(&self) -> usize {
        Self::_serialized_size(self.num_samples as usize)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.num_samples as usize
    }

    /// Serialize to bytes
    pub fn to_bytes<'buffer>(
        &self,
        bytes: &'buffer mut [u8],
    ) -> Result<&'buffer [u8], SerializeError> {
        let output_len: usize = self.serialized_size();
        if bytes.len() < output_len {
            return Err(SerializeError::BufferFull(output_len));
        }

        // zero-sized result
        if self.num_samples == 0 || N_CH == 0 {
            return Ok(&[]);
        }

        let output = &mut bytes[..output_len];
        // 2-byte length field
        output[0..2].copy_from_slice(&self.num_samples.to_le_bytes());
        let output: &mut [u8] = &mut output[2..];

        let n_samples: usize = self.num_samples.into();
        let bytes_per_ch = Self::_serialized_size_per_channel(n_samples);

        // Copy data for each channel
        for ch in 0..N_CH {
            let ch_dst = &mut output[ch * bytes_per_ch..(ch + 1) * bytes_per_ch];

            // 1-byte exponent
            let (ch_exp, ch_dst) = ch_dst.split_at_mut(1);
            ch_exp.copy_from_slice(&self.ch[ch].exponent.to_le_bytes());

            let src = &self.ch[ch].values;
            // 2 bytes per sample
            for spl in 0..n_samples {
                ch_dst[2 * spl..2 * (spl + 1)].copy_from_slice(&src[spl].to_bytes());
            }
        }

        Ok(output)
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DeserializeError> {
        let mut result = Q15XL {
            num_samples: 0,
            ch: [Q15Channel::EMPTY; N_CH],
        };

        let num_samples_u16 = u16::from_le_bytes([bytes[0], bytes[1]]);
        let num_samples = num_samples_u16 as usize;

        // Check if num_samples exceeds the maximum allowed samples
        if num_samples > MAX_N_SAMPLES {
            return Err(DeserializeError::TooManySamples(num_samples, MAX_N_SAMPLES));
        }

        if num_samples > 0 {
            let expected_bytes: usize = Self::_serialized_size(num_samples);
            if bytes.len() < expected_bytes {
                return Err(DeserializeError::BufferUnexpectedEnd(expected_bytes));
            }

            // skip 'num_samples' byte
            let bytes = &bytes[2..];

            let bytes_per_ch = Self::_serialized_size_per_channel(num_samples);

            // Copy data for each channel
            for ch in 0..N_CH {
                let (ch_exp, ch_src) = bytes[ch * bytes_per_ch..(ch + 1) * bytes_per_ch]
                    .split_at(Q15_OVERHEAD_PER_CHANNEL);

                // 1-byte exponent
                result.ch[ch].exponent = i8::from_le_bytes([ch_exp[0]]);

                let dst = &mut result.ch[ch].values;
                for spl in 0..num_samples {
                    dst[spl] = I16::from_bytes([ch_src[2 * spl], ch_src[2 * spl + 1]]);
                }
            }

            result.num_samples = num_samples_u16;
        }
        Ok(result)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SerializeError {
    /// Larger buffer required (argument = minimum buffer size)
    BufferFull(usize),
}

pub enum DeserializeError {
    /// Larger input buffer expected (argument = expected amount of bytes)
    BufferUnexpectedEnd(usize),
    /// Number of samples exceeds maximum allowed (actual, max_allowed)
    TooManySamples(usize, usize),
}
#[cfg(feature = "use-std")]
impl core::fmt::Display for DeserializeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeserializeError::BufferUnexpectedEnd(expected) => {
                write!(f, "Buffer too small (expected {} bytes)", expected)
            }
            DeserializeError::TooManySamples(actual, max_allowed) => {
                write!(f, "Too many samples ({} > {})", actual, max_allowed)
            }
        }
    }
}

impl<const N_CH: usize, const MAX_N_SAMPLES: usize> Uniform<Q15XL<N_CH, MAX_N_SAMPLES>> {
    pub const MAX_SERIALIZED_SIZE: usize = Self::_serialized_size(MAX_N_SAMPLES);

    /// Calculate the maximum number of samples that can be serialized using q15 encoding when using
    /// Uniform<Q15XL<N_CH, _>>
    /// Usage:  Uniform::<Q15XL::<N_CH, 0>>::max_num_input_samples(available_buffer_size)
    pub const fn max_num_input_samples(buffer_size: usize) -> usize {
        let required_overhead = UNIFORM_HEADER_OVERHEAD + Q15_OVERHEAD_PER_PACKET;
        if buffer_size < required_overhead || N_CH == 0 {
            return 0;
        }

        let available_bytes = buffer_size - required_overhead;
        let required_per_channel = Q15_OVERHEAD_PER_CHANNEL + Q15_BYTES_PER_SAMPLE;
        if available_bytes < N_CH * required_per_channel {
            return 0;
        }

        let available_bytes_per_channel = available_bytes / N_CH;
        (available_bytes_per_channel - Q15_OVERHEAD_PER_CHANNEL) / Q15_BYTES_PER_SAMPLE
    }

    #[inline]
    const fn _serialized_size(n_samples: usize) -> usize {
        let size =
            UNIFORM_HEADER_OVERHEAD + Q15XL::<N_CH, MAX_N_SAMPLES>::_serialized_size(n_samples);

        assert!(size <= MAX_MESSAGE_LEN);
        size
    }

    #[inline]
    pub fn serialized_size(&self) -> usize {
        Self::_serialized_size(self.data.num_samples.into())
    }

    /// Compress UniformSamples into UniformQ15
    pub fn from_uniform(input: &UniformSamples<N_CH, MAX_N_SAMPLES>) -> Self {
        let n_samples: usize = input.len();

        let mut result = Self {
            fs: input.fs,
            t: input.t,

            data: Q15XL {
                num_samples: n_samples as u16,
                ch: [Q15Channel::EMPTY; N_CH],
            },
        };
        for (ch_idx, ch) in input.ch.iter().enumerate() {
            let samples = &ch[..n_samples];

            // 1. Find maximum scale factor
            let max = abs_max_finite(0.0, samples);

            // 2. Find next power of two for scale factor
            let exponent = find_max_exponent(max);

            // 3. Scale data with 2**exponent
            result.data.ch[ch_idx] = Q15Channel::from_slice(exponent, samples);
        }
        result
    }

    /// Decompress UniformQ15 into UniformSamples
    pub fn as_uniform(&self) -> UniformSamples<N_CH, MAX_N_SAMPLES> {
        let mut result = UniformSamples::empty_at(self.t, self.fs);

        let n_samples = self.data.num_samples as usize;
        assert!(n_samples <= MAX_N_SAMPLES);
        for (ch_i, ch) in self.data.ch.iter().enumerate() {
            let res_ch = &mut result.ch[ch_i];
            for sample_idx in 0..n_samples {
                res_ch.push(ch.value_at(sample_idx)).unwrap();
            }
        }

        result
    }

    /// Deserialize from topic data
    #[cfg(feature = "use-std")]
    #[allow(dead_code)]
    pub fn from_topic_data(bytes: &[u8]) -> Result<Self, ParseError> {
        let min_len: usize = Self::_serialized_size(0);
        if bytes.len() < min_len {
            return Err(ParseError::Deserialize(String::from("Buffer too small")));
        }

        let result = Self {
            // 8-byte timestamp: i64
            t: i64::from_le_bytes(bytes[0..8].try_into().map_err(
                |err: core::array::TryFromSliceError| ParseError::Deserialize(err.to_string()),
            )?),

            // 4-byte fs: f32
            fs: f32::from_le_bytes(bytes[8..12].try_into().map_err(
                |err: core::array::TryFromSliceError| ParseError::Deserialize(err.to_string()),
            )?),

            // X-byte Q15XL data
            data: Q15XL::from_bytes(&bytes[12..])
                .map_err(|err| ParseError::Deserialize(err.to_string()))?,
        };

        if result.verify() {
            Ok(result)
        } else {
            Err(ParseError::Invalid)
        }
    }

    /// Serialize to topic data
    pub fn as_topic_data<'buffer>(
        &self,
        bytes: &'buffer mut [u8],
    ) -> Result<&'buffer [u8], SerializeError> {
        let output_len: usize = self.serialized_size();
        if bytes.len() < output_len {
            return Err(SerializeError::BufferFull(output_len));
        }

        let output = &mut bytes[..output_len];

        // 8-byte timestamp: i64
        output[0..8].copy_from_slice(&self.t.to_le_bytes());

        // 4-byte fs: f32
        output[8..12].copy_from_slice(&self.fs.to_le_bytes());

        // X-byte Q15XL data
        self.data.to_bytes(&mut output[12..])?;

        Ok(output)
    }

    /// Verify that the data is valid
    ///
    /// Data is invalid if timestamp/fs is zero
    pub fn verify(&self) -> bool {
        if self.t <= 0 {
            return false;
        }
        if self.fs <= 0.0 || self.fs.is_nan() {
            return false;
        }

        true
    }
}

/// Minimum exponent to limit the maximum inverse Q15XL scale factor just below f32::MAX (i16::MAX / 2**EXPONENT_MIN < f32::MAX)
const EXPONENT_MIN: i8 = i8::MIN + 15;

/// Maximum exponent to limit the Q15XL scale factor just below f32::MAX (i16::MAX * 2**EXPONENT_MAX < f32::MAX)
const EXPONENT_MAX: i8 = i8::MAX - 15;

#[derive(Debug, Clone, PartialEq)]
pub struct Q15Channel<const MAX_N_SAMPLES: usize = 100> {
    /// Exponent: all values must be scaled by 2^exponent
    exponent: i8,

    /// Raw values: -32_767..=32_767 (-32_768 = NaN).
    values: [I16; MAX_N_SAMPLES],
}

impl<const MAX_N_SAMPLES: usize> Q15Channel<MAX_N_SAMPLES> {
    pub const EMPTY: Self = Self::empty();

    pub const fn empty() -> Self {
        // compile-time assert: q15xl::Q15Channel only works for MAX_N_SAMPLES <= 65535
        assert!(MAX_N_SAMPLES <= u16::MAX as usize);

        Self {
            exponent: 0,
            values: [I16::ZERO; MAX_N_SAMPLES],
        }
    }

    /// Create channel from slice. Panics if slice is longer than MAX_N_SAMPLES
    pub fn from_slice(exponent: i8, slice: &[f32]) -> Self {
        assert!(slice.len() <= MAX_N_SAMPLES);

        let mut result = Self::empty();
        result.exponent = exponent;

        let inv_scale = i16::MAX as f32 / libm::powf(2.0, exponent.into());
        for (spl_idx, sample) in slice.iter().enumerate() {
            // scale value. NaN / Inf are represented as i16::MIN
            let value = libm::roundf(sample * inv_scale);

            result.values[spl_idx] = I16::from_f32(value);
        }
        result
    }

    #[inline]
    /// Get value at index `idx`. Panics if idx out of bounds
    ///
    /// This has some overhead to calculate the scale factor.
    /// See also [values()](method@Self::values)
    pub fn value_at(&self, idx: usize) -> f32 {
        self.values[idx].as_f32() * self.scale_factor()
    }

    /// Get iterator over all values
    ///
    /// Internal scale-factor is only calculated once for all samples.
    pub fn values(&self) -> impl Iterator<Item = f32> + '_ {
        let scale = self.scale_factor();
        let scaled_iter = self.values.iter().map(move |v| v.as_f32() * scale);
        scaled_iter
    }

    #[inline]
    fn scale_factor(&self) -> f32 {
        libm::powf(2.0, self.exponent.into()) / (i16::MAX as f32)
    }
}

// hide internal raw value to prevent accidental direct access
mod fixedint {
    #[derive(Debug, Copy, Clone, Default, PartialEq)]
    pub struct I16 {
        /// 16-bit value with NaN:
        /// - range is clamped to -32767..=32767
        /// - NaN (or +/- infinity) is represented as -32768
        value: i16,
    }

    impl I16 {
        pub const ZERO: Self = Self { value: 0 };
        pub const BYTES_PER_SAMPLE: usize = 2;

        #[inline]
        pub(super) fn from_bytes(bytes: [u8; Self::BYTES_PER_SAMPLE]) -> Self {
            Self {
                value: i16::from_le_bytes(bytes),
            }
        }

        #[inline]
        pub(super) fn to_bytes(self) -> [u8; Self::BYTES_PER_SAMPLE] {
            self.value.to_le_bytes()
        }

        /// Encode a float into internal 16-bit representation
        pub fn from_f32(value: f32) -> Self {
            let value = if value.is_finite() {
                let value: i16 = value as i16;
                if value == i16::MIN {
                    value + 1
                } else {
                    value
                }
            } else {
                i16::MIN
            };

            Self { value }
        }

        /// Decode internal representation as float
        pub fn as_f32(&self) -> f32 {
            if self.value == i16::MIN {
                f32::NAN
            } else {
                self.value as f32
            }
        }
    }
}
use fixedint::I16;

impl<const N_CH: usize, const MAX_N_SAMPLES: usize> From<Uniform<Q15XL<N_CH, MAX_N_SAMPLES>>>
    for UniformSamples<N_CH, MAX_N_SAMPLES>
{
    fn from(q15: Uniform<Q15XL<N_CH, MAX_N_SAMPLES>>) -> Self {
        q15.as_uniform()
    }
}
impl<const N_CH: usize, const MAX_N_SAMPLES: usize> From<&Uniform<Q15XL<N_CH, MAX_N_SAMPLES>>>
    for UniformSamples<N_CH, MAX_N_SAMPLES>
{
    fn from(q15: &Uniform<Q15XL<N_CH, MAX_N_SAMPLES>>) -> Self {
        q15.as_uniform()
    }
}

impl<const N_CH: usize, const MAX_N_SAMPLES: usize> From<UniformSamples<N_CH, MAX_N_SAMPLES>>
    for Uniform<Q15XL<N_CH, MAX_N_SAMPLES>>
{
    fn from(input: UniformSamples<N_CH, MAX_N_SAMPLES>) -> Self {
        Self::from_uniform(&input)
    }
}

impl<const N_CH: usize, const MAX_N_SAMPLES: usize> From<&UniformSamples<N_CH, MAX_N_SAMPLES>>
    for Uniform<Q15XL<N_CH, MAX_N_SAMPLES>>
{
    fn from(input: &UniformSamples<N_CH, MAX_N_SAMPLES>) -> Self {
        Self::from_uniform(input)
    }
}

/// calculate absolute maximum value of slice
///
/// non-finite (+/-inf, NaN) values are ignored.
/// Returns `default` if slice does not contain any finite values
fn abs_max_finite(default: f32, values: &[f32]) -> f32 {
    {
        let mut max: f32 = default;
        for value in values {
            if value.is_finite() {
                max = libm::fabsf(*value).max(max);
            }
        }
        max
    }
}

/// Find the smallest possible exponent where input < 2^N, truncated to Q15XL-compatible limits
fn find_max_exponent(input: f32) -> i8 {
    let log: f32 = libm::log2f(libm::fabsf(input));

    let exponent = libm::ceilf(log) as i8;

    // limit exponent:
    // 1. to small exponent would mean the quantization step would be smaller than f32::MIN.
    //    This causes numerical issues: the inverse scale factor (1/step) will be +infinity
    //    which means values close to 0.0 are tranformed into NaN
    // 2. Similarly, with excessively large exponent the full range would exceed f32::MAX,
    //      resulting in a scale factor of +Infinity.
    exponent.clamp(EXPONENT_MIN, EXPONENT_MAX)
}

#[cfg(all(test, feature = "use-std"))]
mod tests {
    use super::*;
    use crate::MAX_MESSAGE_LEN;
    use assert_float_eq::{assert_float_absolute_eq, assert_float_relative_eq};
    use core::f32;

    // =============================================================================
    // Unit Tests - Testing individual functions and core Q15XL functionality
    // =============================================================================

    // max error: slightly above 2**-15
    // accounts for:
    // - quantization error (up to 1/2 LSB of 2**-15 -> 2**-16
    // - worst-case scale factor (0.5) which can increase quantization error to 1LSB = 2**-15
    // - small rounding errors in floating-point (assumed to be < 2e-7)
    const REL_ERR_MAX: f32 = 0.0000308;

    #[test]
    fn test_find_exponent() {
        assert_eq!(find_max_exponent(0.0), EXPONENT_MIN);
        assert_eq!(find_max_exponent(f32::INFINITY), EXPONENT_MAX);
        assert_eq!(find_max_exponent(f32::NEG_INFINITY), EXPONENT_MAX);
        assert_eq!(find_max_exponent(f32::NAN), 0); // (arbitrary?)

        assert_eq!(find_max_exponent(3.1), 2);
        assert_eq!(find_max_exponent(0.1), -3);
    }

    #[test]
    fn from_uniform() {
        let mut samples = UniformSamples::<2>::empty(1000.0);
        samples.t = 1_000_000_000;
        samples.ch[0].push(3.14).unwrap();
        samples.ch[0].push(0.314).unwrap();
        samples.ch[1].push(0.628).unwrap();
        samples.ch[1].push(62.8).unwrap();

        let q15: Uniform<Q15XL<2, 100>> = samples.into();

        // error bounds are dependent on largest absolute value in the channel
        const CH0_MAX_ERR: f32 = 3.14 * REL_ERR_MAX;
        const CH1_MAX_ERR: f32 = 62.8 * REL_ERR_MAX;

        assert_float_absolute_eq!(3.14, q15.data.ch[0].value_at(0), CH0_MAX_ERR);
        assert_float_absolute_eq!(0.314, q15.data.ch[0].value_at(1), CH0_MAX_ERR);

        assert_float_absolute_eq!(0.628, q15.data.ch[1].value_at(0), CH1_MAX_ERR);
        assert_float_absolute_eq!(62.8, q15.data.ch[1].value_at(1), CH1_MAX_ERR);
    }

    #[test]
    fn small_values() {
        let mut samples = UniformSamples::<2>::empty(1000.0);
        samples.t = 1_000_000_000;
        samples.ch[0].push(0.314).unwrap();
        samples.ch[0].push(0.0314).unwrap();
        samples.ch[1].push(3.14e-9).unwrap();
        samples.ch[1].push(6.28e-9).unwrap();

        let q15: Uniform<Q15XL<2, 100>> = samples.into();

        // error bounds are dependent on largest absolute value in the channel
        const CH0_MAX_ERR: f32 = 0.314 * REL_ERR_MAX;
        const CH1_MAX_ERR: f32 = 6.28e-9 * REL_ERR_MAX;

        assert_float_relative_eq!(0.314, q15.data.ch[0].value_at(0), REL_ERR_MAX);
        assert_float_absolute_eq!(0.314, q15.data.ch[0].value_at(0), CH0_MAX_ERR);
        assert_float_absolute_eq!(0.0314, q15.data.ch[0].value_at(1), CH0_MAX_ERR);

        assert_float_absolute_eq!(3.14e-9, q15.data.ch[1].value_at(0), CH1_MAX_ERR);
        assert_float_absolute_eq!(6.28e-9, q15.data.ch[1].value_at(1), CH1_MAX_ERR);
    }

    #[test]
    fn extreme_values() {
        let mut samples = UniformSamples::<1>::empty(1000.0);
        samples.t = 1_000_000_000;
        samples.ch[0].push(3.14).unwrap();

        // these will all be represented as NaN (i16::MIN internally)
        samples.ch[0].push(f32::NAN).unwrap();
        samples.ch[0].push(f32::INFINITY).unwrap();
        samples.ch[0].push(f32::NEG_INFINITY).unwrap();

        let q15: Uniform<Q15XL<1, 100>> = samples.into();

        // the only finite value in the channel: should be represented accurately
        assert_float_relative_eq!(3.14, q15.data.ch[0].value_at(0), REL_ERR_MAX);

        // non-finite values: expect NaN
        assert!(q15.data.ch[0].value_at(1).is_nan());
        assert!(q15.data.ch[0].value_at(2).is_nan());
        assert!(q15.data.ch[0].value_at(3).is_nan());

        assert_eq!(2, q15.data.ch[0].exponent);
    }

    #[test]
    fn zero() {
        let mut samples = UniformSamples::<1>::empty(1000.0);
        samples.t = 1_000_000_000;
        samples.ch[0].push(0.0).unwrap();

        let q15: Uniform<Q15XL<1, 100>> = samples.into();

        // expect zero to be represented as exactly zero
        assert_eq!(0.0, q15.data.ch[0].value_at(0));
    }

    /// verify the maximum number of samples that can be stored in a Q15XL buffer is indeed 65535
    #[test]
    fn max_num_input_samples() {
        let mut samples = UniformSamples::<1, 65535>::empty(1000.0);
        assert_eq!(0, samples.len());
        assert_eq!(1, samples.ch.len());
        assert_eq!(65535, samples.ch[0].capacity());
        samples.t = 1_000_000_000;
        for i in 0..65535 {
            samples.ch[0].push(i as f32).unwrap();
        }

        let q15: Uniform<Q15XL<1, 65535>> = samples.into();

        // expect all values to be represented witin 1 LSB
        for i in 0..65535 {
            assert_float_absolute_eq!(i as f32, q15.data.ch[0].value_at(i), 1.0);
        }
    }

    #[test]
    fn serde_size() {
        //zero-sized types
        assert_eq!(0, Q15XL::<0, 0>::MAX_SERIALIZED_SIZE);
        assert_eq!(0, Q15XL::<0, 1>::MAX_SERIALIZED_SIZE);
        assert_eq!(0, Q15XL::<1, 0>::MAX_SERIALIZED_SIZE);

        // 1 channel: 2 + 1 + 2N bytes (2-byte length field for Q15XL)
        assert_eq!(5, Q15XL::<1, 1>::MAX_SERIALIZED_SIZE);
        assert_eq!(15, Q15XL::<1, 6>::MAX_SERIALIZED_SIZE);

        // 3 channel: 2 + 3 * (1 + 2N) bytes (2-byte length field for Q15XL)
        assert_eq!(11, Q15XL::<3, 1>::MAX_SERIALIZED_SIZE);
        assert_eq!(41, Q15XL::<3, 6>::MAX_SERIALIZED_SIZE);
    }

    // =============================================================================
    // Serialization Tests - Testing data format stability and roundtrip behavior
    // =============================================================================

    /// Test the serialization against a hardcoded reference
    ///
    /// this test intends to guard against breaking changes in the serialization format is stable
    #[test]
    fn serialize_format_stable() {
        const N_SPL: usize = 2;
        let mut samples = UniformSamples::<3, N_SPL>::empty_at(1_000_000, 1000.0);
        for i in 0..N_SPL {
            samples.ch[0].push(i as f32 * 0.314).unwrap();
            samples.ch[1].push(i as f32 * 0.1).unwrap();
            samples.ch[2].push(i as f32 * 1.0).unwrap();
        }

        let samples_q15: Uniform<Q15XL<3, N_SPL>> = samples.into();

        // frogwatchlink-specific ser/de
        const SER_LEN: usize = Uniform::<Q15XL<3, N_SPL>>::MAX_SERIALIZED_SIZE;
        assert!(SER_LEN < MAX_MESSAGE_LEN);

        let mut buffer = [0u8; MAX_MESSAGE_LEN];
        let serialized = samples_q15.as_topic_data(&mut buffer).unwrap();

        // serialization is fixed length, so should exactly match MAX_SERIALIZED_SIZE
        assert_eq!(SER_LEN, serialized.len());

        // 1_000_000_i64 as 8 LE bytes
        // 1_000.0_f32 as 4 LE bytes
        // 2-byte length field (u16 for Q15XL)
        // 3x [1-byte exponent + 2-byte 0_i16]
        assert_eq!(
            &[
                64,
                66,
                15,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                122,
                68,
                N_SPL.try_into().unwrap(),
                0,    // 2-byte length field: N_SPL as u16 in little-endian
                0xFF, // e=-1 -> max = 2**-1 = 0.5
                0,
                0,
                0x62, //0x5062/i16::MAX * 0.5 == 0.314
                0x50,
                0xFD, // e=-3 -> max = 2**-3 = 0.125
                0,
                0,
                0x66, //0x6666/i16::MAX * 0.125 == 0.1
                0x66,
                0x00, // e=0 -> max = 2**0 = 1.0
                0,
                0,
                0xFF, //0x7FFF/i16::MAX * 1.0 == 1.0
                0x7F,
            ],
            serialized
        );
    }

    #[test]
    fn serde() {
        const N_SPL: usize = 246;
        let mut samples = UniformSamples::<3, N_SPL>::empty_at(1_000_000, 1000.0);
        for i in 0..N_SPL {
            samples.ch[0].push(i as f32 * 0.314).unwrap();
            samples.ch[1].push(i as f32 * 0.1).unwrap();
            samples.ch[2].push(i as f32 * 1.0).unwrap();
        }

        let samples_original = samples.clone();
        let samples_q15: Uniform<Q15XL<3, N_SPL>> = samples.into();

        // frogwatchlink-specific ser/de
        const SER_LEN: usize = Uniform::<Q15XL<3, N_SPL>>::MAX_SERIALIZED_SIZE;
        assert!(SER_LEN < MAX_MESSAGE_LEN);

        let mut buffer = [0u8; MAX_MESSAGE_LEN];
        let serialized = samples_q15.as_topic_data(&mut buffer).unwrap();

        // serialization is fixed length, so should exactly match MAX_SERIALIZED_SIZE
        assert_eq!(SER_LEN, serialized.len());

        let restored_q15: Uniform<Q15XL<3, N_SPL>> = Uniform::from_topic_data(serialized).unwrap();
        assert_eq!(restored_q15, samples_q15);

        let restored: UniformSamples<3, N_SPL> = restored_q15.into();
        assert_eq!(1000.0, restored.fs);
        assert_eq!(N_SPL, restored.len());

        // NB: cannot assert_eq as the compression is lossy, so check
        // if the restored samples are close to the originals
        for ch in 0..3 {
            let orig = &samples_original.ch[ch];
            let res = &restored.ch[ch];
            for i in 0..N_SPL {
                assert_float_absolute_eq!(orig[i], res[i], 0.01);
            }
        }
    }

    // =============================================================================
    // Integration Tests - Testing complex scenarios and edge cases
    // =============================================================================

    /// Test case for specific MQTT payload that causes a bug
    ///
    /// This test uses the actual hex payload from mosquitto_sub to reproduce the issue
    #[test]
    fn test_max_num_input_samples_edge_cases() {
        // Test with buffer size smaller than required overhead
        let tiny_buffer = UNIFORM_HEADER_OVERHEAD + Q15_OVERHEAD_PER_PACKET - 1;
        assert_eq!(
            0,
            Uniform::<Q15XL<1, 0>>::max_num_input_samples(tiny_buffer)
        );
        assert_eq!(
            0,
            Uniform::<Q15XL<3, 0>>::max_num_input_samples(tiny_buffer)
        );

        // Test with buffer size exactly equal to required overhead
        let exact_overhead = UNIFORM_HEADER_OVERHEAD + Q15_OVERHEAD_PER_PACKET;
        assert_eq!(
            0,
            Uniform::<Q15XL<1, 0>>::max_num_input_samples(exact_overhead)
        );
        assert_eq!(
            0,
            Uniform::<Q15XL<3, 0>>::max_num_input_samples(exact_overhead)
        );

        // Test with zero channels
        assert_eq!(0, Uniform::<Q15XL<0, 0>>::max_num_input_samples(1000));

        // Test with buffer too small for even one sample per channel
        let min_for_channels =
            exact_overhead + 3 * Q15_OVERHEAD_PER_CHANNEL + 3 * Q15_BYTES_PER_SAMPLE - 1;
        assert_eq!(
            0,
            Uniform::<Q15XL<3, 0>>::max_num_input_samples(min_for_channels)
        );

        // Test with buffer exactly enough for one sample per channel
        let exactly_one_sample =
            exact_overhead + 3 * (Q15_OVERHEAD_PER_CHANNEL + Q15_BYTES_PER_SAMPLE);
        assert_eq!(
            1,
            Uniform::<Q15XL<3, 0>>::max_num_input_samples(exactly_one_sample)
        );

        // Test normal case
        let normal_buffer = 1000;
        let expected_1ch =
            (normal_buffer - exact_overhead - Q15_OVERHEAD_PER_CHANNEL) / Q15_BYTES_PER_SAMPLE;
        let expected_3ch = ((normal_buffer - exact_overhead) / 3 - Q15_OVERHEAD_PER_CHANNEL)
            / Q15_BYTES_PER_SAMPLE;

        assert_eq!(
            expected_1ch,
            Uniform::<Q15XL<1, 0>>::max_num_input_samples(normal_buffer)
        );
        assert_eq!(
            expected_3ch,
            Uniform::<Q15XL<3, 0>>::max_num_input_samples(normal_buffer)
        );
    }
}

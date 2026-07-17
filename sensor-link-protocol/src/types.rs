#[cfg(feature = "use-std")]
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

/// Sampling frequency in Hz
pub type Fs = f32;

/// Milliseconds since unix epoch (1970-01-01)
///
/// Requires explicit conversion from/to i64 to avoid confusing
/// [Microseconds] with [Milliseconds]
#[derive(Debug, Serialize, Deserialize, Copy, Clone, PartialEq)]
#[cfg_attr(feature = "use-std", derive(Hash))]
#[serde(transparent)]
pub struct Milliseconds(pub i64);
impl Milliseconds {
    pub fn from_raw_milliseconds(ms: i64) -> Self {
        Self(ms)
    }
    pub fn from_raw_microseconds(us: i64) -> Self {
        Self(us / 1000)
    }
    pub fn microseconds(&self) -> i64 {
        self.0.saturating_mul(1000)
    }
    pub fn milliseconds(&self) -> i64 {
        self.0
    }
}

/// Microseconds since unix epoch (1970-01-01)
///
/// Requires explicit conversion from/to i64 to avoid confusing
/// [Microseconds] with [Milliseconds]
#[derive(Debug, Serialize, Deserialize, Copy, Clone, PartialEq)]
#[cfg_attr(feature = "use-std", derive(Hash))]
#[serde(transparent)]
pub struct Microseconds(pub i64);
impl Microseconds {
    pub fn from_raw_microseconds(ms: i64) -> Self {
        Self(ms)
    }
    pub fn microseconds(&self) -> i64 {
        self.0
    }
    pub fn milliseconds(&self) -> i64 {
        self.0 / 1000
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ThresholdError {
    #[error("Threshold value is not finite")]
    InvalidValue,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(transparent)]
pub struct Threshold(f32);
impl Threshold {
    pub fn new(value: f32) -> Self {
        Threshold(value)
    }

    /// Check if a given value can be used as a valid threshold
    ///
    /// False if infinite/NaN
    pub fn is_valid(value: f32) -> bool {
        value.is_finite()
    }

    /// Get raw threshold as f32
    ///
    /// Returns error if threshold is not valid
    pub fn raw_threshold(&self) -> Result<f32, ThresholdError> {
        if Self::is_valid(self.0) {
            Ok(self.0)
        } else {
            Err(ThresholdError::InvalidValue)
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum FractionError {
    #[error("Fraction value is not valid (must be finite and positive)")]
    InvalidValue,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(transparent)]
pub struct Fraction(f32);
impl Fraction {
    /// Check if a given value can be used as a valid fraction
    ///
    /// False if infinite/NaN/zero/negative
    pub fn is_valid(value: f32) -> bool {
        value.is_finite() && value > 0.0
    }

    pub fn new(fraction: f32) -> Self {
        Fraction(fraction)
    }

    /// Get raw fraction as f32
    ///
    /// Returns error if fraction is not valid
    pub fn raw_fraction(&self) -> Result<f32, FractionError> {
        if Self::is_valid(self.0) {
            Ok(self.0)
        } else {
            Err(FractionError::InvalidValue)
        }
    }
}

#[cfg(feature = "use-std")]
impl Hash for Threshold {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_string().hash(state);
    }
}
#[cfg(feature = "use-std")]
impl Hash for Fraction {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_string().hash(state);
    }
}

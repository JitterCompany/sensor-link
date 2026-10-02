//! Simulated sensor signals
//!

use std::fmt::Debug;

/// Trait for signal generators that can produce N samples for 3 axes
pub trait Signal: Send + Debug {
    /// Compute the signal value for a given time
    ///
    /// # Arguments
    /// * `time` - Current time in seconds
    ///
    /// # Returns
    /// * The computed signal value
    fn compute(&self, time: f64) -> f64;
}

/// Signal generator for sine waves
#[derive(Debug)]
pub struct SineSignal {
    /// Signal amplitude in mm/s²
    pub amplitude: f64,
    /// Signal frequency in Hz
    pub frequency: f64,
    /// Phase offset in radians
    pub phase: f64,
}

impl Signal for SineSignal {
    fn compute(&self, time: f64) -> f64 {
        self.amplitude * (2.0 * std::f64::consts::PI * self.frequency * time + self.phase).sin()
    }
}

/// Signal generator for adding offset to other signals
#[derive(Debug)]
pub struct Offset<S: Signal> {
    pub signal: S,
    pub offset: f64,
}

impl<S: Signal> Offset<S> {
    pub fn new(offset: f64, signal: S) -> Self {
        Self { signal, offset }
    }
}

impl<S: Signal> Signal for Offset<S> {
    fn compute(&self, time: f64) -> f64 {
        self.offset + self.signal.compute(time)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;
    use std::f64::consts::PI;

    #[test]
    fn test_sine_wave_computation() {
        let signal = SineSignal {
            amplitude: 1.0,
            frequency: 1.0,
            phase: 0.0,
        };

        // Test at key points in the sine wave
        assert_relative_eq!(signal.compute(0.0), 0.0, epsilon = 1e-6); // sin(0) = 0
        assert_relative_eq!(signal.compute(0.25), 1.0, epsilon = 1e-6); // sin(π/2) = 1
        assert_relative_eq!(signal.compute(0.5), 0.0, epsilon = 1e-6); // sin(π) = 0
        assert_relative_eq!(signal.compute(0.75), -1.0, epsilon = 1e-6); // sin(3π/2) = -1
        assert_relative_eq!(signal.compute(1.0), 0.0, epsilon = 1e-6); // sin(2π) = 0
    }

    #[test]
    fn test_sine_wave_with_phase() {
        let signal = SineSignal {
            amplitude: 1.0,
            frequency: 1.0,
            phase: PI / 2.0, // Quarter cycle phase shift
        };

        // With π/2 phase shift, sine becomes -cosine
        assert_relative_eq!(signal.compute(0.0), 1.0, epsilon = 1e-6); // sin(π/2) = 1
        assert_relative_eq!(signal.compute(0.25), 0.0, epsilon = 1e-6); // sin(π) = 0
        assert_relative_eq!(signal.compute(0.5), -1.0, epsilon = 1e-6); // sin(3π/2) = -1
        assert_relative_eq!(signal.compute(0.75), 0.0, epsilon = 1e-6); // sin(2π) = 0
    }

    #[test]
    fn test_sine_wave_with_frequency() {
        let signal = SineSignal {
            amplitude: 1.0,
            frequency: 2.0, // Double frequency
            phase: 0.0,
        };

        // Double frequency means the cycle completes in half the time
        assert_relative_eq!(signal.compute(0.0), 0.0, epsilon = 1e-6);
        assert_relative_eq!(signal.compute(0.125), 1.0, epsilon = 1e-6);
        assert_relative_eq!(signal.compute(0.25), 0.0, epsilon = 1e-6);
        assert_relative_eq!(signal.compute(0.375), -1.0, epsilon = 1e-6);
        assert_relative_eq!(signal.compute(0.5), 0.0, epsilon = 1e-6);
    }
}

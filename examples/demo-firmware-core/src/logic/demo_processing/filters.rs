//! Anti-alias filters for the demo processing pipeline

use biquad::Coefficients;

/// Lowpass filter for decimation at fs=100Hz to fs=1Hz.
/// Generated with scipy:
/// ```python
/// fs = 100
/// cutoff = 0.5
/// order=2
/// sos = signal.bessel(order, cutoff, fs=fs, output='sos')
///```
pub const LOWPASS_DECIMATE_100HZ: [Coefficients<f32>; 1] = [Coefficients {
    a1: -1.946_075,
    a2: 9.470_357_3e-1,
    b0: 2.401_861_6e-4,
    b1: 4.803_723e-4,
    b2: 2.401_861_6e-4,
}];

/// Lowpass filter for decimation at fs=10Hz to fs=1Hz.
/// Generated with scipy:
/// ```python
/// fs = 10
/// cutoff = 0.5
/// order=2
/// sos = signal.bessel(order, cutoff, fs=fs, output='sos')
///```
pub const LOWPASS_DECIMATE_10HZ: [Coefficients<f32>; 1] = [Coefficients {
    a1: -1.500_542_9,
    a2: 5.777_641_5e-1,
    b0: 1.930_531_9e-2,
    b1: 3.861_063_7e-2,
    b2: 1.930_531_9e-2,
}];

/// Lowpass filter for decimation at fs=4Hz to fs=1Hz.
/// Generated with scipy:
/// ```python
/// fs = 4
/// cutoff = 0.5
/// order=2
/// sos = signal.bessel(order, cutoff, fs=fs, output='sos')
///```
pub const LOWPASS_DECIMATE_4HZ: [Coefficients<f32>; 1] = [Coefficients {
    a1: -8.771_010_6e-1,
    a2: 2.404_082_1e-1,
    b0: 9.082_679e-2,
    b1: 1.816_535_7e-1,
    b2: 9.082_679e-2,
}];

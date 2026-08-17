//! PT100 RTD helpers (DIN EN 60751, Class A).
//! Assumes a Class A PT100 sensor and provides conversion utilities and a compile-time LUT.
use crate::{lut_from_reverse_mapping, utils::lut::Lut};

pub const fn celsius_from_resistance(r: f32) -> f32 {
    PT100_LUT.lookup(r)
}

const PT100_LUT: Lut<301> = lut_from_reverse_mapping!(301, -50.0, 1.0, resistance_from_celsius);

#[allow(dead_code)]
pub const T_MIN: f32 = PT100_LUT.min().1;
#[allow(dead_code)]
pub const T_MAX: f32 = PT100_LUT.max().1;

pub const R_MIN: f32 = PT100_LUT.min().0;
pub const R_MAX: f32 = PT100_LUT.max().0;

const _: () = assert!(T_MIN == -50.0);
const _: () = assert!(T_MAX == 250.0);

pub const fn resistance_from_celsius(t_c: f64) -> f64 {
    const A: f64 = 3.9083e-3;
    const B: f64 = -5.775e-7;
    const C: f64 = -4.183e-12;
    const R0_PT100: f64 = 100.0;

    if t_c >= 0.0 {
        R0_PT100 * (1.0 + A * t_c + B * t_c * t_c)
    } else {
        R0_PT100 * (1.0 + A * t_c + B * t_c * t_c + C * (t_c - 100.0) * t_c * t_c * t_c)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use assert_float_eq::assert_float_absolute_eq;

    #[test]
    fn test_pt100_lut() {
        // Values at table endpoints
        assert_eq!(PT100_LUT.min(), (80.30628, -50.0));
        assert_eq!(PT100_LUT.max(), (194.09813, 250.0));

        // Spot checks
        assert_float_absolute_eq!(PT100_LUT.lookup(108.9585), 23.000, 0.001);
        assert_float_absolute_eq!(PT100_LUT.lookup(109.0878), 23.333, 0.001);
        assert_float_absolute_eq!(PT100_LUT.lookup(109.3467), 24.000, 0.001);

        // Clamp behavior
        assert_float_absolute_eq!(PT100_LUT.lookup(0.0), T_MIN, 0.001);
        assert_float_absolute_eq!(PT100_LUT.lookup(2000.0), T_MAX, 0.001);
    }
}

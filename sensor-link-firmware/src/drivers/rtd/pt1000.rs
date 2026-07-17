//! PT1000 RTD helpers (DIN EN 60751, Class A).
//! Assumes a Class A PT1000 sensor and provides conversion utilities and a compile-time LUT.
use crate::{lut_from_reverse_mapping, utils::lut::Lut};

pub const fn celsius_from_resistance(r: f32) -> f32 {
    PT1000_LUT.lookup(r)
}

const PT1000_LUT: Lut<261> = lut_from_reverse_mapping!(261, -50.0, 1.0, resistance_from_celsius);

#[allow(dead_code)]
pub const T_MIN: f32 = PT1000_LUT.min().1;

#[allow(dead_code)]
pub const T_MAX: f32 = PT1000_LUT.max().1;

pub const R_MIN: f32 = PT1000_LUT.min().0;
pub const R_MAX: f32 = PT1000_LUT.max().0;

const _: () = assert!(T_MIN == -50.0);
const _: () = assert!(T_MAX == 210.0);

pub const fn resistance_from_celsius(t_c: f64) -> f64 {
    const A: f64 = 3.9083e-3;
    const B: f64 = -5.775e-7;
    const C: f64 = -4.183e-12;
    const R0_PT1000: f64 = 1000.0;

    if t_c >= 0.0 {
        R0_PT1000 * (1.0 + A * t_c + B * t_c * t_c)
    } else {
        R0_PT1000 * (1.0 + A * t_c + B * t_c * t_c + C * (t_c - 100.0) * t_c * t_c * t_c)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use assert_float_eq::assert_float_absolute_eq;

    #[test]
    fn test_pt1000_lut() {
        // Values at table endpoints
        assert_eq!(PT1000_LUT.min(), (803.0628, -50.0));
        assert_eq!(PT1000_LUT.max(), (1795.2753, 210.0));

        // Spot checks
        assert_float_absolute_eq!(PT1000_LUT.lookup(1089.585), 23.000, 0.001);
        assert_float_absolute_eq!(PT1000_LUT.lookup(1090.878), 23.333, 0.001);
        assert_float_absolute_eq!(PT1000_LUT.lookup(1093.467), 24.000, 0.001);

        // Clamp behavior
        assert_float_absolute_eq!(PT1000_LUT.lookup(0.0), T_MIN, 0.001);
        assert_float_absolute_eq!(PT1000_LUT.lookup(2000.0), T_MAX, 0.001);
    }
}

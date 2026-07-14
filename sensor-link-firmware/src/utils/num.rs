/// Ceiled integer division
///
/// Calculates `num/denom` but rounds up instead of towards zero
///
/// ## Example
/// ```rust
///  use sensor_link_firmware::utils::num::div_ceil;
///
/// let three = div_ceil(5,2); // 5/2 == 2.5, round up to 3
/// ```
pub fn div_ceil(num: u32, denom: u32) -> u32 {
    (num + denom - 1) / denom
}

pub const fn clamp_usize(value: usize, min: usize, max: usize) -> usize {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

/// Force value to be clamped to min and max.
///
/// Returns true if clamping was applied
pub fn force_clamped<T: PartialOrd>(value: &mut T, min: T, max: T) -> bool {
    if *value < min {
        *value = min;
        true
    } else if *value > max {
        *value = max;
        true
    } else {
        false
    }
}

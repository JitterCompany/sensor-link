//! Generic lookup table.
//! Stores pairs of `(key, value)` as `f32` for compactness, while allowing
//! construction and computations in `f64`.
//!
//! Assumptions:
//! - `key` is strictly monotonic increasing across the table.
//! - Linear interpolation is acceptable between adjacent points.
//!
//! This is suitable for sensor transfer functions like RTD `R(T)` where
//! the LUT stores `(R, T)` pairs.

pub struct Lut<const N: usize> {
    pub data: [(f32, f32); N], // (key, value)
}

/// Build a `Lut<N>` at compile time using a reverse mapping function
///
/// The output signal value range is defined by the given minimum value and step size,
/// sampled in `N` fixed step increments to generate the `key` values.
///
/// The reverse mapping function must be a const function that takes a `f64` value
/// and returns the `f64` value that maps to it.
///
/// Usage:
/// ```
/// use sensor_link_firmware::utils::lut::Lut;
/// use sensor_link_firmware::lut_from_reverse_mapping;
/// const N: usize = 151;
/// // Example: use `to_radians` to generate radians->degrees LUT
/// const LUT: Lut<N> = lut_from_reverse_mapping!(
///     N,
///     -50.0_f32,
///     1.0_f32,
///     f64::to_radians
/// );
/// ```
#[macro_export]
macro_rules! lut_from_reverse_mapping {
    ($n:expr, $t_min:expr, $t_step:expr, $map:path) => {{
        const fn __build<const N: usize>() -> Lut<N> {
            let mut data: [(f32, f32); N] = [(0.0, 0.0); N];
            let mut i: usize = 0;
            while i < N {
                let t = ($t_min as f64) + (i as f64) * ($t_step as f64);
                let key = $map(t);
                data[i] = (key as f32, t as f32);
                i += 1;
            }
            $crate::utils::lut::Lut { data }
        }
        __build::<{ $n }>()
    }};
}

impl<const N: usize> Lut<N> {
    /// Lookup the interpolated `value` for the given `key`.
    /// The closest entries are selected using binary search, then linear interpolation
    /// is used to compute the result.
    ///
    /// Note: no extrapolation is applied: clamps to the end points if `key` is out of range!
    /// See [Self::min] and [Self::max] for the valid range.
    pub const fn lookup(&self, key: f32) -> f32 {
        const { assert!(N > 1) }

        let last = N - 1;
        let key_min = self.data[0].0;
        let key_max = self.data[last].0;
        if key <= key_min {
            return self.data[0].1;
        }
        if key >= key_max {
            return self.data[last].1;
        }

        // binary search: find adjacent lo,hi pair such that lo < key < hi
        let mut lo: usize = 0;
        let mut hi: usize = last;
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if self.data[mid].0 < key {
                lo = mid;
            } else {
                hi = mid;
            }
        }

        let (key0, value0) = self.data[lo];
        let (key1, value1) = self.data[hi];
        value0 + (key - key0) * (value1 - value0) / (key1 - key0)
    }

    /// Minimum supported (key,value) pair (first table entry)
    ///
    /// Note: [lookup] clamps to this `value` for input < `key`.
    pub const fn min(&self) -> (f32, f32) {
        // Compile-time assert: empty table is useless
        const { assert!(N > 0) };

        self.data[0]
    }

    /// Maximum supported (key,value) pair (last table entry)
    ///
    /// Note: [lookup] clamps to this `value` for input > `key`.
    pub const fn max(&self) -> (f32, f32) {
        // Compile-time assert: empty table is useless
        const { assert!(N > 0) };

        self.data[N - 1]
    }
}

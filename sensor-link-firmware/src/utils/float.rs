pub trait EmbeddedF32: Sized {
    fn abs(self) -> f32;
    fn sqrt(self) -> f32;
    fn square(self) -> f32;
    fn round(self) -> f32;
    fn ceil(self) -> f32;
    fn atan(self) -> f32;
}

impl EmbeddedF32 for f32 {
    #[inline]
    fn abs(self) -> f32 {
        libm::fabsf(self)
    }

    #[inline]
    fn square(self) -> f32 {
        self * self
    }

    #[inline]
    fn sqrt(self) -> f32 {
        libm::sqrtf(self)
    }

    #[inline]
    fn round(self) -> f32 {
        libm::roundf(self)
    }

    #[inline]
    fn ceil(self) -> f32 {
        libm::ceilf(self)
    }
    #[inline]
    fn atan(self) -> f32 {
        libm::atanf(self)
    }
}

pub fn array_max<const N: usize>(array: &[f32; N]) -> f32 {
    match N {
        0 => f32::NAN,
        1 => array[0],
        _ => {
            let mut max = array[0];
            for value in &array[1..N] {
                let value = *value;
                if value > max {
                    max = value;
                }
            }
            max
        }
    }
}

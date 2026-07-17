//! Utilities for working with bitwise data
//!

mod transpose;

pub use transpose::Matrix8x8;

/// Enums specific for 8-bit wide bitfields
pub mod width8 {

    use num_enum::TryFromPrimitive;

    /// Represents a single bit in a 8-bit wide field
    #[repr(u8)]
    #[derive(Debug, Clone, Copy, PartialEq, TryFromPrimitive)]
    pub enum Bit {
        B0 = 0,
        B1,
        B2,
        B3,
        B4,
        B5,
        B6,
        B7,
    }
}

pub mod width16 {

    use num_enum::TryFromPrimitive;

    /// Represents a single bit in a 16-bit wide field
    #[repr(u16)]
    #[derive(Debug, Clone, Copy, PartialEq, TryFromPrimitive)]
    pub enum Bit {
        B0 = 0,
        B1,
        B2,
        B3,
        B4,
        B5,
        B6,
        B7,
        B8,
        B9,
        B10,
        B11,
        B12,
        B13,
        B14,
        B15,
    }
}

pub mod width32 {

    use num_enum::TryFromPrimitive;

    /// Represents a single bit in a 16-bit wide field
    #[repr(u32)]
    #[derive(Debug, Clone, Copy, PartialEq, TryFromPrimitive)]
    pub enum Bit {
        B0 = 0,
        B1,
        B2,
        B3,
        B4,
        B5,
        B6,
        B7,
        B8,

        B9,
        B10,
        B11,
        B12,
        B13,
        B14,
        B15,
        B16,

        B17,
        B18,
        B19,
        B20,
        B21,
        B22,
        B23,
        B24,

        B25,
        B26,
        B27,
        B28,
        B29,
        B30,
        B31,
        B32,
    }
}

/// Represents a bitfield. Can be implemented for any width (u8, u16, ...)
///
/// Enables easy detection of specific bits being set
pub trait Bitfield {
    type Bit;

    ///  Detect if a specific bit is set
    ///
    /// Example:
    /// ```
    /// use sensor_link_firmware::utils::bitwise::{width8::Bit, Bitfield};
    ///
    /// let n:u8 = 0b101;
    /// if n.bit(Bit::B2) {
    ///     // bit 2 is set!
    /// }
    /// ```
    fn bit(&self, n: Self::Bit) -> bool;

    ///  Set a specific bit
    ///
    /// Example:
    /// ```
    /// use sensor_link_firmware::utils::bitwise::{width8::Bit, Bitfield};
    ///
    /// let mut n:u8 = 0b101;
    /// n.set_bit(Bit::B1);
    /// // n is now 0b111;
    /// ```
    fn set_bit(&mut self, n: Self::Bit);

    /// Find all bits that are set
    ///
    /// Example:
    /// ```
    /// use sensor_link_firmware::utils::bitwise::{width8::Bit, Bitfield};
    ///
    /// let n:u8 = 0b101;
    /// n.each_set(|bit| {
    ///     // process each bit here.
    ///     // In this example bit=0 and bit=2
    /// });
    /// ```
    fn each_set<F: FnMut(Self::Bit)>(self, f: F);
}

macro_rules! impl_bitfield {
    ($type:ident, $width:ident) => {
        impl Bitfield for $type {
            type Bit = $width::Bit;

            fn bit(&self, n: Self::Bit) -> bool {
                self & (1 << (n as $type)) != 0
            }

            fn set_bit(&mut self, n: Self::Bit) {
                *self |= (1 << (n as $type));
            }

            fn each_set<F>(mut self, mut f: F)
            where
                F: FnMut(Self::Bit),
            {
                let mut n: $type = 0;
                while self != 0 {
                    if (self & 1) != 0 {
                        Self::Bit::try_from(n).map(&mut f).ok();
                    }
                    self >>= 1;
                    n += 1;
                }
            }
        }
    };
}

impl_bitfield!(u8, width8);
impl_bitfield!(u16, width16);
impl_bitfield!(u32, width32);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_8() {
        let n: u8 = 0b10101110;
        assert_eq!(false, n.bit(width8::Bit::B0));
        assert_eq!(true, n.bit(width8::Bit::B1));
        assert_eq!(true, n.bit(width8::Bit::B2));
        assert_eq!(true, n.bit(width8::Bit::B3));
        assert_eq!(false, n.bit(width8::Bit::B4));
        assert_eq!(true, n.bit(width8::Bit::B5));
        assert_eq!(false, n.bit(width8::Bit::B6));
        assert_eq!(true, n.bit(width8::Bit::B7));
    }

    #[test]
    fn each_8() {
        let n: u8 = 0b10101110;
        let mut res: Vec<<u8 as Bitfield>::Bit> = Vec::new();
        n.each_set(|bit| {
            res.push(bit);
        });
        use width8::Bit;
        assert_eq!(
            &[Bit::B1, Bit::B2, Bit::B3, Bit::B5, Bit::B7],
            res.as_slice()
        );
    }

    #[test]
    fn bits_16() {
        use width16::Bit;
        let n: u16 = 0b10001000_10101000;
        assert_eq!(false, n.bit(Bit::B0));
        assert_eq!(false, n.bit(Bit::B2));
        assert_eq!(false, n.bit(Bit::B4));
        assert_eq!(true, n.bit(Bit::B5));
        assert_eq!(false, n.bit(Bit::B8));
        assert_eq!(true, n.bit(Bit::B11));
        assert_eq!(false, n.bit(Bit::B12));
        assert_eq!(true, n.bit(Bit::B15));
    }

    #[test]
    fn each_16() {
        use width16::Bit;
        let n: u16 = 0b10001000_10101000;
        let mut res: Vec<<u16 as Bitfield>::Bit> = Vec::new();
        n.each_set(|bit| {
            res.push(bit);
        });
        assert_eq!(
            &[Bit::B3, Bit::B5, Bit::B7, Bit::B11, Bit::B15],
            res.as_slice()
        );
    }
}

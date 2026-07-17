pub struct Matrix8x8 {
    // The 64-bit matrix is stored in 2 (big-endian!) words
    words: [u32; 2],
}

impl Matrix8x8 {
    /// Constructs the matrix from 8 8-bit bytes
    pub fn from_bytes(bytes: &[u8; 8]) -> Self {
        Self::from_le_words([
            (bytes[3] as u32) << 24
                | (bytes[2] as u32) << 16
                | (bytes[1] as u32) << 8
                | (bytes[0] as u32),
            (bytes[7] as u32) << 24
                | (bytes[6] as u32) << 16
                | (bytes[5] as u32) << 8
                | (bytes[4] as u32) << 0,
        ])
    }

    /// Constructs the matrix from 2 little-endian 32-bit words
    pub fn from_le_words(words: [u32; 2]) -> Self {
        let words = [u32::from_be(words[0]), u32::from_be(words[1])];
        Self { words }
    }

    /// Get the resulting matrix as slice of bytes
    ///
    /// Note: use this if you expect this order:
    /// (transpose if you call leftmost bit '0')
    /// ```text
    /// 0b110    0b101
    /// 0b000 -> 0b101
    /// 0b111    0b001
    /// ```
    pub fn as_slice(&self) -> [u8; 8] {
        [
            ((self.words[0] >> 24) & 0xFF) as u8,
            ((self.words[0] >> 16) & 0xFF) as u8,
            ((self.words[0] >> 8) & 0xFF) as u8,
            ((self.words[0] >> 0) & 0xFF) as u8,
            ((self.words[1] >> 24) & 0xFF) as u8,
            ((self.words[1] >> 16) & 0xFF) as u8,
            ((self.words[1] >> 8) & 0xFF) as u8,
            ((self.words[1] >> 0) & 0xFF) as u8,
        ]
    }

    /// Get the resulting matrix as slice of bytes
    ///
    /// Note: use this if you expect this order:
    /// (if you call rightmost bit '0')
    /// ```text
    /// 0b110    0b001
    /// 0b000 -> 0b101
    /// 0b111    0b101
    /// ```
    pub fn as_reverse_slice(&self) -> [u8; 8] {
        [
            ((self.words[1] >> 0) & 0xFF) as u8,
            ((self.words[1] >> 8) & 0xFF) as u8,
            ((self.words[1] >> 16) & 0xFF) as u8,
            ((self.words[1] >> 24) & 0xFF) as u8,
            ((self.words[0] >> 0) & 0xFF) as u8,
            ((self.words[0] >> 8) & 0xFF) as u8,
            ((self.words[0] >> 16) & 0xFF) as u8,
            ((self.words[0] >> 24) & 0xFF) as u8,
        ]
    }

    /// Transposes the bit array
    ///
    /// The implementation aims to be efficient and is based on the bitwise tricks
    /// explained in 'Hacker's Delight 7.3' (Henry S. Warren, 2002)
    ///
    /// Example :
    /// ```text
    ///    [10000000,      [11010000,
    ///     11100000, ->    01010000,
    ///     00100000,       01110000,
    ///     11110000,       00010000,
    ///     00000001,       00000000,
    ///     00000001,       00000000,
    ///     00000001,       00000000,
    ///     00000001]       00001111]
    /// ```
    /// The result can be obtained via `as_slice()` or `as_reverse_slice()`
    /// depending on houw you interpret the bit ordering. (lefmost bit=0 or rightmost bit=0)
    pub fn transpose(mut self) -> Self {
        // x,y together represent the 64 bits of data
        let x = self.words[0];
        let y = self.words[1];

        // Stage 1: transpose 16 2x2 submatrices by swapping 2 sets of 8 bits
        let x: u32 = swap_bits(x, 7, 0x00AA00AA);
        let y: u32 = swap_bits(y, 7, 0x00AA00AA);

        // Stage 2: transpose 4 4x4 submatrixes (re-using the pre-transposed 2x2 blocks)
        let x = swap_bits(x, 14, 0x0000CCCC);
        let y = swap_bits(y, 14, 0x0000CCCC);

        // Final step: pack the 4-bit nibles into the 2 32-bit words to complete the transpose
        self.words[0] = (x & 0xF0F0F0F0) | ((y >> 4) & 0x0F0F0F0F);
        self.words[1] = ((x << 4) & 0xF0F0F0F0) | (y & 0x0F0F0F0F);

        self
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn transpose_1bit() {
        #[rustfmt::skip]
        let input: [u8; 8] = [
            0b00000000,
            0b00000000,
            0b00000000,
            0b00000000,
            0b00000000,
            0b00000001, // byte 5, bit 0 || byte 5, bit 7*
            0b00000000,
            0b00000000,
        ];

        #[rustfmt::skip]
        let expected: [u8; 8] = [
            0b00000000,
            0b00000000,
            0b00000000,
            0b00000000,
            0b00000000,
            0b00000000,
            0b00000000,
            0b00000100, // byte 5, bit 0 has become bit 5, byte 7
        ];

        assert_eq!(
            expected,
            Matrix8x8::from_bytes(&input).transpose().as_slice()
        );

        #[rustfmt::skip]
        let expected_inv: [u8; 8] = [
            0b00000100, // byte 5, bit 0 has become bit 5, byte 0
            0b00000000,
            0b00000000,
            0b00000000,
            0b00000000,
            0b00000000,
            0b00000000,
            0b00000000,
        ];

        assert_eq!(
            expected_inv,
            Matrix8x8::from_bytes(&input).transpose().as_reverse_slice()
        );
    }
    #[test]
    fn transpose_ident() {
        #[rustfmt::skip]
        let input: [u8; 8] = [
            0b00000001,
            0b00000010,
            0b00000100,
            0b00001000,
            0b00010000,
            0b00100000,
            0b01000000,
            0b10000000,
        ];

        assert_eq!(
            input.clone(),
            Matrix8x8::from_bytes(&input).transpose().as_slice()
        );
    }

    #[test]
    fn transpose_ident_pattern() {
        #[rustfmt::skip]
        let input: [u8; 8] = [
            0b10101010,
            0b00000000,
            0b10101010,
            0b00000000,
            0b10101010,
            0b00000000,
            0b10101010,
            0b00000000,
        ];

        assert_eq!(
            input.clone(),
            Matrix8x8::from_bytes(&input).transpose().as_slice()
        );
    }

    #[test]
    fn transpose_diagonal_mod() {
        #[rustfmt::skip]
        let input: [u8; 8] = [
            0b10000001,
            0b01000011,
            0b00100000,
            0b00010101,
            0b00001000,
            0b00000100,
            0b00000011,
            0b00000000,
        ];

        #[rustfmt::skip]
        let expected: [u8; 8] = [
            0b10000000,
            0b01000000,
            0b00100000,
            0b00010000,
            0b00001000,
            0b00010100,
            0b01000010,
            0b11010010,
        ];

        assert_eq!(
            expected,
            Matrix8x8::from_bytes(&input).transpose().as_slice()
        );
    }
}

/// Swap two (sets of) bits within the same word
///
/// ```text
///               |<--n_shifts-->|
/// input    = xxxQQQQQ_QQxxxxxx_PPPPPxxx_xxxxxxxx
/// mask     = 00000000_00000000_11111000_00000000
/// ```
/// This swaps each bit in the Q field with the P field.
/// In the example above they are 5 bits wide and 13 bits apart.
/// In general any mask and shift count can work, as long as the
/// two fields P and Q do not overlap.
fn swap_bits(input: u32, n_shifts: usize, mask: u32) -> u32 {
    // record the delta (XOR) between the masked locations
    let delta = (input ^ (input >> n_shifts)) & mask;

    // apply the delta in both field locations
    input ^ delta ^ (delta << n_shifts)
}

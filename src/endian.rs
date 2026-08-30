//! Reading fixed-width little-endian fields out of a byte slice.
//!
//! # Why these are one module and not nine functions
//!
//! `read_u16_le`, `read_u32_le` and `read_u64_le` were declared
//! **nine times** across `header`, `metadata`, `region_table` and `log`
//! — byte-identical bodies, each private to its own module.
//!
//! Nothing was wrong with any of them, and that is the point worth
//! recording: a helper this small is not duplicated because somebody
//! misunderstood it, but because declaring it again is cheaper in the
//! moment than importing it. The cost is paid later, by a reader who
//! has to check that the ninth copy still says `from_le_bytes` and not
//! `from_be_bytes` — a difference that changes every field the module
//! parses and is one character wide.
//!
//! # Little-endian, and only little-endian
//!
//! VHDX is little-endian throughout, unlike XFS which is big-endian
//! throughout. There is deliberately no big-endian half here: a
//! big-endian read in this crate would be a bug, and a helper for it
//! would make the bug spellable.
//!
//! # Panics
//!
//! Each panics if the slice is too short. Every caller has already
//! length-checked its buffer against the structure it is parsing —
//! that check is the parser's job and belongs where the structure's
//! size is known, not repeated per field. A panic here means a caller
//! skipped it, which is a bug in this crate rather than bad input.

/// Read a little-endian `u16` at `off`.
pub(crate) fn read_u16_le(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(b[off..off + 2].try_into().expect("2 bytes"))
}

/// Read a little-endian `u32` at `off`.
pub(crate) fn read_u32_le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().expect("4 bytes"))
}

/// Read a little-endian `u64` at `off`.
pub(crate) fn read_u64_le(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().expect("8 bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Byte order, against literal bytes.
    ///
    /// Asserting it against `from_le_bytes` would restate the
    /// implementation. These are the numbers a hex dump shows.
    #[test]
    fn the_low_byte_comes_first() {
        let b = [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        assert_eq!(read_u16_le(&b, 0), 0x0201);
        assert_eq!(read_u32_le(&b, 0), 0x0403_0201);
        assert_eq!(read_u64_le(&b, 0), 0x0807_0605_0403_0201);
    }

    /// The offset is a byte offset, not an index into units of the
    /// width being read.
    #[test]
    fn the_offset_counts_bytes() {
        let b = [0x00u8, 0xAA, 0xBB, 0x00];
        assert_eq!(read_u16_le(&b, 1), 0xBBAA);
    }

    #[test]
    fn the_widest_values_survive_the_round_trip() {
        let b = [0xFFu8; 8];
        assert_eq!(read_u16_le(&b, 0), u16::MAX);
        assert_eq!(read_u32_le(&b, 0), u32::MAX);
        assert_eq!(read_u64_le(&b, 0), u64::MAX);
    }
}

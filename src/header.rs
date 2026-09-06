//! VHDX header (4 KiB at file offset 64 KiB and 128 KiB; the higher
//! `sequence_number` of the two valid copies wins).
//!
//! Layout (offsets within the 4 KiB header):
//!
//! ```text
//!   0   4   signature "head"
//!   4   4   checksum (CRC-32C with field zeroed)
//!   8   8   sequence_number
//!  16  16   file_write_guid
//!  32  16   data_write_guid
//!  48  16   log_guid
//!  64   2   log_version
//!  66   2   version
//!  68   4   log_length
//!  72   8   log_offset
//!  80 4016  reserved
//! ```

use crate::endian::{read_u16_le, read_u32_le, read_u64_le};
use crate::error::{Error, Result};

pub const HEADER_SIZE: usize = 4096;
pub const HEADER1_OFFSET: u64 = 64 * 1024;
pub const HEADER2_OFFSET: u64 = 128 * 1024;
pub const HEADER_SIGNATURE: &[u8; 4] = b"head";
pub const HEADER_VERSION: u16 = 1;

#[derive(Debug, Clone)]
pub struct Header {
    pub sequence_number: u64,
    pub file_write_guid: [u8; 16],
    pub data_write_guid: [u8; 16],
    pub log_guid: [u8; 16],
    pub version: u16,
    pub log_length: u32,
    pub log_offset: u64,
}

impl Header {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < HEADER_SIZE {
            return Err(Error::Corrupt("header shorter than 4 KiB"));
        }
        if &bytes[0..4] != HEADER_SIGNATURE {
            return Err(Error::Corrupt("header signature mismatch"));
        }
        let stored_crc = read_u32_le(bytes, 4);
        let computed = compute_crc(bytes);
        if stored_crc != computed {
            return Err(Error::BadChecksum {
                expected: stored_crc,
                found: computed,
                what: "header",
            });
        }

        let sequence_number = read_u64_le(bytes, 8);
        let mut file_write_guid = [0u8; 16];
        file_write_guid.copy_from_slice(&bytes[16..32]);
        let mut data_write_guid = [0u8; 16];
        data_write_guid.copy_from_slice(&bytes[32..48]);
        let mut log_guid = [0u8; 16];
        log_guid.copy_from_slice(&bytes[48..64]);
        let _log_version = read_u16_le(bytes, 64);
        let version = read_u16_le(bytes, 66);
        if version != HEADER_VERSION {
            return Err(Error::Corrupt("unsupported VHDX header version"));
        }
        let log_length = read_u32_le(bytes, 68);
        let log_offset = read_u64_le(bytes, 72);

        Ok(Self {
            sequence_number,
            file_write_guid,
            data_write_guid,
            log_guid,
            version,
            log_length,
            log_offset,
        })
    }
}

/// Compute the header's CRC-32C with the checksum field zeroed.
pub fn compute_crc(bytes: &[u8]) -> u32 {
    let mut buf = [0u8; HEADER_SIZE];
    buf.copy_from_slice(&bytes[..HEADER_SIZE]);
    buf[4..8].fill(0);
    crc32c::crc32c(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a structurally valid 4 KiB header with the given sequence
    /// number and a correct CRC-32C. Optional knobs let callers corrupt
    /// individual fields after the fact.
    fn valid_header(seq: u64) -> Vec<u8> {
        let mut h = vec![0u8; HEADER_SIZE];
        h[0..4].copy_from_slice(HEADER_SIGNATURE);
        h[8..16].copy_from_slice(&seq.to_le_bytes());
        h[16..32].copy_from_slice(&[0xAA; 16]); // file_write_guid
        h[32..48].copy_from_slice(&[0xBB; 16]); // data_write_guid
        h[48..64].copy_from_slice(&[0xCC; 16]); // log_guid
        h[66..68].copy_from_slice(&HEADER_VERSION.to_le_bytes());
        h[68..72].copy_from_slice(&(1u32 << 20).to_le_bytes()); // log_length = 1 MiB
        h[72..80].copy_from_slice(&(4u64 << 20).to_le_bytes()); // log_offset = 4 MiB
        let crc = compute_crc(&h);
        h[4..8].copy_from_slice(&crc.to_le_bytes());
        h
    }

    #[test]
    fn parses_a_valid_header_and_exposes_fields() {
        let h = valid_header(42);
        let parsed = Header::parse(&h).unwrap();
        assert_eq!(parsed.sequence_number, 42);
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.log_length, 1 << 20);
        assert_eq!(parsed.log_offset, 4 << 20);
        assert_eq!(parsed.file_write_guid, [0xAA; 16]);
        assert_eq!(parsed.data_write_guid, [0xBB; 16]);
        assert_eq!(parsed.log_guid, [0xCC; 16]);
    }

    #[test]
    fn rejects_buffer_shorter_than_4_kib() {
        let err = Header::parse(&[0u8; 100]).unwrap_err();
        assert!(matches!(err, Error::Corrupt(_)), "got {err:?}");
    }

    #[test]
    fn rejects_bad_signature() {
        let mut h = valid_header(1);
        h[0..4].copy_from_slice(b"xxxx");
        // Recompute CRC so the failure is attributable to the signature,
        // not a checksum mismatch.
        let crc = compute_crc(&h);
        h[4..8].copy_from_slice(&crc.to_le_bytes());
        let err = Header::parse(&h).unwrap_err();
        assert!(matches!(err, Error::Corrupt(_)), "got {err:?}");
    }

    #[test]
    fn rejects_crc_mismatch() {
        let mut h = valid_header(1);
        // Flip a payload byte without recomputing the stored CRC.
        h[8] ^= 0xFF;
        let err = Header::parse(&h).unwrap_err();
        match err {
            Error::BadChecksum { what, .. } => assert_eq!(what, "header"),
            other => panic!("expected BadChecksum, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unsupported_version() {
        let mut h = valid_header(1);
        h[66..68].copy_from_slice(&2u16.to_le_bytes());
        let crc = compute_crc(&h);
        h[4..8].copy_from_slice(&crc.to_le_bytes());

        let err = Header::parse(&h).unwrap_err();
        assert!(matches!(
            err,
            Error::Corrupt("unsupported VHDX header version")
        ));
    }

    #[test]
    fn compute_crc_is_independent_of_stored_checksum_field() {
        let mut h = valid_header(7);
        let a = compute_crc(&h);
        // Scribble over the stored checksum field; compute_crc zeroes it
        // internally, so the result must not change.
        h[4..8].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        let b = compute_crc(&h);
        assert_eq!(a, b);
    }
}

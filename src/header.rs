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

use crate::error::{Error, Result};

pub const HEADER_SIZE: usize = 4096;
pub const HEADER1_OFFSET: u64 = 64 * 1024;
pub const HEADER2_OFFSET: u64 = 128 * 1024;
pub const HEADER_SIGNATURE: &[u8; 4] = b"head";

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

fn read_u32_le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn read_u16_le(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn read_u64_le(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        b[off],
        b[off + 1],
        b[off + 2],
        b[off + 3],
        b[off + 4],
        b[off + 5],
        b[off + 6],
        b[off + 7],
    ])
}

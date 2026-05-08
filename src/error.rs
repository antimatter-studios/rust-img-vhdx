use std::fmt;
use std::io;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// File-identifier signature ("vhdxfile") is missing or wrong.
    NotVhdx,
    /// Header signature ("head") missing on both header slots, or both
    /// failed CRC validation.
    NoValidHeader,
    /// Region-table signature ("regi") missing or both copies invalid.
    NoValidRegionTable,
    /// Metadata-region signature ("metadata") missing.
    BadMetadata(&'static str),
    /// CRC-32C mismatch.
    BadChecksum {
        expected: u32,
        found: u32,
        what: &'static str,
    },
    /// Header field combination is internally inconsistent.
    Corrupt(&'static str),
    /// A feature the reader doesn't yet handle.
    Unsupported(&'static str),
    /// Read past the virtual disk end.
    OutOfBounds {
        offset: u64,
        len: u64,
        size: u64,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io: {e}"),
            Error::NotVhdx => write!(f, "not a VHDX image (file-identifier mismatch)"),
            Error::NoValidHeader => write!(f, "no valid VHDX header found in either slot"),
            Error::NoValidRegionTable => write!(f, "no valid VHDX region table found"),
            Error::BadMetadata(s) => write!(f, "bad metadata region: {s}"),
            Error::BadChecksum {
                expected,
                found,
                what,
            } => {
                write!(
                    f,
                    "{what} CRC-32C mismatch: expected {expected:#x}, found {found:#x}"
                )
            }
            Error::Corrupt(s) => write!(f, "corrupt VHDX: {s}"),
            Error::Unsupported(s) => write!(f, "unsupported VHDX feature: {s}"),
            Error::OutOfBounds { offset, len, size } => {
                write!(
                    f,
                    "read [{offset}, {offset}+{len}) past virtual size {size}"
                )
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

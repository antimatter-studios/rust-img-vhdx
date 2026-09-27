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
    /// Write attempted on a reader opened read-only.
    ReadOnly,
    /// The log holds entries that have not been applied, and the device
    /// cannot take them.
    ///
    /// Distinct from [`Error::ReadOnly`], which describes the *opener*.
    /// This describes the *file*: it needs work done to it before its
    /// region table, metadata and BAT mean anything, and the caller's
    /// available move is to reopen it writable. A non-zero `log_guid`
    /// alone is not this error — it says a writer stamped the file, not
    /// that anything is pending.
    LogNeedsReplay,
    /// The log holds entries and which of them form the active chain
    /// could not be determined. **Nothing has been written.**
    ///
    /// That last sentence is why this is not [`Error::LogReplay`]: this
    /// error is returned before the first descriptor is applied, so the
    /// image is exactly as it was found. Telling the two apart is the
    /// difference between "reopen it elsewhere, or with a recovery tool"
    /// and "this file is now half-changed".
    ///
    /// The condition is a head entry whose `tail` names no entry
    /// discovery found. The format provides `tail` so a replayer does
    /// not have to guess where a sequence began, and guessing is what
    /// this refuses: the entry lowest in the region may belong to a run
    /// the head has disowned, and applying it would write stale bytes
    /// and then erase the log that still holds the live chain.
    ///
    /// It used to be an empty chain, indistinguishable from a log with
    /// nothing to do — so `open` read the region table, the metadata and
    /// the BAT out of the very bytes the log was going to fix, and
    /// returned a reader serving pre-crash data with no error and no
    /// signal (#41).
    LogUnassembled(&'static str),
    /// Log replay failed mid-stream — image is in an inconsistent state
    /// the reader cannot safely interpret.
    ///
    /// Contrast [`Error::LogUnassembled`], which is refused *before*
    /// anything is applied.
    LogReplay(String),
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
            Error::ReadOnly => write!(f, "VHDX reader is read-only"),
            Error::LogNeedsReplay => write!(
                f,
                "the log holds unreplayed entries and the device is read-only; \
                 reopen the image writable so the log can be applied"
            ),
            Error::LogUnassembled(s) => write!(
                f,
                "the log holds entries but its active chain could not be \
                 assembled, so nothing was applied and the image is unchanged: \
                 {s}"
            ),
            Error::LogReplay(s) => write!(f, "VHDX log replay failed: {s}"),
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

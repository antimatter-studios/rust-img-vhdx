//! A copy-on-write view of an image file, so a log can be replayed
//! without writing to it.
//!
//! OPENING A VHDX WHOSE LOG HOLDS ENTRIES IS A WRITE. The library applies
//! the log in place when it opens such an image on a device it can write
//! to, and refuses the open when it cannot (`LogNeedsReplay`). Neither is
//! right for `info` or `read`: the user asked to look, and looking must not
//! change the file they pointed at, nor fail because the last writer did
//! not get to close it.
//!
//! So the read-only verbs open the file READ-ONLY and hand the library this
//! device over it instead. Writes — the replay's — land in memory, a page
//! at a time; reads see those pages over the file's own bytes. The library
//! gets a device it may replay onto, the report and the bytes are the
//! replayed image's, and the file is never opened for writing.
//!
//! [`Overlay::written`] says whether anything was written, which is
//! whether the log had entries to replay: the `dirty` the report carries.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use fs_core::{BlockDevice, BlockRead, Error, Result};

/// The unit a write is kept in. The log's own sector size, which is also
/// the granularity of everything a replay writes.
const PAGE: u64 = 4096;

pub struct Overlay {
    base: Box<dyn BlockRead>,
    /// The base's length, read once: the file is not written, so it does
    /// not change under us.
    base_len: u64,
    /// The device's length as the replay has made it: the base's, or
    /// longer once a replay has grown it with `set_len`.
    len: Mutex<u64>,
    /// Every page written, keyed by its index, whole.
    pages: Mutex<HashMap<u64, Box<[u8]>>>,
    written: AtomicBool,
}

impl Overlay {
    pub fn new(base: Box<dyn BlockRead>) -> Overlay {
        let base_len = base.size_bytes();
        Overlay {
            base,
            base_len,
            len: Mutex::new(base_len),
            pages: Mutex::new(HashMap::new()),
            written: AtomicBool::new(false),
        }
    }

    /// Whether anything has been written to the overlay.
    pub fn written(&self) -> bool {
        self.written.load(Ordering::SeqCst)
    }

    /// Read `buf.len()` bytes of the base at `offset`, zeros past its end.
    fn read_base(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let have = self.base_len.saturating_sub(offset).min(buf.len() as u64) as usize;
        if have > 0 {
            self.base.read_at(offset, &mut buf[..have])?;
        }
        buf[have..].fill(0);
        Ok(())
    }

    fn bound(&self, offset: u64, len: usize) -> Result<()> {
        let size = *self.len.lock().unwrap();
        match offset.checked_add(len as u64) {
            Some(end) if end <= size => Ok(()),
            _ => Err(Error::OutOfBounds {
                offset,
                len: len as u64,
                size,
            }),
        }
    }

    /// `offset..offset + len` as (page index, offset in page, position in
    /// the caller's buffer, length) pieces.
    fn pieces(offset: u64, len: usize) -> impl Iterator<Item = (u64, usize, usize, usize)> {
        let mut at = offset;
        let end = offset + len as u64;
        std::iter::from_fn(move || {
            if at >= end {
                return None;
            }
            let index = at / PAGE;
            let in_page = (at % PAGE) as usize;
            let n = (PAGE as usize - in_page).min((end - at) as usize);
            let piece = (index, in_page, (at - offset) as usize, n);
            at += n as u64;
            Some(piece)
        })
    }
}

impl BlockRead for Overlay {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.bound(offset, buf.len())?;
        let pages = self.pages.lock().unwrap();
        for (index, in_page, pos, n) in Self::pieces(offset, buf.len()) {
            let dst = &mut buf[pos..pos + n];
            match pages.get(&index) {
                Some(page) => dst.copy_from_slice(&page[in_page..in_page + n]),
                None => self.read_base(index * PAGE + in_page as u64, dst)?,
            }
        }
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        *self.len.lock().unwrap()
    }
}

impl BlockDevice for Overlay {
    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<()> {
        self.bound(offset, buf.len())?;
        let mut pages = self.pages.lock().unwrap();
        for (index, in_page, pos, n) in Self::pieces(offset, buf.len()) {
            let page = match pages.entry(index) {
                Entry::Occupied(e) => e.into_mut(),
                Entry::Vacant(e) => {
                    let mut page = vec![0u8; PAGE as usize].into_boxed_slice();
                    self.read_base(index * PAGE, &mut page)?;
                    e.insert(page)
                }
            };
            page[in_page..in_page + n].copy_from_slice(&buf[pos..pos + n]);
        }
        self.written.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        Ok(())
    }

    fn is_writable(&self) -> bool {
        true
    }

    /// Growth only: a replay extends the file to the extent its log names
    /// and never shrinks it, and a shrink here would leave pages past the
    /// new end to reappear on a later grow.
    fn set_len(&self, new_len: u64) -> Result<()> {
        let mut len = self.len.lock().unwrap();
        if new_len < *len {
            return Err(Error::Custom(format!(
                "the in-memory view of the image does not shrink ({} to {new_len} bytes)",
                *len
            )));
        }
        *len = new_len;
        self.written.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn can_grow(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Bytes(Vec<u8>);
    impl BlockRead for Bytes {
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
            let at = offset as usize;
            buf.copy_from_slice(&self.0[at..at + buf.len()]);
            Ok(())
        }
        fn size_bytes(&self) -> u64 {
            self.0.len() as u64
        }
    }

    fn base() -> Vec<u8> {
        (0..3 * PAGE as usize + 100)
            .map(|i| (i % 251) as u8)
            .collect()
    }

    #[test]
    fn reads_see_the_base_until_a_write_and_the_write_after() {
        let o = Overlay::new(Box::new(Bytes(base())));
        let mut got = vec![0u8; 5000];
        o.read_at(4000, &mut got).unwrap();
        assert_eq!(got, base()[4000..9000]);
        assert!(!o.written());

        o.write_at(4090, &[0xaa; 20]).unwrap();
        assert!(o.written());
        o.read_at(4000, &mut got).unwrap();
        let mut want = base()[4000..9000].to_vec();
        want[90..110].fill(0xaa);
        assert_eq!(
            got, want,
            "a write across a page boundary reads back over the base"
        );
    }

    #[test]
    fn growth_reads_as_zeros_past_the_base_and_a_shrink_is_refused() {
        let o = Overlay::new(Box::new(Bytes(base())));
        let old = o.size_bytes();
        assert!(o.write_at(old, &[1]).is_err(), "no write past the end");
        o.set_len(old + 2 * PAGE).unwrap();
        let mut tail = vec![0xffu8; PAGE as usize];
        o.read_at(old, &mut tail).unwrap();
        assert!(tail.iter().all(|b| *b == 0));
        o.write_at(old + PAGE, &[7; 10]).unwrap();
        let mut got = [0u8; 10];
        o.read_at(old + PAGE, &mut got).unwrap();
        assert_eq!(got, [7; 10]);
        assert!(o.set_len(old).is_err());
    }
}

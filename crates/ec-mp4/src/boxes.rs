//! Box plumbing: the walk over a slice of children, the bounded reader over one
//! seekable source, and the writer's nesting helpers.
//!
//! Everything a demuxer touches goes through [`Boxes`], and everything a muxer
//! writes goes through [`open`]/[`close`], so a size field is never computed by
//! hand in either direction.

use std::io::{Read, Seek, SeekFrom};

use ec_core::{Error, Result};

/// A box type, as the four characters it is written with.
pub type FourCc = [u8; 4];

/// The children of one box payload, in order.
///
/// A `size` of 0 means "to the end of the parent" and a size of 1 means the real
/// size is the 64-bit one that follows the type; both are handled here so no
/// caller ever sees them. A header that does not fit, or a size smaller than its
/// own header, ends the walk with [`Error::Corrupt`] — never a panic and never a
/// silent truncation, because a demuxer that walks past a broken box is reading
/// somebody else's bytes.
pub struct Boxes<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Boxes<'a> {
    /// Walk `data` as a sequence of boxes.
    pub fn new(data: &'a [u8]) -> Boxes<'a> {
        Boxes { data, pos: 0 }
    }
}

impl<'a> Iterator for Boxes<'a> {
    type Item = Result<(FourCc, &'a [u8])>;

    fn next(&mut self) -> Option<Self::Item> {
        let left = self.data.len().checked_sub(self.pos)?;
        if left < 8 {
            // Trailing padding shorter than a header is what a lot of real
            // muxers leave behind; only a stub of one is worth complaining about.
            self.pos = self.data.len();
            return match left {
                0 => None,
                _ => Some(Err(Error::corrupt("mp4: trailing bytes are not a box"))),
            };
        }
        let at = self.pos;
        let mut size = u32::from_be_bytes([
            self.data[at],
            self.data[at + 1],
            self.data[at + 2],
            self.data[at + 3],
        ]) as u64;
        let kind: FourCc = [
            self.data[at + 4],
            self.data[at + 5],
            self.data[at + 6],
            self.data[at + 7],
        ];
        let mut head = 8usize;
        if size == 1 {
            if left < 16 {
                self.pos = self.data.len();
                return Some(Err(Error::corrupt("mp4: 64-bit box size past the parent")));
            }
            size = u64::from_be_bytes(self.data[at + 8..at + 16].try_into().unwrap());
            head = 16;
        } else if size == 0 {
            size = left as u64;
        }
        if size < head as u64 || size > left as u64 {
            self.pos = self.data.len();
            let name = String::from_utf8_lossy(&kind).into_owned();
            return Some(Err(Error::corrupt(format!(
                "mp4: box '{name}' states {size} bytes inside {left}"
            ))));
        }
        self.pos = at + size as usize;
        Some(Ok((kind, &self.data[at + head..at + size as usize])))
    }
}

/// The `version` and `flags` every FullBox begins with, and the payload after
/// them.
pub fn full(payload: &[u8]) -> Result<(u8, u32, &[u8])> {
    if payload.len() < 4 {
        return Err(Error::corrupt("mp4: FullBox shorter than its version"));
    }
    let flags = u32::from_be_bytes([0, payload[1], payload[2], payload[3]]);
    Ok((payload[0], flags, &payload[4..]))
}

/// Big-endian `u16` at `at`, or [`Error::Corrupt`] when the box is too short.
pub fn be16(b: &[u8], at: usize) -> Result<u16> {
    let s = b.get(at..at + 2).ok_or_else(short)?;
    Ok(u16::from_be_bytes(s.try_into().unwrap()))
}

/// Big-endian `u32` at `at`.
pub fn be32(b: &[u8], at: usize) -> Result<u32> {
    let s = b.get(at..at + 4).ok_or_else(short)?;
    Ok(u32::from_be_bytes(s.try_into().unwrap()))
}

/// Big-endian `u64` at `at`.
pub fn be64(b: &[u8], at: usize) -> Result<u64> {
    let s = b.get(at..at + 8).ok_or_else(short)?;
    Ok(u64::from_be_bytes(s.try_into().unwrap()))
}

fn short() -> Error {
    Error::corrupt("mp4: box ends inside a field")
}

/// Positioned reads over the one reader a demuxer was opened with, so a seek
/// never needs a second one.
pub struct Src<R> {
    r: R,
    pos: u64,
    /// Length of the source in bytes.
    pub len: u64,
}

impl<R: Read + Seek> Src<R> {
    /// Measure `r` and rewind it.
    pub fn new(mut r: R) -> Result<Src<R>> {
        let len = r.seek(SeekFrom::End(0))?;
        r.seek(SeekFrom::Start(0))?;
        Ok(Src { r, pos: 0, len })
    }

    fn seek_to(&mut self, at: u64) -> Result<()> {
        if at != self.pos {
            self.r.seek(SeekFrom::Start(at))?;
            self.pos = at;
        }
        Ok(())
    }

    /// As many of `buf.len()` bytes from `at` as are left.
    pub fn read_upto(&mut self, at: u64, buf: &mut [u8]) -> Result<usize> {
        self.seek_to(at)?;
        let mut got = 0;
        while got < buf.len() {
            match self.r.read(&mut buf[got..])? {
                0 => break,
                n => got += n,
            }
        }
        self.pos += got as u64;
        Ok(got)
    }

    /// Exactly `buf.len()` bytes from `at`, or [`Error::NeedMore`] for a file
    /// that stops early — a truncated download, which is not corruption.
    pub fn read_exact_at(&mut self, at: u64, buf: &mut [u8]) -> Result<()> {
        if self.read_upto(at, buf)? != buf.len() {
            return Err(Error::NeedMore);
        }
        Ok(())
    }

    /// `len` bytes from `at`, refusing anything over `limit` rather than
    /// allocating it: a crafted `moov` size must not become all the memory there
    /// is.
    pub fn read_vec(&mut self, at: u64, len: u64, limit: u64) -> Result<Vec<u8>> {
        if len > limit {
            return Err(Error::corrupt(format!(
                "mp4: a {len}-byte box where at most {limit} makes sense"
            )));
        }
        if at.saturating_add(len) > self.len {
            return Err(Error::NeedMore);
        }
        let mut out = vec![0u8; len as usize];
        self.read_exact_at(at, &mut out)?;
        Ok(out)
    }

    /// The header of the box at `at`: its type, where its body starts and where
    /// it ends. `None` once `at` reaches `end`.
    pub fn header_at(&mut self, at: u64, end: u64) -> Result<Option<(FourCc, u64, u64)>> {
        if at.saturating_add(8) > end {
            return Ok(None);
        }
        let mut head = [0u8; 16];
        let want = (end - at).min(16) as usize;
        let got = self.read_upto(at, &mut head[..want])?;
        if got < 8 {
            return Ok(None);
        }
        let mut size = u32::from_be_bytes(head[..4].try_into().unwrap()) as u64;
        let kind: FourCc = head[4..8].try_into().unwrap();
        let mut head_len = 8u64;
        if size == 1 {
            if got < 16 {
                return Err(Error::corrupt("mp4: 64-bit box size past the end of file"));
            }
            size = u64::from_be_bytes(head[8..16].try_into().unwrap());
            head_len = 16;
        } else if size == 0 {
            size = end - at;
        }
        if size < head_len {
            let name = String::from_utf8_lossy(&kind).into_owned();
            return Err(Error::corrupt(format!(
                "mp4: box '{name}' is {size} bytes, shorter than its own header"
            )));
        }
        let body = at + head_len;
        let stop = at.saturating_add(size).min(end);
        Ok(Some((kind, body, stop)))
    }
}

/// Start a box of type `kind`, returning the offset its size field will be
/// patched at by [`close`].
pub fn open(out: &mut Vec<u8>, kind: &FourCc) -> usize {
    let at = out.len();
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(kind);
    at
}

/// Finish the box [`open`] started at `at` by writing its size.
pub fn close(out: &mut [u8], at: usize) {
    let size = (out.len() - at) as u32;
    out[at..at + 4].copy_from_slice(&size.to_be_bytes());
}

/// A whole box in one call, for the leaves.
pub fn leaf(out: &mut Vec<u8>, kind: &FourCc, payload: &[u8]) {
    let at = open(out, kind);
    out.extend_from_slice(payload);
    close(out, at);
}

/// The `version`/`flags` word a FullBox starts with.
pub fn full_head(out: &mut Vec<u8>, version: u8, flags: u32) {
    out.push(version);
    out.extend_from_slice(&flags.to_be_bytes()[1..]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(data: &[u8]) -> Result<Vec<(String, usize)>> {
        Boxes::new(data)
            .map(|b| b.map(|(k, p)| (String::from_utf8_lossy(&k).into_owned(), p.len())))
            .collect()
    }

    #[test]
    fn walks_32_and_64_bit_sizes() {
        let mut out = Vec::new();
        leaf(&mut out, b"ftyp", &[1, 2, 3, 4]);
        // The same box written in the 64-bit form a big mdat uses.
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(b"mdat");
        out.extend_from_slice(&24u64.to_be_bytes());
        out.extend_from_slice(&[0; 8]);
        assert_eq!(
            types(&out).unwrap(),
            vec![("ftyp".into(), 4), ("mdat".into(), 8)]
        );
    }

    #[test]
    fn a_size_that_does_not_fit_is_corrupt_not_a_panic() {
        let mut out = Vec::new();
        leaf(&mut out, b"moov", &[0; 4]);
        out[3] = 0xFF; // 255 bytes of moov inside 12
        assert!(types(&out).is_err());

        // A size below the header is the other half of the same trap.
        let mut tiny = 4u32.to_be_bytes().to_vec();
        tiny.extend_from_slice(b"free");
        tiny.extend_from_slice(&[0; 4]);
        assert!(types(&tiny).is_err());

        // ...and a zero size runs to the parent's end rather than looping.
        let mut zero = 0u32.to_be_bytes().to_vec();
        zero.extend_from_slice(b"mdat");
        zero.extend_from_slice(&[0; 4]);
        assert_eq!(types(&zero).unwrap(), vec![("mdat".into(), 4)]);
    }

    #[test]
    fn fields_past_the_end_are_errors() {
        assert!(be32(&[0, 0], 0).is_err());
        assert!(be16(&[0, 0, 0, 0], 3).is_err());
        assert!(be64(&[0; 8], 1).is_err());
        assert!(full(&[0, 0, 0]).is_err());
        assert_eq!(full(&[1, 0, 0, 2, 9]).unwrap(), (1, 2, &[9u8][..]));
    }
}

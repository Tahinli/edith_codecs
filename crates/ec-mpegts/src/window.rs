//! A sliding read window over a `Read + Seek`, so the demuxer can look at bytes
//! around an offset without a syscall per transport packet.

use std::io::{Read, Seek, SeekFrom};

use ec_core::{Error, Result};

/// Bytes fetched per read.
const CHUNK: usize = 256 * 1024;
/// Consumed bytes kept behind the cursor before the buffer is compacted.
const KEEP: usize = 512 * 1024;

pub(crate) struct Window<R> {
    r: R,
    /// File offset of `buf[0]`.
    base: u64,
    buf: Vec<u8>,
    /// True once a read returned zero bytes at `base + buf.len()`.
    at_eof: bool,
}

impl<R: Read + Seek> Window<R> {
    pub(crate) fn new(r: R) -> Window<R> {
        Window {
            r,
            base: 0,
            buf: Vec::new(),
            at_eof: false,
        }
    }

    /// Total length of the file.
    pub(crate) fn len(&mut self) -> Result<u64> {
        let end = self.r.seek(SeekFrom::End(0)).map_err(Error::Io)?;
        self.reposition()?;
        Ok(end)
    }

    fn reposition(&mut self) -> Result<()> {
        self.r
            .seek(SeekFrom::Start(self.base + self.buf.len() as u64))
            .map_err(Error::Io)?;
        Ok(())
    }

    /// Up to `n` bytes at `off`; shorter only at the end of the file.
    pub(crate) fn get(&mut self, off: u64, n: usize) -> Result<&[u8]> {
        let inside = off >= self.base && off - self.base <= self.buf.len() as u64;
        if !inside {
            self.r.seek(SeekFrom::Start(off)).map_err(Error::Io)?;
            self.base = off;
            self.buf.clear();
            self.at_eof = false;
        }
        let mut rel = (off - self.base) as usize;
        if rel > KEEP {
            self.buf.drain(..rel);
            self.base = off;
            rel = 0;
        }
        while self.buf.len() < rel + n && !self.at_eof {
            let old = self.buf.len();
            self.buf.resize(old + CHUNK.max(n), 0);
            let mut filled = old;
            loop {
                match self.r.read(&mut self.buf[filled..]) {
                    Ok(0) => {
                        self.at_eof = true;
                        break;
                    }
                    Ok(k) => {
                        filled += k;
                        if filled >= old + CHUNK.min(self.buf.len() - old) {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => {
                        self.buf.truncate(old);
                        return Err(Error::Io(e));
                    }
                }
            }
            self.buf.truncate(filled);
        }
        let end = (rel + n).min(self.buf.len());
        Ok(&self.buf[rel.min(end)..end])
    }
}

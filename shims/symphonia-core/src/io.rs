//! The source a reader reads from.

use std::io::{Read, Seek};

/// Anything a reader can be built over: seekable, readable, and movable to the
/// thread that will do the reading.
pub trait MediaSource: Read + Seek + Send {}

impl<T: Read + Seek + Send> MediaSource for T {}

/// Options a stream is built with; nothing here has any.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MediaSourceStreamOptions {
    /// Buffer size a caller would like. This shim buffers in the demuxers
    /// themselves, so it is advisory.
    pub buffer_len: usize,
}

/// A source with a buffer in front of it.
pub struct MediaSourceStream {
    inner: Box<dyn MediaSource>,
}

impl MediaSourceStream {
    /// Wrap a source.
    pub fn new(
        source: Box<dyn MediaSource>,
        _options: MediaSourceStreamOptions,
    ) -> MediaSourceStream {
        MediaSourceStream { inner: source }
    }

    /// Take the source back out, which is how a reader in this family is
    /// handed the file it will read.
    pub fn into_inner(self) -> Box<dyn MediaSource> {
        self.inner
    }
}

impl Read for MediaSourceStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Seek for MediaSourceStream {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

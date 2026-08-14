//! `flacenc::bitsink`: where a written stream lands.

use std::convert::Infallible;

/// Somewhere bits can be written.
///
/// Only the part the replica needs: it constructs a [`ByteSink`], hands it to
/// `Stream::write` and reads the bytes back.
pub trait BitSink: Sized {
    /// What writing can fail with.
    type Error: std::error::Error;

    /// Append whole bytes.
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;

    /// Bits written so far.
    fn bit_len(&self) -> usize;
}

/// A `Vec<u8>` sink — `flacenc::bitsink::ByteSink`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ByteSink {
    storage: Vec<u8>,
}

impl ByteSink {
    /// An empty sink.
    pub fn new() -> Self {
        ByteSink::default()
    }

    /// An empty sink with room for `bits`.
    pub fn with_capacity(bits: usize) -> Self {
        ByteSink {
            storage: Vec::with_capacity(bits.div_ceil(8)),
        }
    }

    /// The bytes written so far.
    pub fn as_slice(&self) -> &[u8] {
        &self.storage
    }

    /// Take the bytes.
    pub fn into_inner(self) -> Vec<u8> {
        self.storage
    }

    /// Bytes written so far.
    pub fn len(&self) -> usize {
        self.storage.len()
    }

    /// True when nothing has been written.
    pub fn is_empty(&self) -> bool {
        self.storage.is_empty()
    }
}

impl BitSink for ByteSink {
    type Error = Infallible;

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Infallible> {
        self.storage.extend_from_slice(bytes);
        Ok(())
    }

    fn bit_len(&self) -> usize {
        self.storage.len() * 8
    }
}

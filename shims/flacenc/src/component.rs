//! `flacenc::component`: the encoded stream and its `STREAMINFO`.
//!
//! The incumbent models every FLAC component as an addressable value and
//! writes them out on demand. Ours holds the encoded bytes and the header
//! fields a caller is allowed to correct afterwards — which is the whole of
//! what the replica does with the tree.

use crate::bitsink::BitSink;
use crate::error::{OutputError, VerifyError};

/// Something that can state its size and write itself to a [`BitSink`].
pub trait BitRepr {
    /// Bits this component occupies.
    fn count_bits(&self) -> usize;

    /// Write the component out.
    ///
    /// # Errors
    /// When the sink fails, or the component holds a value FLAC cannot express.
    fn write<S: BitSink>(&self, dest: &mut S) -> Result<(), OutputError<S>>;
}

/// A complete encoded FLAC stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    bytes: Vec<u8>,
    stream_info: StreamInfo,
}

/// Offset of the `STREAMINFO` payload: `fLaC` plus the 4-byte block header.
const STREAM_INFO_AT: usize = 8;

impl Stream {
    pub(crate) fn from_bytes(bytes: Vec<u8>) -> Stream {
        let min = u16::from_be_bytes([bytes[STREAM_INFO_AT], bytes[STREAM_INFO_AT + 1]]);
        let max = u16::from_be_bytes([bytes[STREAM_INFO_AT + 2], bytes[STREAM_INFO_AT + 3]]);
        Stream {
            bytes,
            stream_info: StreamInfo {
                min_block_size: min,
                max_block_size: max,
            },
        }
    }

    /// The stream's `STREAMINFO`, mutable.
    pub fn stream_info_mut(&mut self) -> &mut StreamInfo {
        &mut self.stream_info
    }

    /// The stream's `STREAMINFO`.
    pub fn stream_info(&self) -> &StreamInfo {
        &self.stream_info
    }
}

impl BitRepr for Stream {
    fn count_bits(&self) -> usize {
        self.bytes.len() * 8
    }

    fn write<S: BitSink>(&self, dest: &mut S) -> Result<(), OutputError<S>> {
        // The declared block sizes are the one field a caller may have changed
        // after encoding, so they are patched in on the way out.
        let mut bytes = self.bytes.clone();
        bytes[STREAM_INFO_AT..STREAM_INFO_AT + 2]
            .copy_from_slice(&self.stream_info.min_block_size.to_be_bytes());
        bytes[STREAM_INFO_AT + 2..STREAM_INFO_AT + 4]
            .copy_from_slice(&self.stream_info.max_block_size.to_be_bytes());
        dest.write_bytes(&bytes)
            .map_err(|e| OutputError::new(e.to_string()))
    }
}

/// The `STREAMINFO` fields a caller may correct after encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamInfo {
    min_block_size: u16,
    max_block_size: u16,
}

impl StreamInfo {
    /// State the stream's block sizes.
    ///
    /// # Errors
    /// When either value is not a legal block size.
    pub fn set_block_sizes(
        &mut self,
        min_value: usize,
        max_value: usize,
    ) -> Result<(), VerifyError> {
        self.min_block_size = u16::try_from(min_value)
            .ok()
            .filter(|&n| n >= 16)
            .ok_or_else(|| VerifyError::new("min_block_size", "must be a valid block size."))?;
        self.max_block_size = u16::try_from(max_value)
            .ok()
            .filter(|&n| n >= 16)
            .ok_or_else(|| VerifyError::new("max_block_size", "must be a valid block size."))?;
        Ok(())
    }

    /// Smallest block size stated by the stream.
    pub fn min_block_size(&self) -> usize {
        usize::from(self.min_block_size)
    }

    /// Largest block size stated by the stream.
    pub fn max_block_size(&self) -> usize {
        usize::from(self.max_block_size)
    }
}

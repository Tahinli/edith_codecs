//! Compatibility shim: the `symphonia-core` 0.6 surface edith consumes, over
//! [`ec_core`] and (through the `symphonia` shim) [`ec_probe`].
//!
//! This is the vocabulary half — parameters, packets, units, the reader and
//! decoder traits. The registries that build them live in the `symphonia`
//! shim, and the AAC decoder seat in `symphonia-codec-aac`, exactly as the
//! incumbent splits them.
//!
//! Written from the signatures edith calls (`crates/engine/src/audio.rs`
//! lines 36-53 and their call sites) and the crate's published documentation.
//! No symphonia source was read: the incumbent is MPL-2.0 and this family is
//! MIT OR Apache-2.0.
//!
//! **Not the whole crate.** What edith does not call is not here: metadata
//! revisions, the sample-buffer zoo, `Signal`/`AudioBuffer` generics, the
//! `CodecRegistry` builder API. Each is a small addition when something needs
//! it; none is a silent omission.

#![forbid(unsafe_code)]

pub mod codecs;
pub mod formats;
pub mod io;
pub mod meta;
pub mod packet;
pub mod units;

use std::fmt;

/// The incumbent's error type, in the shape callers match on.
#[derive(Debug)]
pub enum Error {
    /// Underlying I/O failure.
    IoError(std::io::Error),
    /// The bitstream is malformed.
    DecodeError(String),
    /// A capability this build does not have, naming what and why.
    Unsupported(String),
    /// A seek that could not be served.
    SeekError(String),
    /// End of stream.
    ResetRequired,
}

/// `Result` with this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::IoError(e) => write!(f, "io error: {e}"),
            Error::DecodeError(e) => write!(f, "decode error: {e}"),
            Error::Unsupported(e) => write!(f, "unsupported: {e}"),
            Error::SeekError(e) => write!(f, "seek error: {e}"),
            Error::ResetRequired => write!(f, "decoder reset required"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::IoError(e)
    }
}

impl From<ec_core::Error> for Error {
    fn from(e: ec_core::Error) -> Error {
        match e {
            ec_core::Error::Io(io) => Error::IoError(io),
            ec_core::Error::Eof | ec_core::Error::NeedMore => Error::ResetRequired,
            ec_core::Error::Unsupported { .. } => Error::Unsupported(e.to_string()),
            ec_core::Error::Corrupt { .. } => Error::DecodeError(e.to_string()),
        }
    }
}

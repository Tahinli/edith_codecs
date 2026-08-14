//! Family-wide error type.

use std::fmt;

/// Result alias used by every crate in the family.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong in a demuxer, decoder, encoder or muxer.
///
/// The taxonomy is deliberately small: callers branch on "give me more data"
/// (`NeedMore`), "the stream ended" (`Eof`), "this input is broken" (`Corrupt`)
/// and "we do not implement this" (`Unsupported`). Every `Unsupported` carries
/// both *what* was refused and *why* — a refusal string without a reason is a
/// bug report waiting to happen, not a diagnosis.
#[derive(Debug)]
pub enum Error {
    /// Not enough data buffered yet; feed more and retry the same call.
    NeedMore,
    /// End of stream reached; no further data will ever arrive.
    Eof,
    /// A capability this build genuinely does not have.
    Unsupported {
        /// The construct that was refused, e.g. "HE-AAC SBR".
        what: String,
        /// Why it is refused, e.g. "SBR resampler not implemented".
        why: String,
    },
    /// The bitstream violates its own format rules.
    Corrupt {
        /// Where and how, e.g. "H.264 SPS: log2_max_frame_num_minus4 = 13".
        context: String,
    },
    /// Underlying I/O failure.
    Io(std::io::Error),
}

impl Error {
    /// Build an [`Error::Unsupported`] from any two string-likes.
    pub fn unsupported(what: impl Into<String>, why: impl Into<String>) -> Self {
        Error::Unsupported {
            what: what.into(),
            why: why.into(),
        }
    }

    /// Build an [`Error::Corrupt`] from any string-like.
    pub fn corrupt(context: impl Into<String>) -> Self {
        Error::Corrupt {
            context: context.into(),
        }
    }

    /// True for [`Error::NeedMore`] — the streaming "try again later" contract.
    pub fn is_need_more(&self) -> bool {
        matches!(self, Error::NeedMore)
    }

    /// True for [`Error::Eof`].
    pub fn is_eof(&self) -> bool {
        matches!(self, Error::Eof)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NeedMore => write!(f, "need more data"),
            Error::Eof => write!(f, "end of stream"),
            Error::Unsupported { what, why } => write!(f, "unsupported: {what} ({why})"),
            Error::Corrupt { context } => write!(f, "corrupt bitstream: {context}"),
            Error::Io(e) => write!(f, "io error: {e}"),
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

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

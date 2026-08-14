//! Typed errors. Nothing in this crate panics on a driver or bitstream fault.
//!
//! That is a hard requirement rather than a style preference: edith loads the
//! VA-API path as a `dlopen`ed plugin behind `catch_unwind`, and its
//! fall-back-to-software guarantee is only as good as this crate's promise to
//! return instead of unwinding. Every `VAStatus` becomes an [`Error::Va`],
//! every malformed bitstream an [`Error::Stream`], and every capability this
//! GPU does not have an [`Error::Unsupported`] naming what and why.

use std::fmt;

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong on the hardware path.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A libva call failed, or the driver contradicted its own API.
    Va(ec_va::Error),
    /// The bitstream could not be parsed.
    Stream(ec_core::Error),
    /// A capability this GPU or this build genuinely does not have.
    Unsupported {
        /// What was asked for, e.g. `"VP9 Profile 1 decode"`.
        what: String,
        /// Why it cannot be served, e.g. `"driver reports no VLD entrypoint"`.
        why: String,
    },
    /// The caller asked for something inconsistent, e.g. feeding H.264 to a
    /// decoder configured for AV1, or encoding a frame of the wrong size.
    Config(String),
}

impl Error {
    /// Build an [`Error::Unsupported`].
    pub fn unsupported(what: impl Into<String>, why: impl Into<String>) -> Error {
        Error::Unsupported {
            what: what.into(),
            why: why.into(),
        }
    }

    /// Build an [`Error::Config`].
    pub fn config(what: impl Into<String>) -> Error {
        Error::Config(what.into())
    }

    /// The raw `VAStatus`, when this came from a libva call.
    pub fn va_status(&self) -> Option<i32> {
        match self {
            Error::Va(e) => e.status(),
            _ => None,
        }
    }
}

impl From<ec_va::Error> for Error {
    fn from(e: ec_va::Error) -> Error {
        Error::Va(e)
    }
}

impl From<ec_core::Error> for Error {
    fn from(e: ec_core::Error) -> Error {
        Error::Stream(e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Va(e) => write!(f, "VA-API: {e}"),
            Error::Stream(e) => write!(f, "bitstream: {e}"),
            Error::Unsupported { what, why } => write!(f, "{what} is not supported: {why}"),
            Error::Config(what) => write!(f, "{what}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Va(e) => Some(e),
            Error::Stream(e) => Some(e),
            _ => None,
        }
    }
}

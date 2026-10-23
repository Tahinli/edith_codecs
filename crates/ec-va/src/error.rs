//! Typed errors. Every libva failure becomes an [`Error::Va`] carrying the
//! failing entry point, the raw status and libva's own description of it.

use std::fmt;

use cros_libva::VaError;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything this crate can fail with.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A libva entry point returned something other than `VA_STATUS_SUCCESS`.
    Va {
        /// The libva entry point that failed, e.g. `"vaBeginPicture"`.
        op: &'static str,
        /// The raw `VAStatus`.
        status: i32,
        /// libva's description of `status`.
        message: String,
    },
    /// A DRM render node could not be opened as a VA display.
    Drm {
        /// The node that was tried.
        path: String,
        /// Why it failed.
        message: String,
    },
    /// No usable `/dev/dri/renderD*` VA display exists.
    NoDevice,
    /// libva returned a value this crate cannot interpret.
    Protocol(String),
}

impl Error {
    /// Wrap a cros-libva error with the libva entry point it came from.
    pub(crate) fn va(op: &'static str, e: &VaError) -> Self {
        Error::Va {
            op,
            status: e.va_status(),
            message: e.to_string(),
        }
    }

    /// The raw `VAStatus`, if this error came from libva.
    pub fn status(&self) -> Option<i32> {
        match self {
            Error::Va { status, .. } => Some(*status),
            _ => None,
        }
    }
}

/// Turn a cros-libva result into ours, tagging the entry point.
pub fn va_ok<T>(op: &'static str, r: std::result::Result<T, VaError>) -> Result<T> {
    r.map_err(|e| Error::va(op, &e))
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Va {
                op,
                status,
                message,
            } => write!(f, "{op} failed: {message} (VAStatus 0x{status:08x})"),
            Error::Drm { path, message } => write!(f, "opening DRM node {path}: {message}"),
            Error::NoDevice => write!(f, "no usable /dev/dri/renderD* VA display"),
            Error::Protocol(what) => write!(f, "libva returned an uninterpretable value: {what}"),
        }
    }
}

impl std::error::Error for Error {}

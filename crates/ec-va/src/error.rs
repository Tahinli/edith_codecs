//! Typed errors. Every `VAStatus` that is not `VA_STATUS_SUCCESS` becomes an
//! [`Error::Va`] carrying the failing entry point, the raw status and libva's
//! own description of it — a bare status number in a log is a bug report
//! waiting to happen.

use std::ffi::CStr;
use std::fmt;

use crate::sys;

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong between here and the driver.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A libva entry point returned a failing `VAStatus`.
    Va {
        /// The libva function that failed, e.g. `"vaCreateConfig"`.
        op: &'static str,
        /// The raw `VAStatus`.
        status: sys::VAStatus,
        /// `vaErrorStr(status)`.
        message: String,
    },
    /// Opening a DRM render node failed.
    Drm {
        /// The device path that was tried.
        path: String,
        /// The underlying `open(2)` failure.
        source: std::io::Error,
    },
    /// No `/dev/dri/renderD*` node could be opened as a VA display.
    NoDevice,
    /// The runtime libva is older than the ABI this crate transcribes.
    ///
    /// This is the drift guard: the struct layouts in [`crate::sys`] are
    /// compile-time transcriptions of the 1.23 headers, so refusing to talk to
    /// an older runtime is the only sound answer.
    Version {
        /// Runtime major version reported by `vaInitialize`.
        major: i32,
        /// Runtime minor version reported by `vaInitialize`.
        minor: i32,
    },
    /// The driver returned something this FFI cannot interpret.
    ///
    /// Not a bitstream error and not a status code — a shape violation, e.g. a
    /// surface attribute whose value type contradicts its documented type.
    Protocol(String),
}

impl Error {
    /// Build an [`Error::Va`] for `op`, looking the status string up in libva.
    pub(crate) fn va(op: &'static str, status: sys::VAStatus) -> Self {
        Error::Va {
            op,
            status,
            message: status_str(status),
        }
    }

    /// The raw `VAStatus`, when this error came from a libva call.
    pub fn status(&self) -> Option<sys::VAStatus> {
        match self {
            Error::Va { status, .. } => Some(*status),
            _ => None,
        }
    }
}

/// `vaErrorStr` as an owned Rust string.
pub(crate) fn status_str(status: sys::VAStatus) -> String {
    // SAFETY: `vaErrorStr` is documented (va.h:401) to return a pointer to a
    // static, NUL-terminated english description for any input value —
    // unknown codes map to a fallback string rather than NULL. It reads no
    // global state we mutate, so it is safe to call at any time, including
    // before vaInitialize. The pointer is static, so building a CStr from it
    // and copying is sound; the copy outlives the borrow.
    let ptr = unsafe { sys::vaErrorStr(status) };
    if ptr.is_null() {
        return format!("VAStatus 0x{status:08x}");
    }
    // SAFETY: non-NULL and NUL-terminated per the contract above.
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

/// Turn a `VAStatus` into a `Result`, tagging it with the entry point name.
pub(crate) fn check(op: &'static str, status: sys::VAStatus) -> Result<()> {
    if status == sys::VA_STATUS_SUCCESS {
        Ok(())
    } else {
        Err(Error::va(op, status))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Va {
                op,
                status,
                message,
            } => write!(f, "{op} failed: {message} (VAStatus 0x{status:08x})"),
            Error::Drm { path, source } => write!(f, "opening DRM node {path}: {source}"),
            Error::NoDevice => write!(f, "no usable /dev/dri/renderD* VA display"),
            Error::Version { major, minor } => write!(
                f,
                "libva {major}.{minor} is older than the {}.{} ABI this build transcribes",
                crate::MIN_VA_MAJOR,
                crate::MIN_VA_MINOR
            ),
            Error::Protocol(what) => write!(f, "libva returned an uninterpretable value: {what}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Drm { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_strings_come_from_libva() {
        assert_eq!(status_str(sys::VA_STATUS_SUCCESS), "success (no error)");
        // Any unknown code must still produce a string, never a panic or NULL deref.
        assert!(!status_str(0x7fff_0000).is_empty());
    }

    #[test]
    fn check_maps_success_and_failure() {
        assert!(check("vaFake", sys::VA_STATUS_SUCCESS).is_ok());
        let err = check("vaFake", sys::VA_STATUS_ERROR_UNIMPLEMENTED).unwrap_err();
        assert_eq!(err.status(), Some(sys::VA_STATUS_ERROR_UNIMPLEMENTED));
        assert!(err.to_string().contains("vaFake"));
    }
}

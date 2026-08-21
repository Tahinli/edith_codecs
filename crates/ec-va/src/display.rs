//! [`Display`] — an initialized `VADisplay` on a DRM render node.

use std::ffi::CStr;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{Error, Result, check};
use crate::sys;

/// An initialized VA display, owning both the DRM file descriptor and the
/// `VADisplay` derived from it.
///
/// Everything else in this crate holds an `Arc<Display>`, so the display is
/// terminated only after the last surface, config, context and buffer is gone —
/// the ordering libva requires, enforced by the type system rather than by
/// documentation.
pub struct Display {
    /// Raw `VADisplay`. Valid until [`Drop`] calls `vaTerminate`.
    handle: sys::VADisplay,
    /// The render node. Must outlive `handle`: libva does not dup the fd.
    node: File,
    path: PathBuf,
    version: (i32, i32),
}

// SAFETY: libva's dispatch layer (`va.c`) and the Mesa VA driver both take an
// internal lock per display around every entry point used here, so a
// `VADisplay` may be used from any thread and from several threads at once.
// The `File` and `PathBuf` fields are themselves `Send + Sync`. This mirrors
// what every VA-API consumer (the oracle, GStreamer, cros-libva) relies on.
//
// Note the narrower claim this crate actually needs: `&Display` is only ever
// used to *call* libva, never to hand out interior pointers.
unsafe impl Send for Display {}
// SAFETY: see above.
unsafe impl Sync for Display {}

impl Display {
    /// Open the first usable DRM render node (`/dev/dri/renderD128` upwards).
    ///
    /// Render nodes are used rather than card nodes: they need no DRM master
    /// and no display server, which is what a headless encode/decode pipeline
    /// wants.
    pub fn open() -> Result<Arc<Display>> {
        let mut last: Option<Error> = None;
        // 128..=143 is the kernel's render-node numbering (16 GPUs is plenty).
        for n in 128..=143u32 {
            let path = PathBuf::from(format!("/dev/dri/renderD{n}"));
            if !path.exists() {
                continue;
            }
            match Display::open_path(&path) {
                Ok(display) => return Ok(display),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or(Error::NoDevice))
    }

    /// Open a specific DRM node, e.g. `/dev/dri/renderD128`.
    ///
    /// The node is opened **read-write**, and that is not a detail: with a
    /// read-only fd radeonsi 26.1 fails `amdgpu_bo_cpu_map` and then segfaults
    /// *inside* `vaInitialize`, before it can return a status this crate could
    /// turn into an error. Verified on this machine (gfx1200, Mesa 26.1.6).
    pub fn open_path(path: &Path) -> Result<Arc<Display>> {
        let node = File::options()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| Error::Drm {
                path: path.display().to_string(),
                source,
            })?;

        // SAFETY: `node` is an open DRM fd that lives as long as the returned
        // Display (it is moved into it below, and closed only after
        // vaTerminate in Drop). vaGetDisplayDRM does not take ownership of the
        // fd and returns NULL on failure, which is checked.
        let handle = unsafe { sys::vaGetDisplayDRM(node.as_raw_fd()) };
        if handle.is_null() {
            return Err(Error::va(
                "vaGetDisplayDRM",
                sys::VA_STATUS_ERROR_OPERATION_FAILED,
            ));
        }

        let mut major: i32 = 0;
        let mut minor: i32 = 0;
        // SAFETY: `handle` is a non-NULL VADisplay from vaGetDisplayDRM; the
        // two out-parameters are valid, aligned, initialized `int`s that
        // outlive the call.
        let status = unsafe { sys::vaInitialize(handle, &mut major, &mut minor) };
        if let Err(e) = check("vaInitialize", status) {
            // libva allocates display state inside vaGetDisplayDRM, so a failed
            // initialize still has to be terminated or that state leaks. This
            // is what the oracle's vaapi_device_create does on its failure path.
            //
            // SAFETY: `handle` is still a valid (if uninitialized) VADisplay;
            // vaTerminate accepts exactly this state.
            unsafe { sys::vaTerminate(handle) };
            return Err(e);
        }

        // Runtime drift guard. The struct layouts in `sys` are transcribed from
        // the 1.23 headers; talking to an older runtime would be guesswork.
        if major != crate::MIN_VA_MAJOR || minor < crate::MIN_VA_MINOR {
            // SAFETY: initialize succeeded, so terminating is required and valid.
            unsafe { sys::vaTerminate(handle) };
            return Err(Error::Version { major, minor });
        }

        Ok(Arc::new(Display {
            handle,
            node,
            path: path.to_path_buf(),
            version: (major, minor),
        }))
    }

    /// The raw handle, for the rest of the crate's FFI calls.
    pub(crate) fn handle(&self) -> sys::VADisplay {
        self.handle
    }

    /// Runtime libva version as `(major, minor)`, from `vaInitialize`.
    pub fn version(&self) -> (i32, i32) {
        self.version
    }

    /// The DRM node this display was opened on.
    pub fn device_path(&self) -> &Path {
        &self.path
    }

    /// The raw DRM render-node fd. Borrowed, never closed by the caller.
    pub fn drm_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        // `File` guarantees the fd is open for as long as `self` lives.
        std::os::fd::AsFd::as_fd(&self.node)
    }

    /// Driver vendor string, e.g. `"Mesa Gallium driver 26.1.6 for AMD ..."`.
    pub fn vendor(&self) -> Result<String> {
        // SAFETY: `self.handle` is initialized and valid for the lifetime of
        // `self`. The returned pointer is owned by the driver and stays valid
        // until vaTerminate, i.e. strictly longer than this borrow; the string
        // is copied before returning.
        let ptr = unsafe { sys::vaQueryVendorString(self.handle) };
        if ptr.is_null() {
            return Err(Error::Protocol(
                "vaQueryVendorString returned NULL".to_string(),
            ));
        }
        // SAFETY: non-NULL, NUL-terminated, driver-owned static string.
        Ok(unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned())
    }

    /// `vaDisplayIsValid`, mostly useful in tests.
    pub fn is_valid(&self) -> bool {
        // SAFETY: `self.handle` is a valid VADisplay for the lifetime of `self`.
        unsafe { sys::vaDisplayIsValid(self.handle) != 0 }
    }
}

impl std::fmt::Debug for Display {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Display")
            .field("path", &self.path)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        // SAFETY: `self.handle` was initialized in `open_path` and no child
        // object can still be alive: every one of them holds an `Arc<Display>`,
        // so this runs only after the last of them was dropped. `self.node` is
        // closed after this, as libva requires.
        unsafe { sys::vaTerminate(self.handle) };
    }
}

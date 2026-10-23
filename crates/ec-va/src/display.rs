//! Opening a VA display on a DRM render node.

use std::path::Path;
use std::rc::Rc;

use crate::error::{Error, Result};

/// A VA display. An alias of cros-libva's type: displays are shared through
/// `Rc`, and every config, context and surface keeps one alive.
pub type Display = cros_libva::Display;

/// Open the first usable `/dev/dri/renderD*` node.
pub fn open() -> Result<Rc<Display>> {
    Display::open().ok_or(Error::NoDevice)
}

/// Open a specific DRM render node.
pub fn open_path(path: &Path) -> Result<Rc<Display>> {
    Display::open_drm_display(path).map_err(|e| Error::Drm {
        path: path.display().to_string(),
        message: format!("{e:?}"),
    })
}

/// The driver's vendor string (`vaQueryVendorString`).
pub fn vendor(display: &Display) -> Result<String> {
    display
        .query_vendor_string()
        .map_err(|m| Error::Protocol(format!("vaQueryVendorString: {m}")))
}

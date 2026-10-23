//! Bounded header probes: format, size and EXIF orientation from the front of a file.
//!
//! [`probe`] and [`probe_reader`] read no pixels. They look at the signature, the
//! image header and the EXIF orientation, and at no more than [`PROBE_LIMIT`]
//! bytes of the file, so a probe of an arbitrarily large picture costs a bounded
//! read and a bounded allocation (none beyond the window itself).
//!
//! A probe reports what the bytes it saw say. An `eXIf`, `EXIF` or APP1 segment
//! that starts past the window is not seen, and the orientation reads as 1.

use crate::{Info, Result};
use ec_core::Error;
use std::io::Read;

/// The most bytes a probe examines: 1 MiB.
pub const PROBE_LIMIT: usize = 1 << 20;

/// Format, dimensions and orientation from the first [`PROBE_LIMIT`] bytes of
/// `prefix`. Bytes past the limit are ignored, not an error.
pub fn probe(prefix: &[u8]) -> Result<Info> {
    crate::info(&prefix[..prefix.len().min(PROBE_LIMIT)])
}

/// Format, dimensions and orientation from a reader. At most [`PROBE_LIMIT`]
/// bytes are read, and the reader is left positioned after them.
pub fn probe_reader<R: Read>(reader: R) -> Result<Info> {
    let mut window = Vec::new();
    reader
        .take(PROBE_LIMIT as u64)
        .read_to_end(&mut window)
        .map_err(Error::Io)?;
    crate::info(&window)
}

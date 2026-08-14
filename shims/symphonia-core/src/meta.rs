//! Metadata options.
//!
//! edith passes `MetadataOptions::default()` and never reads a revision back,
//! so the tags a file carries are read by `ec_probe::Tags` and surfaced there
//! rather than through this type.

/// Options metadata is read with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MetadataOptions {
    /// Read cover art as well as text. Nothing here does.
    pub read_visuals: bool,
}

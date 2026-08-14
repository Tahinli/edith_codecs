//! `flacenc::config`: the encoder configuration tree.
//!
//! The fields the replica reads (`block_size`) and the ones its `Default`
//! walks are kept; the knobs that only steer the incumbent's own search
//! (window functions, per-order caps) are not reproduced, because a value set
//! there would silently do nothing.

use crate::error::{Verify, VerifyError};

/// Encoder configuration — `flacenc::config::Encoder`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Encoder {
    /// Encoder block size.
    pub block_size: usize,
    /// Whether the encoder may use several threads.
    pub multithread: bool,
    /// Stereo coding switches.
    pub stereo_coding: StereoCoding,
    /// Per-channel coding switches.
    pub subframe_coding: SubFrameCoding,
}

impl Default for Encoder {
    fn default() -> Self {
        Encoder {
            block_size: 4096,
            multithread: false,
            stereo_coding: StereoCoding::default(),
            subframe_coding: SubFrameCoding::default(),
        }
    }
}

impl Verify for Encoder {
    fn verify(&self) -> Result<(), VerifyError> {
        if !(16..=65535).contains(&self.block_size) {
            return Err(VerifyError::new(
                "block_size",
                "must be in the range of 16..=65535.",
            ));
        }
        Ok(())
    }
}

/// Stereo coding switches — `flacenc::config::StereoCoding`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct StereoCoding {
    /// Allow left/side coding.
    pub use_leftside: bool,
    /// Allow right/side coding.
    pub use_rightside: bool,
    /// Allow mid/side coding.
    pub use_midside: bool,
}

impl Default for StereoCoding {
    fn default() -> Self {
        StereoCoding {
            use_leftside: true,
            use_rightside: true,
            use_midside: true,
        }
    }
}

/// Per-channel coding switches — `flacenc::config::SubFrameCoding`.
///
/// Verbatim coding is deliberately not switchable: it is the representation
/// that always exists, so turning it off could leave a signal unencodable.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct SubFrameCoding {
    /// Allow constant subframes.
    pub use_constant: bool,
    /// Allow fixed-predictor subframes.
    pub use_fixed: bool,
    /// Allow LPC subframes.
    pub use_lpc: bool,
}

impl Default for SubFrameCoding {
    fn default() -> Self {
        SubFrameCoding {
            use_constant: true,
            use_fixed: true,
            use_lpc: true,
        }
    }
}

//! H.264 (ITU-T Rec. H.264 / ISO 14496-10) bitstream syntax.
//!
//! Parsing only — no reconstruction. Shared by the software decoder
//! (`ec-h264`) and the stateless hardware path, so every struct is a plain
//! value type whose derived geometry (macroblock counts, crop rectangle,
//! bit depths) is computed once at parse time; a decoder feeds these fields
//! straight into flat per-picture arrays without re-deriving anything per
//! macroblock.
//!
//! Layout of the crate follows the spec's own structure:
//! - [`nal`]: Annex B framing + emulation-prevention removal (7.3.1, 7.4.1)
//! - [`sps`]: sequence parameter set incl. VUI and HRD (7.3.2.1, E.1)
//! - [`pps`]: picture parameter set incl. the High-profile tail (7.3.2.2)
//! - [`slice`]: slice header for I/SI/P/SP/B (7.3.3)

#![forbid(unsafe_code)]

pub mod nal;
pub mod pps;
pub mod slice;
pub mod sps;

pub use nal::{AnnexBIter, NalHeader, NalUnitType, unescape_rbsp};
pub use pps::Pps;
pub use slice::{DeblockControl, SliceHeader, SliceType};
pub use sps::{Hrd, ScalingLists, Sps, Vui};

/// True when there is at least one more RBSP syntax element before the
/// `rbsp_stop_one_bit` (spec 7.2 `more_rbsp_data()`).
///
/// `data` is the full unescaped RBSP the reader walks; the stop bit is the
/// last set bit of the last non-zero byte.
pub fn more_rbsp_data(reader: &ec_core::BitReader<'_>, data: &[u8]) -> bool {
    // Find the rbsp_stop_one_bit from the tail.
    let Some(last_nonzero) = data.iter().rposition(|&b| b != 0) else {
        return false; // all zero: corrupt, but by definition nothing left
    };
    let byte = data[last_nonzero];
    let stop_bit_index = last_nonzero as u64 * 8 + (7 - byte.trailing_zeros() as u64);
    reader.bit_position() < stop_bit_index
}

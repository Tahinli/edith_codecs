//! HEVC bitstream syntax: NAL framing, parameter sets, slice headers, SEI.
//!
//! This crate is the half of H.265 that both a software encoder and a *stateless
//! hardware* decoder need, and it is deliberately the same code for both
//! directions: every structure here is written and parsed, so a round trip is a
//! test rather than an article of faith.
//!
//! What that buys, concretely:
//!
//! - The conformance window is a field on [`Sps`], not a caller's padding
//!   problem. 1920x1080 in 64x64 coding tree blocks is coded 1920x1088 and
//!   cropped by the SPS; [`Sps::display_size`] gives back what a player shows.
//! - Every field of `VAPictureParameterBufferHEVC` and
//!   `VASliceParameterBufferHEVC` is either a field on [`Sps`], [`Pps`] or
//!   [`SliceHeader`], or a named method on one of them —
//!   [`Sps::pic_width_in_ctbs`], [`ParsePositions::slice_data_byte_offset`],
//!   [`ParsePositions::st_rps_bits`]. A hardware decode path built on this crate
//!   should never need to re-parse a header itself.
//! - [`sei::decoded_picture_hash_rbsp`] writes the MD5 of the reconstruction, so
//!   any conformant decoder can be asked whether the encoder was right.
//!
//! Malformed input is an [`Error`](ec_core::Error), never a panic: truncation is
//! `NeedMore`, a rule violation is `Corrupt`, and the two constructs the family
//! genuinely does not implement (scaling lists, and short-term reference picture
//! sets predicted from an SPS set that a caller did not keep) are `Unsupported`
//! with the reason stated.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod md5;
pub mod nal;
pub mod ps;
pub mod sei;
pub mod slice;
pub mod vui;

pub use nal::{AnnexBNal, NalHeader, NalUnitType, escape_rbsp, split_annex_b, unescape_rbsp};
pub use ps::{
    ConformanceWindow, MAX_ST_REF_PICS, Pps, ProfileTierLevel, ShortTermRefPicSet, Sps, Vps,
};
pub use slice::{
    LongTermRef, ParsePositions, PredWeightTable, SliceHeader, SliceType, WeightEntry,
    count_emulation_prevention_bytes,
};
pub use vui::{ColourDescription, VideoSignalType, VuiParameters};

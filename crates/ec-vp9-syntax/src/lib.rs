//! VP9 bitstream syntax: the superframe index and the uncompressed frame header.
//!
//! This crate parses exactly the part of VP9 a *stateless* hardware decoder needs
//! handed to it — everything up to, and not including, the compressed header.
//! The compressed header is bool-coded probability update state; a VA-API driver
//! consumes it itself out of the slice data buffer, so re-coding it here would be
//! work nobody reads. What the driver cannot do is tell you where the frames are:
//! [`superframe::split`] unpacks the multi-frame chunks libvpx emits for hidden
//! ALTREFs, and [`Vp9Parser::parse_frame`] turns one such frame into a
//! [`FrameHeader`] whose fields map one-to-one onto
//! `VADecPictureParameterBufferVP9` and `VASegmentParameterVP9`.
//!
//! The parser is stateful because the *format* is: `frame_size_with_refs` takes
//! the frame dimensions from a reference slot, and segmentation data and loop
//! filter deltas persist from frame to frame until a keyframe, an intra-only
//! frame or an error-resilient frame resets them (spec 6.2, "setup past
//! independence"). [`Vp9Parser`] carries that state and applies the reference
//! refresh, so feeding it every frame of a stream in order is all a caller does.
//!
//! Derivations the picture parameter buffer wants but the bitstream does not
//! carry directly are provided next to the fields they come from:
//! [`FrameHeader::segment_qindex`] and [`FrameHeader::segment_dequant`]
//! (spec 8.6.1) and [`FrameHeader::loop_filter_levels`] (spec 8.8.1). Nothing in
//! `VADecPictureParameterBufferVP9` or `VASegmentParameterVP9` is left for the
//! caller to work out except the surface ids, which are a property of its frame
//! pool rather than of the bitstream.
//!
//! Malformed input is an [`Error`], never a panic: truncation is
//! [`Error::NeedMore`], a rule violation is [`Error::Corrupt`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod header;
pub mod quant;
pub mod superframe;

pub use header::{
    ColorSpace, FrameHeader, FrameType, InterpolationFilter, LoopFilterParams, QuantizationParams,
    ReferenceSlot, SegmentDequant, SegmentationParams, TileInfo, Vp9Parser,
};
pub use quant::{ac_q, dc_q};
pub use superframe::split;

/// Reference frame slots in the VP9 decoded picture buffer (spec 6.2).
pub const NUM_REF_FRAMES: usize = 8;
/// Reference frames a single inter frame may name: LAST, GOLDEN, ALTREF.
pub const REFS_PER_FRAME: usize = 3;
/// Number of segments a frame can define (spec 6.2, `segmentation_params`).
pub const MAX_SEGMENTS: usize = 8;
/// Segment feature: alternate quantizer index, signed, 8 bits (spec 6.2).
pub const SEG_LVL_ALT_Q: usize = 0;
/// Segment feature: alternate loop filter level, signed, 6 bits (spec 6.2).
pub const SEG_LVL_ALT_L: usize = 1;
/// Segment feature: reference frame override, unsigned, 2 bits (spec 6.2).
pub const SEG_LVL_REF_FRAME: usize = 2;
/// Segment feature: skip, a flag with no payload bits (spec 6.2).
pub const SEG_LVL_SKIP: usize = 3;
/// Number of per-segment features (spec 6.2).
pub const SEG_LVL_MAX: usize = 4;

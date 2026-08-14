//! Opus decoding (RFC 6716, with the RFC 8251 corrections).
//!
//! Opus is two codecs behind one entropy coder: a linear-prediction layer
//! (SILK) for speech up to 8 kHz of bandwidth, and an MDCT layer (CELT) for
//! everything above that and for music. A packet's table-of-contents byte picks
//! one of them or both, and this crate decodes all three cases plus the
//! multistream framing that carries 5.1 and 7.1.
//!
//! Contracts worth knowing before implementing against this crate:
//!
//! - **The MDCT is the shared one.** CELT synthesis runs through
//!   [`ec_dsp::Mdct`], never a direct-form DFT. An incumbent Opus decoder in
//!   this product shipped its FFT behind a default-off feature and ran at 0.95x
//!   realtime on 5.1; there is no such switch here, and no code path that could
//!   answer to one.
//! - **Malformed input is an error, never a panic.** Framing rules [R1]..[R7]
//!   are checked in [`packet`], and the range decoder feeds zeros past the end
//!   of a truncated frame exactly as Section 4.1.2.1 requires.
//! - **Output is `f32`, interleaved, at the rate you asked for.** 48, 24, 16,
//!   12 and 8 kHz are supported; SILK's internal rate is resampled up and the
//!   sum of the two layers is resampled down once, at the end.
//!
//! No unsafe, no allocation on the per-frame path beyond the decoder's own
//! buffers, no dependencies outside the family.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod packet;
pub mod range;

pub use packet::{Bandwidth, Mode, Packet, Toc};
pub use range::RangeDecoder;

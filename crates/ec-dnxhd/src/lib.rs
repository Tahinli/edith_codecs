//! Avid DNxHD / DNxHR (SMPTE VC-3) video decoder.
//!
//! Intra-only. Covers every DNxHD compression id ffmpeg knows (1235-1260,
//! 8- and 10-bit 4:2:2, progressive and two-field interlaced, the MBAFF 1260
//! layout) and DNxHR LB/SQ/HQ (8-bit 4:2:2), HQX (10/12-bit 4:2:2) and 444
//! (10/12-bit 4:4:4 YCbCr), at any resolution.
//!
//! Refused by name: alpha channel streams, RGB (non-YCbCr) 4:4:4 streams and
//! 4:2:2 streams that set the adaptive-colour-transform flag.
//!
//! Output is [`PixelFormat::I422`](ec_core::frame::PixelFormat), `I210`, `I444` or `I410`.
//! ec-core has no 12-bit pixel formats, so [`DnxhdDecoder::decode`] narrows
//! 12-bit pictures to 10 bits (`(v + 2) >> 2`, clamped); [`DnxhdDecoder::decode_native`]
//! returns the samples at their coded depth.
#![allow(clippy::chunks_exact_to_as_chunks, clippy::needless_range_loop)]
#![forbid(unsafe_code)]
mod bits;
mod codec;
mod decoder;
mod idct;
mod tables;

pub use codec::DnxhdDecoder;
pub use decoder::{Chroma, FrameInfo, Picture};

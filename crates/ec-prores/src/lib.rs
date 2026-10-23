#![allow(clippy::needless_range_loop, clippy::too_many_arguments)]
//! Apple ProRes video decoder (SMPTE RDD 36).
//!
//! Profiles 422 Proxy / LT / Standard / HQ (`apco`, `apcs`, `apcn`, `apch`) decode to
//! [`PixelFormat::I210`](ec_core::frame::PixelFormat::I210); 4444 and 4444 XQ (`ap4h`, `ap4x`)
//! decode to [`PixelFormat::I412`](ec_core::frame::PixelFormat::I412) (the 12-bit 4:4:4 layout
//! ffmpeg also reports). Interlaced frames (two field pictures) come back woven with
//! [`VideoFrame::interlace`](ec_core::frame::VideoFrame) set; slice sizes, both scan orders and
//! custom quantisation matrices are handled. The optional alpha plane is returned beside the
//! frame as a 16-bit [`AlphaPlane`], because `VideoFrame` has no alpha-bearing planar format.
//!
//! The arithmetic is ffmpeg's: its integer simple IDCT (`ff_prores_idct_10` / `_12`) is
//! reproduced including 16-bit intermediate wrap, so output is byte-exact against ffmpeg.
#![forbid(unsafe_code)]

mod codec;
mod encode;
mod frame;
mod header;
mod idct;
mod slice;
mod tables;

pub use codec::ProResDecoder;
pub use encode::{EncoderConfig, Profile, ProResEncoder, nominal_frame_bytes};
pub use frame::{AlphaPlane, DecodedFrame};
pub use header::FrameInfo;

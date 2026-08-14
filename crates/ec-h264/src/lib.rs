//! H.264 software decoder.
//!
//! Current scope: CAVLC-coded I slices (Intra_4x4, Intra_16x16, I_PCM) of
//! 8-bit 4:2:0 progressive streams, with the full in-loop deblocking filter,
//! decoded bit-exactly against the JVT conformance suite. Everything outside
//! that scope returns a named [`Error::Unsupported`] — never wrong output.
//!
//! Implemented from Rec. ITU-T H.264 (V15, 08/2024) only; no third-party
//! decoder source was consulted.
//!
//! Two entry surfaces over one decode core: [`Decoder`] takes NAL units, which
//! is what a bitstream tool has, and [`H264Decoder`] implements
//! [`ec_core::registry::Decoder`] over packets — including `avcC` extradata and
//! length-prefixed NAL units — which is what a demuxer hands over.
//!
//! ```no_run
//! use ec_core::registry::{CodecId, CodecParameters, Decoder};
//! use ec_core::{Packet, TimeBase};
//! use ec_h264::H264Decoder;
//!
//! # fn main() -> ec_core::Result<()> {
//! let annex_b: Vec<u8> = std::fs::read("stream.264")?;
//! let mut decoder = H264Decoder::new(CodecParameters::new(CodecId::H264))?;
//! decoder.send_packet(&Packet::new(0, TimeBase::new(1, 25), annex_b))?;
//! let frame = decoder.receive_frame()?; // the first decoded picture, I420
//! # let _ = frame;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

mod bits;
mod cavlc;
mod codec;
mod deblock;
mod decoder;
mod entropy;
mod pred;
mod tables;
mod transform;

pub use codec::H264Decoder;
pub use decoder::{Decoder, NalOutcome};
pub use ec_core::error::{Error, Result};

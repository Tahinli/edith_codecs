//! H.264 software decoder and encoder.
//!
//! Decoder scope: I, P and B slices under both entropy coders (CAVLC and
//! CABAC) of 8-bit 4:2:0 progressive streams — intra prediction, inter
//! prediction with the full decoded picture buffer of clause 8.2, weighted
//! prediction, and the in-loop deblocking filter — decoded bit-exactly against
//! the JVT conformance suite. Frames come out in display order. Everything
//! outside that scope returns a named [`Error::Unsupported`] — never wrong
//! output.
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
mod cabac;
mod cabac_tables;
mod cavlc;
mod codec;
mod deblock;
mod decoder;
mod dpb;
mod enc;
mod entropy;
mod inter;
mod mv;
mod pred;
mod tables;
mod transform;

pub use codec::H264Decoder;
pub use decoder::{Decoder, NalOutcome, OutputOrder};
pub use ec_core::error::{Error, Result};
pub use enc::{EncodedPicture, Encoder, EncoderConfig, PictureView, Preset};

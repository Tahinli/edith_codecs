//! The intermediate representation every edith_codecs crate is built on.
//!
//! One vocabulary spans demux, decode, encode and mux: a [`Packet`] of
//! compressed bytes, a [`Frame`] of decoded samples, a [`CodecId`] naming what
//! turns one into the other, [`CodecParameters`] describing a stream before its
//! first packet, and a rational [`TimeBase`] that keeps `24000/1001` exact from
//! the container to the muxer. [`bitio`] holds the one bit reader/writer pair
//! the parsers share, and [`color`] the H.273 meaning behind [`ColorInfo`]'s raw
//! code points — including the HDR mastering/content-light a tone map needs.
//!
//! Contracts worth knowing before implementing against this crate:
//!
//! - Timing is integer. `f64` appears only in `as_secs_f64` helpers, for
//!   display, and never flows back into a timestamp.
//! - Truncated input is [`Error::NeedMore`], not a panic; a refusal is
//!   [`Error::Unsupported`] and always names *what* and *why*.
//! - Codecs are push/pull ([`Decoder::send_packet`] / [`Decoder::receive_frame`]),
//!   because packets and frames do not correspond one to one.
//! - Payloads are [`Buf`], a refcounted byte range: cloning a packet or sharing
//!   a plane is a refcount bump, not a copy.
//!
//! No async, no unsafe, no external dependencies.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod bitio;
pub mod color;
pub mod error;
pub mod frame;
pub mod packet;
pub mod registry;
pub mod timebase;

pub use bitio::{BitReader, BitReaderLsb, BitWriter, BitWriterLsb};
pub use color::{
    ColorDescription, ContentLight, Matrix, Primaries, Tags, Transfer, bitstream_tags,
    hevc_sei_light,
};
pub use error::{Error, Result};
pub use frame::{
    AudioFrame, ChannelLayout, ChannelPosition, ColorInfo, Frame, PixelFormat, Plane, SampleFormat,
    VideoFrame,
};
pub use packet::{Buf, Packet, PacketFlags, SideData};
pub use registry::{
    AudioParameters, CodecId, CodecParameters, Decoder, Demuxer, Encoder, MediaParameters,
    MediaType, Muxer, SeekMode, StreamInfo, VideoParameters,
};
pub use timebase::{Rounding, TimeBase, Timestamp};

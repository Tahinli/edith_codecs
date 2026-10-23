//! Stateless VA-API video decoding and encoding.
//!
//! "Stateless" is the VA-API decode model, not a description of this crate: the
//! driver decodes one picture at a time and knows nothing between pictures, so
//! everything a codec spreads across frames — the decoded picture buffer,
//! reference lists, picture order counts, reference slot updates — is kept
//! here, in Rust, and handed to the driver per picture. That is the whole job.
//!
//! ```no_run
//! use ec_hw::{Codec, Decoder};
//!
//! let display = ec_va::open()?;
//! let mut decoder = Decoder::new(&display, Codec::H264)?;
//! # let annex_b_access_unit: &[u8] = &[];
//! decoder.decode(annex_b_access_unit, 0)?;
//! while let Some(frame) = decoder.next_frame() {
//!     let planes = frame.to_i420()?;   // or frame.export_prime() for zero copy
//! }
//! # Ok::<(), ec_hw::Error>(())
//! ```
//! # Bit depth
//!
//! [`Frame::to_i420`] truncates a 10-bit (P010) surface to 8 bits, because a
//! byte per sample is what the software path speaks. A 10-bit consumer must
//! call [`Frame::to_i420_16`] instead — that is the lossless path that keeps
//! all ten bits.
//!
//! Encoding is 10-bit too, for AV1: `EncoderConfig::ten_bit()` allocates P010
//! surfaces and takes its pixels through [`Encoder::encode_16`] (or straight
//! from a 10-bit [`Frame`], with no conversion at all). H.264 and H.265 stay
//! 8-bit here, because High and Main — the profiles this crate configures —
//! are 8-bit profiles.
//!
//! # Shape
//!
//! * [`params`] — the codec parameter buffers, converted to cros-libva's typed
//!   buffers.
//! * [`dec`] — one stateless decoder per codec behind [`Decoder`]. H.264 is
//!   ported; HEVC, VP9 and AV1 return [`Error::Unsupported`] for now.
//! * [`enc`] — H.264, HEVC and AV1 encoders. Not ported yet: they return
//!   [`Error::Unsupported`].
//! * [`SurfacePool`] / [`Frame`] — surface recycling and pixel access.
//!
//! # Unsafe
//!
//! None. Every libva call goes through `ec-va`, which goes through cros-libva's
//! safe wrappers, so this crate is `forbid(unsafe_code)`.
//!
//! # Panics
//!
//! None by contract. Every driver and bitstream fault is an [`Error`], because
//! edith loads this path as a `dlopen`ed plugin whose fall back to software is
//! only as good as that promise.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod dec;
pub mod enc;
pub mod error;
pub mod frame;
pub mod params;
pub mod pool;
pub mod reset;

pub use dec::{Codec, Decoder, StreamInfo};
pub use enc::{
    CodedFrame, EncCodec, Encoder, EncoderConfig, FrameMetadata, H264Profile, RateControlMode,
    Tunings,
};
pub use error::{Error, Result};
pub use frame::{Colour, Frame, I420, I420_16, i420_to_nv12, nv12_to_i420};
pub use pool::{PooledSurface, SurfacePool};
pub use reset::{gpu_recently_reset, gpu_reset_epoch, report_gpu_reset};

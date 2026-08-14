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
//! let display = ec_va::Display::open()?;
//! let mut decoder = Decoder::new(&display, Codec::H264)?;
//! decoder.decode(annex_b_access_unit, 0)?;
//! while let Some(frame) = decoder.next_frame() {
//!     let planes = frame.to_i420()?;   // or frame.export_prime() for zero copy
//! }
//! # Ok::<(), ec_hw::Error>(())
//! # fn annex_b_access_unit() -> &'static [u8] { &[] }
//! ```
//!
//! # Shape
//!
//! * [`params`] — the `#[repr(C)]` codec parameter buffers, each checked
//!   against the system headers by a `const` assertion.
//! * [`dec`] — one stateless decoder per codec behind [`Decoder`].
//! * [`enc`] — H.264 and HEVC encoders, and an opt-in AV1 one.
//! * [`SurfacePool`] / [`Frame`] — surface recycling and pixel access.
//!
//! # Unsafe
//!
//! This crate is not `forbid(unsafe_code)`: handing a parameter struct to the
//! driver and walking a coded-buffer segment list are both pointer work that
//! `ec-va` cannot type for us. Both are confined to two functions —
//! [`params::param_buffer`] and `enc::coded_bytes` — and everything else,
//! including every codec's parameter derivation, is safe Rust.
//!
//! # Panics
//!
//! None by contract. Every driver and bitstream fault is an [`Error`], because
//! edith loads this path as a `dlopen`ed plugin whose fall back to software is
//! only as good as that promise.

#![deny(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod dec;
pub mod enc;
pub mod error;
pub mod frame;
pub mod params;
pub mod pool;

pub use dec::{Codec, Decoder, StreamInfo};
pub use enc::{
    CodedFrame, EncCodec, Encoder, EncoderConfig, FrameMetadata, RateControlMode, Tunings,
};
pub use error::{Error, Result};
pub use frame::{Frame, I420, I420_16, i420_to_nv12, nv12_to_i420};
pub use pool::{PooledSurface, SurfacePool};

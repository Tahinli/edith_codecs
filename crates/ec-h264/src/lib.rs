//! H.264 software decoder.
//!
//! Current scope: CAVLC-coded I slices (Intra_4x4, Intra_16x16, I_PCM) of
//! 8-bit 4:2:0 progressive streams, with the full in-loop deblocking filter,
//! decoded bit-exactly against the JVT conformance suite. Everything outside
//! that scope returns a named [`Error::Unsupported`] — never wrong output.
//!
//! Implemented from Rec. ITU-T H.264 (V15, 08/2024) only; no third-party
//! decoder source was consulted.

#![forbid(unsafe_code)]

mod bits;
mod cavlc;
mod deblock;
mod decoder;
mod pred;
mod tables;
mod transform;

pub use decoder::{Decoder, NalOutcome};
pub use ec_core::error::{Error, Result};

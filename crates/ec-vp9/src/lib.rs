//! Native VP9 (profile 0, 8-bit 4:2:0 keyframes) decoder.
//!
//! The crate implements the VP9 bitstream specification directly
//! ([spec]); libvpx's output is used only as a test oracle
//! (sample-exactness witnesses against ffmpeg's libvpx-based decoder),
//! never as code linked at runtime. Every module documents the spec
//! section it implements. Inter-frame prediction is refused by name;
//! this lane is keyframes only.
//!
//! [spec]: https://www.webmproject.org/docs/vp9/
//!
//! # Unsafe
//!
//! This crate contains no `unsafe` code.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod bool;
pub mod decode;
pub mod header;
pub mod intra;
pub mod loopfilter;
pub mod modes;
pub mod stream;
pub mod tables;
pub mod tokens;
pub mod transform;

pub use ec_core::{Error, Result};

/// Whether `EC_VP9_TRACE` decision tracing is enabled, resolved once per
/// process. An `var_os` per bool read would lock std's ENV lock on every
/// coefficient; the hot path is a single relaxed atomic load.
pub(crate) fn trace_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        0 => {
            let on = std::env::var_os("EC_VP9_TRACE").is_some();
            STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
        1 => false,
        _ => true,
    }
}

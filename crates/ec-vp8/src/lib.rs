//! Native VP8 (RFC 6386) decoder — the future replacement for edith's
//! runtime-dlopen'd libvpx seat.
//!
//! The crate is written from the specification ([RFC 6386]) alone; libvpx's
//! output is used only as a test oracle (sample-exactness witnesses), never
//! as code to port. Every module documents the spec section it implements.
//!
//! [RFC 6386]: https://www.rfc-editor.org/rfc/rfc6386
//!
//! # Unsafe
//!
//! This crate contains no `unsafe` code.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod bool;
pub mod decode;
pub mod frame;
pub mod header;
pub mod intra;
pub mod loopfilter;
mod mc;
pub mod modes;
pub mod stream;
pub mod tables;
pub mod tokens;
pub mod transform;

pub use ec_core::{Error, Result};
pub use header::PersistedState;

/// Whether `EC_VP8_TRACE` decision tracing is enabled, resolved once per
/// process. A `var_os` per bool read was 46% of the decode profile (each
/// call locks std's ENV lock and allocates); `OnceLock::get_or_init` was
/// no better, so the hot path is a single relaxed atomic load.
pub(crate) fn trace_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        0 => {
            let on = std::env::var_os("EC_VP8_TRACE").is_some();
            STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
        1 => false,
        _ => true,
    }
}

/// Cached AVX2 availability for the explicit-SIMD kernels (`mc`).
#[cfg(target_arch = "x86_64")]
pub(crate) fn avx2_supported() -> bool {
    static AVX2: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| std::arch::is_x86_feature_detected!("avx2"));
    *AVX2
}

/// Whether `EC_VP8_DEBUG` per-macroblock dumps are enabled (same shape as
/// [`trace_enabled`]; the check sits in the per-MB reconstruct path).
pub(crate) fn debug_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        0 => {
            let on = std::env::var_os("EC_VP8_DEBUG").is_some();
            STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
        1 => false,
        _ => true,
    }
}

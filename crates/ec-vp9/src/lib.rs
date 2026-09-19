//! Native VP9 (profiles 0 and 2, 8- and 10-bit 4:2:0) decoder.
//!
//! The crate implements the VP9 bitstream specification directly
//! ([spec]); libvpx's output is used only as a test oracle
//! (sample-exactness witnesses against ffmpeg's libvpx-based decoder),
//! never as code linked at runtime. Every module documents the spec
//! section it implements. Inter frames decode through motion compensation,
//! residual reconstruction and the loop filter; intra-only frames, profiles
//! 1/3 (4:4:4), 12-bit and other subsamplings are refused by name.
//!
//! Planes are stored as `u16` for every depth ([`Sample`]); kernels take the
//! frame's `bd` and clamp with `clip_pixel_bd`, so one decode walk serves
//! both depths. The only depth-dependent bitstream element is the CAT6
//! coefficient token (14 bits/`vp9_cat6_prob` at 8-bit, 16 bits/
//! `vp9_cat6_prob_high12 + 2` at 10- and 12-bit).
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
pub(crate) mod inter;
pub mod intra;
pub mod loopfilter;
pub(crate) mod mc;
pub mod modes;
pub mod stream;
pub mod tables;
pub mod tokens;
pub mod transform;

pub use ec_core::{Error, Result};

/// A reconstructed sample. Planes are stored as `u16` for EVERY bit depth
/// (8-bit values occupy the low byte), so profile-0 and profile-2 frames share
/// one representation and one decode walk; the kernels take the frame's
/// `bd` and clamp with [`clip_pixel_bd`].
pub type Sample = u16;

/// `clip_pixel_highbd` (`vpx_dsp/vpx_dsp_common.h`): clamp to `[0, (1<<bd)-1]`.
/// For `bd == 8` this is libvpx's `clip_pixel` exactly.
#[inline]
pub(crate) fn clip_pixel_bd(v: i32, bd: u8) -> Sample {
    v.clamp(0, (1i32 << bd) - 1) as Sample
}

/// `ROUND_POWER_OF_TWO(v, n)` (`vpx_dsp/txfm_common.h`, arithmetic shift).
#[inline]
pub(crate) fn round_pow2(v: i32, n: i32) -> i32 {
    (v + (1 << (n - 1))) >> n
}

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

/// Cached `getenv` probe: `EC_VP9_*` gates are consulted per block, per MV
/// component and per switchable-interp read, and `std::env::var_os` locks std's
/// ENV lock, so each gate resolves once per process and the hot path is a
/// single relaxed atomic load (see `trace_enabled`).
macro_rules! cached_gate {
    ($fn_name:ident, $env:literal) => {
        pub(crate) fn $fn_name() -> bool {
            use std::sync::atomic::{AtomicU8, Ordering};
            static STATE: AtomicU8 = AtomicU8::new(0);
            match STATE.load(Ordering::Relaxed) {
                0 => {
                    let on = std::env::var_os($env).is_some();
                    STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
                    on
                }
                1 => false,
                _ => true,
            }
        }
    };
}

// Per-block dump gate for the inter-syntax lane (scratch harnesses);
// a candidate-list dump (`mode == NEWMV` only);
// a search-position cell + grid-store dump; and a post-compressed-header
// MV probability table dump. (Plain comments: a doc comment on a macro
// invocation is an "unused doc comment" warning.)
cached_gate!(interdump_enabled, "EC_VP9_INTERDUMP");
cached_gate!(mvdbg_enabled, "EC_VP9_MVDGB");
cached_gate!(mvdbg2_enabled, "EC_VP9_MVDGB2");
cached_gate!(mvdump_enabled, "EC_VP9_MVDUMP");
// [scratch] per-transform-block token state (the inter pixel lane).
cached_gate!(tokdbg_enabled, "EC_VP9_TOKDGB");
cached_gate!(lfmasksy_enabled, "EC_VP9_LFMASKSY");

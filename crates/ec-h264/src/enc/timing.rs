//! Stage timers for one encoder, read through [`Encoder::timings`]. They cost a
//! clock read per stage per picture, which is noise next to a picture's coding.
//!
//! [`Encoder::timings`]: super::Encoder::timings

/// Wall-clock seconds spent in each stage of `encode`, summed over every
/// picture the encoder has coded.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EncodeTimings {
    /// Pictures counted.
    pub pictures: u64,
    /// Copying and padding the source planes.
    pub pad: f64,
    /// The parallel band coding, wall clock.
    pub bands_wall: f64,
    /// Sum of every band's own coding time (the work the threads share).
    pub bands_busy: f64,
    /// Sum over pictures of the slowest band (the critical path).
    pub bands_slowest: f64,
    /// Stitching the bands into one picture.
    pub stitch: f64,
    /// The in-loop deblocking filter, which runs on one thread.
    pub deblock: f64,
    /// Border extension, the reference swap and assembling the access unit.
    pub output: f64,
}

/// Per-stage nanosecond counters for the macroblock coder, behind the
/// `enc-profile` feature so a normal build pays nothing. Shared atomics: the
/// bands add to them concurrently, which is fine for a profile.
#[cfg(feature = "enc-profile")]
pub mod mb_stages {
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Stage names, in index order.
    pub const NAMES: [&str; 16] = [
        "choose_inter",
        "intra_pre_cost",
        "inter_rd_candidates",
        "intra_rd_total",
        "state_save_load",
        "entropy_clone",
        "intra4x4_rd",
        "intra8x8_rd",
        "intra_bits_and_ssd",
        "(unused)",
        "chroma_mode_rd",
        "intra16_rd",
        "me_seeds",
        "me_int",
        "me_sub",
        "me_chroma_skip",
    ];
    pub(crate) static NANOS: [AtomicU64; 16] = [const { AtomicU64::new(0) }; 16];

    /// Nanoseconds spent in each stage so far, in [`NAMES`] order.
    pub fn snapshot() -> [u64; 16] {
        std::array::from_fn(|i| NANOS[i].load(Ordering::Relaxed))
    }
}

/// Time `$body` into stage `$idx` of [`mb_stages`] when profiling, else just run it.
#[cfg(feature = "enc-profile")]
macro_rules! mb_timed {
    ($idx:expr, $body:expr) => {{
        let t = std::time::Instant::now();
        let r = $body;
        $crate::enc::timing::mb_stages::NANOS[$idx].fetch_add(
            t.elapsed().as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        r
    }};
}
#[cfg(not(feature = "enc-profile"))]
macro_rules! mb_timed {
    ($idx:expr, $body:expr) => {
        $body
    };
}
pub(crate) use mb_timed;

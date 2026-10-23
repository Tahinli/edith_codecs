//! Phase timers for the decoder, enabled by `EC_VP9_PROFILE=1`.
//!
//! Each phase accumulates CPU nanoseconds across all threads (atomic adds), so with
//! tile threads the sums can exceed wall time. Off by default: `start` is then one
//! relaxed atomic load and no clock read.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::time::Instant;

/// Phases, indexes into [`TOTALS`].
#[derive(Clone, Copy)]
pub(crate) enum Phase {
    /// Whole tile walk (parse plus reconstruction).
    Walk = 0,
    /// Inter prediction (motion compensation) for all planes of a block.
    Mc = 1,
    /// Coefficient token decoding.
    Tokens = 2,
    /// Inverse transform and add.
    Idct = 3,
    /// Loop filter over the whole frame.
    LoopFilter = 4,
    /// Tile-thread overhead: worker state setup and copying columns back.
    Threads = 5,
}

/// Names for [`report`], in [`Phase`] order.
pub const NAMES: [&str; 6] = ["walk", "mc", "tokens", "idct", "loopfilter", "threads"];

static TOTALS: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];

/// Whether `EC_VP9_PROFILE` turned the timers on (read once per process).
pub fn enabled() -> bool {
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        0 => {
            let on = std::env::var_os("EC_VP9_PROFILE").is_some_and(|v| v != "0");
            STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
        s => s == 2,
    }
}

/// Start a timer; `None` when profiling is off.
pub(crate) fn start() -> Option<Instant> {
    if enabled() {
        Some(Instant::now())
    } else {
        None
    }
}

/// Add the time since `t` to `phase`.
pub(crate) fn stop(phase: Phase, t: Option<Instant>) {
    if let Some(t) = t {
        TOTALS[phase as usize].fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
}

/// Accumulated seconds per phase, in [`NAMES`] order.
pub fn totals() -> [f64; 6] {
    std::array::from_fn(|i| TOTALS[i].load(Ordering::Relaxed) as f64 * 1e-9)
}

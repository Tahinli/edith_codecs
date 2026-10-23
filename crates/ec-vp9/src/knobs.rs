//! Debug switches the VP9 decoder reads from the environment, with a
//! per-thread override that tests set instead of editing the process
//! environment (which is `unsafe` in Rust 2024).
//!
//! The decoder reads these on the thread that calls `decode`: tile workers
//! and the loop-filter threads never read them. An override set on a test's
//! own thread therefore covers exactly that test's decodes. With no override
//! set, each reader answers from the environment exactly as it always has.

use std::cell::Cell;
use std::thread::LocalKey;

thread_local! {
    static SKIP_LF: Cell<Option<bool>> = const { Cell::new(None) };
    static FORCE_TAIL: Cell<Option<usize>> = const { Cell::new(None) };
}

/// Restores the value a knob had before the guard was created, when dropped.
#[doc(hidden)]
#[must_use = "the override ends as soon as this guard is dropped"]
pub struct Override {
    restore: Option<Box<dyn FnOnce()>>,
}

impl Drop for Override {
    fn drop(&mut self) {
        if let Some(restore) = self.restore.take() {
            restore();
        }
    }
}

fn force<T: Copy + 'static>(slot: &'static LocalKey<Cell<T>>, value: T) -> Override {
    let prev = slot.with(|c| c.replace(value));
    Override {
        restore: Some(Box::new(move || slot.with(|c| c.set(prev)))),
    }
}

/// `EC_VP9_SKIP_LF`: when set, the loop-filter post-pass is skipped.
pub(crate) fn skip_lf() -> bool {
    SKIP_LF
        .with(Cell::get)
        .unwrap_or_else(|| std::env::var_os("EC_VP9_SKIP_LF").is_some())
}

/// `EC_VP9_FORCE_TAIL`: overrides the keyframe tail start offset, if set and
/// parseable.
pub(crate) fn force_tail() -> Option<usize> {
    FORCE_TAIL.with(Cell::get).or_else(|| {
        std::env::var("EC_VP9_FORCE_TAIL")
            .ok()
            .and_then(|s| s.parse().ok())
    })
}

/// Forces `EC_VP9_SKIP_LF` on (`Some(true)`), off (`Some(false)`) or back to
/// the environment (`None`) on this thread, until the guard drops.
#[doc(hidden)]
pub fn set_skip_lf(on: Option<bool>) -> Override {
    force(&SKIP_LF, on)
}

/// Forces `EC_VP9_FORCE_TAIL` to `tail` on this thread, until the guard drops.
/// `None` returns the knob to the environment.
#[doc(hidden)]
pub fn set_force_tail(tail: Option<usize>) -> Override {
    force(&FORCE_TAIL, tail)
}

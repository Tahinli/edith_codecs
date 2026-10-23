//! Test-only fault injection for the threaded decode (lane-av1panichang).
//!
//! A worker calls [`point`] at a named site; once [`arm`]ed, the `nth` time
//! that site is reached it panics, and the plan disarms itself. Disarmed (the
//! default) a point is one relaxed atomic load. Hidden from the docs: it exists
//! so `tests/worker_panic.rs` can prove a panicking worker never hangs the
//! decode and that the next decode in the same process still works.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

static ARMED: AtomicBool = AtomicBool::new(false);
static PLAN: Mutex<Option<(&'static str, usize)>> = Mutex::new(None);

fn plan() -> std::sync::MutexGuard<'static, Option<(&'static str, usize)>> {
    PLAN.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Panics the `nth` (1-based) time a worker reaches `site`, once.
#[doc(hidden)]
pub fn arm(site: &'static str, nth: usize) {
    assert!(nth >= 1, "fault::arm: nth is 1-based");
    *plan() = Some((site, nth));
    ARMED.store(true, Ordering::SeqCst);
}

/// Clears any armed plan.
#[doc(hidden)]
pub fn disarm() {
    *plan() = None;
    ARMED.store(false, Ordering::SeqCst);
}

/// The sites a worker reports: `frame` (inside a frame job's own catch),
/// `frame-after` (past it, unwinding through the job), `recon` (a wavefront
/// job).
pub(crate) fn point(site: &'static str) {
    if !ARMED.load(Ordering::Relaxed) {
        return;
    }
    let fire = {
        let mut p = plan();
        match p.as_mut() {
            Some((s, n)) if *s == site => {
                *n -= 1;
                if *n == 0 {
                    *p = None;
                    ARMED.store(false, Ordering::SeqCst);
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    };
    if fire {
        panic!("injected worker panic at fault site `{site}`");
    }
}

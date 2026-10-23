//! Process-wide GPU reset epoch.
//!
//! When the kernel resets a hung VCN ring, every VA context alive in the
//! process is reset with it, the innocent ones too, and Mesa aborts the whole
//! process on the next submit to any of them. One session noticing (a sync that
//! errors or comes back after a ring reset) therefore has to condemn all the
//! others: each session remembers the epoch it was created in and refuses to
//! submit once the epoch has moved.
//!
//! New sessions are refused during a short grace period after the event, while
//! the ring is still being reset, and work normally afterwards.
//!
//! A DRM `AMDGPU_CTX_OP_QUERY_STATE2` query was considered and rejected: the
//! context id belongs to Mesa's winsys, not to ec-va, so there is nothing to
//! query from outside, and `vaQuerySurfaceStatus` does not report resets.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long after a reset new sessions are refused. The kernel's ring reset
/// takes about 2 s after the 2 s job timeout has fired; the sync that detects
/// it returns after the reset.
pub const GRACE: Duration = Duration::from_secs(3);

/// The epoch counter and the time of the last reset, in ms since `base` plus 1
/// (0 means never).
#[derive(Debug, Default)]
pub(crate) struct ResetClock {
    epoch: AtomicU64,
    last_ms: AtomicU64,
}

impl ResetClock {
    pub(crate) const fn new() -> ResetClock {
        ResetClock {
            epoch: AtomicU64::new(0),
            last_ms: AtomicU64::new(0),
        }
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    pub(crate) fn report(&self, now_ms: u64) {
        self.last_ms.store(now_ms + 1, Ordering::Release);
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn in_grace(&self, now_ms: u64, grace: Duration) -> bool {
        match self.last_ms.load(Ordering::Acquire) {
            0 => false,
            last => u128::from(now_ms.saturating_sub(last - 1)) < grace.as_millis(),
        }
    }
}

/// Hard limit on one wait for a picture; past it the sync gives up.
pub(crate) const SYNC_LIMIT: Duration = Duration::from_secs(8);

/// A wait that took longer than this spent its time in a ring reset: a
/// healthy picture takes milliseconds. 1080p..8K pictures stay far below it.
pub(crate) const SLOW_PICTURE: Duration = Duration::from_millis(1500);

/// Wait for `surface`, and condemn every session if the wait shows that the
/// GPU was reset: an error, or a wait longer than [`SLOW_PICTURE`].
pub(crate) fn sync_watched<D: ec_va::va::SurfaceMemoryDescriptor>(
    surface: &ec_va::va::Surface<D>,
) -> crate::Result<()> {
    use ec_va::SurfaceExt;

    let started = Instant::now();
    let synced = surface.sync_timeout(SYNC_LIMIT);
    let took = started.elapsed();
    match synced {
        Ok(()) if took <= SLOW_PICTURE => Ok(()),
        Ok(()) => {
            report_gpu_reset();
            Err(crate::Error::hw_reset(format!(
                "a picture took {took:?} (limit {SLOW_PICTURE:?}); the GPU ring was probably reset"
            )))
        }
        Err(e) => {
            report_gpu_reset();
            Err(e.into())
        }
    }
}

static CLOCK: ResetClock = ResetClock::new();

fn now_ms() -> u64 {
    static BASE: OnceLock<Instant> = OnceLock::new();
    u64::try_from(BASE.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Record that the GPU was reset (or a context was lost, or a job timed out).
/// Every session created before this call refuses further submits.
pub fn report_gpu_reset() {
    // Make sure BASE is initialised before the first reading is taken.
    CLOCK.report(now_ms());
}

/// The number of resets seen by this process so far.
pub fn gpu_reset_epoch() -> u64 {
    CLOCK.epoch()
}

/// Whether a reset happened within the last [`GRACE`]: a hardware session
/// created now would be reset under the caller, so use software instead.
pub fn gpu_recently_reset() -> bool {
    CLOCK.in_grace(now_ms(), GRACE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_clock_is_calm() {
        let c = ResetClock::new();
        assert_eq!(c.epoch(), 0);
        assert!(!c.in_grace(0, GRACE));
        assert!(!c.in_grace(1_000_000, GRACE));
    }

    #[test]
    fn a_report_moves_the_epoch_and_opens_the_grace_period() {
        let c = ResetClock::new();
        c.report(10_000);
        assert_eq!(c.epoch(), 1);
        assert!(c.in_grace(10_000, GRACE));
        assert!(c.in_grace(12_999, GRACE));
        assert!(!c.in_grace(13_000, GRACE));
    }

    #[test]
    fn a_report_at_time_zero_still_counts() {
        let c = ResetClock::new();
        c.report(0);
        assert!(c.in_grace(0, GRACE));
        assert!(c.in_grace(2_999, GRACE));
        assert!(!c.in_grace(3_000, GRACE));
    }

    #[test]
    fn a_second_report_restarts_the_grace_period() {
        let c = ResetClock::new();
        c.report(1_000);
        c.report(3_500);
        assert_eq!(c.epoch(), 2);
        assert!(c.in_grace(6_000, GRACE));
        assert!(!c.in_grace(6_500, GRACE));
    }

    #[test]
    fn a_session_born_before_the_report_is_stale_one_born_after_is_not() {
        let c = ResetClock::new();
        let old = c.epoch();
        c.report(5);
        let young = c.epoch();
        assert_ne!(c.epoch(), old);
        assert_eq!(c.epoch(), young);
    }
}

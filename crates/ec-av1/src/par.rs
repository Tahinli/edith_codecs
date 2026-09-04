//! lane-filt1: in-frame parallelism for the post-decode filter chain
//! (deblock, CDEF, loop restoration, film grain).
//!
//! Every stage here is split along an axis whose per-band writes are
//! *disjoint* and whose reads come either from an immutable snapshot or from
//! the band's own pixels, so N bands reproduce the single-threaded output
//! byte for byte -- gated by
//! `a_real_stream_filters_identically_with_one_and_four_filter_threads`.
//!
//! Threading is opt-in: [`filter_threads`] is 1 unless `EC_AV1_FILTER_THREADS`
//! says otherwise, and at 1 band nothing is spawned at all (the work runs on
//! the calling thread, so every `thread_local!` gate counter in `decode.rs`
//! still sees it). Above 1, a worker's counter increments are lost -- which is
//! why the default stays 1 and every existing gate is unaffected. The knob
//! composes multiplicatively with `EC_AV1_THREADS` (frame workers), so the
//! product of the two is what must stay inside the core count.
//!
//! Measured on a 12-core box, `seg4k` (272 shown frames, interleaved medians
//! of 3, other lanes decoding alongside): 34.41 s at 1 filter thread, 30.04 s
//! at 2, 28.41 s at 4, 28.36 s at 6, 27.70 s at 8 -- i.e. the filter chain is
//! ~19% of frame time and 4 threads take most of what is there. Recommended:
//! `EC_AV1_FILTER_THREADS=4` with frame threading off, and the product with
//! `EC_AV1_THREADS` kept at or below the core count once it lands (e.g. 2
//! frame workers x 4 filter threads on 12 cores).
#![allow(unsafe_code)]

/// `EC_AV1_FILTER_THREADS`, the number of worker threads each in-frame filter
/// stage splits itself across (default 1 = single-threaded, the shipped
/// behaviour). Read once per process.
/// 0 = "not read yet"; the env read happens once and the value is a plain
/// atomic afterwards, so a test can flip it ([`set_filter_threads`]) without
/// touching the process environment.
static FILTER_THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub(crate) fn filter_threads() -> usize {
    match FILTER_THREADS.load(std::sync::atomic::Ordering::Relaxed) {
        0 => {
            let n = crate::envflags::var("EC_AV1_FILTER_THREADS")
                .ok()
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(1)
                .clamp(1, 64);
            FILTER_THREADS.store(n, std::sync::atomic::Ordering::Relaxed);
            n
        }
        n => n,
    }
}

/// Test-only override of [`filter_threads`]. Process-global, so a gate using
/// it must hold `stream.rs`'s gate-counter lock (which also keeps the
/// counter-delta gates from decoding while the workers are on).
#[cfg(test)]
pub(crate) fn set_filter_threads(n: usize) {
    FILTER_THREADS.store(n.clamp(1, 64), std::sync::atomic::Ordering::Relaxed);
}

/// Splits `units` work units into at most `threads` contiguous, non-empty
/// bands. The caller picks the unit so that a band boundary is always a legal
/// cut of its stage (16-pixel rows for the deblocker, 2 mi for CDEF, one
/// restoration-unit row for LR, one 32-pixel block row for film grain).
pub(crate) fn bands(units: usize, threads: usize) -> Vec<(usize, usize)> {
    if units == 0 {
        return Vec::new();
    }
    let t = threads.clamp(1, units);
    let per = units.div_ceil(t);
    (0..t)
        .map(|i| (i * per, ((i + 1) * per).min(units)))
        .filter(|(a, b)| a < b)
        .collect()
}

/// A `&mut T` handed to several scoped workers that write provably disjoint
/// parts of it.
///
/// corner-cut: the disjointness is proved by the caller's band arithmetic and
/// checked end-to-end by the 1-vs-4-thread byte-exact gate, not by the borrow
/// checker -- so Miri/stacked-borrows would flag the overlapping `&mut`s. The
/// upgrade path is per-stage `chunks_mut`/`split_at_mut` plumbing (an origin
/// offset through `filter_edge`, `cdef_filter_block`,
/// `filter_restoration_unit` and `add_noise_to_block`), which is a far larger
/// diff for the same output.
pub(crate) struct Shared<'a, T: ?Sized>(*mut T, std::marker::PhantomData<&'a mut T>);

// SAFETY: `Shared` only ever hands out the pointer it was built from; sending
// it to a scoped thread is sending the `&mut T` the constructor consumed, and
// the bands each worker touches are disjoint.
unsafe impl<T: ?Sized + Send> Send for Shared<'_, T> {}
unsafe impl<T: ?Sized + Send> Sync for Shared<'_, T> {}

impl<T: ?Sized> Clone for Shared<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: ?Sized> Copy for Shared<'_, T> {}

impl<'a, T: ?Sized> Shared<'a, T> {
    pub(crate) fn new(v: &'a mut T) -> Self {
        Self(v, std::marker::PhantomData)
    }

    /// # Safety
    /// The caller must touch only its own band, disjoint from every other
    /// worker's, for as long as the returned reference lives.
    pub(crate) unsafe fn get(&self) -> &mut T {
        unsafe { &mut *self.0 }
    }
}

/// Runs `f(band_start, band_end, ctx)` once per band: band 0 on this thread
/// with the caller's own [`crate::decode::FrameCtx`], the rest on scoped
/// worker threads, each with its own filter-scope copy of it (`FrameCtx` is
/// `!Sync` by design). Nothing outlives the call.
#[inline]
pub(crate) fn run_bands<F>(bands: &[(usize, usize)], fctx: &crate::decode::FrameCtx, f: F)
where
    F: Fn(usize, usize, &crate::decode::FrameCtx) + Sync,
{
    match bands {
        [] => {}
        [(a, b)] => f(*a, *b, fctx),
        [(a0, b0), rest @ ..] => {
            std::thread::scope(|s| {
                for &(a, b) in rest {
                    let ctx = crate::decode::filter_ctx_copy(fctx);
                    let f = &f;
                    s.spawn(move || f(a, b, &ctx));
                }
                f(*a0, *b0, fctx);
            });
        }
    }
}

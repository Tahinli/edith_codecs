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

    /// [`Shared::new`] from a raw pointer, for a caller that keeps using the
    /// `&mut T` itself (lane-pipefilt: the frame thread parses into the
    /// planes and the mode-info grids while the filter pipeline reads the
    /// superblock rows the parse is already three bands past).
    ///
    /// # Safety
    /// `p` must stay valid and unaliased-in-practice for `'a` -- the same
    /// band-disjointness contract as [`Shared::get`], proved by the caller's
    /// row-lag arithmetic and the byte-exact gate.
    pub(crate) unsafe fn from_ptr(p: *mut T) -> Self {
        Self(p, std::marker::PhantomData)
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
            // lane-pool1: same shape as the `thread::scope` this replaced --
            // band 0 inline, the rest on this thread's persistent pool, all
            // waited out by `batch`'s drop at the end of the block.
            let batch = Batch::new("ec-av1-filter");
            for &(a, b) in rest {
                let ctx = crate::decode::filter_ctx_copy(fctx);
                let f = &f;
                batch.submit(move || f(a, b, &ctx));
            }
            f(*a0, *b0, fctx);
            drop(batch);
        }
    }
}

/// `EC_AV1_RECON_THREADS`, the number of worker threads a tile's
/// reconstruction (lane-wave1) is spread across, in superblock-row wavefront
/// order (default 1 = reconstruct inline at the parse site, i.e. exactly the
/// pre-wavefront decoder). Read once per process, same shape as
/// [`filter_threads`].
static RECON_THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub(crate) fn recon_threads() -> usize {
    match RECON_THREADS.load(std::sync::atomic::Ordering::Relaxed) {
        0 => {
            let n = crate::envflags::var("EC_AV1_RECON_THREADS")
                .ok()
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(1)
                .clamp(1, 64);
            RECON_THREADS.store(n, std::sync::atomic::Ordering::Relaxed);
            n
        }
        n => n,
    }
}

/// Test-only override of [`recon_threads`] -- see [`set_filter_threads`].
#[cfg(test)]
pub(crate) fn set_recon_threads(n: usize) {
    RECON_THREADS.store(n.clamp(1, 64), std::sync::atomic::Ordering::Relaxed);
}

// --- lane-pool1: persistent worker pools ---------------------------------
//
// Every threaded stage of the decoder used to spawn its threads per unit of
// work: one `thread::scope` per filter STAGE per frame, `EC_AV1_RECON_THREADS`
// workers per TILE, one frame worker per coded frame -- 2363 distinct threads
// in a 13 s 4K decode. The cost is not the clone of the thread itself but what
// dies with it: every `thread_local!` scratch pool in `decode.rs` (planes,
// rows, dq, mc, wiener, sgr) is allocated and faulted in again on each new
// thread, which is where the 3.65 M page faults and the 2.7x malloc traffic of
// the composed setting came from.
//
// The replacement is one pool per POOL-OWNING THREAD, kept in a thread-local
// and joined when that thread exits: a stage submits a [`Batch`] of jobs, the
// pool hands each to an idle worker (growing to `busy + queued` so a job never
// queues behind a blocking one -- the recon wavefront's workers block on their
// dependencies), and the batch's `Drop` waits every job out. Scratch then
// lives as long as the worker, not as long as the frame.
type Job = Box<dyn FnOnce() + Send + 'static>;

struct PoolQ {
    jobs: std::collections::VecDeque<Job>,
    /// Jobs handed to a worker and not yet finished.
    busy: usize,
    closed: bool,
    /// lane-memfix: the pool's threads live behind the same lock as its
    /// queue, because the thread that has to GROW the pool is whichever one
    /// submits -- including one of the pool's own workers (see
    /// [`pool_push`]). Joined by [`Pool::drop`] on the owning thread.
    handles: Vec<std::thread::JoinHandle<()>>,
}

struct PoolInner {
    q: std::sync::Mutex<PoolQ>,
    cv: std::sync::Condvar,
}

struct Pool {
    inner: std::sync::Arc<PoolInner>,
}

fn pool_worker(inner: &std::sync::Arc<PoolInner>) {
    // lane-memfix: a job this worker runs submits its own sub-batches to THIS
    // pool rather than opening a second one on the worker's thread-local --
    // see [`Batch::submit`].
    ON_POOL.with(|c| *c.borrow_mut() = Some(std::sync::Arc::clone(inner)));
    let mut q = inner.q.lock().unwrap();
    loop {
        if let Some(job) = q.jobs.pop_front() {
            q.busy += 1;
            drop(q);
            // A job's own panic is captured by the batch wrapper below, so
            // this only guards against a wrapper that itself unwinds.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
            q = inner.q.lock().unwrap();
            q.busy -= 1;
            continue;
        }
        if q.closed {
            POOL_THREADS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        q = inner.cv.wait(q).unwrap();
    }
}

impl Pool {
    fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(PoolInner {
                q: std::sync::Mutex::new(PoolQ {
                    jobs: std::collections::VecDeque::new(),
                    busy: 0,
                    closed: false,
                    handles: Vec::new(),
                }),
                cv: std::sync::Condvar::new(),
            }),
        }
    }
}

/// Queues `job` on `inner` and makes sure a worker is free for it: the pool
/// grows to `busy + queued` threads, so a submitted job always starts, even
/// when every other running job is blocked waiting for it.
/// `class` names the submitting site of the job that grows the pool, so a
/// profiler attributes the thread to the work that created it
/// (`ec-av1-frame`, `ec-av1-recon`, `ec-av1-filter`). One pool serves every
/// class submitted to it, so later classes reuse the threads an earlier one
/// named.
///
/// lane-memfix: the pool only grows for work that is queued and unserved AT
/// THAT MOMENT, and a pool worker submits back into its own pool, so the
/// process-wide thread count is the peak of `busy + queued` over the whole
/// job tree -- not the product of one pool per nesting level (a filter band
/// job used to open a third-level pool on the worker running it, and none of
/// those pools ever shrank: 1038 threads at (16,4,2)).
fn pool_push(inner: &std::sync::Arc<PoolInner>, job: Job, class: &'static str) {
    let mut q = inner.q.lock().unwrap();
    q.jobs.push_back(job);
    let want = q.busy + q.jobs.len();
    while q.handles.len() < want {
        let inner = std::sync::Arc::clone(inner);
        let name = format!("{class}-{}", q.handles.len());
        q.handles.push(
            std::thread::Builder::new()
                .name(name)
                .spawn(move || pool_worker(&inner))
                .expect("spawning an ec-av1 pool worker"),
        );
        POOL_THREADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    drop(q);
    inner.cv.notify_one();
}

/// Live threads across every ec-av1 pool -- what the bounded-growth test
/// asserts on, and what a profiler counts as `ec-av1-*`.
static POOL_THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
pub(crate) fn pool_threads() -> usize {
    POOL_THREADS.load(std::sync::atomic::Ordering::Relaxed)
}

impl Drop for Pool {
    fn drop(&mut self) {
        let handles = {
            let mut q = self.inner.q.lock().unwrap();
            q.closed = true;
            std::mem::take(&mut q.handles)
        };
        self.inner.cv.notify_all();
        for h in handles {
            let _ = h.join();
        }
    }
}

thread_local! {
    /// This thread's pool, created on its first [`Batch::submit`] and joined
    /// when the thread exits (the process's main thread may skip that
    /// destructor, in which case the idle workers die with the process).
    static POOL: std::cell::RefCell<Option<Pool>> = const { std::cell::RefCell::new(None) };
    /// lane-memfix: set on a pool WORKER for its whole life -- the pool it
    /// serves. A job submitting a sub-batch (a pipeline stage banding itself
    /// across the filter pool) then reuses that pool instead of opening a
    /// nested one of its own.
    static ON_POOL: std::cell::RefCell<Option<std::sync::Arc<PoolInner>>> =
        const { std::cell::RefCell::new(None) };
}

struct BatchState {
    left: std::sync::Mutex<usize>,
    cv: std::sync::Condvar,
    panic: std::sync::Mutex<Option<Box<dyn std::any::Any + Send>>>,
}

/// A set of jobs running on the submitting thread's pool. `Drop` waits for
/// every one of them and re-raises the first panic, which is what makes
/// [`Batch::submit`]'s borrow of non-`'static` data sound -- exactly
/// `thread::scope`'s contract, minus the spawns.
///
/// corner-cut: soundness rests on this `Drop` running; `std::mem::forget` of a
/// `Batch` would let a job outlive its borrows. The type is crate-private and
/// every construction site below binds it to a local. Upgrade path is the
/// closure-taking `scope(|s| ...)` shape, which costs every caller a level of
/// indentation for the same guarantee.
pub(crate) struct Batch<'a> {
    st: std::sync::Arc<BatchState>,
    class: &'static str,
    _p: std::marker::PhantomData<&'a ()>,
}

impl<'a> Batch<'a> {
    /// `class` is the thread-name prefix for any pool worker this batch has
    /// to spawn -- see [`Pool::push`].
    pub(crate) fn new(class: &'static str) -> Self {
        Self {
            class,
            st: std::sync::Arc::new(BatchState {
                left: std::sync::Mutex::new(0),
                cv: std::sync::Condvar::new(),
                panic: std::sync::Mutex::new(None),
            }),
            _p: std::marker::PhantomData,
        }
    }

    pub(crate) fn submit<F: FnOnce() + Send + 'a>(&self, f: F) {
        *self.st.left.lock().unwrap() += 1;
        let st = std::sync::Arc::clone(&self.st);
        let job: Box<dyn FnOnce() + Send + 'a> = Box::new(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
            if let Err(p) = r {
                let mut slot = st.panic.lock().unwrap();
                if slot.is_none() {
                    *slot = Some(p);
                }
            }
            let mut left = st.left.lock().unwrap();
            *left -= 1;
            st.cv.notify_all();
        });
        // SAFETY: the job is dropped before `Batch::drop` returns (it waits
        // for `left == 0`), and `Batch<'a>` cannot outlive `'a`, so the
        // laundered lifetime is never observed past the real one.
        let job: Job = unsafe { std::mem::transmute::<Box<dyn FnOnce() + Send + 'a>, Job>(job) };
        // lane-memfix: on a pool worker, back into the pool it is serving; on
        // any other thread, this thread's own pool.
        //
        // No deadlock: a job blocked in `Batch::drop` is counted in `busy`,
        // so `pool_push` grows the pool by one thread for every queued job
        // that has no idle worker -- the child of a blocked parent always
        // gets a thread. The only waits inside a job are on its own sub-batch
        // (same rule, one level deeper) and on wavefront row atomics /
        // `PipeSync` counters, which are published by jobs that are already
        // running, never by a job still in the queue.
        let on = ON_POOL.with(|c| c.borrow().clone());
        match on {
            Some(inner) => pool_push(&inner, job, self.class),
            None => POOL.with(|p| {
                let mut p = p.borrow_mut();
                let inner = std::sync::Arc::clone(&p.get_or_insert_with(Pool::new).inner);
                drop(p);
                pool_push(&inner, job, self.class);
            }),
        }
    }
}

impl Drop for Batch<'_> {
    fn drop(&mut self) {
        let mut left = self.st.left.lock().unwrap();
        while *left > 0 {
            left = self.st.cv.wait(left).unwrap();
        }
        drop(left);
        if let Some(p) = self.st.panic.lock().unwrap().take() {
            std::panic::resume_unwind(p);
        }
    }
}

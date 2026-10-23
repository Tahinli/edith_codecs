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

/// `EC_AV1_FILTER_THREADS`, the number of worker threads each in-frame filter
/// stage splits itself across (default 1 = single-threaded, the shipped
/// behaviour). Read once per process.
/// 0 = "not read yet"; the env read happens once and the value is a plain
/// atomic afterwards, so a test can flip it ([`set_filter_threads`]) without
/// touching the process environment.
static FILTER_THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

thread_local! {
    /// This thread's override of [`filter_threads`], 0 = none. The ENCODER's
    /// frame-level filter search sets it for the span of that search
    /// (lane-av1fpar): the tile search is over by then, so every core is
    /// free, and each candidate's deblock/CDEF replay is exactly the banded
    /// stage the decoder already splits -- same bands, same byte-exact
    /// output, gated by
    /// `a_real_stream_filters_identically_with_one_and_four_filter_threads`
    /// on the decode side and by
    /// `encoder::tests::tile_bytes_do_not_depend_on_the_thread_count` here.
    static FILTER_BANDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Restores the previous [`FILTER_BANDS`] override when it drops.
pub(crate) struct FilterBands(usize);

impl Drop for FilterBands {
    fn drop(&mut self) {
        FILTER_BANDS.set(self.0);
    }
}

/// Bands every filter stage on THIS thread across `n` workers until the
/// returned guard drops; `n <= 1` leaves the process default in place.
pub(crate) fn override_filter_threads(n: usize) -> FilterBands {
    let previous = FILTER_BANDS.replace(if n > 1 { n.clamp(1, 64) } else { 0 });
    FilterBands(previous)
}

pub(crate) fn filter_threads() -> usize {
    let over = FILTER_BANDS.get();
    if over > 0 {
        return over;
    }
    match FILTER_THREADS.load(std::sync::atomic::Ordering::Relaxed) {
        0 => {
            let n = crate::envflags::var("EC_AV1_FILTER_THREADS")
                .ok()
                .and_then(|v| v.trim().parse::<usize>().ok())
                .map(|n| n.clamp(1, 64));
            // lane-av1recon: an unset env falls through to the split the
            // running decode picked ([`DecodeSplit`]); only a set value (or a
            // test override) is cached.
            match n {
                Some(n) => {
                    FILTER_THREADS.store(n, std::sync::atomic::Ordering::Relaxed);
                    n
                }
                None => AUTO_FILTER.load(std::sync::atomic::Ordering::Relaxed),
            }
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

/// `EC_AV1_TILE_THREADS`, how many workers the ENCODER spreads a frame's
/// tiles across -- both the RD/motion SEARCH (`encode::search_tiles`) and the
/// entropy WRITE (`encode::write_tiles`); default 1 = the shipped
/// single-threaded encode. Tiles are independent by construction (each starts
/// from the frame's own tables, resets its own contexts, and reads no sample
/// and no mode outside its own rectangle), so the bytes a frame comes out as
/// do not depend on this -- `encoder::tests::tile_bytes_do_not_depend_on_the_
/// thread_count` is the pin.
///
/// Measured (lane-av1tsearch, 12-core box, best of two interleaved passes):
/// 1920x1080, 12 frames -- 11.49 s at one tile (any thread count), 6.98 s at
/// 4x2 tiles and 8 threads (1.65x, 1.04 -> 1.72 fps); 3840x1608, 6 frames --
/// 17.12 s at 4x2 tiles and one thread, 8.64 s at 8 (1.98x, 0.35 -> 0.69
/// fps). Past ~8 tiles the extra tiles cost bytes without buying wall: what
/// is left is the per-frame serial tail (the filter search re-decodes the
/// whole frame), ~43% of an inter frame by Amdahl on those numbers.
static TILE_THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub(crate) fn tile_threads() -> usize {
    match TILE_THREADS.load(std::sync::atomic::Ordering::Relaxed) {
        0 => {
            let n = crate::envflags::var("EC_AV1_TILE_THREADS")
                .ok()
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(1)
                .clamp(1, 64);
            TILE_THREADS.store(n, std::sync::atomic::Ordering::Relaxed);
            n
        }
        n => n,
    }
}

/// Test-only override of [`tile_threads`]; see [`set_filter_threads`].
#[cfg(test)]
pub(crate) fn set_tile_threads(n: usize) {
    TILE_THREADS.store(n.clamp(1, 64), std::sync::atomic::Ordering::Relaxed);
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

/// Runs `f(lo, hi, slice, ctx)` once per band, band 0 on this thread and the
/// rest on scoped threads, all joined before the call returns. Band `(lo, hi)`
/// gets `data[lo * unit..hi * unit]` as its own `&mut` slice, carved with
/// `split_at_mut`, so no `unsafe` and no shared `&mut` is needed. `bands` must
/// be ascending and non-overlapping, and `hi * unit` must fit in `data`.
#[inline]
pub(crate) fn run_bands_mut<T, F>(
    bands: &[(usize, usize)],
    unit: usize,
    fctx: &crate::decode::FrameCtx,
    data: &mut [T],
    f: F,
) where
    T: Send,
    F: Fn(usize, usize, &mut [T], &crate::decode::FrameCtx) + Sync,
{
    let mut slices = Vec::with_capacity(bands.len());
    let mut rest = data;
    let mut pos = 0;
    for &(lo, hi) in bands {
        let (_, tail) = rest.split_at_mut((lo * unit) - pos);
        let (mine, after) = tail.split_at_mut((hi - lo) * unit);
        slices.push((lo, hi, mine));
        pos = hi * unit;
        rest = after;
    }
    let mut it = slices.into_iter();
    let Some((a0, b0, first)) = it.next() else {
        return;
    };
    if bands.len() == 1 {
        f(a0, b0, first, fctx);
        return;
    }
    scope("ec-av1-filter", |batch| {
        for (a, b, mine) in it {
            let ctx = crate::decode::filter_ctx_copy(fctx);
            let f = &f;
            batch.submit(move || f(a, b, mine, &ctx));
        }
        f(a0, b0, first, fctx);
    });
}

/// Carves `data` into one `&mut` slice per element range, `ranges` ascending
/// and non-overlapping (`split_at_mut` only, so no `unsafe`). Elements outside
/// every range are left alone.
pub(crate) fn carve_ranges<'a, T>(data: &'a mut [T], ranges: &[(usize, usize)]) -> Vec<&'a mut [T]> {
    split_bands_mut(ranges, 1, data)
}

/// Carves `data` into the slices [`run_bands_mut`] hands out, without running
/// anything: band `(lo, hi)` is `data[lo * unit..hi * unit]`. `bands` must be
/// ascending and non-overlapping.
#[inline]
pub(crate) fn split_bands_mut<'a, T>(
    bands: &[(usize, usize)],
    unit: usize,
    data: &'a mut [T],
) -> Vec<&'a mut [T]> {
    let mut out = Vec::with_capacity(bands.len());
    let mut rest: &'a mut [T] = data;
    let mut pos = 0;
    for &(lo, hi) in bands {
        let (_, tail) = std::mem::take(&mut rest).split_at_mut((lo * unit) - pos);
        let (mine, after) = tail.split_at_mut((hi - lo) * unit);
        out.push(mine);
        pos = hi * unit;
        rest = after;
    }
    out
}

/// [`run_bands_mut`] where band `i` also gets its own payload `parts[i]`
/// (moved in, so a caller can hand each band the disjoint `&mut` slices
/// [`carve_ranges`] made). `parts` is in the same order as `bands`.
#[inline]
pub(crate) fn run_bands_with<P, F>(
    bands: &[(usize, usize)],
    parts: Vec<P>,
    fctx: &crate::decode::FrameCtx,
    f: F,
) where
    P: Send,
    F: Fn(usize, usize, P, &crate::decode::FrameCtx) + Sync,
{
    debug_assert_eq!(bands.len(), parts.len());
    let mut it = bands.iter().zip(parts);
    let Some(((a0, b0), first)) = it.next() else {
        return;
    };
    if bands.len() == 1 {
        f(*a0, *b0, first, fctx);
        return;
    }
    scope("ec-av1-filter", |batch| {
        for (&(a, b), p) in it {
            let ctx = crate::decode::filter_ctx_copy(fctx);
            let f = &f;
            batch.submit(move || f(a, b, p, &ctx));
        }
        f(*a0, *b0, first, fctx);
    });
}

/// [`run_bands_mut`] for parts the caller has already carved, each one holding
/// its own `&mut` views: `f` runs once per part, part 0 inline and the rest on
/// the scope's threads, the same shape as [`run_bands_mut`].
#[inline]
pub(crate) fn run_parts_mut<S, F>(parts: &mut [S], fctx: &crate::decode::FrameCtx, f: F)
where
    S: Send,
    F: Fn(&mut S, &crate::decode::FrameCtx) + Sync,
{
    let Some((first, rest)) = parts.split_first_mut() else {
        return;
    };
    if rest.is_empty() {
        f(first, fctx);
        return;
    }
    scope("ec-av1-filter", |batch| {
        for part in rest.iter_mut() {
            let ctx = crate::decode::filter_ctx_copy(fctx);
            let f = &f;
            batch.submit(move || f(part, &ctx));
        }
        f(first, fctx);
    });
}

/// lane-av1recon: the in-frame thread split a decode at `EC_AV1_THREADS > 1`
/// picked ([`crate::autothreads::split`]), used only while the matching env
/// variable is unset. 1 = serial, which is what everything outside a threaded
/// `decode_stream_with_threads` call (the encoder's own decodes, the gates)
/// sees.
static AUTO_RECON: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
static AUTO_FILTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

/// Holds the default in-frame split for the span of one threaded decode and
/// restores the previous one on drop.
pub(crate) struct DecodeSplit(usize, usize);

impl DecodeSplit {
    /// Installs the split [`crate::autothreads::split`] picks for `threads`
    /// decode threads. Does nothing in the crate's own test build, where the
    /// gates read thread-local counters the in-frame workers never touch (a
    /// test forces the knobs through the env).
    pub(crate) fn install(threads: usize) -> Self {
        use std::sync::atomic::Ordering::Relaxed;
        let prev = Self(AUTO_RECON.load(Relaxed), AUTO_FILTER.load(Relaxed));
        if !cfg!(test) {
            let (r, f) = crate::autothreads::split(threads);
            AUTO_RECON.store(r, Relaxed);
            AUTO_FILTER.store(f, Relaxed);
        }
        prev
    }
}

impl Drop for DecodeSplit {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        AUTO_RECON.store(self.0, Relaxed);
        AUTO_FILTER.store(self.1, Relaxed);
    }
}

/// The superblock-row reconstruction workers per tile ([`crate::decode::wave`]).
static RECON_THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The recon thread count: `EC_AV1_RECON_THREADS` when set, else the split a
/// threaded decode installed ([`DecodeSplit`]). 1 means inline reconstruction.
pub(crate) fn recon_threads() -> usize {
    match RECON_THREADS.load(std::sync::atomic::Ordering::Relaxed) {
        0 => {
            let n = crate::envflags::var("EC_AV1_RECON_THREADS")
                .ok()
                .and_then(|v| v.trim().parse::<usize>().ok())
                .map(|n| n.clamp(1, 64));
            match n {
                Some(n) => {
                    RECON_THREADS.store(n, std::sync::atomic::Ordering::Relaxed);
                    n
                }
                None => AUTO_RECON.load(std::sync::atomic::Ordering::Relaxed),
            }
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
// work: one `thread::scope` per filter STAGE per frame, one frame worker per
// coded frame -- 2363 distinct threads in a 13 s 4K decode. The cost is not the clone of the thread itself but what
// dies with it: every `thread_local!` scratch pool in `decode.rs` (planes,
// rows, dq, mc, wiener, sgr) is allocated and faulted in again on each new
// thread, which is where the 3.65 M page faults and the 2.7x malloc traffic of
// the composed setting came from.
//
// The replacement is one pool per POOL-OWNING THREAD, kept in a thread-local
// and joined when that thread exits: the frame workers submit a [`Batch`] of
// `'static` jobs, the pool hands each to an idle worker (growing to
// `busy + queued` so a job never queues behind a blocking one -- a frame
// worker blocks on its references' promises), and the batch's `Drop` waits
// every job out. Scratch then lives as long as the worker, not as long as the
// frame. Borrowed jobs (filter bands, tile search) go through [`scope`]
// instead.
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

/// Live threads across every ec-av1 pool (what a profiler counts as `ec-av1-*`).
static POOL_THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

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

/// A set of `'static` jobs running on the submitting thread's pool. `Drop`
/// waits for every one of them and re-raises the first panic. Jobs own what
/// they touch (`Arc`s, moved values): a job that needs borrowed data uses
/// [`scope`] instead, which joins on its own threads.
pub(crate) struct Batch {
    st: std::sync::Arc<BatchState>,
    class: &'static str,
}

impl Batch {
    /// `class` is the thread-name prefix for any pool worker this batch has
    /// to spawn -- see [`pool_push`].
    pub(crate) fn new(class: &'static str) -> Self {
        Self {
            class,
            st: std::sync::Arc::new(BatchState {
                left: std::sync::Mutex::new(0),
                cv: std::sync::Condvar::new(),
                panic: std::sync::Mutex::new(None),
            }),
        }
    }

    pub(crate) fn submit<F: FnOnce() + Send + 'static>(&self, f: F) {
        *self.st.left.lock().unwrap() += 1;
        let st = std::sync::Arc::clone(&self.st);
        let job: Job = Box::new(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
            if let Err(p) = r {
                let mut slot = st.panic.lock().unwrap();
                if slot.is_none() {
                    *slot = Some(p);
                }
            }
            // Poison-tolerant (lane-av1panichang): the count must come down
            // even if some other holder of this lock panicked, or `drop`
            // would wait forever.
            let mut left = st
                .left
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *left -= 1;
            st.cv.notify_all();
        });
        // lane-memfix: on a pool worker, back into the pool it is serving; on
        // any other thread, this thread's own pool.
        //
        // No deadlock: a job blocked in `Batch::drop` is counted in `busy`,
        // so `pool_push` grows the pool by one thread for every queued job
        // that has no idle worker -- the child of a blocked parent always
        // gets a thread. A frame worker does wait on its references' promises,
        // which are published by jobs that are already running (never by one
        // still in the queue), so the growth rule covers those waits too.
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

impl Drop for Batch {
    fn drop(&mut self) {
        let mut left = self
            .st
            .left
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *left > 0 {
            left = self
                .st
                .cv
                .wait(left)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        drop(left);
        let panicked = self
            .st
            .panic
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        // Re-raise the first job panic on the waiting thread, unless this
        // thread is already unwinding: a second panic here would abort the
        // process (lane-av1panichang).
        if let Some(p) = panicked
            && !std::thread::panicking()
        {
            std::panic::resume_unwind(p);
        }
    }
}

/// Jobs of a [`scope`] that may borrow from the caller's stack. Each one runs
/// on its own `std::thread::scope` thread and is joined before `scope`
/// returns; the first job panic is re-raised after all of them have finished,
/// as [`Batch`] does.
pub(crate) struct Scope<'scope, 'env: 'scope> {
    inner: &'scope std::thread::Scope<'scope, 'env>,
    class: &'static str,
    panic: std::sync::Arc<std::sync::Mutex<Option<Box<dyn std::any::Any + Send>>>>,
}

impl<'scope> Scope<'scope, '_> {
    /// Runs `f` on a new thread named `class`, joined when the scope ends.
    pub(crate) fn submit<F: FnOnce() + Send + 'scope>(&self, f: F) {
        let panic = std::sync::Arc::clone(&self.panic);
        std::thread::Builder::new()
            .name(self.class.to_owned())
            .spawn_scoped(self.inner, move || {
                if let Err(p) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
                    let mut slot = panic
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if slot.is_none() {
                        *slot = Some(p);
                    }
                }
            })
            .expect("spawning an ec-av1 scoped worker");
    }
}

/// Runs `f` with a [`Scope`] whose jobs may borrow anything that outlives the
/// call, and returns once every job has finished. The borrowed counterpart of
/// [`Batch`]: a job here waits on nothing but its own work, so it needs no
/// pool growth rule, and its `thread_local!` scratch is freed with the thread.
pub(crate) fn scope<'env, R>(
    class: &'static str,
    f: impl for<'scope> FnOnce(&Scope<'scope, 'env>) -> R,
) -> R {
    let panic = std::sync::Arc::new(std::sync::Mutex::new(None));
    let out = std::thread::scope(|inner| {
        f(&Scope {
            inner,
            class,
            panic: std::sync::Arc::clone(&panic),
        })
    });
    let panicked = panic
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(p) = panicked {
        std::panic::resume_unwind(p);
    }
    out
}

// --- lane-av1fpar: encoder per-stage wall breakdown -----------------------
//
// Amdahl on the per-tile search (lane-av1tsearch) put ~43% of an inter frame
// OUTSIDE the tile search without saying what it was. These counters name it:
// one accumulator per serial stage of `encode_*_frame`, added to on whatever
// thread runs the stage and printed by `encoder::tests::filter_stage_wall_*`.
// Off (the default) each `stage` call is one relaxed atomic load.

/// The stages [`stage`] accumulates, in the order a frame runs them.
pub(crate) const STAGES: [&str; 10] = [
    "tile search",
    "tile write",
    "filter: deblock replay",
    "filter: cdef replay",
    "filter: sse",
    "filter search (total)",
    "capture decode",
    "lr search",
    "filter: plane copies",
    "filter: first decode",
];
pub(crate) const S_TILE_SEARCH: usize = 0;
pub(crate) const S_TILE_WRITE: usize = 1;
pub(crate) const S_DEBLOCK: usize = 2;
pub(crate) const S_CDEF: usize = 3;
pub(crate) const S_SSE: usize = 4;
pub(crate) const S_FILTER: usize = 5;
pub(crate) const S_CAPTURE: usize = 6;
pub(crate) const S_LR: usize = 7;
pub(crate) const S_COPY: usize = 8;
pub(crate) const S_FIRST: usize = 9;

static STAGE_NS: [std::sync::atomic::AtomicU64; STAGES.len()] =
    [const { std::sync::atomic::AtomicU64::new(0) }; STAGES.len()];
/// 0 = not read yet, 1 = off, 2 = on. `EC_AV1_STAGE_TIMES`, or
/// [`set_stage_times`] from a gate (the env snapshot is process-constant).
static STAGE_ON: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub(crate) fn stage_times() -> bool {
    match STAGE_ON.load(std::sync::atomic::Ordering::Relaxed) {
        0 => {
            let on = 1 + usize::from(crate::envflags::is_set("EC_AV1_STAGE_TIMES"));
            STAGE_ON.store(on, std::sync::atomic::Ordering::Relaxed);
            on == 2
        }
        n => n == 2,
    }
}

/// Test-only override of [`stage_times`]; process-global.
#[cfg(test)]
pub(crate) fn set_stage_times(on: bool) {
    STAGE_ON.store(1 + usize::from(on), std::sync::atomic::Ordering::Relaxed);
}

/// A live stage measurement: added to stage `i` when it drops. `None` when
/// the breakdown is off, so a call site is `let _t = par::timer(S_X);`.
pub(crate) struct Timer(usize, std::time::Instant);

#[inline]
pub(crate) fn timer(i: usize) -> Option<Timer> {
    stage_times().then(|| Timer(i, std::time::Instant::now()))
}

impl Drop for Timer {
    fn drop(&mut self) {
        STAGE_NS[self.0]
            .fetch_add(self.1.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Takes and clears the breakdown, in [`STAGES`] order.
#[cfg(test)]
pub(crate) fn take_stage_ns() -> [u64; STAGES.len()] {
    std::array::from_fn(|i| STAGE_NS[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

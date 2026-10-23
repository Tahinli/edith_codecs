//! Superblock-row wavefront reconstruction, in safe Rust.
//!
//! The tile thread parses. While a wavefront is installed, every
//! reconstruction write it would make ([`super::exec_intra`] and
//! [`super::exec_mc`]) is queued as an owned [`ReconOp`] instead. The queue
//! is cut into one [`Job`] per superblock and sent to the worker that owns
//! that superblock row (`row % workers`).
//!
//! A worker never shares a plane. It reconstructs into its own full-size
//! copy, seeded with the rows it owns. Intra prediction reads one pixel row
//! above a superblock row, and superblock `c` of row `r` reads up to
//! superblock `c + 1` of that row (above-right). Row `r - 1` therefore
//! publishes the bottom pixel row of each superblock it finishes into a
//! shared line buffer. Superblock `c` of row `r` waits until row `r - 1` has
//! finished column `c + 1`, copies that segment into the row above its own
//! strip, and then reconstructs. When the tile is done each worker's rows are
//! copied back into the frame planes, and its worker context's corrupt flag
//! is carried over to the tile's context.

use super::block::PlaneBuf;
use super::TxParams;
use super::{CflSrc, FrameCtx, Reach, RefPix, SharedRefs, WithRef, exec_intra, exec_mc};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use crate::encode::Picture;
use ec_core::Result;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, mpsc};
use crate::par::Batch;

/// A deferred residual: either finished, or the transform's inputs, run by
/// the worker that replays the write (the inverse transform is most of the
/// reconstruction work, so it must not stay on the parse thread).
pub(super) enum Residual {
    /// Codes nothing: the empty marker the reconstruction reads as all zeros.
    Zero,
    /// An already-transformed residual: `(offset, len)` in the job's arena.
    Ready(usize, usize),
    /// The transform's inputs: `(offset, len)` of the coefficients in the
    /// job's arena.
    Tx(TxParams, usize, usize),
}

impl Residual {
    /// An already-transformed residual; the shared zero block (a skipped
    /// block) is the marker, not a copy.
    pub(super) fn ready(residual: &[i32]) -> Self {
        if residual.is_empty() || std::ptr::eq(residual.as_ptr(), super::ZERO_RESIDUAL.as_ptr()) {
            Self::Zero
        } else {
            let off = push_coeffs(residual);
            Self::Ready(off, residual.len())
        }
    }

    /// The transform's inputs; a grid that codes nothing needs no transform.
    pub(super) fn tx(tx: TxParams, grid: &[i32]) -> Self {
        if super::is_zero_grid(grid) {
            Self::Zero
        } else {
            let off = push_coeffs(grid);
            Self::Tx(tx, off, grid.len())
        }
    }

    /// The residual to reconstruct with; `scratch` is where a transform lands.
    fn resolve<'a>(self, arena: &'a [i32], scratch: &'a mut Vec<i32>) -> &'a [i32] {
        match self {
            Self::Zero => &[],
            Self::Ready(off, len) => &arena[off..][..len],
            Self::Tx(tx, off, len) => {
                *scratch = tx.run(&arena[off..][..len]);
                scratch
            }
        }
    }
}

/// Appends to the superblock's coefficient arena, returning the offset.
fn push_coeffs(v: &[i32]) -> usize {
    QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        let off = q.coeffs.len();
        q.coeffs.extend_from_slice(v);
        off
    })
}

/// The ops of the superblock being parsed, with the arena their residuals
/// live in.
#[derive(Default)]
struct Queue {
    ops: Vec<ReconOp>,
    coeffs: Vec<i32>,
}

/// A block's whole-block prediction build, run by the worker into its
/// prediction slots (one `Vec` per plane). Owned, so it can cross to a worker.
pub(super) type PredFn = Box<
    dyn FnOnce(
            &mut PlaneBuf<'static>,
            &mut PlaneBuf<'static>,
            &mut PlaneBuf<'static>,
            &RefPix<'_>,
            &FrameCtx,
            &mut [Vec<u16>; 3],
        ) + Send,
>;

/// Where a deferred motion-compensated write reads its prediction from.
pub(super) enum PredSrc {
    /// The samples, carried by the op.
    Owned(Vec<u16>),
    /// A window of the block prediction the last [`ReconOp::Pred`] left in the
    /// worker's slot `plane`, at offset `off`, read at stride `src`.
    Cur { plane: usize, off: usize, src: usize },
}

/// One deferred reconstruction write. Owned, so it can cross to a worker.
pub(super) enum ReconOp {
    /// Build this block's prediction into the worker's slots.
    Pred(PredFn),
    Intra {
        plane: usize,
        x: usize,
        oy: usize,
        bw: usize,
        bh: usize,
        rect: bool,
        mode: usize,
        angle_delta: i32,
        reach: Reach,
        residual: Residual,
        cfl: Option<(i32, CflSrc)>,
        filter_intra: Option<usize>,
        smooth_neighbor: bool,
        pred: Option<Vec<u16>>,
    },
    Mc {
        plane: usize,
        x: usize,
        oy: usize,
        stride: usize,
        w: usize,
        h: usize,
        prediction: PredSrc,
        residual: Residual,
    },
}

thread_local! {
    /// Whether this thread's reconstruction is being deferred.
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    /// The running wavefront of this thread's current tile, if any.
    static GUARD: RefCell<Option<WaveGuard>> = const { RefCell::new(None) };
    /// The superblock being parsed (tile-relative), sent by the next `begin_sb`.
    static CURRENT: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
    /// The ops of the superblock being parsed, cut by [`submit`].
    static QUEUE: RefCell<Queue> = const { RefCell::new(Queue { ops: Vec::new(), coeffs: Vec::new() }) };
    /// Plane sets of finished workers, reused by the next tile so that a
    /// tile does not fault in fresh full-frame planes.
    static POOL: RefCell<Vec<[Vec<u16>; 3]>> = const { RefCell::new(Vec::new()) };
}

/// Plane sets kept for reuse; more than a tile's worth of workers is waste.
const POOL_SETS: usize = 4;

/// Ends the thread's wavefront when a tile's scope unwinds, so an error
/// return never leaves later reconstruction deferred.
pub(super) struct TileScope;

impl Drop for TileScope {
    fn drop(&mut self) {
        let guard = GUARD.with(|g| g.borrow_mut().take());
        drop(guard);
        ACTIVE.with(|a| a.set(false));
        CURRENT.with(|c| c.set(None));
        QUEUE.with(|q| {
            let mut q = q.borrow_mut();
            q.ops.clear();
            q.coeffs.clear();
        });
    }
}

/// Whether the current thread defers its reconstruction writes.
pub(super) fn active() -> bool {
    ACTIVE.with(Cell::get)
}

/// Queues one write. Only meaningful while [`active`].
pub(super) fn push(op: ReconOp) {
    QUEUE.with(|q| q.borrow_mut().ops.push(op));
}

/// Geometry of one tile's wavefront, in plane samples.
#[derive(Clone, Copy)]
struct Grid {
    /// Superblock width and height per plane.
    ux: [usize; 3],
    uy: [usize; 3],
    /// First superblock row and column of the tile, in units.
    row0: usize,
    col0: usize,
    /// Superblock columns of the tile.
    cols: usize,
    /// Superblock rows of the tile.
    rows: usize,
}

/// One superblock's ops, addressed in tile-relative units.
struct Job {
    row: usize,
    col: usize,
    ops: Vec<ReconOp>,
    /// The arena the ops' residuals live in.
    coeffs: Vec<i32>,
}

/// State shared by the tile thread and every worker.
struct Shared {
    /// Superblock columns each row has finished.
    done: Vec<usize>,
    /// Per row, the bottom pixel row of its last finished superblock, per
    /// plane, indexed by absolute column.
    lines: Vec<[Vec<u16>; 3]>,
    /// Set when a worker or the tile thread panicked: waiters give up.
    abort: bool,
}

struct Link {
    state: Mutex<Shared>,
    moved: Condvar,
}

impl Link {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn abort(&self) {
        self.lock().abort = true;
        self.moved.notify_all();
    }
}

/// What a worker hands back when its job stream ends.
struct Out {
    planes: [PlaneBuf<'static>; 3],
    corrupt: Option<&'static str>,
    /// The first reference-picture failure a deferred prediction parked.
    err: Option<ec_core::Error>,
}

/// Sets the abort flag when a worker unwinds, so its waiters do not hang.
struct AbortOnPanic(Arc<Link>);

impl Drop for AbortOnPanic {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.abort();
        }
    }
}

struct Worker {
    link: Arc<Link>,
    grid: Grid,
    planes: [PlaneBuf<'static>; 3],
    fctx: FrameCtx,
    /// The frame's reference pictures, waited for at the first deferred
    /// prediction.
    refs: Option<SharedRefs>,
    /// The block predictions the last [`ReconOp::Pred`] built, per plane.
    cur: [Vec<u16>; 3],
}

impl Worker {
    fn run(mut self, jobs: mpsc::Receiver<Job>) -> Out {
        let _abort = AbortOnPanic(Arc::clone(&self.link));
        let shared = self.refs.take();
        let pics: OnceLock<[Option<Arc<Picture>>; 8]> = OnceLock::new();
        let wait = || -> Result<[Option<&Picture>; 8]> {
            let Some(shared) = &shared else {
                return Ok([None; 8]);
            };
            if pics.get().is_none() {
                let _ = pics.set((shared.wait)()?);
            }
            let got = pics.get().expect("the pictures were just set");
            Ok(std::array::from_fn(|i| got[i].as_deref()))
        };
        let refpix = RefPix::lazy(shared.as_ref().map_or([None; 8], |s| s.dims), &wait, None);
        while let Ok(job) = jobs.recv() {
            if !self.job(job, &refpix) {
                break;
            }
        }
        Out {
            planes: self.planes,
            corrupt: self.fctx.corrupt.get(),
            err: refpix.take_err().err(),
        }
    }

    /// Reconstructs one superblock. `false` when the wavefront aborted.
    fn job(&mut self, job: Job, refpix: &RefPix<'_>) -> bool {
        crate::fault::point("recon");
        let Job { row, col, ops, coeffs } = job;
        let mut scratch = Vec::new();
        if row > 0 && !self.halo(row, col) {
            return false;
        }
        let [y, u, v] = &mut self.planes;
        for op in ops {
            match op {
                ReconOp::Pred(build) => build(y, u, v, refpix, &self.fctx, &mut self.cur),
                ReconOp::Intra {
                    plane,
                    x,
                    oy,
                    bw,
                    bh,
                    rect,
                    mode,
                    angle_delta,
                    reach,
                    residual,
                    cfl,
                    filter_intra,
                    smooth_neighbor,
                    pred,
                } => {
                    let res = residual.resolve(&coeffs, &mut scratch);
                    exec_intra(
                        y,
                        u,
                        v,
                        plane,
                        x,
                        oy,
                        bw,
                        bh,
                        rect,
                        mode,
                        angle_delta,
                        reach,
                        res,
                        cfl,
                        filter_intra,
                        smooth_neighbor,
                        pred,
                        &self.fctx,
                    );
                }
                ReconOp::Mc {
                    plane,
                    x,
                    oy,
                    stride,
                    w,
                    h,
                    prediction,
                    residual,
                } => {
                    let res = residual.resolve(&coeffs, &mut scratch);
                    match prediction {
                        PredSrc::Owned(p) => {
                            exec_mc(y, u, v, plane, x, oy, stride, w, h, &p, res, &self.fctx);
                        }
                        PredSrc::Cur { plane: sp, off, src } => {
                            let block = &self.cur[sp][off..];
                            if src == stride {
                                exec_mc(y, u, v, plane, x, oy, stride, w, h, block, res, &self.fctx);
                            } else {
                                // A transform unit's residual is dense, so its window
                                // of the block prediction is taken out dense too.
                                let mut window = Vec::with_capacity(w * h);
                                for row in 0..h {
                                    window.extend_from_slice(&block[row * src..][..w]);
                                }
                                exec_mc(y, u, v, plane, x, oy, stride, w, h, &window, res, &self.fctx);
                            }
                        }
                    }
                }
            }
        }
        self.publish(row, col);
        true
    }

    /// Waits for row `row - 1` to clear column `col + 1`, then copies the
    /// pixels above this superblock into the row above the local strip.
    fn halo(&mut self, row: usize, col: usize) -> bool {
        let g = self.grid;
        let need = (col + 2).min(g.cols);
        let mut s = self.link.lock();
        while s.done[row - 1] < need && !s.abort {
            s = self
                .link
                .moved
                .wait(s)
                .unwrap_or_else(PoisonError::into_inner);
        }
        if s.abort {
            return false;
        }
        for p in 0..3 {
            let w = self.planes[p].width;
            if w == 0 {
                continue;
            }
            let halo_row = (g.row0 + row) * g.uy[p] - 1;
            let c0 = (g.col0 + col) * g.ux[p];
            let from = c0.saturating_sub(1);
            let to = (c0 + 2 * g.ux[p]).min(w);
            self.planes[p].data.to_mut()[halo_row * w + from..halo_row * w + to]
                .copy_from_slice(&s.lines[row - 1][p][from..to]);
        }
        true
    }

    /// Publishes this superblock's bottom pixel row, per plane, and wakes the
    /// row below.
    fn publish(&self, row: usize, col: usize) {
        let g = self.grid;
        let mut s = self.link.lock();
        for p in 0..3 {
            let w = self.planes[p].width;
            if w == 0 {
                continue;
            }
            let h = self.planes[p].height;
            let last = ((g.row0 + row + 1) * g.uy[p]).min(h).saturating_sub(1);
            let c0 = (g.col0 + col) * g.ux[p];
            let end = (c0 + g.ux[p]).min(w);
            s.lines[row][p][c0..end]
                .copy_from_slice(&self.planes[p].data[last * w + c0..last * w + end]);
        }
        s.done[row] = col + 1;
        drop(s);
        self.link.moved.notify_all();
    }
}

/// The tile thread's end of a running wavefront. Dropping it without
/// [`WaveGuard::finish`] (a parse error or panic) aborts the workers.
pub(super) struct WaveGuard {
    link: Arc<Link>,
    grid: Grid,
    jobs: Vec<mpsc::Sender<Job>>,
    /// The workers, on the thread's persistent pool: their thread-local
    /// scratch outlives the tile. Dropping it waits for every worker.
    batch: Option<Batch>,
    outs: mpsc::Receiver<(usize, Out)>,
}

/// Starts the wavefront for one tile of `planes` (`[y, u, v]`), when the tile
/// is worth splitting: more than one recon thread and at least two superblock
/// rows. Otherwise reconstruction stays inline.
pub(super) fn install(
    fctx: &FrameCtx,
    refpix: &RefPix<'_>,
    planes: [&PlaneBuf<'static>; 3],
    unit: usize,
    span: (usize, usize, usize, usize),
) {
    let (row0, rows, col0, cols) = span;
    let workers = crate::par::recon_threads().min(rows);
    if workers < 2 || cols == 0 {
        return;
    }
    let (ss_x, ss_y) = (super::ss_x(fctx), super::ss_y(fctx));
    let grid = Grid {
        ux: [unit, unit >> ss_x, unit >> ss_x],
        uy: [unit, unit >> ss_y, unit >> ss_y],
        row0,
        col0,
        cols,
        rows,
    };
    let link = Arc::new(Link {
        state: Mutex::new(Shared {
            done: vec![0; rows],
            lines: (0..rows)
                .map(|_| [0, 1, 2].map(|p| vec![0u16; planes[p].width]))
                .collect(),
            abort: false,
        }),
        moved: Condvar::new(),
    });
    let mut jobs = Vec::with_capacity(workers);
    let batch = Batch::new("ec-av1-recon");
    let (out_tx, outs) = mpsc::channel();
    for w in 0..workers {
        let mut own = take_planes(planes);
        for rr in (w..rows).step_by(workers) {
            for p in 0..3 {
                let src = planes[p];
                let (top, end, x0, x1) = strip(&grid, p, row0 + rr, src);
                for y in top..end {
                    own[p].data.to_mut()[y * src.width + x0..y * src.width + x1]
                        .copy_from_slice(&src.data[y * src.width + x0..y * src.width + x1]);
                }
            }
        }
        let worker = Worker {
            link: Arc::clone(&link),
            grid,
            planes: own,
            fctx: recon_ctx(fctx),
            refs: refpix.shared(),
            cur: Default::default(),
        };
        let (tx, rx) = mpsc::channel();
        jobs.push(tx);
        let out_tx = out_tx.clone();
        batch.submit(move || {
            let out = worker.run(rx);
            let _ = out_tx.send((w, out));
        });
    }
    CURRENT.with(|c| c.set(None));
    ACTIVE.with(|a| a.set(true));
    let guard = WaveGuard {
        link,
        grid,
        jobs,
        batch: Some(batch),
        outs,
    };
    GUARD.with(|g| *g.borrow_mut() = Some(guard));
}

/// Cuts the queue: the superblock `(row, col)` (tile-relative) that was being
/// parsed is handed to its worker, and `(row, col)` becomes the one in flight.
pub(super) fn begin_sb(row: usize, col: usize) {
    if !active() {
        return;
    }
    if let Some((r, c)) = CURRENT.with(|cur| cur.replace(Some((row, col)))) {
        send(r, c);
    }
}

/// Sends the superblock `(row, col)` with every op queued since the last cut.
fn send(row: usize, col: usize) {
    let (ops, coeffs) = QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        let next = Queue {
            ops: Vec::with_capacity(q.ops.len()),
            coeffs: Vec::with_capacity(q.coeffs.len()),
        };
        let Queue { ops, coeffs } = std::mem::replace(&mut *q, next);
        (ops, coeffs)
    });
    GUARD.with(|g| {
        if let Some(guard) = g.borrow().as_ref() {
            // A dead worker has already aborted the wavefront; its error surfaces
            // at `finish`.
            let _ = guard.jobs[row % guard.jobs.len()].send(Job { row, col, ops, coeffs });
        }
    });
}

/// Ends the tile's wavefront (if one is running): sends the last superblock,
/// joins the workers and copies their rows into `planes`. Reconstruction then
/// stays inline for the rest of the tile. Called at the end of each tile, and
/// before an inter-intra block, which reads its neighbours' reconstructed pixels
/// while it is parsed.
pub(super) fn finish_tile(
    fctx: &FrameCtx,
    refpix: &RefPix<'_>,
    planes: [&mut PlaneBuf<'static>; 3],
) {
    if !active() {
        return;
    }
    if let Some((row, col)) = CURRENT.with(|c| c.take()) {
        send(row, col);
    }
    ACTIVE.with(|a| a.set(false));
    let guard = GUARD.with(|g| g.borrow_mut().take());
    if let Some(guard) = guard {
        guard.finish(fctx, refpix, planes);
    }
}

impl WaveGuard {
    /// Joins the workers and copies their rows back into `planes`. A worker
    /// panic is resumed here; its corrupt flag is carried into `fctx`.
    pub(super) fn finish(
        mut self,
        fctx: &FrameCtx,
        refpix: &RefPix<'_>,
        mut planes: [&mut PlaneBuf<'static>; 3],
    ) {
        // Closing the queues lets every worker run out its jobs and return.
        self.jobs.clear();
        // Waits for the workers, and resumes the first one's panic.
        drop(self.batch.take());
        let mut outs: Vec<(usize, Out)> = self.outs.try_iter().collect();
        outs.sort_by_key(|(w, _)| *w);
        let nw = outs.len();
        let g = self.grid;
        for (w, out) in outs {
            for rr in (w..g.rows).step_by(nw) {
                for (p, dst) in planes.iter_mut().enumerate() {
                    let width = dst.width;
                    let (top, end, x0, x1) = strip(&g, p, g.row0 + rr, dst);
                    for y in top..end {
                        dst.data.to_mut()[y * width + x0..y * width + x1]
                            .copy_from_slice(&out.planes[p].data[y * width + x0..y * width + x1]);
                    }
                }
            }
            if let Some(why) = out.corrupt {
                fctx.flag_corrupt(why);
            }
            if let Some(e) = out.err {
                refpix.park_err(e);
            }
            let [a, b, c] = out.planes;
            give_planes([a.data.into_owned(), b.data.into_owned(), c.data.into_owned()]);
        }
    }
}

impl Drop for WaveGuard {
    fn drop(&mut self) {
        // Abort first, so no worker waits on a row that will never finish.
        self.link.abort();
        self.jobs.clear();
        // Unwinding already: a worker's panic must not be raised a second time.
        if let Some(batch) = self.batch.take() {
            if std::thread::panicking() {
                std::mem::forget(batch);
            } else {
                drop(batch);
            }
        }
    }
}

/// The rows `[top, end)` and columns `[x0, x1)` of superblock row `unit_row`
/// in plane `p`, clipped to the plane and to the tile's superblock columns.
fn strip(g: &Grid, p: usize, unit_row: usize, plane: &PlaneBuf<'static>) -> (usize, usize, usize, usize) {
    let top = unit_row * g.uy[p];
    let end = ((unit_row + 1) * g.uy[p]).min(plane.height);
    let x0 = (g.col0 * g.ux[p]).min(plane.width);
    let x1 = ((g.col0 + g.cols) * g.ux[p]).min(plane.width);
    if top >= end {
        (0, 0, 0, 0)
    } else {
        (top, end, x0, x1)
    }
}

/// Plane buffers for one worker: a recycled set if one of this size is
/// pooled, else fresh zeroed ones. Recycled planes keep their old samples
/// outside the rows and columns the worker is seeded with, and the decode
/// never reads a sample it has not written or seeded.
fn take_planes(planes: [&PlaneBuf<'static>; 3]) -> [PlaneBuf<'static>; 3] {
    let lens = planes.map(|p| p.data.len());
    let reuse = POOL.with(|pool| {
        let mut pool = pool.borrow_mut();
        let at = pool
            .iter()
            .position(|set| [0, 1, 2].iter().all(|&p| set[p].len() == lens[p]))?;
        Some(pool.swap_remove(at))
    });
    let bufs = reuse.unwrap_or_else(|| lens.map(|n| vec![0u16; n]));
    let [a, b, c] = bufs;
    [
        blank_like(planes[0], a),
        blank_like(planes[1], b),
        blank_like(planes[2], c),
    ]
}

/// Returns a finished worker's plane set to the pool.
fn give_planes(set: [Vec<u16>; 3]) {
    POOL.with(|pool| {
        let mut pool = pool.borrow_mut();
        if pool.len() < POOL_SETS {
            pool.push(set);
        }
    });
}

/// A plane with `src`'s geometry and tile bounds over `data`.
fn blank_like(src: &PlaneBuf<'static>, data: Vec<u16>) -> PlaneBuf<'static> {
    PlaneBuf {
        data: Cow::Owned(data),
        width: src.width,
        height: src.height,
        true_width: src.true_width,
        true_height: src.true_height,
        tile_x0: src.tile_x0,
        tile_y0: src.tile_y0,
        tile_x1: src.tile_x1,
        tile_y1: src.tile_y1,
        plane: src.plane,
    }
}

/// A worker's own context: the frame's values the reconstruction reads.
fn recon_ctx(fctx: &FrameCtx) -> FrameCtx {
    let out = super::filter_ctx_copy(fctx);
    super::set_enable_edge_filter(fctx.enable_edge_filter.with(Cell::get), &out);
    out
}

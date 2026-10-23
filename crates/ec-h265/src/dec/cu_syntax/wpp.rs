//! Wavefront parallel decoding of a slice segment (H.265 9.3.1 and 9.3.2.2) for pictures
//! with `entropy_coding_sync_enabled_flag`.
//!
//! The segment starts at the first CTB of a row and its substreams start at the rows
//! after it, so each row can be parsed and reconstructed by its own worker, from its own
//! substream, once the row above is two CTBs ahead: the contexts after CTB 1 of the row
//! above start the row, and the CTBs of the row above that a CTB reads (its left, above,
//! above-left and above-right neighbours) are final by then.
//!
//! Each row works on a band of the picture: its own maps and samples, plus a copy of the
//! last sample, unit and CTB row of the row above. The row above publishes that copy as
//! each of its CTBs completes, on the [`Board`]. When the segment is done the bands are
//! merged back in row order: the samples and maps, the motion cells and their checksum,
//! and then the in-loop filter callbacks, which therefore run in the same order as a
//! serial parse.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};

use super::{MotionSource, SliceStats, new_parser, trailing_bits_ok};
use crate::cabac::{CabacDecoder, Contexts};
use crate::dec::inter::InterRefs;
use crate::dec::motion_field::{FieldCell, MotionField, SliceMotion};
use crate::dec::pic::{BlockInfo, CtbInfo, PicState, SliceFilter};
use crate::dec::tiles::TileScan;
use crate::dec::trace::{self, Phase};
use ec_core::error::{Error, Result};
use ec_h265_syntax::slice::{SliceHeader, SliceType};
use ec_h265_syntax::{Pps, Sps};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // The guarded values are plain data; a poisoned lock is still usable.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What a finished CTB row publishes for the row below it: its last sample row, unit row
/// and CTB entries, and the contexts after its CTB 1.
#[derive(Default)]
struct Slot {
    /// CTBs of the row completed, in order.
    done: usize,
    luma: Vec<u16>,
    cb: Vec<u16>,
    cr: Vec<u16>,
    units: Vec<BlockInfo>,
    cells: Vec<Option<FieldCell>>,
    ctb: Vec<CtbInfo>,
    wpp: Option<Contexts>,
}

#[derive(Default)]
struct BoardState {
    slots: Vec<Slot>,
    failed: Option<String>,
}

/// The rows' published state of one segment. A row waits on the board for the row above.
struct Board {
    state: Mutex<BoardState>,
    changed: Condvar,
    cols: usize,
    ctb_log2: u32,
    width: usize,
    height: usize,
    w4: usize,
    /// The log2 chroma subsampling `(sub_x, sub_y)`.
    sub_x: u32,
    sub_y: u32,
}

impl Board {
    fn new(pic: &PicState) -> Board {
        Board {
            state: Mutex::new(BoardState {
                slots: (0..pic.ctb_rows).map(|_| Slot::default()).collect(),
                failed: None,
            }),
            changed: Condvar::new(),
            cols: pic.ctb_cols,
            ctb_log2: pic.ctb_log2,
            width: pic.width,
            height: pic.height,
            w4: pic.w4,
            sub_x: pic.sub_x(),
            sub_y: pic.sub_y(),
        }
    }

    /// Mark the segment failed and wake every waiter.
    fn fail(&self, why: &str) {
        let mut st = lock(&self.state);
        if st.failed.is_none() {
            st.failed = Some(why.to_string());
        }
        self.changed.notify_all();
    }

    /// Publish the row above a segment that starts a row: its state as the picture
    /// holds it now, which is final because the earlier segments are complete.
    fn prefill(&self, ry: usize, pic: &PicState, field: &MotionField) {
        // The same bottom rows of CTB row `ry` that `leave` publishes.
        let ctb = 1usize << self.ctb_log2;
        let y0 = ry << self.ctb_log2;
        let cw = self.width.div_ceil(1 << self.sub_x);
        let mut st = lock(&self.state);
        let slot = &mut st.slots[ry];
        if pic.has_pixels() {
            let lr = (y0 + ctb - 1) - pic.origin_y;
            slot.luma = pic.planes.y[lr * self.width..(lr + 1) * self.width].to_vec();
            let lc = ((y0 >> self.sub_y) + (ctb >> self.sub_y) - 1) - pic.origin_c;
            slot.cb = pic.planes.cb[lc * cw..(lc + 1) * cw].to_vec();
            slot.cr = pic.planes.cr[lc * cw..(lc + 1) * cw].to_vec();
        }
        let ur = (y0 + ctb - 4) / 4;
        let band_row = ur - pic.origin_unit / 4;
        slot.units = pic.info[band_row * self.w4..(band_row + 1) * self.w4].to_vec();
        slot.cells = field.cell_row(ur).to_vec();
        slot.ctb = pic.ctb[ry * self.cols..(ry + 1) * self.cols].to_vec();
        slot.wpp = pic.carry.wpp.clone();
        slot.done = self.cols;
        self.changed.notify_all();
    }

    /// Before CTB `x` of row `ry`: wait until the row above is two CTBs ahead (or done),
    /// copy in what the CTB reads from it, and return the row's starting contexts when
    /// `x` is 0 (the contexts the row above stored after its CTB 1).
    fn enter(
        &self,
        ry: usize,
        x: usize,
        pic: &mut PicState,
        motion: &mut dyn MotionSource,
    ) -> Result<Option<Contexts>> {
        if ry == 0 {
            return Ok(None);
        }
        let ctb = 1usize << self.ctb_log2;
        let cols = self.cols;
        let need = (x + 2).min(cols);
        let mut st = lock(&self.state);
        let t = trace::now();
        loop {
            if let Some(why) = &st.failed {
                return Err(Error::corrupt(format!("HEVC slice (wavefront): {why}")));
            }
            if st.slots[ry - 1].done >= need {
                break;
            }
            st = self.changed.wait(st).unwrap_or_else(|e| e.into_inner());
        }
        trace::since(Phase::WaitSync, t);
        let slot = &st.slots[ry - 1];
        let k0 = x.saturating_sub(1);
        let k1 = (x + 1).min(cols - 1);
        let cw = self.width.div_ceil(1 << self.sub_x);
        for k in k0..=k1 {
            let (a, b) = (k * ctb, ((k + 1) * ctb).min(self.width));
            let (ca, cb_end) = (
                (k * ctb) >> self.sub_x,
                (((k + 1) * ctb) >> self.sub_x).min(cw),
            );
            if pic.has_pixels() {
                pic.planes.y[a..b].copy_from_slice(&slot.luma[a..b]);
                pic.planes.cb[ca..cb_end].copy_from_slice(&slot.cb[ca..cb_end]);
                pic.planes.cr[ca..cb_end].copy_from_slice(&slot.cr[ca..cb_end]);
            }
            let (ua, ub) = (k * ctb / 4, ((k + 1) * ctb / 4).min(self.w4));
            pic.info[ua..ub].copy_from_slice(&slot.units[ua..ub]);
            pic.ctb[(ry - 1) * cols + k] = slot.ctb[k];
        }
        let unit_row = (ry << self.ctb_log2) / 4 - 1;
        let field = motion
            .band_cells()
            .ok_or_else(|| Error::corrupt("HEVC slice (wavefront): a row without a motion band"))?;
        for k in k0..=k1 {
            let (ua, ub) = (k * ctb / 4, ((k + 1) * ctb / 4).min(self.w4));
            field.cell_row_mut(unit_row)[ua..ub].copy_from_slice(&slot.cells[ua..ub]);
        }
        Ok((x == 0).then(|| slot.wpp.clone()).flatten())
    }

    /// After CTB `x` of row `ry` is complete: publish what the row below reads. `wpp` is
    /// the contexts after CTB 1, when `x` is 1.
    fn leave(
        &self,
        ry: usize,
        x: usize,
        pic: &PicState,
        motion: &mut dyn MotionSource,
        wpp: Option<&Contexts>,
    ) -> Result<()> {
        let ctb = 1usize << self.ctb_log2;
        let y0 = ry << self.ctb_log2;
        let full = y0 + ctb <= self.height;
        let (a, b) = (x * ctb, ((x + 1) * ctb).min(self.width));
        let cw = self.width.div_ceil(1 << self.sub_x);
        let (ca, cb_end) = (
            (x * ctb) >> self.sub_x,
            (((x + 1) * ctb) >> self.sub_x).min(cw),
        );
        let (ua, ub) = (x * ctb / 4, ((x + 1) * ctb / 4).min(self.w4));
        let field = if full {
            Some(motion.band_cells().ok_or_else(|| {
                Error::corrupt("HEVC slice (wavefront): a row without a motion band")
            })?)
        } else {
            None
        };
        let mut st = lock(&self.state);
        let slot = &mut st.slots[ry];
        if slot.ctb.is_empty() {
            slot.units = vec![BlockInfo::default(); self.w4];
            slot.cells = vec![None; self.w4];
            slot.ctb = vec![CtbInfo::default(); self.cols];
            slot.luma = vec![0; self.width];
            slot.cb = vec![0; cw];
            slot.cr = vec![0; cw];
        }
        if full {
            // Band rows: the sample rows and unit row of the CTB row's last rows.
            if pic.has_pixels() {
                let lr = (y0 + ctb - 1) - pic.origin_y;
                slot.luma[a..b]
                    .copy_from_slice(&pic.planes.y[lr * self.width + a..lr * self.width + b]);
                let lc = ((y0 >> self.sub_y) + (ctb >> self.sub_y) - 1) - pic.origin_c;
                slot.cb[ca..cb_end].copy_from_slice(&pic.planes.cb[lc * cw + ca..lc * cw + cb_end]);
                slot.cr[ca..cb_end].copy_from_slice(&pic.planes.cr[lc * cw + ca..lc * cw + cb_end]);
            }
            let ur = (y0 + ctb - 4) / 4;
            let band_row = ur - pic.origin_unit / 4;
            slot.units[ua..ub]
                .copy_from_slice(&pic.info[band_row * self.w4 + ua..band_row * self.w4 + ub]);
            if let Some(field) = field {
                slot.cells[ua..ub].copy_from_slice(&field.cell_row(ur)[ua..ub]);
            }
        }
        // The CTB entry is always kept, so that the row below sees the slice and tile of
        // every CTB it may read.
        slot.ctb[x] = pic.ctb[ry * self.cols + x];
        if let Some(c) = wpp {
            slot.wpp = Some(c.clone());
        }
        slot.done = x + 1;
        self.changed.notify_all();
        Ok(())
    }

    /// The contexts a row stored after its CTB 1, taken once the segment is done.
    fn take_wpp(&self, ry: usize) -> Option<Contexts> {
        lock(&self.state).slots[ry].wpp.take()
    }
}

/// The state of one CTB row's parse, as the row's [`Link`] sees it.
struct Link<'b> {
    board: &'b Board,
    ry: usize,
}

/// What the row loop asks of its neighbours: the board reads and publications.
trait RowSync {
    fn enter(
        &mut self,
        pic: &mut PicState,
        motion: &mut dyn MotionSource,
        x: usize,
    ) -> Result<Option<Contexts>>;
    fn leave(
        &mut self,
        pic: &PicState,
        motion: &mut dyn MotionSource,
        x: usize,
        wpp: Option<&Contexts>,
    ) -> Result<()>;
}

impl RowSync for Link<'_> {
    fn enter(
        &mut self,
        pic: &mut PicState,
        motion: &mut dyn MotionSource,
        x: usize,
    ) -> Result<Option<Contexts>> {
        self.board.enter(self.ry, x, pic, motion)
    }

    fn leave(
        &mut self,
        pic: &PicState,
        motion: &mut dyn MotionSource,
        x: usize,
        wpp: Option<&Contexts>,
    ) -> Result<()> {
        self.board.leave(self.ry, x, pic, motion, wpp)
    }
}

/// The values every row of the segment shares.
struct SegConsts {
    slice_qp: i32,
    fresh: Contexts,
    slice_index: u16,
    slice_addr: u32,
}

/// What one row's parse hands back.
struct RowOut {
    stats: SliceStats,
    contexts: Contexts,
    last_qp: i32,
    trailing_ok: bool,
}

/// One finished row: its band and what it parsed.
struct RowDone {
    ry: usize,
    band: PicState,
    field: MotionField,
    out: RowOut,
}

/// Parse one CTB row of the segment from `data` (the row's substream, to the end of the
/// slice data). `row_len` is the substream's length when another row follows it.
#[allow(clippy::too_many_arguments)] // the row's inputs and its neighbours
fn parse_row(
    sps: &Sps,
    pps: &Pps,
    header: &SliceHeader,
    pic: &mut PicState,
    motion: &mut dyn MotionSource,
    refs: Option<&InterRefs>,
    seg: &SegConsts,
    data: &[u8],
    ry: usize,
    last: bool,
    row_len: Option<usize>,
    sync: &mut dyn RowSync,
) -> Result<RowOut> {
    let cols = pic.ctb_cols;
    let ctb_log2 = pic.ctb_log2;
    let ctb = 1usize << ctb_log2;
    let sao = header.sao_luma || header.sao_chroma;
    let mut p = new_parser(sps, pps, header, pic, refs, motion, seg.slice_qp);
    let mut dec = CabacDecoder::new(data, seg.fresh.clone());
    let mut ended = false;
    for x in 0..cols {
        let rs = ry * cols + x;
        let (x0, y0) = (x << ctb_log2, ry << ctb_log2);
        {
            let info = p
                .pic
                .ctb
                .get_mut(rs)
                .ok_or_else(|| Error::corrupt("HEVC slice: CTB address past the picture"))?;
            *info = CtbInfo {
                slice: seg.slice_index,
                slice_addr: seg.slice_addr,
                tile: 0,
                sao: info.sao,
            };
        }
        p.stats.ctus += 1;
        let wpp_ctx = sync.enter(&mut *p.pic, &mut *p.motion, x)?;
        if x == 0 {
            // 9.3.1: a row syncs from the row above when its CTB 1 is available.
            let top_right =
                p.pic
                    .available(x0, y0, (x0 + ctb) as isize, y0 as isize - ctb as isize);
            dec.contexts = match (top_right, wpp_ctx) {
                (true, Some(saved)) => saved,
                _ => seg.fresh.clone(),
            };
            p.last_qp = seg.slice_qp;
        }
        if sao {
            p.sao(&mut dec, rs, x, ry)?;
        }
        p.coding_quadtree(&mut dec, x0, y0, ctb_log2, 0, &mut |_| {})?;
        // 9.3.2.2: the wavefront storage after the second CTB of a row.
        let stored = (x == 1 && cols > 1).then(|| dec.contexts.clone());
        sync.leave(&*p.pic, &mut *p.motion, x, stored.as_ref())?;
        if dec.decode_terminate() != 0 {
            if !last {
                return Err(Error::corrupt(
                    "HEVC slice syntax: the segment ended inside a wavefront row before its last substream",
                ));
            }
            ended = true;
            break;
        }
        if x + 1 == cols {
            if last {
                return Err(Error::corrupt(
                    "HEVC slice syntax: slice data continues past the last wavefront substream",
                ));
            }
            if dec.decode_terminate() != 1 {
                return Err(Error::corrupt(
                    "HEVC slice syntax: end_of_subset_one_bit was not set at a substream boundary",
                ));
            }
            if row_len.is_some_and(|len| dec.byte_position_aligned() != len) {
                return Err(Error::corrupt(
                    "HEVC slice syntax: a wavefront substream does not end at its entry point",
                ));
            }
        }
    }
    if last && !ended {
        return Err(Error::corrupt(
            "HEVC slice syntax: end_of_slice_segment_flag 0 after the last CTB of the picture",
        ));
    }
    let trailing_ok = last && trailing_bits_ok(data, dec.bits_read());
    let stats = std::mem::take(&mut p.stats);
    Ok(RowOut {
        stats,
        contexts: dec.contexts.clone(),
        last_qp: p.last_qp,
        trailing_ok,
    })
}

/// Whether a slice segment can run wavefront rows in parallel: a wavefront picture without
/// tiles, a segment that starts a CTB row, and entry points for the rows after its first.
pub(in crate::dec) fn can_run(
    pps: &Pps,
    header: &SliceHeader,
    cols: usize,
    ctb_rows: usize,
    starts: &[usize],
    data_len: usize,
) -> bool {
    if !pps.entropy_coding_sync_enabled || pps.tiles_enabled || starts.is_empty() {
        return false;
    }
    let addr = header.segment_address as usize;
    let rows = starts.len() + 1;
    addr.is_multiple_of(cols)
        && addr / cols + rows <= ctb_rows
        && starts.windows(2).all(|w| w[0] < w[1])
        && starts.iter().all(|&s| s > 0 && s < data_len)
}

/// Parse a slice segment whose rows run on up to `workers` threads (the calling thread
/// included). `starts` are the data offsets of the substreams after the first. Errors
/// and results match the serial parse: the same maps, samples, motion and checksum, and
/// `rows` is called for each row in order, after the row's motion is final.
#[allow(clippy::too_many_arguments)] // the segment, the picture it decodes into and its motion
pub(in crate::dec) fn parse_segment(
    sps: &Sps,
    pps: &Pps,
    header: &SliceHeader,
    data: &[u8],
    starts: &[usize],
    pic: &mut PicState,
    source: &mut SliceMotion<'_>,
    refs: Option<&InterRefs>,
    workers: usize,
    rows: &mut dyn FnMut(&mut PicState, usize),
) -> Result<SliceStats> {
    let cols = pic.ctb_cols;
    let ctb_rows = pic.ctb_rows;
    let ctb_log2 = pic.ctb_log2;
    let ctb = 1usize << ctb_log2;
    let ry0 = header.segment_address as usize / cols;
    let nrows = starts.len() + 1;
    if !can_run(pps, header, cols, ctb_rows, starts, data.len()) || ry0 + nrows > ctb_rows {
        return Err(Error::corrupt(
            "HEVC slice (wavefront): the segment's substreams do not match its rows",
        ));
    }
    // 6.5.1 and the slice's entry in the picture's slice list, as the serial parse sets them.
    pic.tiles = TileScan::from_pps(pps, cols, ctb_rows)?;
    if !header.dependent_slice_segment {
        pic.carry.slice_addr = header.segment_address as usize;
    }
    let slice_index = u16::try_from(pic.slices.len()).map_err(|_| {
        Error::unsupported("HEVC slice syntax: slice count", "more than 65535 slices")
    })?;
    pic.slices.push(SliceFilter {
        deblock_disabled: header.deblocking_filter_disabled,
        beta_offset_div2: header.beta_offset_div2,
        tc_offset_div2: header.tc_offset_div2,
        loop_filter_across_slices: header.loop_filter_across_slices_enabled,
        sao_luma: header.sao_luma,
        sao_chroma: header.sao_chroma,
    });
    let slice_qp = 26 + pps.init_qp_minus26 + header.qp_delta;
    let init_type = match header.slice_type {
        SliceType::I => 0,
        SliceType::P => 1 + u8::from(header.cabac_init),
        SliceType::B => 2 - u8::from(header.cabac_init),
    };
    let seg = SegConsts {
        slice_qp,
        fresh: Contexts::for_init_type(slice_qp, init_type),
        slice_index,
        slice_addr: pic.carry.slice_addr as u32,
    };

    let board = Board::new(pic);
    if ry0 > 0 {
        board.prefill(ry0 - 1, pic, &*source.field);
    }
    let pixels = pic.has_pixels();
    let next = AtomicUsize::new(0);
    let lists = source.lists;
    let cur_poc = source.cur_poc;
    let slice = source.slice;
    let global: &PicState = pic;
    let field_global: &MotionField = &*source.field;
    let col = source.col.clone();

    // One row: its band, its motion band, and the parse of the row's substream.
    let do_row = |ry: usize| -> Result<RowDone> {
        let i = ry - ry0;
        let last = i + 1 == nrows;
        let y0 = ry << ctb_log2;
        let bound = usize::from(ry > 0);
        let own = ctb.min(global.height - y0);
        let mut band = global.band(y0, own, pixels);
        let mut field = field_global.band((y0 - 4 * bound) / 4, bound + own.div_ceil(4));
        let row_start = if i == 0 { 0 } else { starts[i - 1] };
        let row_len = (!last).then(|| starts[i] - row_start);
        let row_data = data
            .get(row_start..)
            .ok_or_else(|| Error::corrupt("HEVC slice (wavefront): a substream past the data"))?;
        let mut motion = SliceMotion::new(
            sps,
            pps,
            header,
            lists,
            cur_poc,
            &mut field,
            col.clone(),
            slice,
        );
        let mut link = Link { board: &board, ry };
        let out = parse_row(
            sps,
            pps,
            header,
            &mut band,
            &mut motion,
            refs,
            &seg,
            row_data,
            ry,
            last,
            row_len,
            &mut link,
        )?;
        Ok(RowDone {
            ry,
            band,
            field,
            out,
        })
    };
    // The rows are taken in order, so a row only ever waits on a row another worker has
    // already taken. A failing row fails the segment, which wakes every waiter.
    let run = || -> Result<Vec<RowDone>> {
        let mut done = Vec::new();
        loop {
            let i = next.fetch_add(1, Ordering::SeqCst);
            if i >= nrows {
                break;
            }
            let ry = ry0 + i;
            let t = trace::now();
            let outcome = catch_unwind(AssertUnwindSafe(|| do_row(ry)));
            trace::since(Phase::Decode, t);
            match outcome {
                Ok(Ok(row)) => done.push(row),
                Ok(Err(e)) => {
                    board.fail(&e.to_string());
                    return Err(e);
                }
                Err(_) => {
                    board.fail("the row's worker panicked");
                    return Err(Error::corrupt("HEVC slice (wavefront): a row panicked"));
                }
            }
        }
        Ok(done)
    };
    let results: Vec<Result<Vec<RowDone>>> = std::thread::scope(|s| {
        let helpers: Vec<_> = (1..workers.clamp(1, nrows))
            .map(|_| {
                s.spawn(|| {
                    trace::set_role("wpp-helper");
                    run()
                })
            })
            .collect();
        let mut all = vec![run()];
        for h in helpers {
            all.push(h.join().unwrap_or_else(|_| {
                Err(Error::corrupt("HEVC slice (wavefront): a worker panicked"))
            }));
        }
        all
    });
    let mut rows_done = Vec::with_capacity(nrows);
    for r in results {
        rows_done.extend(r?);
    }
    rows_done.sort_by_key(|r| r.ry);
    if rows_done.len() != nrows {
        return Err(Error::corrupt(
            "HEVC slice (wavefront): a row was not parsed",
        ));
    }

    // Merge the bands back in row order, running the per-row callbacks as a serial parse does.
    let mut stats = SliceStats::default();
    let mut last_out: Option<(Contexts, i32)> = None;
    for done in rows_done {
        let ry = done.ry;
        let y0 = ry << ctb_log2;
        let bound = usize::from(ry > 0);
        let own = ctb.min(pic.height - y0);
        let (sx, sy) = (pic.sub_x(), pic.sub_y());
        let own_c = own.div_ceil(1 << sy);
        let cw = pic.width.div_ceil(1 << sx);
        if pixels {
            let w = pic.width;
            pic.planes.y[y0 * w..(y0 + own) * w]
                .copy_from_slice(&done.band.planes.y[bound * w..(bound + own) * w]);
            let (c0, c1) = (y0 >> sy, (y0 >> sy) + own_c);
            pic.planes.cb[c0 * cw..c1 * cw]
                .copy_from_slice(&done.band.planes.cb[bound * cw..(bound + own_c) * cw]);
            pic.planes.cr[c0 * cw..c1 * cw]
                .copy_from_slice(&done.band.planes.cr[bound * cw..(bound + own_c) * cw]);
        }
        let u0 = y0 / 4;
        let units = own.div_ceil(4);
        pic.info[u0 * pic.w4..(u0 + units) * pic.w4]
            .copy_from_slice(&done.band.info[bound * pic.w4..(bound + units) * pic.w4]);
        let (k0, k1) = (ry * cols, (ry + 1) * cols);
        pic.ctb[k0..k1].copy_from_slice(&done.band.ctb[k0..k1]);
        source.field.merge_band(&done.field, u0);
        // A row the segment ends inside is finished by the next segment (serially), so
        // its motion and filter rows wait for that: publish only complete rows.
        if done.out.stats.ctus == cols {
            source.row_done(pic, y0 + ctb);
            rows(pic, ry);
        }
        stats.ctus += done.out.stats.ctus;
        stats.trailing_bits_ok = done.out.trailing_ok;
        last_out = Some((done.out.contexts, done.out.last_qp));
    }
    let Some((contexts, last_qp)) = last_out else {
        return Err(Error::corrupt("HEVC slice (wavefront): no row was parsed"));
    };
    if let Some(wpp) = (ry0..ry0 + nrows).rev().find_map(|ry| board.take_wpp(ry)) {
        pic.carry.wpp = Some(wpp);
    }
    pic.carry.ds = Some(contexts);
    pic.carry.last_qp = last_qp;
    Ok(stats)
}

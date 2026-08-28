//! Reference MV list construction (AV1 spec 7.10.2, `find_mv_stack`),
//! reduced to the single-reference, non-compound, `use_ref_frame_mvs = 0`
//! case a key/inter frame without temporal MV projection needs.
//!
//! Implemented from the spec: the row-above scan (7.10.2.2), the col-left
//! scan (7.10.2.3), a single top-right probe (the first of 7.10.2.4's extra
//! positions), weight-based dedupe (7.10.2.5's `add_ref_mv_candidate`), the
//! weight-descending sort (7.10.2.6), and the `NewMvContext`/`RefMvContext`/
//! `ZeroMvContext`/`DrlCtx` derivations of 7.10.2.8, ported from libaom's
//! `av1_find_mv_stack` (`av1/common/mvref_common.c`, the `mode_context`
//! switch around its `nearest_match`/`ref_match_count` and the
//! `newmv_count > 0` tests) and `av1_drl_ctx` (`mvref_common.h`), plus MV
//! clamping (7.10.2.14, libaom's `clamp_mv_ref`).
//!
//! This module's immediate row/col/top-right scan alone is *not* enough to
//! get `RefMvContext` (`ref_mv_ctx`) right, *nor* `NewMvContext` at
//! `nearest_match == 0` (see below). Ported straight from libaom's
//! `av1_find_mv_stack` (`av1/common/mvref_common.c`, `setup_ref_mv_list`,
//! ~line 480 on): after the immediate row (-1), col (-1) and top-right
//! probes settle `nearest_match` (and `mode_context`'s `new_mv` bits, which
//! for `nearest_match >= 1` depend on `nearest_match` alone), libaom runs one
//! more probe *before* computing `ref_match_count` — `scan_blk_mbmi` at the
//! diagonal top-left corner `(-1, -1)` — whose match folds into
//! `row_match_count` (and hence can flip `ref_match_count`'s row term true)
//! without ever touching the already-captured `nearest_match`. So
//! `ref_match_count >= nearest_match` always, sometimes strictly greater, and
//! `ref_mv_ctx` (the `REFMV_OFFSET` half of libaom's `mode_context` switch,
//! mvref_common.c ~line 619-643) genuinely depends on both counts, not
//! `nearest_match` alone — that collapse was the bug this module shipped
//! with for several rounds (see the crate report). The `case 0` arm of that
//! same switch *also* sets `new_mv_ctx`'s own bit from `ref_match_count`
//! (`mode_context[ref_frame] |= 1` when `ref_match_count >= 1`) — a second
//! corner-probe dependency this module missed for several more rounds after
//! the first fix, until real-content inter streams (I8 CLASS B) caught it:
//! synthetic fixtures never produced a block with `nearest_match == 0` and a
//! genuine corner match. [`find_mv_stack`] now runs that corner probe and
//! computes both `ref_mv_ctx` and `new_mv_ctx` from the true
//! `(nearest_match, ref_match_count)` pair.
//!
//! The corner probe's own candidate is added to the stack too (matching
//! libaom: a genuinely new MV there becomes a real, if low-weight, entry),
//! at libaom's real `2 * mi_size_wide[BLOCK_8X8]` (`CORNER_WEIGHT`, 4) —
//! same as the top-right probe, which shares `scan_blk_mbmi`'s fixed weight
//! in libaom too. The row/col scans (`scan_row`/`scan_col`) carry libaom's
//! real per-block-run `len * weight` formula (`scan_row_mbmi`/
//! `scan_col_mbmi`, mvref_common.c), not a flat per-cell weight: this
//! module shipped a flat `IMMEDIATE_WEIGHT = 2` for several rounds, exact
//! only for `tile.rs`'s own encoder (always a fixed `bw4 == bh4 == 8` query
//! against same-size-8 neighbours, where the real formula's `bw4 <= n4`
//! weight boost applies identically on every candidate and so never
//! reorders anything) — decode.rs's real variable partition sizes need the
//! real formula, since a same-or-wider neighbour against a *smaller* query
//! block gets a real, non-flat weight boost there libaom's DRL-index
//! selection depends on.
//!
//! The corner probe's own match does *not* count toward the real
//! `newmv_count` that feeds `new_mv_ctx` (libaom passes it a `dummy_newmv_count`
//! there) — only row/col/top-right do.
//!
//! Also ported (from `av1_drl_ctx`, `mvref_common.h`): the `REF_CAT_LEVEL`
//! (640) bonus applies only to the entries the row/col/top-right scan found
//! (`nearest_refmv_count`), added to their stored weight *before* the corner
//! probe can add a new, unboosted entry — so `drl_ctx` can now be non-zero
//! when the corner probe contributes a genuinely new candidate.
//!
//! The `GLOBALMV_OFFSET` bit of libaom's `mode_context` is set only inside
//! `cm->features.allow_ref_frame_mvs` (temporal MV projection), which this
//! module never has (`use_ref_frame_mvs = 0`), so `zero_mv_ctx` is always 0
//! here — exact for the case this module covers, not a stand-in.
//!
//! Deliberately reduced away (see the crate report, not reproduced bit-exact
//! here): compound/multi-reference candidates, the temporal MV projection
//! scan, and the single-reference "extension" pass that re-walks the same
//! row/col
//! neighbours through `process_single_ref_mv_candidate` (a no-op here: with
//! only one reference frame ever in play, whatever it would add was already
//! added by the row/col scan), and the "extra search" that pads a short
//! stack to two entries (spec 7.10.2.12) — this module's stack can be
//! shorter than two candidates where libaom's never is.

/// How many query blocks [`find_mv_stack_with_sign_bias`] has folded at
/// least one temporal MV candidate into (spec 7.9/7.10.2.8's
/// `add_tpl_ref_mv`) — the real-aomenc temporal-MV gate's own firing
/// counter (`crate::decode::tmv_hits`), proving the projection path in
/// [`crate::motion_field`] is actually reached, not just compiled.
static TMV_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The number of query blocks that received at least one temporal MV
/// candidate so far, across every [`find_mv_stack_with_sign_bias`] call in
/// this process.
pub(crate) fn tmv_hits() -> usize {
    TMV_HITS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Everything [`find_mv_stack_with_sign_bias`] needs to fold temporal MV
/// candidates into a block's stack (spec 7.10.2.8, only reached when the
/// frame header's own `use_ref_frame_mvs` is set): the current frame's own
/// projected motion field, the distance (in `get_relative_dist` units) from
/// the current frame to *this query's* single reference frame's own
/// `OrderHint` (`cur_offset_0`, spec 7.9.3 — computed by the caller, since
/// this module has no reason to carry `order_hint_bits`/`OrderHints[]`
/// itself), and `allow_high_precision_mv` (`lower_mv_precision`, spec
/// 7.10.2.8).
pub struct TplArgs<'a> {
    /// The current frame's projected temporal motion field
    /// ([`crate::motion_field::setup_motion_field`]'s output).
    pub field: &'a crate::motion_field::TplField,
    /// `get_relative_dist(OrderHint, OrderHints[ref_frame - LAST_FRAME])`,
    /// pre-computed by the caller.
    pub cur_offset_0: i32,
    /// The frame header's own `allow_high_precision_mv`.
    pub allow_high_precision_mv: bool,
}

/// One 4x4 `mi` unit's motion state, as the encode loop will have filled it
/// in by the time it asks for a block's MV stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MiInfo {
    /// Whether this unit was coded inter (as opposed to intra).
    pub is_inter: bool,
    /// The single reference frame this unit's MV points into. Ignored (and
    /// the unit contributes no candidate) when `is_inter` is `false`.
    pub ref_frame: i8,
    /// The unit's *second* reference frame when it was coded compound
    /// (spec `RefFrames[1]`), `None` for a single-reference/intra unit --
    /// compound-coded decode leaves set this. lane-av1idx r5:
    /// [`find_mv_stack`]'s single-ref row/col/corner scans (via
    /// `single_ref_match`) now also match a compound neighbour on this
    /// second field, donating that side's own MV -- the real read #538
    /// desync (a `new_mv_ctx` fed by a `newmv_count`/`nearest_match` that
    /// silently dropped a compound neighbour's vote whenever its match sat
    /// in this slot).
    pub ref_frame1: Option<i8>,
    /// The unit's motion vector, `(row, col)`, in the spec's 1/8-pel units.
    pub mv: (i32, i32),
    /// The unit's second motion vector, matching `ref_frame1` (spec
    /// `Mvs[1]`) — `Some` only for a compound-coded unit, `None` otherwise.
    pub mv1: Option<(i32, i32)>,
    /// Whether this unit's own mode was `NEWMV` (spec's
    /// `have_newmv_in_inter_mode`, compound modes excluded since this module
    /// has no compound candidates). Feeds `NewMvContext` (7.10.2.8): a
    /// neighbour's own coded-with-NEWMV state, not this block's.
    pub is_new_mv: bool,
    /// The side, in 4x4 units, of the square block this unit belongs to (8
    /// for a whole 32x32 block, 4 for a 16x16 leaf -- every block this crate
    /// ever codes is square, so one dimension is enough). Feeds the extended
    /// row/col scan's `processed_rows`/`processed_cols` (spec 7.10.2.2/.3,
    /// libaom `scan_row_mbmi`/`scan_col_mbmi`'s `inc` term): a query block no
    /// wider than its immediate neighbour has that neighbour's whole span
    /// already covered by the immediate (-1) scan, so the extended scan
    /// below only has to run past that. Set for *every* coded unit, intra or
    /// inter -- libaom's `scan_row_mbmi` advances `processed_rows` from any
    /// candidate's `bsize` regardless of whether it also casts a vote (that
    /// requires a ref-frame match, checked separately in `add_ref_mv_candidate`).
    pub size: usize,
}

/// The whole frame's `mi` grid, one [`MiInfo`] per 4x4 unit, in raster order.
/// A unit an encode pass hasn't visited yet (or that sits outside the tile)
/// reads as `None` and contributes nothing to any block's MV stack.
#[derive(Clone, Debug)]
pub struct MiGrid {
    cols: usize,
    rows: usize,
    cells: Vec<Option<MiInfo>>,
}

impl MiGrid {
    /// A grid of `cols` by `rows` 4x4 units, all initially unset.
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            cols,
            rows,
            cells: vec![None; cols * rows],
        }
    }

    /// Records the unit at `(row, col)`. Out-of-range coordinates are a
    /// no-op — the caller only ever addresses units inside the grid it built.
    pub fn set(&mut self, row: usize, col: usize, info: MiInfo) {
        if row < self.rows && col < self.cols {
            self.cells[row * self.cols + col] = Some(info);
        }
    }

    /// The unit at `(row, col)`, or `None` when it is outside the grid (the
    /// tile) or hasn't been coded.
    pub fn get(&self, row: usize, col: usize) -> Option<&MiInfo> {
        if row < self.rows && col < self.cols {
            self.cells[row * self.cols + col].as_ref()
        } else {
            None
        }
    }
}

/// One entry of the reference MV stack: a candidate motion vector and the
/// total weight the neighbours that agreed on it contributed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StackEntry {
    /// The candidate `(row, col)` motion vector.
    pub mv: (i32, i32),
    /// The summed weight of every neighbour cell that voted for this MV.
    pub weight: u32,
}

/// The result of [`find_mv_stack`]: the sorted candidate list, the two
/// predictors a `NEARESTMV`/`NEARMV` block reads from it, the predictor a
/// `NEWMV` residual is coded against, and the symbol contexts the block's
/// mode and DRL-index syntax elements are coded with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MvStack {
    /// The candidates, highest weight first (ties keep scan order: above,
    /// then left, then top-right).
    pub entries: Vec<StackEntry>,
    /// `RefStackMv[0]`, or `(0, 0)` when the stack is empty.
    pub nearest_mv: (i32, i32),
    /// `RefStackMv[1]`, or `(0, 0)` when the stack has fewer than two
    /// entries.
    pub near_mv: (i32, i32),
    /// The predictor a `NEWMV` block's MV difference is coded against:
    /// `nearest_mv` when the stack is non-empty, `(0, 0)` otherwise.
    pub pred_mv: (i32, i32),
    /// Context for the `new_mv` symbol.
    pub new_mv_ctx: usize,
    /// Context for the `ref_mv` symbol.
    pub ref_mv_ctx: usize,
    /// Context for the `zero_mv` symbol.
    pub zero_mv_ctx: usize,
    /// Context for each `drl_mode` symbol between consecutive stack
    /// entries, `entries.len().saturating_sub(1)` long.
    pub drl_ctx: Vec<usize>,
}

/// Spec 7.10.2.8's threshold a candidate's weight is compared against to
/// pick its DRL context.
const REF_CAT_LEVEL: u32 = 640;

/// The most candidates a stack keeps (spec `MAX_REF_MV_STACK_SIZE`).
const MAX_STACK_SIZE: usize = 8;

/// The single-reference extension's own, *smaller* cap (spec's
/// `MAX_MV_REF_CANDIDATES`, `av1/common/enums.h`): that pass only ever tops
/// the stack up to two entries — enough to guarantee `NEARESTMV`/`NEARMV`
/// predictors — never all the way to [`MAX_STACK_SIZE`]. libaom gates its
/// two extension loops on `*refmv_count < MAX_MV_REF_CANDIDATES`, not the
/// 8-entry stack cap; this module's prior hand-written pass used
/// `MAX_STACK_SIZE` here, which is the wrong bound.
const MAX_MV_REF_CANDIDATES: usize = 2;

/// The floor libaom's `scan_row_mbmi`/`scan_col_mbmi` weight starts at
/// before a same-or-wider candidate block can raise it (`uint16_t weight =
/// 2;`, mvref_common.c).
const ROW_COL_WEIGHT_FLOOR: u32 = 2;

/// Fixed weight libaom's `scan_blk_mbmi` uses for both single-cell probes
/// (`2 * mi_size_wide[BLOCK_8X8]` = `2 * 2`): the diagonal corner and the
/// top-right, neither of which ever runs the row/col scans' real per-block
/// weight below.
const CORNER_WEIGHT: u32 = 4;

/// Spec `MVREF_ROW_COLS`: the extended row/col scan runs at offsets `-3` and
/// `-5` (`idx = 2..=3`), one past the immediate scan's own `-1`.
const MVREF_ROW_COLS: usize = 3;

/// Adds one candidate MV with `weight` to `candidates`, merging it into an
/// existing entry with the same MV rather than duplicating it (spec
/// `add_ref_mv_candidate`, 7.10.2.5).
fn add_candidate(candidates: &mut Vec<StackEntry>, mv: (i32, i32), weight: u32) {
    if let Some(entry) = candidates.iter_mut().find(|e| e.mv == mv) {
        entry.weight += weight;
    } else {
        candidates.push(StackEntry { mv, weight });
    }
}

/// `cm->ref_frame_sign_bias[ref_frame]`, looked up from the frame header's
/// own `ref_frame_sign_bias` (spec 5.9.2's `get_relative_dist(ref_order_hint,
/// OrderHint) > 0` per active reference, already computed by
/// `ec_av1_syntax::frame::read_uncompressed_header` and indexed exactly like
/// `ref_frame_idx`: `table[ref_frame - LAST_FRAME]`). `find_mv_stack`'s
/// no-sign-bias callers (this crate's own encoder, which only ever writes
/// `LAST_FRAME`) pass an all-`false` table, matching this stub's old
/// always-`false` behaviour exactly.
fn sign_bias(table: &SignBiasTable, ref_frame: i8) -> bool {
    table[(ref_frame - LAST_FRAME) as usize]
}

/// One `bool` per `MV_REFERENCE_FRAME` past `INTRA_FRAME`/`NONE`
/// (`LAST_FRAME`..=`ALTREF_FRAME`), indexed `[ref_frame - LAST_FRAME]` --
/// spec `ec_av1_syntax::frame::FrameHeader::ref_frame_sign_bias`'s own
/// shape, threaded straight through.
pub type SignBiasTable = [bool; 7];

/// The all-`false` table every no-sign-bias caller (this crate's own
/// encoder, and every test below) passes: correct as long as no candidate
/// or query ever names a backward reference, exactly this module's old
/// hardcoded `sign_bias` stub.
pub const NO_SIGN_BIAS: SignBiasTable = [false; 7];

/// libaom's `process_single_ref_mv_candidate` (mvref_common.c): folds one
/// already-coded neighbour into the stack as a low-weight (`2`, unboosted)
/// filler entry, used only by the single-reference extension pass below.
/// Unlike every scan above, this runs over *any* inter neighbour regardless
/// of which reference frame it coded — that is the whole point of the
/// pass (topping the stack up when the real scans found too few same-ref
/// candidates) — and dedupes by MV value against the *whole* stack so far,
/// silently dropping the candidate (no weight bump, unlike
/// [`add_candidate`]) when a match is already there.
///
/// libaom's real signature loops `rf_idx` over the candidate's up to two
/// coded reference-frame slots (a compound-coded neighbour donates two
/// candidates, one per slot) -- now ported in full: a compound `candidate`
/// (`ref_frame1`/`mv1` both `Some`) donates a second entry from its second
/// slot too, each sign-flipped against its own reference frame independently.
fn process_single_ref_mv_candidate(
    candidate: &MiInfo,
    ref_frame: i8,
    sign_bias_table: &SignBiasTable,
    candidates: &mut Vec<StackEntry>,
) {
    if !candidate.is_inter {
        return;
    }
    let mut slots = [(candidate.ref_frame, candidate.mv); 2];
    let mut n_slots = 1;
    if let (Some(rf1), Some(mv1)) = (candidate.ref_frame1, candidate.mv1) {
        slots[1] = (rf1, mv1);
        n_slots = 2;
    }
    for &(rf, mv) in &slots[..n_slots] {
        let mut this_mv = mv;
        if sign_bias(sign_bias_table, rf) != sign_bias(sign_bias_table, ref_frame) {
            this_mv = (-this_mv.0, -this_mv.1);
        }
        if !candidates.iter().any(|e| e.mv == this_mv) {
            candidates.push(StackEntry {
                mv: this_mv,
                weight: 2,
            });
        }
    }
}

/// The row scan (spec 7.10.2.2, libaom `scan_row_mbmi`): walks candidate
/// *blocks* — not individual 4x4 cells — along the row `row_offset` mi units
/// above the block's own row, in run-length steps sized by each candidate's
/// own block width. Handles both the immediate probe (`row_offset == -1`)
/// and the extended ones (`-3`, `-5`) libaom threads through the same
/// function: `max_row_offset` is the immediate scan's own reach (spec
/// 7.10.2.4's `find_valid_row_offset`, clamped to the tile's top edge), used
/// only to size the weight boost below, not to gate whether this call runs
/// at all (the caller does that via `processed_rows`/the tile edge).
///
/// Each matching candidate's vote is weighted `len * weight`: `len` is how
/// many mi columns the run covers (the candidate's own width, floored to
/// the query block's `bw4` and — at the wide/extended-scan geometries
/// libaom's own `use_step_16`/`abs(row_offset) > 1` branches cover — raised
/// to a block-size floor); `weight` starts at 2 and, only when the candidate
/// is at least as wide as the query block (`bw4 <= n4`), rises to how much
/// of `max_row_offset`'s remaining reach the candidate's own height can
/// still cover. That `bw4 <= n4` condition is why this module's old flat
/// `IMMEDIATE_WEIGHT = 2` was exact for every real caller before decode.rs
/// existed: `tile.rs`'s own encoder only ever asks with `bw4 == 8` against
/// neighbours it also always wrote at `size == 8`, and every synthetic test
/// neighbour here uses `size: 1` (which the `n4 < weight`-floor condition
/// keeps at the `2` floor regardless of `bw4`) — this real formula only
/// diverges from that flat weight when a *bigger* candidate meets a
/// *smaller* query block, which only decode.rs's variable partition sizes
/// ever produce.
///
/// Threads `processed_rows` (`*mut` in libaom, `&mut` here) across every row
/// call — immediate, then the two extended offsets — the way libaom's
/// single stack `int` does: only the `bw4 <= n4` branch above updates it,
/// same as libaom only updating `*processed_rows` there.
#[allow(clippy::too_many_arguments)]
fn scan_row(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    bw4: usize,
    row_offset: isize,
    max_row_offset: isize,
    ref_frame: i8,
    candidates: &mut Vec<StackEntry>,
    newmv_count: &mut u32,
    processed_rows: &mut usize,
) -> bool {
    let Some(row) = mi_row.checked_add_signed(row_offset) else {
        return false;
    };
    let col_shift: usize = if row_offset.unsigned_abs() > 1 { 1 } else { 0 };
    let use_step_16 = bw4 >= 16;
    let end_mi = bw4.min(16);
    let mut found = false;
    let mut i = 0usize;
    while i < end_mi {
        let Some(info) = grid.get(row, mi_col + col_shift + i) else {
            i += 1;
            continue;
        };
        let n4 = info.size;
        let mut len = bw4.min(n4);
        if use_step_16 {
            len = len.max(4);
        } else if row_offset.unsigned_abs() > 1 {
            len = len.max(2);
        }
        let mut weight = ROW_COL_WEIGHT_FLOOR;
        if bw4 <= n4 {
            let inc = ((-max_row_offset + row_offset + 1) as usize).min(n4);
            weight = weight.max(inc as u32);
            *processed_rows = (inc as isize - row_offset - 1).max(0) as usize;
        }
        if let Some(mv) = single_ref_match(info, ref_frame) {
            found = true;
            add_candidate(candidates, mv, len as u32 * weight);
            *newmv_count += u32::from(info.is_new_mv);
        }
        i += len.max(1);
    }
    found
}

/// libaom's `add_ref_mv_candidate` (mvref_common.c) for a single-reference
/// query: a compound-coded neighbour still votes when *either* of its two
/// coded reference frames matches `ref_frame`, contributing that side's own
/// MV -- checked first slot then second, matching libaom's `for (ref = 0;
/// ref < 1 + is_compound; ++ref)` loop order. The row/col/corner scans below
/// used to check `info.ref_frame == ref_frame` alone, silently dropping a
/// compound neighbour's vote whenever the match sat in its *second* slot
/// (the class this crate's own compound-stack scan already had to close for
/// [`find_mv_stack_compound`] -- see the module doc).
fn single_ref_match(info: &MiInfo, ref_frame: i8) -> Option<(i32, i32)> {
    if !info.is_inter {
        return None;
    }
    if info.ref_frame == ref_frame {
        Some(info.mv)
    } else if info.ref_frame1 == Some(ref_frame) {
        info.mv1
    } else {
        None
    }
}

/// The col scan (spec 7.10.2.3, libaom `scan_col_mbmi`): `scan_row`,
/// transposed.
#[allow(clippy::too_many_arguments)]
fn scan_col(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    bh4: usize,
    col_offset: isize,
    max_col_offset: isize,
    ref_frame: i8,
    candidates: &mut Vec<StackEntry>,
    newmv_count: &mut u32,
    processed_cols: &mut usize,
) -> bool {
    let Some(col) = mi_col.checked_add_signed(col_offset) else {
        return false;
    };
    let row_shift: usize = if col_offset.unsigned_abs() > 1 { 1 } else { 0 };
    let use_step_16 = bh4 >= 16;
    let end_mi = bh4.min(16);
    let mut found = false;
    let mut i = 0usize;
    while i < end_mi {
        let Some(info) = grid.get(mi_row + row_shift + i, col) else {
            i += 1;
            continue;
        };
        let n4 = info.size;
        let mut len = bh4.min(n4);
        if use_step_16 {
            len = len.max(4);
        } else if col_offset.unsigned_abs() > 1 {
            len = len.max(2);
        }
        let mut weight = ROW_COL_WEIGHT_FLOOR;
        if bh4 <= n4 {
            let inc = ((-max_col_offset + col_offset + 1) as usize).min(n4);
            weight = weight.max(inc as u32);
            *processed_cols = (inc as isize - col_offset - 1).max(0) as usize;
        }
        if let Some(mv) = single_ref_match(info, ref_frame) {
            found = true;
            add_candidate(candidates, mv, len as u32 * weight);
            *newmv_count += u32::from(info.is_new_mv);
        }
        i += len.max(1);
    }
    found
}

/// The single top-right probe this reduction keeps of spec 7.10.2.4's extra
/// scan positions, at the unit diagonally above-right of the block. Its
/// vote is folded into the row scan's candidates (it sits on the same row
/// as the block's neighbours above), which is what makes the row and column
/// scans asymmetric under transposing a grid — spec has no matching
/// bottom-left probe.
fn scan_top_right(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    bw4: usize,
    ref_frame: i8,
    candidates: &mut Vec<StackEntry>,
    newmv_count: &mut u32,
) -> bool {
    let Some(row) = mi_row.checked_sub(1) else {
        return false;
    };
    let col = mi_col + bw4;
    if let Some(info) = grid.get(row, col)
        && let Some(mv) = single_ref_match(info, ref_frame)
    {
        add_candidate(candidates, mv, CORNER_WEIGHT);
        *newmv_count += u32::from(info.is_new_mv);
        return true;
    }
    false
}

/// The diagonal top-left corner probe (libaom `scan_blk_mbmi` at
/// `row_offset = col_offset = -1`, mvref_common.c's "second outer area",
/// called *after* `nearest_match` is already captured): the single unit
/// diagonally above-left of the block. Its match folds into the row bucket
/// too (see module doc) but must never move `nearest_match` itself or the
/// real `newmv_count` — callers pass a throwaway counter for the latter.
fn scan_corner(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    ref_frame: i8,
    candidates: &mut Vec<StackEntry>,
    newmv_count: &mut u32,
) -> bool {
    let (Some(row), Some(col)) = (mi_row.checked_sub(1), mi_col.checked_sub(1)) else {
        return false;
    };
    if let Some(info) = grid.get(row, col)
        && let Some(mv) = single_ref_match(info, ref_frame)
    {
        add_candidate(candidates, mv, CORNER_WEIGHT);
        *newmv_count += u32::from(info.is_new_mv);
        return true;
    }
    false
}

/// `MV_BORDER` (spec 7.10.2.14): 16 pixels of slack in 1/8-pel units, added
/// on top of the block's own span before a candidate is clamped to the
/// frame.
const MV_BORDER: i32 = 16 << 3;

/// Clamps one candidate MV to the range spec 7.10.2.14 (libaom's
/// `clamp_mv_ref`) allows for a `bw4`-by-`bh4` block at `(mi_row, mi_col)` in
/// a `mi_cols`-by-`mi_rows` frame: the distance from the block's own edge to
/// the matching frame edge, plus the block's own span and [`MV_BORDER`] of
/// slack, in every direction.
fn clamp_mv_ref(
    mv: (i32, i32),
    mi_row: usize,
    mi_col: usize,
    bw4: usize,
    bh4: usize,
    mi_cols: usize,
    mi_rows: usize,
) -> (i32, i32) {
    // Pixels-to-1/8-pel: 4 mi units/pixel-row * 8 subpel steps/pixel.
    let to_subpel = |mi_units: usize| (mi_units as i32) * 4 * 8;
    let (bw8, bh8) = (to_subpel(bw4), to_subpel(bh4));
    let to_left = -to_subpel(mi_col);
    let to_right = to_subpel(mi_cols.saturating_sub(bw4).saturating_sub(mi_col));
    let to_top = -to_subpel(mi_row);
    let to_bottom = to_subpel(mi_rows.saturating_sub(bh4).saturating_sub(mi_row));

    let (row, col) = mv;
    (
        row.clamp(to_top - bh8 - MV_BORDER, to_bottom + bh8 + MV_BORDER),
        col.clamp(to_left - bw8 - MV_BORDER, to_right + bw8 + MV_BORDER),
    )
}

/// Builds the reference MV stack for the `bw4`-by-`bh4` (in 4x4 units)
/// block at `(mi_row, mi_col)`, predicting against `ref_frame`, in a frame
/// that is `mi_cols`-by-`mi_rows` 4x4 units (used only to clamp the
/// predictors to the frame, spec 7.10.2.14).
pub fn find_mv_stack(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    bw4: usize,
    bh4: usize,
    ref_frame: i8,
    mi_cols: usize,
    mi_rows: usize,
) -> MvStack {
    find_mv_stack_with_sign_bias(
        grid,
        mi_row,
        mi_col,
        bw4,
        bh4,
        ref_frame,
        mi_cols,
        mi_rows,
        &NO_SIGN_BIAS,
        None,
    )
}

/// [`find_mv_stack`], with the frame header's real `ref_frame_sign_bias`
/// (lane-av1refs: `BWDREF_FRAME`/`ALTREF2_FRAME`/`ALTREF_FRAME` are forward
/// in display order, so the single-reference extension pass's borrowed-MV
/// flip is no longer always inert once one of them is live next to a
/// backward-biased neighbour).
#[allow(clippy::too_many_arguments)]
pub fn find_mv_stack_with_sign_bias(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    bw4: usize,
    bh4: usize,
    ref_frame: i8,
    mi_cols: usize,
    mi_rows: usize,
    sign_bias_table: &SignBiasTable,
    tpl: Option<TplArgs>,
) -> MvStack {
    let mut candidates = Vec::new();
    let mut newmv_count = 0u32;

    // libaom's `max_row_offset`/`max_col_offset` (`setup_ref_mv_list`): how
    // far the row/col scans may reach, clamped to the tile's near edge
    // (single-tile-per-frame here, so that's just the frame edge at 0) --
    // `row_adj`/`col_adj` only ever fire for a sub-8x8 query block, which no
    // real caller of this module ever is, but are ported anyway since
    // nothing here relies on that.
    let row_adj = bh4 < 2 && mi_row % 2 == 1;
    let col_adj = bw4 < 2 && mi_col % 2 == 1;
    let max_row_offset: isize = if mi_row > 0 {
        let reach = if bh4 < 2 { -4 } else { -6 } + isize::from(row_adj);
        reach.max(-(mi_row as isize))
    } else {
        0
    };
    let max_col_offset: isize = if mi_col > 0 {
        let reach = if bw4 < 2 { -4 } else { -6 } + isize::from(col_adj);
        reach.max(-(mi_col as isize))
    } else {
        0
    };

    let mut processed_rows = 0usize;
    let mut processed_cols = 0usize;
    let found_above = mi_row > 0
        && scan_row(
            grid,
            mi_row,
            mi_col,
            bw4,
            -1,
            max_row_offset,
            ref_frame,
            &mut candidates,
            &mut newmv_count,
            &mut processed_rows,
        );
    let found_left = mi_col > 0
        && scan_col(
            grid,
            mi_row,
            mi_col,
            bh4,
            -1,
            max_col_offset,
            ref_frame,
            &mut candidates,
            &mut newmv_count,
            &mut processed_cols,
        );
    let found_top_right = scan_top_right(
        grid,
        mi_row,
        mi_col,
        bw4,
        ref_frame,
        &mut candidates,
        &mut newmv_count,
    );

    // libaom's `row_match_count`/`col_match_count` (mvref_common.c) fold the
    // top-right probe into the row side, so "found above" for context
    // purposes means the row scan *or* the top-right one matched.
    let row_matched = found_above || found_top_right;
    let nearest_match = usize::from(row_matched) + usize::from(found_left);

    // av1_find_mv_stack boosts only the entries the immediate scan found —
    // exactly `candidates` at this point, before the corner probe (below)
    // can add anything else (mvref_common.c ~line 544-545).
    for entry in &mut candidates {
        entry.weight += REF_CAT_LEVEL;
    }

    // Temporal MV candidates (spec 7.10.2.8's `add_tpl_ref_mv`, only reached
    // when the frame header set `use_ref_frame_mvs`): probed over a grid of
    // 8x8-unit offsets spanning the block (libaom's own `blk_row_end`/
    // `blk_col_end`/`step_h`/`step_w`, every real block this crate ever
    // codes being square and no wider than 32x32 — `BLOCK_64X64`'s own
    // 16x16-mi-unit step branch is dead here, ported anyway since nothing
    // relies on that), plus three "extension" samples past the block's own
    // span (`tpl_sample_pos`) once the block is at least 8x8 and smaller
    // than 64x64 (`allow_extension` — true for every block size this crate
    // codes). Folded in *after* the row/col/top-right scan's own
    // `REF_CAT_LEVEL` boost above and *before* the corner probe, matching
    // libaom's call order in `setup_ref_mv_list` exactly (a temporal
    // candidate never gets the boost, and the corner probe can still add a
    // genuinely new, lower-weight entry after it).
    let mut zero_mv_ctx = 0usize;
    if let Some(TplArgs {
        field,
        cur_offset_0,
        allow_high_precision_mv,
    }) = tpl
    {
        let mut any_hit = false;
        let voffset = bh4.max(2);
        let hoffset = bw4.max(2);
        let blk_row_end = bh4.min(16);
        let blk_col_end = bw4.min(16);
        let step_h = if bh4 >= 16 { 4 } else { 2 };
        let step_w = if bw4 >= 16 { 4 } else { 2 };
        let mut first_sample_missing = true;
        let mut first_sample_far = false;
        let mut blk_row = 0usize;
        while blk_row < blk_row_end {
            let mut blk_col = 0usize;
            while blk_col < blk_col_end {
                if let Some(cand) = crate::motion_field::add_tpl_ref_mv(
                    field,
                    mi_row,
                    mi_col,
                    blk_row as isize,
                    blk_col as isize,
                    cur_offset_0,
                    allow_high_precision_mv,
                ) {
                    any_hit = true;
                    if blk_row == 0 && blk_col == 0 {
                        first_sample_missing = false;
                        first_sample_far = cand.mv.0.abs() >= 16 || cand.mv.1.abs() >= 16;
                    }
                    add_candidate(&mut candidates, cand.mv, 2);
                }
                blk_col += step_w;
            }
            blk_row += step_h;
        }
        // Three "extension" samples past the block's own span (libaom
        // `tpl_sample_pos`), reachable for every 8x8..32x32 block this crate
        // codes (`allow_extension`), gated on staying inside the block's own
        // 64x64 superblock (`check_sb_border`).
        let allow_extension = (2..16).contains(&bh4) && (2..16).contains(&bw4);
        if allow_extension {
            let sb_mask = 16isize;
            for (row_off, col_off) in [
                (voffset as isize, -2isize),
                (voffset as isize, hoffset as isize),
                (voffset as isize - 2, hoffset as isize),
            ] {
                let row = (mi_row as isize & (sb_mask - 1)) + row_off;
                let col = (mi_col as isize & (sb_mask - 1)) + col_off;
                if row < 0 || row >= sb_mask || col < 0 || col >= sb_mask {
                    continue;
                }
                if let Some(cand) = crate::motion_field::add_tpl_ref_mv(
                    field,
                    mi_row,
                    mi_col,
                    row_off,
                    col_off,
                    cur_offset_0,
                    allow_high_precision_mv,
                ) {
                    any_hit = true;
                    add_candidate(&mut candidates, cand.mv, 2);
                }
            }
        }
        if any_hit {
            TMV_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        zero_mv_ctx = usize::from(first_sample_missing || first_sample_far);
    }

    // The diagonal top-left corner probe (module doc): folds into the row
    // side for `ref_match_count` without moving the already-captured
    // `nearest_match`, and its own newmv-ness is a dummy count libaom
    // discards (`&dummy_newmv_count` at the real call site).
    let mut dummy_newmv_count = 0u32;
    let corner_matched = scan_corner(
        grid,
        mi_row,
        mi_col,
        ref_frame,
        &mut candidates,
        &mut dummy_newmv_count,
    );
    // The extended row/col scan (spec 7.10.2.4's `idx = 2..MVREF_ROW_COLS`,
    // libaom's second `scan_row_mbmi`/`scan_col_mbmi` loop): reachable only
    // once the immediate scan's own reach (`processed_rows`/`processed_cols`,
    // threaded across calls the way libaom's own `int`s are) leaves a gap
    // `max_row_offset`/`max_col_offset` (the tile edge) doesn't already
    // clip. At this module's fixed 8-mi `tile.rs` geometry with every coded
    // cell (intra or inter) feeding coverage, that reach always hits
    // `MVREF_ROW_COLS`'s farthest offset first -- a true no-op there (see
    // `extended_scan_is_a_no_op_at_8mi_geometry` below); decode.rs's smaller
    // query blocks can leave real gaps this now actually scans. Folds into
    // the row/col match booleans, never `nearest_match`; shares the corner
    // probe's `dummy_newmv_count` (libaom's `setup_ref_mv_list` does too).
    let mut row_matched_ext = false;
    let mut col_matched_ext = false;
    for idx in 2..=MVREF_ROW_COLS {
        let row_offset = -((idx as isize) * 2) + 1 + isize::from(row_adj);
        let col_offset = -((idx as isize) * 2) + 1 + isize::from(col_adj);
        if row_offset.unsigned_abs() <= max_row_offset.unsigned_abs()
            && row_offset.unsigned_abs() > processed_rows
            && scan_row(
                grid,
                mi_row,
                mi_col,
                bw4,
                row_offset,
                max_row_offset,
                ref_frame,
                &mut candidates,
                &mut dummy_newmv_count,
                &mut processed_rows,
            )
        {
            row_matched_ext = true;
        }
        if col_offset.unsigned_abs() <= max_col_offset.unsigned_abs()
            && col_offset.unsigned_abs() > processed_cols
            && scan_col(
                grid,
                mi_row,
                mi_col,
                bh4,
                col_offset,
                max_col_offset,
                ref_frame,
                &mut candidates,
                &mut dummy_newmv_count,
                &mut processed_cols,
            )
        {
            col_matched_ext = true;
        }
    }
    let ref_match_count = usize::from(row_matched || corner_matched || row_matched_ext)
        + usize::from(found_left || col_matched_ext);

    // Highest weight first; a stable sort keeps scan order (above, left,
    // top-right, corner) among ties, matching spec 7.10.2.6. The corner
    // probe's un-boosted entry (if new) always sorts after the boosted
    // ones above, matching libaom's separate two-phase ranking.
    candidates.sort_by_key(|e| std::cmp::Reverse(e.weight));
    candidates.truncate(MAX_STACK_SIZE);

    // libaom's "single reference frame extension" (`setup_ref_mv_list`,
    // mvref_common.c, the `else` branch under "Handle single reference
    // frame extension", calling `process_single_ref_mv_candidate`): when
    // fewer than [`MAX_MV_REF_CANDIDATES`] (2, *not* `MAX_STACK_SIZE`) survive
    // the scans above, top the stack up to that guarantee by re-walking the
    // row directly above and the column directly left, gated on
    // `refmv_count < MAX_MV_REF_CANDIDATES` per step (so it can stop after
    // just one addition) -- over *any* inter neighbour regardless of which
    // reference frame it coded, not just `ref_frame`. This module's doc
    // used to call this pass a no-op ("whatever it would add was already
    // added by the row/col scan"), true only while a stream ever has one
    // live reference frame: once a second reference (`GOLDEN_FRAME`) is
    // live next to `LAST_FRAME` blocks, this pass pulls in the other
    // reference's MV as a low-weight (`2`, unboosted -- below
    // `REF_CAT_LEVEL`) filler entry purely to make `entries.len()` (and
    // hence how many `drl_mode` bits a `NEWMV` block reads) match the real
    // bitstream -- round 10 (lane-av1golden8) traced a `GOLDEN_FRAME`
    // `NEWMV` block desyncing its own `read_mv` by exactly this gap (real
    // stack `len=2`, this module's `len=1`, one missing `drl_mode` bit).
    // Round 11 replaced that round's hand-proved reduction (a bespoke
    // re-scan gated on `MAX_STACK_SIZE`, weighing every hit `2` unconditionally)
    // with this exact port: real gate is `MAX_MV_REF_CANDIDATES` (2), real
    // reach is the block's own `bw4`/`bh4` (see `mi_size` below, not a flat
    // 16), and dedup/weight/sign-bias now live in
    // [`process_single_ref_mv_candidate`] itself, ported from libaom's
    // function of the same name rather than re-derived by hand.
    // libaom's `mi_size` for this pass (`setup_ref_mv_list`): the block's
    // own span (`xd->width`/`xd->height`, i.e. `bw4`/`bh4` here), capped at
    // a 64x64 superblock's 16 mi units, further clipped to the frame edge —
    // never just "16, clipped to the frame edge" the way the prior
    // hand-written pass had it, which over-walked past a small query
    // block's own row/col run into cells that real neighbour scan (a wider
    // block covering more than `bw4`/`bh4`) had already folded in above.
    let mi_width = (16usize).min(bw4).min(mi_cols.saturating_sub(mi_col));
    let mi_height = (16usize).min(bh4).min(mi_rows.saturating_sub(mi_row));
    let mi_size = mi_width.min(mi_height);
    if max_row_offset.unsigned_abs() >= 1 {
        let mut idx = 0usize;
        while idx < mi_size && candidates.len() < MAX_MV_REF_CANDIDATES {
            let cand = grid.get(mi_row - 1, mi_col + idx);
            let step = cand.map_or(1, |c| c.size).max(1);
            if let Some(c) = cand {
                process_single_ref_mv_candidate(c, ref_frame, sign_bias_table, &mut candidates);
            }
            idx += step;
        }
    }
    if max_col_offset.unsigned_abs() >= 1 {
        let mut idx = 0usize;
        while idx < mi_size && candidates.len() < MAX_MV_REF_CANDIDATES {
            let cand = grid.get(mi_row + idx, mi_col - 1);
            let step = cand.map_or(1, |c| c.size).max(1);
            if let Some(c) = cand {
                process_single_ref_mv_candidate(c, ref_frame, sign_bias_table, &mut candidates);
            }
            idx += step;
        }
    }

    let clamp = |mv| clamp_mv_ref(mv, mi_row, mi_col, bw4, bh4, mi_cols, mi_rows);
    // libaom `av1_find_mv_refs` clamps EVERY stack entry once the stack is
    // built -- the NEWMV predictor is the DRL-selected entry itself, so a
    // raw out-of-range candidate (left neighbour coding past the frame
    // edge) must not survive here (pinned warp-flake-5.obu: pred col 520
    // vs libaom's clamped 384, a value-equal-entropy mv drift).
    for e in candidates.iter_mut() {
        e.mv = clamp(e.mv);
    }
    let nearest_mv = candidates.first().map_or((0, 0), |e| clamp(e.mv));
    let near_mv = candidates.get(1).map_or((0, 0), |e| clamp(e.mv));
    let pred_mv = if candidates.is_empty() {
        (0, 0)
    } else {
        nearest_mv
    };

    // Exact port of the `mode_context[ref_frame]` switch (mvref_common.c
    // ~line 619-643). `new_mv_ctx` depends on `nearest_match` and the real
    // `newmv_count` alone — libaom's switch never reads `ref_match_count`
    // for those bits, so this half was already exact before this round.
    let new_mv_ctx = match nearest_match {
        // libaom's `case 0` also sets this bit from `ref_match_count`
        // (`mode_context[ref_frame] |= 1` when `ref_match_count >= 1`,
        // mvref_common.c ~line 621) — the corner-probe-derived count, not
        // `nearest_match` alone, despite this module's doc comment above
        // having claimed the opposite for several rounds.
        0 => usize::from(ref_match_count >= 1),
        1 => {
            if newmv_count > 0 {
                2
            } else {
                3
            }
        }
        _ => {
            if newmv_count >= 1 {
                4
            } else {
                5
            }
        }
    };
    // `ref_mv_ctx` (the `REFMV_OFFSET` half of the same switch) genuinely
    // depends on both counts — this is the part that was wrongly collapsed
    // to `nearest_match` alone (see module doc / crate report).
    let ref_mv_ctx = match nearest_match {
        0 => match ref_match_count {
            0 => 0,
            1 => 1,
            _ => 2,
        },
        1 => {
            if ref_match_count >= 2 {
                4
            } else {
                3
            }
        }
        _ => 5,
    };
    // `zero_mv_ctx` was already computed above, from the temporal-MV pass
    // when `tpl` is `Some` (GLOBALMV_OFFSET, spec 7.10.2.8) — `0` otherwise,
    // exactly this reduction's old always-0 behaviour when
    // `use_ref_frame_mvs` is unset.

    // av1_drl_ctx (mvref_common.h): weight was boosted in place above for
    // the nearest entries only, so this now compares the real stored weight
    // against the threshold directly, the way libaom does.
    let drl_ctx = candidates
        .windows(2)
        .map(|w| {
            let (a, b) = (w[0].weight, w[1].weight);
            if a >= REF_CAT_LEVEL && b >= REF_CAT_LEVEL {
                0
            } else if a >= REF_CAT_LEVEL && b < REF_CAT_LEVEL {
                1
            } else {
                2
            }
        })
        .collect();

    MvStack {
        entries: candidates,
        nearest_mv,
        near_mv,
        pred_mv,
        new_mv_ctx,
        ref_mv_ctx,
        zero_mv_ctx,
        drl_ctx,
    }
}

/// One neighbour's vote for a compound-reference (spec `RefFrame[0]` *and*
/// `RefFrame[1]` both active) candidate: two full motion vectors and the
/// combined weight — spec 7.10.2's compound half of `add_ref_mv_candidate`
/// (libaom `mvref_common.c`, the `is_compound` branch of `scan_row_mbmi`/
/// `scan_col_mbmi`/`scan_blk_mbmi`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompoundStackEntry {
    /// The candidate's vote for `RefFrame[0]`'s motion vector.
    pub mv0: (i32, i32),
    /// The candidate's vote for `RefFrame[1]`'s motion vector.
    pub mv1: (i32, i32),
    /// The summed weight of every neighbour cell that voted for this pair.
    pub weight: u32,
}

/// [`find_mv_stack_compound`]'s result — [`MvStack`]'s shape, doubled for
/// the reference pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompoundMvStack {
    /// The candidates, highest weight first.
    pub entries: Vec<CompoundStackEntry>,
    /// `RefStackMv[0]` for both refs, or `((0,0),(0,0))` when the stack is
    /// empty.
    pub nearest_mv: ((i32, i32), (i32, i32)),
    /// `RefStackMv[1]` for both refs, or `((0,0),(0,0))` when the stack has
    /// fewer than two entries.
    pub near_mv: ((i32, i32), (i32, i32)),
    /// The predictor pair a `NEW_NEWMV`-family block's MV differences are
    /// coded against.
    pub pred_mv: ((i32, i32), (i32, i32)),
    /// Context for the compound `new_mv` symbol.
    pub new_mv_ctx: usize,
    /// Context for the compound `ref_mv` symbol.
    pub ref_mv_ctx: usize,
    /// Context for the compound `zero_mv` symbol.
    pub zero_mv_ctx: usize,
    /// Context for each `drl_mode` symbol between consecutive stack entries.
    pub drl_ctx: Vec<usize>,
}

/// [`add_candidate`], doubled: dedupes on the whole `(mv0, mv1)` pair.
fn add_compound_candidate(
    candidates: &mut Vec<CompoundStackEntry>,
    mv0: (i32, i32),
    mv1: (i32, i32),
    weight: u32,
) {
    if let Some(entry) = candidates.iter_mut().find(|e| e.mv0 == mv0 && e.mv1 == mv1) {
        entry.weight += weight;
    } else {
        candidates.push(CompoundStackEntry { mv0, mv1, weight });
    }
}

/// The compound row scan (spec 7.10.2.2's compound branch, libaom
/// `scan_row_mbmi`'s `is_compound` path): [`scan_row`]'s weight/run-length
/// math unchanged, but a candidate only votes when *both* its `ref_frame`
/// and `ref_frame1` equal the query's pair exactly, in that order — a
/// compound pair is always encoded forward-then-backward
/// (`RefFrame[0]`/`RefFrame[1]`), so unlike [`process_single_ref_mv_candidate`]'s
/// extension pass, no sign-bias flip ever applies to an exact-pair scan
/// match (libaom's own `scan_row_mbmi` compound branch doesn't flip either).
#[allow(clippy::too_many_arguments)]
fn scan_row_compound(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    bw4: usize,
    row_offset: isize,
    max_row_offset: isize,
    ref_frame: (i8, i8),
    candidates: &mut Vec<CompoundStackEntry>,
    newmv_count: &mut u32,
    processed_rows: &mut usize,
) -> bool {
    let Some(row) = mi_row.checked_add_signed(row_offset) else {
        return false;
    };
    let col_shift: usize = if row_offset.unsigned_abs() > 1 { 1 } else { 0 };
    let use_step_16 = bw4 >= 16;
    let end_mi = bw4.min(16);
    let mut found = false;
    let mut i = 0usize;
    while i < end_mi {
        let Some(info) = grid.get(row, mi_col + col_shift + i) else {
            i += 1;
            continue;
        };
        let n4 = info.size;
        let mut len = bw4.min(n4);
        if use_step_16 {
            len = len.max(4);
        } else if row_offset.unsigned_abs() > 1 {
            len = len.max(2);
        }
        let mut weight = ROW_COL_WEIGHT_FLOOR;
        if bw4 <= n4 {
            let inc = ((-max_row_offset + row_offset + 1) as usize).min(n4);
            weight = weight.max(inc as u32);
            *processed_rows = (inc as isize - row_offset - 1).max(0) as usize;
        }
        if info.is_inter && info.ref_frame == ref_frame.0 && info.ref_frame1 == Some(ref_frame.1) {
            found = true;
            add_compound_candidate(
                candidates,
                info.mv,
                info.mv1.unwrap_or((0, 0)),
                len as u32 * weight,
            );
            *newmv_count += u32::from(info.is_new_mv);
        }
        i += len.max(1);
    }
    found
}

/// The compound col scan (spec 7.10.2.3's compound branch): [`scan_row_compound`],
/// transposed — mirrors [`scan_col`]/[`scan_row`]'s own relationship.
#[allow(clippy::too_many_arguments)]
fn scan_col_compound(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    bh4: usize,
    col_offset: isize,
    max_col_offset: isize,
    ref_frame: (i8, i8),
    candidates: &mut Vec<CompoundStackEntry>,
    newmv_count: &mut u32,
    processed_cols: &mut usize,
) -> bool {
    let Some(col) = mi_col.checked_add_signed(col_offset) else {
        return false;
    };
    let row_shift: usize = if col_offset.unsigned_abs() > 1 { 1 } else { 0 };
    let use_step_16 = bh4 >= 16;
    let end_mi = bh4.min(16);
    let mut found = false;
    let mut i = 0usize;
    while i < end_mi {
        let Some(info) = grid.get(mi_row + row_shift + i, col) else {
            i += 1;
            continue;
        };
        let n4 = info.size;
        let mut len = bh4.min(n4);
        if use_step_16 {
            len = len.max(4);
        } else if col_offset.unsigned_abs() > 1 {
            len = len.max(2);
        }
        let mut weight = ROW_COL_WEIGHT_FLOOR;
        if bh4 <= n4 {
            let inc = ((-max_col_offset + col_offset + 1) as usize).min(n4);
            weight = weight.max(inc as u32);
            *processed_cols = (inc as isize - col_offset - 1).max(0) as usize;
        }
        if info.is_inter && info.ref_frame == ref_frame.0 && info.ref_frame1 == Some(ref_frame.1) {
            found = true;
            add_compound_candidate(
                candidates,
                info.mv,
                info.mv1.unwrap_or((0, 0)),
                len as u32 * weight,
            );
            *newmv_count += u32::from(info.is_new_mv);
        }
        i += len.max(1);
    }
    found
}

/// The compound top-right probe ([`scan_top_right`]'s pair-matched twin).
fn scan_top_right_compound(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    bw4: usize,
    ref_frame: (i8, i8),
    candidates: &mut Vec<CompoundStackEntry>,
    newmv_count: &mut u32,
) -> bool {
    let Some(row) = mi_row.checked_sub(1) else {
        return false;
    };
    let col = mi_col + bw4;
    if let Some(info) = grid.get(row, col)
        && info.is_inter
        && info.ref_frame == ref_frame.0
        && info.ref_frame1 == Some(ref_frame.1)
    {
        add_compound_candidate(
            candidates,
            info.mv,
            info.mv1.unwrap_or((0, 0)),
            CORNER_WEIGHT,
        );
        *newmv_count += u32::from(info.is_new_mv);
        return true;
    }
    false
}

/// The compound diagonal corner probe ([`scan_corner`]'s pair-matched twin).
fn scan_corner_compound(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    ref_frame: (i8, i8),
    candidates: &mut Vec<CompoundStackEntry>,
    newmv_count: &mut u32,
) -> bool {
    let (Some(row), Some(col)) = (mi_row.checked_sub(1), mi_col.checked_sub(1)) else {
        return false;
    };
    if let Some(info) = grid.get(row, col)
        && info.is_inter
        && info.ref_frame == ref_frame.0
        && info.ref_frame1 == Some(ref_frame.1)
    {
        add_compound_candidate(
            candidates,
            info.mv,
            info.mv1.unwrap_or((0, 0)),
            CORNER_WEIGHT,
        );
        *newmv_count += u32::from(info.is_new_mv);
        return true;
    }
    false
}

/// Per-side unresolved-MV accumulator [`process_compound_ref_mv_candidate`]
/// fills across every candidate the compound extension pass walks: up to two
/// motion vectors that matched `ref_frame.0`/`.1` exactly (`ref_id`), and up
/// to two more borrowed from a differently-referenced but still-inter
/// neighbour under the sign-bias flip [`process_single_ref_mv_candidate`]
/// also applies (`ref_diff`). Ported from libaom's `setup_ref_mv_list`
/// (`mvref_common.c`, `process_compound_ref_mv_candidate` ~line 380-430 plus
/// the combine step at ~696-762): the extension walk fills this once across
/// the whole row-then-col scan, and [`combine_compound_candidates`] below
/// zips the two independently-filled sides back into full pair candidates.
#[derive(Default)]
struct CompoundRefLists {
    ref_id: [Vec<(i32, i32)>; 2],
    ref_diff: [Vec<(i32, i32)>; 2],
}

/// libaom's `process_compound_ref_mv_candidate`: folds one already-coded
/// neighbour's up to two reference slots into `lists`, split by which side
/// of `ref_frame` (the query block's own compound pair) each slot's own
/// reference frame matches — an exact match goes to that side's `ref_id`
/// list, anything else (still inter, wrong reference) goes to `ref_diff`
/// under the usual sign-bias flip.
fn process_compound_ref_mv_candidate(
    candidate: &MiInfo,
    ref_frame: (i8, i8),
    sign_bias_table: &SignBiasTable,
    lists: &mut CompoundRefLists,
) {
    if !candidate.is_inter {
        return;
    }
    let slots = [
        Some((candidate.ref_frame, candidate.mv)),
        candidate
            .ref_frame1
            .map(|rf1| (rf1, candidate.mv1.unwrap_or((0, 0)))),
    ];
    for (candidate_ref, candidate_mv) in slots.into_iter().flatten() {
        for (i, side_ref) in [ref_frame.0, ref_frame.1].into_iter().enumerate() {
            if candidate_ref == side_ref {
                if lists.ref_id[i].len() < 2 {
                    lists.ref_id[i].push(candidate_mv);
                }
            } else if lists.ref_diff[i].len() < 2 {
                let mut mv = candidate_mv;
                if sign_bias(sign_bias_table, candidate_ref) != sign_bias(sign_bias_table, side_ref)
                {
                    mv = (-mv.0, -mv.1);
                }
                lists.ref_diff[i].push(mv);
            }
        }
    }
}

/// libaom's combine step (`setup_ref_mv_list`, `mvref_common.c` ~696-720):
/// zips each side's `ref_id` (exact matches) then `ref_diff` (borrowed,
/// sign-flipped) entries, position-wise and independently per side, into up
/// to two full `(mv0, mv1)` pair candidates — missing slots default to
/// `(0, 0)`, matching libaom's zero-initialized `combined_mvs`.
fn combine_compound_candidates(lists: &CompoundRefLists) -> [((i32, i32), (i32, i32)); 2] {
    let side_slots = |side: usize| -> [(i32, i32); 2] {
        let merged: Vec<(i32, i32)> = lists.ref_id[side]
            .iter()
            .chain(lists.ref_diff[side].iter())
            .take(2)
            .copied()
            .collect();
        std::array::from_fn(|i| merged.get(i).copied().unwrap_or((0, 0)))
    };
    let side0 = side_slots(0);
    let side1 = side_slots(1);
    [(side0[0], side1[0]), (side0[1], side1[1])]
}

/// [`find_mv_stack_with_sign_bias`]'s compound-reference twin (spec
/// 7.10.2's `is_compound` path throughout): builds the reference MV stack
/// for a block predicted against *two* simultaneously-active reference
/// frames (`ref_frame.0`, `ref_frame.1`), keeping every existing
/// single-reference caller of [`find_mv_stack`]/[`find_mv_stack_with_sign_bias`]
/// untouched — this is a new, parallel entry point, not an extension of the
/// single-ref one. Same reduction scope as the single-ref module doc
/// (single tile, square blocks, `IDENTITY` global motion only); the
/// mode-context switch below reuses the identical `(nearest_match,
/// ref_match_count)` derivation the single-ref path uses, since libaom's own
/// switch statement (`mvref_common.c` ~619-643) is ref-count-agnostic — it
/// only ever reads those two counts, regardless of which `MODE_CTX_REF_FRAMES`
/// slot `mode_context` it's indexing.
///
/// The compound extension combine step (`setup_ref_mv_list`, ~line 700-754)
/// and the temporal `add_tpl_ref_mv` fold-in (~line 341-433) are both exact
/// ports of libaom's real behaviour as of r5 — see the inline comments at
/// each site.
#[allow(clippy::too_many_arguments)]
pub fn find_mv_stack_compound(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    bw4: usize,
    bh4: usize,
    ref_frame: (i8, i8),
    mi_cols: usize,
    mi_rows: usize,
    sign_bias_table: &SignBiasTable,
    tpl: Option<CompoundTplArgs>,
) -> CompoundMvStack {
    let mut candidates: Vec<CompoundStackEntry> = Vec::new();
    let mut newmv_count = 0u32;

    let row_adj = bh4 < 2 && mi_row % 2 == 1;
    let col_adj = bw4 < 2 && mi_col % 2 == 1;
    let max_row_offset: isize = if mi_row > 0 {
        let reach = if bh4 < 2 { -4 } else { -6 } + isize::from(row_adj);
        reach.max(-(mi_row as isize))
    } else {
        0
    };
    let max_col_offset: isize = if mi_col > 0 {
        let reach = if bw4 < 2 { -4 } else { -6 } + isize::from(col_adj);
        reach.max(-(mi_col as isize))
    } else {
        0
    };

    let mut processed_rows = 0usize;
    let mut processed_cols = 0usize;
    let found_above = mi_row > 0
        && scan_row_compound(
            grid,
            mi_row,
            mi_col,
            bw4,
            -1,
            max_row_offset,
            ref_frame,
            &mut candidates,
            &mut newmv_count,
            &mut processed_rows,
        );
    let found_left = mi_col > 0
        && scan_col_compound(
            grid,
            mi_row,
            mi_col,
            bh4,
            -1,
            max_col_offset,
            ref_frame,
            &mut candidates,
            &mut newmv_count,
            &mut processed_cols,
        );
    let found_top_right = scan_top_right_compound(
        grid,
        mi_row,
        mi_col,
        bw4,
        ref_frame,
        &mut candidates,
        &mut newmv_count,
    );

    let row_matched = found_above || found_top_right;
    let nearest_match = usize::from(row_matched) + usize::from(found_left);

    for entry in &mut candidates {
        entry.weight += REF_CAT_LEVEL;
    }

    // Temporal compound candidates (spec 7.10.2.8's compound
    // `add_tpl_ref_mv`): the same projected field, read once per side of the
    // pair through the existing single-ref [`crate::motion_field::add_tpl_ref_mv`]
    // — each side's own `cur_offset` scales the *same* stored candidate to
    // that side's reference frame. libaom's real `add_tpl_ref_mv` looks up
    // the stored `mfmv0`/`ref_frame_offset` pair exactly once per `(blk_row,
    // blk_col)` and, if present, always projects it to *both* sides (the
    // lookup itself never fails for one side and succeeds for the other);
    // a missing side there is `INVALID_MV`/global-motion zero-fill, never a
    // dropped candidate. Ported exactly: each side is read independently and
    // a missing side zero-fills rather than vetoing the pair.
    let mut zero_mv_ctx = 0usize;
    if let Some(CompoundTplArgs {
        field,
        cur_offset_0,
        cur_offset_1,
        allow_high_precision_mv,
    }) = tpl
    {
        let mut any_hit = false;
        let blk_row_end = bh4.min(16);
        let blk_col_end = bw4.min(16);
        let step_h = if bh4 >= 16 { 4 } else { 2 };
        let step_w = if bw4 >= 16 { 4 } else { 2 };
        let mut first_sample_missing = true;
        let mut first_sample_far = false;
        let mut blk_row = 0usize;
        while blk_row < blk_row_end {
            let mut blk_col = 0usize;
            while blk_col < blk_col_end {
                let cand0 = crate::motion_field::add_tpl_ref_mv(
                    field,
                    mi_row,
                    mi_col,
                    blk_row as isize,
                    blk_col as isize,
                    cur_offset_0,
                    allow_high_precision_mv,
                );
                let cand1 = crate::motion_field::add_tpl_ref_mv(
                    field,
                    mi_row,
                    mi_col,
                    blk_row as isize,
                    blk_col as isize,
                    cur_offset_1,
                    allow_high_precision_mv,
                );
                if cand0.is_some() || cand1.is_some() {
                    any_hit = true;
                    let mv0 = cand0.map_or((0, 0), |c| c.mv);
                    let mv1 = cand1.map_or((0, 0), |c| c.mv);
                    if blk_row == 0 && blk_col == 0 {
                        first_sample_missing = false;
                        first_sample_far = mv0.0.abs() >= 16
                            || mv0.1.abs() >= 16
                            || mv1.0.abs() >= 16
                            || mv1.1.abs() >= 16;
                    }
                    add_compound_candidate(&mut candidates, mv0, mv1, 2);
                }
                blk_col += step_w;
            }
            blk_row += step_h;
        }
        if any_hit {
            TMV_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        zero_mv_ctx = usize::from(first_sample_missing || first_sample_far);
    }

    let mut dummy_newmv_count = 0u32;
    let corner_matched = scan_corner_compound(
        grid,
        mi_row,
        mi_col,
        ref_frame,
        &mut candidates,
        &mut dummy_newmv_count,
    );
    let mut row_matched_ext = false;
    let mut col_matched_ext = false;
    for idx in 2..=MVREF_ROW_COLS {
        let row_offset = -((idx as isize) * 2) + 1 + isize::from(row_adj);
        let col_offset = -((idx as isize) * 2) + 1 + isize::from(col_adj);
        if row_offset.unsigned_abs() <= max_row_offset.unsigned_abs()
            && row_offset.unsigned_abs() > processed_rows
            && scan_row_compound(
                grid,
                mi_row,
                mi_col,
                bw4,
                row_offset,
                max_row_offset,
                ref_frame,
                &mut candidates,
                &mut dummy_newmv_count,
                &mut processed_rows,
            )
        {
            row_matched_ext = true;
        }
        if col_offset.unsigned_abs() <= max_col_offset.unsigned_abs()
            && col_offset.unsigned_abs() > processed_cols
            && scan_col_compound(
                grid,
                mi_row,
                mi_col,
                bh4,
                col_offset,
                max_col_offset,
                ref_frame,
                &mut candidates,
                &mut dummy_newmv_count,
                &mut processed_cols,
            )
        {
            col_matched_ext = true;
        }
    }
    let ref_match_count = usize::from(row_matched || corner_matched || row_matched_ext)
        + usize::from(found_left || col_matched_ext);

    candidates.sort_by_key(|e| std::cmp::Reverse(e.weight));
    candidates.truncate(MAX_STACK_SIZE);

    // The compound extension pass (libaom's `setup_ref_mv_list`, "Handle
    // compound reference frame extension"): gather across the row directly
    // above and the column directly left (same `mi_size` reach as the
    // single-ref pass), then combine once.
    let mi_width = (16usize).min(bw4).min(mi_cols.saturating_sub(mi_col));
    let mi_height = (16usize).min(bh4).min(mi_rows.saturating_sub(mi_row));
    let mi_size = mi_width.min(mi_height);
    if candidates.len() < MAX_MV_REF_CANDIDATES {
        let mut lists = CompoundRefLists::default();
        if max_row_offset.unsigned_abs() >= 1 {
            let mut idx = 0usize;
            while idx < mi_size {
                let cand = grid.get(mi_row - 1, mi_col + idx);
                let step = cand.map_or(1, |c| c.size).max(1);
                if let Some(c) = cand {
                    process_compound_ref_mv_candidate(c, ref_frame, sign_bias_table, &mut lists);
                }
                idx += step;
            }
        }
        if max_col_offset.unsigned_abs() >= 1 {
            let mut idx = 0usize;
            while idx < mi_size {
                let cand = grid.get(mi_row + idx, mi_col - 1);
                let step = cand.map_or(1, |c| c.size).max(1);
                if let Some(c) = cand {
                    process_compound_ref_mv_candidate(c, ref_frame, sign_bias_table, &mut lists);
                }
                idx += step;
            }
        }
        // libaom's real dedup (`setup_ref_mv_list` ~735-754): when the stack
        // already has exactly one entry, compare only `comp_list[0]` against
        // it and, on a match, take `comp_list[1]` instead (skip, no weight
        // bump) — otherwise append `comp_list[0]` as a brand-new entry. When
        // the stack is empty, both `comp_list[0]` and `comp_list[1]` are
        // appended unconditionally, with no dedup check at all.
        let comp_list = combine_compound_candidates(&lists);
        if candidates.len() == 1 {
            let (mv0, mv1) = if comp_list[0] == (candidates[0].mv0, candidates[0].mv1) {
                comp_list[1]
            } else {
                comp_list[0]
            };
            candidates.push(CompoundStackEntry {
                mv0,
                mv1,
                weight: 2,
            });
        } else if candidates.is_empty() {
            for (mv0, mv1) in comp_list {
                candidates.push(CompoundStackEntry {
                    mv0,
                    mv1,
                    weight: 2,
                });
            }
        }
    }

    let clamp = |mv| clamp_mv_ref(mv, mi_row, mi_col, bw4, bh4, mi_cols, mi_rows);
    // Same as the single-ref builder: libaom clamps every stack entry, and
    // DRL reads the entries directly.
    for e in candidates.iter_mut() {
        e.mv0 = clamp(e.mv0);
        e.mv1 = clamp(e.mv1);
    }
    let nearest_mv = candidates
        .first()
        .map_or(((0, 0), (0, 0)), |e| (clamp(e.mv0), clamp(e.mv1)));
    let near_mv = candidates
        .get(1)
        .map_or(((0, 0), (0, 0)), |e| (clamp(e.mv0), clamp(e.mv1)));
    let pred_mv = if candidates.is_empty() {
        ((0, 0), (0, 0))
    } else {
        nearest_mv
    };

    let new_mv_ctx = match nearest_match {
        0 => usize::from(ref_match_count >= 1),
        1 => {
            if newmv_count > 0 {
                2
            } else {
                3
            }
        }
        _ => {
            if newmv_count >= 1 {
                4
            } else {
                5
            }
        }
    };
    let ref_mv_ctx = match nearest_match {
        0 => match ref_match_count {
            0 => 0,
            1 => 1,
            _ => 2,
        },
        1 => {
            if ref_match_count >= 2 {
                4
            } else {
                3
            }
        }
        _ => 5,
    };

    let drl_ctx = candidates
        .windows(2)
        .map(|w| {
            let (a, b) = (w[0].weight, w[1].weight);
            if a >= REF_CAT_LEVEL && b >= REF_CAT_LEVEL {
                0
            } else if a >= REF_CAT_LEVEL && b < REF_CAT_LEVEL {
                1
            } else {
                2
            }
        })
        .collect();

    CompoundMvStack {
        entries: candidates,
        nearest_mv,
        near_mv,
        pred_mv,
        new_mv_ctx,
        ref_mv_ctx,
        zero_mv_ctx,
        drl_ctx,
    }
}

/// [`find_mv_stack_compound`]'s temporal-MV inputs — [`TplArgs`], doubled:
/// one `cur_offset` per side of the pair (`get_relative_dist(OrderHint,
/// OrderHints[ref_frame.N - LAST_FRAME])`, spec 7.9.3), since a compound
/// query projects the same stored field twice, once per reference.
pub struct CompoundTplArgs<'a> {
    /// The current frame's projected temporal motion field.
    pub field: &'a crate::motion_field::TplField,
    /// `get_relative_dist` for `ref_frame.0`.
    pub cur_offset_0: i32,
    /// `get_relative_dist` for `ref_frame.1`.
    pub cur_offset_1: i32,
    /// The frame header's own `allow_high_precision_mv`.
    pub allow_high_precision_mv: bool,
}

/// The single-reference context shared by the `single_ref_p1`/`p3`/`p4`
/// binary decisions this crate's LAST-only reference chain codes (spec
/// 5.11.25; libaom's `av1_get_pred_context_single_ref_p1/p3/p4`,
/// `pred_common.c`). Each of those three functions compares a forward-ref
/// neighbour count against a same-direction count that is always zero in a
/// LAST-only world (no LAST2/LAST3/GOLDEN/backward candidate ever exists
/// here), which collapses all three of libaom's formulas to the same
/// two-valued test: "is there an inter neighbour at all". `has_inter`
/// gathers that over the block's whole span, the way spec 5.11.39 reads any
/// neighbour-derived context.
pub(crate) fn single_ref_ctx(has_inter_neighbour: bool) -> usize {
    if has_inter_neighbour { 2 } else { 1 }
}

/// `MV_REFERENCE_FRAME` values a single (non-compound) reference can name
/// (spec 6.10.24's `ref_frame` alphabet; libaom `av1/common/enums.h`).
pub(crate) const LAST_FRAME: i8 = 1;
pub(crate) const LAST2_FRAME: i8 = 2;
pub(crate) const LAST3_FRAME: i8 = 3;
pub(crate) const GOLDEN_FRAME: i8 = 4;
pub(crate) const BWDREF_FRAME: i8 = 5;
pub(crate) const ALTREF2_FRAME: i8 = 6;
pub(crate) const ALTREF_FRAME: i8 = 7;

/// libaom `pred_common.c`'s shared shape behind every `single_ref_p*`/
/// `comp_ref*` context function once compound prediction (a second ref per
/// neighbour) is out of scope: count how many of the immediate above/left
/// neighbours (spec 5.11.25's own single-neighbour read, not a whole-span
/// scan -- `av1_collect_neighbors_ref_counts`, `MACROBLOCKD::above_mbmi`/
/// `left_mbmi`) coded a reference in `a_set` vs. `b_set`, and compare: fewer
/// `a`s than `b`s is context 0, equal is 1 (also every unavailable-neighbour
/// case, both counts zero), more `a`s is 2.
// libaom `av1_collect_neighbors_ref_counts` (mvref_common.h): a neighbour
// contributes ONE count per reference frame it actually names, which for a
// compound neighbour is BOTH `ref_frame[0]` and `ref_frame[1]` -- not just
// its first. Every one of these vote functions takes each side's pair
// (`ref0` always, `ref1` only for a compound neighbour) so a compound
// neighbour is not undercounted the way a single scalar per side would.
#[allow(clippy::too_many_arguments)]
fn ref_ctx(
    above0: Option<i8>,
    above1: Option<i8>,
    left0: Option<i8>,
    left1: Option<i8>,
    a_set: &[i8],
    b_set: &[i8],
) -> usize {
    let refs = [above0, above1, left0, left1];
    let a = refs.into_iter().flatten().filter(|r| a_set.contains(r)).count();
    let b = refs.into_iter().flatten().filter(|r| b_set.contains(r)).count();
    match a.cmp(&b) {
        std::cmp::Ordering::Less => 0,
        std::cmp::Ordering::Equal => 1,
        std::cmp::Ordering::Greater => 2,
    }
}

const FORWARD_REFS: [i8; 4] = [LAST_FRAME, LAST2_FRAME, LAST3_FRAME, GOLDEN_FRAME];
const BACKWARD_REFS: [i8; 3] = [BWDREF_FRAME, ALTREF2_FRAME, ALTREF_FRAME];

/// `av1_get_pred_context_single_ref_p1`: forward vs. backward reference.
pub(crate) fn single_ref_p1_ctx(
    above0: Option<i8>,
    above1: Option<i8>,
    left0: Option<i8>,
    left1: Option<i8>,
) -> usize {
    ref_ctx(above0, above1, left0, left1, &FORWARD_REFS, &BACKWARD_REFS)
}

/// `av1_get_pred_context_single_ref_p2`
/// (`get_pred_context_brfarf2_or_arf`): `BWDREF`/`ALTREF2` vs. `ALTREF`.
pub(crate) fn single_ref_p2_ctx(
    above0: Option<i8>,
    above1: Option<i8>,
    left0: Option<i8>,
    left1: Option<i8>,
) -> usize {
    ref_ctx(
        above0,
        above1,
        left0,
        left1,
        &[BWDREF_FRAME, ALTREF2_FRAME],
        &[ALTREF_FRAME],
    )
}

/// `av1_get_pred_context_single_ref_p3`
/// (`get_pred_context_ll2_or_l3gld`): `LAST`/`LAST2` vs. `LAST3`/`GOLDEN`.
pub(crate) fn single_ref_p3_ctx(
    above0: Option<i8>,
    above1: Option<i8>,
    left0: Option<i8>,
    left1: Option<i8>,
) -> usize {
    ref_ctx(
        above0,
        above1,
        left0,
        left1,
        &[LAST_FRAME, LAST2_FRAME],
        &[LAST3_FRAME, GOLDEN_FRAME],
    )
}

/// `av1_get_pred_context_single_ref_p4`
/// (`get_pred_context_last_or_last2`): `LAST` vs. `LAST2`.
pub(crate) fn single_ref_p4_ctx(
    above0: Option<i8>,
    above1: Option<i8>,
    left0: Option<i8>,
    left1: Option<i8>,
) -> usize {
    ref_ctx(above0, above1, left0, left1, &[LAST_FRAME], &[LAST2_FRAME])
}

/// `av1_get_pred_context_single_ref_p5`
/// (`get_pred_context_last3_or_gld`): `LAST3` vs. `GOLDEN`.
pub(crate) fn single_ref_p5_ctx(
    above0: Option<i8>,
    above1: Option<i8>,
    left0: Option<i8>,
    left1: Option<i8>,
) -> usize {
    ref_ctx(above0, above1, left0, left1, &[LAST3_FRAME], &[GOLDEN_FRAME])
}

/// `av1_get_pred_context_single_ref_p6`
/// (`get_pred_context_brf_or_arf2`): `BWDREF` vs. `ALTREF2`.
pub(crate) fn single_ref_p6_ctx(
    above0: Option<i8>,
    above1: Option<i8>,
    left0: Option<i8>,
    left1: Option<i8>,
) -> usize {
    ref_ctx(above0, above1, left0, left1, &[BWDREF_FRAME], &[ALTREF2_FRAME])
}

/// `av1_get_pred_context_uni_comp_ref_p1`: `LAST2` vs. `LAST3`/`GOLDEN`,
/// conditioned on a unidirectional pair known to be one of the three
/// `LAST`-anchored ones (lane-av1comp).
pub(crate) fn uni_comp_ref_p1_ctx(
    above0: Option<i8>,
    above1: Option<i8>,
    left0: Option<i8>,
    left1: Option<i8>,
) -> usize {
    ref_ctx(
        above0,
        above1,
        left0,
        left1,
        &[LAST2_FRAME],
        &[LAST3_FRAME, GOLDEN_FRAME],
    )
}

/// One neighbouring block's reference-frame shape, as
/// [`reference_mode_ctx`]/[`comp_reference_type_ctx`] need it: `None` for an
/// out-of-frame neighbour, `Some(ref0, None)` for an intra or single-ref
/// inter block, `Some(ref0, Some(ref1))` for a compound one. `uni` is
/// `ref1`'s meaning only when compound: whether the pair libaom's
/// `has_uni_comp_refs` would call unidirectional (both refs on the same
/// side of the current frame).
#[derive(Clone, Copy)]
pub(crate) struct NeighbourRef {
    pub is_inter: bool,
    pub ref0: i8,
    pub ref1: Option<i8>,
    pub uni: bool,
}

fn is_backward(r: i8) -> bool {
    (BWDREF_FRAME..=ALTREF_FRAME).contains(&r)
}

/// `av1_get_reference_mode_context` (spec 5.11.25's `comp_mode` context,
/// libaom `pred_common.c`): 5 contexts from how many of the above/left
/// neighbours are themselves compound-predicted, and (when neither/one is)
/// whether their single reference is a backward one.
pub(crate) fn reference_mode_ctx(above: Option<NeighbourRef>, left: Option<NeighbourRef>) -> usize {
    match (above, left) {
        (Some(a), Some(l)) => match (a.ref1.is_some(), l.ref1.is_some()) {
            (false, false) => usize::from(is_backward(a.ref0) ^ is_backward(l.ref0)),
            (false, true) => 2 + usize::from(is_backward(a.ref0) || !a.is_inter),
            (true, false) => 2 + usize::from(is_backward(l.ref0) || !l.is_inter),
            (true, true) => 4,
        },
        (Some(e), None) | (None, Some(e)) => {
            if e.ref1.is_some() {
                3
            } else {
                usize::from(is_backward(e.ref0))
            }
        }
        (None, None) => 1,
    }
}

/// `av1_get_comp_reference_type_context` (spec 5.11.25's
/// `comp_reference_type` context, libaom `pred_common.c`): 5 contexts from
/// whether the above/left neighbours are intra, single-ref, unidirectional
/// compound, or bidirectional compound.
pub(crate) fn comp_reference_type_ctx(
    above: Option<NeighbourRef>,
    left: Option<NeighbourRef>,
) -> usize {
    match (above, left) {
        (Some(a), Some(l)) => match (a.is_inter, l.is_inter) {
            (false, false) => 2,
            (false, true) | (true, false) => {
                let inter = if a.is_inter { a } else { l };
                if inter.ref1.is_none() {
                    2
                } else {
                    1 + 2 * usize::from(inter.uni)
                }
            }
            (true, true) => {
                let (a_sg, l_sg) = (a.ref1.is_none(), l.ref1.is_none());
                if a_sg && l_sg {
                    1 + 2 * usize::from(!(is_backward(a.ref0) ^ is_backward(l.ref0)))
                } else if a_sg || l_sg {
                    let uni = if a_sg { l.uni } else { a.uni };
                    if !uni {
                        1
                    } else {
                        3 + usize::from(!(is_backward(a.ref0) ^ is_backward(l.ref0)))
                    }
                } else if !a.uni && !l.uni {
                    0
                } else if !a.uni || !l.uni {
                    2
                } else {
                    3 + usize::from(!((a.ref0 == BWDREF_FRAME) ^ (l.ref0 == BWDREF_FRAME)))
                }
            }
        },
        (Some(e), None) | (None, Some(e)) => {
            if !e.is_inter {
                2
            } else if e.ref1.is_none() {
                2
            } else {
                4 * usize::from(e.uni)
            }
        }
        (None, None) => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inter(mv: (i32, i32)) -> MiInfo {
        MiInfo {
            is_inter: true,
            ref_frame: 1,
            ref_frame1: None,
            mv1: None,
            mv,
            is_new_mv: false,
            // 1: smaller than every `bw4`/`bh4` these small-grid tests use,
            // so it never trips the extended-scan coverage boost -- these
            // tests only exercise the immediate/corner/top-right scans.
            size: 1,
        }
    }

    #[test]
    fn all_same_mv_neighbourhood_dedupes_to_one_entry() {
        let mut grid = MiGrid::new(8, 8);
        // Block at (2, 2), 2x2 mi units. Above row 1, cols 2..4; left col 1,
        // rows 2..4; top-right at (1, 4). All the same MV.
        let mv = (4, 4);
        grid.set(1, 2, inter(mv));
        grid.set(1, 3, inter(mv));
        grid.set(2, 1, inter(mv));
        grid.set(3, 1, inter(mv));
        grid.set(1, 4, inter(mv));

        let stack = find_mv_stack(&grid, 2, 2, 2, 2, 1, 8, 8);

        assert_eq!(stack.entries.len(), 1);
        assert_eq!(stack.entries[0].mv, mv);
        // 2 above cells + 2 left cells (weight 2 each, `size: 1` neighbours
        // below the `bw4 <= n4` weight-boost floor) + 1 top-right (libaom's
        // real `scan_blk_mbmi` weight, `CORNER_WEIGHT` = 4, not a flat 2),
        // plus the REF_CAT_LEVEL boost av1_find_mv_stack applies to every
        // entry the immediate row/col/top-right scan found (module doc).
        assert_eq!(stack.entries[0].weight, 12 + REF_CAT_LEVEL);
        assert_eq!(stack.nearest_mv, mv);
        // Exact ctx (see module doc): both sides matched (nearest_match=2)
        // and no neighbour was coded NEWMV, which is libaom's
        // `mode_context[ref_frame] |= 5` branch.
        assert_eq!(stack.new_mv_ctx, 5);
        assert_eq!(stack.ref_mv_ctx, 5);
        assert_eq!(stack.zero_mv_ctx, 0);
    }

    #[test]
    fn two_distinct_neighbour_mvs_yield_two_entries_in_weight_order() {
        let mut grid = MiGrid::new(8, 8);
        // Block at (3, 3), bw4=2, bh4=3: above spans 2 cells (weight 4),
        // left spans 3 cells (weight 6) — distinct MVs, distinct weights.
        let above_mv = (4, 4);
        let left_mv = (8, 8);
        grid.set(2, 3, inter(above_mv));
        grid.set(2, 4, inter(above_mv));
        grid.set(3, 2, inter(left_mv));
        grid.set(4, 2, inter(left_mv));
        grid.set(5, 2, inter(left_mv));

        let stack = find_mv_stack(&grid, 3, 3, 2, 3, 1, 8, 8);

        // Both are entries the immediate scan found, so both get the
        // REF_CAT_LEVEL boost (module doc) — their relative order is
        // unaffected.
        assert_eq!(stack.entries.len(), 2);
        assert_eq!(
            stack.entries[0],
            StackEntry {
                mv: left_mv,
                weight: 6 + REF_CAT_LEVEL
            }
        );
        assert_eq!(
            stack.entries[1],
            StackEntry {
                mv: above_mv,
                weight: 4 + REF_CAT_LEVEL
            }
        );
        assert_eq!(stack.nearest_mv, left_mv);
        assert_eq!(stack.near_mv, above_mv);
    }

    #[test]
    fn transposing_above_and_left_produces_a_different_stack() {
        // Grid A: above = (4,4) x2 cells, left = (8,8) x2 cells, top-right
        // matches the above MV (so it folds into the above bucket, making
        // it heavier than left).
        let mut a = MiGrid::new(8, 8);
        a.set(1, 2, inter((4, 4)));
        a.set(1, 3, inter((4, 4)));
        a.set(2, 1, inter((8, 8)));
        a.set(3, 1, inter((8, 8)));
        a.set(1, 4, inter((4, 4)));
        let stack_a = find_mv_stack(&a, 2, 2, 2, 2, 1, 8, 8);

        // Grid B: the naive transpose — swap which MV sits above vs left —
        // with the top-right probe again matching whatever now sits above.
        let mut b = MiGrid::new(8, 8);
        b.set(1, 2, inter((8, 8)));
        b.set(1, 3, inter((8, 8)));
        b.set(2, 1, inter((4, 4)));
        b.set(3, 1, inter((4, 4)));
        b.set(1, 4, inter((8, 8)));
        let stack_b = find_mv_stack(&b, 2, 2, 2, 2, 1, 8, 8);

        assert_ne!(stack_a, stack_b);
        assert_eq!(stack_a.entries[0].mv, (4, 4));
        assert_eq!(stack_b.entries[0].mv, (8, 8));
    }

    #[test]
    fn mi_units_outside_the_tile_contribute_nothing() {
        let grid = MiGrid::new(4, 4);
        // Block at the tile's top-left corner: no row above, no col left,
        // and the top-right probe is also off the top edge.
        let stack = find_mv_stack(&grid, 0, 0, 2, 2, 1, 4, 4);

        assert!(stack.entries.is_empty());
        assert_eq!(stack.nearest_mv, (0, 0));
        assert_eq!(stack.near_mv, (0, 0));
        assert_eq!(stack.pred_mv, (0, 0));
        assert!(stack.drl_ctx.is_empty());
        assert_eq!(stack.new_mv_ctx, 0);
        assert_eq!(stack.ref_mv_ctx, 0);

        // Out-of-range reads are also inert directly against the grid.
        assert!(grid.get(0, 10).is_none());
        assert!(grid.get(10, 0).is_none());
    }

    #[test]
    fn a_candidate_far_outside_the_frame_is_clamped_to_it() {
        // A 4x4-mi frame (16x16 px), block 1x1 mi at (1, 1): room for one
        // neighbour above and one to the left, tight edges on every side.
        let mut grid = MiGrid::new(4, 4);
        grid.set(0, 1, inter((100_000, 100_000)));

        let stack = find_mv_stack(&grid, 1, 1, 1, 1, 1, 4, 4);

        // Unclamped this candidate is (100000, 100000); spec 7.10.2.14 caps
        // it at the frame edge (2 mi below/right) plus the block's own 4x4
        // span plus MV_BORDER (128), all in 1/8-pel: (2*4)*8 + 1*4*8 + 128 =
        // 64 + 32 + 128 = 224.
        assert_eq!(stack.nearest_mv, (224, 224));
        // libaom `av1_find_mv_refs` clamps every stack ENTRY too -- the DRL
        // reads entries directly, so a raw candidate must not survive
        // (pinned warp-flake-5.obu regression).
        assert_eq!(stack.entries[0].mv, (224, 224));
    }

    #[test]
    fn a_neighbour_coded_newmv_raises_the_new_mv_context() {
        let mut grid = MiGrid::new(8, 8);
        grid.set(
            1,
            2,
            MiInfo {
                is_inter: true,
                ref_frame: 1,
                ref_frame1: None,
                mv1: None,
                mv: (4, 4),
                is_new_mv: true,
                size: 1,
            },
        );
        // Only the row matches (nearest_match == 1): newmv_count > 0 picks
        // libaom's `mode_context[ref_frame] |= 2` branch.
        let stack = find_mv_stack(&grid, 2, 2, 1, 1, 1, 8, 8);
        assert_eq!(stack.new_mv_ctx, 2);
        assert_eq!(stack.ref_mv_ctx, 3);
    }

    #[test]
    fn a_diagonal_corner_match_raises_ref_mv_ctx_past_nearest_match() {
        // Block at (2, 2), 1x1 mi unit: only the col to the left matches
        // (nearest_match == 1, so a pre-fix `ref_mv_ctx` collapse would say
        // 3 regardless of anything else — this is the exact round-10 defect
        // shape). The row above has no match of its own, but the diagonal
        // top-left corner (1, 1) does; libaom folds that into the row side
        // of `ref_match_count` (raising it to 2) without ever touching the
        // already-captured `nearest_match`.
        let mut grid = MiGrid::new(8, 8);
        grid.set(2, 1, inter((4, 4)));
        grid.set(1, 1, inter((8, 8)));

        let stack = find_mv_stack(&grid, 2, 2, 1, 1, 1, 8, 8);

        assert_eq!(stack.new_mv_ctx, 3); // depends on nearest_match alone: unchanged
        assert_eq!(stack.ref_mv_ctx, 4); // nearest_match=1, ref_match_count=2
        // The corner's distinct MV becomes its own low-weight stack entry,
        // ranked behind the boosted nearest one — DRL context sees a real
        // (non-nearest) second candidate for the first time.
        assert_eq!(stack.entries.len(), 2);
        assert_eq!(stack.entries[0].mv, (4, 4));
        assert_eq!(stack.entries[1].mv, (8, 8));
        assert_eq!(stack.drl_ctx, vec![1]);
    }

    #[test]
    fn a_corner_only_match_raises_new_mv_ctx_even_when_nearest_match_is_zero() {
        // Block at (2, 2), 1x1 mi unit: row above (1, 2), col left (2, 1)
        // and top-right (1, 3) all empty — nearest_match == 0 — but the
        // diagonal top-left corner (1, 1) matches. libaom's `case 0` arm
        // (mvref_common.c ~line 621) sets `new_mv_ctx`'s own bit from
        // `ref_match_count` here, not just `ref_mv_ctx`'s: this is I8 CLASS
        // B's real-content bitstream desync (real clips hit blocks whose
        // *only* neighbour match is diagonal; the repo's synthetic tests
        // never had one until this case).
        let mut grid = MiGrid::new(8, 8);
        grid.set(1, 1, inter((4, 4)));

        let stack = find_mv_stack(&grid, 2, 2, 1, 1, 1, 8, 8);

        assert_eq!(stack.entries.len(), 1);
        assert_eq!(stack.new_mv_ctx, 1); // ref_match_count == 1, not the old hard-0
        assert_eq!(stack.ref_mv_ctx, 1); // nearest_match=0, ref_match_count=1
    }

    /// The class this round ports: a query block *smaller* than its
    /// same-row neighbour (decode.rs's real variable partition sizes, never
    /// `tile.rs`'s fixed `bw4 == 8`) gets libaom's real, non-flat weight —
    /// hand-computed against `scan_row_mbmi` (mvref_common.c): `bw4 = 2`
    /// against an `n4 = 8` neighbour 6 mi rows from the frame's top edge
    /// gives `len = min(bw4, n4) = 2`, `inc = min(-max_row_offset +
    /// row_offset + 1, n4) = min(6 - 1 + 1, 8) = 6`, `weight = max(2, inc) =
    /// 6`, total `len * weight = 12` -- not the flat `IMMEDIATE_WEIGHT = 2`
    /// this module used to report for every match regardless of block size.
    #[test]
    fn a_wider_neighbour_than_the_query_block_gets_libaoms_real_weight() {
        let mut grid = MiGrid::new(32, 32);
        let (mi_row, mi_col) = (8, 8);
        let mv = (4, 4);
        grid.set(
            mi_row - 1,
            mi_col,
            MiInfo {
                is_inter: true,
                ref_frame: 1,
                ref_frame1: None,
                mv1: None,
                mv,
                is_new_mv: false,
                size: 8,
            },
        );

        let stack = find_mv_stack(&grid, mi_row, mi_col, 2, 2, 1, 32, 32);

        assert_eq!(stack.entries.len(), 1);
        assert_eq!(stack.entries[0].mv, mv);
        assert_eq!(stack.entries[0].weight, 12 + REF_CAT_LEVEL);
    }

    #[test]
    fn single_ref_ctx_matches_libaoms_collapsed_forward_only_case() {
        assert_eq!(single_ref_ctx(false), 1);
        assert_eq!(single_ref_ctx(true), 2);
    }

    /// An 8-mi-sized (`size: 8`) coded-but-intra neighbour: contributes no
    /// vote (`is_inter: false`) but must still count for extended-scan
    /// coverage, the way libaom's `scan_row_mbmi`/`scan_col_mbmi` advance
    /// `processed_rows`/`processed_cols` from any candidate's `bsize` alone.
    fn intra8() -> MiInfo {
        MiInfo {
            is_inter: false,
            ref_frame: -1,
            ref_frame1: None,
            mv1: None,
            mv: (0, 0),
            is_new_mv: false,
            size: 8,
        }
    }

    /// An 8-mi-sized inter neighbour with an arbitrary, distinctive `mv` --
    /// used below as a "would this get picked up" tracer at extended-scan
    /// offsets that a correct implementation must never reach at 8-mi
    /// geometry.
    fn inter8(mv: (i32, i32)) -> MiInfo {
        MiInfo {
            is_inter: true,
            ref_frame: 1,
            ref_frame1: None,
            mv1: None,
            mv,
            is_new_mv: false,
            size: 8,
        }
    }

    /// The class of bug this round fixed: at 8-mi geometry, an intra
    /// neighbour directly above the query block must suppress the extended
    /// row scan exactly as an inter one would (spec 7.10.2.2's
    /// `processed_rows` tracks *coded* neighbours, not merely *matching*
    /// ones). Two tracer candidates sit at the row's extended-scan offsets
    /// (-3 and -5, spec 7.10.2.4) with MVs no other scan could produce; if
    /// the coverage bug regresses, they show up in `stack.entries`.
    #[test]
    fn extended_row_scan_is_a_no_op_at_8mi_geometry_even_behind_an_intra_neighbour() {
        let mut grid = MiGrid::new(32, 32);
        let (mi_row, mi_col) = (10, 10);
        for col in mi_col..mi_col + 8 {
            grid.set(mi_row - 1, col, intra8());
        }
        for col in mi_col + 1..mi_col + 1 + 8 {
            grid.set(mi_row - 3, col, inter8((999, 999)));
            grid.set(mi_row - 5, col, inter8((888, 888)));
        }

        let stack = find_mv_stack(&grid, mi_row, mi_col, 8, 8, 1, 32, 32);

        assert!(
            stack.entries.is_empty(),
            "extended row scan fired at 8-mi geometry: {:?}",
            stack.entries
        );
    }

    /// `extended_row_scan_is_a_no_op_at_8mi_geometry_even_behind_an_intra_neighbour`,
    /// transposed to the col-left scan.
    #[test]
    fn extended_col_scan_is_a_no_op_at_8mi_geometry_even_behind_an_intra_neighbour() {
        let mut grid = MiGrid::new(32, 32);
        let (mi_row, mi_col) = (10, 10);
        for row in mi_row..mi_row + 8 {
            grid.set(row, mi_col - 1, intra8());
        }
        for row in mi_row + 1..mi_row + 1 + 8 {
            grid.set(row, mi_col - 3, inter8((999, 999)));
            grid.set(row, mi_col - 5, inter8((888, 888)));
        }

        let stack = find_mv_stack(&grid, mi_row, mi_col, 8, 8, 1, 32, 32);

        assert!(
            stack.entries.is_empty(),
            "extended col scan fired at 8-mi geometry: {:?}",
            stack.entries
        );
    }

    /// Same shape, but the immediate neighbour is itself inter (a same-size
    /// vote) rather than intra -- the geometry-only coverage path must not
    /// have broken the ordinary matching one.
    #[test]
    fn extended_row_scan_is_a_no_op_at_8mi_geometry_behind_an_inter_neighbour() {
        let mut grid = MiGrid::new(32, 32);
        let (mi_row, mi_col) = (10, 10);
        for col in mi_col..mi_col + 8 {
            grid.set(mi_row - 1, col, inter8((4, 4)));
        }
        for col in mi_col + 1..mi_col + 1 + 8 {
            grid.set(mi_row - 3, col, inter8((999, 999)));
            grid.set(mi_row - 5, col, inter8((888, 888)));
        }

        let stack = find_mv_stack(&grid, mi_row, mi_col, 8, 8, 1, 32, 32);

        assert_eq!(stack.entries.len(), 1);
        assert_eq!(stack.entries[0].mv, (4, 4));
    }

    fn single(ref0: i8) -> NeighbourRef {
        NeighbourRef {
            is_inter: true,
            ref0,
            ref1: None,
            uni: false,
        }
    }
    fn comp(ref0: i8, ref1: i8, uni: bool) -> NeighbourRef {
        NeighbourRef {
            is_inter: true,
            ref0,
            ref1: Some(ref1),
            uni,
        }
    }
    fn intra() -> NeighbourRef {
        NeighbourRef {
            is_inter: false,
            ref0: 0,
            ref1: None,
            uni: false,
        }
    }

    /// `av1_get_reference_mode_context`, no edges available: libaom's `ctx
    /// = 1` fallback.
    #[test]
    fn reference_mode_ctx_no_edges_is_one() {
        assert_eq!(reference_mode_ctx(None, None), 1);
    }

    /// Both edges single-ref, same forward/backward-ness: `ctx = 0`; one of
    /// each: `ctx = 1` (the XOR in libaom's `ctx = IS_BACKWARD(above) ^
    /// IS_BACKWARD(left)`).
    #[test]
    fn reference_mode_ctx_single_single() {
        assert_eq!(
            reference_mode_ctx(Some(single(LAST_FRAME)), Some(single(LAST2_FRAME))),
            0
        );
        assert_eq!(
            reference_mode_ctx(Some(single(LAST_FRAME)), Some(single(BWDREF_FRAME))),
            1
        );
    }

    /// One edge compound, the other single forward: libaom's `2 +
    /// IS_BACKWARD(above)` collapses to `2` here (forward, inter).
    #[test]
    fn reference_mode_ctx_single_and_compound() {
        assert_eq!(
            reference_mode_ctx(
                Some(single(LAST_FRAME)),
                Some(comp(LAST_FRAME, LAST2_FRAME, true))
            ),
            2
        );
        assert_eq!(
            reference_mode_ctx(
                Some(comp(LAST_FRAME, LAST2_FRAME, true)),
                Some(single(LAST_FRAME))
            ),
            2
        );
    }

    /// Both edges compound: libaom's fixed `ctx = 4`.
    #[test]
    fn reference_mode_ctx_compound_compound_is_four() {
        assert_eq!(
            reference_mode_ctx(
                Some(comp(LAST_FRAME, LAST2_FRAME, true)),
                Some(comp(BWDREF_FRAME, ALTREF_FRAME, true))
            ),
            4
        );
    }

    /// `av1_get_comp_reference_type_context`, no edges: libaom's `ctx = 2`
    /// fallback.
    #[test]
    fn comp_reference_type_ctx_no_edges_is_two() {
        assert_eq!(comp_reference_type_ctx(None, None), 2);
    }

    /// Both edges intra: libaom's `ctx = 2`.
    #[test]
    fn comp_reference_type_ctx_intra_intra_is_two() {
        assert_eq!(comp_reference_type_ctx(Some(intra()), Some(intra())), 2);
    }

    /// Both edges single/single: forward-vs-backward split doubles the same
    /// way `single_ref_p1`'s own XOR does (libaom's `1 + 2 * !(a ^ l)`).
    #[test]
    fn comp_reference_type_ctx_single_single() {
        assert_eq!(
            comp_reference_type_ctx(Some(single(LAST_FRAME)), Some(single(LAST2_FRAME))),
            3
        );
        assert_eq!(
            comp_reference_type_ctx(Some(single(LAST_FRAME)), Some(single(BWDREF_FRAME))),
            1
        );
    }

    /// Both edges bidirectional compound: libaom's `ctx = 0`.
    #[test]
    fn comp_reference_type_ctx_bidir_bidir_is_zero() {
        assert_eq!(
            comp_reference_type_ctx(
                Some(comp(LAST_FRAME, BWDREF_FRAME, false)),
                Some(comp(LAST2_FRAME, ALTREF_FRAME, false))
            ),
            0
        );
    }

    /// Both edges unidirectional compound: libaom's `3 + !(a==BWDREF ^
    /// l==BWDREF)`.
    #[test]
    fn comp_reference_type_ctx_unidir_unidir() {
        assert_eq!(
            comp_reference_type_ctx(
                Some(comp(LAST_FRAME, LAST2_FRAME, true)),
                Some(comp(LAST_FRAME, LAST3_FRAME, true))
            ),
            4
        );
        assert_eq!(
            comp_reference_type_ctx(
                Some(comp(LAST_FRAME, LAST2_FRAME, true)),
                Some(comp(BWDREF_FRAME, ALTREF_FRAME, true))
            ),
            3
        );
    }

    /// `av1_get_pred_context_uni_comp_ref_p1`: `LAST2` beats `LAST3`/
    /// `GOLDEN` in the vote, context 2 (libaom's `<` branch reversed --
    /// fewer `a`s than `b`s is context 0, matching [`ref_ctx`]'s shared
    /// convention).
    #[test]
    fn uni_comp_ref_p1_ctx_matches_ref_ctx_shape() {
        assert_eq!(uni_comp_ref_p1_ctx(Some(LAST2_FRAME), None, None, None), 2);
        assert_eq!(uni_comp_ref_p1_ctx(Some(LAST3_FRAME), None, None, None), 0);
        assert_eq!(uni_comp_ref_p1_ctx(None, None, None, None), 1);
    }

    // --- lane-av1comp step 4: compound mvstack (find_mv_stack_compound) ---

    const COMP_PAIR: (i8, i8) = (LAST_FRAME, ALTREF_FRAME);

    fn comp_inter(mv0: (i32, i32), mv1: (i32, i32)) -> MiInfo {
        MiInfo {
            is_inter: true,
            ref_frame: COMP_PAIR.0,
            ref_frame1: Some(COMP_PAIR.1),
            mv: mv0,
            mv1: Some(mv1),
            is_new_mv: false,
            size: 1,
        }
    }

    #[test]
    fn compound_scan_only_matches_the_exact_pair() {
        let mut grid = MiGrid::new(8, 8);
        // Same-side single-ref neighbour (LAST only) must not vote.
        grid.set(1, 2, inter((4, 4)));
        // Wrong second ref (GOLDEN, not ALTREF) must not vote either.
        grid.set(
            1,
            3,
            MiInfo {
                is_inter: true,
                ref_frame: LAST_FRAME,
                ref_frame1: Some(GOLDEN_FRAME),
                mv: (9, 9),
                mv1: Some((9, 9)),
                is_new_mv: false,
                size: 1,
            },
        );
        // Exact pair match.
        let (mv0, mv1) = ((2, 2), (-6, 3));
        grid.set(2, 1, comp_inter(mv0, mv1));

        let stack = find_mv_stack_compound(&grid, 2, 2, 2, 2, COMP_PAIR, 8, 8, &NO_SIGN_BIAS, None);

        // The exact-pair immediate scan lands one boosted entry; the
        // compound extension pass (short stack, len < MAX_MV_REF_CANDIDATES)
        // separately tops it up with a low-weight combo built from the two
        // near-miss neighbours it's allowed to gather over regardless of
        // reference frame -- a real second entry, not spurious.
        assert_eq!(stack.entries.len(), 2);
        assert_eq!(stack.entries[0].mv0, mv0);
        assert_eq!(stack.entries[0].mv1, mv1);
        assert_eq!(stack.entries[0].weight, 2 + REF_CAT_LEVEL);
        assert_eq!(stack.nearest_mv, (mv0, mv1));
    }

    #[test]
    fn compound_dedupes_same_pair_and_sums_weight() {
        let mut grid = MiGrid::new(8, 8);
        let (mv0, mv1) = ((4, 4), (-4, -4));
        // Block at (2, 2), 2x2 mi units: above spans cols 2..4, left rows 2..4.
        grid.set(1, 2, comp_inter(mv0, mv1));
        grid.set(1, 3, comp_inter(mv0, mv1));
        grid.set(2, 1, comp_inter(mv0, mv1));
        grid.set(3, 1, comp_inter(mv0, mv1));

        let stack = find_mv_stack_compound(&grid, 2, 2, 2, 2, COMP_PAIR, 8, 8, &NO_SIGN_BIAS, None);

        // Exact libaom `is_dup` port: with exactly one entry already in the
        // stack (the four identical-pair neighbours deduped by the
        // immediate-scan pass), the extension combine step compares only
        // `comp_list[0]` against that entry -- a match here, since both are
        // built from the same identical-pair neighbours -- so it takes
        // `comp_list[1]` (the *second* ref_id slot, still the same (mv0,
        // mv1) pair) as a brand-new stack entry rather than folding weight
        // into the existing one. Two entries, same pair, not merged.
        assert_eq!(stack.entries.len(), 2);
        assert_eq!(stack.entries[0].mv0, mv0);
        assert_eq!(stack.entries[0].mv1, mv1);
        // 2 above + 2 left cells, weight 2 each (size:1 floor), plus the
        // REF_CAT_LEVEL boost every immediate-scan entry gets.
        assert_eq!(stack.entries[0].weight, 8 + REF_CAT_LEVEL);
        assert_eq!(stack.entries[1].mv0, mv0);
        assert_eq!(stack.entries[1].mv1, mv1);
        assert_eq!(stack.entries[1].weight, 2);
    }

    #[test]
    fn compound_two_distinct_pairs_sort_by_weight() {
        let mut grid = MiGrid::new(8, 8);
        let above = ((4, 4), (1, 1));
        let left = ((8, 8), (2, 2));
        grid.set(2, 3, comp_inter(above.0, above.1));
        grid.set(2, 4, comp_inter(above.0, above.1));
        grid.set(3, 2, comp_inter(left.0, left.1));
        grid.set(4, 2, comp_inter(left.0, left.1));
        grid.set(5, 2, comp_inter(left.0, left.1));

        let stack = find_mv_stack_compound(&grid, 3, 3, 2, 3, COMP_PAIR, 8, 8, &NO_SIGN_BIAS, None);

        assert_eq!(stack.entries.len(), 2);
        assert_eq!(stack.entries[0].mv0, left.0);
        assert_eq!(stack.entries[0].mv1, left.1);
        assert_eq!(stack.entries[1].mv0, above.0);
        assert_eq!(stack.entries[1].mv1, above.1);
        assert_eq!(stack.nearest_mv, left);
        assert_eq!(stack.near_mv, above);
    }

    #[test]
    fn combine_compound_candidates_zips_ref_id_then_ref_diff_per_side() {
        let mut lists = CompoundRefLists::default();
        lists.ref_id[0].push((1, 1));
        lists.ref_diff[0].push((2, 2));
        lists.ref_id[1].push((3, 3));
        // side 1 has only one entry -> comp_idx 1 falls back to ref_diff.
        lists.ref_diff[1].push((4, 4));

        let combined = combine_compound_candidates(&lists);
        assert_eq!(combined[0], ((1, 1), (3, 3)));
        assert_eq!(combined[1], ((2, 2), (4, 4)));
    }

    #[test]
    fn process_compound_ref_mv_candidate_splits_exact_vs_borrowed() {
        let mut lists = CompoundRefLists::default();
        // Exact match on side 0 (LAST_FRAME), wrong ref on side 1 -> borrowed
        // into ref_diff[1] with no sign flip (NO_SIGN_BIAS).
        let candidate = MiInfo {
            is_inter: true,
            ref_frame: LAST_FRAME,
            ref_frame1: None,
            mv: (5, 5),
            mv1: None,
            is_new_mv: false,
            size: 1,
        };
        process_compound_ref_mv_candidate(&candidate, COMP_PAIR, &NO_SIGN_BIAS, &mut lists);
        assert_eq!(lists.ref_id[0], vec![(5, 5)]);
        assert_eq!(lists.ref_diff[1], vec![(5, 5)]);
        assert!(lists.ref_id[1].is_empty());
        assert!(lists.ref_diff[0].is_empty());
    }

    #[test]
    fn compound_extension_pass_tops_up_a_short_stack() {
        // No exact-pair neighbour at all, but a LAST-only neighbour above and
        // an ALTREF-only neighbour left: the extension pass should combine
        // them into one topped-up candidate rather than leaving the stack
        // empty.
        let mut grid = MiGrid::new(8, 8);
        grid.set(
            1,
            2,
            MiInfo {
                is_inter: true,
                ref_frame: LAST_FRAME,
                ref_frame1: None,
                mv: (7, 7),
                mv1: None,
                is_new_mv: false,
                size: 8,
            },
        );
        grid.set(
            2,
            1,
            MiInfo {
                is_inter: true,
                ref_frame: ALTREF_FRAME,
                ref_frame1: None,
                mv: (-3, -3),
                mv1: None,
                is_new_mv: false,
                size: 8,
            },
        );

        let stack = find_mv_stack_compound(&grid, 2, 2, 2, 2, COMP_PAIR, 8, 8, &NO_SIGN_BIAS, None);

        // The combine step (libaom's real "Handle compound reference frame
        // extension") zips each side's ref_id-then-ref_diff lists
        // independently, producing *two* distinct combos here (the exact
        // match and the borrowed one land on opposite sides for each combo
        // index) — both get added, `MAX_MV_REF_CANDIDATES` (2) is exactly
        // this stack's cap.
        assert_eq!(stack.entries.len(), 2);
        assert_eq!(stack.entries[0].weight, 2);
        // combo 0: side 0's own exact ref_id match, side 1's borrowed one.
        assert_eq!(stack.entries[0].mv0, (7, 7));
        assert_eq!(stack.entries[0].mv1, (-3, -3));
        // combo 1: the mirror -- side 0's borrowed entry, side 1's own exact
        // ref_id match.
        assert_eq!(stack.entries[1].mv0, (-3, -3));
        assert_eq!(stack.entries[1].mv1, (7, 7));
    }
}

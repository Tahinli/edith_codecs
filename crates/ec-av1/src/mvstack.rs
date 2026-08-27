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

/// One 4x4 `mi` unit's motion state, as the encode loop will have filled it
/// in by the time it asks for a block's MV stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MiInfo {
    /// Whether this unit was coded inter (as opposed to intra).
    pub is_inter: bool,
    /// The single reference frame this unit's MV points into. Ignored (and
    /// the unit contributes no candidate) when `is_inter` is `false`.
    pub ref_frame: i8,
    /// The unit's motion vector, `(row, col)`, in the spec's 1/8-pel units.
    pub mv: (i32, i32),
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
        if info.is_inter && info.ref_frame == ref_frame {
            found = true;
            add_candidate(candidates, info.mv, len as u32 * weight);
            *newmv_count += u32::from(info.is_new_mv);
        }
        i += len.max(1);
    }
    found
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
        if info.is_inter && info.ref_frame == ref_frame {
            found = true;
            add_candidate(candidates, info.mv, len as u32 * weight);
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
        && info.is_inter
        && info.ref_frame == ref_frame
    {
        add_candidate(candidates, info.mv, CORNER_WEIGHT);
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
        && info.is_inter
        && info.ref_frame == ref_frame
    {
        add_candidate(candidates, info.mv, CORNER_WEIGHT);
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

    let clamp = |mv| clamp_mv_ref(mv, mi_row, mi_col, bw4, bh4, mi_cols, mi_rows);
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
    // GLOBALMV_OFFSET is only ever set by temporal MV projection
    // (`cm->features.allow_ref_frame_mvs`), which this reduction never runs
    // (see module doc) — exact, not a stand-in.
    let zero_mv_ctx = 0;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn inter(mv: (i32, i32)) -> MiInfo {
        MiInfo {
            is_inter: true,
            ref_frame: 1,
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
        assert_ne!(stack.entries[0].mv, stack.nearest_mv);
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
}

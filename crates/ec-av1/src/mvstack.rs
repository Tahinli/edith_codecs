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
//! This module's scan is already reduced to the immediate row/col/top-right
//! positions with no extended-offset scan (see below), which lets those
//! context formulas collapse exactly rather than approximately: libaom's
//! `ref_match_count` comes from the *same* row/col match counters as
//! `nearest_match` once the extended scan is gone, so `ref_match_count ==
//! nearest_match` always here, and every candidate this module finds is one
//! of libaom's "nearest" ones, so the `REF_CAT_LEVEL` (640) bonus
//! `av1_find_mv_stack` adds to nearest candidates before computing DRL
//! context applies to all of them uniformly — which is why [`find_mv_stack`]
//! always reports `drl_ctx == 0`: with the extended scan absent, no
//! candidate here can ever be a "non-nearest" (sub-640) one for a pair
//! comparison to distinguish. Likewise, the `GLOBALMV_OFFSET` bit of
//! libaom's `mode_context` is set only inside
//! `cm->features.allow_ref_frame_mvs` (temporal MV projection), which this
//! module never has (`use_ref_frame_mvs = 0`), so `zero_mv_ctx` is always 0
//! here — again exact for the case this module covers, not a stand-in.
//!
//! Deliberately reduced away (see the crate report, not reproduced bit-exact
//! here): compound/multi-reference candidates, the temporal MV projection
//! scan, and the `>1` row/col scans at offset 3 and their skip-by-block-size
//! stepping (which is also why the "extra search" that pads a short stack to
//! two entries, spec 7.10.2.12, is not implemented — this module's stack can
//! be shorter than two candidates where libaom's never is).

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

/// Weight one immediate (row `-1` / col `-1`) neighbour 4x4 unit's vote
/// counts for (spec: `len * 2` with `len = 1` per unit for the reduction
/// this module makes — see the module doc).
const IMMEDIATE_WEIGHT: u32 = 2;

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

/// The row-above scan (spec 7.10.2.2): every 4x4 unit directly above the
/// block's span. Returns whether any unit voted.
fn scan_row(
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
    let mut found = false;
    for col in mi_col..mi_col + bw4 {
        if let Some(info) = grid.get(row, col)
            && info.is_inter
            && info.ref_frame == ref_frame
        {
            found = true;
            add_candidate(candidates, info.mv, IMMEDIATE_WEIGHT);
            *newmv_count += u32::from(info.is_new_mv);
        }
    }
    found
}

/// The col-left scan (spec 7.10.2.3): every 4x4 unit directly left of the
/// block's span. Returns whether any unit voted.
fn scan_col(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    bh4: usize,
    ref_frame: i8,
    candidates: &mut Vec<StackEntry>,
    newmv_count: &mut u32,
) -> bool {
    let Some(col) = mi_col.checked_sub(1) else {
        return false;
    };
    let mut found = false;
    for row in mi_row..mi_row + bh4 {
        if let Some(info) = grid.get(row, col)
            && info.is_inter
            && info.ref_frame == ref_frame
        {
            found = true;
            add_candidate(candidates, info.mv, IMMEDIATE_WEIGHT);
            *newmv_count += u32::from(info.is_new_mv);
        }
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
        add_candidate(candidates, info.mv, IMMEDIATE_WEIGHT);
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
    let found_above = scan_row(
        grid,
        mi_row,
        mi_col,
        bw4,
        ref_frame,
        &mut candidates,
        &mut newmv_count,
    );
    let found_left = scan_col(
        grid,
        mi_row,
        mi_col,
        bh4,
        ref_frame,
        &mut candidates,
        &mut newmv_count,
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

    // Highest weight first; a stable sort keeps scan order (above, left,
    // top-right) among ties, matching spec 7.10.2.6.
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

    // libaom's `row_match_count`/`col_match_count` (mvref_common.c) fold the
    // top-right probe into the row side, so "found above" for context
    // purposes means the row scan *or* the top-right one matched.
    let row_matched = found_above || found_top_right;
    let nearest_match = usize::from(row_matched) + usize::from(found_left);

    // Exact port of the `mode_context[ref_frame]` switch (mvref_common.c
    // ~line 619-643): with no extended-offset scan, `ref_match_count` here is
    // always equal to `nearest_match` (both come from the same row/col match
    // counters this module never advances further), so the `ref_match_count`
    // branches of libaom's switch collapse into the constants below — see
    // module doc.
    let new_mv_ctx = match nearest_match {
        0 => 0,
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
        0 => 0,
        1 => 3,
        _ => 5,
    };
    // GLOBALMV_OFFSET is only ever set by temporal MV projection
    // (`cm->features.allow_ref_frame_mvs`), which this reduction never runs
    // (see module doc) — exact, not a stand-in.
    let zero_mv_ctx = 0;

    // av1_drl_ctx (mvref_common.h): every candidate here is one of libaom's
    // "nearest" ones (see module doc), so each gets `REF_CAT_LEVEL` before
    // the comparison, which is why this always lands on 0.
    let drl_ctx = candidates
        .windows(2)
        .map(|w| {
            let (a, b) = (w[0].weight + REF_CAT_LEVEL, w[1].weight + REF_CAT_LEVEL);
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
        // 2 above cells + 2 left cells + 1 top-right, each weight 2.
        assert_eq!(stack.entries[0].weight, 10);
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

        assert_eq!(stack.entries.len(), 2);
        assert_eq!(
            stack.entries[0],
            StackEntry {
                mv: left_mv,
                weight: 6
            }
        );
        assert_eq!(
            stack.entries[1],
            StackEntry {
                mv: above_mv,
                weight: 4
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
            },
        );
        // Only the row matches (nearest_match == 1): newmv_count > 0 picks
        // libaom's `mode_context[ref_frame] |= 2` branch.
        let stack = find_mv_stack(&grid, 2, 2, 1, 1, 1, 8, 8);
        assert_eq!(stack.new_mv_ctx, 2);
        assert_eq!(stack.ref_mv_ctx, 3);
    }

    #[test]
    fn single_ref_ctx_matches_libaoms_collapsed_forward_only_case() {
        assert_eq!(single_ref_ctx(false), 1);
        assert_eq!(single_ref_ctx(true), 2);
    }
}

//! Temporal MV projection (AV1 spec 7.9, `use_ref_frame_mvs`): projects a
//! previously decoded frame's stored motion vectors into the current
//! frame's own 8x8 motion field, ported from libaom's
//! `av1_setup_motion_field`/`motion_field_projection`
//! (`av1/common/mvref_common.c`) and the projection math `add_tpl_ref_mv`
//! (same file) applies a second time, per query block, against the block's
//! own single reference frame.

use crate::mvstack::{ALTREF_FRAME, BWDREF_FRAME, GOLDEN_FRAME, LAST_FRAME, LAST2_FRAME};

/// One 8x8 cell's saved motion state, as libaom's `av1_copy_frame_mvs`
/// leaves it after decoding the frame that owns this grid: the coded MV and
/// the single reference frame (`LAST_FRAME..=ALTREF_FRAME`, 1..=7) it
/// pointed to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SavedMv {
    pub mv: (i32, i32),
    pub ref_frame: i8,
}

/// A decoded inter frame's whole motion field (spec `MotionFieldMvs`... no
/// — libaom's `cur_frame->mvs`), one [`SavedMv`] per 8x8 unit, plus the
/// order-hint bookkeeping [`setup_motion_field`] needs to project it into a
/// later frame: this frame's own `OrderHint` and the `OrderHint` of each of
/// the 7 references *it* was coded against (`SavedOrderHints`/
/// `ref_order_hints`).
#[derive(Clone, Debug)]
pub struct MotionField {
    cols: usize,
    rows: usize,
    cells: Vec<Option<SavedMv>>,
    pub order_hint: u32,
    pub ref_order_hints: [u32; 7],
    /// libaom `motion_field_projection`'s own first test: a start frame whose
    /// `frame_type` is `KEY_FRAME`/`INTRA_ONLY_FRAME` is never projected at
    /// all (`return 0`), so it also never spends a `ref_stamp` slot.
    pub is_intra: bool,
}

impl MotionField {
    /// A field sized for an `mi_cols` by `mi_rows` frame, all cells
    /// initially unset (spec 7.9's own reset to `-1`/invalid before any
    /// block writes into it).
    pub fn new(
        mi_cols: usize,
        mi_rows: usize,
        order_hint: u32,
        ref_order_hints: [u32; 7],
        is_intra: bool,
    ) -> Self {
        let cols = mi_cols.div_ceil(2);
        let rows = mi_rows.div_ceil(2);
        Self {
            cols,
            rows,
            cells: vec![None; cols * rows],
            order_hint,
            ref_order_hints,
            is_intra,
        }
    }

    /// Records `saved` at the 8x8 cell containing 4x4 unit `(mi_row,
    /// mi_col)` — every `MiInfo` inside one 8x8 cell shares a single
    /// motion-field entry, matching `av1_copy_frame_mvs`'s own half-resolution
    /// write.
    /// `saved == None` is `av1_copy_frame_mvs`'s own leading clear
    /// (`mv->ref_frame = NONE_FRAME; mv->mv.as_int = 0;`), which every block
    /// of an inter frame performs -- an intra (or nothing-to-store) block
    /// sharing an 8x8 cell with an earlier inter one wipes that cell.
    pub fn set(&mut self, mi_row: usize, mi_col: usize, saved: Option<SavedMv>) {
        let (r, c) = (mi_row / 2, mi_col / 2);
        if r < self.rows && c < self.cols {
            self.cells[r * self.cols + c] = saved;
        }
    }

    pub(crate) fn debug_get(&self, row8: usize, col8: usize) -> Option<SavedMv> {
        self.get(row8, col8)
    }

    fn get(&self, row8: usize, col8: usize) -> Option<SavedMv> {
        if row8 < self.rows && col8 < self.cols {
            self.cells[row8 * self.cols + col8]
        } else {
            None
        }
    }
}

/// The current frame's projected temporal motion field (spec 7.9.2's
/// `MotionFieldMvs`): one projected `(mv, ref_frame_offset)` per 8x8 cell,
/// `None` where no source candidate landed (libaom's `INVALID_MV`).
#[derive(Clone, Debug)]
pub struct TplField {
    cols: usize,
    rows: usize,
    cells: Vec<Option<((i32, i32), i32)>>,
}

impl TplField {
    fn get(&self, row8: usize, col8: usize) -> Option<((i32, i32), i32)> {
        if row8 < self.rows && col8 < self.cols {
            self.cells[row8 * self.cols + col8]
        } else {
            None
        }
    }
}

/// spec `get_relative_dist` (5.9.3): `a - b` wrapped into the signed range
/// order hints roll over in at `order_hint_bits` width.
pub fn get_relative_dist(order_hint_bits: u32, a: u32, b: u32) -> i32 {
    if order_hint_bits == 0 {
        return 0;
    }
    let diff = a as i32 - b as i32;
    let m = 1i32 << (order_hint_bits - 1);
    (diff & (m - 1)) - (diff & m)
}

/// spec `FRAME_OFFSET_BITS`-derived clamp (libaom `MAX_FRAME_DISTANCE`).
const MAX_FRAME_DISTANCE: i32 = 31;

/// libaom's `div_mult`: precomputed `16384 / den` (`av1_get_mv_projection`'s
/// own comment: "all the values are strictly under 14 bits").
const DIV_MULT: [i64; 32] = [
    0, 16384, 8192, 5461, 4096, 3276, 2730, 2340, 2048, 1820, 1638, 1489, 1365, 1260, 1170, 1092,
    1024, 963, 910, 862, 819, 780, 744, 712, 682, 655, 630, 606, 585, 564, 546, 528,
];

fn round_pow2_signed(value: i64, n: u32) -> i32 {
    let half = 1i64 << (n - 1);
    (if value < 0 {
        -((-value + half) >> n)
    } else {
        (value + half) >> n
    }) as i32
}

/// `av1_get_mv_projection` (spec 7.9.3): scales `mv` by the ratio of two
/// frame-distance offsets.
fn get_mv_projection(mv: (i32, i32), num: i32, den: i32) -> (i32, i32) {
    let den = den.min(MAX_FRAME_DISTANCE);
    let num = if num > 0 {
        num.min(MAX_FRAME_DISTANCE)
    } else {
        num.max(-MAX_FRAME_DISTANCE)
    };
    let mult = DIV_MULT[den as usize];
    let scale = |v: i32| round_pow2_signed(v as i64 * num as i64 * mult, 14).clamp(-16383, 16383);
    (scale(mv.0), scale(mv.1))
}

/// `lower_mv_precision` (`mvref_common.h`), the `is_integer == false` half
/// only — every stream this decoder ever reaches has `force_integer_mv ==
/// false` on an inter frame (`allow_screen_content_tools`, the only source
/// of a `true` value here, is refused outright at the stream level).
pub fn lower_mv_precision(mv: (i32, i32), allow_high_precision_mv: bool) -> (i32, i32) {
    if allow_high_precision_mv {
        return mv;
    }
    let round = |v: i32| {
        if v & 1 != 0 {
            v + if v > 0 { -1 } else { 1 }
        } else {
            v
        }
    };
    (round(mv.0), round(mv.1))
}

/// `get_block_position` (spec 7.9.2): the 8x8 cell a projected `mv` lands on
/// starting from `(blk_row, blk_col)` (already in 8x8 units), or `None` when
/// it falls outside the frame or outside the projection's own bound (a
/// 64x64-superblock-aligned base block, spec's `MAX_OFFSET_WIDTH`/
/// `MAX_OFFSET_HEIGHT`).
fn get_block_position(
    mvs_rows: usize,
    mvs_cols: usize,
    blk_row: usize,
    blk_col: usize,
    mv: (i32, i32),
    sign_bias: bool,
) -> Option<(usize, usize)> {
    let base_blk_row = (blk_row >> 3) << 3;
    let base_blk_col = (blk_col >> 3) << 3;
    let row_offset = if mv.0 >= 0 {
        mv.0 >> 6
    } else {
        -((-mv.0) >> 6)
    };
    let col_offset = if mv.1 >= 0 {
        mv.1 >> 6
    } else {
        -((-mv.1) >> 6)
    };
    let row = if sign_bias {
        blk_row as i64 - row_offset as i64
    } else {
        blk_row as i64 + row_offset as i64
    };
    let col = if sign_bias {
        blk_col as i64 - col_offset as i64
    } else {
        blk_col as i64 + col_offset as i64
    };
    if row < 0 || row >= mvs_rows as i64 || col < 0 || col >= mvs_cols as i64 {
        return None;
    }
    if row < base_blk_row as i64
        || row >= base_blk_row as i64 + 8
        || col < base_blk_col as i64 - 8
        || col >= base_blk_col as i64 + 16
    {
        return None;
    }
    Some((row as usize, col as usize))
}

/// `motion_field_projection` (spec 7.9.2): projects `start`'s stored MVs
/// into `tpl`, scaled by the OrderHint distance from `start` to the current
/// frame. `dir` mirrors libaom's own parameter: `2` for a backward-looking
/// source (`LAST_FRAME`/`LAST2_FRAME`, offset negated and `sign_bias`
/// flipped), `0` for a forward one (`BWDREF_FRAME`/`ALTREF2_FRAME`/
/// `ALTREF_FRAME`). Returns whether the projection ran at all (`start`'s
/// frame size has to match the current frame's — always true for this
/// decoder's fixed-size streams, but ported anyway).
#[allow(clippy::too_many_arguments)]
fn motion_field_projection(
    start: &MotionField,
    cur_order_hint: u32,
    order_hint_bits: u32,
    mi_rows: usize,
    mi_cols: usize,
    dir: u8,
    tpl_cells: &mut [Option<((i32, i32), i32)>],
) -> bool {
    // libaom's own first two `return 0`s, in order: a key/intra-only start
    // frame is never projected (and so never spends a `ref_stamp` slot --
    // returning `true` here made a forward key frame, `--enable-fwd-kf=1`,
    // consume the slot ALTREF/LAST2 should have had), then the frame-size
    // test.
    if start.is_intra {
        return false;
    }
    let mvs_rows = mi_rows.div_ceil(2);
    let mvs_cols = mi_cols.div_ceil(2);
    if start.rows != mvs_rows || start.cols != mvs_cols {
        return false;
    }
    let start_order_hint = start.order_hint;
    let mut start_to_current = get_relative_dist(order_hint_bits, start_order_hint, cur_order_hint);
    if dir == 2 {
        start_to_current = -start_to_current;
    }
    let ref_offset: [i32; 8] = std::array::from_fn(|rf| {
        if rf == 0 {
            0
        } else {
            get_relative_dist(
                order_hint_bits,
                start_order_hint,
                start.ref_order_hints[rf - 1],
            )
        }
    });
    for row8 in 0..start.rows {
        for col8 in 0..start.cols {
            let Some(saved) = start.get(row8, col8) else {
                continue;
            };
            if saved.ref_frame <= 0 {
                continue;
            }
            let ref_frame_offset = ref_offset[saved.ref_frame as usize];
            let pos_valid = ref_frame_offset.abs() <= MAX_FRAME_DISTANCE
                && ref_frame_offset > 0
                && start_to_current.abs() <= MAX_FRAME_DISTANCE;
            if !pos_valid {
                continue;
            }
            let projected = get_mv_projection(saved.mv, start_to_current, ref_frame_offset);
            if let Some((r, c)) =
                get_block_position(mvs_rows, mvs_cols, row8, col8, projected, dir >> 1 == 1)
            {
                tpl_cells[r * mvs_cols + c] = Some((saved.mv, ref_frame_offset));
            }
        }
    }
    true
}

thread_local! {
    /// Frames whose `av1_setup_motion_field` saw at least one forward
    /// references holding a key/intra-only frame -- the `ref_stamp` path
    /// [`motion_field_projection`]'s intra early return governs.
    static REFSTAMP_INTRA_FRAMES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// The subset of those frames with TWO or more such references.
    static REFSTAMP_INTRA2_FRAMES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Reads the counter above (gate
/// `a_real_aomenc_inter_sequence_with_forward_keyframes_and_temporal_mvs_decodes_pixel_exact`).
pub fn refstamp_intra_frames() -> (usize, usize) {
    (
        REFSTAMP_INTRA_FRAMES.with(std::cell::Cell::get),
        REFSTAMP_INTRA2_FRAMES.with(std::cell::Cell::get),
    )
}

/// `av1_setup_motion_field` (spec 7.9.2's own driver): scans up to
/// [`MFMV_STACK_SIZE`]`= 3` reference-frame slots in libaom's fixed order
/// (`LAST_FRAME`, then `BWDREF_FRAME`/`ALTREF2_FRAME`/`ALTREF_FRAME` when
/// each is display-order-forward of the current frame, then `LAST2_FRAME`
/// once the stack still has room) and projects each into the returned
/// [`TplField`].
///
/// `slots` is the DPB's 8 reference-picture slots' saved [`MotionField`]s
/// (a `None` slot, or one still holding an intra frame's — never
/// constructed, so also `None` — motion field, contributes nothing);
/// `ref_frame_idx` is the current frame header's own `ref_frame_idx`
/// (`LAST_FRAME..=ALTREF_FRAME`, indexed `[i]` for `LAST_FRAME + i`).
pub fn setup_motion_field(
    slots: &[Option<MotionField>; 8],
    ref_frame_idx: [u8; 7],
    cur_order_hint: u32,
    order_hint_bits: u32,
    mi_rows: usize,
    mi_cols: usize,
) -> TplField {
    let cols = mi_cols.div_ceil(2);
    let rows = mi_rows.div_ceil(2);
    let mut cells = vec![None; cols * rows];
    // `av1_setup_motion_field`'s own head: `if (!enable_order_hint) return;`
    // leaves every `tpl_mvs` cell INVALID.
    if order_hint_bits == 0 {
        return TplField { cols, rows, cells };
    }

    let slot_of =
        |ref_frame: i8| slots[ref_frame_idx[(ref_frame - LAST_FRAME) as usize] as usize].as_ref();
    let order_hint_of = |ref_frame: i8| slot_of(ref_frame).map_or(0, |m| m.order_hint);

    let mut ref_stamp = 2i32; // MFMV_STACK_SIZE - 1

    // Gate counter (lane-refstamp): frames whose forward references include at
    // least two key/intra-only frames, i.e. exactly the `ref_stamp` path the
    // early return above fixes.
    let intra_forward_refs = [
        BWDREF_FRAME,
        crate::mvstack::ALTREF2_FRAME,
        ALTREF_FRAME,
    ]
    .into_iter()
    .filter(|&rf| {
        get_relative_dist(order_hint_bits, order_hint_of(rf), cur_order_hint) > 0
            && slot_of(rf).is_some_and(|m| m.is_intra)
    })
    .count();
    if intra_forward_refs >= 1 {
        REFSTAMP_INTRA_FRAMES.with(|c| c.set(c.get() + 1));
    }
    if intra_forward_refs >= 2 {
        REFSTAMP_INTRA2_FRAMES.with(|c| c.set(c.get() + 1));
    }

    if let Some(last) = slot_of(LAST_FRAME) {
        let alt_of_lst = last.ref_order_hints[(ALTREF_FRAME - LAST_FRAME) as usize];
        let is_lst_overlay = alt_of_lst == order_hint_of(GOLDEN_FRAME);
        if !is_lst_overlay {
            motion_field_projection(
                last,
                cur_order_hint,
                order_hint_bits,
                mi_rows,
                mi_cols,
                2,
                &mut cells,
            );
        }
        ref_stamp -= 1;
    }

    if get_relative_dist(order_hint_bits, order_hint_of(BWDREF_FRAME), cur_order_hint) > 0
        && let Some(bwd) = slot_of(BWDREF_FRAME)
        && motion_field_projection(
            bwd,
            cur_order_hint,
            order_hint_bits,
            mi_rows,
            mi_cols,
            0,
            &mut cells,
        )
    {
        ref_stamp -= 1;
    }

    if get_relative_dist(
        order_hint_bits,
        order_hint_of(crate::mvstack::ALTREF2_FRAME),
        cur_order_hint,
    ) > 0
        && let Some(alt2) = slot_of(crate::mvstack::ALTREF2_FRAME)
        && motion_field_projection(
            alt2,
            cur_order_hint,
            order_hint_bits,
            mi_rows,
            mi_cols,
            0,
            &mut cells,
        )
    {
        ref_stamp -= 1;
    }

    if ref_stamp >= 0
        && get_relative_dist(order_hint_bits, order_hint_of(ALTREF_FRAME), cur_order_hint) > 0
        && let Some(alt) = slot_of(ALTREF_FRAME)
        && motion_field_projection(
            alt,
            cur_order_hint,
            order_hint_bits,
            mi_rows,
            mi_cols,
            0,
            &mut cells,
        )
    {
        ref_stamp -= 1;
    }

    if ref_stamp >= 0
        && let Some(last2) = slot_of(LAST2_FRAME)
    {
        motion_field_projection(
            last2,
            cur_order_hint,
            order_hint_bits,
            mi_rows,
            mi_cols,
            2,
            &mut cells,
        );
    }

    if std::env::var_os("EC_TRACE_TPL").is_some() {
        eprintln!("EC_TPL_FIELD oh={cur_order_hint} rows={rows} cols={cols}");
        for r in 0..rows {
            let row: String = (0..cols)
                .map(|c| if cells[r * cols + c].is_some() { '#' } else { '.' })
                .collect();
            eprintln!("EC_TPL_FIELD r{r} {row}");
        }
    }
    TplField { cols, rows, cells }
}

/// The result [`add_tpl_ref_mv`] folds into a query block's MV stack: the
/// projected candidate MV (already scaled to the block's own reference
/// frame and MV-precision-lowered) plus whether the very first (`blk_row ==
/// 0 && blk_col == 0`) sample differs enough from the zero vector to raise
/// `GLOBALMV_OFFSET` (spec 7.10.2.8 — this decoder only ever reaches
/// `IDENTITY` global motion, so the spec's `gm_get_motion_vector` compare
/// collapses to "far from `(0, 0)`").
pub struct TplCandidate {
    pub mv: (i32, i32),
}

/// `add_tpl_ref_mv`'s single-reference half (spec 7.10.2.8, libaom
/// `mvref_common.c`): probes one 8x8-unit offset `(blk_row, blk_col)` from
/// `(mi_row, mi_col)` in [`TplField`] and, if it landed a candidate,
/// projects it a second time — this time scaled to `cur_offset_0`, the
/// distance from the current frame to the query block's own single
/// reference frame, rather than `start_to_current_frame_offset` from the
/// first (storage-time) projection.
pub fn add_tpl_ref_mv(
    tpl: &TplField,
    mi_row: usize,
    mi_col: usize,
    blk_row: isize,
    blk_col: isize,
    cur_offset_0: i32,
    allow_high_precision_mv: bool,
) -> Option<TplCandidate> {
    let pos_row = if mi_row & 1 != 0 {
        blk_row
    } else {
        blk_row + 1
    };
    let pos_col = if mi_col & 1 != 0 {
        blk_col
    } else {
        blk_col + 1
    };
    let row8 = mi_row as isize + pos_row;
    let col8 = mi_col as isize + pos_col;
    if row8 < 0 || col8 < 0 {
        return None;
    }
    let probe = tpl.get(row8 as usize / 2, col8 as usize / 2);
    if std::env::var_os("EC_TRACE_TPL").is_some() {
        match probe {
            None => eprintln!("EC_TPL mi_row={mi_row} mi_col={mi_col} blk=({blk_row},{blk_col}) INVALID"),
            Some((m, rfo)) => eprintln!(
                "EC_TPL mi_row={mi_row} mi_col={mi_col} blk=({blk_row},{blk_col}) mfmv0=({},{}) rfo={rfo}",
                m.0, m.1
            ),
        }
    }
    let (fwd_mv, ref_frame_offset) = probe?;
    let projected = get_mv_projection(fwd_mv, cur_offset_0, ref_frame_offset);
    let mv = lower_mv_precision(projected, allow_high_precision_mv);
    Some(TplCandidate { mv })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_dist_wraps_at_order_hint_bits() {
        // 7-bit order hints (aomenc's usual default): 1 - 126 should read as
        // +3 (wrapping backward past 0), not -125.
        assert_eq!(get_relative_dist(7, 1, 126), 3);
        assert_eq!(get_relative_dist(7, 10, 5), 5);
    }

    #[test]
    fn mv_projection_scales_by_the_distance_ratio() {
        // A MV recorded across a 2-frame gap, projected across a 1-frame
        // gap, should halve (libaom's own div_mult table is exact for small
        // integer ratios).
        let (row, col) = get_mv_projection((16, -32), 1, 2);
        assert_eq!((row, col), (8, -16));
        // Same-distance projection is the identity.
        assert_eq!(get_mv_projection((7, -3), 4, 4), (7, -3));
    }

    #[test]
    fn setup_motion_field_projects_last_frame_forward() {
        // A slot-0 inter frame (order_hint 2) that coded one 8x8 cell's MV
        // against its own LAST_FRAME (order_hint 1). The current frame
        // (order_hint 4) names that same slot as *its* LAST_FRAME.
        let mut mf = MotionField::new(16, 16, 2, [1, 0, 0, 0, 0, 0, 0], false);
        mf.set(
            0,
            0,
            Some(SavedMv {
                mv: (-64, 0),
                ref_frame: LAST_FRAME,
            }),
        );
        let slots: [Option<MotionField>; 8] =
            std::array::from_fn(|i| if i == 0 { Some(mf.clone()) } else { None });
        let ref_frame_idx = [0u8; 7];
        let tpl = setup_motion_field(&slots, ref_frame_idx, 4, 7, 16, 16);
        // LAST_FRAME's own dir=2 projection: start_to_current = -get_relative_dist(2,4) = 2,
        // ref_frame_offset = get_relative_dist(2,1) = 1, scaling (-64,0) by
        // 2/1 -> (-128,0); get_block_position's sign_bias (dir=2) subtracts
        // the offset, landing at 8x8 cell (2, 0).
        let cand = add_tpl_ref_mv(&tpl, 4, 0, 0isize, 0isize, get_relative_dist(7, 4, 2), true);
        assert!(cand.is_some(), "expected a projected temporal candidate");
    }
}

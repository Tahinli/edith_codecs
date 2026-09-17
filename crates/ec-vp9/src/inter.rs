//! Inter-frame syntax (libvpx `vp9/decoder/vp9_decodemv.c`
//! `read_inter_frame_mode_info`, `vp9/common/vp9_mvref_common.h`).
//!
//! Syntax only: nothing here reconstructs pixels. The MV *predictor* is still
//! parse-load-bearing — `read_mv` reads the high-precision bit only when
//! `allow_high_precision_mv && use_mv_hp(predictor)`, so the candidate scan
//! runs even though no motion compensation happens.

use crate::bool::BoolDecoder;
use crate::header::FrameContext;
use crate::modes::{MiInfo, MiState};
use crate::tables::*;

/// `INTRA_FRAME` .. `ALTREF_FRAME` (`MV_REFERENCE_FRAME`), `NO_REF_FRAME` = -1.
pub(crate) const INTRA_FRAME: i8 = 0;
pub(crate) const LAST_FRAME: i8 = 1;
pub(crate) const GOLDEN_FRAME: i8 = 2;
pub(crate) const ALTREF_FRAME: i8 = 3;
pub(crate) const NO_REF_FRAME: i8 = -1;
/// `PREDICTION_MODE` values of the inter modes.
pub(crate) const NEARESTMV: u8 = 10;
pub(crate) const NEARMV: u8 = 11;
pub(crate) const ZEROMV: u8 = 12;
pub(crate) const NEWMV: u8 = 13;
/// `REFERENCE_MODE`.
pub(crate) const SINGLE_REFERENCE: u8 = 0;
pub(crate) const COMPOUND_REFERENCE: u8 = 1;
pub(crate) const REFERENCE_MODE_SELECT: u8 = 2;
/// `SWITCHABLE`: `interpolation_filter` value meaning "per block".
pub(crate) const SWITCHABLE: u8 = 4;
/// `SWITCHABLE_FILTERS`: the sentinel `mi->interp_filter` of intra blocks.
pub(crate) const SWITCHABLE_FILTERS: u8 = 3;

const MAX_MV_REF_CANDIDATES: usize = 2;
const MVREF_NEIGHBOURS: usize = 8;
/// `MV_BORDER` (1/8-pel units).
const MV_BORDER: i32 = 16 << 3;
/// `use_mv_hp`'s `kMvRefThresh`.
const MV_HP_THRESH: i32 = 64;
const CLASS0_SIZE: i32 = 2;
const CLASS0_BITS: i32 = 1;
/// `SEG_LVL_SKIP` / `SEG_LVL_REF_FRAME`.
const SEG_LVL_REF_FRAME: usize = 2;
const SEG_LVL_SKIP: usize = 3;

/// `mv_ref_blocks[BLOCK_SIZES]` (`vp9_mvref_common.h`): `(row, col)` offsets,
/// transcribed verbatim from the C. libvpx's `POSITION` is `{ row, col }`, so
/// every accessor must add the FIRST element to the row and the SECOND to the
/// column (a transposed accessor silently picks a different neighbour for every
/// asymmetric entry).
const MV_REF_BLOCKS: [[(i32, i32); MVREF_NEIGHBOURS]; 13] = [
    // 4X4
    [(-1, 0), (0, -1), (-1, -1), (-2, 0), (0, -2), (-2, -1), (-1, -2), (-2, -2)],
    // 4X8
    [(-1, 0), (0, -1), (-1, -1), (-2, 0), (0, -2), (-2, -1), (-1, -2), (-2, -2)],
    // 8X4
    [(-1, 0), (0, -1), (-1, -1), (-2, 0), (0, -2), (-2, -1), (-1, -2), (-2, -2)],
    // 8X8
    [(-1, 0), (0, -1), (-1, -1), (-2, 0), (0, -2), (-2, -1), (-1, -2), (-2, -2)],
    // 8X16
    [(0, -1), (-1, 0), (1, -1), (-1, -1), (0, -2), (-2, 0), (-2, -1), (-1, -2)],
    // 16X8
    [(-1, 0), (0, -1), (-1, 1), (-1, -1), (-2, 0), (0, -2), (-1, -2), (-2, -1)],
    // 16X16
    [(-1, 0), (0, -1), (-1, 1), (1, -1), (-1, -1), (-3, 0), (0, -3), (-3, -3)],
    // 16X32
    [(0, -1), (-1, 0), (2, -1), (-1, -1), (-1, 1), (0, -3), (-3, 0), (-3, -3)],
    // 32X16
    [(-1, 0), (0, -1), (-1, 2), (-1, -1), (1, -1), (-3, 0), (0, -3), (-3, -3)],
    // 32X32
    [(-1, 1), (1, -1), (-1, 2), (2, -1), (-1, -1), (-3, 0), (0, -3), (-3, -3)],
    // 32X64
    [(0, -1), (-1, 0), (4, -1), (-1, 2), (-1, -1), (0, -3), (-3, 0), (2, -1)],
    // 64X32
    [(-1, 0), (0, -1), (-1, 4), (2, -1), (-1, -1), (-3, 0), (0, -3), (-1, 2)],
    // 64X64
    [(-1, 3), (3, -1), (-1, 4), (4, -1), (-1, -1), (-1, 0), (0, -1), (-1, 6)],
];

/// `idx_n_column_to_subblock` (`vp9_mvref_common.h`).
const IDX_N_COLUMN_TO_SUBBLOCK: [[usize; 2]; 4] = [[1, 2], [1, 3], [3, 2], [3, 3]];

/// One `MV_REF` entry: the previous frame's per-8x8 reference pair and MVs
/// (`vp9_read_mode_info` stores them for every block of an inter frame).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct MvRef {
    /// `ref_frame[0]`/`[1]`; zeroed means `INTRA_FRAME`.
    pub ref_frame: [i8; 2],
    /// `mv[0]`/`[1]` in 1/8-pel units.
    pub mv: [(i32, i32); 2],
}

/// Frame-level reference-mode state (`VP9_COMMON::reference_mode`,
/// `comp_fixed_ref`, `comp_var_ref`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InterFrameState {
    pub reference_mode: u8,
    pub comp_fixed_ref: i8,
    pub comp_var_ref: [i8; 2],
}

impl Default for InterFrameState {
    fn default() -> Self {
        InterFrameState {
            reference_mode: SINGLE_REFERENCE,
            comp_fixed_ref: ALTREF_FRAME,
            comp_var_ref: [LAST_FRAME, GOLDEN_FRAME],
        }
    }
}

/// `vp9_setup_compound_reference_mode` (`vp9_pred_common.c`): which ref frame
/// of a compound pair is coded explicitly, and which two the coded bit picks.
pub(crate) fn setup_compound_reference_mode(ifs: &mut InterFrameState, sign_bias: [bool; 4]) {
    let b = |f: i8| sign_bias[f as usize];
    if b(LAST_FRAME) == b(GOLDEN_FRAME) {
        ifs.comp_fixed_ref = ALTREF_FRAME;
        ifs.comp_var_ref = [LAST_FRAME, GOLDEN_FRAME];
    } else if b(LAST_FRAME) == b(ALTREF_FRAME) {
        ifs.comp_fixed_ref = GOLDEN_FRAME;
        ifs.comp_var_ref = [LAST_FRAME, ALTREF_FRAME];
    } else {
        ifs.comp_fixed_ref = LAST_FRAME;
        ifs.comp_var_ref = [GOLDEN_FRAME, ALTREF_FRAME];
    }
}

/// Frame- and position-scoped inputs of the block-level reads.
pub(crate) struct InterBlockCtx<'a> {
    pub seg: &'a ec_vp9_syntax::SegmentationParams,
    pub ifs: InterFrameState,
    /// `ref_frame_sign_bias[INTRA..ALTREF]`.
    pub sign_bias: [bool; 4],
    pub allow_hp: bool,
    pub frame_interp: u8,
    pub tx_mode: u8,
    pub mi_rows: usize,
    pub mi_cols: usize,
    pub tile_col_start: usize,
    /// `tile->mi_col_end`.
    pub tile_col_end: usize,
    /// The previous frame's segment map, when it has one.
    pub last_seg: Option<&'a [u8]>,
    /// `prev_frame->mvs` when `use_prev_frame_mvs`.
    pub prev_mvs: Option<&'a [MvRef]>,
}

/// The two decoded neighbours of the current block.
#[derive(Clone, Copy)]
struct Nb<'a> {
    above: Option<&'a MiInfo>,
    left: Option<&'a MiInfo>,
}

/// `read_inter_frame_mode_info` (`vp9_decodemv.c`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn read_inter_frame_mode_info(
    r: &mut BoolDecoder,
    st: &MiState,
    c: &InterBlockCtx<'_>,
    fc: &FrameContext,
    row: usize,
    col: usize,
    sb_type: usize,
    x_mis: usize,
    y_mis: usize,
    partition: u8,
    cur_seg: &mut [u8],
) -> crate::Result<MiInfo> {
    let above = st.above_mi(row, col).cloned();
    let left = st.left_mi(row, col, c.tile_col_start).cloned();
    let nb = Nb {
        above: above.as_ref(),
        left: left.as_ref(),
    };

    let mut seg_id_predicted = false;
    let segment = read_inter_segment_id(r, c, nb, row, col, x_mis, y_mis, cur_seg, &mut seg_id_predicted);
    let skip = read_skip(r, c, fc, segment, nb);
    let inter_block = read_is_inter_block(r, c, fc, segment, nb);
    let tx_size = read_tx_size(r, c, fc, sb_type, nb, !skip || !inter_block);

    let mut info = MiInfo {
        sb_type,
        segment,
        skip,
        tx_size,
        mode: 0,
        uv_mode: 0,
        bmi: [0; 4],
        is_inter: inter_block,
        ref_frame: [NO_REF_FRAME; 2],
        mv: [(0, 0); 2],
        mv_diff: [(0, 0); 2],
        bmi_mv: [[(0, 0); 2]; 4],
        seg_id_predicted,
        interp_filter: SWITCHABLE_FILTERS,
    };
    if inter_block {
        read_inter_block_mode_info(r, st, c, fc, &mut info, nb, row, col, partition)?;
    } else {
        read_intra_block_mode_info(r, fc, &mut info);
    }
    Ok(info)
}

/// `dec_get_segment_id`: the minimum id over the block's covered cells.
fn min_seg(map: &[u8], row: usize, col: usize, x_mis: usize, y_mis: usize, mi_cols: usize) -> u8 {
    let mut id = u8::MAX;
    for y in 0..y_mis {
        for x in 0..x_mis {
            id = id.min(map[(row + y) * mi_cols + col + x]);
        }
    }
    id
}

/// `set_segment_id` / `copy_segment_id`.
fn write_seg(
    src: Option<&[u8]>,
    dst: &mut [u8],
    row: usize,
    col: usize,
    x_mis: usize,
    y_mis: usize,
    mi_cols: usize,
    id: u8,
) {
    for y in 0..y_mis {
        for x in 0..x_mis {
            let i = (row + y) * mi_cols + col + x;
            dst[i] = match src {
                Some(m) => m[i],
                None => id,
            };
        }
    }
}

/// `read_inter_segment_id` (`vp9_decodemv.c:151`).
#[allow(clippy::too_many_arguments)]
fn read_inter_segment_id(
    r: &mut BoolDecoder,
    c: &InterBlockCtx<'_>,
    nb: Nb<'_>,
    row: usize,
    col: usize,
    x_mis: usize,
    y_mis: usize,
    cur_seg: &mut [u8],
    seg_id_predicted: &mut bool,
) -> u8 {
    let seg = c.seg;
    if !seg.enabled {
        return 0;
    }
    let predicted = c
        .last_seg
        .map_or(0, |m| min_seg(m, row, col, x_mis, y_mis, c.mi_cols));
    if !seg.update_map {
        write_seg(c.last_seg, cur_seg, row, col, x_mis, y_mis, c.mi_cols, 0);
        return predicted;
    }
    let segment_id = if seg.temporal_update {
        let ctx = usize::from(nb.above.is_some_and(|m| m.seg_id_predicted))
            + usize::from(nb.left.is_some_and(|m| m.seg_id_predicted));
        *seg_id_predicted = r.read_bool(seg.pred_probs[ctx]);
        if *seg_id_predicted {
            predicted
        } else {
            r.read_tree(&SEGMENT_TREE, &seg.tree_probs)
        }
    } else {
        r.read_tree(&SEGMENT_TREE, &seg.tree_probs)
    };
    write_seg(None, cur_seg, row, col, x_mis, y_mis, c.mi_cols, segment_id);
    segment_id
}

fn seg_feature(seg: &ec_vp9_syntax::SegmentationParams, segment_id: u8, feature: usize) -> bool {
    seg.enabled
        && seg
            .feature_enabled
            .get(segment_id as usize)
            .is_some_and(|f| f[feature])
}

/// `read_skip`.
fn read_skip(
    r: &mut BoolDecoder,
    c: &InterBlockCtx<'_>,
    fc: &FrameContext,
    segment_id: u8,
    nb: Nb<'_>,
) -> bool {
    if seg_feature(c.seg, segment_id, SEG_LVL_SKIP) {
        true
    } else {
        let ctx = usize::from(nb.above.is_some_and(|m| m.skip))
            + usize::from(nb.left.is_some_and(|m| m.skip));
        r.read_bool(fc.skip[ctx])
    }
}

/// `get_intra_inter_context` (`vp9_pred_common.h`).
fn intra_inter_context(nb: Nb<'_>) -> usize {
    match (nb.above, nb.left) {
        (Some(a), Some(l)) => {
            let above_intra = !a.is_inter;
            let left_intra = !l.is_inter;
            if left_intra && above_intra {
                3
            } else {
                usize::from(left_intra || above_intra)
            }
        }
        (Some(m), None) | (None, Some(m)) => 2 * usize::from(!m.is_inter),
        (None, None) => 0,
    }
}

/// `read_is_inter_block`.
fn read_is_inter_block(
    r: &mut BoolDecoder,
    c: &InterBlockCtx<'_>,
    fc: &FrameContext,
    segment_id: u8,
    nb: Nb<'_>,
) -> bool {
    if seg_feature(c.seg, segment_id, SEG_LVL_REF_FRAME) {
        c.seg.feature_data[segment_id as usize][SEG_LVL_REF_FRAME] != INTRA_FRAME as i16
    } else {
        r.read_bool(fc.intra_inter[intra_inter_context(nb)])
    }
}

/// `read_tx_size` with `allow_select` (spec 6.3.5 / `vp9_decodemv.c:85`).
fn read_tx_size(
    r: &mut BoolDecoder,
    c: &InterBlockCtx<'_>,
    fc: &FrameContext,
    sb_type: usize,
    nb: Nb<'_>,
    allow_select: bool,
) -> usize {
    let max_tx = MAX_TXSIZE_LOOKUP[sb_type] as usize;
    if allow_select && c.tx_mode == TX_MODE_SELECT && sb_type >= 3 {
        // get_tx_size_context: default each side to max_tx, then substitute
        // the other side when one is missing, in this exact order.
        let a0 = nb
            .above
            .map_or(max_tx, |m| if m.skip { max_tx } else { m.tx_size });
        let l0 = nb
            .left
            .map_or(max_tx, |m| if m.skip { max_tx } else { m.tx_size });
        let mut a = a0;
        let mut l = l0;
        if nb.left.is_none() {
            l = a;
        }
        if nb.above.is_none() {
            a = l;
        }
        let ctx = usize::from(a + l > max_tx);
        let probs: &[u8] = match max_tx {
            1 => &fc.tx_p8x8[ctx][..],
            2 => &fc.tx_p16x16[ctx][..],
            _ => &fc.tx_p32x32[ctx][..],
        };
        let mut tx = r.read_bool(probs[0]) as usize;
        if tx != TX_4X4 && max_tx >= TX_16X16 {
            tx += r.read_bool(probs[1]) as usize;
            if tx != TX_8X8 && max_tx >= TX_32X32 {
                tx += r.read_bool(probs[2]) as usize;
            }
        }
        tx
    } else {
        max_tx.min(TX_MODE_TO_BIGGEST_TX_SIZE[c.tx_mode as usize])
    }
}

/// `read_inter_mode`.
fn read_inter_mode(r: &mut BoolDecoder, fc: &FrameContext, ctx: usize) -> u8 {
    NEARESTMV + r.read_tree(&INTER_MODE_TREE, &fc.inter_mode[ctx])
}

/// `get_mode_context` (`vp9_decodemv.c:681`): the two nearest candidates'
/// modes fold into one of 5 contexts.
fn get_mode_context(st: &MiState, c: &InterBlockCtx<'_>, bsize: usize, row: usize, col: usize) -> usize {
    let mut counter = 0usize;
    for p in MV_REF_BLOCKS[bsize][..2].iter() {
        if is_inside(c, row, col, *p) {
            let mode = st
                .at((row as i32 + p.0) as usize, (col as i32 + p.1) as usize)
                .map_or(0u8, |m| m.mode);
            counter += MODE_2_COUNTER[mode as usize] as usize;
        }
    }
    COUNTER_TO_CONTEXT[counter] as usize
}

/// `get_pred_context_switchable_interp` (`vp9_pred_common.h:69`).
fn switchable_interp_ctx(nb: Nb<'_>) -> usize {
    let left_type = nb.left.map_or(SWITCHABLE_FILTERS, |m| m.interp_filter);
    let above_type = nb.above.map_or(SWITCHABLE_FILTERS, |m| m.interp_filter);
    if left_type == above_type {
        left_type as usize
    } else if left_type == SWITCHABLE_FILTERS {
        above_type as usize
    } else if above_type == SWITCHABLE_FILTERS {
        left_type as usize
    } else {
        SWITCHABLE_FILTERS as usize
    }
}

/// `read_intra_block_mode_info` (the intra fallback inside an inter frame).
fn read_intra_block_mode_info(r: &mut BoolDecoder, fc: &FrameContext, info: &mut MiInfo) {
    let bsize = info.sb_type;
    match bsize {
        0 => {
            for i in 0..4 {
                info.bmi[i] = r.read_tree(&INTRA_MODE_TREE, &fc.y_mode[0]);
            }
            info.mode = info.bmi[3];
        }
        1 => {
            let m0 = r.read_tree(&INTRA_MODE_TREE, &fc.y_mode[0]);
            info.bmi[0] = m0;
            info.bmi[2] = m0;
            let m1 = r.read_tree(&INTRA_MODE_TREE, &fc.y_mode[0]);
            info.bmi[1] = m1;
            info.bmi[3] = m1;
            info.mode = m1;
        }
        2 => {
            let m0 = r.read_tree(&INTRA_MODE_TREE, &fc.y_mode[0]);
            info.bmi[0] = m0;
            info.bmi[1] = m0;
            let m2 = r.read_tree(&INTRA_MODE_TREE, &fc.y_mode[0]);
            info.bmi[2] = m2;
            info.bmi[3] = m2;
            info.mode = m2;
        }
        _ => {
            info.mode = r.read_tree(&INTRA_MODE_TREE, &fc.y_mode[SIZE_GROUP_LOOKUP[bsize] as usize]);
        }
    }
    info.uv_mode = r.read_tree(&INTRA_MODE_TREE, &fc.uv_mode[info.mode as usize]);
    info.interp_filter = SWITCHABLE_FILTERS;
    info.ref_frame = [INTRA_FRAME, NO_REF_FRAME];
}

/// `is_inside` (`vp9_mvref_common.h:278`) — note it bounds rows by the FRAME
/// and columns by the TILE's end; there is no row bound against the tile.
fn is_inside(c: &InterBlockCtx<'_>, row: usize, col: usize, p: (i32, i32)) -> bool {
    let r = row as i32 + p.0;
    let cc = col as i32 + p.1;
    !(r < 0 || cc < c.tile_col_start as i32 || r >= c.mi_rows as i32 || cc >= c.tile_col_end as i32)
}

/// `get_sub_block_mv` (`vp9_mvref_common.h:224`).
fn get_sub_block_mv(cand: &MiInfo, which: usize, search_col: i32, block: i32) -> (i32, i32) {
    if block >= 0 && cand.sb_type < 3 {
        let b = IDX_N_COLUMN_TO_SUBBLOCK[block as usize][usize::from(search_col == 0)];
        cand.bmi_mv[b][which]
    } else {
        cand.mv[which]
    }
}

/// `use_mv_hp`: a small predictor keeps 1/8-pel precision.
fn use_mv_hp(mv: (i32, i32)) -> bool {
    mv.0.abs() < MV_HP_THRESH && mv.1.abs() < MV_HP_THRESH
}

/// `lower_mv_precision`: round the predictor to 1/4-pel when hp is not used.
fn lower_mv_precision(mv: &mut (i32, i32), allow_hp: bool) {
    if !(allow_hp && use_mv_hp(*mv)) {
        if mv.0 & 1 != 0 {
            mv.0 += if mv.0 > 0 { -1 } else { 1 };
        }
        if mv.1 & 1 != 0 {
            mv.1 += if mv.1 > 0 { -1 } else { 1 };
        }
    }
}

/// `clamp_mv_ref`.
fn clamp_mv_ref(mv: &mut (i32, i32), c: &InterBlockCtx<'_>, row: usize, col: usize, bw: usize, bh: usize) {
    // xd->mb_to_{left,right,top,bottom}_edge (vp9_onyxc_int.h:424-427). The
    // right/bottom differences are SIGNED in libvpx (a wide/tall block on the
    // frame's partial last row/column makes them negative), so they must not be
    // computed in usize.
    let left = -(col as i32 * 64);
    let right = (c.mi_cols as i32 - bw as i32 - col as i32) * 64;
    let top = -(row as i32 * 64);
    let bottom = (c.mi_rows as i32 - bh as i32 - row as i32) * 64;
    // `clamp_mv(mv, min_col, max_col, min_row, max_row)` (vp9_mv.h:47): the
    // COLUMN takes the left/right edges and the ROW the top/bottom ones, and
    // `mv.0` is the row (read_mv fills it from the vertical joint component).
    mv.1 = mv.1.clamp(left - MV_BORDER, right + MV_BORDER);
    mv.0 = mv.0.clamp(top - MV_BORDER, bottom + MV_BORDER);
}

/// `ADD_MV_REF_LIST_EB`: returns true when the C `goto Done` fires.
fn add_mv_ref_list_eb(
    mv: (i32, i32),
    list: &mut [(i32, i32); MAX_MV_REF_CANDIDATES],
    count: &mut usize,
    early_break: bool,
) -> bool {
    if *count > 0 {
        if mv != list[0] {
            list[*count] = mv;
            *count += 1;
            true
        } else {
            false
        }
    } else {
        list[0] = mv;
        *count = 1;
        early_break
    }
}

/// `scale_mv` (the decoder's sign-flip-only variant, `vp9_mvref_common.h`).
fn scale_mv(cand: &MiInfo, which: usize, ref_frame: i8, sign_bias: [bool; 4]) -> (i32, i32) {
    let mv = cand.mv[which];
    if sign_bias[cand.ref_frame[which] as usize] != sign_bias[ref_frame as usize] {
        (-mv.0, -mv.1)
    } else {
        mv
    }
}

/// `dec_find_mv_refs` (`vp9_decodemv.c:495`): the candidate list, clamped.
/// Returns `(refmv_count, list)`.
#[allow(clippy::too_many_arguments)]
fn find_mv_refs(
    st: &MiState,
    c: &InterBlockCtx<'_>,
    mode: u8,
    ref_frame: i8,
    bsize: usize,
    row: usize,
    col: usize,
    block: i32,
    bw: usize,
    bh: usize,
) -> (usize, [(i32, i32); MAX_MV_REF_CANDIDATES]) {
    let search = &MV_REF_BLOCKS[bsize];
    let early_break = mode != NEARMV;
    let mvref_dbg = crate::mvdbg_enabled() && mode == NEWMV;
    let mut list = [(0, 0); MAX_MV_REF_CANDIDATES];
    let mut count = 0usize;
    let mut different_ref_found = false;
    let mut done = false;

    let cell = |p: (i32, i32)| -> Option<&MiInfo> {
        st.at((row as i32 + p.0) as usize, (col as i32 + p.1) as usize)
    };

    let mut i = 0usize;
    if block >= 0 {
        while i < 2 && !done {
            let p = search[i];
            if is_inside(c, row, col, p) {
                different_ref_found = true;
                if let Some(cand) = cell(p) {
                    if cand.ref_frame[0] == ref_frame {
                        done = add_mv_ref_list_eb(
                            get_sub_block_mv(cand, 0, p.1, block),
                            &mut list,
                            &mut count,
                            early_break,
                        );
                    } else if cand.ref_frame[1] == ref_frame {
                        done = add_mv_ref_list_eb(
                            get_sub_block_mv(cand, 1, p.1, block),
                            &mut list,
                            &mut count,
                            early_break,
                        );
                    }
                }
            }
            i += 1;
        }
    }
    while i < MVREF_NEIGHBOURS && !done {
        let p = search[i];
        if is_inside(c, row, col, p) {
            different_ref_found = true;
            if let Some(cand) = cell(p) {
                if cand.ref_frame[0] == ref_frame {
                    done = add_mv_ref_list_eb(cand.mv[0], &mut list, &mut count, early_break);
                } else if cand.ref_frame[1] == ref_frame {
                    done = add_mv_ref_list_eb(cand.mv[1], &mut list, &mut count, early_break);
                }
            }
        }
        i += 1;
    }

    if !done {
        if let Some(prev) = c.prev_mvs {
            let e = prev[row * c.mi_cols + col];
            if e.ref_frame[0] == ref_frame {
                done = add_mv_ref_list_eb(e.mv[0], &mut list, &mut count, early_break);
            } else if e.ref_frame[1] == ref_frame {
                done = add_mv_ref_list_eb(e.mv[1], &mut list, &mut count, early_break);
            }
        }
    }

    if crate::mvdbg2_enabled()
        && ((row == 8 && col == 104) || (row == 32 && col == 64) || (row == 8 && col == 32))
    {
        for (i, p) in search.iter().enumerate() {
            let ins = is_inside(c, row, col, *p);
            let c2 = st.at((row as i32 + p.0) as usize, (col as i32 + p.1) as usize);
            let qr = (row as i32 + p.0) as usize;
            let qc = (col as i32 + p.1) as usize;
            match c2 {
                Some(cd) => eprintln!(
                    "MVC r={row} c={col} i={i} p={p:?} q=({qr} {qc}) idx={} inside={ins} ref0={} ref1={} mv0={:?} mv1={:?} inter={}",
                    qr * c.mi_cols + qc,
                    cd.ref_frame[0], cd.ref_frame[1], cd.mv[0], cd.mv[1], cd.is_inter
                ),
                None => eprintln!("MVC r={row} c={col} i={i} p={p:?} q=({qr} {qc}) inside={ins} none"),
            }
        }
    }
    if mvref_dbg {
        eprintln!(
            "MVREF r={row} c={col} bsize={bsize} mode={mode} n={count} l0={:?} l1={:?}",
            list[0], list[1]
        );
    }
    if !done && different_ref_found {
        for p in search.iter() {
            if !is_inside(c, row, col, *p) {
                continue;
            }
            let Some(cand) = cell(*p) else { continue };
            if !cand.is_inter {
                continue;
            }
            if cand.ref_frame[0] != ref_frame {
                done = add_mv_ref_list_eb(
                    scale_mv(cand, 0, ref_frame, c.sign_bias),
                    &mut list,
                    &mut count,
                    early_break,
                );
            }
            if done {
                break;
            }
            if cand.ref_frame[1] > INTRA_FRAME
                && cand.ref_frame[1] != ref_frame
                && cand.mv[1] != cand.mv[0]
            {
                done = add_mv_ref_list_eb(
                    scale_mv(cand, 1, ref_frame, c.sign_bias),
                    &mut list,
                    &mut count,
                    early_break,
                );
            }
        }
    }

    if !done {
        if let Some(prev) = c.prev_mvs {
            let e = prev[row * c.mi_cols + col];
            if e.ref_frame[0] != ref_frame && e.ref_frame[0] > INTRA_FRAME {
                let mut mv = e.mv[0];
                if c.sign_bias[e.ref_frame[0] as usize] != c.sign_bias[ref_frame as usize] {
                    mv = (-mv.0, -mv.1);
                }
                done = add_mv_ref_list_eb(mv, &mut list, &mut count, early_break);
            }
            if !done
                && e.ref_frame[1] > INTRA_FRAME
                && e.ref_frame[1] != ref_frame
                && e.mv[1] != e.mv[0]
            {
                let mut mv = e.mv[1];
                if c.sign_bias[e.ref_frame[1] as usize] != c.sign_bias[ref_frame as usize] {
                    mv = (-mv.0, -mv.1);
                }
                add_mv_ref_list_eb(mv, &mut list, &mut count, early_break);
            }
        }
    }

    // `dec_find_mv_refs`' tail (vp9_decodemv.c:626): an early exit (`goto Done`
    // -- either the second candidate was added, or `early_break` stopped us at
    // the first) JUMPS OVER the mode-derived override and returns the count as
    // accumulated; falling out of the loops applies the override, so a NEARMV
    // block reports two candidates even when only one was found and the caller
    // then reads the zeroed second slot. The list was memset to zero first, so
    // that second slot is (0, 0), and the clamp covers the final count only.
    let n = if done {
        count
    } else if mode == NEARMV {
        MAX_MV_REF_CANDIDATES
    } else {
        1
    };
    for k in 0..n {
        clamp_mv_ref(&mut list[k], c, row, col, bw, bh);
    }
    (n, list)
}

/// `append_sub8x8_mvs_for_idx` (`vp9_decodemv.c:620`).
#[allow(clippy::too_many_arguments)]
fn append_sub8x8_mvs_for_idx(
    st: &MiState,
    c: &InterBlockCtx<'_>,
    info: &MiInfo,
    bmode: u8,
    block: usize,
    rf: usize,
    row: usize,
    col: usize,
    bw: usize,
    bh: usize,
) -> (i32, i32) {
    let bsize = info.sb_type;
    match block {
        0 => {
            let (n, list) = find_mv_refs(st, c, bmode, info.ref_frame[rf], bsize, row, col, 0, bw, bh);
            list[n - 1]
        }
        1 | 2 => {
            if bmode == NEARESTMV {
                info.bmi_mv[0][rf]
            } else {
                let (_n, list) =
                    find_mv_refs(st, c, bmode, info.ref_frame[rf], bsize, row, col, block as i32, bw, bh);
                let mut out = (0, 0);
                for m in list.iter() {
                    if info.bmi_mv[0][rf] != *m {
                        out = *m;
                        break;
                    }
                }
                out
            }
        }
        _ => {
            if bmode == NEARESTMV {
                info.bmi_mv[2][rf]
            } else {
                if info.bmi_mv[2][rf] != info.bmi_mv[1][rf] {
                    info.bmi_mv[1][rf]
                } else if info.bmi_mv[2][rf] != info.bmi_mv[0][rf] {
                    info.bmi_mv[0][rf]
                } else {
                    let (_n, list) = find_mv_refs(
                        st,
                        c,
                        bmode,
                        info.ref_frame[rf],
                        bsize,
                        row,
                        col,
                        block as i32,
                        bw,
                        bh,
                    );
                    let mut out = (0, 0);
                    for m in list.iter() {
                        if info.bmi_mv[2][rf] != *m {
                            out = *m;
                            break;
                        }
                    }
                    out
                }
            }
        }
    }
}

/// `read_mv_component` (`vp9_decodemv.c:246`).
fn read_mv_component(r: &mut BoolDecoder, fc: &FrameContext, comp: usize, use_hp: bool) -> i32 {
    let dbg = crate::interdump_enabled();
    if dbg {
        eprintln!("MVF c{comp} sign");
    }
    let sign = r.read_bool(fc.nmv_sign[comp]);
    if dbg {
        eprintln!("MVF c{comp} cls");
    }
    let mv_class = r.read_tree(&MV_CLASS_TREE, &fc.nmv_classes[comp]) as i32;
    let class0 = mv_class == 0;
    let mut d = 0i32;
    let mut mag;
    if dbg {
        eprintln!("MVF c{comp} class={mv_class}");
    }
    if class0 {
        if dbg {
            eprintln!("MVF c{comp} class0bit");
        }
        d = r.read_bool(fc.nmv_class0[comp][0]) as i32;
        mag = 0;
    } else {
        let n = mv_class + CLASS0_BITS - 1;
        for i in 0..n {
            if dbg {
                eprintln!("MVF c{comp} bits[{i}]");
            }
            d |= (r.read_bool(fc.nmv_bits[comp][i as usize]) as i32) << i;
        }
        mag = CLASS0_SIZE << (mv_class + 2);
    }
    if dbg {
        eprintln!("MVF c{comp} fp");
    }
    let fr = if class0 {
        r.read_tree(&MV_FP_TREE, &fc.nmv_class0_fp[comp][d as usize])
    } else {
        r.read_tree(&MV_FP_TREE, &fc.nmv_fp[comp])
    } as i32;
    let hp = if use_hp {
        if dbg {
            eprintln!("MVF c{comp} hp");
        }
        r.read_bool(if class0 {
            fc.nmv_class0_hp[comp]
        } else {
            fc.nmv_hp[comp]
        }) as i32
    } else {
        1
    };
    mag += ((d << 3) | (fr << 1) | hp) + 1;
    if sign {
        -mag
    } else {
        mag
    }
}

/// `read_mv`: returns `(mv, diff)`.
fn read_mv(
    r: &mut BoolDecoder,
    pred: (i32, i32),
    fc: &FrameContext,
    allow_hp: bool,
) -> ((i32, i32), (i32, i32)) {
    let joint = r.read_tree(&MV_JOINT_TREE, &fc.nmv_joints);
    let use_hp = allow_hp && use_mv_hp(pred);
    if crate::interdump_enabled() {
        eprintln!("MVJ joint={joint} use_hp={use_hp} pred={pred:?}");
    }
    // mv_joint_vertical = joint & 2, mv_joint_horizontal = joint & 1.
    let mut diff = (0i32, 0i32);
    if joint & 2 != 0 {
        diff.0 = read_mv_component(r, fc, 0, use_hp);
    }
    if joint & 1 != 0 {
        diff.1 = read_mv_component(r, fc, 1, use_hp);
    }
    ((pred.0 + diff.0, pred.1 + diff.1), diff)
}

/// `assign_mv`: returns `(mv, diff)`; only `NEWMV` consumes bits.
fn assign_mv(
    r: &mut BoolDecoder,
    fc: &FrameContext,
    mode: u8,
    ref_mv: &[(i32, i32); 2],
    near_nearest: &[(i32, i32); 2],
    is_compound: usize,
    allow_hp: bool,
) -> crate::Result<([(i32, i32); 2], [(i32, i32); 2])> {
    let mut mv = [(0, 0); 2];
    let mut diff = [(0, 0); 2];
    match mode {
        NEWMV => {
            for i in 0..1 + is_compound {
                let (m, d) = read_mv(r, ref_mv[i], fc, allow_hp);
                mv[i] = m;
                diff[i] = d;
            }
        }
        NEARMV | NEARESTMV => mv = *near_nearest,
        ZEROMV => {}
        other => {
            return Err(crate::Error::corrupt(format!(
                "VP9 assign_mv: invalid inter mode {other}"
            )))
        }
    }
    Ok((mv, diff))
}

/// `read_block_reference_mode`.
fn read_block_reference_mode(
    r: &mut BoolDecoder,
    c: &InterBlockCtx<'_>,
    fc: &FrameContext,
    nb: Nb<'_>,
) -> u8 {
    if c.ifs.reference_mode == REFERENCE_MODE_SELECT {
        let ctx = reference_mode_context(c, nb);
        u8::from(r.read_bool(fc.comp_inter[ctx]))
    } else {
        c.ifs.reference_mode
    }
}

/// `vp9_get_reference_mode_context` (`vp9_pred_common.c`).
fn reference_mode_context(c: &InterBlockCtx<'_>, nb: Nb<'_>) -> usize {
    let fixed = c.ifs.comp_fixed_ref;
    let has_second = |m: &MiInfo| m.ref_frame[1] > INTRA_FRAME;
    match (nb.above, nb.left) {
        (Some(a), Some(l)) => {
            if !has_second(a) && !has_second(l) {
                usize::from((a.ref_frame[0] == fixed) ^ (l.ref_frame[0] == fixed))
            } else if !has_second(a) {
                2 + usize::from(a.ref_frame[0] == fixed || !a.is_inter)
            } else if !has_second(l) {
                2 + usize::from(l.ref_frame[0] == fixed || !l.is_inter)
            } else {
                4
            }
        }
        (Some(m), None) | (None, Some(m)) => {
            if !has_second(m) {
                usize::from(m.ref_frame[0] == fixed)
            } else {
                3
            }
        }
        (None, None) => 1,
    }
}

/// `vp9_get_pred_context_comp_ref_p` (`vp9_pred_common.c`).
fn pred_context_comp_ref_p(c: &InterBlockCtx<'_>, nb: Nb<'_>) -> usize {
    let fixed = c.ifs.comp_fixed_ref;
    let var = c.ifs.comp_var_ref;
    let var_ref_idx = usize::from(!c.sign_bias[fixed as usize]);
    let has_second = |m: &MiInfo| m.ref_frame[1] > INTRA_FRAME;
    let vr = |m: &MiInfo| -> i8 {
        if has_second(m) {
            m.ref_frame[var_ref_idx]
        } else {
            m.ref_frame[0]
        }
    };
    match (nb.above, nb.left) {
        (Some(a), Some(l)) => {
            if !a.is_inter && !l.is_inter {
                2
            } else if !a.is_inter || !l.is_inter {
                let e = if !a.is_inter { l } else { a };
                if !has_second(e) {
                    1 + 2 * usize::from(e.ref_frame[0] != var[1])
                } else {
                    1 + 2 * usize::from(e.ref_frame[var_ref_idx] != var[1])
                }
            } else {
                let a_sg = !has_second(a);
                let l_sg = !has_second(l);
                let vrfa = vr(a);
                let vrfl = vr(l);
                if vrfa == vrfl && var[1] == vrfa {
                    0
                } else if l_sg && a_sg {
                    if (vrfa == fixed && vrfl == var[0]) || (vrfl == fixed && vrfa == var[0]) {
                        4
                    } else if vrfa == vrfl {
                        3
                    } else {
                        1
                    }
                } else if l_sg || a_sg {
                    let vrfc = if l_sg { vrfa } else { vrfl };
                    let rfs = if a_sg { vrfa } else { vrfl };
                    if vrfc == var[1] && rfs != var[1] {
                        1
                    } else if rfs == var[1] && vrfc != var[1] {
                        2
                    } else {
                        4
                    }
                } else if vrfa == vrfl {
                    4
                } else {
                    2
                }
            }
        }
        (Some(e), None) | (None, Some(e)) => {
            if !e.is_inter {
                2
            } else if has_second(e) {
                4 * usize::from(e.ref_frame[var_ref_idx] != var[1])
            } else {
                3 * usize::from(e.ref_frame[0] != var[1])
            }
        }
        (None, None) => 2,
    }
}

/// `vp9_get_pred_context_single_ref_p1` (`vp9_pred_common.c`).
fn pred_context_single_ref_p1(nb: Nb<'_>) -> usize {
    let has_second = |m: &MiInfo| m.ref_frame[1] > INTRA_FRAME;
    match (nb.above, nb.left) {
        (Some(a), Some(l)) => {
            if !a.is_inter && !l.is_inter {
                2
            } else if !a.is_inter || !l.is_inter {
                let e = if !a.is_inter { l } else { a };
                if !has_second(e) {
                    4 * usize::from(e.ref_frame[0] == LAST_FRAME)
                } else {
                    1 + usize::from(e.ref_frame[0] == LAST_FRAME || e.ref_frame[1] == LAST_FRAME)
                }
            } else {
                let a2 = has_second(a);
                let l2 = has_second(l);
                if a2 && l2 {
                    1 + usize::from(
                        a.ref_frame[0] == LAST_FRAME
                            || a.ref_frame[1] == LAST_FRAME
                            || l.ref_frame[0] == LAST_FRAME
                            || l.ref_frame[1] == LAST_FRAME,
                    )
                } else if a2 || l2 {
                    let rfs = if !a2 { a.ref_frame[0] } else { l.ref_frame[0] };
                    let crf1 = if a2 { a.ref_frame[0] } else { l.ref_frame[0] };
                    let crf2 = if a2 { a.ref_frame[1] } else { l.ref_frame[1] };
                    if rfs == LAST_FRAME {
                        3 + usize::from(crf1 == LAST_FRAME || crf2 == LAST_FRAME)
                    } else {
                        usize::from(crf1 == LAST_FRAME || crf2 == LAST_FRAME)
                    }
                } else {
                    2 * usize::from(a.ref_frame[0] == LAST_FRAME)
                        + 2 * usize::from(l.ref_frame[0] == LAST_FRAME)
                }
            }
        }
        (Some(e), None) | (None, Some(e)) => {
            if !e.is_inter {
                2
            } else if !has_second(e) {
                4 * usize::from(e.ref_frame[0] == LAST_FRAME)
            } else {
                1 + usize::from(e.ref_frame[0] == LAST_FRAME || e.ref_frame[1] == LAST_FRAME)
            }
        }
        (None, None) => 2,
    }
}

/// `vp9_get_pred_context_single_ref_p2` (`vp9_pred_common.c`).
fn pred_context_single_ref_p2(nb: Nb<'_>) -> usize {
    let has_second = |m: &MiInfo| m.ref_frame[1] > INTRA_FRAME;
    match (nb.above, nb.left) {
        (Some(a), Some(l)) => {
            if !a.is_inter && !l.is_inter {
                2
            } else if !a.is_inter || !l.is_inter {
                let e = if !a.is_inter { l } else { a };
                if !has_second(e) {
                    if e.ref_frame[0] == LAST_FRAME {
                        3
                    } else {
                        4 * usize::from(e.ref_frame[0] == GOLDEN_FRAME)
                    }
                } else {
                    1 + 2 * usize::from(
                        e.ref_frame[0] == GOLDEN_FRAME || e.ref_frame[1] == GOLDEN_FRAME,
                    )
                }
            } else {
                let a2 = has_second(a);
                let l2 = has_second(l);
                if a2 && l2 {
                    if a.ref_frame[0] == l.ref_frame[0] && a.ref_frame[1] == l.ref_frame[1] {
                        3 * usize::from(
                            a.ref_frame[0] == GOLDEN_FRAME
                                || a.ref_frame[1] == GOLDEN_FRAME
                                || l.ref_frame[0] == GOLDEN_FRAME
                                || l.ref_frame[1] == GOLDEN_FRAME,
                        )
                    } else {
                        2
                    }
                } else if a2 || l2 {
                    let rfs = if !a2 { a.ref_frame[0] } else { l.ref_frame[0] };
                    let crf1 = if a2 { a.ref_frame[0] } else { l.ref_frame[0] };
                    let crf2 = if a2 { a.ref_frame[1] } else { l.ref_frame[1] };
                    let crf_golden = usize::from(crf1 == GOLDEN_FRAME || crf2 == GOLDEN_FRAME);
                    if rfs == GOLDEN_FRAME {
                        3 + crf_golden
                    } else if rfs == ALTREF_FRAME {
                        crf_golden
                    } else {
                        1 + 2 * crf_golden
                    }
                } else if a.ref_frame[0] == LAST_FRAME && l.ref_frame[0] == LAST_FRAME {
                    3
                } else if a.ref_frame[0] == LAST_FRAME || l.ref_frame[0] == LAST_FRAME {
                    let edge0 = if a.ref_frame[0] == LAST_FRAME {
                        l.ref_frame[0]
                    } else {
                        a.ref_frame[0]
                    };
                    4 * usize::from(edge0 == GOLDEN_FRAME)
                } else {
                    2 * usize::from(a.ref_frame[0] == GOLDEN_FRAME)
                        + 2 * usize::from(l.ref_frame[0] == GOLDEN_FRAME)
                }
            }
        }
        (Some(e), None) | (None, Some(e)) => {
            if !e.is_inter || (e.ref_frame[0] == LAST_FRAME && !has_second(e)) {
                2
            } else if !has_second(e) {
                4 * usize::from(e.ref_frame[0] == GOLDEN_FRAME)
            } else {
                3 * usize::from(e.ref_frame[0] == GOLDEN_FRAME || e.ref_frame[1] == GOLDEN_FRAME)
            }
        }
        (None, None) => 2,
    }
}

/// `read_ref_frames`.
fn read_ref_frames(
    r: &mut BoolDecoder,
    c: &InterBlockCtx<'_>,
    fc: &FrameContext,
    nb: Nb<'_>,
    segment_id: u8,
    out: &mut [i8; 2],
) {
    if seg_feature(c.seg, segment_id, SEG_LVL_REF_FRAME) {
        out[0] = c.seg.feature_data[segment_id as usize][SEG_LVL_REF_FRAME] as i8;
        out[1] = NO_REF_FRAME;
        return;
    }
    let mode = read_block_reference_mode(r, c, fc, nb);
    if mode == COMPOUND_REFERENCE {
        let idx = usize::from(c.sign_bias[c.ifs.comp_fixed_ref as usize]);
        let ctx = pred_context_comp_ref_p(c, nb);
        let bit = usize::from(r.read_bool(fc.comp_ref[ctx]));
        out[idx] = c.ifs.comp_fixed_ref;
        out[1 - idx] = c.ifs.comp_var_ref[bit];
    } else {
        let ctx0 = pred_context_single_ref_p1(nb);
        let bit0 = r.read_bool(fc.single_ref[ctx0][0]);
        if bit0 {
            let ctx1 = pred_context_single_ref_p2(nb);
            let bit1 = r.read_bool(fc.single_ref[ctx1][1]);
            out[0] = if bit1 { ALTREF_FRAME } else { GOLDEN_FRAME };
        } else {
            out[0] = LAST_FRAME;
        }
        out[1] = NO_REF_FRAME;
    }
}

/// `read_inter_block_mode_info` (`vp9_decodemv.c:702`).
#[allow(clippy::too_many_arguments)]
fn read_inter_block_mode_info(
    r: &mut BoolDecoder,
    st: &MiState,
    c: &InterBlockCtx<'_>,
    fc: &FrameContext,
    info: &mut MiInfo,
    nb: Nb<'_>,
    row: usize,
    col: usize,
    partition: u8,
) -> crate::Result<()> {
    let bsize = info.sb_type;
    let mut best_ref_mvs = [(0, 0); 2];
    read_ref_frames(r, c, fc, nb, info.segment, &mut info.ref_frame);
    let is_compound = usize::from(info.ref_frame[1] > INTRA_FRAME);
    let inter_mode_ctx = get_mode_context(st, c, bsize, row, col);

    if seg_feature(c.seg, info.segment, SEG_LVL_SKIP) {
        info.mode = ZEROMV;
        if bsize < 3 {
            return Err(crate::Error::corrupt(
                "VP9 segment skip feature on a sub-8x8 block",
            ));
        }
    } else if bsize >= 3 {
        info.mode = read_inter_mode(r, fc, inter_mode_ctx);
    }

    info.interp_filter = if c.frame_interp == SWITCHABLE {
        let ictx = switchable_interp_ctx(nb);
        if crate::interdump_enabled() {
            eprintln!(
                "SWI ctx={} p0={} p1={}",
                ictx, fc.switchable_interp[ictx][0], fc.switchable_interp[ictx][1]
            );
        }
        let t = r.read_tree(&SWITCHABLE_INTERP_TREE, &fc.switchable_interp[ictx]);
        if crate::interdump_enabled() {
            eprintln!("SWItype {t}");
        }
        t
    } else {
        c.frame_interp
    };

    // Block dimensions in 8x8 units for the MV clamp (libvpx's bw/bh from
    // `1 << (bwl - 1)`: a sub-8x8 shape occupies a whole 8x8 cell).
    let bw = if bsize < 3 {
        1
    } else {
        1usize << (B_WIDTH_LOG2_LOOKUP[bsize] as usize - 1)
    };
    let bh = if bsize < 3 {
        1
    } else {
        1usize << (B_HEIGHT_LOG2_LOOKUP[bsize] as usize - 1)
    };

    if bsize < 3 {
        // PARTITION_VERT clears the wide bit, PARTITION_HORZ the tall one.
        let num_4x4_w = 1usize << u32::from(partition & 2 == 0);
        let num_4x4_h = 1usize << u32::from(partition & 1 == 0);
        let mut got_mv_refs_for_new = false;
        let mut best_sub8x8 = [(0, 0); 2];
        let mut b_mode = info.mode;
        let mut idy = 0usize;
        while idy < 2 {
            let mut idx = 0usize;
            while idx < 2 {
                let j = idy * 2 + idx;
                b_mode = read_inter_mode(r, fc, inter_mode_ctx);
                info.bmi[j] = b_mode;
                if b_mode == NEARESTMV || b_mode == NEARMV {
                    for rf in 0..1 + is_compound {
                        best_sub8x8[rf] = append_sub8x8_mvs_for_idx(
                            st, c, info, b_mode, j, rf, row, col, bw, bh,
                        );
                    }
                } else if b_mode == NEWMV && !got_mv_refs_for_new {
                    for rf in 0..1 + is_compound {
                        let frame = info.ref_frame[rf];
                        let (_n, list) =
                            find_mv_refs(st, c, NEWMV, frame, bsize, row, col, -1, bw, bh);
                        let mut mv = list[0];
                        lower_mv_precision(&mut mv, c.allow_hp);
                        best_ref_mvs[rf] = mv;
                        got_mv_refs_for_new = true;
                    }
                }
                let (mv, diff) = assign_mv(
                    r,
                    fc,
                    b_mode,
                    &best_ref_mvs,
                    &best_sub8x8,
                    is_compound,
                    c.allow_hp,
                )?;
                info.bmi_mv[j] = mv;
                info.mv_diff = diff;
                if num_4x4_h == 2 {
                    info.bmi_mv[j + 2] = info.bmi_mv[j];
                }
                if num_4x4_w == 2 {
                    info.bmi_mv[j + 1] = info.bmi_mv[j];
                }
                idx += num_4x4_w;
            }
            idy += num_4x4_h;
        }
        info.mode = b_mode;
        info.mv = info.bmi_mv[3];
    } else {
        if info.mode != ZEROMV {
            for rf in 0..1 + is_compound {
                let frame = info.ref_frame[rf];
                let (n, list) = find_mv_refs(st, c, info.mode, frame, bsize, row, col, -1, bw, bh);
                let mut mv = list[n - 1];
                lower_mv_precision(&mut mv, c.allow_hp);
                best_ref_mvs[rf] = mv;
            }
        }
        let (mv, diff) = assign_mv(
            r,
            fc,
            info.mode,
            &best_ref_mvs,
            &best_ref_mvs,
            is_compound,
            c.allow_hp,
        )?;
        info.mv = mv;
        info.mv_diff = diff;
    }
    Ok(())
}

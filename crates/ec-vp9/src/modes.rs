//! Partition coding and intra-frame mode info (spec 6.3.4/6.3.5,
//! libvpx `vp9_decodeframe.c decode_partition` + `vp9_decodemv.c
//! read_intra_frame_mode_info`).

use crate::bool::BoolDecoder;
use crate::header::FrameContext;
use crate::tables::*;

/// One block's parsed prediction record (`MODE_INFO`).
#[derive(Clone, Default)]
pub(crate) struct MiInfo {
    pub sb_type: usize,
    pub segment: u8,
    pub skip: bool,
    pub tx_size: usize,
    pub mode: u8,
    pub uv_mode: u8,
    /// Per-4x4 sub-block y modes, valid when `sb_type < 3` (BLOCK_8X8).
    pub bmi: [u8; 4],
    /// `is_inter_block(mi)`: `ref_frame[0] > INTRA_FRAME`.
    pub is_inter: bool,
    /// `mi->ref_frame[0]`/`[1]`: `INTRA_FRAME` (0), `LAST`/`GOLDEN`/`ALTREF`
    /// (1..3), `NO_REF_FRAME` (-1).
    pub ref_frame: [i8; 2],
    /// `mi->mv[..]`: the refreshed MV (predictor + codeword diff).
    pub mv: [(i32, i32); 2],
    /// The raw MV codeword diff of each reference frame.
    pub mv_diff: [(i32, i32); 2],
    /// `mi->bmi[j].as_mv`.
    pub bmi_mv: [[(i32, i32); 2]; 4],
    /// `mi->seg_id_predicted`.
    pub seg_id_predicted: bool,
    /// `mi->interp_filter`.
    pub interp_filter: u8,
}

/// The mode-info grid and the partition contexts of the frame.
pub(crate) struct MiState {
    pub mi_cols: usize,
    pub mi_rows: usize,
    /// `mi_row * mi_cols + mi_col`; every cell a block covers holds a
    /// clone of its record (libvpx shares the pointer). Frame-scoped: a
    /// tile row k > 0 reads the rows written by tile row k - 1.
    pub grid: Vec<Option<MiInfo>>,
    /// `above_seg_context`, one byte per 8x8 column (frame-scoped, like
    /// libvpx's `cm->above_seg_context`, memset once per frame).
    pub above_seg: Vec<u8>,
    /// `left_seg_context`, one byte per 8x8 row (reset per SB row).
    pub left_seg: [u8; 32],
}

impl MiState {
    pub(crate) fn new(mi_cols: usize, mi_rows: usize) -> Self {
        MiState {
            mi_cols,
            mi_rows,
            grid: vec![None; mi_cols * mi_rows],
            // `above_seg_context` is `mi_cols_aligned_to_sb` long in libvpx;
            // `update_partition_ctx` fills `[col .. col + bw)` which can
            // overrun `mi_cols` by up to a full superblock (a 64x64 block at
            // the last mi col).
            above_seg: vec![0; mi_cols.div_ceil(8) * 8],
            left_seg: [0; 32],
        }
    }

    /// The grid cell at an absolute (row, col), `None` when not decoded yet.
    pub(crate) fn at(&self, row: usize, col: usize) -> Option<&MiInfo> {
        self.grid.get(row * self.mi_cols + col).and_then(|c| c.as_ref())
    }

    /// `xd->above_mi` (`set_mi_row_col`, vp9_onyxc_int.h:430): available iff
    /// the block is not on the FRAME's first mi row. libvpx gates on
    /// `mi_row != 0`, not on the tile row start — for a tile-row stream the
    /// row above tile row k > 0 was decoded by tile row k - 1 into the same
    /// frame-scoped mi grid.
    pub(crate) fn above_mi(&self, row: usize, col: usize) -> Option<&MiInfo> {
        if row > 0 {
            self.grid[(row - 1) * self.mi_cols + col].as_ref()
        } else {
            None
        }
    }

    pub(crate) fn left_mi(&self, row: usize, col: usize, tile_col_start: usize) -> Option<&MiInfo> {
        if col > tile_col_start {
            self.grid[row * self.mi_cols + col - 1].as_ref()
        } else {
            None
        }
    }

    /// `dec_partition_plane_context`.
    pub(crate) fn partition_ctx(&self, row: usize, col: usize, bsl: u32) -> usize {
        let above = (self.above_seg[col] >> bsl) & 1;
        let left = (self.left_seg[row & 31] >> bsl) & 1;
        (left * 2 + above) as usize + (bsl as usize) * 4
    }

    /// `dec_update_partition_context`; `partition_context_lookup[subsize]`.
    pub(crate) fn update_partition_ctx(
        &mut self,
        row: usize,
        col: usize,
        subsize: usize,
        bw: usize,
    ) {
        // (above, left) per BLOCK_SIZE, common_data.c:230.
        const LOOKUP: [(u8, u8); 13] = [
            (15, 15),
            (15, 14),
            (14, 15),
            (14, 14),
            (14, 12),
            (12, 14),
            (12, 12),
            (12, 8),
            (8, 12),
            (8, 8),
            (8, 0),
            (0, 8),
            (0, 0),
        ];
        let (a, l) = LOOKUP[subsize];
        self.above_seg[col..col + bw].fill(a);
        self.left_seg[row & 31..(row & 31) + bw].fill(l);
    }
}

/// `read_partition` (decodeframe.c:1195).
fn read_partition(
    r: &mut BoolDecoder,
    st: &MiState,
    partition_probs: &[[u8; 3]; 16],
    row: usize,
    col: usize,
    has_rows: bool,
    has_cols: bool,
    bsl: u32,
) -> crate::Result<u8> {
    let ctx = st.partition_ctx(row, col, bsl);
    let probs = &partition_probs[ctx];
    if has_rows && has_cols {
        Ok(r.read_tree(&PARTITION_TREE, probs))
    } else if !has_rows && has_cols {
        Ok(if r.read_bool(probs[1]) {
            3 /* SPLIT */
        } else {
            1 /* HORZ */
        })
    } else if has_rows && !has_cols {
        Ok(if r.read_bool(probs[2]) {
            3 /* SPLIT */
        } else {
            2 /* VERT */
        })
    } else {
        Ok(3)
    }
}

/// `read_intra_mode_kf` + `read_intra_frame_mode_info` (decodemv.c).
pub(crate) fn read_intra_frame_mode_info(
    r: &mut BoolDecoder,
    st: &mut MiState,
    seg: &ec_vp9_syntax::SegmentationParams,
    partition_probs: &[[u8; 3]; 16],
    row: usize,
    col: usize,
    sb_type: usize,
    x_mis: usize,
    y_mis: usize,
    tile_col_start: usize,
    tx_mode: u8,
    fc: &FrameContext,
) -> crate::Result<MiInfo> {
    let above_mi = st.above_mi(row, col).cloned();
    let left_mi = st.left_mi(row, col, tile_col_start).cloned();

    // read_intra_segment_id: keyframes have no temporal prediction; a
    // map that is not updated keeps the previous id, which for a
    // keyframe's fresh map is 0 (setup_past_independence cleared it).
    let segment = if seg.enabled && seg.update_map {
        r.read_tree(&SEGMENT_TREE, &seg.tree_probs)
    } else {
        0
    };

    // read_skip
    let skip_ctx = above_mi.as_ref().map_or(0, |m| u32::from(m.skip))
        + left_mi.as_ref().map_or(0, |u| u32::from(u.skip));
    let skip_ctx = skip_ctx as usize;
    let seg_skip = seg.enabled
        && seg
            .feature_enabled
            .get(segment as usize)
            .is_some_and(|f| f[3]);
    let skip = if seg_skip {
        true
    } else {
        r.read_bool(fc.skip[skip_ctx])
    };

    // read_tx_size
    let max_tx = MAX_TXSIZE_LOOKUP[sb_type] as usize;
    let tx_size = if tx_mode == TX_MODE_SELECT && sb_type >= 3 {
        // get_tx_size_context (vp9_pred_common.h:156): default each side to
        // max_tx, then substitute the other side when one is missing, in
        // this exact order — the `!has_above` fallback is load-bearing.
        let ctx = {
            let a = above_mi
                .as_ref()
                .map_or(max_tx, |m| if m.skip { max_tx } else { m.tx_size });
            let mut l = left_mi
                .as_ref()
                .map_or(max_tx, |m| if m.skip { max_tx } else { m.tx_size });
            if left_mi.is_none() {
                l = a;
            }
            let mut a = a;
            if above_mi.is_none() {
                a = l;
            }
            usize::from(a + l > max_tx)
        };
        // get_tx_probs (vp9_pred_common.h:173): the FRAME-updated tables
        // (`cm->fc->tx_probs`), not the const defaults.
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
        max_tx.min(TX_MODE_TO_BIGGEST_TX_SIZE[tx_mode as usize])
    };

    // y mode(s), per sub-block for sub-8x8 blocks
    let above_pair: Option<(usize, [u8; 4], u8)> =
        above_mi.as_ref().map(|m| (m.sb_type, m.bmi, m.mode));
    let left_pair: Option<(usize, [u8; 4], u8)> =
        left_mi.as_ref().map(|m| (m.sb_type, m.bmi, m.mode));
    let mut mi = MiInfo {
        sb_type,
        segment,
        skip,
        tx_size,
        mode: DC_PRED,
        uv_mode: DC_PRED,
        bmi: [DC_PRED; 4],
        is_inter: false,
        ref_frame: [0, -1],
        mv: [(0, 0); 2],
        mv_diff: [(0, 0); 2],
        bmi_mv: [[(0, 0); 2]; 4],
        seg_id_predicted: false,
        // read_intra_block_mode_info sets this so a later
        // get_pred_context_switchable_interp never sees a stale filter.
        interp_filter: 3,
    };
    match sb_type {
        0 => {
            // BLOCK_4X4
            for i in 0..4 {
                let a = above_block_mode(&mi.bmi, above_pair, i);
                let l = left_block_mode(&mi.bmi, left_pair, i);
                mi.bmi[i] = r.read_tree(&INTRA_MODE_TREE, &kf_y_mode_probs(a, l));
            }
            mi.mode = mi.bmi[3];
        }
        1 => {
            // BLOCK_4X8; decodemv.c commits bmi[0]/bmi[2] BEFORE the
            // second mode's probs are derived from the current bmi.
            let a0 = above_block_mode(&mi.bmi, above_pair, 0);
            let l0 = left_block_mode(&mi.bmi, left_pair, 0);
            let m0 = r.read_tree(&INTRA_MODE_TREE, &kf_y_mode_probs(a0, l0));
            mi.bmi[0] = m0;
            mi.bmi[2] = m0;
            let a1 = above_block_mode(&mi.bmi, above_pair, 1);
            let l1 = left_block_mode(&mi.bmi, left_pair, 1);
            let m1 = r.read_tree(&INTRA_MODE_TREE, &kf_y_mode_probs(a1, l1));
            mi.bmi[1] = m1;
            mi.bmi[3] = m1;
            mi.mode = m1;
        }
        2 => {
            // BLOCK_8X4; same commit-before-read ordering.
            let a0 = above_block_mode(&mi.bmi, above_pair, 0);
            let l0 = left_block_mode(&mi.bmi, left_pair, 0);
            let m0 = r.read_tree(&INTRA_MODE_TREE, &kf_y_mode_probs(a0, l0));
            mi.bmi[0] = m0;
            mi.bmi[1] = m0;
            let a2 = above_block_mode(&mi.bmi, above_pair, 2);
            let l2 = left_block_mode(&mi.bmi, left_pair, 2);
            let m2 = r.read_tree(&INTRA_MODE_TREE, &kf_y_mode_probs(a2, l2));
            mi.bmi[2] = m2;
            mi.bmi[3] = m2;
            mi.mode = m2;
        }
        _ => {
            let a = above_block_mode(&mi.bmi, above_pair, 0);
            let l = left_block_mode(&mi.bmi, left_pair, 0);
            mi.mode = r.read_tree(&INTRA_MODE_TREE, &kf_y_mode_probs(a, l));
        }
    }
    // uv mode: `vp9_kf_uv_mode_prob` indexed by y mode, decoded with the
    // SAME intra_mode_tree (decodemv.c:232 calls read_intra_mode).
    mi.uv_mode = r.read_tree(&INTRA_MODE_TREE, &kf_uv_probs(mi.mode));
    if crate::trace_enabled() {
        eprintln!(
            "MODE row={} col={} bsize={} skip={} tx={} y={} uv={}",
            row, col, sb_type, mi.skip as u8, mi.tx_size, mi.mode, mi.uv_mode
        );
    }
    Ok(mi)
}

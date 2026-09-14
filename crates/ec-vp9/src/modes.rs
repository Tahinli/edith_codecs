//! Partition coding and intra-frame mode info (spec 6.3.4/6.3.5,
//! libvpx `vp9_decodeframe.c decode_partition` + `vp9_decodemv.c
//! read_intra_frame_mode_info`).

use crate::bool::BoolDecoder;
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
}

/// The mode-info grid and the partition contexts of one tile pass.
pub(crate) struct MiState {
    pub mi_cols: usize,
    pub mi_rows: usize,
    /// `mi_row * mi_cols + mi_col`; every cell a block covers holds a
    /// clone of its record (libvpx shares the pointer).
    pub grid: Vec<Option<MiInfo>>,
    /// `above_seg_context`, one byte per 8x8 column (tile-scoped).
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
            above_seg: vec![0; mi_cols],
            left_seg: [0; 32],
        }
    }

    fn above_mi(&self, row: usize, col: usize, tile_row_start: usize) -> Option<&MiInfo> {
        if row > tile_row_start {
            self.grid[(row - 1) * self.mi_cols + col].as_ref()
        } else {
            None
        }
    }

    fn left_mi(&self, row: usize, col: usize, tile_col_start: usize) -> Option<&MiInfo> {
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
    tile_row_start: usize,
    tile_col_start: usize,
    tx_mode: u8,
    skip_probs: &[u8; 3],
) -> crate::Result<MiInfo> {
    let above_mi = st.above_mi(row, col, tile_row_start).cloned();
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
        r.read_bool(skip_probs[skip_ctx])
    };

    // read_tx_size
    let max_tx = MAX_TXSIZE_LOOKUP[sb_type] as usize;
    let tx_size = if tx_mode == TX_MODE_SELECT && sb_type >= 3 {
        // read_selected_tx_size: ctx from above/left tx sizes.
        let ctx = {
            let a = above_mi
                .as_ref()
                .map_or(max_tx, |m| if m.skip { max_tx } else { m.tx_size });
            let l = left_mi
                .as_ref()
                .map_or(a, |m| if m.skip { max_tx } else { m.tx_size });
            usize::from(a + l > max_tx)
        };
        let probs: &[u8] = match max_tx {
            1 => &TX_P8X8_PROB[ctx..ctx + 1],
            2 => &TX_P16X16_PROB[ctx * 2..ctx * 2 + 2],
            _ => &TX_P32X32_PROB[ctx * 3..ctx * 3 + 3],
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
            // BLOCK_4X8
            let a0 = above_block_mode(&mi.bmi, above_pair, 0);
            let l0 = left_block_mode(&mi.bmi, left_pair, 0);
            let m0 = r.read_tree(&INTRA_MODE_TREE, &kf_y_mode_probs(a0, l0));
            let a1 = above_block_mode(&mi.bmi, above_pair, 1);
            let l1 = left_block_mode(&mi.bmi, left_pair, 1);
            let m1 = r.read_tree(&INTRA_MODE_TREE, &kf_y_mode_probs(a1, l1));
            mi.bmi[0] = m0;
            mi.bmi[2] = m0;
            mi.bmi[1] = m1;
            mi.bmi[3] = m1;
            mi.mode = m1;
        }
        2 => {
            // BLOCK_8X4
            let a0 = above_block_mode(&mi.bmi, above_pair, 0);
            let l0 = left_block_mode(&mi.bmi, left_pair, 0);
            let m0 = r.read_tree(&INTRA_MODE_TREE, &kf_y_mode_probs(a0, l0));
            let a2 = above_block_mode(&mi.bmi, above_pair, 2);
            let l2 = left_block_mode(&mi.bmi, left_pair, 2);
            let m2 = r.read_tree(&INTRA_MODE_TREE, &kf_y_mode_probs(a2, l2));
            mi.bmi[0] = m0;
            mi.bmi[1] = m0;
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
    Ok(mi)
}

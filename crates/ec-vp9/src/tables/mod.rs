//! Handwritten trees and constants around the generated bulk tables,
//! plus the block/transform-size enums shared by every module.

mod gen_tables;

pub(crate) use gen_tables::*;

use crate::Result;
use ec_core::Error;

pub(crate) const DIFF_UPDATE_PROB: u8 = 252;
pub(crate) const ALLOW_32X32: u8 = 3;

// spec 5.3 (PREDICTION_MODE)
pub(crate) const DC_PRED: u8 = 0;
pub(crate) const TM_PRED: u8 = 9;
pub(crate) const INTRA_MODES: usize = 10;

// spec 5.2 (TX_SIZE)
pub(crate) const TX_4X4: usize = 0;
pub(crate) const TX_8X8: usize = 1;
pub(crate) const TX_16X16: usize = 2;
pub(crate) const TX_32X32: usize = 3;

// spec 5.4 (TX_TYPE)
pub(crate) const DCT_DCT: usize = 0;
pub(crate) const ADST_DCT: usize = 1;
pub(crate) const DCT_ADST: usize = 2;
pub(crate) const ADST_ADST: usize = 3;

pub(crate) const BLOCK_INVALID: u8 = 255;

// spec 5.3 (TX_MODE)
pub(crate) const TX_MODE_SELECT: u8 = 4;
/// decodeframe.c `tx_mode_to_biggest_tx_size`.
pub(crate) const TX_MODE_TO_BIGGEST_TX_SIZE: [usize; 5] =
    [TX_4X4, TX_8X8, TX_16X16, TX_32X32, TX_32X32];

/// spec 6.3.5 `partition_tree`.
pub(crate) const PARTITION_TREE: [i8; 6] = [-0, 2, -1, 4, -2, -3];
/// spec 6.3.4 y mode tree (8.2 `read_intra_mode`).
pub(crate) const INTRA_MODE_TREE: [i8; 18] = [
    -0, 2, -9, 4, -1, 6, 8, 12, -2, 10, -5, -6, -3, 14, -7, 16, -8, -4,
];
/// spec 6.3.4 uv mode tree.
pub(crate) const UV_MODE_TREE: [i8; 18] = [
    -0, 2, -1, 4, -2, 6, -3, 8, -4, 10, -5, 12, -6, 14, -7, 16, -8, -9,
];
/// spec 6.2 segment id tree.
pub(crate) const SEGMENT_TREE: [i8; 14] =
    [-0, 2, -1, 4, -2, 6, -3, 8, -4, 10, -5, 12, -6, -7];

/// blockd.h `get_y_mode`.
#[inline]
pub(crate) fn get_y_mode(sb_type: usize, bmi: [u8; 4], mode: u8, block: usize) -> u8 {
    if sb_type < 3 { bmi[block] } else { mode }
}

/// vp9_blockd.c `vp9_above_block_mode` (keyframes: neighbours are intra).
pub(crate) fn above_block_mode(
    cur_bmi: &[u8; 4],
    above: Option<(usize, [u8; 4], u8)>,
    b: usize,
) -> u8 {
    if b == 0 || b == 1 {
        match above {
            Some((sb_type, bmi, mode)) => get_y_mode(sb_type, bmi, mode, b + 2),
            None => DC_PRED,
        }
    } else {
        cur_bmi[b - 2]
    }
}

/// vp9_blockd.c `vp9_left_block_mode`.
pub(crate) fn left_block_mode(
    cur_bmi: &[u8; 4],
    left: Option<(usize, [u8; 4], u8)>,
    b: usize,
) -> u8 {
    if b == 0 || b == 2 {
        match left {
            Some((sb_type, bmi, mode)) => get_y_mode(sb_type, bmi, mode, b + 1),
            None => DC_PRED,
        }
    } else {
        cur_bmi[b - 1]
    }
}

/// blockd.h `get_y_mode_probs` reduced to the keyframe table.
#[inline]
pub(crate) fn kf_y_mode_probs(above: u8, left: u8) -> [u8; 9] {
    let base = above as usize * INTRA_MODES + left as usize;
    let mut out = [0u8; 9];
    out.copy_from_slice(&KF_Y_MODE_PROB[base * 9..base * 9 + 9]);
    out
}

/// `vp9_kf_uv_mode_prob` reshaped per y mode (9 branch probs).
pub(crate) fn kf_uv_probs(mode: u8) -> [u8; 9] {
    let base = mode as usize * 9;
    let mut out = [0u8; 9];
    out.copy_from_slice(&KF_UV_MODE_PROB[base..base + 9]);
    out
}

/// common_data.c `uv_txsize_lookup` for 4:2:0.
pub(crate) fn uv_txsize_lookup(bsize: usize, tx_size: usize) -> usize {
    let uv = SS_SIZE_LOOKUP[bsize * 4 + 3];
    if uv == BLOCK_INVALID {
        TX_4X4
    } else {
        tx_size.min(MAX_TXSIZE_LOOKUP[uv as usize] as usize)
    }
}

/// common_data.c `ss_size_lookup[bsize][1][1]`.
#[inline]
pub(crate) fn ss_size(bsize: usize) -> usize {
    SS_SIZE_LOOKUP[bsize * 4 + 3] as usize
}

/// blockd.h `get_uv_tx_size` for 4:2:0.
#[inline]
pub(crate) fn get_uv_tx_size(mi_tx_size: usize, sb_type: usize) -> usize {
    if sb_type < 3 {
        TX_4X4
    } else {
        mi_tx_size.min(MAX_TXSIZE_LOOKUP[ss_size(sb_type)] as usize)
    }
}

pub(crate) fn corrupt(what: impl Into<String>) -> Error {
    Error::corrupt(format!("vp9: {}", what.into()))
}

pub(crate) fn ensure(cond: bool, what: &str) -> Result<()> {
    if cond { Ok(()) } else { Err(corrupt(what)) }
}

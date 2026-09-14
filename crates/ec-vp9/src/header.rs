//! The frame context (bool-coded probabilities) and the compressed
//! header (spec 6.2 `compressed_header` / 8.2): thin wrap over
//! [`ec_vp9_syntax::Vp9Parser`] for the uncompressed part, plus the
//! differential probability-update machinery (libvpx `vp9_dsubexp.c`).

use crate::bool::BoolDecoder;
use crate::tables::*;

/// All bool-coded probabilities a keyframe reads.
#[derive(Clone)]
pub(crate) struct FrameContext {
    pub coef: [[[[[[u8; 3]; 6]; 6]; 2]; 2]; 4],
    pub partition: [[u8; 3]; 16],
    pub tx_p8x8: [[u8; 1]; 2],
    pub tx_p16x16: [[u8; 2]; 2],
    pub tx_p32x32: [[u8; 3]; 2],
    pub skip: [u8; 3],
}

impl FrameContext {
    /// Spec 7.2 `setup_past_independence`: back to the defaults. Key
    /// frames code partitions with `vp9_kf_partition_probs`, every other
    /// table is type-independent.
    pub(crate) fn new(key_frame: bool) -> Self {
        let partition_src: &[u8] = if key_frame {
            &KF_PARTITION_PROBS
        } else {
            &DEFAULT_PARTITION_PROBS
        };
        FrameContext {
            coef: [
                DEFAULT_COEF_PROBS_4X4,
                DEFAULT_COEF_PROBS_8X8,
                DEFAULT_COEF_PROBS_16X16,
                DEFAULT_COEF_PROBS_32X32,
            ],
            partition: {
                let mut p = [[0u8; 3]; 16];
                for (i, row) in p.iter_mut().enumerate() {
                    row.copy_from_slice(&partition_src[i * 3..i * 3 + 3]);
                }
                p
            },
            tx_p8x8: [[TX_P8X8_PROB[0]], [TX_P8X8_PROB[1]]],
            tx_p16x16: [
                [TX_P16X16_PROB[0], TX_P16X16_PROB[1]],
                [TX_P16X16_PROB[2], TX_P16X16_PROB[3]],
            ],
            tx_p32x32: [
                [TX_P32X32_PROB[0], TX_P32X32_PROB[1], TX_P32X32_PROB[2]],
                [TX_P32X32_PROB[3], TX_P32X32_PROB[4], TX_P32X32_PROB[5]],
            ],
            skip: DEFAULT_SKIP_PROBS,
        }
    }
}

/// `inv_recenter_nonneg` (vp9_dsubexp.c).
fn inv_recenter_nonneg(v: i32, m: i32) -> i32 {
    if v > 2 * m {
        return v;
    }
    if v & 1 != 0 {
        m - ((v + 1) >> 1)
    } else {
        m + (v >> 1)
    }
}

/// `decode_uniform` (vp9_dsubexp.c): l = 8.
fn decode_uniform(r: &mut BoolDecoder) -> i32 {
    let l = 8;
    let m = (1 << l) - 191;
    let v = r.read_literal(7) as i32;
    if v < m {
        v
    } else {
        (v << 1) - m + r.read_bool(128) as i32
    }
}

/// `decode_term_subexp` (vp9_dsubexp.c).
fn decode_term_subexp(r: &mut BoolDecoder) -> i32 {
    if !r.read_bool(128) {
        return r.read_literal(4) as i32;
    }
    if !r.read_bool(128) {
        return r.read_literal(4) as i32 + 16;
    }
    if !r.read_bool(128) {
        return r.read_literal(5) as i32 + 32;
    }
    decode_uniform(r) + 64
}

/// `inv_remap_prob` with libvpx's 255-entry map table.
fn inv_remap_prob(v: i32, m: u8) -> u8 {
    const MAP: [u8; 255] = [
        7, 20, 33, 46, 59, 72, 85, 98, 111, 124, 137, 150, 163, 176, 189, 202, 215, 228, 241, 254,
        1, 2, 3, 4, 5, 6, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 21, 22, 23, 24, 25, 26, 27,
        28, 29, 30, 31, 32, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 47, 48, 49, 50, 51, 52,
        53, 54, 55, 56, 57, 58, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 73, 74, 75, 76, 77,
        78, 79, 80, 81, 82, 83, 84, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 99, 100, 101,
        102, 103, 104, 105, 106, 107, 108, 109, 110, 112, 113, 114, 115, 116, 117, 118, 119, 120,
        121, 122, 123, 125, 126, 127, 128, 129, 130, 131, 132, 133, 134, 135, 136, 138, 139, 140,
        141, 142, 143, 144, 145, 146, 147, 148, 149, 151, 152, 153, 154, 155, 156, 157, 158, 159,
        160, 161, 162, 164, 165, 166, 167, 168, 169, 170, 171, 172, 173, 174, 175, 177, 178, 179,
        180, 181, 182, 183, 184, 185, 186, 187, 188, 190, 191, 192, 193, 194, 195, 196, 197, 198,
        199, 200, 201, 203, 204, 205, 206, 207, 208, 209, 210, 211, 212, 213, 214, 216, 217, 218,
        219, 220, 221, 222, 223, 224, 225, 226, 227, 229, 230, 231, 232, 233, 234, 235, 236, 237,
        238, 239, 240, 242, 243, 244, 245, 246, 247, 248, 249, 250, 251, 252, 253, 253,
    ];
    let v = MAP[v as usize] as i32;
    let m = m as i32 - 1;
    if (m << 1) <= 255 {
        (1 + inv_recenter_nonneg(v, m)) as u8
    } else {
        (255 - inv_recenter_nonneg(v, 255 - 1 - m)) as u8
    }
}

/// `vp9_diff_update_prob`.
fn diff_update_prob(r: &mut BoolDecoder, p: &mut u8) {
    if r.read_bool(DIFF_UPDATE_PROB) {
        let delp = decode_term_subexp(r);
        *p = inv_remap_prob(delp, *p);
    }
}

/// `read_coef_probs_common` (decodeframe.c:1314): one flag per tx size
/// covers all plane/ref/band/context updates of the first three (base)
/// probs per slot.
fn read_coef_probs(r: &mut BoolDecoder, ctx: &mut FrameContext, tx_mode: u8) {
    let max_tx = TX_MODE_TO_BIGGEST_TX_SIZE[tx_mode as usize];
    for tx in 0..=max_tx {
        if r.read_bool(128) {
            for plane in 0..2 {
                for _ref in 0..2 {
                    for band in 0..6 {
                        let n_ctx = if band == 0 { 3 } else { 6 };
                        for ci in 0..n_ctx {
                            for m in 0..3 {
                                diff_update_prob(r, &mut ctx.coef[tx][plane][_ref][band][ci][m]);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// `read_tx_mode` (decodeframe.c:58).
fn read_tx_mode(r: &mut BoolDecoder) -> u8 {
    let mut m = r.read_literal(2) as u8;
    if m == ALLOW_32X32 {
        m += r.read_bool(128) as u8;
    }
    m
}

/// `read_compressed_header` for intra frames (the inter branch is dead
/// in this lane): tx mode, tx probs, coef probs, skip probs.
///
/// Lossless forces `ONLY_4X4` and consumes no tx-mode bits — decodeframe.c
/// `read_compressed_header`: `cm->tx_mode = xd->lossless ? ONLY_4X4 :
/// read_tx_mode(&r);`.
pub(crate) fn read_compressed_header(
    data: &[u8],
    ctx: &mut FrameContext,
    lossless: bool,
) -> crate::Result<u8> {
    let mut r = BoolDecoder::new(data)?;
    let tx_mode = if lossless {
        ONLY_4X4
    } else {
        read_tx_mode(&mut r)
    };
    if tx_mode == TX_MODE_SELECT {
        for row in 0..2 {
            for j in 0..1 {
                diff_update_prob(&mut r, &mut ctx.tx_p8x8[row][j]);
            }
        }
        for row in 0..2 {
            for j in 0..2 {
                diff_update_prob(&mut r, &mut ctx.tx_p16x16[row][j]);
            }
        }
        for row in 0..2 {
            for j in 0..3 {
                diff_update_prob(&mut r, &mut ctx.tx_p32x32[row][j]);
            }
        }
    }
    read_coef_probs(&mut r, ctx, tx_mode);
    for p in &mut ctx.skip {
        diff_update_prob(&mut r, p);
    }
    if std::env::var_os("EC_VP9_DBG").is_some() {
        eprintln!("DBG tx_mode={} consumed={}", tx_mode, r.byte_offset());
        eprintln!(
            "DBG skip_probs={:?} p8x8={:?} p16={:?}",
            ctx.skip, ctx.tx_p8x8, ctx.tx_p16x16
        );
        eprintln!(
            "DBG coef[0][0][0][0][0..3]={:?} coef[0][0][1][1][0..3]={:?}",
            ctx.coef[0][0][0][0], ctx.coef[0][0][1][1]
        );
    }
    ensure(r.overreads() == 0, "compressed header desync")?;
    Ok(tx_mode)
}

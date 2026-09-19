//! The frame context (bool-coded probabilities) and the compressed
//! header (spec 6.2 `compressed_header` / 8.2): thin wrap over
//! [`ec_vp9_syntax::Vp9Parser`] for the uncompressed part, plus the
//! differential probability-update machinery (libvpx `vp9_dsubexp.c`).

use crate::bool::BoolDecoder;
use crate::inter::{COMPOUND_REFERENCE, REFERENCE_MODE_SELECT, SINGLE_REFERENCE};
use crate::tables::*;
use ec_vp9_syntax::FrameHeader;

/// All bool-coded probabilities a keyframe reads.
#[derive(Clone)]
pub(crate) struct FrameContext {
    pub coef: [[[[[[u8; 3]; 6]; 6]; 2]; 2]; 4],
    pub partition: [[u8; 3]; 16],
    pub tx_p8x8: [[u8; 1]; 2],
    pub tx_p16x16: [[u8; 2]; 2],
    pub tx_p32x32: [[u8; 3]; 2],
    pub skip: [u8; 3],
    /// Inter-frame tables (all defaults until a compressed header updates
    /// them): `inter_mode_probs`, `switchable_interp_prob`, `intra_inter_prob`,
    /// `comp_inter_prob`, `single_ref_prob`, `comp_ref_prob`, `y_mode_prob`,
    /// `uv_mode_prob`, and the MV probs.
    pub inter_mode: [[u8; 3]; 7],
    pub switchable_interp: [[u8; 2]; 4],
    pub intra_inter: [u8; 4],
    pub comp_inter: [u8; 5],
    pub single_ref: [[u8; 2]; 5],
    pub comp_ref: [u8; 5],
    pub y_mode: [[u8; 9]; 4],
    pub uv_mode: [[u8; 9]; 10],
    pub nmv_joints: [u8; 3],
    pub nmv_sign: [u8; 2],
    pub nmv_classes: [[u8; 10]; 2],
    pub nmv_class0: [[u8; 1]; 2],
    pub nmv_bits: [[u8; 10]; 2],
    pub nmv_class0_fp: [[[u8; 3]; 2]; 2],
    pub nmv_fp: [[u8; 3]; 2],
    pub nmv_class0_hp: [u8; 2],
    pub nmv_hp: [u8; 2],
}

/// Rows from a flat C initializer.
fn rows<const R: usize, const C: usize>(flat: &[u8]) -> [[u8; C]; R] {
    let mut out = [[0u8; C]; R];
    for (i, row) in out.iter_mut().enumerate() {
        row.copy_from_slice(&flat[i * C..i * C + C]);
    }
    out
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
            // `vp9_init_frame_context` / `vp9_init_mv_probs` defaults. Key
            // frames do not use these; inter frames start from them unless a
            // stored context (or a compressed header) says otherwise.
            inter_mode: rows(&DEFAULT_INTER_MODE_PROBS),
            switchable_interp: rows(&DEFAULT_SWITCHABLE_INTERP_PROBS),
            intra_inter: DEFAULT_INTRA_INTER_PROBS,
            comp_inter: DEFAULT_COMP_INTER_PROBS,
            single_ref: rows(&DEFAULT_SINGLE_REF_PROBS),
            comp_ref: DEFAULT_COMP_REF_PROBS,
            y_mode: rows(&DEFAULT_Y_MODE_PROBS),
            uv_mode: rows(&DEFAULT_IF_UV_PROBS),
            nmv_joints: DEFAULT_NMVC_JOINTS,
            nmv_sign: DEFAULT_NMVC_SIGN,
            nmv_classes: rows(&DEFAULT_NMVC_CLASSES),
            nmv_class0: [[DEFAULT_NMVC_CLASS0[0]], [DEFAULT_NMVC_CLASS0[1]]],
            nmv_bits: rows(&DEFAULT_NMVC_BITS),
            nmv_class0_fp: [
                [
                    [DEFAULT_NMVC_CLASS0_FP[0], DEFAULT_NMVC_CLASS0_FP[1], DEFAULT_NMVC_CLASS0_FP[2]],
                    [DEFAULT_NMVC_CLASS0_FP[3], DEFAULT_NMVC_CLASS0_FP[4], DEFAULT_NMVC_CLASS0_FP[5]],
                ],
                [
                    [DEFAULT_NMVC_CLASS0_FP[6], DEFAULT_NMVC_CLASS0_FP[7], DEFAULT_NMVC_CLASS0_FP[8]],
                    [DEFAULT_NMVC_CLASS0_FP[9], DEFAULT_NMVC_CLASS0_FP[10], DEFAULT_NMVC_CLASS0_FP[11]],
                ],
            ],
            nmv_fp: rows(&DEFAULT_NMVC_FP),
            nmv_class0_hp: DEFAULT_NMVC_CLASS0_HP,
            nmv_hp: DEFAULT_NMVC_HP,
        }
    }
}

impl FrameContext {
    /// `vp9_kf_partition_probs` are NOT a `FRAME_CONTEXT` field in libvpx
    /// (`get_partition_probs` picks the const table for intra-only frames);
    /// a context that a keyframe stored must therefore go back to the inter
    /// defaults before the next frame reads it.
    pub(crate) fn reset_inter_partition(&mut self) {
        self.partition = rows(&DEFAULT_PARTITION_PROBS);
    }

    /// `set_partition_probs` (`vp9/onyxc_int.h:367`): a key frame OR an
    /// intra-only frame reads partitions from the const
    /// `vp9_kf_partition_probs`, never from the stored `FRAME_CONTEXT`. An
    /// intra-only frame's context otherwise comes from storage, so its own
    /// `partition` field must be left alone (this only swaps the field on the
    /// active read copy).
    pub(crate) fn use_key_partition(&mut self) {
        for (i, row) in self.partition.iter_mut().enumerate() {
            row.copy_from_slice(&KF_PARTITION_PROBS[i * 3..i * 3 + 3]);
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

/// `MV_UPDATE_PROB` (`vp9_entropymv.h`): the flag prob of `update_mv_probs`.
const MV_UPDATE_PROB: u8 = 252;

/// `update_mv_probs` (vp9_decodeframe.c:136). NOT a `vp9_diff_update_prob`:
/// a flag at `MV_UPDATE_PROB`, then seven LITERAL bits, and the new prob is
/// `(literal << 1) | 1` — no `decode_term_subexp`, no `inv_remap_prob`.
fn update_mv_probs(r: &mut BoolDecoder, p: &mut [u8]) {
    for v in p.iter_mut() {
        if r.read_bool(MV_UPDATE_PROB) {
            *v = ((r.read_literal(7) << 1) | 1) as u8;
        }
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

/// What `read_compressed_header` decides for the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CompressedHeader {
    pub tx_mode: u8,
    pub reference_mode: u8,
}

/// `read_compressed_header` (libvpx `vp9_decodeframe.c:2889`): tx mode, tx
/// probs, coef probs, skip probs, then — for inter frames only — the inter
/// mode, switchable-interp, intra/inter, reference-mode, y-mode, partition
/// and MV probability updates.
///
/// Lossless forces `ONLY_4X4` and consumes no tx-mode bits — decodeframe.c
/// `read_compressed_header`: `cm->tx_mode = xd->lossless ? ONLY_4X4 :
/// read_tx_mode(&r);`.
pub(crate) fn read_compressed_header(
    data: &[u8],
    ctx: &mut FrameContext,
    hdr: &FrameHeader,
) -> crate::Result<CompressedHeader> {
    if crate::interdump_enabled() && !hdr.frame_is_intra {
        let mut line = String::from("PPFLAT_PRE");
        for row in ctx.partition.iter() {
            for v in row.iter() {
                line.push_str(&format!(" {v}"));
            }
        }
        eprintln!("{line}");
    }
    let lossless = hdr.quantization.lossless();
    let mut r = BoolDecoder::new(data)?;
    let tx_mode = if lossless {
        ONLY_4X4
    } else {
        read_tx_mode(&mut r)
    };
    if crate::trace_enabled() {
        eprintln!("SECT txmode");
    }
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
    if crate::trace_enabled() {
        eprintln!("SECT txprobs");
    }
    read_coef_probs(&mut r, ctx, tx_mode);
    if crate::trace_enabled() {
        eprintln!("SECT coef");
    }
    for p in &mut ctx.skip {
        diff_update_prob(&mut r, p);
    }
    let mut reference_mode = SINGLE_REFERENCE;
    if crate::trace_enabled() {
        eprintln!("SECT skip");
    }
    if !hdr.frame_is_intra {
    if crate::trace_enabled() {
        eprintln!("SECT intermode");
    }
        for i in 0..7 {
            for j in 0..3 {
                diff_update_prob(&mut r, &mut ctx.inter_mode[i][j]);
            }
        }
    if crate::trace_enabled() {
        eprintln!("SECT switchable");
    }
        if hdr.interpolation_filter as u8 == crate::inter::SWITCHABLE {
            for j in 0..4 {
                for i in 0..2 {
                    diff_update_prob(&mut r, &mut ctx.switchable_interp[j][i]);
                }
            }
        }
    if crate::trace_enabled() {
        eprintln!("SECT intrainter");
    }
        for p in &mut ctx.intra_inter {
            diff_update_prob(&mut r, p);
        }
        // read_frame_reference_mode: `vp9_compound_reference_allowed` gates
        // the two bits (bias[GOLDEN]/bias[ALTREF] vs bias[LAST]).
    if crate::trace_enabled() {
        eprintln!("SECT refmode");
    }
        let allowed = hdr.ref_frame_sign_bias[1] != hdr.ref_frame_sign_bias[0]
            || hdr.ref_frame_sign_bias[2] != hdr.ref_frame_sign_bias[0];
        reference_mode = if allowed {
            if r.read_bool(128) {
                if r.read_bool(128) {
                    REFERENCE_MODE_SELECT
                } else {
                    COMPOUND_REFERENCE
                }
            } else {
                SINGLE_REFERENCE
            }
        } else {
            SINGLE_REFERENCE
        };
        // read_frame_reference_mode_probs.
    if crate::trace_enabled() {
        eprintln!("SECT refprobs");
    }
        if reference_mode == REFERENCE_MODE_SELECT {
            for p in &mut ctx.comp_inter {
                diff_update_prob(&mut r, p);
            }
        }
        if reference_mode != COMPOUND_REFERENCE {
            for i in 0..5 {
                for j in 0..2 {
                    diff_update_prob(&mut r, &mut ctx.single_ref[i][j]);
                }
            }
        }
        if reference_mode != SINGLE_REFERENCE {
            for p in &mut ctx.comp_ref {
                diff_update_prob(&mut r, p);
            }
        }
    if crate::trace_enabled() {
        eprintln!("SECT ymode");
    }
        for j in 0..4 {
            for i in 0..9 {
                diff_update_prob(&mut r, &mut ctx.y_mode[j][i]);
            }
        }
    if crate::trace_enabled() {
        eprintln!("SECT partition");
    }
        for j in 0..16 {
            for i in 0..3 {
                let before = ctx.partition[j][i];
                diff_update_prob(&mut r, &mut ctx.partition[j][i]);
                if crate::interdump_enabled() {
                    eprintln!("PUPD {} {} {} {}", j, i, before, ctx.partition[j][i]);
                }
            }
        }
        // read_mv_probs (vp9_decodeframe.c:76): joints, then per component
        // sign/classes/class0/bits, then the fractional parts, then hp.
    if crate::trace_enabled() {
        eprintln!("SECT mv");
    }
        update_mv_probs(&mut r, &mut ctx.nmv_joints);
        for comp in 0..2 {
            update_mv_probs(&mut r, &mut ctx.nmv_sign[comp..comp + 1]);
            update_mv_probs(&mut r, &mut ctx.nmv_classes[comp]);
            update_mv_probs(&mut r, &mut ctx.nmv_class0[comp]);
            update_mv_probs(&mut r, &mut ctx.nmv_bits[comp]);
        }
        for comp in 0..2 {
            for j in 0..2 {
                update_mv_probs(&mut r, &mut ctx.nmv_class0_fp[comp][j]);
            }
            update_mv_probs(&mut r, &mut ctx.nmv_fp[comp]);
        }
        if hdr.allow_high_precision_mv {
            for comp in 0..2 {
                update_mv_probs(&mut r, &mut ctx.nmv_class0_hp[comp..comp + 1]);
                update_mv_probs(&mut r, &mut ctx.nmv_hp[comp..comp + 1]);
    if crate::mvdump_enabled() {
        eprintln!(
            "MVPROB j0={} j1={} j2={}",
            ctx.nmv_joints[0], ctx.nmv_joints[1], ctx.nmv_joints[2]
        );
        for comp in 0..2 {
            eprintln!(
                "MVPROB c{} sign={} class0={} class0hp={} hp={}",
                comp,
                ctx.nmv_sign[comp],
                ctx.nmv_class0[comp][0],
                ctx.nmv_class0_hp[comp],
                ctx.nmv_hp[comp]
            );
            eprint!("MVPROB c{} cls", comp);
            for v in ctx.nmv_classes[comp].iter() { eprint!(" {}", v); }
            eprintln!();
            eprint!("MVPROB c{} bits", comp);
            for v in ctx.nmv_bits[comp].iter() { eprint!(" {}", v); }
            eprintln!();
            eprint!("MVPROB c{} c0fp", comp);
            for row in ctx.nmv_class0_fp[comp].iter() { for v in row.iter() { eprint!(" {}", v); } }
            eprintln!();
            eprint!("MVPROB c{} fp", comp);
            for v in ctx.nmv_fp[comp].iter() { eprint!(" {}", v); }
            eprintln!();
        }
    }
            }
        }
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
    Ok(CompressedHeader {
        tx_mode,
        reference_mode,
    })
}

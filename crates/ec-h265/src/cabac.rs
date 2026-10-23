//! The CABAC arithmetic coder and its context variables (clause 9.3).
//!
//! Two things live here that are usually separate: the encoder that writes bins,
//! and the same encoder in *counting* mode, which runs the identical context
//! updates but throws the bytes away and keeps the bit count. Rate-distortion
//! decisions are made against that count, so the rate a decision is judged by is
//! the rate it actually costs — no bit-estimate table to drift out of step with
//! the coder.
//!
//! A third mode, *estimating*, exists purely for RDOQ's own trial-costing
//! instrument (`rdoq_enc`): it skips the range/renorm arithmetic and instead
//! looks up a per-context fractional-bit cost from [`ENTROPY_BITS`], the
//! x265/HM `entropyBits`-table idiom. The committing encoder never uses it.

use std::sync::LazyLock;

/// Number of context variables this encoder keeps.
pub const NUM_CONTEXTS: usize = ctx::TOTAL;

/// Context index layout: one named base per syntax element. Intra elements
/// come first (their indices predate the inter tables), inter elements follow.
pub mod ctx {
    /// `split_cu_flag`, 3 contexts.
    pub const SPLIT_CU: usize = 0;
    /// `part_mode`, 4 contexts (intra uses the first; inter uses all four).
    pub const PART_MODE: usize = SPLIT_CU + 3;
    /// `prev_intra_luma_pred_flag`.
    pub const PREV_INTRA_LUMA_PRED: usize = PART_MODE + 4;
    /// `intra_chroma_pred_mode`.
    pub const INTRA_CHROMA_PRED_MODE: usize = PREV_INTRA_LUMA_PRED + 1;
    /// `split_transform_flag`, 3 contexts.
    pub const SPLIT_TRANSFORM: usize = INTRA_CHROMA_PRED_MODE + 1;
    /// `cbf_luma`, 2 contexts.
    pub const CBF_LUMA: usize = SPLIT_TRANSFORM + 3;
    /// `cbf_cb` / `cbf_cr`, 5 contexts (indexed by trafoDepth).
    pub const CBF_CHROMA: usize = CBF_LUMA + 2;
    /// `transform_skip_flag`, 2 contexts (luma, chroma).
    pub const TRANSFORM_SKIP: usize = CBF_CHROMA + 5;
    /// `last_sig_coeff_x_prefix`, 18 contexts.
    pub const LAST_X: usize = TRANSFORM_SKIP + 2;
    /// `last_sig_coeff_y_prefix`, 18 contexts.
    pub const LAST_Y: usize = LAST_X + 18;
    /// `coded_sub_block_flag`, 4 contexts.
    pub const CODED_SUB_BLOCK: usize = LAST_Y + 18;
    /// `sig_coeff_flag`, 44 contexts (42 transform + 2 transform-skip).
    pub const SIG_COEFF: usize = CODED_SUB_BLOCK + 4;
    /// `coeff_abs_level_greater1_flag`, 24 contexts.
    pub const GREATER1: usize = SIG_COEFF + 44;
    /// `coeff_abs_level_greater2_flag`, 6 contexts.
    pub const GREATER2: usize = GREATER1 + 24;
    /// `cu_skip_flag`, 3 contexts.
    pub const CU_SKIP: usize = GREATER2 + 6;
    /// `pred_mode_flag`, 1 context.
    pub const PRED_MODE: usize = CU_SKIP + 3;
    /// `merge_flag`, 1 context.
    pub const MERGE_FLAG: usize = PRED_MODE + 1;
    /// `merge_idx`, 1 context (the first bin; the rest are bypass).
    pub const MERGE_IDX: usize = MERGE_FLAG + 1;
    /// `inter_pred_idc`, 5 contexts (0..3 by CtDepth, 4 for the 8x4/4x8 case).
    pub const INTER_PRED_IDC: usize = MERGE_IDX + 1;
    /// `ref_idx_lX`, 2 contexts (the first two bins; the rest are bypass).
    pub const REF_IDX: usize = INTER_PRED_IDC + 5;
    /// `mvp_lX_flag`, 1 context.
    pub const MVP_FLAG: usize = REF_IDX + 2;
    /// `abs_mvd_greater0_flag`, 1 context.
    pub const ABS_MVD_GT0: usize = MVP_FLAG + 1;
    /// `abs_mvd_greater1_flag`, 1 context.
    pub const ABS_MVD_GT1: usize = ABS_MVD_GT0 + 1;
    /// `rqt_root_cbf`, 1 context.
    pub const RQT_ROOT_CBF: usize = ABS_MVD_GT1 + 1;
    /// `cu_qp_delta_abs`, 2 contexts (first bin; bins 1..4).
    pub const CU_QP_DELTA_ABS: usize = RQT_ROOT_CBF + 1;
    /// `sao_merge_left_flag` / `sao_merge_up_flag`, 1 context.
    pub const SAO_MERGE: usize = CU_QP_DELTA_ABS + 2;
    /// `sao_type_idx_luma` / `sao_type_idx_chroma`, 1 context.
    pub const SAO_TYPE_IDX: usize = SAO_MERGE + 1;
    /// Total context count.
    /// `cu_transquant_bypass_flag`, 1 context.
    pub const CU_TRANSQUANT_BYPASS: usize = SAO_TYPE_IDX + 1;
    /// `cu_chroma_qp_offset_flag`, 1 context.
    pub const CU_CHROMA_QP_OFFSET_FLAG: usize = CU_TRANSQUANT_BYPASS + 1;
    /// `cu_chroma_qp_offset_idx`, 1 context (every bin).
    pub const CU_CHROMA_QP_OFFSET_IDX: usize = CU_CHROMA_QP_OFFSET_FLAG + 1;
    /// `explicit_rdpcm_flag`, 2 contexts (luma, chroma).
    pub const EXPLICIT_RDPCM_FLAG: usize = CU_CHROMA_QP_OFFSET_IDX + 1;
    /// `explicit_rdpcm_dir_flag`, 2 contexts (luma, chroma).
    pub const EXPLICIT_RDPCM_DIR: usize = EXPLICIT_RDPCM_FLAG + 2;
    /// `log2_res_scale_abs_plus1`, 8 contexts (`4 * c + binIdx`).
    pub const LOG2_RES_SCALE_ABS: usize = EXPLICIT_RDPCM_DIR + 2;
    /// `res_scale_sign_flag`, 2 contexts (one per chroma component).
    pub const RES_SCALE_SIGN: usize = LOG2_RES_SCALE_ABS + 8;
    /// Number of context slots.
    pub const TOTAL: usize = RES_SCALE_SIGN + 2;
}

/// The `initValue`s of one syntax element for each initType (Tables 9-5 to 9-37).
/// Rows are in [`ctx`] layout order; `t0` is I slices, `t1` and `t2` are P and B
/// slices (initType 1 and 2, swapped by `cabac_init_flag`). Elements that do not
/// occur in I slices carry the 154 filler in `t0`, which no I-slice bin reads.
struct InitRow {
    t0: &'static [u8],
    t1: &'static [u8],
    t2: &'static [u8],
}

#[rustfmt::skip]
const INIT_ROWS: &[InitRow] = &[
    // split_cu_flag (9-7)
    InitRow { t0: &[139, 141, 157], t1: &[107, 139, 126], t2: &[107, 139, 126] },
    // part_mode (9-11): initType 0 has one context
    InitRow { t0: &[184, 154, 154, 154], t1: &[154, 139, 154, 154], t2: &[154, 139, 154, 154] },
    // prev_intra_luma_pred_flag (9-12)
    InitRow { t0: &[184], t1: &[154], t2: &[183] },
    // intra_chroma_pred_mode (9-13)
    InitRow { t0: &[63], t1: &[152], t2: &[152] },
    // split_transform_flag (9-20)
    InitRow { t0: &[153, 138, 138], t1: &[124, 138, 94], t2: &[224, 167, 122] },
    // cbf_luma (9-21)
    InitRow { t0: &[111, 141], t1: &[153, 111], t2: &[153, 111] },
    // cbf_cb / cbf_cr (9-22); the fifth context is 154 in every initType
    InitRow { t0: &[94, 138, 182, 154, 154], t1: &[149, 107, 167, 154, 154], t2: &[149, 92, 167, 154, 154] },
    // transform_skip_flag (9-25)
    InitRow { t0: &[139, 139], t1: &[139, 139], t2: &[139, 139] },
    // last_sig_coeff_x_prefix (9-26)
    InitRow {
        t0: &[110, 110, 124, 125, 140, 153, 125, 127, 140, 109, 111, 143, 127, 111, 79, 108, 123, 63],
        t1: &[125, 110, 94, 110, 95, 79, 125, 111, 110, 78, 110, 111, 111, 95, 94, 108, 123, 108],
        t2: &[125, 110, 124, 110, 95, 94, 125, 111, 111, 79, 125, 126, 111, 111, 79, 108, 123, 93],
    },
    // last_sig_coeff_y_prefix (9-27)
    InitRow {
        t0: &[110, 110, 124, 125, 140, 153, 125, 127, 140, 109, 111, 143, 127, 111, 79, 108, 123, 63],
        t1: &[125, 110, 94, 110, 95, 79, 125, 111, 110, 78, 110, 111, 111, 95, 94, 108, 123, 108],
        t2: &[125, 110, 124, 110, 95, 94, 125, 111, 111, 79, 125, 126, 111, 111, 79, 108, 123, 93],
    },
    // coded_sub_block_flag (9-28)
    InitRow { t0: &[91, 171, 134, 141], t1: &[121, 140, 61, 154], t2: &[121, 140, 61, 154] },
    // sig_coeff_flag (9-29): 42 transform contexts, then the two transform-skip ones
    InitRow {
        t0: &[
            111, 111, 125, 110, 110, 94, 124, 108, 124, 107, 125, 141, 179, 153, 125, 107,
            125, 141, 179, 153, 125, 107, 125, 141, 179, 153, 125, 140, 139, 182, 182, 152,
            136, 152, 136, 153, 136, 139, 111, 136, 139, 111, 141, 111,
        ],
        t1: &[
            155, 154, 139, 153, 139, 123, 123, 63, 153, 166, 183, 140, 136, 153, 154, 166,
            183, 140, 136, 153, 154, 166, 183, 140, 136, 153, 154, 170, 153, 123, 123, 107,
            121, 107, 121, 167, 151, 183, 140, 151, 183, 140, 140, 140,
        ],
        t2: &[
            170, 154, 139, 153, 139, 123, 123, 63, 124, 166, 183, 140, 136, 153, 154, 166,
            183, 140, 136, 153, 154, 166, 183, 140, 136, 153, 154, 170, 153, 138, 138, 122,
            121, 122, 121, 167, 151, 183, 140, 151, 183, 140, 140, 140,
        ],
    },
    // coeff_abs_level_greater1_flag (9-30)
    InitRow {
        t0: &[
            140, 92, 137, 138, 140, 152, 138, 139, 153, 74, 149, 92, 139, 107, 122, 152,
            140, 179, 166, 182, 140, 227, 122, 197,
        ],
        t1: &[
            154, 196, 196, 167, 154, 152, 167, 182, 182, 134, 149, 136, 153, 121, 136, 137,
            169, 194, 166, 167, 154, 167, 137, 182,
        ],
        t2: &[
            154, 196, 167, 167, 154, 152, 167, 182, 182, 134, 149, 136, 153, 121, 136, 122,
            169, 208, 166, 167, 154, 152, 167, 182,
        ],
    },
    // coeff_abs_level_greater2_flag (9-31)
    InitRow { t0: &[138, 153, 136, 167, 152, 152], t1: &[107, 167, 91, 122, 107, 167], t2: &[107, 167, 91, 107, 107, 167] },
    // cu_skip_flag (9-9): no I-slice value
    InitRow { t0: &[154, 154, 154], t1: &[197, 185, 201], t2: &[197, 185, 201] },
    // pred_mode_flag (9-10)
    InitRow { t0: &[154], t1: &[149], t2: &[134] },
    // merge_flag (9-15)
    InitRow { t0: &[154], t1: &[110], t2: &[154] },
    // merge_idx (9-16)
    InitRow { t0: &[154], t1: &[122], t2: &[137] },
    // inter_pred_idc (9-17)
    InitRow { t0: &[154, 154, 154, 154, 154], t1: &[95, 79, 63, 31, 31], t2: &[95, 79, 63, 31, 31] },
    // ref_idx_lX (9-18)
    InitRow { t0: &[154, 154], t1: &[153, 153], t2: &[153, 153] },
    // mvp_lX_flag (9-19)
    InitRow { t0: &[154], t1: &[168], t2: &[168] },
    // abs_mvd_greater0_flag (9-23)
    InitRow { t0: &[154], t1: &[140], t2: &[169] },
    // abs_mvd_greater1_flag (9-23)
    InitRow { t0: &[154], t1: &[198], t2: &[198] },
    // rqt_root_cbf (9-14)
    InitRow { t0: &[154], t1: &[79], t2: &[79] },
    // cu_qp_delta_abs (9-24)
    InitRow { t0: &[154, 154], t1: &[154, 154], t2: &[154, 154] },
    // sao_merge_left_flag / sao_merge_up_flag (9-5)
    InitRow { t0: &[153], t1: &[153], t2: &[153] },
    // sao_type_idx_luma / sao_type_idx_chroma (9-6)
    InitRow { t0: &[200], t1: &[185], t2: &[160] },
    // cu_transquant_bypass_flag (9-22)
    InitRow { t0: &[154], t1: &[154], t2: &[154] },
    // cu_chroma_qp_offset_flag (9-38)
    InitRow { t0: &[154], t1: &[154], t2: &[154] },
    // cu_chroma_qp_offset_idx (9-39)
    InitRow { t0: &[154], t1: &[154], t2: &[154] },
    // explicit_rdpcm_flag (9-40)
    InitRow { t0: &[139, 139], t1: &[139, 139], t2: &[139, 139] },
    // explicit_rdpcm_dir_flag (9-41)
    InitRow { t0: &[139, 139], t1: &[139, 139], t2: &[139, 139] },
    // log2_res_scale_abs_plus1 (9-42)
    InitRow { t0: &[154; 8], t1: &[154; 8], t2: &[154; 8] },
    // res_scale_sign_flag (9-43)
    InitRow { t0: &[154, 154], t1: &[154, 154], t2: &[154, 154] },
];

/// `rangeTabLps[pStateIdx][qRangeIdx]` (Table 9-52).
#[rustfmt::skip]
const RANGE_TAB_LPS: [[u8; 4]; 64] = [
    [128, 176, 208, 240], [128, 167, 197, 227], [128, 158, 187, 216], [123, 150, 178, 205],
    [116, 142, 169, 195], [111, 135, 160, 185], [105, 128, 152, 175], [100, 122, 144, 166],
    [95, 116, 137, 158], [90, 110, 130, 150], [85, 104, 123, 142], [81, 99, 117, 135],
    [77, 94, 111, 128], [73, 89, 105, 122], [69, 85, 100, 116], [66, 80, 95, 110],
    [62, 76, 90, 104], [59, 72, 86, 99], [56, 69, 81, 94], [53, 65, 77, 89],
    [51, 62, 73, 85], [48, 59, 69, 80], [46, 56, 66, 76], [43, 53, 63, 72],
    [41, 50, 59, 69], [39, 48, 56, 65], [37, 45, 54, 62], [35, 43, 51, 59],
    [33, 41, 48, 56], [32, 39, 46, 53], [30, 37, 43, 50], [29, 35, 41, 48],
    [27, 33, 39, 45], [26, 31, 37, 43], [24, 30, 35, 41], [23, 28, 33, 39],
    [22, 27, 32, 37], [21, 26, 30, 35], [20, 24, 29, 33], [19, 23, 27, 31],
    [18, 22, 26, 30], [17, 21, 25, 28], [16, 20, 23, 27], [15, 19, 22, 25],
    [14, 18, 21, 24], [14, 17, 20, 23], [13, 16, 19, 22], [12, 15, 18, 21],
    [12, 14, 17, 20], [11, 14, 16, 19], [11, 13, 15, 18], [10, 12, 15, 17],
    [10, 12, 14, 16], [9, 11, 13, 15], [9, 11, 12, 14], [8, 10, 12, 14],
    [8, 9, 11, 13], [7, 9, 11, 12], [7, 9, 10, 12], [7, 8, 10, 11],
    [6, 8, 9, 11], [6, 7, 9, 10], [6, 7, 8, 9], [2, 2, 2, 2],
];

/// `transIdxLps[pStateIdx]` (Table 9-53).
#[rustfmt::skip]
const TRANS_IDX_LPS: [u8; 64] = [
    0, 0, 1, 2, 2, 4, 4, 5, 6, 7, 8, 9, 9, 11, 11, 12,
    13, 13, 15, 15, 16, 16, 18, 18, 19, 19, 21, 21, 22, 22, 23, 24,
    24, 25, 26, 26, 27, 27, 28, 29, 29, 30, 30, 30, 31, 32, 32, 33,
    33, 33, 34, 34, 35, 35, 35, 36, 36, 36, 37, 37, 37, 38, 38, 63,
];

/// `transIdxMps[pStateIdx]` (Table 9-53).
#[rustfmt::skip]
const TRANS_IDX_MPS: [u8; 64] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
    17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32,
    33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48,
    49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 62, 63,
];

/// `ENTROPY_BITS[pStateIdx] = [bits if the bin matches valMps, bits if it
/// doesn't]`, the fractional-bit cost of a context-coded bin under its
/// context's current probability state — RDOQ's estimating mode reads this
/// instead of replaying the exact range-narrowing arithmetic per trial.
///
/// HEVC's 64 probability states follow the same geometric decay as H.264's
/// (Marpe et al.): `pLPS(pStateIdx) = 0.5 * ALPHA^pStateIdx`, `ALPHA =
/// 0.9493`, running from 0.5 at state 0 down to the ~0.01875 floor at state
/// 63 that `RANGE_TAB_LPS`'s last row (all `2`s) also converges to. Bits are
/// `-log2` of the matching/mismatching probability, x265/HM's `entropyBits`
/// idiom — an entropy estimate, not the coder's own count, so RDOQ under
/// this mode ranks candidates instead of pricing them exactly.
static ENTROPY_BITS: LazyLock<[[f64; 2]; 64]> = LazyLock::new(|| {
    const ALPHA: f64 = 0.9493;
    std::array::from_fn(|p_state| {
        let p_lps = 0.5 * ALPHA.powi(p_state as i32);
        [-(1.0 - p_lps).log2(), -p_lps.log2()]
    })
});

/// The context variables, `pStateIdx << 1 | valMps` per entry.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Contexts {
    state: [u8; ctx::TOTAL],
    /// `StatCoeff[0..4]` of persistent Rice adaptation (9.3.2.4); stored and restored with
    /// the contexts at wavefront and dependent-slice-segment boundaries.
    pub stat_coeff: [u8; 4],
}

impl Contexts {
    /// Initialise every context for an I slice at `slice_qp` (9.3.2.2).
    pub fn new(slice_qp: i32) -> Contexts {
        Contexts::for_init_type(slice_qp, 0)
    }

    /// Initialise every context for `init_type` (0 for I slices, 1 and 2 for
    /// P and B slices, Table 9-4) at `slice_qp` (9.3.2.2, equations 9-4 to 9-6).
    pub fn for_init_type(slice_qp: i32, init_type: u8) -> Contexts {
        let qp = slice_qp.clamp(0, 51);
        let mut state = [0u8; ctx::TOTAL];
        let mut k = 0usize;
        for row in INIT_ROWS {
            let values = match init_type {
                0 => row.t0,
                1 => row.t1,
                _ => row.t2,
            };
            for &init_value in values {
                if k >= ctx::TOTAL {
                    break;
                }
                let slope_idx = (init_value >> 4) as i32;
                let offset_idx = (init_value & 15) as i32;
                let m = slope_idx * 5 - 45;
                let n = (offset_idx << 3) - 16;
                let pre_ctx_state = (((m * qp) >> 4) + n).clamp(1, 126);
                let val_mps = i32::from(pre_ctx_state > 63);
                let p_state = if val_mps == 1 {
                    pre_ctx_state - 64
                } else {
                    63 - pre_ctx_state
                };
                state[k] = ((p_state as u8) << 1) | val_mps as u8;
                k += 1;
            }
        }
        Contexts {
            state,
            stat_coeff: [0; 4],
        }
    }
}

/// A rollback point for [`CabacEncoder`].
///
/// A plain [`CabacEncoder::snapshot`] rewinds by truncation, which is all a
/// trial-and-discard needs. A [`CabacEncoder::snapshot_since`] also carries the
/// bytes written since an earlier point, which is what a trial-and-*keep* needs:
/// a second trial overwrites the first one's bytes in place, so rewinding to a
/// length alone would leave the loser's bytes in the winner's positions.
#[derive(Clone)]
pub struct CabacState {
    low: u32,
    range: u32,
    bits_outstanding: u32,
    first_bit: bool,
    bits: u64,
    out_len: usize,
    tail_base: usize,
    tail: Vec<u8>,
    partial: u8,
    partial_bits: u8,
    contexts: Contexts,
    /// [`CabacEncoder::est_bits`] at snapshot time, restored alongside it.
    est_bits: f64,
}

/// The CABAC encoding engine (9.3.5).
#[derive(Clone)]
pub struct CabacEncoder {
    low: u32,
    range: u32,
    bits_outstanding: u32,
    first_bit: bool,
    /// Bits written, counted whether or not they are kept.
    bits: u64,
    /// `None` while estimating a rate rather than producing a bitstream.
    out: Option<Vec<u8>>,
    /// While set, bins are coded and counted but no bytes are kept — what a
    /// rate-distortion trial wants, since its bytes are about to be thrown away.
    counting: bool,
    partial: u8,
    partial_bits: u8,
    /// Context variables, public to the modules that index them by name.
    pub contexts: Contexts,
    /// While set, `encode_bin`/`encode_bypass*` skip the range/renorm
    /// arithmetic entirely and instead accumulate [`ENTROPY_BITS`]-table
    /// lookups into `est_bits` — RDOQ's trial-costing instrument only; the
    /// committing encoder never sets this. `low`/`range`/`bits_outstanding`
    /// are left untouched while this is set, so a `restore()` back to a
    /// snapshot taken before estimating began is exact.
    estimating: bool,
    /// Fractional bits accumulated while `estimating`; read by
    /// [`CabacEncoder::bit_count_f64`].
    est_bits: f64,
}

impl CabacEncoder {
    /// A fresh engine writing into its own buffer.
    pub fn new(contexts: Contexts) -> CabacEncoder {
        CabacEncoder {
            low: 0,
            range: 510,
            bits_outstanding: 0,
            first_bit: true,
            bits: 0,
            out: Some(Vec::new()),
            counting: false,
            partial: 0,
            partial_bits: 0,
            contexts,
            estimating: false,
            est_bits: 0.0,
        }
    }

    /// An engine that counts bits and writes nothing — the rate half of every
    /// rate-distortion decision.
    pub fn counter(contexts: Contexts) -> CabacEncoder {
        let mut enc = CabacEncoder::new(contexts);
        enc.out = None;
        enc
    }

    /// Bits produced so far, including the ones a counting engine discards.
    /// In `estimating` mode this is stale (the estimating path never
    /// increments it) — read [`CabacEncoder::bit_count_f64`] instead.
    pub fn bit_count(&self) -> u64 {
        self.bits
    }

    /// [`CabacEncoder::bit_count`], but fractional and estimating-mode-aware:
    /// the exact bit count as `f64` normally, or the accumulated
    /// [`ENTROPY_BITS`] estimate while `estimating` is set.
    pub fn bit_count_f64(&self) -> f64 {
        if self.estimating {
            self.est_bits
        } else {
            self.bits as f64
        }
    }

    /// Whether the engine is in `estimating` mode.
    pub fn is_estimating(&self) -> bool {
        self.estimating
    }

    /// Add `bits` to the estimating-mode total (a caller's own closed-form estimate).
    pub fn add_estimated_bits(&mut self, bits: f64) {
        self.est_bits += bits;
    }

    /// Switch between exact range/renorm costing and the estimated
    /// per-context fracbits table. See the `estimating` field.
    pub fn set_estimating(&mut self, estimating: bool) {
        self.estimating = estimating;
    }

    /// Code bins without keeping their bytes; the counter still runs.
    pub fn set_counting(&mut self, counting: bool) {
        self.counting = counting;
    }

    fn write_bit(&mut self, bit: u32) {
        self.bits += 1;
        if self.counting {
            return;
        }
        if let Some(out) = &mut self.out {
            self.partial = (self.partial << 1) | (bit as u8);
            self.partial_bits += 1;
            if self.partial_bits == 8 {
                out.push(self.partial);
                self.partial = 0;
                self.partial_bits = 0;
            }
        }
    }

    fn put_bit(&mut self, bit: u32) {
        if self.first_bit {
            self.first_bit = false;
        } else {
            self.write_bit(bit);
        }
        while self.bits_outstanding > 0 {
            self.write_bit(1 - bit);
            self.bits_outstanding -= 1;
        }
    }

    fn renorm(&mut self) {
        while self.range < 256 {
            if self.low < 256 {
                self.put_bit(0);
            } else if self.low >= 512 {
                self.low -= 512;
                self.put_bit(1);
            } else {
                self.low -= 256;
                self.bits_outstanding += 1;
            }
            self.range <<= 1;
            self.low <<= 1;
        }
    }

    /// Take a snapshot the encoder can be rolled back to.
    ///
    /// This is what makes exact rate measurement affordable: a trial encode runs
    /// into the real coder, its bits are read off the counter, and the coder is
    /// put back. The cost is one 129-byte context copy, not a copy of the
    /// substream written so far.
    pub fn snapshot(&self) -> CabacState {
        let out_len = self.out.as_ref().map_or(0, |o| o.len());
        CabacState {
            low: self.low,
            range: self.range,
            bits_outstanding: self.bits_outstanding,
            first_bit: self.first_bit,
            bits: self.bits,
            out_len,
            tail_base: out_len,
            tail: Vec::new(),
            partial: self.partial,
            partial_bits: self.partial_bits,
            contexts: self.contexts.clone(),
            est_bits: self.est_bits,
        }
    }

    /// A snapshot that also carries the bytes written since `base`, so that
    /// restoring it puts those bytes back after a competing trial overwrote
    /// them.
    pub fn snapshot_since(&self, base: &CabacState) -> CabacState {
        let mut state = self.snapshot();
        state.tail_base = base.out_len;
        if let Some(out) = &self.out {
            state.tail = out[base.out_len..].to_vec();
        }
        state
    }

    /// Roll back to a snapshot.
    pub fn restore(&mut self, state: &CabacState) {
        self.low = state.low;
        self.range = state.range;
        self.bits_outstanding = state.bits_outstanding;
        self.first_bit = state.first_bit;
        self.bits = state.bits;
        self.partial = state.partial;
        self.partial_bits = state.partial_bits;
        self.contexts.clone_from(&state.contexts);
        self.est_bits = state.est_bits;
        if let Some(out) = &mut self.out {
            out.truncate(state.tail_base);
            out.extend_from_slice(&state.tail);
            debug_assert_eq!(out.len(), state.out_len);
        }
    }

    /// Encode one context-coded bin (9.3.5.3).
    #[inline]
    pub fn encode_bin(&mut self, ctx_idx: usize, bin: u32) {
        let state = self.contexts.state[ctx_idx];
        let p_state = (state >> 1) as usize;
        let val_mps = u32::from(state & 1);
        if self.estimating {
            self.est_bits += ENTROPY_BITS[p_state][usize::from(bin != val_mps)];
            self.contexts.state[ctx_idx] = if bin != val_mps {
                let new_mps = if p_state == 0 { 1 - val_mps } else { val_mps };
                (TRANS_IDX_LPS[p_state] << 1) | new_mps as u8
            } else {
                (TRANS_IDX_MPS[p_state] << 1) | val_mps as u8
            };
            return;
        }
        let q_range_idx = ((self.range >> 6) & 3) as usize;
        let lps_range = u32::from(RANGE_TAB_LPS[p_state][q_range_idx]);
        self.range -= lps_range;
        if bin != val_mps {
            self.low += self.range;
            self.range = lps_range;
            let new_mps = if p_state == 0 { 1 - val_mps } else { val_mps };
            self.contexts.state[ctx_idx] = (TRANS_IDX_LPS[p_state] << 1) | new_mps as u8;
        } else {
            self.contexts.state[ctx_idx] = (TRANS_IDX_MPS[p_state] << 1) | val_mps as u8;
        }
        self.renorm();
    }

    /// Encode one bypass bin (9.3.5.5).
    #[inline]
    pub fn encode_bypass(&mut self, bin: u32) {
        if self.estimating {
            // Bypass bins are uncontexted and uniform-probability: the real
            // coder's carry propagation can occasionally defer more than one
            // output bit to a later call, but the standard entropy estimate
            // (x265/HM alike) is exactly one bit per bypass bin.
            self.est_bits += 1.0;
            return;
        }
        self.low <<= 1;
        if bin != 0 {
            self.low += self.range;
        }
        if self.low >= 1024 {
            self.put_bit(1);
            self.low -= 1024;
        } else if self.low < 512 {
            self.put_bit(0);
        } else {
            self.low -= 512;
            self.bits_outstanding += 1;
        }
    }

    /// Encode `count` bypass bins from the low bits of `value`, most
    /// significant first.
    pub fn encode_bypass_bits(&mut self, value: u32, count: u32) {
        for i in (0..count).rev() {
            self.encode_bypass((value >> i) & 1);
        }
    }

    /// Encode a terminating bin: `end_of_slice_segment_flag` or
    /// `end_of_subset_one_bit` (9.3.5.6). A one flushes the engine.
    pub fn encode_terminate(&mut self, bin: u32) {
        self.range -= 2;
        if bin != 0 {
            self.low += self.range;
            self.flush();
        } else {
            self.renorm();
        }
    }

    fn flush(&mut self) {
        self.range = 2;
        self.renorm();
        self.put_bit((self.low >> 9) & 1);
        // The last two bits, the low one always set: it is the
        // rbsp_stop_one_bit or the alignment_bit_equal_to_one.
        self.write_bit((self.low >> 8) & 1);
        self.write_bit(1);
    }

    /// Finish a substream: pad the last byte with zeros and hand back the bytes.
    ///
    /// Only valid after [`CabacEncoder::encode_terminate`] with a one bin, which
    /// is where the flush happened.
    pub fn finish(mut self) -> Vec<u8> {
        let mut out = self.out.take().unwrap_or_default();
        if self.partial_bits > 0 {
            out.push(self.partial << (8 - self.partial_bits));
        }
        out
    }
}

/// The CABAC decoding engine (9.3.4.3.2), the mirror image of
/// [`CabacEncoder`]: same context tables, same `RANGE_TAB_LPS` /
/// `TRANS_IDX_LPS` / `TRANS_IDX_MPS`, so a context updates identically on
/// both sides and a decoded bin is exactly the bin the encoder wrote.
///
/// Reads bits MSB-first from `data` starting at bit 0; a read past the end of
/// `data` returns zero, the standard convention for the handful of bits a
/// CABAC engine may read past a substream's real end while renormalising
/// around its final terminating bin.
pub struct CabacDecoder<'a> {
    data: &'a [u8],
    bit: usize,
    range: u32,
    offset: u32,
    /// Context variables, public for the same reason [`CabacEncoder::contexts`] is.
    pub contexts: Contexts,
}

impl<'a> CabacDecoder<'a> {
    /// A fresh engine reading `data` from its first bit (9.3.2.5 init).
    pub fn new(data: &'a [u8], contexts: Contexts) -> CabacDecoder<'a> {
        let mut d = CabacDecoder {
            data,
            bit: 0,
            range: 510,
            offset: 0,
            contexts,
        };
        for _ in 0..9 {
            d.offset = (d.offset << 1) | d.read_bit();
        }
        d
    }

    fn read_bit(&mut self) -> u32 {
        let byte = self.data.get(self.bit / 8).copied().unwrap_or(0);
        let bit = u32::from((byte >> (7 - (self.bit % 8))) & 1);
        self.bit += 1;
        bit
    }

    /// Decode one context-coded bin (9.3.4.3.2.2).
    pub fn decode_bin(&mut self, ctx_idx: usize) -> u32 {
        let state = self.contexts.state[ctx_idx];
        let p_state = (state >> 1) as usize;
        let val_mps = u32::from(state & 1);
        let q = ((self.range >> 6) & 3) as usize;
        let lps = u32::from(RANGE_TAB_LPS[p_state][q]);
        self.range -= lps;
        let bin;
        if self.offset >= self.range {
            bin = 1 - val_mps;
            self.offset -= self.range;
            self.range = lps;
            let new_mps = if p_state == 0 { 1 - val_mps } else { val_mps };
            self.contexts.state[ctx_idx] = (TRANS_IDX_LPS[p_state] << 1) | new_mps as u8;
        } else {
            bin = val_mps;
            self.contexts.state[ctx_idx] = (TRANS_IDX_MPS[p_state] << 1) | val_mps as u8;
        }
        while self.range < 256 {
            self.range <<= 1;
            self.offset = (self.offset << 1) | self.read_bit();
        }
        bin
    }

    /// Decode one bypass bin (9.3.4.3.4).
    pub fn decode_bypass(&mut self) -> u32 {
        self.offset = (self.offset << 1) | self.read_bit();
        if self.offset >= self.range {
            self.offset -= self.range;
            1
        } else {
            0
        }
    }

    /// Decode `count` bypass bins into one value, most significant first.
    pub fn decode_bypass_bits(&mut self, count: u32) -> u32 {
        let mut v = 0;
        for _ in 0..count {
            v = (v << 1) | self.decode_bypass();
        }
        v
    }

    /// Decode a terminating bin: `end_of_slice_segment_flag` or
    /// `end_of_subset_one_bit` (9.3.4.3.5).
    pub fn decode_terminate(&mut self) -> u32 {
        self.range -= 2;
        if self.offset >= self.range {
            1
        } else {
            while self.range < 256 {
                self.range <<= 1;
                self.offset = (self.offset << 1) | self.read_bit();
            }
            0
        }
    }

    /// Byte offset, from the start of `data`, of the substream after a
    /// `decode_terminate() == 1`: the encoder's flush always pads to a byte
    /// boundary here, so the next WPP row's substream starts exactly there.
    pub fn byte_position_aligned(&self) -> usize {
        self.bit.div_ceil(8)
    }

    /// Bits of the slice data read so far, including the bits read ahead by the
    /// 9-bit offset register (9.3.2.5).
    pub fn bits_read(&self) -> usize {
        self.bit
    }

    /// The slice data this engine reads.
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Restart the arithmetic decoder at byte `byte` of the data (9.3.2.5 after
    /// PCM samples): the offset is read again, the contexts are kept.
    pub fn restart_at_byte(&mut self, byte: usize) {
        self.bit = byte * 8;
        self.range = 510;
        self.offset = 0;
        for _ in 0..9 {
            self.offset = (self.offset << 1) | self.read_bit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_inverts_the_encoder_bin_by_bin() {
        // Every context-coded and bypass bin the encoder can write, decoded
        // back through the same context table, must reproduce the exact
        // sequence: this is what makes a CABAC decoder a mirror rather than a
        // second implementation that might disagree with the first.
        let bins: Vec<(usize, u32)> = (0..NUM_CONTEXTS)
            .flat_map(|ctx| [(ctx, 0u32), (ctx, 1), (ctx, 0), (ctx, 1), (ctx, 1)])
            .collect();
        let mut enc = CabacEncoder::new(Contexts::new(27));
        for &(ctx, bin) in &bins {
            enc.encode_bin(ctx, bin);
        }
        for i in 0..37u32 {
            enc.encode_bypass(i & 1);
        }
        enc.encode_terminate(1);
        let bytes = enc.finish();

        let mut dec = CabacDecoder::new(&bytes, Contexts::new(27));
        for &(ctx, bin) in &bins {
            assert_eq!(dec.decode_bin(ctx), bin);
        }
        for i in 0..37u32 {
            assert_eq!(dec.decode_bypass(), i & 1);
        }
        assert_eq!(dec.decode_terminate(), 1);
    }

    #[test]
    fn context_init_matches_the_worked_formula() {
        // initValue 154 is the "no slope" entry: m = 0, n = 64, so every QP
        // lands on preCtxState 64, valMps 1, pStateIdx 0.
        let init_value: u8 = 154;
        let m = (init_value >> 4) as i32 * 5 - 45;
        let n = (((init_value & 15) as i32) << 3) - 16;
        assert_eq!((m, n), (0, 64));
        // And a real one: split_cu_flag ctx 0 (139) at QP 30.
        let ctxs = Contexts::new(30);
        let m = (139u8 >> 4) as i32 * 5 - 45;
        let n = (((139u8 & 15) as i32) << 3) - 16;
        let pre = (((m * 30) >> 4) + n).clamp(1, 126);
        let expect = if pre > 63 {
            (((pre - 64) as u8) << 1) | 1
        } else {
            ((63 - pre) as u8) << 1
        };
        assert_eq!(ctxs.state[ctx::SPLIT_CU], expect);
    }

    #[test]
    fn state_tables_are_consistent() {
        // Every LPS transition moves towards the uncertain end, every MPS
        // transition towards the certain one, and state 63 is absorbing.
        for s in 0..64usize {
            assert!(TRANS_IDX_LPS[s] as usize <= s);
            assert!(TRANS_IDX_MPS[s] as usize >= s);
            assert!(TRANS_IDX_MPS[s] < 64 && TRANS_IDX_LPS[s] < 64);
            // rangeTabLps shrinks as the state gets more certain.
            if s > 0 {
                for (q, &value) in RANGE_TAB_LPS[s].iter().enumerate() {
                    assert!(value <= RANGE_TAB_LPS[s - 1][q]);
                }
            }
        }
        assert_eq!(TRANS_IDX_LPS[63], 63);
        assert_eq!(TRANS_IDX_MPS[63], 63);
    }

    #[test]
    fn counting_engine_tracks_the_writing_engine() {
        let mut writer = CabacEncoder::new(Contexts::new(30));
        let mut counter = CabacEncoder::counter(Contexts::new(30));
        for i in 0..500u32 {
            let bin = (i * 7 % 5) & 1;
            writer.encode_bin(ctx::SIG_COEFF + (i as usize % 44), bin);
            counter.encode_bin(ctx::SIG_COEFF + (i as usize % 44), bin);
            writer.encode_bypass(bin);
            counter.encode_bypass(bin);
        }
        assert_eq!(writer.bit_count(), counter.bit_count());
        assert_eq!(writer.contexts, counter.contexts);
    }

    #[test]
    fn the_spec_decoder_reads_back_what_the_encoder_wrote() {
        // A long mixed sequence: context bins across many contexts, bypass bins,
        // and the terminating bin that flushes. If the engine's carry handling
        // or state transitions were wrong, this diverges within a few bins —
        // which is exactly the failure a real decoder shows as a broken picture.
        let mut plan: Vec<(usize, u32, u8)> = Vec::new(); // (ctx or 0, bin, kind)
        let mut seed = 12345u32;
        let mut rand = move || {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            (seed >> 16) & 0x7fff
        };
        for i in 0..3000 {
            let kind = if i % 7 == 3 { 1 } else { 0 };
            let ctx = (rand() as usize) % ctx::TOTAL;
            let bin = if i % 5 == 0 { 1 } else { rand() & 1 };
            plan.push((ctx, bin, kind));
        }
        let mut enc = CabacEncoder::new(Contexts::new(32));
        for &(ctx_idx, bin, kind) in &plan {
            if kind == 0 {
                enc.encode_bin(ctx_idx, bin);
            } else {
                enc.encode_bypass(bin);
            }
        }
        enc.encode_terminate(1);
        let bytes = enc.finish();

        let mut dec = CabacDecoder::new(&bytes, Contexts::new(32));
        for (i, &(ctx_idx, bin, kind)) in plan.iter().enumerate() {
            let got = if kind == 0 {
                dec.decode_bin(ctx_idx)
            } else {
                dec.decode_bypass()
            };
            assert_eq!(got, bin, "bin {i} (ctx {ctx_idx}, kind {kind})");
        }
        assert_eq!(dec.decode_terminate(), 1, "terminating bin");
    }

    #[test]
    fn a_skewed_symbol_stream_compresses() {
        // 4000 zeros through one adapting context must cost far less than 4000
        // bits — the property that makes the engine an arithmetic coder at all.
        let mut enc = CabacEncoder::new(Contexts::new(30));
        for _ in 0..4000 {
            enc.encode_bin(ctx::SIG_COEFF, 0);
        }
        enc.encode_terminate(1);
        let bits = enc.bit_count();
        let bytes = enc.finish();
        assert!(bits < 400, "{bits} bits for 4000 skewed bins");
        assert!(!bytes.is_empty());
    }
}

#[cfg(test)]
mod init_table_tests {
    use super::*;

    /// Every syntax element's init row covers exactly its contexts, in all three
    /// initTypes, so `Contexts::for_init_type` never reads past a row.
    #[test]
    fn init_rows_cover_every_context_for_each_init_type() {
        let total: usize = INIT_ROWS.iter().map(|r| r.t0.len()).sum();
        assert_eq!(total, ctx::TOTAL);
        for row in INIT_ROWS {
            assert_eq!(row.t0.len(), row.t1.len());
            assert_eq!(row.t0.len(), row.t2.len());
        }
    }

    /// initType 0 reproduces the I-slice states the intra path has always used.
    #[test]
    fn init_type_zero_is_the_intra_table() {
        assert_eq!(Contexts::new(30), Contexts::for_init_type(30, 0));
        assert_ne!(
            Contexts::for_init_type(30, 0),
            Contexts::for_init_type(30, 1)
        );
        assert_ne!(
            Contexts::for_init_type(30, 1),
            Contexts::for_init_type(30, 2)
        );
    }
}

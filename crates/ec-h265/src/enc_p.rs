//! P pictures: the inter half of the sequence encoder.
//!
//! A sequence is an IDR every `keyint` pictures and P pictures in between. A P
//! picture has one reference, the previous reconstructed picture, named by a
//! short-term RPS in its slice header (one negative picture, one bit of
//! delta). Every coding unit is inter and 2Nx2N; its prediction is chosen by
//! rate-distortion among
//!
//! - skip, and merge with a residual, for each merge candidate of
//!   [`derive_merge`](crate::dec::merge::derive_merge),
//! - AMVP with either predictor, with the vector the motion search found and
//!   a residual or none (`rqt_root_cbf`).
//!
//! Motion is searched per block ([`crate::enc_me`]) over a quadtree of 8 to 32
//! luma samples, decided on the search costs before any coding unit is
//! committed. The reconstruction is the decoder's own: the prediction is
//! [`predict_pu`], the in-loop deblocking is [`deblock_picture`], and the
//! motion the merge and AMVP candidates read is the decoder's
//! [`MotionField`]. Sample adaptive offset is off in this stream, as it is in
//! the intra encoder.

use crate::cabac::{CabacEncoder, Contexts, ctx};
use crate::ctu::{Pel, lambda_for};
use crate::dec::amvp::derive_amvp;
use crate::dec::cu_syntax::{CuGeometry, PartMode, PredictionUnit};
use crate::dec::deblock::{deblock_picture, deblock_rows};
use crate::dec::mc::{Block, McScratch, PlaneMut, PlaneRef, RefPicture, predict_pu};
use crate::dec::merge::derive_merge;
use crate::dec::motion_field::{
    FieldCell, MotionField, MotionParams, PbMotion, SliceMotion, SliceRefs, to32,
};
use crate::dec::pic::{BlockInfo, EDGE_PU, EDGE_TU, PicState, PuMotion, SliceFilter};
use crate::dec::rps::{RefEntry, RefPicLists};
use crate::dec::shared::SharedPic;
use crate::enc_me::{FULL_RANGE_LARGE, FULL_RANGE_SMALL, RefPlanes, Search};
use crate::enc_rc::{BitRateControl, offset_after_batch};
use crate::encoder::Preset;
use crate::encoder::{
    EncodedPicture, Encoder, PaddedSource, Progress, RateControl, bitrate_picture_bits,
    plane_samples, wait_for_or_abort,
};
use crate::intra::{mpm_list, scan_index};
use crate::residual::hide_signs;
use crate::residual::{encode_residual, estimate_residual_bits};
use crate::transform::{
    chroma_qp_bd, dequantize_bd, forward_transform, inverse_transform_bd, min_nonzero_coeff,
    quant_qp, quantize_offset, scale_forward_bd,
};
use ec_core::bitio::BitWriter;
use ec_core::error::{Error, Result};
use ec_core::frame::VideoFrame;
use ec_h265_syntax::nal::{NalHeader, NalUnitType, escape_rbsp, write_annex_b};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// `MaxNumMergeCand` of the P slices this encoder writes.
const MAX_MERGE_CAND: usize = 5;

/// QP offset of the P pictures from the IDR's. A coarser P picture spends fewer
/// bits on the residual, which the mode decision then trades better: +2 gives the
/// best BD-rate on the natural clip (+4 is no better on the chroma-weighted metric).
pub(crate) const P_QP_DELTA: i32 = 2;

/// `MinCbLog2SizeY` of the sequence (8x8), which codes `part_mode` for intra units.
const MIN_CB_LOG2: u32 = 3;

/// Cost per sample (SSD plus lambda times bits, over the block) below which a merge
/// candidate with no residual ends the mode search of a coding unit.
const EARLY_SKIP_COST: f64 = 4.0;

/// Mean absolute difference per sample below which a motion-searched block is not split.
const EARLY_SPLIT_SAD: f64 = 0.25;

/// Search thresholds of the P-picture decision. The defaults are the measured
/// values; the `EC_H265_*` overrides exist for the sweep in the lane report.
#[derive(Clone, Copy)]
pub(crate) struct PKnobs {
    /// Whole-sample scan half-width for blocks of 32 and up.
    full_large: i16,
    /// Whole-sample scan half-width for smaller blocks.
    full_small: i16,
    /// Mean absolute difference per sample below which a block is not split.
    split_sad: f64,
    /// Cost per sample below which a residual-free merge ends the search.
    skip_cost: f64,
    /// Cost per sample below which the whole-sample scan is skipped.
    full_gate: f64,
    /// Cost per sample below which the best inter candidate ends the intra search.
    intra_gate: f64,
    /// Intra luma modes costed in full, the best by SAD of their prediction.
    intra_k: usize,
    /// A transform block whose residual energy times the transform gain is below this many
    /// squares of the smallest non-zero coefficient is taken to quantise to zero without
    /// transforming it (1 is the bound that is exact; larger trades a little accuracy).
    zero_alpha: f64,
    /// First pattern step (quarter samples) of the search for 16x16 and 8x8 blocks, and halves.
    step16: i16,
    step8: i16,
    step_part: i16,
    full_stride: i16,
    res_proxy: bool,
    sub_passes: u32,
    /// Most distinct merge candidates costed in full.
    merge_max: usize,
    /// Cost both AMVP predictor flags (otherwise only the one with the smaller vector difference).
    amvp_both: bool,
    /// log2 of the smallest inter coding unit the analysis searches (3 = 8x8).
    min_log2: u32,
    /// Smallest square (samples) whose horizontal and vertical halves are searched and costed.
    halves_min: usize,
    /// Most intra luma modes considered (MPMs first) before the SAD ranking.
    intra_modes: usize,
    /// Cost per sample below which the sub-sample refinement is skipped.
    sub_gate: f64,
    /// Skip the quarter-sample refinement step.
    no_quarter: bool,
}

impl PKnobs {
    /// The knobs of a speed preset. `Default` is the measured set; `EC_H265_*`
    /// environment variables override any of them (sweeps only).
    pub(crate) fn for_preset(p: Preset) -> PKnobs {
        let f = |name: &str, d: f64| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(d)
        };
        // (fast, faster) values against the default.
        let pick = |d: f64, fast: f64, faster: f64| match p {
            Preset::Default => d,
            Preset::Fast => fast,
            Preset::Faster => faster,
        };
        PKnobs {
            full_large: f(
                "EC_H265_FULL_LARGE",
                pick(f64::from(FULL_RANGE_LARGE), 8.0, 4.0),
            ) as i16,
            full_small: f(
                "EC_H265_FULL_SMALL",
                pick(f64::from(FULL_RANGE_SMALL), 4.0, 0.0),
            ) as i16,
            split_sad: f("EC_H265_SPLIT_SAD", pick(EARLY_SPLIT_SAD, 1.0, 2.0)),
            skip_cost: f("EC_H265_SKIP_COST", pick(EARLY_SKIP_COST, 16.0, 32.0)),
            full_gate: f("EC_H265_FULL_GATE", pick(4.0, 12.0, 16.0)),
            intra_gate: f("EC_H265_INTRA_GATE", pick(0.0, 6.0, 12.0)),
            intra_k: f("EC_H265_INTRA_K", 1.0) as usize,
            zero_alpha: f("EC_H265_ZERO_ALPHA", pick(24.0, 24.0, 24.0)),
            step16: f("EC_H265_STEP16", 16.0) as i16,
            step8: f("EC_H265_STEP8", 16.0) as i16,
            step_part: f("EC_H265_STEPP", 16.0) as i16,
            full_stride: f("EC_H265_FULL_STRIDE", 4.0) as i16,
            sub_passes: f("EC_H265_SUB_PASSES", pick(1.0, 1.0, 1.0)) as u32,
            res_proxy: f("EC_H265_RES_PROXY", 1.0) != 0.0,
            merge_max: f("EC_H265_MERGE_MAX", pick(5.0, 3.0, 2.0)) as usize,
            amvp_both: f("EC_H265_AMVP_BOTH", pick(1.0, 0.0, 0.0)) != 0.0,
            min_log2: f("EC_H265_MIN_LOG2", pick(3.0, 3.0, 4.0)) as u32,
            intra_modes: f("EC_H265_INTRA_MODES", pick(7.0, 3.0, 2.0)) as usize,
            sub_gate: f("EC_H265_SUB_GATE", pick(0.0, 1.0, 2.0)),
            no_quarter: f("EC_H265_NO_QUARTER", pick(0.0, 0.0, 1.0)) != 0.0,
            halves_min: f("EC_H265_HALVES_MIN", pick(16.0, 32.0, 1024.0)) as usize,
        }
    }
}

/// Intra luma modes tried besides the most probable ones: planar, DC,
/// horizontal and vertical.
const INTRA_FIXED_MODES: [u8; 4] = [0, 1, 10, 26];

/// The reconstructed luma and chroma of a picture at the coded size, as the
/// decoder holds it: one `u16` per sample, rows `width` (luma) or `width / 2`
/// (chroma) apart.
#[derive(Debug, Clone)]
pub(crate) struct PRecon {
    /// Luma samples.
    pub y: Vec<u16>,
    /// Cb samples.
    pub cb: Vec<u16>,
    /// Cr samples.
    pub cr: Vec<u16>,
}

/// An intra coding unit's luma mode and the candidate list it is coded against.
#[derive(Debug, Clone, Copy)]
struct IntraCu {
    mode: u8,
    mpm: [u8; 3],
}

/// One candidate's prediction mode, as the syntax writes it.
#[derive(Debug, Clone)]
struct CuSyntax {
    skip: bool,
    /// The intra mode, for an intra unit (which then has no `merge`, `mvd`).
    intra: Option<IntraCu>,
    /// The prediction split, and for a split unit its two `(mvd, mvp_lX_flag)`
    /// pairs and its four transform units.
    part: Part,
    pus: [([i32; 2], u8); 2],
    tus: Vec<TuSyn>,
    /// `merge_idx` when the unit is a merge unit.
    merge: Option<u8>,
    /// `mvd_coding` values and `mvp_lX_flag`, for AMVP.
    mvd: [i32; 2],
    mvp_flag: u8,
    /// `rqt_root_cbf`, for AMVP (merge units always carry a transform tree).
    root: bool,
    /// `cbf_luma`, `cbf_cb`, `cbf_cr` at depth 0.
    cbf: [bool; 3],
    /// Quantised levels of the luma, Cb and Cr transform blocks.
    levels: [Vec<i32>; 3],
}

/// The reconstruction and cost of one candidate motion.
struct Eval {
    syntax: CuSyntax,
    recon: [Vec<u16>; 3],
}

/// The partition of one coding tree block, decided before coding it.
#[derive(Debug)]
enum Tree {
    /// A coding unit of the node's size with this motion.
    Leaf(LeafMotion),
    /// Four children (only those whose origin is inside the picture), in
    /// z-order.
    Split(Vec<Tree>),
}

/// One transform block coded against its prediction.
struct Tb {
    cbf: bool,
    levels: Vec<i32>,
    recon: Vec<u16>,
    ssd: f64,
}

/// How a coding unit's prediction is split: one block, or two halves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    /// 2Nx2N.
    Square,
    /// 2NxN: the top and the bottom half.
    Horizontal,
    /// Nx2N: the left and the right half.
    Vertical,
}

/// The rectangles `(x, y, w, h)` of the two prediction blocks of a coding unit.
fn partition_rects(x: usize, y: usize, n: usize, part: Part) -> [(usize, usize, usize, usize); 2] {
    match part {
        Part::Square => [(x, y, n, n), (x, y, n, n)],
        Part::Horizontal => [(x, y, n, n / 2), (x, y + n / 2, n, n / 2)],
        Part::Vertical => [(x, y, n / 2, n), (x + n / 2, y, n / 2, n)],
    }
}

/// The motion analysis found for a coding unit: its square vector, and the split
/// with the two halves' vectors when the analysis found it cheaper.
#[derive(Debug, Clone, Copy)]
struct LeafMotion {
    mv: [i16; 2],
    part: Part,
    pmvs: [[i16; 2]; 2],
}

/// The quantised levels and coded block flags of one transform unit of a coding
/// unit split into two halves (its four units are in z-order).
#[derive(Debug, Clone)]
struct TuSyn {
    cbf: [bool; 3],
    levels: [Vec<i32>; 3],
}

/// Quantise, reconstruct and measure one `n` x `n` block at `(sx, sy)` of
/// `src` against the prediction in `pred` (both at the picture's stride).
#[allow(clippy::too_many_arguments)]
fn code_tb(
    src: &[Pel],
    sstride: usize,
    pred: &[Pel],
    pstride: usize,
    at: (usize, usize),
    n: usize,
    qp: i32,
    bd: u32,
    sign_hiding: bool,
    scan_dst: (usize, bool),
    zero_alpha: f64,
) -> Tb {
    match n {
        4 => code_tb_n::<16>(
            src,
            sstride,
            pred,
            pstride,
            at,
            n,
            qp,
            bd,
            sign_hiding,
            scan_dst,
            zero_alpha,
        ),
        8 => code_tb_n::<64>(
            src,
            sstride,
            pred,
            pstride,
            at,
            n,
            qp,
            bd,
            sign_hiding,
            scan_dst,
            zero_alpha,
        ),
        16 => code_tb_n::<256>(
            src,
            sstride,
            pred,
            pstride,
            at,
            n,
            qp,
            bd,
            sign_hiding,
            scan_dst,
            zero_alpha,
        ),
        _ => code_tb_n::<1024>(
            src,
            sstride,
            pred,
            pstride,
            at,
            n,
            qp,
            bd,
            sign_hiding,
            scan_dst,
            zero_alpha,
        ),
    }
}

/// [`code_tb`] with the working buffers on the stack, sized `SQ = n * n`.
#[allow(clippy::too_many_arguments)]
fn code_tb_n<const SQ: usize>(
    src: &[Pel],
    sstride: usize,
    pred: &[Pel],
    pstride: usize,
    (sx, sy): (usize, usize),
    n: usize,
    qp: i32,
    bd: u32,
    sign_hiding: bool,
    (scan, dst): (usize, bool),
    zero_alpha: f64,
) -> Tb {
    let mut res = [0i32; SQ];
    let mut energy = 0i64;
    for r in 0..n {
        let s = &src[(sy + r) * sstride + sx..][..n];
        let p = &pred[(sy + r) * pstride + sx..][..n];
        let o = &mut res[r * n..][..n];
        let mut e = 0i32;
        for c in 0..n {
            let d = i32::from(s[c]) - i32::from(p[c]);
            o[c] = d;
            // |d| < 2^12 and n <= 32 samples per row: the row sum stays below 2^29.
            e += d * d;
        }
        energy += i64::from(e);
    }
    // The transform scales energy by (128 / n)^2, so a residual whose energy cannot lift
    // any coefficient to the smallest non-zero level quantises to nothing.
    let t = min_nonzero_coeff(n, qp, 12) as f64;
    if energy as f64 * (128.0 / n as f64).powi(2) < zero_alpha * t * t {
        let mut recon = vec![0u16; n * n];
        for r in 0..n {
            recon[r * n..][..n].copy_from_slice(&pred[(sy + r) * pstride + sx..][..n]);
        }
        return Tb {
            cbf: false,
            levels: vec![0; n * n],
            recon,
            ssd: energy as f64,
        };
    }
    let mut coeffs = [0i32; SQ];
    forward_transform(&res, &mut coeffs, n, dst);
    scale_forward_bd(&mut coeffs, n, bd);
    let mut levels = vec![0i32; n * n];
    // Round to nearest: a residual is noise-like, and the intra dead zone (1/3)
    // throws away the small levels that carry most of the inter detail.
    let mut nonzero = quantize_offset(&coeffs, &mut levels, n, quant_qp(qp, bd), 12);
    if sign_hiding && nonzero > 0 {
        nonzero = hide_signs(&coeffs, &mut levels, n, quant_qp(qp, bd), scan);
    }
    let cbf = nonzero > 0;
    if cbf {
        let mut scaled = [0i32; SQ];
        dequantize_bd(&levels, &mut scaled, n, qp, bd);
        inverse_transform_bd(&scaled, &mut res, n, dst, bd);
    } else {
        res.fill(0);
    }
    let mut recon = vec![0u16; n * n];
    let max = (1i32 << bd) - 1;
    let mut ssd = 0i64;
    for r in 0..n {
        let s = &src[(sy + r) * sstride + sx..][..n];
        let p = &pred[(sy + r) * pstride + sx..][..n];
        let rr = &res[r * n..][..n];
        let o = &mut recon[r * n..][..n];
        let mut e = 0i32;
        for c in 0..n {
            let v = (i32::from(p[c]) + rr[c]).clamp(0, max);
            o[c] = v as u16;
            let d = i32::from(s[c]) - v;
            e += d * d;
        }
        ssd += i64::from(e);
    }
    Tb {
        cbf,
        levels,
        recon,
        ssd: ssd as f64,
    }
}

/// `k`-th order exp-Golomb bins in bypass mode (9.3.3.11 with k = 1 here).
fn write_egk(e: &mut CabacEncoder, mut value: u32, mut k: u32) {
    loop {
        if value >= (1 << k) {
            e.encode_bypass(1);
            value -= 1 << k;
            k += 1;
        } else {
            e.encode_bypass(0);
            while k > 0 {
                k -= 1;
                e.encode_bypass((value >> k) & 1);
            }
            break;
        }
    }
}

/// `merge_idx`: truncated unary with the first bin in context (9.3.4.2).
fn write_merge_idx(e: &mut CabacEncoder, idx: u8) {
    let c_max = (MAX_MERGE_CAND - 1) as u8;
    for i in 0..c_max {
        let bin = u32::from(i < idx);
        if i == 0 {
            e.encode_bin(ctx::MERGE_IDX, bin);
        } else {
            e.encode_bypass(bin);
        }
        if bin == 0 {
            return;
        }
    }
}

/// `mvd_coding` (7.3.8.9) for one list-0 vector difference.
fn write_mvd(e: &mut CabacEncoder, mvd: [i32; 2]) {
    let abs = [mvd[0].unsigned_abs(), mvd[1].unsigned_abs()];
    e.encode_bin(ctx::ABS_MVD_GT0, u32::from(abs[0] > 0));
    e.encode_bin(ctx::ABS_MVD_GT0, u32::from(abs[1] > 0));
    for a in abs {
        if a > 0 {
            e.encode_bin(ctx::ABS_MVD_GT1, u32::from(a > 1));
        }
    }
    for (a, v) in abs.into_iter().zip(mvd) {
        if a > 0 {
            if a > 1 {
                write_egk(e, a - 2, 1);
            }
            e.encode_bypass(u32::from(v < 0));
        }
    }
}

/// The previous picture, as the next P picture sees it: its motion field (which
/// seeds the search) and the collocated view the decoder's temporal candidates
/// read (8.5.3.2.8).
pub(crate) struct PrevPic {
    field: MotionField,
    shared: Arc<SharedPic>,
}

/// The running state of one P picture.
struct PCoder<'a> {
    w: usize,
    h: usize,
    ctb_log2: u32,
    qp: i32,
    /// The bit depth both planes are coded at.
    bd: u32,
    /// qP of luma, QpY + QpBdOffset, and qP of chroma: what the quantiser takes.
    qpd: i32,
    qpc: i32,
    /// Lagrange multiplier of the mode decision, in squared-error units per bit.
    lambda: f64,
    /// Lagrange multiplier of the motion search, in SAD units per bit: the
    /// square root of the squared-error one (as HM sets it), because SAD grows
    /// with the error amplitude while squared error grows with its square.
    search_lambda: f64,
    sign_hiding: bool,
    /// The SPS allows one level of transform-tree split for intra units, so the
    /// `split_transform_flag` of an intra unit is coded.
    intra_tt_split: bool,
    deblock: bool,
    poc: i32,
    src: &'a PaddedSource,
    /// The reference picture, as the decoder's `RefPicture`.
    ref_pic: RefPicture<'a>,
    ref_poc: i32,
    pic: PicState,
    field: MotionField,
    lists: RefPicLists,
    params: MotionParams,
    /// Prediction buffers at the picture size, one per component.
    pred: [Vec<u16>; 3],
    mc: McScratch,
    /// The reference luma as the motion search reads it.
    rp: &'a RefPlanes,
    /// The previous picture: the collocated motion of the temporal candidates.
    col: Option<Arc<PrevPic>>,
    k: PKnobs,
}

impl<'a> PCoder<'a> {
    #[allow(clippy::too_many_arguments)] // one picture's inputs and outputs
    fn new(
        enc: &Encoder,
        src: &'a PaddedSource,
        refp: &'a PRecon,
        rp: &'a RefPlanes,
        ref_poc: i32,
        poc: i32,
        qp: i32,
        col: Option<Arc<PrevPic>>,
    ) -> PCoder<'a> {
        let (w, h) = (enc.coded_width, enc.coded_height);
        let ctb_log2 = enc.cfg.ctb_size.trailing_zeros();
        // qP = QpY + QpBdOffset for luma and chroma (8.6.1); the quantiser works on qP.
        let bd = u32::from(enc.cfg.bit_depth);
        let off = 6 * (bd as i32 - 8);
        let (qpd, qpc) = (qp + off, chroma_qp_bd(qp, 0, bd) + off);
        let ref_pic: RefPicture<'a> = [
            PlaneRef {
                samples: &refp.y,
                stride: w,
                width: w,
                height: h,
                bit_depth: bd,
            },
            PlaneRef {
                samples: &refp.cb,
                stride: w / 2,
                width: w / 2,
                height: h / 2,
                bit_depth: bd,
            },
            PlaneRef {
                samples: &refp.cr,
                stride: w / 2,
                width: w / 2,
                height: h / 2,
                bit_depth: bd,
            },
        ];
        let mut pic = PicState::new(w, h, ctb_log2, 2, bd, bd, true);
        pic.strong_intra_smoothing = true; // as the sequence parameter set says
        pic.slices = vec![SliceFilter {
            deblock_disabled: !enc.cfg.deblock,
            beta_offset_div2: 0,
            tc_offset_div2: 0,
            loop_filter_across_slices: false,
            sao_luma: false,
            sao_chroma: false,
        }];
        let lists = RefPicLists {
            l0: vec![RefEntry {
                id: 0,
                poc: ref_poc,
                long_term: false,
            }],
            l1: Vec::new(),
        };
        let params = MotionParams {
            is_b: false,
            max_merge_cand: MAX_MERGE_CAND,
            log2_par_mrg_level: 2,
            ctb_log2,
            collocated_from_l0: true,
            // slice_temporal_mvp_enabled_flag is 1 in every P slice; with no
            // previous picture (after an IDR) the collocated view is absent.
            temporal_mvp: true,
            no_backward_pred: true,
        };
        let mut field = MotionField::new(w, h);
        field.slices.push(SliceRefs::from_lists(&lists));
        PCoder {
            w,
            h,
            ctb_log2,
            qp,
            bd,
            qpd,
            qpc,
            lambda: lambda_for(qpd) * enc.cfg.inter_lambda_scale,
            // The search measures SAD on 8-bit samples, so its lambda is the 8-bit one.
            search_lambda: lambda_for(qp).sqrt(),
            sign_hiding: enc.cfg.sign_hiding,
            intra_tt_split: enc.cfg.rqt,
            deblock: enc.cfg.deblock,
            poc,
            src,
            ref_pic,
            ref_poc,
            pic,
            field,
            lists,
            params,
            pred: [vec![0; w * h], vec![0; w * h / 4], vec![0; w * h / 4]],
            mc: McScratch::new(),
            rp,
            col,
            k: enc.pk,
        }
    }

    /// The motion source the decoder's merge and AMVP derivations read.
    fn sm(&mut self) -> SliceMotion<'_> {
        SliceMotion {
            params: self.params,
            lists: &self.lists,
            cur_poc: self.poc,
            field: &mut self.field,
            col: self.col.as_ref().map(|p| Arc::clone(&p.shared)),
            slice: 0,
            publish: None,
        }
    }

    /// `cu_skip_flag` context: the left and above units' skip flags.
    fn skip_ctx(&self, x: usize, y: usize) -> usize {
        let skip_at =
            |xx: usize, yy: usize| self.pic.info[(yy >> 2) * self.pic.w4 + (xx >> 2)].skip;
        usize::from(x > 0 && skip_at(x - 1, y)) + usize::from(y > 0 && skip_at(x, y - 1))
    }

    /// Write `split_cu_flag` for the node at `(x, y)` of size `1 << log2`.
    fn write_split(&self, e: &mut CabacEncoder, x: usize, y: usize, log2: u32, split: bool) {
        let depth = (self.ctb_log2 - log2) as u8;
        let deeper =
            |xx: usize, yy: usize| self.pic.info[(yy >> 2) * self.pic.w4 + (xx >> 2)].depth > depth;
        let inc = usize::from(x > 0 && deeper(x - 1, y)) + usize::from(y > 0 && deeper(x, y - 1));
        e.encode_bin(ctx::SPLIT_CU + inc, u32::from(split));
    }

    /// The vector of the neighbour at `(x, y)` when it is an inter unit of this
    /// slice that uses list 0.
    fn nb_mv(&mut self, x: isize, y: isize) -> Option<[i16; 2]> {
        let m = self.sm().neighbour(x, y)?;
        m.pred[0].then_some(m.mv[0])
    }

    /// Motion search for the `n` x `n` block at `(x, y)`: its cost and motion,
    /// with the split into halves when that costs less (blocks of 16 and up).
    fn search_node(
        &mut self,
        x: usize,
        y: usize,
        n: usize,
        parent: [i16; 2],
    ) -> Result<(f64, LeafMotion)> {
        let mvp = {
            let sm = self.sm();
            let pu = PredictionUnit {
                x,
                y,
                width: n,
                height: n,
                ..PredictionUnit::default()
            };
            derive_amvp(&sm, &pu).mv[0]
        };
        // Seeds: the zero vector, the parent's vector, the spatial neighbours
        // (left, above, above-right, above-left), and the previous picture's
        // vector at the block's centre.
        let mut seeds = vec![[0i16, 0i16], parent];
        let left = self.nb_mv(x as isize - 1, (y + n - 1) as isize);
        let above = self.nb_mv((x + n - 1) as isize, y as isize - 1);
        let above_right = self.nb_mv((x + n) as isize, y as isize - 1);
        let above_left = self.nb_mv(x as isize - 1, y as isize - 1);
        let (cx, cy) = ((x + n / 2) as isize, (y + n / 2) as isize);
        let col = self
            .col
            .as_ref()
            .and_then(|p| p.field.at(cx, cy))
            .filter(|c| c.motion.pred[0])
            .map(|c| c.motion.mv[0]);
        seeds.extend(
            left.into_iter()
                .chain(above)
                .chain(above_right)
                .chain(above_left)
                .chain(col),
        );
        let src: &'a PaddedSource = self.src;
        let search = Search {
            src: &src.y8,
            src_stride: self.w,
            reference: self.rp,
            lambda: self.search_lambda,
            full_range: if n >= 32 {
                self.k.full_large
            } else {
                self.k.full_small
            },
            full_gate: self.k.full_gate,
            full_stride: self.k.full_stride,
            sub_passes: self.k.sub_passes,
            sub_gate: self.k.sub_gate,
            no_quarter: self.k.no_quarter,
            first_step: match n {
                32.. => 16,
                16.. => self.k.step16,
                _ => self.k.step8,
            },
        };
        let best = search.run((x, y), (n, n), mvp, &seeds);
        let mut leaf = LeafMotion {
            mv: best.mv,
            part: Part::Square,
            pmvs: [best.mv; 2],
        };
        let mut cost = best.cost;
        // A square that the seeds predict almost exactly is not split into halves.
        if n >= self.k.halves_min && cost >= self.k.split_sad * (n * n) as f64 {
            for part in [Part::Horizontal, Part::Vertical] {
                let (pcost, pmvs) = self.search_parts(x, y, n, part, best.mv)?;
                if pcost < cost {
                    cost = pcost;
                    leaf = LeafMotion {
                        mv: best.mv,
                        part,
                        pmvs,
                    };
                }
            }
        }
        Ok((cost, leaf))
    }

    /// Search the two halves of the `n` x `n` block at `(x, y)` split as `part`,
    /// each on its own, the second seeded by the first. Returns the total cost and
    /// the two vectors.
    fn search_parts(
        &mut self,
        x: usize,
        y: usize,
        n: usize,
        part: Part,
        square: [i16; 2],
    ) -> Result<(f64, [[i16; 2]; 2])> {
        let mut total = 0f64;
        let mut mvs = [[0i16; 2]; 2];
        for (i, (rx, ry, rw, rh)) in partition_rects(x, y, n, part).into_iter().enumerate() {
            let mvp = {
                let sm = self.sm();
                let pu = PredictionUnit {
                    x: rx,
                    y: ry,
                    width: rw,
                    height: rh,
                    ..PredictionUnit::default()
                };
                derive_amvp(&sm, &pu).mv[0]
            };
            let mut seeds = vec![[0i16, 0i16], square];
            if i == 1 {
                seeds.push(mvs[0]);
            }
            let src: &'a PaddedSource = self.src;
            let search = Search {
                src: &src.y8,
                src_stride: self.w,
                reference: self.rp,
                lambda: self.search_lambda,
                full_range: 0,
                full_gate: 0.0,
                full_stride: 1,
                sub_passes: self.k.sub_passes,
                sub_gate: self.k.sub_gate,
                no_quarter: self.k.no_quarter,
                first_step: self.k.step_part,
            };
            let best = search.run((rx, ry), (rw, rh), mvp, &seeds);
            total += best.cost;
            mvs[i] = best.mv;
        }
        Ok((total, mvs))
    }

    /// The quadtree analysis of the block at `(x, y)` of size `1 << log2`: the
    /// cost of the cheapest partition and that partition.
    ///
    /// `parent` is the vector the enclosing node found, which seeds this one.
    fn analyse(&mut self, x: usize, y: usize, log2: u32, parent: [i16; 2]) -> Result<(f64, Tree)> {
        let n = 1usize << log2;
        let inside = x + n <= self.w && y + n <= self.h;
        let must_split = !inside || log2 > 5;
        let floor = if must_split {
            MIN_CB_LOG2
        } else {
            self.k.min_log2
        };
        // The node's own vector first, so its children can start from it.
        let leaf = if must_split {
            None
        } else {
            Some(self.search_node(x, y, n, parent)?)
        };
        // Early termination: a block that the seeds predict almost exactly is coded
        // whole, without searching its children.
        let near_exact =
            leaf.filter(|(c, _)| log2 > floor && *c < self.k.split_sad * (n * n) as f64);
        if let Some((leaf_cost, l)) = near_exact {
            return Ok((leaf_cost, Tree::Leaf(l)));
        }
        let child_seed = leaf.map_or(parent, |(_, l)| l.mv);
        let mut split_cost = 0f64;
        let mut kids = Vec::new();
        if log2 > floor {
            for (dx, dy) in child_offsets(n / 2) {
                if x + dx < self.w && y + dy < self.h {
                    let (c, t) = self.analyse(x + dx, y + dy, log2 - 1, child_seed)?;
                    split_cost += c;
                    kids.push(t);
                }
            }
        }
        match leaf {
            Some((leaf_cost, l)) if !(log2 > floor && split_cost < leaf_cost) => {
                Ok((leaf_cost, Tree::Leaf(l)))
            }
            _ => Ok((split_cost, Tree::Split(kids))),
        }
    }

    /// Code the partition `tree` of the block at `(x, y)`.
    fn code_tree(
        &mut self,
        x: usize,
        y: usize,
        log2: u32,
        tree: &Tree,
        e: &mut CabacEncoder,
    ) -> Result<()> {
        let n = 1usize << log2;
        let inside = x + n <= self.w && y + n <= self.h;
        match tree {
            Tree::Leaf(leaf) => {
                if log2 > 3 && inside {
                    self.write_split(e, x, y, log2, false);
                }
                self.code_cu(x, y, log2, *leaf, e)
            }
            Tree::Split(kids) => {
                if log2 > 3 && inside {
                    self.write_split(e, x, y, log2, true);
                }
                let mut it = kids.iter();
                for (dx, dy) in child_offsets(n / 2) {
                    if x + dx < self.w && y + dy < self.h {
                        let kid = it.next().ok_or_else(|| {
                            Error::corrupt("HEVC P encode: partition has too few children")
                        })?;
                        self.code_tree(x + dx, y + dy, log2 - 1, kid, e)?;
                    }
                }
                Ok(())
            }
        }
    }

    /// Choose the prediction of one coding unit by rate-distortion, then code it.
    fn code_cu(
        &mut self,
        x: usize,
        y: usize,
        log2: u32,
        analysis: LeafMotion,
        e: &mut CabacEncoder,
    ) -> Result<()> {
        let n = 1usize << log2;
        let analysis_mv = analysis.mv;
        let geom = CuGeometry {
            x,
            y,
            size: n,
            part_mode: PartMode::Part2Nx2N,
            skip: false,
        };
        // Candidate motions: each distinct merge candidate, then AMVP with the
        // analysed vector under either predictor.
        let mut merge_motions: Vec<(u8, PbMotion)> = Vec::new();
        for idx in 0..MAX_MERGE_CAND as u8 {
            let pu = PredictionUnit {
                x,
                y,
                width: n,
                height: n,
                merge_flag: true,
                merge_idx: idx,
                ..PredictionUnit::default()
            };
            let m = {
                let sm = self.sm();
                derive_merge(&sm, &geom, 0, &pu)
            };
            if merge_motions.iter().all(|(_, prev)| *prev != m) {
                merge_motions.push((idx, m));
            }
        }
        let mut amvp = Vec::with_capacity(2);
        for flag in 0..2u8 {
            let pu = PredictionUnit {
                x,
                y,
                width: n,
                height: n,
                mvp_flag: [flag, 0],
                ..PredictionUnit::default()
            };
            let mvp = {
                let sm = self.sm();
                derive_amvp(&sm, &pu).mv[0]
            };
            let mvd = [
                i32::from(analysis_mv[0]) - i32::from(mvp[0]),
                i32::from(analysis_mv[1]) - i32::from(mvp[1]),
            ];
            amvp.push((
                flag,
                mvd,
                PbMotion::new([true, false], [0, -1], [analysis_mv, [0, 0]]),
            ));
        }

        // An AMVP candidate that predicts with the very motion of a merge candidate
        // codes the same samples for more bits (a vector difference against a merge
        // index), so merge has already costed it.
        if !self.k.amvp_both && amvp.len() == 2 {
            let size = |m: &(u8, [i32; 2], PbMotion)| m.1[0].abs() + m.1[1].abs();
            let keep = usize::from(size(&amvp[1]) < size(&amvp[0]));
            amvp = vec![amvp.swap_remove(keep)];
        }
        amvp.retain(|(_, _, m)| merge_motions.iter().all(|(_, mm)| mm != m));
        let contexts = e.contexts.clone();
        // The chosen prediction: the motion of each half (both equal for a square
        // unit), and the coded unit.
        let mut best: Option<(f64, [PbMotion; 2], Eval)> = None;
        for (idx, motion) in merge_motions.into_iter().take(self.k.merge_max) {
            let (cost, eval) =
                self.eval_candidate(x, y, log2, motion, Mode::Merge(idx), &contexts)?;
            // A residual-free merge that predicts the block almost exactly is taken
            // at once: the later merge candidates are not costed.
            let skip_hit = eval.syntax.skip && cost < self.k.skip_cost * (n * n) as f64;
            if best.as_ref().is_none_or(|(c, _, _)| cost < *c) {
                best = Some((cost, [motion; 2], eval));
            }
            if skip_hit {
                break;
            }
        }
        // Early skip: a merge candidate with no residual that predicts the block
        // almost exactly ends the search. The other modes cost time for no gain.
        let early_skip = best
            .as_ref()
            .is_some_and(|(c, _, eval)| eval.syntax.skip && *c < self.k.skip_cost * (n * n) as f64);
        if !early_skip {
            for (flag, mvd, motion) in amvp {
                let (cost, eval) =
                    self.eval_candidate(x, y, log2, motion, Mode::Amvp { flag, mvd }, &contexts)?;
                if best.as_ref().is_none_or(|(c, _, _)| cost < *c) {
                    best = Some((cost, [motion; 2], eval));
                }
            }
            // Intra candidates: the most probable modes, then the fixed set.
            let mpm = self.intra_mpm(x, y);
            let mut modes = mpm.to_vec();
            for m in INTRA_FIXED_MODES {
                if !modes.contains(&m) {
                    modes.push(m);
                }
            }
            modes.truncate(self.k.intra_modes);
            let inter_best = best.as_ref().map_or(f64::MAX, |(c, _, _)| *c);
            if inter_best < self.k.intra_gate * (n * n) as f64 {
                modes.clear();
            } else if self.k.intra_k < modes.len() {
                let mut ranked: Vec<(u64, u8)> = modes
                    .iter()
                    .map(|&m| {
                        self.intra_pred(x, y, log2, m);
                        (self.luma_sad(x, y, n), m)
                    })
                    .collect();
                ranked.sort_unstable();
                modes = ranked
                    .into_iter()
                    .take(self.k.intra_k)
                    .map(|(_, m)| m)
                    .collect();
            }
            let no_motion = PbMotion::new([false, false], [-1, -1], [[0, 0], [0, 0]]);
            for mode in modes {
                let (cost, eval) = self.eval_intra(x, y, log2, mode, mpm, &contexts)?;
                if best.as_ref().is_none_or(|(c, _, _)| cost < *c) {
                    best = Some((cost, [no_motion; 2], eval));
                }
            }
            // The split the analysis chose, with its own vectors.
            if analysis.part != Part::Square {
                let (cost, motions, eval) =
                    self.eval_partition(x, y, log2, analysis.part, analysis.pmvs, &contexts)?;
                if best.as_ref().is_none_or(|(c, _, _)| cost < *c) {
                    best = Some((cost, motions, eval));
                }
            }
        }
        let (_, motions, eval) =
            best.ok_or_else(|| Error::corrupt("HEVC P encode: no candidate for a coding unit"))?;
        self.write_cu(e, x, y, log2, &eval.syntax);
        self.commit(x, y, log2, motions, eval);
        Ok(())
    }

    /// Sum of absolute differences of the luma `n` x `n` block at `(x, y)` between
    /// the source and `self.pred`.
    fn luma_sad(&self, x: usize, y: usize, n: usize) -> u64 {
        let mut sum = 0u64;
        for r in 0..n {
            let at = (y + r) * self.w + x;
            for c in 0..n {
                let d = i32::from(self.src.y[at + c]) - i32::from(self.pred[0][at + c]);
                sum += u64::from(d.unsigned_abs());
            }
        }
        sum
    }

    /// Predict the `w` x `h` block at `(x, y)` with `motion` into `self.pred`.
    fn predict_rect(
        &mut self,
        (x, y): (usize, usize),
        (w, h): (usize, usize),
        motion: &PbMotion,
    ) -> Result<()> {
        let refs: [&[RefPicture<'_>]; 2] = [std::slice::from_ref(&self.ref_pic), &[]];
        let [py, pcb, pcr] = &mut self.pred;
        let mut outs = [
            PlaneMut {
                samples: &mut py[..],
                stride: self.w,
                bit_depth: self.bd,
                origin: 0,
            },
            PlaneMut {
                samples: &mut pcb[..],
                stride: self.w / 2,
                bit_depth: self.bd,
                origin: 0,
            },
            PlaneMut {
                samples: &mut pcr[..],
                stride: self.w / 2,
                bit_depth: self.bd,
                origin: 0,
            },
        ];
        predict_pu(
            refs,
            &motion.to_motion(),
            None,
            Block {
                x,
                y,
                width: w,
                height: h,
            },
            &mut outs,
            &mut self.mc,
        )
    }

    /// Predict and quantise a coding unit split as `part`, each half with its own
    /// vector (AMVP, the predictor the shorter difference picks), and its four
    /// half-size transform units (the split an inter unit takes when it is not
    /// 2Nx2N and the depth is zero). Costs it as `SSD + lambda * bits`.
    fn eval_partition(
        &mut self,
        x: usize,
        y: usize,
        log2: u32,
        part: Part,
        pmvs: [[i16; 2]; 2],
        contexts: &Contexts,
    ) -> Result<(f64, [PbMotion; 2], Eval)> {
        let n = 1usize << log2;
        let rects = partition_rects(x, y, n, part);
        let mut motions = [PbMotion::new([false, false], [-1, -1], [[0, 0], [0, 0]]); 2];
        let mut pus = [([0i32; 2], 0u8); 2];
        for (i, (rx, ry, rw, rh)) in rects.into_iter().enumerate() {
            let mut pick: Option<(i32, [i32; 2], u8)> = None;
            for flag in 0..2u8 {
                let pu = PredictionUnit {
                    x: rx,
                    y: ry,
                    width: rw,
                    height: rh,
                    mvp_flag: [flag, 0],
                    ..PredictionUnit::default()
                };
                let mvp = {
                    let sm = self.sm();
                    derive_amvp(&sm, &pu).mv[0]
                };
                let mvd = [
                    i32::from(pmvs[i][0]) - i32::from(mvp[0]),
                    i32::from(pmvs[i][1]) - i32::from(mvp[1]),
                ];
                let size = mvd[0].abs() + mvd[1].abs();
                if pick.is_none_or(|(s, _, _)| size < s) {
                    pick = Some((size, mvd, flag));
                }
            }
            let (_, mvd, flag) = pick.unwrap_or((0, [0, 0], 0));
            let motion = PbMotion::new([true, false], [0, -1], [pmvs[i], [0, 0]]);
            pus[i] = (mvd, flag);
            motions[i] = motion;
            // The first half is stored before the second derives its predictor,
            // as the decoder sees it.
            self.field.store(rx, ry, rw, rh, 0, motion);
            self.predict_rect((rx, ry), (rw, rh), &motion)?;
        }
        // The field holds this unit's motion only while it is being costed.
        self.field.clear(x, y, n, n);

        let src: &'a PaddedSource = self.src;
        let sh = self.sign_hiding;
        let qp_c = self.qpc;
        let half = n / 2;
        let mut recon = [
            vec![0u16; n * n],
            vec![0u16; n * n / 4],
            vec![0u16; n * n / 4],
        ];
        let mut tus = Vec::with_capacity(4);
        let mut ssd = 0f64;
        for (dx, dy) in child_offsets(half) {
            let (cx, cy) = (x + dx, y + dy);
            let luma = code_tb(
                &src.y,
                self.w,
                &self.pred[0],
                self.w,
                (cx, cy),
                half,
                self.qpd,
                self.bd,
                sh,
                (0, false),
                self.k.zero_alpha,
            );
            let cb = code_tb(
                &src.cb,
                self.w / 2,
                &self.pred[1],
                self.w / 2,
                (cx / 2, cy / 2),
                half / 2,
                qp_c,
                self.bd,
                sh,
                (0, false),
                self.k.zero_alpha,
            );
            let cr = code_tb(
                &src.cr,
                self.w / 2,
                &self.pred[2],
                self.w / 2,
                (cx / 2, cy / 2),
                half / 2,
                qp_c,
                self.bd,
                sh,
                (0, false),
                self.k.zero_alpha,
            );
            ssd += luma.ssd + cb.ssd + cr.ssd;
            for r in 0..half {
                recon[0][(dy + r) * n + dx..][..half]
                    .copy_from_slice(&luma.recon[r * half..][..half]);
            }
            for r in 0..half / 2 {
                let at = (dy / 2 + r) * (n / 2) + dx / 2;
                recon[1][at..][..half / 2].copy_from_slice(&cb.recon[r * half / 2..][..half / 2]);
                recon[2][at..][..half / 2].copy_from_slice(&cr.recon[r * half / 2..][..half / 2]);
            }
            tus.push(TuSyn {
                cbf: [luma.cbf, cb.cbf, cr.cbf],
                levels: [luma.levels, cb.levels, cr.levels],
            });
        }
        let cbf = [
            tus.iter().any(|t| t.cbf[0]),
            tus.iter().any(|t| t.cbf[1]),
            tus.iter().any(|t| t.cbf[2]),
        ];
        let syntax = CuSyntax {
            skip: false,
            intra: None,
            part,
            pus,
            tus,
            merge: None,
            mvd: [0, 0],
            mvp_flag: 0,
            root: cbf.iter().any(|&c| c),
            cbf,
            levels: [Vec::new(), Vec::new(), Vec::new()],
        };
        let mut counter = CabacEncoder::counter(contexts.clone());
        counter.set_estimating(true);
        self.write_cu(&mut counter, x, y, log2, &syntax);
        let cost = ssd + self.lambda * counter.bit_count_f64();
        let eval = Eval { syntax, recon };
        Ok((cost, motions, eval))
    }

    /// Predict one candidate motion, quantise its residual and cost it:
    /// `SSD + lambda * bits` of its syntax under the coder state `contexts`.
    fn eval_candidate(
        &mut self,
        x: usize,
        y: usize,
        log2: u32,
        motion: PbMotion,
        mode: Mode,
        contexts: &Contexts,
    ) -> Result<(f64, Eval)> {
        let n = 1usize << log2;
        self.predict_rect((x, y), (n, n), &motion)?;
        let src: &'a PaddedSource = self.src;
        let qp_c = self.qpc;
        let sh = self.sign_hiding;
        let luma = code_tb(
            &src.y,
            self.w,
            &self.pred[0],
            self.w,
            (x, y),
            n,
            self.qpd,
            self.bd,
            sh,
            (0, false),
            self.k.zero_alpha,
        );
        let cb = code_tb(
            &src.cb,
            self.w / 2,
            &self.pred[1],
            self.w / 2,
            (x / 2, y / 2),
            n / 2,
            qp_c,
            self.bd,
            sh,
            (0, false),
            self.k.zero_alpha,
        );
        let cr = code_tb(
            &src.cr,
            self.w / 2,
            &self.pred[2],
            self.w / 2,
            (x / 2, y / 2),
            n / 2,
            qp_c,
            self.bd,
            sh,
            (0, false),
            self.k.zero_alpha,
        );
        let cbf = [luma.cbf, cb.cbf, cr.cbf];
        let any = cbf.iter().any(|&c| c);
        let syntax = match mode {
            Mode::Merge(idx) if !any => CuSyntax {
                skip: true,
                intra: None,
                part: Part::Square,
                pus: [([0, 0], 0); 2],
                tus: Vec::new(),
                merge: Some(idx),
                mvd: [0, 0],
                mvp_flag: 0,
                root: false,
                cbf,
                levels: [Vec::new(), Vec::new(), Vec::new()],
            },
            Mode::Merge(idx) => CuSyntax {
                skip: false,
                intra: None,
                part: Part::Square,
                pus: [([0, 0], 0); 2],
                tus: Vec::new(),
                merge: Some(idx),
                mvd: [0, 0],
                mvp_flag: 0,
                root: true,
                cbf,
                levels: [luma.levels.clone(), cb.levels.clone(), cr.levels.clone()],
            },
            Mode::Amvp { flag, mvd } => CuSyntax {
                skip: false,
                intra: None,
                part: Part::Square,
                pus: [([0, 0], 0); 2],
                tus: Vec::new(),
                merge: None,
                mvd,
                mvp_flag: flag,
                root: any,
                cbf,
                levels: [luma.levels.clone(), cb.levels.clone(), cr.levels.clone()],
            },
        };
        let mut counter = CabacEncoder::counter(contexts.clone());
        counter.set_estimating(true);
        self.write_cu(&mut counter, x, y, log2, &syntax);
        let ssd = luma.ssd + cb.ssd + cr.ssd;
        let cost = ssd + self.lambda * counter.bit_count_f64();
        let eval = Eval {
            syntax,
            recon: [luma.recon, cb.recon, cr.recon],
        };
        Ok((cost, eval))
    }

    /// `candModeList` of the coding unit at `(x, y)` (8.4.2): the left and above
    /// units' luma modes, where a unit that is not intra, or lies outside the
    /// picture or slice, counts as DC, and so does the row above a CTB.
    fn intra_mpm(&self, x: usize, y: usize) -> [u8; 3] {
        let mode_at = |xn: isize, yn: isize| -> Option<u8> {
            if !self.pic.available(x, y, xn, yn) {
                return None;
            }
            let info = &self.pic.info[self.pic.unit(xn as usize, yn as usize)];
            info.intra.then_some(info.intra_mode)
        };
        let left = mode_at(x as isize - 1, y as isize);
        let ctb_top = y & ((1 << self.ctb_log2) - 1) == 0;
        let above = if ctb_top {
            None
        } else {
            mode_at(x as isize, y as isize - 1)
        };
        mpm_list(left, above)
    }

    /// Copy the decoder's intra prediction of the `1 << log2` coding unit at
    /// `(x, y)` in `mode` from the reconstruction planes into `self.pred`.
    fn intra_pred(&mut self, x: usize, y: usize, log2: u32, mode: u8) {
        let n = 1usize << log2;
        let (w, cw) = (self.w, self.w / 2);
        self.pic.predict_intra(0, x, y, log2, mode);
        self.pic.predict_intra(1, x / 2, y / 2, log2 - 1, mode);
        self.pic.predict_intra(2, x / 2, y / 2, log2 - 1, mode);
        for r in 0..n {
            let at = (y + r) * w + x;
            self.pred[0][at..][..n].copy_from_slice(&self.pic.planes.y[at..][..n]);
        }
        for r in 0..n / 2 {
            let at = (y / 2 + r) * cw + x / 2;
            self.pred[1][at..][..n / 2].copy_from_slice(&self.pic.planes.cb[at..][..n / 2]);
            self.pred[2][at..][..n / 2].copy_from_slice(&self.pic.planes.cr[at..][..n / 2]);
        }
    }

    /// Predict and quantise an intra coding unit in `mode` (chroma follows the
    /// luma mode), and cost it as `SSD + lambda * bits` like an inter candidate.
    fn eval_intra(
        &mut self,
        x: usize,
        y: usize,
        log2: u32,
        mode: u8,
        mpm: [u8; 3],
        contexts: &Contexts,
    ) -> Result<(f64, Eval)> {
        let n = 1usize << log2;
        self.intra_pred(x, y, log2, mode);
        let src: &'a PaddedSource = self.src;
        let sh = self.sign_hiding;
        let qp_c = self.qpc;
        let scan = scan_index(mode, log2, true);
        let cscan = scan_index(mode, log2 - 1, false);
        let luma = code_tb(
            &src.y,
            self.w,
            &self.pred[0],
            self.w,
            (x, y),
            n,
            self.qpd,
            self.bd,
            sh,
            (scan, n == 4),
            self.k.zero_alpha,
        );
        let cb = code_tb(
            &src.cb,
            self.w / 2,
            &self.pred[1],
            self.w / 2,
            (x / 2, y / 2),
            n / 2,
            qp_c,
            self.bd,
            sh,
            (cscan, false),
            self.k.zero_alpha,
        );
        let cr = code_tb(
            &src.cr,
            self.w / 2,
            &self.pred[2],
            self.w / 2,
            (x / 2, y / 2),
            n / 2,
            qp_c,
            self.bd,
            sh,
            (cscan, false),
            self.k.zero_alpha,
        );
        let syntax = CuSyntax {
            skip: false,
            intra: Some(IntraCu { mode, mpm }),
            part: Part::Square,
            pus: [([0, 0], 0); 2],
            tus: Vec::new(),
            merge: None,
            mvd: [0, 0],
            mvp_flag: 0,
            root: true,
            cbf: [luma.cbf, cb.cbf, cr.cbf],
            levels: [luma.levels.clone(), cb.levels.clone(), cr.levels.clone()],
        };
        let mut counter = CabacEncoder::counter(contexts.clone());
        counter.set_estimating(true);
        self.write_cu(&mut counter, x, y, log2, &syntax);
        let ssd = luma.ssd + cb.ssd + cr.ssd;
        let cost = ssd + self.lambda * counter.bit_count_f64();
        let eval = Eval {
            syntax,
            recon: [luma.recon, cb.recon, cr.recon],
        };
        Ok((cost, eval))
    }

    /// Write the intra part of a coding unit (7.3.8.5 with MODE_INTRA, 2Nx2N):
    /// the luma mode as `prev_intra_luma_pred_flag` with `mpm_idx` or the
    /// remainder, `intra_chroma_pred_mode` 4 (the luma mode), then the transform
    /// tree at depth 0 with no split.
    fn write_intra_cu(&self, e: &mut CabacEncoder, log2: u32, s: &CuSyntax, im: IntraCu) {
        e.encode_bin(ctx::PRED_MODE, 1); // MODE_INTRA
        if log2 == MIN_CB_LOG2 {
            e.encode_bin(ctx::PART_MODE, 1); // PART_2Nx2N
        }
        let is_mpm = im.mpm.contains(&im.mode);
        e.encode_bin(ctx::PREV_INTRA_LUMA_PRED, u32::from(is_mpm));
        match im.mpm.iter().position(|&m| m == im.mode) {
            Some(idx) => {
                // mpm_idx: truncated rice, cMax 2, all bypass.
                e.encode_bypass(u32::from(idx > 0));
                if idx > 0 {
                    e.encode_bypass(u32::from(idx == 2));
                }
            }
            None => {
                // rem_intra_luma_pred_mode: the index among the non-candidates.
                let mut sorted = im.mpm;
                sorted.sort_unstable();
                let mut rem = im.mode;
                for &m in sorted.iter().rev() {
                    if rem > m {
                        rem -= 1;
                    }
                }
                e.encode_bypass_bits(u32::from(rem), 5);
            }
        }
        e.encode_bin(ctx::INTRA_CHROMA_PRED_MODE, 0); // 4: the luma mode
        // split_transform_flag at depth 0, coded when the block is between the
        // minimum and maximum transform sizes (MaxTrafoDepth is one).
        if self.intra_tt_split && (3..=5).contains(&log2) {
            e.encode_bin(ctx::SPLIT_TRANSFORM + (5 - log2) as usize, 0);
        }
        let scan = scan_index(im.mode, log2, true);
        let cscan = scan_index(im.mode, log2 - 1, false);
        let [luma, cb, cr] = s.cbf;
        e.encode_bin(ctx::CBF_CHROMA, u32::from(cb));
        e.encode_bin(ctx::CBF_CHROMA, u32::from(cr));
        e.encode_bin(ctx::CBF_LUMA + 1, u32::from(luma));
        if luma {
            self.put_residual(e, &s.levels[0], log2, 0, scan);
        }
        if cb {
            self.put_residual(e, &s.levels[1], log2 - 1, 1, cscan);
        }
        if cr {
            self.put_residual(e, &s.levels[2], log2 - 1, 2, cscan);
        }
    }

    /// Write an inter coding unit split into two halves (7.3.8.5, AMVP halves): the
    /// part mode (2NxN as `01`, Nx2N as `00`, with AMP off), the two AMVP
    /// prediction units, `rqt_root_cbf`, then the four half-size transform units
    /// of the inferred split, whose chroma flags are the depth-zero ones.
    fn write_split_cu(&self, e: &mut CabacEncoder, log2: u32, s: &CuSyntax) {
        e.encode_bin(ctx::PRED_MODE, 0); // MODE_INTER
        e.encode_bin(ctx::PART_MODE, 0);
        e.encode_bin(ctx::PART_MODE + 1, u32::from(s.part == Part::Horizontal));
        for (mvd, flag) in s.pus {
            e.encode_bin(ctx::MERGE_FLAG, 0);
            write_mvd(e, mvd);
            e.encode_bin(ctx::MVP_FLAG, u32::from(flag));
        }
        e.encode_bin(ctx::RQT_ROOT_CBF, u32::from(s.root));
        if !s.root {
            return;
        }
        let cb0 = s.tus.iter().any(|t| t.cbf[1]);
        let cr0 = s.tus.iter().any(|t| t.cbf[2]);
        e.encode_bin(ctx::CBF_CHROMA, u32::from(cb0));
        e.encode_bin(ctx::CBF_CHROMA, u32::from(cr0));
        for t in &s.tus {
            if cb0 {
                e.encode_bin(ctx::CBF_CHROMA + 1, u32::from(t.cbf[1]));
            }
            if cr0 {
                e.encode_bin(ctx::CBF_CHROMA + 1, u32::from(t.cbf[2]));
            }
            e.encode_bin(ctx::CBF_LUMA, u32::from(t.cbf[0]));
            if t.cbf[0] {
                self.put_residual(e, &t.levels[0], log2 - 1, 0, 0);
            }
            if cb0 && t.cbf[1] {
                self.put_residual(e, &t.levels[1], log2 - 2, 1, 0);
            }
            if cr0 && t.cbf[2] {
                self.put_residual(e, &t.levels[2], log2 - 2, 2, 0);
            }
        }
    }

    /// Code one block's residual; while the engine only estimates a rate, the closed-form
    /// level estimate stands in for the per-bin walk.
    fn put_residual(
        &self,
        e: &mut CabacEncoder,
        levels: &[i32],
        log2: u32,
        c_idx: usize,
        scan: usize,
    ) {
        if e.is_estimating() && self.k.res_proxy {
            e.add_estimated_bits(estimate_residual_bits(levels, log2, scan));
        } else {
            encode_residual(e, levels, log2, c_idx, scan, self.sign_hiding, None);
        }
    }

    /// Write one coding unit's syntax (7.3.8.5 with 2Nx2N inter units).
    fn write_cu(&self, e: &mut CabacEncoder, x: usize, y: usize, log2: u32, s: &CuSyntax) {
        e.encode_bin(ctx::CU_SKIP + self.skip_ctx(x, y), u32::from(s.skip));
        if s.skip {
            if let Some(idx) = s.merge {
                write_merge_idx(e, idx);
            }
            return;
        }
        if let Some(im) = s.intra {
            self.write_intra_cu(e, log2, s, im);
            return;
        }
        if s.part != Part::Square {
            self.write_split_cu(e, log2, s);
            return;
        }
        e.encode_bin(ctx::PRED_MODE, 0); // MODE_INTER
        e.encode_bin(ctx::PART_MODE, 1); // PART_2Nx2N
        match s.merge {
            Some(idx) => {
                e.encode_bin(ctx::MERGE_FLAG, 1);
                write_merge_idx(e, idx);
            }
            None => {
                e.encode_bin(ctx::MERGE_FLAG, 0);
                write_mvd(e, s.mvd);
                e.encode_bin(ctx::MVP_FLAG, u32::from(s.mvp_flag));
                e.encode_bin(ctx::RQT_ROOT_CBF, u32::from(s.root));
                if !s.root {
                    return;
                }
            }
        }
        // transform_tree at depth 0: no split (max_transform_hierarchy_depth_inter
        // is 0 and the block is at most one maximum transform), so the cbf flags
        // of this depth and one transform unit.
        let [luma, cb, cr] = s.cbf;
        e.encode_bin(ctx::CBF_CHROMA, u32::from(cb));
        e.encode_bin(ctx::CBF_CHROMA, u32::from(cr));
        // cbf_luma is inferred to be 1 when both chroma flags are 0.
        if cb || cr {
            e.encode_bin(ctx::CBF_LUMA + 1, u32::from(luma));
        }
        if luma {
            self.put_residual(e, &s.levels[0], log2, 0, 0);
        }
        if cb {
            self.put_residual(e, &s.levels[1], log2 - 1, 1, 0);
        }
        if cr {
            self.put_residual(e, &s.levels[2], log2 - 1, 2, 0);
        }
    }

    /// Record a committed coding unit: its samples, motion, and the per-unit
    /// facts the context selection and the deblocking filter read.
    fn commit(&mut self, x: usize, y: usize, log2: u32, motions: [PbMotion; 2], eval: Eval) {
        let n = 1usize << log2;
        let (w, cw) = (self.w, self.w / 2);
        let [ry, rcb, rcr] = eval.recon;
        for r in 0..n {
            self.pic.planes.y[(y + r) * w + x..][..n].copy_from_slice(&ry[r * n..][..n]);
        }
        for r in 0..n / 2 {
            let at = (y / 2 + r) * cw + x / 2;
            self.pic.planes.cb[at..][..n / 2].copy_from_slice(&rcb[r * n / 2..][..n / 2]);
            self.pic.planes.cr[at..][..n / 2].copy_from_slice(&rcr[r * n / 2..][..n / 2]);
        }
        let intra_mode = eval.syntax.intra.map(|im| im.mode);
        let part = eval.syntax.part;
        let rects = partition_rects(x, y, n, part);
        if intra_mode.is_none() {
            for ((rx, ry, rw, rh), motion) in rects.into_iter().zip(motions) {
                self.field.store(rx, ry, rw, rh, 0, motion);
            }
        }
        let depth = (self.ctb_log2 - log2) as u8;
        let skip = eval.syntax.skip;
        let nz = eval.syntax.cbf[0];
        let qp = self.qp as i8;
        self.pic.update_info(x, y, n, n, |b| {
            b.intra = intra_mode.is_some();
            b.skip = skip;
            b.depth = depth;
            b.intra_mode = intra_mode.unwrap_or(1);
            b.qp = qp;
            b.nz_luma = nz;
        });
        self.pic.mark_edges(x, y, n, n, EDGE_TU | EDGE_PU);
        if part != Part::Square {
            // The four transform units of a split unit: their own luma flags, and
            // their edges, which the deblocking filter reads.
            for ((dx, dy), tu) in child_offsets(n / 2).into_iter().zip(&eval.syntax.tus) {
                let nz_tu = tu.cbf[0];
                self.pic
                    .update_info(x + dx, y + dy, n / 2, n / 2, |b| b.nz_luma = nz_tu);
            }
            self.pic.mark_edges(x + n / 2, y, n / 2, n, EDGE_TU);
            self.pic.mark_edges(x, y + n / 2, n, n / 2, EDGE_TU);
            let (rx, ry, rw, rh) = rects[1];
            self.pic.mark_edges(rx, ry, rw, rh, EDGE_PU);
        }
    }

    /// The deblocking filter over the finished picture, with the motion the
    /// decoder's filter reads; then the reconstruction and the motion field.
    fn finish_picture(mut self) -> (PRecon, MotionField) {
        let (w4, h4) = (self.pic.w4, self.pic.h4);
        for by in 0..h4 {
            for bx in 0..w4 {
                let at = by * w4 + bx;
                self.pic.motion[at] = match self.field.at((bx * 4) as isize, (by * 4) as isize) {
                    Some(cell) => PuMotion {
                        pred: cell.motion.pred,
                        ref_pic: [
                            if cell.motion.pred[0] {
                                self.ref_poc
                            } else {
                                -1
                            },
                            -1,
                        ],
                        mv: [to32(cell.motion.mv[0]), to32(cell.motion.mv[1])],
                    },
                    None => PuMotion::default(),
                };
            }
        }
        if self.deblock {
            deblock_picture(&mut self.pic);
        }
        let recon = PRecon {
            y: std::mem::take(&mut self.pic.planes.y),
            cb: std::mem::take(&mut self.pic.planes.cb),
            cr: std::mem::take(&mut self.pic.planes.cr),
        };
        (recon, self.field)
    }
}

/// How a candidate's motion was derived, which decides its syntax.
#[derive(Debug, Clone, Copy)]
enum Mode {
    /// `merge_idx` of the merge list.
    Merge(u8),
    /// AMVP: `mvp_lX_flag` and the vector difference.
    Amvp { flag: u8, mvd: [i32; 2] },
}

/// The four child origins of a node, offsets in z-order.
fn child_offsets(half: usize) -> [(usize, usize); 4] {
    [(0, 0), (half, 0), (0, half), (half, half)]
}

/// The P slice header (7.3.6.1) for a TRAIL_R picture: one reference, the
/// previous picture, named by an explicit short-term RPS, the slice QP, and
/// with a wavefront the entry points of its CTB-row substreams.
fn write_p_header(poc: i32, qp: i32, entry_points: Option<&[u32]>) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_bit(true); // first_slice_segment_in_pic_flag
    w.write_ue(1); // slice_pic_parameter_set_id: the P picture parameter set
    w.write_ue(1); // slice_type: P
    w.write_bits((poc & 0xff) as u32, 8); // slice_pic_order_cnt_lsb
    w.write_bit(false); // short_term_ref_pic_set_sps_flag: explicit RPS
    // st_ref_pic_set(0): no prediction from an earlier set, one negative picture
    // at distance 1, no positive pictures.
    w.write_ue(1); // num_negative_pics
    w.write_ue(0); // num_positive_pics
    w.write_ue(0); // delta_poc_s0_minus1
    w.write_bit(true); // used_by_curr_pic_s0_flag
    w.write_bit(true); // slice_temporal_mvp_enabled_flag
    w.write_bit(false); // num_ref_idx_active_override_flag: one reference
    w.write_ue(0); // five_minus_max_num_merge_cand
    w.write_se(qp - 26); // slice_qp_delta
    if let Some(offsets) = entry_points {
        // entropy_coding_sync_enabled_flag: num_entry_point_offsets, then each
        // offset minus one in offset_len_minus1 + 1 bits (ceil_log2 of the largest + 1).
        w.write_ue(offsets.len() as u32);
        if let Some(&max) = offsets.iter().max() {
            let len = (32 - max.leading_zeros()).max(1);
            w.write_ue(len - 1);
            for &offset in offsets {
                w.write_bits(offset.saturating_sub(1), len);
            }
        }
    }
    w.write_bit(true); // byte_alignment(): the one bit
    w.align_to_byte();
    w.into_bytes()
}

/// Code one P picture from the padded source and the reference, returning its
/// access unit and its reconstruction.
#[allow(clippy::too_many_arguments)] // one picture's inputs and outputs
pub(crate) fn code_p_picture(
    enc: &Encoder,
    src: &PaddedSource,
    refp: &PRecon,
    rp: &mut RefPlanes,
    ref_poc: i32,
    poc: i32,
    qp: i32,
    col: Option<PrevPic>,
) -> Result<(Vec<u8>, PRecon, PrevPic)> {
    let col = col.map(Arc::new);
    rp.fill(&refp.y, enc.threads);
    let rp = &*rp;
    let (substreams, recon, field) = if enc.cfg.inter_wpp {
        code_p_wavefront(enc, src, refp, rp, ref_poc, poc, qp, col)?
    } else {
        code_p_sequential(enc, src, refp, rp, ref_poc, poc, qp, col)?
    };
    let entry_points = enc.cfg.inter_wpp.then(|| {
        // Each substream is escaped on its own: it ends on a non-zero byte (its
        // stop bit), so escaping the picture is escaping the substreams in turn.
        let escaped: Vec<u32> = substreams
            .iter()
            .map(|s| {
                let mut out = Vec::with_capacity(s.len() + 8);
                escape_rbsp(s, &mut out);
                out.len() as u32
            })
            .collect();
        escaped[..escaped.len().saturating_sub(1)].to_vec()
    });
    let mut rbsp = write_p_header(poc, qp, entry_points.as_deref());
    for substream in &substreams {
        rbsp.extend_from_slice(substream);
    }
    let (w, h) = (enc.coded_width, enc.coded_height);
    let mut au = Vec::with_capacity(rbsp.len() + 64);
    write_annex_b(
        &mut au,
        NalHeader::new(NalUnitType::Pps),
        &enc.pps_p.to_rbsp(),
        true,
    );
    write_annex_b(&mut au, NalHeader::new(NalUnitType::TrailR), &rbsp, true);
    let shared = Arc::new(SharedPic::new(poc, w, h, (8, 8), false, 1));
    shared.publish_col(&field, h);
    Ok((au, recon, PrevPic { field, shared }))
}

/// The sequential P coder: one CABAC substream for the whole picture, every CTB
/// in raster order on one thread. The `inter_wpp = false` layout.
#[allow(clippy::too_many_arguments)] // one picture's inputs and outputs
fn code_p_sequential(
    enc: &Encoder,
    src: &PaddedSource,
    refp: &PRecon,
    rp: &RefPlanes,
    ref_poc: i32,
    poc: i32,
    qp: i32,
    col: Option<Arc<PrevPic>>,
) -> Result<(Vec<Vec<u8>>, PRecon, MotionField)> {
    let mut pc = PCoder::new(enc, src, refp, rp, ref_poc, poc, qp, col);
    let ctb = 1usize << pc.ctb_log2;
    let (cols, rows) = (pc.w.div_ceil(ctb), pc.h.div_ceil(ctb));
    let mut cab = CabacEncoder::new(Contexts::for_init_type(qp, 1));
    for row in 0..rows {
        for col in 0..cols {
            let (x, y) = (col * ctb, row * ctb);
            let (_, tree) = pc.analyse(x, y, pc.ctb_log2, [0, 0])?;
            pc.code_tree(x, y, pc.ctb_log2, &tree, &mut cab)?;
            let last = row + 1 == rows && col + 1 == cols;
            cab.encode_terminate(u32::from(last));
        }
    }
    let (recon, field) = pc.finish_picture();
    Ok((vec![cab.finish()], recon, field))
}

/// The published state of one finished CTB of a P picture: the bottom eight sample rows,
/// the 4x4 units and the motion cells of its bottom two unit rows. The CTB row below reads
/// these (its neighbours above and above-right, and the deblocking of its top edge) and
/// nothing else of the row above.
struct CtbSnapshot {
    y: Vec<u16>,
    cb: Vec<u16>,
    cr: Vec<u16>,
    info: Vec<BlockInfo>,
    cells: Vec<Option<FieldCell>>,
}

/// Sample rows of a CTB the row below needs: the line its intra prediction reads and the
/// rows its deblocking filter reads across the top edge (8.7.2: eight rows, a multiple of
/// the 8x8 filter grid).
const SNAP_ROWS: usize = 8;

/// The shared state of one wavefront-coded P picture.
struct Wave {
    cols: usize,
    rows: usize,
    ctb: usize,
    ctb_log2: u32,
    qp: i32,
    /// Finished CTBs per row: row `r` has published `progress[r]` of them.
    progress: Vec<Progress>,
    /// The snapshot of CTB `row * cols + col`, stored before its progress.
    snapshots: Vec<Mutex<Option<CtbSnapshot>>>,
    /// `TableStateIdxWpp` of each row: the contexts after its second CTB.
    sync: Vec<Mutex<Option<Contexts>>>,
    /// The CABAC substream of each row.
    substreams: Vec<Mutex<Vec<u8>>>,
    /// Set by a failing worker so the others stop waiting.
    abort: AtomicBool,
    failure: Mutex<Option<Error>>,
}

/// Where a CTB row writes its share of the finished picture: the deblocked luma rows
/// `[y0, y1)` and chroma rows of the row (see [`bands`]), and the motion cells of its unit rows.
struct RowOut<'o> {
    y: &'o mut [u16],
    cb: &'o mut [u16],
    cr: &'o mut [u16],
    cells: &'o mut [Option<FieldCell>],
}

/// The luma and chroma sample rows `(y0, y1, c0, c1)` that CTB row `row` of `rows` owns in
/// the deblocked picture. The filter of a row's top edge rewrites the three luma rows (one
/// chroma row) above it, so a row's output starts that far above its first line and the row
/// above ends there.
fn bands(row: usize, rows: usize, ctb: usize, h: usize) -> (usize, usize, usize, usize) {
    let start = |r: usize| {
        if r == 0 {
            (0, 0)
        } else {
            (r * ctb - 3, r * ctb / 2 - 1)
        }
    };
    let (y0, c0) = start(row);
    let (y1, c1) = if row + 1 == rows {
        (h, h / 2)
    } else {
        start(row + 1)
    };
    (y0, y1, c0, c1)
}

/// The wavefront P coder: CTB rows as substreams, coded by `enc.threads` workers
/// dealt round-robin, a row two CTBs behind the row above (the prediction reach,
/// and the CABAC sync point of 9.3.2.2). Every worker owns full-picture scratch
/// state and imports the CTBs it reads from the row above, so each row sees the
/// same neighbours whichever worker codes it: the output does not depend on the
/// thread count. Each worker also deblocks its own rows (the filter of a row needs
/// the eight sample rows above it, which it has imported) and writes the finished rows
/// straight into the picture.
#[allow(clippy::too_many_arguments)]
fn code_p_wavefront(
    enc: &Encoder,
    src: &PaddedSource,
    refp: &PRecon,
    rp: &RefPlanes,
    ref_poc: i32,
    poc: i32,
    qp: i32,
    col: Option<Arc<PrevPic>>,
) -> Result<(Vec<Vec<u8>>, PRecon, MotionField)> {
    let ctb = enc.cfg.ctb_size;
    let (w, h) = (enc.coded_width, enc.coded_height);
    let (cols, rows) = (w.div_ceil(ctb), h.div_ceil(ctb));
    let wave = Wave {
        cols,
        rows,
        ctb,
        ctb_log2: ctb.trailing_zeros(),
        qp,
        progress: (0..rows).map(|_| Progress(AtomicUsize::new(0))).collect(),
        snapshots: (0..rows * cols).map(|_| Mutex::new(None)).collect(),
        sync: (0..rows).map(|_| Mutex::new(None)).collect(),
        substreams: (0..rows).map(|_| Mutex::new(Vec::new())).collect(),
        abort: AtomicBool::new(false),
        failure: Mutex::new(None),
    };
    let threads = enc.threads.min(rows).max(1);

    let mut recon = PRecon {
        y: vec![0; w * h],
        cb: vec![0; w * h / 4],
        cr: vec![0; w * h / 4],
    };
    let mut field = MotionField::new(w, h);
    field.slices.push(SliceRefs::from_lists(&RefPicLists {
        l0: vec![RefEntry {
            id: 0,
            poc: ref_poc,
            long_term: false,
        }],
        l1: Vec::new(),
    }));
    // The picture split into one output per CTB row, dealt to the workers like the rows.
    let mut per_thread: Vec<Vec<(usize, RowOut<'_>)>> = (0..threads).map(|_| Vec::new()).collect();
    {
        let (mut ry, mut rcb, mut rcr) = (&mut recon.y[..], &mut recon.cb[..], &mut recon.cr[..]);
        let mut cell_bands = field.bands_mut(ctb / 4);
        for row in 0..rows {
            let (y0, y1, c0, c1) = bands(row, rows, ctb, h);
            let (y, rest) = ry.split_at_mut((y1 - y0) * w);
            ry = rest;
            let (cb, rest) = rcb.split_at_mut((c1 - c0) * (w / 2));
            rcb = rest;
            let (cr, rest) = rcr.split_at_mut((c1 - c0) * (w / 2));
            rcr = rest;
            let cells = cell_bands.next().unwrap_or_default();
            per_thread[row % threads].push((row, RowOut { y, cb, cr, cells }));
        }
    }
    std::thread::scope(|scope| {
        for mine in per_thread {
            let wave = &wave;
            let col = col.clone();
            scope.spawn(move || {
                let mut pc = PCoder::new(enc, src, refp, rp, ref_poc, poc, qp, col);
                for (row, out) in mine {
                    if let Err(e) = code_p_row(wave, &mut pc, row, out) {
                        wave.abort.store(true, Ordering::Release);
                        if let Ok(mut slot) = wave.failure.lock() {
                            slot.get_or_insert(e);
                        }
                        return;
                    }
                }
            });
        }
    });
    if let Some(e) = wave.failure.into_inner().unwrap_or_else(|p| p.into_inner()) {
        return Err(e);
    }
    let mut substreams = Vec::with_capacity(rows);
    for stream in &wave.substreams {
        let mut stream = stream
            .lock()
            .map_err(|_| Error::corrupt("HEVC P wavefront: substream lock poisoned"))?;
        substreams.push(std::mem::take(&mut *stream));
    }
    Ok((substreams, recon, field))
}

/// Code CTB row `row` of a wavefront P picture into its substream, publishing
/// each CTB as it finishes, then deblock it and write it to `out`.
fn code_p_row(wave: &Wave, pc: &mut PCoder<'_>, row: usize, out: RowOut<'_>) -> Result<()> {
    let (cols, ctb, qp) = (wave.cols, wave.ctb, wave.qp);
    let (w, h) = (pc.w, pc.h);
    if row > 0 && !wait_for_or_abort(&wave.progress[row - 1], 2.min(cols), &wave.abort) {
        return Ok(());
    }
    // The sync point is the row above's second CTB, so the contexts are read only
    // once that CTB is published.
    let contexts = if row == 0 || cols < 2 {
        Contexts::for_init_type(qp, 1)
    } else {
        wave.sync[row - 1]
            .lock()
            .ok()
            .and_then(|c| c.clone())
            .unwrap_or_else(|| Contexts::for_init_type(qp, 1))
    };
    let mut cab = CabacEncoder::new(contexts);
    for col in 0..cols {
        let (x0, y0) = (col * ctb, row * ctb);
        if row > 0 {
            if !wait_for_or_abort(&wave.progress[row - 1], (col + 2).min(cols), &wave.abort) {
                return Ok(());
            }
            // The CTBs of the row above this one reads: its own column and the next.
            if col == 0 {
                import_above(wave, pc, row - 1, 0)?;
            }
            if col + 1 < cols {
                import_above(wave, pc, row - 1, col + 1)?;
            }
        }
        let (_, tree) = pc.analyse(x0, y0, wave.ctb_log2, [0, 0])?;
        pc.code_tree(x0, y0, wave.ctb_log2, &tree, &mut cab)?;
        let (x1, y1) = ((x0 + ctb).min(w), (y0 + ctb).min(h));
        let snap = pc.export_ctb(x0, y0, x1, y1);
        if let Ok(mut slot) = wave.snapshots[row * cols + col].lock() {
            *slot = Some(snap);
        }
        if col == 1 {
            // 9.3.2.2: the row below syncs from the state after the second CTB.
            if let Ok(mut slot) = wave.sync[row].lock() {
                *slot = Some(cab.contexts.clone());
            }
        }
        wave.progress[row].0.store(col + 1, Ordering::Release);
        let last = row + 1 == wave.rows && col + 1 == cols;
        cab.encode_terminate(u32::from(last));
        if !last && col + 1 == cols {
            // end_of_subset_one_bit, then byte alignment.
            cab.encode_terminate(1);
        }
    }
    let bytes = cab.finish();
    if let Ok(mut slot) = wave.substreams[row].lock() {
        *slot = bytes;
    }
    pc.finish_row(row, wave.rows, out);
    Ok(())
}

/// Copy the published CTB `(row, col)` into the worker's picture.
fn import_above(wave: &Wave, pc: &mut PCoder<'_>, row: usize, col: usize) -> Result<()> {
    let slot = wave.snapshots[row * wave.cols + col]
        .lock()
        .map_err(|_| Error::corrupt("HEVC P wavefront: snapshot lock poisoned"))?;
    let snap = slot
        .as_ref()
        .ok_or_else(|| Error::corrupt("HEVC P wavefront: row above not published"))?;
    let (x0, y0) = (col * wave.ctb, row * wave.ctb);
    let (x1, y1) = ((x0 + wave.ctb).min(pc.w), (y0 + wave.ctb).min(pc.h));
    pc.import_ctb(x0, y0, x1, y1, snap);
    Ok(())
}

/// The `w` by `h` block of `src` at `(x, y)`, row by row.
fn region<T: Copy>(src: &[T], stride: usize, x: usize, y: usize, w: usize, h: usize) -> Vec<T> {
    let mut out = Vec::with_capacity(w * h);
    for r in 0..h {
        out.extend_from_slice(&src[(y + r) * stride + x..][..w]);
    }
    out
}

/// The inverse of [`region`]: write `src` (row by row, `w` wide) at `(x, y)`.
fn place<T: Copy>(dst: &mut [T], stride: usize, x: usize, y: usize, w: usize, src: &[T]) {
    for (r, line) in src.chunks_exact(w).enumerate() {
        dst[(y + r) * stride + x..][..w].copy_from_slice(line);
    }
}

impl PCoder<'_> {
    /// The bottom rows of the finished CTB covering luma `[x0, x1) x [y0, y1)`.
    fn export_ctb(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> CtbSnapshot {
        let _ = y0;
        let (w, w4) = (self.w, self.pic.w4);
        let lw = x1 - x0;
        let ys = y1 - SNAP_ROWS;
        CtbSnapshot {
            y: region(&self.pic.planes.y, w, x0, ys, lw, SNAP_ROWS),
            cb: region(
                &self.pic.planes.cb,
                w / 2,
                x0 / 2,
                ys / 2,
                lw / 2,
                SNAP_ROWS / 2,
            ),
            cr: region(
                &self.pic.planes.cr,
                w / 2,
                x0 / 2,
                ys / 2,
                lw / 2,
                SNAP_ROWS / 2,
            ),
            info: region(&self.pic.info, w4, x0 / 4, ys / 4, lw / 4, SNAP_ROWS / 4),
            cells: self.field.export_cells(x0, ys, lw, SNAP_ROWS),
        }
    }

    /// Write a snapshot of the CTB covering luma `[x0, x1) x [y0, y1)`.
    fn import_ctb(&mut self, x0: usize, y0: usize, x1: usize, y1: usize, snap: &CtbSnapshot) {
        let _ = y0;
        let (w, w4) = (self.w, self.pic.w4);
        let lw = x1 - x0;
        let ys = y1 - SNAP_ROWS;
        place(&mut self.pic.planes.y, w, x0, ys, lw, &snap.y);
        place(
            &mut self.pic.planes.cb,
            w / 2,
            x0 / 2,
            ys / 2,
            lw / 2,
            &snap.cb,
        );
        place(
            &mut self.pic.planes.cr,
            w / 2,
            x0 / 2,
            ys / 2,
            lw / 2,
            &snap.cr,
        );
        place(&mut self.pic.info, w4, x0 / 4, ys / 4, lw / 4, &snap.info);
        self.field.import_cells(x0, ys, lw, SNAP_ROWS, &snap.cells);
    }

    /// Deblock CTB row `row` of `rows` in this worker's picture and write the rows it owns
    /// (see [`bands`]) and its motion cells to `out`. The vertical edges are filtered
    /// from eight rows above the row so that the horizontal filter of its top edge sees the
    /// rows of the row above as the whole-picture filter leaves them; the extra rows of that
    /// pass are not written out.
    fn finish_row(&mut self, row: usize, rows: usize, out: RowOut<'_>) {
        let (w, h, ctb) = (self.w, self.h, 1usize << self.ctb_log2);
        let w4 = self.pic.w4;
        let (f0, f1) = (
            (row * ctb).saturating_sub(SNAP_ROWS),
            ((row + 1) * ctb).min(h),
        );
        if self.deblock {
            for by in f0 / 4..f1 / 4 {
                for bx in 0..w4 {
                    let at = by * w4 + bx;
                    self.pic.motion[at] = match self.field.at((bx * 4) as isize, (by * 4) as isize)
                    {
                        Some(cell) => PuMotion {
                            pred: cell.motion.pred,
                            ref_pic: [
                                if cell.motion.pred[0] {
                                    self.ref_poc
                                } else {
                                    -1
                                },
                                -1,
                            ],
                            mv: [to32(cell.motion.mv[0]), to32(cell.motion.mv[1])],
                        },
                        None => PuMotion::default(),
                    };
                }
            }
            deblock_rows(&mut self.pic, f0..f1);
        }
        let (y0, y1, c0, c1) = bands(row, rows, ctb, h);
        let cw = w / 2;
        out.y.copy_from_slice(&self.pic.planes.y[y0 * w..y1 * w]);
        out.cb
            .copy_from_slice(&self.pic.planes.cb[c0 * cw..c1 * cw]);
        out.cr
            .copy_from_slice(&self.pic.planes.cr[c0 * cw..c1 * cw]);
        let u0 = row * ctb / 4;
        for (i, dst) in out.cells.chunks_exact_mut(w4).enumerate() {
            dst.copy_from_slice(self.field.cell_row(u0 + i));
        }
    }
}

/// Longest GOP (in pictures) coded as a unit by the GOP-parallel path of constant-QP sequences.
const GOP_PARALLEL_MAX_KEYINT: usize = 64;

/// GOPs per batch on the bitrate path. Fixed (not the thread count), so the bytes do not
/// depend on the number of threads; it matches the 6 threads the encoder is tuned for.
const GOP_RC_BATCH: usize = 6;

/// One GOP's pictures and the rate model at its end (bitrate sequences only).
type GopOut = (Vec<EncodedPicture>, Option<BitRateControl>);

/// Join scoped GOP workers, turning a panic into an error.
fn join_all<T>(handles: Vec<std::thread::ScopedJoinHandle<'_, Result<T>>>) -> Vec<Result<T>> {
    handles
        .into_iter()
        .map(|h| {
            h.join()
                .unwrap_or_else(|_| Err(Error::corrupt("HEVC GOP worker panicked")))
        })
        .collect()
}

/// How each GOP of the parallel path picks its QPs.
#[derive(Clone, Copy)]
enum GopMode {
    /// Constant QP: the IDR at this QP, predicted pictures at it plus the P offset.
    Fixed(i32),
    /// Bitrate control, run independently per GOP.
    Bitrate { kbps: u32, fps: f64 },
}

impl Encoder {
    /// Encode a sequence of I420 frames at the configured size: an IDR every
    /// [`EncoderConfig::keyint`](crate::encoder::EncoderConfig::keyint) pictures
    /// and P pictures between them. Each frame is one access unit.
    pub fn encode_sequence(&self, frames: &[VideoFrame]) -> Result<Vec<EncodedPicture>> {
        let mut out = Vec::with_capacity(frames.len());
        self.encode_sequence_with(frames.iter().cloned(), |pic| {
            out.push(pic);
            Ok(())
        })?;
        Ok(out)
    }

    /// Encode a sequence as [`Encoder::encode_sequence`] does, but hand each
    /// picture to `sink` as soon as it is coded, in order. Frames are consumed
    /// one at a time, so a long clip never has to fit in memory.
    pub fn encode_sequence_with<I, F>(&self, frames: I, sink: F) -> Result<()>
    where
        I: IntoIterator<Item = VideoFrame>,
        F: FnMut(EncodedPicture) -> Result<()>,
    {
        let keyint = self.cfg.keyint.max(1);
        if keyint > 256 {
            return Err(Error::unsupported(
                format!("HEVC GOP of {keyint} pictures"),
                "the picture order count is coded in 8 bits, so a GOP holds at most 256 pictures",
            ));
        }
        // Constant QP codes every picture at one quantiser; a bitrate runs the
        // frame-level model, which picks each picture's QP from what the
        // previous pictures cost.
        let fixed_qp = match self.cfg.rate_control {
            RateControl::ConstantQp(q) => Some(q.clamp(self.qp_min(), 51)),
            RateControl::Bitrate { .. } => None,
            RateControl::TargetBits(_) => {
                return Err(Error::unsupported(
                    "HEVC sequence encode at a per-picture bit target",
                    "use RateControl::Bitrate for a sequence",
                ));
            }
        };
        // Constant QP makes every GOP independent (an IDR resets all state), so whole GOPs
        // are coded concurrently and their pictures delivered in order: the stream is the
        // same bytes as the serial coder writes, whatever the thread count.
        //
        // A bitrate makes GOPs depend on each other through the rate model. GOPs then each
        // run their own model, starting from the IDR's default QP with the budget of their
        // own pictures (`base` bits each): the bytes differ from one serial model, but they
        // are the same at every thread count, the path being taken at one thread too.
        if keyint > 1 && keyint <= GOP_PARALLEL_MAX_KEYINT {
            let mode = match self.cfg.rate_control {
                RateControl::ConstantQp(_) if self.threads > 1 => {
                    Some(GopMode::Fixed(fixed_qp.unwrap_or(27)))
                }
                RateControl::Bitrate { kbps, fps } => Some(GopMode::Bitrate { kbps, fps }),
                _ => None,
            };
            if let Some(mode) = mode {
                return self.encode_gops_parallel(frames.into_iter(), keyint, mode, sink);
            }
        }
        let mut rc = match self.cfg.rate_control {
            RateControl::Bitrate { kbps, fps } => Some(BitRateControl::new(
                kbps,
                fps,
                keyint,
                u32::from(self.cfg.bit_depth),
            )),
            _ => None,
        };
        self.run_sequence(frames, keyint, fixed_qp, &mut rc, sink)
    }

    /// Code `frames` (a whole number of GOPs from a keyframe, except possibly the last) in
    /// batches of GOPs on concurrent workers, then hand the pictures to `sink` in order.
    ///
    /// Constant QP: a batch is `threads` GOPs, each on its own wavefront workers. Bitrate: a
    /// batch is [`GOP_RC_BATCH`] GOPs on `threads` lanes, each GOP starting from the rate model
    /// the previous batch ended with and the QP offset that batch's error implies. The batch
    /// size does not depend on the thread count, so the bytes do not either.
    fn encode_gops_parallel<I, F>(
        &self,
        mut frames: I,
        keyint: usize,
        mode: GopMode,
        mut sink: F,
    ) -> Result<()>
    where
        I: Iterator<Item = VideoFrame>,
        F: FnMut(EncodedPicture) -> Result<()>,
    {
        let bd = u32::from(self.cfg.bit_depth);
        let mut carry: Option<BitRateControl> = None;
        let mut offset = 0.0_f64;
        loop {
            // The first bitrate GOP is coded alone: it is the only one with no model to start
            // from, so the batches after it start from a measured complexity.
            let batch_size = match mode {
                GopMode::Fixed(_) => self.threads,
                GopMode::Bitrate { .. } if carry.is_none() => 1,
                GopMode::Bitrate { .. } => GOP_RC_BATCH,
            };
            let mut gops: Vec<Vec<VideoFrame>> = Vec::new();
            while gops.len() < batch_size {
                let gop: Vec<VideoFrame> = frames.by_ref().take(keyint).collect();
                if gop.is_empty() {
                    break;
                }
                gops.push(gop);
            }
            if gops.is_empty() {
                return Ok(());
            }
            let results: Vec<Result<GopOut>> = match mode {
                GopMode::Fixed(qp) => {
                    let per_gop = self.threads.div_ceil(gops.len()).max(1);
                    std::thread::scope(|scope| {
                        let handles: Vec<_> = gops
                            .into_iter()
                            .map(|gop| {
                                let enc = self.with_threads(per_gop);
                                scope.spawn(move || {
                                    let mut out = Vec::with_capacity(gop.len());
                                    let mut none = None;
                                    enc.run_sequence(gop, keyint, Some(qp), &mut none, |pic| {
                                        out.push(pic);
                                        Ok(())
                                    })?;
                                    Ok((out, None))
                                })
                            })
                            .collect();
                        join_all(handles)
                    })
                }
                GopMode::Bitrate { kbps, fps } => {
                    let n_gops = gops.len();
                    let lanes = self.threads.min(n_gops).max(1);
                    let per_gop = (self.threads / lanes).max(1);
                    let carry_ref = carry.as_ref();
                    std::thread::scope(|scope| {
                        // Lane `l` codes GOPs `l`, `l + lanes`, ... in order.
                        let mut work: Vec<Vec<(usize, Vec<VideoFrame>)>> =
                            (0..lanes).map(|_| Vec::new()).collect();
                        for (i, gop) in gops.into_iter().enumerate() {
                            work[i % lanes].push((i, gop));
                        }
                        let handles: Vec<_> = work
                            .into_iter()
                            .map(|mine| {
                                let enc = self.with_threads(per_gop);
                                scope.spawn(move || {
                                    let mut done = Vec::with_capacity(mine.len());
                                    for (i, gop) in mine {
                                        let mut rc = BitRateControl::new(kbps, fps, keyint, bd);
                                        if let Some(prev) = carry_ref {
                                            rc.continue_from(prev, offset);
                                        }
                                        let mut slot = Some(rc);
                                        let mut out = Vec::with_capacity(gop.len());
                                        enc.run_sequence(gop, keyint, None, &mut slot, |pic| {
                                            out.push(pic);
                                            Ok(())
                                        })?;
                                        done.push((i, out, slot));
                                    }
                                    Ok(done)
                                })
                            })
                            .collect();
                        let lanes_done = join_all(handles);
                        let mut by_gop: Vec<Option<Result<GopOut>>> =
                            (0..n_gops).map(|_| None).collect();
                        for lane in lanes_done {
                            match lane {
                                Ok(items) => {
                                    for (i, out, slot) in items {
                                        by_gop[i] = Some(Ok((out, slot)));
                                    }
                                }
                                Err(e) => return vec![Err(e)],
                            }
                        }
                        by_gop.into_iter().flatten().collect()
                    })
                }
            };
            let mut batch: Vec<GopOut> = Vec::new();
            for gop in results {
                batch.push(gop?);
            }
            if let GopMode::Bitrate { kbps, fps } = mode {
                let bits: f64 = batch
                    .iter()
                    .flat_map(|(pics, _)| pics)
                    .map(|p| p.au.len() as f64 * 8.0)
                    .sum();
                let pictures: usize = batch.iter().map(|(pics, _)| pics.len()).sum();
                let budget = pictures as f64 * bitrate_picture_bits(kbps, fps) as f64;
                offset = offset_after_batch(offset, bits, budget);
                carry = batch.last().and_then(|(_, rc)| rc.clone());
            }
            for (pics, _) in batch {
                for pic in pics {
                    sink(pic)?;
                }
            }
        }
    }

    /// The sequence loop: an IDR at every `keyint`-th frame, P pictures between.
    fn run_sequence<I, F>(
        &self,
        frames: I,
        keyint: usize,
        fixed_qp: Option<i32>,
        rc: &mut Option<BitRateControl>,
        mut sink: F,
    ) -> Result<()>
    where
        I: IntoIterator<Item = VideoFrame>,
        F: FnMut(EncodedPicture) -> Result<()>,
    {
        let mut prev: Option<PRecon> = None;
        let mut prev_pic: Option<PrevPic> = None;
        let mut ref_planes = RefPlanes::new(
            self.coded_width,
            self.coded_height,
            u32::from(self.cfg.bit_depth),
        );
        for (i, frame) in frames.into_iter().enumerate() {
            let want = self.frame_format();
            if frame.format != want {
                return Err(Error::unsupported(
                    format!("HEVC encode of {:?}", frame.format),
                    format!("this encoder codes {want:?} at {} bits", self.cfg.bit_depth),
                ));
            }
            if frame.width != self.cfg.width || frame.height != self.cfg.height {
                return Err(Error::corrupt(format!(
                    "HEVC encode: frame is {}x{}, encoder configured for {}x{}",
                    frame.width, frame.height, self.cfg.width, self.cfg.height
                )));
            }
            let sixteen = self.cfg.bit_depth == 10;
            let (y, ys) = plane_samples(&frame.planes[0], sixteen);
            let (cb, cbs) = plane_samples(&frame.planes[1], sixteen);
            let (cr, crs) = plane_samples(&frame.planes[2], sixteen);
            let padded = self.pad_source(&y, ys, &cb, cbs, &cr, crs)?;
            let poc = (i % keyint) as i32;
            let idr = poc == 0;
            // The coded QP of this picture. A fixed QP keeps the P offset on P
            // pictures; the model's QP already carries it.
            let qp = match (rc.as_ref(), fixed_qp) {
                (Some(model), _) => model.frame_qp(idr),
                (None, Some(q)) if idr => q,
                (None, Some(q)) => (q + P_QP_DELTA).clamp(self.qp_min(), 51),
                (None, None) => 27,
            };
            if idr {
                let (coded, y, cb, cr) = self.code_picture_planes(&padded, qp)?;
                if let Some(model) = rc.as_mut() {
                    model.update(true, qp, coded.au.len() as u64 * 8);
                }
                prev = Some(PRecon { y, cb, cr });
                prev_pic = None;
                sink(coded)?;
            } else {
                let refp = prev.take().ok_or_else(|| {
                    Error::corrupt("HEVC P encode: no reference picture before a P picture")
                })?;
                let col = prev_pic.take();
                let (au, recon, pp) =
                    code_p_picture(self, &padded, &refp, &mut ref_planes, poc - 1, poc, qp, col)?;
                if let Some(model) = rc.as_mut() {
                    model.update(false, qp, au.len() as u64 * 8);
                }
                prev_pic = Some(pp);
                let recon_frame = self
                    .cfg
                    .keep_recon
                    .then(|| self.crop_recon(&recon.y, &recon.cb, &recon.cr));
                prev = Some(recon);
                sink(EncodedPicture {
                    au,
                    qp,
                    recon: recon_frame,
                })?;
            }
        }
        Ok(())
    }
}

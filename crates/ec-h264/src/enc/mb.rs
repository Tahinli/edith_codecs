//! Macroblock coding: mode decision, quantisation, reconstruction and syntax.
//!
//! Reconstruction here is the decoder's own: prediction comes from
//! [`crate::pred`] and [`crate::inter`], dequantisation and the inverse
//! transforms from [`crate::transform`], and the metadata every block writes
//! into [`crate::dpb::Picture`] is what the decoder writes at the same point.
//! The encoder therefore cannot drift from the decoder by construction — the
//! only thing it adds is the *choice* of mode and the forward quantiser.

// A block index is simultaneously a bitstream position, a geometry key
// (BLK4_POS) and a grid coordinate here, exactly as in the decoder; iterating
// the arrays instead would hide which geometry each loop names.
#![allow(clippy::needless_range_loop)]

use ec_h264_syntax::SliceType;
use wide::i16x16;

use crate::deblock::{edge_params, filter_luma_h_edge16, filter_luma_line};
use crate::decoder::{
    chroma_nz_pair, gather_nbr4, gather_nbr8, luma_nz_pair, mb_neighbors, MbNeighbors,
};
use crate::dpb::{Picture, BLK_SKIP};
use crate::entropy::{
    BlockCat, MbCtx, MbInfo, FLAG_CHROMA_PRED, FLAG_DECODED, FLAG_I16, FLAG_INTER, FLAG_SKIP,
    FLAG_TRANS8X8,
};
use crate::inter::{integer_origin, mc_chroma, mc_luma, RefPlane};
use crate::mv::{neighbour_mvd, predict_mv, write_block, write_intra_mb, write_mvd, MvCtx};
use crate::pred::{
    add_residual_4x4, add_residual_8x8, filter_nbr8, pred_16x16, pred_4x4, pred_8x8,
    pred_chroma_8x8, PlaneWindow,
};
use crate::tables::{BLK4_POS, CHROMA_QP};
use crate::transform::{
    chroma_dc_transform_420, dequant_4x4, dequant_8x8, inverse_transform_4x4,
    inverse_transform_8x8, luma_dc_transform, unzigzag, unzigzag_8x8, unzigzag_ac15, LevelScale4x4,
    LevelScale8x8,
};

use super::quant::{
    forward_4x4, forward_8x8, forward_hadamard_2x2, forward_hadamard_4x4, quant_4x4, quant_8x8,
    quant_chroma_dc, quant_luma_dc,
};
use super::{
    entropy::{sub_block_4x4, EncEntropy},
    lambda_ssd,
};

/// Speed/quality ladder. Two rungs, because the two the incumbent exposed are
/// the two edith actually picks between.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Preset {
    /// Diamond motion search, 16x16 partitions.
    #[default]
    Fast,
    /// Wider search with a hexagon refinement and half-macroblock partitions.
    Balanced,
}

/// Source planes of one picture, edge-padded out to whole macroblocks.
pub(crate) struct Source {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    /// Luma row pitch, equal to the coded width.
    pub stride: usize,
    /// Chroma row pitch.
    pub c_stride: usize,
}

/// Everything one macroblock's coding reads and updates.
pub(crate) struct MbEnc<'a> {
    pub src: &'a Source,
    /// The single list-0 reference, absent in an I slice.
    pub reference: Option<&'a Picture>,
    pub slice_type: SliceType,
    pub slice_id: u16,
    /// QPY of the previous macroblock: the prediction chain of 7.4.5.
    pub qp: i32,
    /// What rate control wants this macroblock coded at.
    pub target_qp: i32,
    /// Lagrangian multiplier; it is in the squared-error domain when 8x8
    /// transform decisions are enabled.
    pub lambda: f64,
    /// Whether `lambda` is the standard squared-error multiplier.
    pub lambda_standard: bool,
    /// Extra rate saving an 8x8 trial must buy before it may replace 4x4.
    pub t8x8_margin_bits: f64,
    pub preset: Preset,
    /// The PPS carries transform_8x8_mode_flag: transform_size_8x8_flag must
    /// be written for every eligible macroblock (7.3.5).
    pub transform_8x8: bool,
    /// Whether intra macroblocks may select 8x8. The PPS can remain enabled
    /// while an ablation measures the classes independently.
    pub transform_8x8_intra: bool,
    /// Whether inter macroblocks may select 8x8.
    pub transform_8x8_inter: bool,
    /// Whether the ablation restricts 8x8 eligibility to even macroblock
    /// rows; odd rows then code exactly as they would with 8x8 disabled.
    pub transform_8x8_roweven: bool,
    pub ls: LevelScale4x4,
    pub ls8: LevelScale8x8,
    /// Neighbourhood of the macroblock being written (CABAC).
    pub mb_ctx: MbCtx,
    /// ctxIdxInc for this macroblock's mb_skip_flag.
    pub skip_inc: usize,
    /// `qp_delta_inc` of 9.3.3.1.1.5: whether the previous macroblock in this
    /// slice carried a non-zero mb_qp_delta.
    pub qp_delta_inc: u8,
}

impl MbEnc<'_> {
    /// Intra 8x8 eligibility of macroblock row `mb_y`.
    fn t8x8_intra(&self, mb_y: usize) -> bool {
        self.transform_8x8_intra && (mb_y % 2 == 0 || !self.transform_8x8_roweven)
    }

    /// Inter 8x8 eligibility of macroblock row `mb_y`.
    fn t8x8_inter(&self, mb_y: usize) -> bool {
        self.transform_8x8_inter && (mb_y % 2 == 0 || !self.transform_8x8_roweven)
    }
}

/// Quantised levels of one macroblock, scan order per block.
#[derive(Clone)]
struct Levels {
    /// Luma AC/4x4 blocks in Z-order; only `[..15]` is used under Intra_16x16.
    luma: [[i32; 16]; 16],
    /// Non-zero count per luma block, Z-order.
    luma_nz: [u8; 16],
    /// transform_size_8x8_flag: the luma residual is `luma8`, not `luma`.
    trans8: bool,
    /// Luma 8x8 blocks in 8x8 zig-zag order.
    luma8: [[i32; 64]; 4],
    /// Intra_16x16 luma DC, scan order.
    dc: [i32; 16],
    /// Chroma DC per component.
    chroma_dc: [[i32; 16]; 2],
    /// Chroma AC per component and block.
    chroma: [[[i32; 16]; 4]; 2],
    chroma_nz: [[u8; 4]; 2],
    cbp_luma: u8,
    cbp_chroma: u8,
}

impl Default for Levels {
    fn default() -> Levels {
        Levels {
            luma: [[0; 16]; 16],
            luma_nz: [0; 16],
            trans8: false,
            luma8: [[0; 64]; 4],
            dc: [0; 16],
            chroma_dc: [[0; 16]; 2],
            chroma: [[[0; 16]; 4]; 2],
            chroma_nz: [[0; 4]; 2],
            cbp_luma: 0,
            cbp_chroma: 0,
        }
    }
}

/// 4x4 Hadamard sum of absolute transformed differences between the source at
/// `(sx, sy)` and a 4x4 prediction.
fn satd4(src: &[u8], sstride: usize, sx: usize, sy: usize, pred: &[u8], pstride: usize) -> i32 {
    let mut d = [0i32; 16];
    for y in 0..4 {
        for x in 0..4 {
            d[y * 4 + x] =
                i32::from(src[(sy + y) * sstride + sx + x]) - i32::from(pred[y * pstride + x]);
        }
    }
    for i in 0..4 {
        let o = i * 4;
        let (a, b, c, e) = (d[o], d[o + 1], d[o + 2], d[o + 3]);
        let (s0, s1, s2, s3) = (a + b, c + e, a - b, c - e);
        d[o] = s0 + s1;
        d[o + 1] = s0 - s1;
        d[o + 2] = s2 + s3;
        d[o + 3] = s2 - s3;
    }
    let mut sum = 0;
    for j in 0..4 {
        let (a, b, c, e) = (d[j], d[4 + j], d[8 + j], d[12 + j]);
        let (s0, s1, s2, s3) = (a + b, c + e, a - b, c - e);
        sum += (s0 + s1).abs() + (s0 - s1).abs() + (s2 + s3).abs() + (s2 - s3).abs();
    }
    (sum + 2) >> 2
}

/// SATD of a whole 16x16 luma macroblock against the prediction already in the
/// picture plane.
fn satd16(
    src: &[u8],
    sstride: usize,
    sx: usize,
    sy: usize,
    plane: &[u8],
    pstride: usize,
    po: usize,
) -> i32 {
    satd_block(src, sstride, sx, sy, plane, pstride, po, 16, 16)
}

/// The same over a `w` x `h` partition.
#[allow(clippy::too_many_arguments)]
fn satd_block(
    src: &[u8],
    sstride: usize,
    sx: usize,
    sy: usize,
    plane: &[u8],
    pstride: usize,
    po: usize,
    w: usize,
    h: usize,
) -> i32 {
    let mut sum = 0;
    for by in 0..h / 4 {
        for bx in 0..w / 4 {
            sum += satd4(
                src,
                sstride,
                sx + bx * 4,
                sy + by * 4,
                &plane[po + by * 4 * pstride + bx * 4..],
                pstride,
            );
        }
    }
    sum
}

/// Sum of absolute differences of a `w` x `h` block against a prediction with
/// its own pitch. This is the motion search's inner loop and nothing else in
/// the encoder is called as often, so a full-width row goes through one vector.
#[allow(clippy::too_many_arguments)]
fn sad(
    src: &[u8],
    sstride: usize,
    sx: usize,
    sy: usize,
    pred: &[u8],
    pstride: usize,
    w: usize,
    h: usize,
) -> i32 {
    if w == 16 {
        let mut acc = i16x16::ZERO;
        for y in 0..h {
            let a = load16(src, (sy + y) * sstride + sx);
            let b = load16(pred, y * pstride);
            acc += (a - b).abs();
        }
        return acc.to_array().iter().map(|&v| i32::from(v)).sum();
    }
    let mut sum = 0;
    for y in 0..h {
        let s = &src[(sy + y) * sstride + sx..];
        let p = &pred[y * pstride..];
        for x in 0..w {
            sum += (i32::from(s[x]) - i32::from(p[x])).abs();
        }
    }
    sum
}

/// Sixteen samples widened to 16-bit lanes; the tail past the slice is zero,
/// which only a caller reading past the plane could reach.
#[inline]
fn load16(data: &[u8], at: usize) -> i16x16 {
    let mut a = [0i16; 16];
    if let Some(row) = data.get(at..at + 16) {
        for (o, &b) in a.iter_mut().zip(row) {
            *o = i16::from(b);
        }
    }
    i16x16::from(a)
}

/// Bits an exp-Golomb `se(v)` costs, for the motion-vector cost term.
#[inline]
pub(crate) fn se_bits(v: i32) -> i32 {
    let k = if v > 0 { 2 * v - 1 } else { -2 * v } as u32;
    (32 - (k + 1).leading_zeros()) as i32 * 2 - 1
}

/// Chroma QP of a luma QP under a zero `chroma_qp_index_offset`.
#[inline]
fn chroma_qp(qp: i32) -> i32 {
    i32::from(CHROMA_QP[qp.clamp(0, 51) as usize])
}

/// Which intra 4x4 modes the available neighbours allow (8.3.1.2).
fn modes_allowed(have_top: bool, have_left: bool, have_tl: bool) -> [bool; 9] {
    [
        have_top,
        have_left,
        true,
        have_top,
        have_top && have_left && have_tl,
        have_top && have_left && have_tl,
        have_top && have_left && have_tl,
        have_top,
        have_left,
    ]
}

#[inline]
fn motion_rate(e: &MbEnc<'_>, bits: f64) -> i32 {
    let lambda = if e.lambda_standard {
        e.lambda.sqrt()
    } else {
        e.lambda
    };
    (lambda * bits).round() as i32
}

/// Code one macroblock: decide its mode, reconstruct it into `pic` and write
/// its syntax into `w`.
pub(crate) fn encode_mb(pic: &mut Picture, e: &mut MbEnc<'_>, w: &mut EncEntropy, mb_addr: usize) {
    let mb_x = mb_addr % pic.mb_w;
    let mb_y = mb_addr / pic.mb_w;
    let nbr = mb_neighbors(pic, mb_x, mb_y, e.slice_id);
    // The neighbourhood CABAC reads, captured before anything is written.
    let info = |addr: usize| MbInfo {
        flags: pic.mb_flags[addr],
        cbp: pic.mb_cbp[addr],
        dc_cbf: pic.mb_dc_cbf[addr],
    };
    e.mb_ctx = MbCtx {
        a: nbr.a.then(|| info(mb_addr - 1)),
        b: nbr.b.then(|| info(mb_addr - pic.mb_w)),
        qp_delta_inc: e.qp_delta_inc,
    };
    // ctxIdxInc for mb_skip_flag (9.3.3.1.1.1).
    e.skip_inc = usize::from(nbr.a && pic.mb_flags[mb_addr - 1] & FLAG_SKIP == 0)
        + usize::from(nbr.b && pic.mb_flags[mb_addr - pic.mb_w] & FLAG_SKIP == 0);

    // Clear this macroblock's motion state, as the decoder does before parsing.
    let w4 = pic.mb_w * 4;
    for py in 0..4 {
        let base = (mb_y * 4 + py) * w4 + mb_x * 4;
        for idx in base..base + 4 {
            pic.mv[idx] = [[0; 2]; 2];
            pic.ref_idx[idx] = [-1; 2];
            pic.ref_id[idx] = [-1; 2];
            pic.mvd_abs[idx] = [0; 4];
            pic.blk[idx] = 0;
        }
    }

    let mut inter: Option<InterChoice> = None;
    if e.slice_type == SliceType::P {
        inter = Some(choose_inter(pic, e, mb_x, mb_y));
    }

    // Intra cost against the inter one: a P macroblock goes intra only when it
    // is clearly cheaper, because an intra macroblock in a P picture also costs
    // the next picture's prediction quality. Skipping the intra trial when the
    // inter cost is already low was measured and dropped: it cost 0.7 dB on
    // screen capture and bought no measurable time.
    let intra_cost = intra_pre_cost(pic, e, &nbr, mb_x, mb_y);
    let go_intra = match &inter {
        None => true,
        Some(i) => intra_cost.best() + motion_rate(e, 8.0) < i.cost,
    };

    if go_intra {
        encode_intra_mb(pic, e, w, mb_addr, nbr, intra_cost);
    } else {
        let choice = inter.expect("a P macroblock has an inter choice");
        encode_inter_mb(pic, e, w, mb_addr, nbr, choice);
    }
}

/// The Intra_16x16 mode this macroblock would use, and what it would cost:
/// the *inter or intra* decision needs a number before anything is coded, and
/// this is the cheap one. Intra_16x16 against Intra_4x4 is decided later, on
/// real rate and real distortion (see [`encode_intra_mb`]).
struct IntraCost {
    i16_mode: u8,
    i16_cost: i32,
    /// True when Intra_4x4 may be used at all for this picture and preset.
    allow_i4: bool,
}

impl IntraCost {
    fn best(&self) -> i32 {
        // Intra_4x4 usually beats this when it is allowed; the margin below is
        // what keeps a P macroblock from going intra on the 16x16 cost alone.
        if self.allow_i4 {
            self.i16_cost * 3 / 4
        } else {
            self.i16_cost
        }
    }
}

/// Best Intra_16x16 mode by SATD, leaving nothing behind in the plane that a
/// later branch does not overwrite.
fn intra_pre_cost(
    pic: &mut Picture,
    e: &MbEnc<'_>,
    nbr: &MbNeighbors,
    mb_x: usize,
    mb_y: usize,
) -> IntraCost {
    let (sx, sy) = (mb_x * 16, mb_y * 16);
    let stride = pic.y.stride;
    let origin = pic.y.at(sx, sy);
    let mut best = (i32::MAX, 2u8);
    for mode in [2u8, 0, 1, 3] {
        let ok = match mode {
            0 => nbr.b,
            1 => nbr.a,
            3 => nbr.a && nbr.b && nbr.d,
            _ => true,
        };
        if !ok {
            continue;
        }
        let mut win = PlaneWindow {
            data: &mut pic.y.data,
            stride,
            origin,
        };
        pred_16x16(mode, &mut win, nbr.b, nbr.a);
        let cost = satd16(&e.src.y, e.src.stride, sx, sy, &pic.y.data, stride, origin);
        if cost < best.0 {
            best = (cost, mode);
        }
    }
    IntraCost {
        i16_mode: best.1,
        i16_cost: best.0,
        // Intra_4x4 is tried in P pictures too, at every preset. Measured
        // against x264 on a 3840x1608 clip from the library (BD-PSNR over QP
        // 22/26/30/34, matched features): restricting it to I pictures cost
        // -2.992 dB against x264 where allowing it everywhere reads -1.557 dB,
        // for 5% more encode time. Nothing else in the Fast/Balanced split
        // came close -- a 48-sample search range read -3.039 dB and the
        // half-macroblock partitions -3.009 dB, both inside the noise of the
        // -2.992 dB baseline.
        allow_i4: true,
    }
}

/// How a P macroblock is partitioned, in the mb_type numbering of Table 7-13.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PShape {
    /// P_L0_16x16.
    Whole,
    /// P_L0_L0_16x8: a top and a bottom half.
    Horizontal,
    /// P_L0_L0_8x16: a left and a right half.
    Vertical,
}

impl PShape {
    /// mb_type of Table 7-13.
    fn mb_type(self) -> u32 {
        match self {
            PShape::Whole => 0,
            PShape::Horizontal => 1,
            PShape::Vertical => 2,
        }
    }

    fn parts(self) -> usize {
        usize::from(self != PShape::Whole) + 1
    }

    /// Partition `part`'s origin in 4x4 blocks and its size in luma samples.
    fn geometry(self, part: usize) -> ((usize, usize), (usize, usize)) {
        match self {
            PShape::Whole => ((0, 0), (16, 16)),
            PShape::Horizontal => ((0, part * 2), (16, 8)),
            PShape::Vertical => ((part * 2, 0), (8, 16)),
        }
    }

    /// `MbPartWidth`/`MbPartHeight` in 4x4 blocks, which select the
    /// directional shortcuts of 8.4.1.3.
    fn part_blocks(self) -> (usize, usize) {
        match self {
            PShape::Whole => (4, 4),
            PShape::Horizontal => (4, 2),
            PShape::Vertical => (2, 4),
        }
    }
}

/// The inter decision for a P macroblock.
struct InterChoice {
    shape: PShape,
    /// One motion vector per partition.
    mv: [[i16; 2]; 2],
    /// The P_Skip motion vector of 8.4.1.1.
    skip_mv: [i16; 2],
    cost: i32,
    /// The skip candidate is at least as good as the searched one.
    prefer_skip: bool,
}

/// Motion search plus the skip candidate.
fn choose_inter(pic: &Picture, e: &MbEnc<'_>, mb_x: usize, mb_y: usize) -> InterChoice {
    let reference = e.reference.expect("a P slice has a reference");
    let ctx = MvCtx {
        mb_x,
        mb_y,
        slice_id: e.slice_id,
        written: 0,
    };
    let n = ctx.neighbours(pic, 0, 0, 4, 0);
    let mvp = predict_mv(&n, 0, 4, 4, 0);
    // P_Skip (8.4.1.1): zero when either neighbour is missing or is itself a
    // zero-motion reference-0 partition.
    let a = ctx.at(pic, mb_x as i32 * 4 - 1, mb_y as i32 * 4, 0);
    let b = ctx.at(pic, mb_x as i32 * 4, mb_y as i32 * 4 - 1, 0);
    let zero = |nb: &crate::mv::Nb| nb.ref_idx == 0 && nb.mv == [0, 0];
    let skip_mv = if !a.avail || !b.avail || zero(&a) || zero(&b) {
        [0, 0]
    } else {
        mvp
    };

    let plane = RefPlane {
        data: &reference.y.data,
        stride: reference.y.stride,
        origin: reference.y.origin,
        width: reference.y.width,
        height: reference.y.height,
        pad: reference.y.pad,
    };
    let (sx, sy) = (mb_x * 16, mb_y * 16);
    let sw = e.src.stride;
    let range = match e.preset {
        Preset::Fast => 16i16,
        Preset::Balanced => 48,
    };

    // One search over a `w` x `h` partition at `(px, py)` samples inside the
    // macroblock, seeded with the candidate vectors of its neighbourhood.
    let search = |px: usize, py: usize, w: usize, h: usize, mvp: [i16; 2], seeds: &[[i16; 2]]| {
        let (x0, y0) = (sx + px, sy + py);
        let mut buf = [0u8; 256];
        // Whole-sample candidates read the reference plane in place; only the
        // sub-sample refinement pays for interpolation. The search runs on SAD,
        // which is what a search can afford; the winner is re-costed on SATD
        // because the intra candidates are SATD and the two scales differ.
        let mut cost_of = |mv: [i16; 2], satd: bool| -> i32 {
            let bits = motion_rate(
                e,
                f64::from(se_bits(i32::from(mv[0] - mvp[0])) + se_bits(i32::from(mv[1] - mvp[1])))
                    / 2.0,
            );
            if !satd && mv[0] & 3 == 0 && mv[1] & 3 == 0 {
                let o = integer_origin(
                    &plane,
                    x0 as i32 + (mv[0] >> 2) as i32,
                    y0 as i32 + (mv[1] >> 2) as i32,
                    w,
                    h,
                );
                return sad(&e.src.y, sw, x0, y0, &plane.data[o..], plane.stride, w, h) + bits;
            }
            mc_luma(&plane, x0 as i32, y0 as i32, mv, w, h, w, &mut buf);
            let d = if satd {
                satd_block(&e.src.y, sw, x0, y0, &buf, w, 0, w, h)
            } else {
                sad(&e.src.y, sw, x0, y0, &buf, w, w, h)
            };
            d + bits
        };

        let mut best = ([0i16; 2], i32::MAX);
        for &cand in seeds {
            let c = cost_of(cand, false);
            if c < best.1 {
                best = (cand, c);
            }
        }
        // A partition the prediction already fits within half a level a sample
        // is not searched further: on real content most of them are, and the
        // search is the encoder's hot loop.
        let good_enough = (w * h / 2) as i32;
        if best.1 > good_enough {
            let mut step = 4i16 * 4; // four whole samples, in quarter units
            while step >= 4 {
                let mut improved = true;
                while improved {
                    improved = false;
                    for (dx, dy) in [(step, 0), (-step, 0), (0, step), (0, -step)] {
                        let cand = [best.0[0] + dx, best.0[1] + dy];
                        if (cand[0] - mvp[0]).abs() > range * 4
                            || (cand[1] - mvp[1]).abs() > range * 4
                        {
                            continue;
                        }
                        let c = cost_of(cand, false);
                        if c < best.1 {
                            best = (cand, c);
                            improved = true;
                        }
                    }
                }
                step /= 2;
            }
            for step in [2i16, 1] {
                let mut improved = true;
                while improved {
                    improved = false;
                    for (dx, dy) in [
                        (step, 0),
                        (-step, 0),
                        (0, step),
                        (0, -step),
                        (step, step),
                        (-step, -step),
                        (step, -step),
                        (-step, step),
                    ] {
                        let cand = [best.0[0] + dx, best.0[1] + dy];
                        let c = cost_of(cand, false);
                        if c < best.1 {
                            best = (cand, c);
                            improved = true;
                        }
                    }
                    if e.preset == Preset::Fast {
                        break;
                    }
                }
            }
        }
        (best.0, cost_of(best.0, true))
    };

    let seeds = [mvp, skip_mv, [0, 0], n[0].mv, n[1].mv, n[2].mv];
    let (mv16, cost16) = search(0, 0, 16, 16, mvp, &seeds);
    let skip_cost = {
        let mut buf = [0u8; 256];
        mc_luma(&plane, sx as i32, sy as i32, skip_mv, 16, 16, 16, &mut buf);
        satd_block(&e.src.y, sw, sx, sy, &buf, 16, 0, 16, 16) - motion_rate(e, 4.0)
    };

    let mut best = (PShape::Whole, [mv16, mv16], cost16);
    // Halves, seeded with the whole-macroblock winner. The predictor used here
    // is the whole-macroblock one rather than each half's own: the real
    // predictors are derived in coding order when the choice is made, and this
    // only has to rank the shapes. A macroblock the 16x16 search already fits
    // is left alone — splitting it could only spend bits.
    //
    // Measured: two more searches per macroblock buy 0.1-0.2 dB on screen
    // capture and nothing on camera content, for roughly twice the search
    // time. That is a Balanced trade, not a Fast one.
    if e.preset == Preset::Balanced && cost16 > 16 * 16 {
        for shape in [PShape::Horizontal, PShape::Vertical] {
            let mut total = motion_rate(e, 6.0); // the partition's own signalling
            let mut mvs = [[0i16; 2]; 2];
            for part in 0..2 {
                let ((bx, by), (w, h)) = shape.geometry(part);
                let seeds = [mv16, mvp, skip_mv, [0, 0]];
                let (mv, cost) = search(bx * 4, by * 4, w, h, mvp, &seeds);
                mvs[part] = mv;
                total += cost;
            }
            if total < best.2 {
                best = (shape, mvs, total);
            }
        }
    }

    InterChoice {
        shape: best.0,
        mv: best.1,
        skip_mv,
        cost: best.2.min(skip_cost),
        prefer_skip: best.0 == PShape::Whole && skip_cost <= best.2,
    }
}

/// Quantise and reconstruct the luma residual of a non-Intra_16x16 macroblock,
/// prediction already in the plane.
fn code_luma_4x4(
    pic: &mut Picture,
    e: &MbEnc<'_>,
    mb_x: usize,
    mb_y: usize,
    qp: i32,
    intra: bool,
    lv: &mut Levels,
) {
    let stride = pic.y.stride;
    for blk in 0..16 {
        let (dx, dy) = BLK4_POS[blk];
        let (x, y) = (mb_x * 16 + dx as usize * 4, mb_y * 16 + dy as usize * 4);
        let origin = pic.y.at(x, y);
        let mut d = [0i32; 16];
        for ry in 0..4 {
            for rx in 0..4 {
                d[ry * 4 + rx] = i32::from(e.src.y[(y + ry) * e.src.stride + x + rx])
                    - i32::from(pic.y.data[origin + ry * stride + rx]);
            }
        }
        let source = d;
        forward_4x4(&mut d);
        let nz = quant_4x4(&d, qp, intra, false, &mut lv.luma[blk]);
        lv.luma_nz[blk] = nz;
        if nz == 0 {
            continue;
        }
        let mut raster = [0i32; 16];
        unzigzag(&lv.luma[blk], &mut raster);
        dequant_4x4(&mut raster, &e.ls, qp, false);
        let mut resid = [0i32; 16];
        inverse_transform_4x4(&raster, &mut resid);
        // Is the block worth its bits? A block whose residual barely moves the
        // reconstruction is pure quantisation churn; dropping it is the cheap
        // half of rate-distortion optimised quantisation, and the half that
        // pays. Intra blocks are left alone: their reconstruction is what the
        // next block in the macroblock predicts from.
        if !intra
            && !worth_coding(
                &source,
                &resid,
                &lv.luma[blk],
                zero_block_lambda(e, qp, e.t8x8_inter(mb_y)),
            )
        {
            lv.luma[blk] = [0; 16];
            lv.luma_nz[blk] = 0;
            continue;
        }
        lv.cbp_luma |= 1 << (blk >> 2);
        add_residual_4x4(&mut pic.y.data, stride, origin, &resid);
    }
}

/// How much of the mode-decision lambda the zero-block test uses.
///
/// [`block_bits`] counts a level at its exp-Golomb-ish width, which is what
/// CAVLC spends and an over-estimate of what CABAC spends; the multiplier
/// brings the rate term back to the coder actually in force. Measured over
/// 0.0, 0.4 and 1.0 on 1080p camera and screen-capture clips: 0.4 is the only
/// one that gains on screen content (+0.13 to +0.39 dB at matched bitrate)
/// without costing camera content.
const ZERO_BLOCK_LAMBDA: f64 = 0.4;

fn zero_block_lambda(e: &MbEnc<'_>, qp: i32, transform_8x8: bool) -> f64 {
    if transform_8x8 {
        e.lambda
    } else {
        ZERO_BLOCK_LAMBDA * lambda_ssd(qp)
    }
}

/// The rate-distortion test behind the per-block zero decision: coding the
/// block has to buy more squared error than its bits are worth.
fn worth_coding(source: &[i32], resid: &[i32], levels: &[i32], lambda: f64) -> bool {
    let mut ssd_zero = 0i64;
    let mut ssd_coded = 0i64;
    for i in 0..source.len() {
        let z = i64::from(source[i]);
        let c = i64::from(source[i] - resid[i]);
        ssd_zero += z * z;
        ssd_coded += c * c;
    }
    ssd_coded as f64 + lambda * (block_bits(levels) as f64) < ssd_zero as f64
}

/// Predict and code the luma of an Intra_16x16 macroblock.
#[allow(clippy::too_many_arguments)]
fn code_i16_luma(
    pic: &mut Picture,
    e: &MbEnc<'_>,
    mb_x: usize,
    mb_y: usize,
    qp: i32,
    mode: u8,
    nbr: &MbNeighbors,
    lv: &mut Levels,
) {
    let w4 = pic.mb_w * 4;
    for dy in 0..4 {
        let base = (mb_y * 4 + dy) * w4 + mb_x * 4;
        pic.i4_modes[base..base + 4].fill(2);
    }
    let stride = pic.y.stride;
    let origin = pic.y.at(mb_x * 16, mb_y * 16);
    let mut win = PlaneWindow {
        data: &mut pic.y.data,
        stride,
        origin,
    };
    pred_16x16(mode, &mut win, nbr.b, nbr.a);
    code_luma_i16(pic, e, mb_x, mb_y, qp, lv);
}

/// The Intra_16x16 luma path (8.5.10 in reverse then forward again).
fn code_luma_i16(
    pic: &mut Picture,
    e: &MbEnc<'_>,
    mb_x: usize,
    mb_y: usize,
    qp: i32,
    lv: &mut Levels,
) {
    let stride = pic.y.stride;
    let mut dc = [0i32; 16];
    let mut ac = [[0i32; 16]; 16];
    for blk in 0..16 {
        let (dx, dy) = BLK4_POS[blk];
        let (x, y) = (mb_x * 16 + dx as usize * 4, mb_y * 16 + dy as usize * 4);
        let origin = pic.y.at(x, y);
        let mut d = [0i32; 16];
        for ry in 0..4 {
            for rx in 0..4 {
                d[ry * 4 + rx] = i32::from(e.src.y[(y + ry) * e.src.stride + x + rx])
                    - i32::from(pic.y.data[origin + ry * stride + rx]);
            }
        }
        forward_4x4(&mut d);
        dc[dy as usize * 4 + dx as usize] = d[0];
        let nz = quant_4x4(&d, qp, true, true, &mut lv.luma[blk]);
        lv.luma_nz[blk] = nz;
        ac[blk] = lv.luma[blk];
        if nz > 0 {
            lv.cbp_luma = 15;
        }
    }
    if lv.cbp_luma == 0 {
        for blk in 0..16 {
            lv.luma[blk] = [0; 16];
            lv.luma_nz[blk] = 0;
        }
    }
    forward_hadamard_4x4(&mut dc);
    quant_luma_dc(&dc, qp, &mut lv.dc);

    // Reconstruct exactly as the decoder does: DC through 8.5.10, then each
    // block's AC with the DC substituted at raster position 0.
    let mut raster = [0i32; 16];
    unzigzag(&lv.dc, &mut raster);
    luma_dc_transform(&mut raster, &e.ls, qp);
    let dc_recon = raster;
    for blk in 0..16 {
        let (dx, dy) = BLK4_POS[blk];
        let dc_blk = dc_recon[dy as usize * 4 + dx as usize];
        let tc = lv.luma_nz[blk];
        if tc == 0 && dc_blk == 0 {
            continue;
        }
        let mut raster = [0i32; 16];
        if tc > 0 {
            unzigzag_ac15(&ac[blk], &mut raster);
            dequant_4x4(&mut raster, &e.ls, qp, true);
        }
        raster[0] = dc_blk;
        let mut resid = [0i32; 16];
        inverse_transform_4x4(&raster, &mut resid);
        let origin = pic
            .y
            .at(mb_x * 16 + dx as usize * 4, mb_y * 16 + dy as usize * 4);
        add_residual_4x4(&mut pic.y.data, stride, origin, &resid);
    }
}

/// Chroma residual for both components (8.5.11 + the AC path).
fn code_chroma(
    pic: &mut Picture,
    e: &MbEnc<'_>,
    mb_x: usize,
    mb_y: usize,
    qp_y: i32,
    intra: bool,
    lv: &mut Levels,
) {
    let qp_c = chroma_qp(qp_y);
    let mut dc_lists = [[0i32; 4]; 2];
    let mut ac = [[[0i32; 16]; 4]; 2];
    let mut any_ac = false;
    for comp in 0..2 {
        let (plane, src) = if comp == 0 {
            (&pic.cb, &e.src.u)
        } else {
            (&pic.cr, &e.src.v)
        };
        let stride = plane.stride;
        for blk in 0..4 {
            let (x, y) = (mb_x * 8 + (blk & 1) * 4, mb_y * 8 + (blk >> 1) * 4);
            let origin = plane.at(x, y);
            let mut d = [0i32; 16];
            for ry in 0..4 {
                for rx in 0..4 {
                    d[ry * 4 + rx] = i32::from(src[(y + ry) * e.src.c_stride + x + rx])
                        - i32::from(plane.data[origin + ry * stride + rx]);
                }
            }
            let source = d;
            forward_4x4(&mut d);
            dc_lists[comp][blk] = d[0];
            let mut nz = quant_4x4(&d, qp_c, intra, true, &mut ac[comp][blk]);
            if nz > 0 {
                // The same zero decision as luma, against the AC part alone:
                // the DC of this block travels through the 2x2 Hadamard and is
                // decided with the other three.
                let mut raster = [0i32; 16];
                unzigzag_ac15(&ac[comp][blk], &mut raster);
                dequant_4x4(&mut raster, &e.ls, qp_c, true);
                let mut resid = [0i32; 16];
                inverse_transform_4x4(&raster, &mut resid);
                if !worth_coding(
                    &source,
                    &resid,
                    &ac[comp][blk],
                    zero_block_lambda(
                        e,
                        qp_c,
                        if intra {
                            e.t8x8_intra(mb_y)
                        } else {
                            e.t8x8_inter(mb_y)
                        },
                    ),
                ) {
                    ac[comp][blk] = [0; 16];
                    nz = 0;
                }
            }
            lv.chroma_nz[comp][blk] = nz;
            if nz > 0 {
                any_ac = true;
            }
        }
    }
    let mut any_dc = false;
    for comp in 0..2 {
        forward_hadamard_2x2(&mut dc_lists[comp]);
        let nz = quant_chroma_dc(&dc_lists[comp], qp_c, intra, &mut lv.chroma_dc[comp]);
        if nz > 0 {
            any_dc = true;
        }
    }
    lv.cbp_chroma = if any_ac {
        2
    } else if any_dc {
        1
    } else {
        0
    };
    if lv.cbp_chroma < 2 {
        lv.chroma_nz = [[0; 4]; 2];
    } else {
        lv.chroma = ac;
    }
    if lv.cbp_chroma == 0 {
        return;
    }
    for comp in 0..2 {
        let mut dc = [
            lv.chroma_dc[comp][0],
            lv.chroma_dc[comp][1],
            lv.chroma_dc[comp][2],
            lv.chroma_dc[comp][3],
        ];
        chroma_dc_transform_420(&mut dc, &e.ls, qp_c);
        let plane = if comp == 0 { &mut pic.cb } else { &mut pic.cr };
        let stride = plane.stride;
        for blk in 0..4 {
            let tc = lv.chroma_nz[comp][blk];
            if tc == 0 && dc[blk] == 0 {
                continue;
            }
            let mut raster = [0i32; 16];
            if tc > 0 {
                unzigzag_ac15(&lv.chroma[comp][blk], &mut raster);
                dequant_4x4(&mut raster, &e.ls, qp_c, true);
            }
            raster[0] = dc[blk];
            let mut resid = [0i32; 16];
            inverse_transform_4x4(&raster, &mut resid);
            let origin = plane.at(mb_x * 8 + (blk & 1) * 4, mb_y * 8 + (blk >> 1) * 4);
            add_residual_4x4(&mut plane.data, stride, origin, &resid);
        }
    }
}

/// Pick the chroma prediction mode by SATD, leaving the winner in the planes.
fn choose_chroma_mode(
    pic: &mut Picture,
    e: &MbEnc<'_>,
    nbr: &MbNeighbors,
    mb_x: usize,
    mb_y: usize,
) -> u8 {
    let mut best = (i32::MAX, 0u8);
    for mode in [0u8, 1, 2, 3] {
        let ok = match mode {
            1 => nbr.a,
            2 => nbr.b,
            3 => nbr.a && nbr.b && nbr.d,
            _ => true,
        };
        if !ok {
            continue;
        }
        let mut cost = 0;
        for comp in 0..2 {
            let (plane, src) = if comp == 0 {
                (&mut pic.cb, &e.src.u)
            } else {
                (&mut pic.cr, &e.src.v)
            };
            let stride = plane.stride;
            let origin = plane.at(mb_x * 8, mb_y * 8);
            let mut win = PlaneWindow {
                data: &mut plane.data,
                stride,
                origin,
            };
            pred_chroma_8x8(mode, &mut win, nbr.b, nbr.a);
            cost += sad(
                src,
                e.src.c_stride,
                mb_x * 8,
                mb_y * 8,
                &plane.data[origin..],
                stride,
                8,
                8,
            );
        }
        if cost < best.0 {
            best = (cost, mode);
        }
    }
    // Leave the chosen mode's prediction in place.
    for comp in 0..2 {
        let plane = if comp == 0 { &mut pic.cb } else { &mut pic.cr };
        let stride = plane.stride;
        let origin = plane.at(mb_x * 8, mb_y * 8);
        let mut win = PlaneWindow {
            data: &mut plane.data,
            stride,
            origin,
        };
        pred_chroma_8x8(best.1, &mut win, nbr.b, nbr.a);
    }
    best.1
}

/// Code the luma of an Intra_4x4 macroblock: per block, choose the prediction
/// mode against the *reconstructed* neighbours and reconstruct it immediately,
/// because the next block predicts from it. Returns the chosen modes and the
/// modes they were predicted to be.
/// How many SATD survivors the Intra_4x4 mode decision re-costs with real
/// quantisation before choosing.
const I4_RD_CANDIDATES: usize = 3;

fn code_intra_4x4(
    pic: &mut Picture,
    e: &MbEnc<'_>,
    mb_x: usize,
    mb_y: usize,
    qp: i32,
    nbr: &MbNeighbors,
    lv: &mut Levels,
) -> ([u8; 16], [u8; 16]) {
    let w4 = pic.mb_w * 4;
    let (bx0, by0) = (mb_x * 4, mb_y * 4);
    let mut modes = [2u8; 16];
    let mut pred_modes = [2u8; 16];
    for blk in 0..16 {
        let (dx, dy) = BLK4_POS[blk];
        let (bx, by) = (bx0 + dx as usize, by0 + dy as usize);
        let (x, y) = (mb_x * 16 + dx as usize * 4, mb_y * 16 + dy as usize * 4);
        let left_avail = if dx == 0 { nbr.a } else { true };
        let top_avail = if dy == 0 { nbr.b } else { true };
        let predicted = if left_avail && top_avail {
            pic.i4_modes[by * w4 + bx - 1].min(pic.i4_modes[(by - 1) * w4 + bx])
        } else {
            2
        };
        pred_modes[blk] = predicted;
        let n = gather_nbr4(pic, nbr, blk, x, y);
        let have_tl = match (dx, dy) {
            (0, 0) => nbr.d,
            (0, _) => nbr.a,
            (_, 0) => nbr.b,
            _ => true,
        };
        let allowed = modes_allowed(n.have_top, n.have_left, have_tl);
        let mut best = (i32::MAX, predicted.min(2));
        let mut p = [0u8; 16];
        let mut ranked = [(i32::MAX, 2u8); 9];
        for mode in 0..9u8 {
            if !allowed[mode as usize] {
                continue;
            }
            pred_4x4(mode, &n, &mut p);
            let bits = if mode == predicted { 1 } else { 4 };
            let c = satd4(&e.src.y, e.src.stride, x, y, &p, 4) + motion_rate(e, f64::from(bits));
            ranked[mode as usize] = (c, mode);
            if c < best.0 {
                best = (c, mode);
            }
        }
        let mut mode = best.1;
        // SATD ranks the modes; the survivors are re-costed with the
        // quantisation actually in force, because a mode that predicts a
        // little worse can quantise to far fewer levels. Measured against
        // x264 with `bd_psnr_vs_x264` on a 3840x1608 film clip and a 2560x1440
        // screen capture: +0.129 and +0.088 dB all-intra, +0.062 and +0.043 dB
        // at GOP 10, for 3-4% encode time. Three survivors is where it
        // saturates -- all nine landed within 0.002 dB of three, for 7% more
        // encode time.
        {
            ranked.sort_unstable();
            let keep = I4_RD_CANDIDATES;
            // The same over-count [`ZERO_BLOCK_LAMBDA`] corrects for: with the
            // full rate term this search loses 0.065 dB instead of gaining,
            // and it loses more the more candidates it sees.
            let lambda = ZERO_BLOCK_LAMBDA
                * if e.lambda_standard {
                    e.lambda
                } else {
                    lambda_ssd(qp)
                };
            let mut best_rd = f64::MAX;
            for &(c, cand) in ranked.iter().take(keep) {
                if c == i32::MAX {
                    break;
                }
                pred_4x4(cand, &n, &mut p);
                let mut d = [0i32; 16];
                for ry in 0..4 {
                    for rx in 0..4 {
                        d[ry * 4 + rx] = i32::from(e.src.y[(y + ry) * e.src.stride + x + rx])
                            - i32::from(p[ry * 4 + rx]);
                    }
                }
                let src = d;
                forward_4x4(&mut d);
                let mut levels = [0i32; 16];
                let nz = quant_4x4(&d, qp, true, false, &mut levels);
                let mut resid = [0i32; 16];
                if nz > 0 {
                    let mut raster = [0i32; 16];
                    unzigzag(&levels, &mut raster);
                    dequant_4x4(&mut raster, &e.ls, qp, false);
                    inverse_transform_4x4(&raster, &mut resid);
                }
                let mut ssd = 0i64;
                for i in 0..16 {
                    let err = i64::from(src[i] - resid[i]);
                    ssd += err * err;
                }
                let mode_bits = if cand == predicted { 1 } else { 4 };
                let bits = block_bits(&levels) + mode_bits;
                let cost = ssd as f64 + lambda * bits as f64;
                if cost < best_rd {
                    best_rd = cost;
                    mode = cand;
                }
            }
        }
        modes[blk] = mode;
        pic.i4_modes[by * w4 + bx] = mode;
        // Prediction into the plane, then residual and reconstruction.
        pred_4x4(mode, &n, &mut p);
        let stride = pic.y.stride;
        let origin = pic.y.at(x, y);
        for ry in 0..4 {
            let row = origin + ry * stride;
            pic.y.data[row..row + 4].copy_from_slice(&p[ry * 4..ry * 4 + 4]);
        }
        let mut d = [0i32; 16];
        for ry in 0..4 {
            for rx in 0..4 {
                d[ry * 4 + rx] = i32::from(e.src.y[(y + ry) * e.src.stride + x + rx])
                    - i32::from(p[ry * 4 + rx]);
            }
        }
        forward_4x4(&mut d);
        let nz = quant_4x4(&d, qp, true, false, &mut lv.luma[blk]);
        lv.luma_nz[blk] = nz;
        if nz > 0 {
            lv.cbp_luma |= 1 << (blk >> 2);
            let mut raster = [0i32; 16];
            unzigzag(&lv.luma[blk], &mut raster);
            dequant_4x4(&mut raster, &e.ls, qp, false);
            let mut resid = [0i32; 16];
            inverse_transform_4x4(&raster, &mut resid);
            add_residual_4x4(&mut pic.y.data, stride, origin, &resid);
        }
    }
    (modes, pred_modes)
}

/// Predicted Intra_8x8 mode of `blk8` (8.3.2.1): the decoder's rule, read
/// from the 4x4 slots every size writes its mode into.
fn predicted_mode8(pic: &Picture, nbr: &MbNeighbors, bx: usize, by: usize, blk8: usize) -> u8 {
    let w4 = pic.mb_w * 4;
    let left_avail = if blk8.is_multiple_of(2) { nbr.a } else { true };
    let top_avail = if blk8 < 2 { nbr.b } else { true };
    if left_avail && top_avail {
        pic.i4_modes[by * w4 + bx - 1].min(pic.i4_modes[(by - 1) * w4 + bx])
    } else {
        2
    }
}

/// Residual of one 8x8 luma block against the prediction already in the
/// plane: forward transform, quantise, and — when anything survives —
/// reconstruct with the decoder's own inverse. Returns the non-zero count.
fn code_block_8x8(
    pic: &mut Picture,
    e: &MbEnc<'_>,
    x: usize,
    y: usize,
    qp: i32,
    intra: bool,
    out: &mut [i32; 64],
) -> u8 {
    let stride = pic.y.stride;
    let origin = pic.y.at(x, y);
    let mut d = [0i32; 64];
    for ry in 0..8 {
        for rx in 0..8 {
            d[ry * 8 + rx] = i32::from(e.src.y[(y + ry) * e.src.stride + x + rx])
                - i32::from(pic.y.data[origin + ry * stride + rx]);
        }
    }
    let nz = quant_8x8(&forward_8x8(&d), &e.ls8, qp, intra, out);
    if nz == 0 {
        return 0;
    }
    let mut raster = [0i32; 64];
    unzigzag_8x8(out, &mut raster);
    dequant_8x8(&mut raster, &e.ls8, qp);
    let mut resid = [0i32; 64];
    inverse_transform_8x8(&raster, &mut resid);
    if !intra
        && !worth_coding(
            &d,
            &resid,
            out,
            zero_block_lambda(e, qp, e.transform_8x8_inter),
        )
    {
        *out = [0; 64];
        return 0;
    }
    add_residual_8x8(&mut pic.y.data, stride, origin, &resid);
    nz
}

/// Code the luma of an Intra_8x8 macroblock, the 8x8 sibling of
/// [`code_intra_4x4`]: neighbours are the decoder's filtered reference
/// samples, and every 4x4 mode slot of a block carries its mode.
fn code_intra_8x8(
    pic: &mut Picture,
    e: &MbEnc<'_>,
    mb_x: usize,
    mb_y: usize,
    qp: i32,
    nbr: &MbNeighbors,
    lv: &mut Levels,
) -> ([u8; 4], [u8; 4]) {
    let w4 = pic.mb_w * 4;
    let mut modes = [2u8; 4];
    let mut pred_modes = [2u8; 4];
    lv.trans8 = true;
    for blk8 in 0..4 {
        let (bx, by) = (mb_x * 4 + (blk8 % 2) * 2, mb_y * 4 + (blk8 / 2) * 2);
        let (x, y) = (mb_x * 16 + (blk8 % 2) * 8, mb_y * 16 + (blk8 / 2) * 8);
        let predicted = predicted_mode8(pic, nbr, bx, by, blk8);
        pred_modes[blk8] = predicted;
        let n = filter_nbr8(&gather_nbr8(pic, nbr, blk8, x, y));
        let allowed = modes_allowed(n.have_top, n.have_left, n.have_tl);
        let mut best = (i32::MAX, predicted.min(2));
        let mut p = [0u8; 64];
        for mode in 0..9u8 {
            if !allowed[mode as usize] {
                continue;
            }
            pred_8x8(mode, &n, &mut p);
            let bits = if mode == predicted { 1 } else { 4 };
            let mut c = motion_rate(e, f64::from(bits));
            for q in 0..4 {
                let (ox, oy) = ((q % 2) * 4, (q / 2) * 4);
                c += satd4(&e.src.y, e.src.stride, x + ox, y + oy, &p[oy * 8 + ox..], 8);
            }
            if c < best.0 {
                best = (c, mode);
            }
        }
        let mode = best.1;
        modes[blk8] = mode;
        for dy in 0..2 {
            let base = (by + dy) * w4 + bx;
            pic.i4_modes[base..base + 2].fill(mode);
        }
        pred_8x8(mode, &n, &mut p);
        let stride = pic.y.stride;
        let origin = pic.y.at(x, y);
        for ry in 0..8 {
            let row = origin + ry * stride;
            pic.y.data[row..row + 8].copy_from_slice(&p[ry * 8..ry * 8 + 8]);
        }
        if code_block_8x8(pic, e, x, y, qp, true, &mut lv.luma8[blk8]) > 0 {
            lv.cbp_luma |= 1 << blk8;
        }
    }
    (modes, pred_modes)
}

/// The inter residual under the 8x8 transform, prediction already in the
/// plane: the sibling of [`code_luma_4x4`].
fn code_luma_8x8(
    pic: &mut Picture,
    e: &MbEnc<'_>,
    mb_x: usize,
    mb_y: usize,
    qp: i32,
    lv: &mut Levels,
) {
    lv.trans8 = true;
    for blk8 in 0..4 {
        let (x, y) = (mb_x * 16 + (blk8 % 2) * 8, mb_y * 16 + (blk8 / 2) * 8);
        if code_block_8x8(pic, e, x, y, qp, false, &mut lv.luma8[blk8]) > 0 {
            lv.cbp_luma |= 1 << blk8;
        }
    }
}

/// Squared error of the luma macroblock against the source.
fn ssd_luma(pic: &Picture, e: &MbEnc<'_>, mb_x: usize, mb_y: usize) -> i64 {
    let stride = pic.y.stride;
    let origin = pic.y.at(mb_x * 16, mb_y * 16);
    let mut sum = 0i64;
    for row in 0..16 {
        let s = (mb_y * 16 + row) * e.src.stride + mb_x * 16;
        for x in 0..16 {
            let d = i64::from(e.src.y[s + x]) - i64::from(pic.y.data[origin + row * stride + x]);
            sum += d * d;
        }
    }
    sum
}

/// Copy the luma macroblock out of the plane, and back in.
fn save_luma(pic: &Picture, mb_x: usize, mb_y: usize, out: &mut [u8; 256]) {
    let stride = pic.y.stride;
    let origin = pic.y.at(mb_x * 16, mb_y * 16);
    for row in 0..16 {
        out[row * 16..row * 16 + 16].copy_from_slice(&pic.y.data[origin + row * stride..][..16]);
    }
}

fn restore_luma(pic: &mut Picture, mb_x: usize, mb_y: usize, src: &[u8; 256]) {
    let stride = pic.y.stride;
    let origin = pic.y.at(mb_x * 16, mb_y * 16);
    for row in 0..16 {
        pic.y.data[origin + row * stride..][..16].copy_from_slice(&src[row * 16..row * 16 + 16]);
    }
}

fn rough_intra_bits(lv: &Levels, i4: Option<(&[u8], &[u8])>) -> i64 {
    let mut bits = estimate_luma_bits(lv);
    match i4 {
        Some((modes, pred)) => {
            bits += 6;
            for blk in 0..modes.len() {
                bits += if modes[blk] == pred[blk] { 1 } else { 4 };
            }
        }
        None => bits += 8,
    }
    bits
}

fn entropy_bits(w: &EncEntropy, write: impl FnOnce(&mut EncEntropy)) -> i64 {
    let mut probe = w.clone();
    let before = probe.bit_len();
    write(&mut probe);
    (probe.bit_len() - before) as i64
}

fn cost_luma_nz_pair(
    pic: &Picture,
    nbr: &MbNeighbors,
    local: &[u8; 16],
    bx0: usize,
    by0: usize,
    bx: usize,
    by: usize,
) -> (Option<u8>, Option<u8>) {
    let w4 = pic.mb_w * 4;
    let left_avail = if bx.is_multiple_of(4) { nbr.a } else { true };
    let top_avail = if by.is_multiple_of(4) { nbr.b } else { true };
    let left = (left_avail && bx > 0).then(|| {
        if bx > bx0 {
            local[(by - by0) * 4 + (bx - bx0 - 1)]
        } else {
            pic.nz_y[by * w4 + bx - 1]
        }
    });
    let top = (top_avail && by > 0).then(|| {
        if by > by0 {
            local[(by - by0 - 1) * 4 + (bx - bx0)]
        } else {
            pic.nz_y[(by - 1) * w4 + bx]
        }
    });
    (left, top)
}

fn write_luma_for_cost(
    pic: &Picture,
    nbr: &MbNeighbors,
    w: &mut EncEntropy,
    mb_x: usize,
    mb_y: usize,
    lv: &Levels,
    is_i16: bool,
) {
    let (bx0, by0) = (mb_x * 4, mb_y * 4);
    let mut nz = [0u8; 16];
    if is_i16 {
        let (na, nb) = cost_luma_nz_pair(pic, nbr, &nz, bx0, by0, bx0, by0);
        nz[0] = w.residual_block(&lv.dc, BlockCat::LumaDc, na, nb);
    }
    if lv.trans8 {
        for blk8 in 0..4 {
            let (bx, by) = (bx0 + (blk8 % 2) * 2, by0 + (blk8 / 2) * 2);
            if lv.cbp_luma & (1 << blk8) == 0 {
                continue;
            }
            if w.is_cabac() {
                let tc = w.residual_block(&lv.luma8[blk8], BlockCat::Luma8x8, None, None);
                for dy in 0..2 {
                    let base = (by - by0 + dy) * 4 + bx - bx0;
                    nz[base..base + 2].fill(tc.min(16));
                }
            } else {
                for i4 in 0..4 {
                    let (dx, dy) = BLK4_POS[blk8 * 4 + i4];
                    let (sx, sy) = (bx0 + dx as usize, by0 + dy as usize);
                    let (na, nb) = cost_luma_nz_pair(pic, nbr, &nz, bx0, by0, sx, sy);
                    let sub = sub_block_4x4(&lv.luma8[blk8], i4);
                    nz[(sy - by0) * 4 + (sx - bx0)] =
                        w.residual_block(&sub, BlockCat::Luma4x4, na, nb);
                }
            }
        }
        return;
    }
    for blk in 0..16 {
        let (dx, dy) = BLK4_POS[blk];
        let (bx, by) = (bx0 + dx as usize, by0 + dy as usize);
        let group = 1 << (blk >> 2);
        let coded_block = if is_i16 {
            lv.cbp_luma != 0
        } else {
            lv.cbp_luma & group != 0
        };
        if coded_block {
            let (na, nb) = cost_luma_nz_pair(pic, nbr, &nz, bx0, by0, bx, by);
            let (levels, cat) = if is_i16 {
                (&lv.luma[blk][..15], BlockCat::LumaAc)
            } else {
                (&lv.luma[blk][..16], BlockCat::Luma4x4)
            };
            nz[(by - by0) * 4 + (bx - bx0)] = w.residual_block(levels, cat, na, nb);
        }
    }
}
fn cost_chroma_nz_pair(
    pic: &Picture,
    nbr: &MbNeighbors,
    local: &[[u8; 4]; 2],
    comp: usize,
    cx0: usize,
    cy0: usize,
    cx: usize,
    cy: usize,
) -> (Option<u8>, Option<u8>) {
    let w2 = pic.mb_w * 2;
    let left_avail = if cx.is_multiple_of(2) { nbr.a } else { true };
    let top_avail = if cy.is_multiple_of(2) { nbr.b } else { true };
    let left = (left_avail && cx > 0).then(|| {
        if cx > cx0 {
            local[comp][(cy - cy0) * 2 + (cx - cx0 - 1)]
        } else {
            pic.nz_c[comp][cy * w2 + cx - 1]
        }
    });
    let top = (top_avail && cy > 0).then(|| {
        if cy > cy0 {
            local[comp][(cy - cy0 - 1) * 2 + (cx - cx0)]
        } else {
            pic.nz_c[comp][(cy - 1) * w2 + cx]
        }
    });
    (left, top)
}

fn write_chroma_for_cost(
    pic: &Picture,
    nbr: &MbNeighbors,
    w: &mut EncEntropy,
    mb_x: usize,
    mb_y: usize,
    lv: &Levels,
) {
    if lv.cbp_chroma != 0 {
        for comp in 0..2 {
            w.residual_block(
                &lv.chroma_dc[comp][..4],
                BlockCat::ChromaDc(comp as u8),
                None,
                None,
            );
        }
    }
    if lv.cbp_chroma != 2 {
        return;
    }
    let (cx0, cy0) = (mb_x * 2, mb_y * 2);
    let mut nz = [[0u8; 4]; 2];
    for comp in 0..2 {
        for blk in 0..4 {
            let (cx, cy) = (cx0 + (blk & 1), cy0 + (blk >> 1));
            let (na, nb) = cost_chroma_nz_pair(pic, nbr, &nz, comp, cx0, cy0, cx, cy);
            nz[comp][blk] =
                w.residual_block(&lv.chroma[comp][blk][..15], BlockCat::ChromaAc, na, nb);
        }
    }
}

fn intra_mb_bits(
    pic: &Picture,
    e: &MbEnc<'_>,
    w: &EncEntropy,
    mb_addr: usize,
    nbr: &MbNeighbors,
    lv: &Levels,
    i4: Option<(&[u8], &[u8])>,
    i16_mode: u8,
    chroma_mode: u8,
) -> i64 {
    let use_i4 = i4.is_some();
    let p_slice = e.slice_type == SliceType::P;
    let offset = if p_slice { 5 } else { 0 };
    let intra_type = if use_i4 {
        0
    } else {
        1 + u32::from(i16_mode)
            + 4 * u32::from(lv.cbp_chroma)
            + if lv.cbp_luma != 0 { 12 } else { 0 }
    };
    entropy_bits(w, |cw| {
        cw.begin_mb(&e.mb_ctx);
        cw.coded_mb(p_slice, e.skip_inc);
        cw.set_intra(true);
        cw.mb_type(p_slice, intra_type + offset);
        if let Some((modes, pred)) = i4 {
            if e.transform_8x8 {
                cw.transform_size_8x8_flag(lv.trans8);
            }
            for blk in 0..modes.len() {
                let rem = if modes[blk] == pred[blk] {
                    None
                } else if modes[blk] > pred[blk] {
                    Some(modes[blk] - 1)
                } else {
                    Some(modes[blk])
                };
                cw.intra4x4_pred_mode(rem);
            }
        }
        cw.intra_chroma_pred_mode(chroma_mode);
        if use_i4 {
            cw.coded_block_pattern(lv.cbp_luma, lv.cbp_chroma, true);
        }
        let coded = lv.cbp_luma != 0 || lv.cbp_chroma != 0 || !use_i4;
        if coded {
            cw.mb_qp_delta(e.target_qp - e.qp);
        }
        let mb_x = mb_addr % pic.mb_w;
        let mb_y = mb_addr / pic.mb_w;
        write_luma_for_cost(pic, nbr, cw, mb_x, mb_y, lv, !use_i4);
        write_chroma_for_cost(pic, nbr, cw, mb_x, mb_y, lv);
    })
}

fn inter_mb_bits(
    pic: &Picture,
    e: &MbEnc<'_>,
    w: &EncEntropy,
    mb_addr: usize,
    nbr: &MbNeighbors,
    shape: PShape,
    mvd: &[[i16; 2]; 2],
    inc: &[[usize; 2]; 2],
    lv: &Levels,
) -> i64 {
    entropy_bits(w, |cw| {
        cw.begin_mb(&e.mb_ctx);
        cw.coded_mb(true, e.skip_inc);
        cw.set_intra(false);
        cw.mb_type(true, shape.mb_type());
        for part in 0..shape.parts() {
            for comp in 0..2 {
                cw.mvd(comp, inc[part][comp], i32::from(mvd[part][comp]));
            }
        }
        cw.coded_block_pattern(lv.cbp_luma, lv.cbp_chroma, false);
        if lv.cbp_luma != 0 && e.transform_8x8 {
            cw.transform_size_8x8_flag(lv.trans8);
        }
        if lv.cbp_luma != 0 || lv.cbp_chroma != 0 {
            cw.mb_qp_delta(e.target_qp - e.qp);
            let mb_x = mb_addr % pic.mb_w;
            let mb_y = mb_addr / pic.mb_w;
            write_luma_for_cost(pic, nbr, cw, mb_x, mb_y, lv, false);
            write_chroma_for_cost(pic, nbr, cw, mb_x, mb_y, lv);
        }
    })
}

/// Code an intra macroblock, choosing Intra_16x16 against Intra_4x4 on real
/// rate and real distortion.
///
/// A SATD estimate of the 4x4 form has to predict from the source samples
/// inside the macroblock — the reconstruction it would predict from does not
/// exist until the mode is chosen — and on flat or text-heavy pictures that
/// estimate misjudges which form wins. Coding both and measuring is worth
/// more than it costs: it is 1.1 dB on screen capture at matched bitrate.
fn encode_intra_mb(
    pic: &mut Picture,
    e: &mut MbEnc<'_>,
    w: &mut EncEntropy,
    mb_addr: usize,
    nbr: MbNeighbors,
    cost: IntraCost,
) {
    let mb_x = mb_addr % pic.mb_w;
    let mb_y = mb_addr / pic.mb_w;
    let w4 = pic.mb_w * 4;
    let (bx0, by0) = (mb_x * 4, mb_y * 4);
    let qp = e.target_qp;
    write_intra_mb(pic, mb_x, mb_y);
    let mut lv: Levels;
    let mut modes = [2u8; 16];
    let mut pred_modes = [2u8; 16];
    let mut modes8 = [2u8; 4];
    let mut pred_modes8 = [2u8; 4];
    let mut use_i4 = false;
    let chroma_mode = choose_chroma_mode(pic, e, &nbr, mb_x, mb_y);
    let mut chroma_lv = Levels::default();
    code_chroma(pic, e, mb_x, mb_y, qp, true, &mut chroma_lv);
    let mut use_i8 = false;

    if cost.allow_i4 {
        let mut lv4 = chroma_lv.clone();
        let (m4, p4) = code_intra_4x4(pic, e, mb_x, mb_y, qp, &nbr, &mut lv4);
        let cost4 = if e.t8x8_intra(mb_y) {
            let bits = intra_mb_bits(
                pic,
                e,
                w,
                mb_addr,
                &nbr,
                &lv4,
                Some((&m4, &p4)),
                cost.i16_mode,
                chroma_mode,
            );
            ssd_mb_deblocked(pic, e, mb_x, mb_y, &nbr, false, intra_bs) as f64
                + e.lambda * bits as f64
        } else {
            ssd_luma(pic, e, mb_x, mb_y) as f64
                + lambda_ssd(qp) * rough_intra_bits(&lv4, Some((&m4, &p4))) as f64
        };
        let mut recon4 = [0u8; 256];
        save_luma(pic, mb_x, mb_y, &mut recon4);
        let mut modes4 = [2u8; 16];
        for blk in 0..16 {
            let (dx, dy) = BLK4_POS[blk];
            modes4[blk] = pic.i4_modes[(by0 + dy as usize) * w4 + bx0 + dx as usize];
        }

        // Intra_8x8 on the same terms, when the PPS allows it.
        let mut lv8 = chroma_lv.clone();
        let mut recon8 = [0u8; 256];
        let cost8 = if e.t8x8_intra(mb_y) {
            let (m8, p8) = code_intra_8x8(pic, e, mb_x, mb_y, qp, &nbr, &mut lv8);
            modes8 = m8;
            pred_modes8 = p8;
            save_luma(pic, mb_x, mb_y, &mut recon8);
            let bits = intra_mb_bits(
                pic,
                e,
                w,
                mb_addr,
                &nbr,
                &lv8,
                Some((&m8, &p8)),
                cost.i16_mode,
                chroma_mode,
            );
            ssd_mb_deblocked(pic, e, mb_x, mb_y, &nbr, true, intra_bs) as f64
                + e.lambda * bits as f64
        } else {
            f64::INFINITY
        };

        let mut lv16 = chroma_lv;
        code_i16_luma(pic, e, mb_x, mb_y, qp, cost.i16_mode, &nbr, &mut lv16);
        let cost16 = if e.t8x8_intra(mb_y) {
            let bits = intra_mb_bits(
                pic,
                e,
                w,
                mb_addr,
                &nbr,
                &lv16,
                None,
                cost.i16_mode,
                chroma_mode,
            );
            ssd_mb_deblocked(pic, e, mb_x, mb_y, &nbr, false, intra_bs) as f64
                + e.lambda * bits as f64
        } else {
            ssd_luma(pic, e, mb_x, mb_y) as f64
                + lambda_ssd(qp) * rough_intra_bits(&lv16, None) as f64
        };

        if cost8 + e.lambda * e.t8x8_margin_bits <= cost4
            && cost8 + e.lambda * e.t8x8_margin_bits <= cost16
        {
            use_i4 = true;
            use_i8 = true;
            restore_luma(pic, mb_x, mb_y, &recon8);
            for blk8 in 0..4 {
                for dy in 0..2 {
                    let base = (by0 + (blk8 / 2) * 2 + dy) * w4 + bx0 + (blk8 % 2) * 2;
                    pic.i4_modes[base..base + 2].fill(modes8[blk8]);
                }
            }
            lv = lv8;
        } else if cost4 <= cost16 {
            use_i4 = true;
            restore_luma(pic, mb_x, mb_y, &recon4);
            for blk in 0..16 {
                let (dx, dy) = BLK4_POS[blk];
                pic.i4_modes[(by0 + dy as usize) * w4 + bx0 + dx as usize] = modes4[blk];
            }
            lv = lv4;
            modes = m4;
            pred_modes = p4;
        } else {
            lv = lv16;
        }
    } else {
        lv = chroma_lv;
        code_i16_luma(pic, e, mb_x, mb_y, qp, cost.i16_mode, &nbr, &mut lv);
    }

    // ---- syntax ----
    w.begin_mb(&e.mb_ctx);
    w.coded_mb(e.slice_type == SliceType::P, e.skip_inc);
    w.set_intra(true);
    let intra_type = if use_i4 {
        0
    } else {
        1 + u32::from(cost.i16_mode)
            + 4 * u32::from(lv.cbp_chroma)
            + if lv.cbp_luma != 0 { 12 } else { 0 }
    };
    let p_slice = e.slice_type == SliceType::P;
    let offset = if p_slice { 5 } else { 0 };
    w.mb_type(p_slice, intra_type + offset);
    if use_i4 {
        if e.transform_8x8 {
            w.transform_size_8x8_flag(use_i8);
        }
        // prev_intra8x8_pred_mode_flag / rem_intra8x8_pred_mode share the 4x4
        // elements' binarisation and contexts (9.3.2.2, Table 9-34).
        let (modes, pred_modes): (&[u8], &[u8]) = if use_i8 {
            (&modes8, &pred_modes8)
        } else {
            (&modes, &pred_modes)
        };
        for blk in 0..modes.len() {
            let rem = if modes[blk] == pred_modes[blk] {
                None
            } else if modes[blk] > pred_modes[blk] {
                Some(modes[blk] - 1)
            } else {
                Some(modes[blk])
            };
            w.intra4x4_pred_mode(rem);
        }
    }
    w.intra_chroma_pred_mode(chroma_mode);
    if use_i4 {
        w.coded_block_pattern(lv.cbp_luma, lv.cbp_chroma, true);
    }
    finish_mb(
        pic,
        e,
        w,
        mb_addr,
        &nbr,
        &lv,
        MbRecord {
            intra: true,
            is_i16: !use_i4,
            chroma_mode,
            qp,
        },
    );
}

/// Squared error of the whole macroblock against the source, luma and chroma:
/// the distortion term of the skip decision.
fn ssd_mb(pic: &Picture, e: &MbEnc<'_>, mb_x: usize, mb_y: usize) -> i64 {
    let mut sum = 0i64;
    let stride = pic.y.stride;
    let origin = pic.y.at(mb_x * 16, mb_y * 16);
    for row in 0..16 {
        let s = (mb_y * 16 + row) * e.src.stride + mb_x * 16;
        for x in 0..16 {
            let d = i64::from(e.src.y[s + x]) - i64::from(pic.y.data[origin + row * stride + x]);
            sum += d * d;
        }
    }
    for comp in 0..2 {
        let (plane, src) = if comp == 0 {
            (&pic.cb, &e.src.u)
        } else {
            (&pic.cr, &e.src.v)
        };
        let stride = plane.stride;
        let origin = plane.at(mb_x * 8, mb_y * 8);
        for row in 0..8 {
            let s = (mb_y * 8 + row) * e.src.c_stride + mb_x * 8;
            for x in 0..8 {
                let d = i64::from(src[s + x]) - i64::from(plane.data[origin + row * stride + x]);
                sum += d * d;
            }
        }
    }
    sum
}

/// [`ssd_mb`] with the luma half measured after the loop filter this
/// macroblock's reconstruction will get. `bs(edge, seg, vertical)` is the
/// boundary strength of one 4-sample edge segment — edge 0 the macroblock
/// edge, edges 1..3 the internal edges at offsets 4/8/12 — as the decoder
/// will derive it, so an intra candidate passes its forced 4/3 and an inter
/// candidate its own coded/motion strengths. A transform-8x8 macroblock has
/// no transform edge at luma offsets 4 and 12, so those stay unfiltered,
/// exactly as the decoder's `internal` gate decides it. Neighbours share
/// this slice's qp with the zero offsets the encoder writes, so every edge
/// is parameterised by the trial qp alone. The right and bottom edges
/// belong to macroblocks coded later and cannot be known here; the aprons
/// are the pre-filter neighbour samples, which is everything that exists at
/// decision time. The chroma half is left pre-filter: chroma never takes
/// the 8x8 transform, so it cannot separate the candidates.
///
/// The 8x8-vs-4x4 trials are judged by this and not by [`ssd_mb`] because the
/// two candidates meet different edge sets under the same loop filter: a
/// pre-filter SSD prices the 4x4 candidate as if its extra filtered edges
/// cost nothing.
fn ssd_mb_deblocked(
    pic: &Picture,
    e: &MbEnc<'_>,
    mb_x: usize,
    mb_y: usize,
    nbr: &MbNeighbors,
    trans8: bool,
    bs: impl Fn(usize, usize, bool) -> u8,
) -> i64 {
    // The macroblock at (A, A) plus 4-sample left and top aprons: a bS-4
    // edge reads p3 and rewrites p2 on its far side.
    const A: usize = 4;
    const S: usize = A + 16;
    let mut scratch = [0u8; S * S];
    let stride = pic.y.stride;
    let origin = pic.y.at(mb_x * 16, mb_y * 16);
    for row in 0..16 {
        let src = origin + row * stride;
        let dst = (A + row) * S + A;
        scratch[dst..dst + 16].copy_from_slice(&pic.y.data[src..src + 16]);
    }
    if nbr.a {
        for row in 0..16 {
            let src = origin + row * stride - A;
            let dst = (A + row) * S;
            scratch[dst..dst + A].copy_from_slice(&pic.y.data[src..src + A]);
        }
    }
    if nbr.b {
        for row in 0..A {
            let src = origin - (A - row) * stride;
            let dst = row * S + A;
            scratch[dst..dst + 16].copy_from_slice(&pic.y.data[src..src + 16]);
        }
    }
    // Vertical edges left to right, then horizontal top to bottom, the order
    // of 8.7.1: each edge filters what the earlier edges left behind. Each
    // 4-sample segment carries its own strength, as in the decoder.
    let qp = e.target_qp;
    for edge in 0..4 {
        let x = edge * 4;
        if (edge == 0 && !nbr.a) || (trans8 && x != 0 && x != 8) {
            continue;
        }
        for seg in 0..4 {
            let s = bs(edge, seg, true);
            if s == 0 {
                continue;
            }
            let ep = edge_params(qp, 0, 0, s);
            for row in seg * 4..seg * 4 + 4 {
                filter_luma_line(&mut scratch, (A + row) * S + A + x, 1, &ep);
            }
        }
    }
    for edge in 0..4 {
        let y = edge * 4;
        if (edge == 0 && !nbr.b) || (trans8 && y != 0 && y != 8) {
            continue;
        }
        let seg_bs = [
            bs(edge, 0, false),
            bs(edge, 1, false),
            bs(edge, 2, false),
            bs(edge, 3, false),
        ];
        if seg_bs[0] == seg_bs[1] && seg_bs[1] == seg_bs[2] && seg_bs[2] == seg_bs[3] {
            if seg_bs[0] != 0 {
                let ep = edge_params(qp, 0, 0, seg_bs[0]);
                filter_luma_h_edge16(&mut scratch, (A + y) * S + A, S, &ep);
            }
            continue;
        }
        for seg in 0..4 {
            if seg_bs[seg] == 0 {
                continue;
            }
            let ep = edge_params(qp, 0, 0, seg_bs[seg]);
            for col in seg * 4..seg * 4 + 4 {
                filter_luma_line(&mut scratch, (A + y) * S + A + col, S, &ep);
            }
        }
    }
    let mut sum = ssd_mb(pic, e, mb_x, mb_y) - ssd_luma(pic, e, mb_x, mb_y);
    for row in 0..16 {
        let src = (mb_y * 16 + row) * e.src.stride + mb_x * 16;
        for x in 0..16 {
            let d =
                i64::from(e.src.y[src + x]) - i64::from(scratch[(A + row) * S + A + x]);
            sum += d * d;
        }
    }
    sum
}

/// Boundary strengths an intra candidate forces (8.7.2.1): 4 on its
/// macroblock edges, 3 on the internal luma edges.
fn intra_bs(edge: usize, _seg: usize, _vertical: bool) -> u8 {
    if edge == 0 {
        4
    } else {
        3
    }
}

/// Rough bit cost of a macroblock's luma residual.
fn estimate_luma_bits(lv: &Levels) -> i64 {
    let mut bits = 0;
    if lv.trans8 {
        for blk8 in 0..4 {
            if lv.cbp_luma & (1 << blk8) != 0 {
                // A level's position costs two more bits in a 64-coefficient
                // significance map than in a 16-coefficient one; measured
                // against the coded size on the text clip, without this the
                // estimate undercharges Intra_8x8 by ~44 bits a macroblock.
                let b = &lv.luma8[blk8];
                bits += block_bits(b) + 2 * b.iter().filter(|&&l| l != 0).count() as i64;
            }
        }
        return bits;
    }
    for blk in 0..16 {
        if lv.cbp_luma & (1 << (blk >> 2)) != 0 {
            bits += block_bits(&lv.luma[blk]);
        }
    }
    if lv.cbp_luma == 15 {
        // Intra_16x16 also carries the DC block.
        bits += block_bits(&lv.dc);
    }
    bits
}

/// Bits one residual block costs, to the accuracy a mode decision needs.
fn block_bits(b: &[i32]) -> i64 {
    let mut n = 0i64;
    for &l in b {
        if l != 0 {
            n += 2 * (64 - u64::from(l.unsigned_abs()).leading_zeros() as i64) + 1;
        }
    }
    if n > 0 {
        n + 6
    } else {
        2
    }
}

/// Rough bit cost of a macroblock's residual, for the skip decision: a level
/// costs its magnitude's exp-Golomb-ish width, a coded block its token.
fn estimate_bits(lv: &Levels) -> i64 {
    let mut bits = 10 + estimate_luma_bits(lv);
    if lv.cbp_chroma != 0 {
        for comp in 0..2 {
            bits += block_bits(&lv.chroma_dc[comp][..4]);
            if lv.cbp_chroma == 2 {
                for blk in 0..4 {
                    bits += block_bits(&lv.chroma[comp][blk][..15]);
                }
            }
        }
    }
    bits
}

/// Motion-compensate one partition into the picture planes.
fn compensate_part(
    pic: &mut Picture,
    reference: &Picture,
    mb_x: usize,
    mb_y: usize,
    (bx, by): (usize, usize),
    (w, h): (usize, usize),
    mv: [i16; 2],
) {
    let (x0, y0) = ((mb_x * 16 + bx * 4) as i32, (mb_y * 16 + by * 4) as i32);
    let plane = RefPlane {
        data: &reference.y.data,
        stride: reference.y.stride,
        origin: reference.y.origin,
        width: reference.y.width,
        height: reference.y.height,
        pad: reference.y.pad,
    };
    let stride = pic.y.stride;
    let origin = pic.y.at(mb_x * 16 + bx * 4, mb_y * 16 + by * 4);
    mc_luma(&plane, x0, y0, mv, w, h, stride, &mut pic.y.data[origin..]);
    for comp in 0..2 {
        let src = if comp == 0 {
            &reference.cb
        } else {
            &reference.cr
        };
        let plane = RefPlane {
            data: &src.data,
            stride: src.stride,
            origin: src.origin,
            width: src.width,
            height: src.height,
            pad: src.pad,
        };
        let dst = if comp == 0 { &mut pic.cb } else { &mut pic.cr };
        let stride = dst.stride;
        let origin = dst.at(mb_x * 8 + bx * 2, mb_y * 8 + by * 2);
        mc_chroma(
            &plane,
            x0 / 2,
            y0 / 2,
            mv,
            w / 2,
            h / 2,
            stride,
            &mut dst.data[origin..],
        );
    }
}

/// Code a P macroblock: skip, one 16x16 partition, or two halves.
fn encode_inter_mb(
    pic: &mut Picture,
    e: &mut MbEnc<'_>,
    w: &mut EncEntropy,
    mb_addr: usize,
    nbr: MbNeighbors,
    choice: InterChoice,
) {
    let mb_x = mb_addr % pic.mb_w;
    let mb_y = mb_addr / pic.mb_w;
    let reference = e.reference.expect("a P macroblock has a reference");
    let ref_id = reference.id;
    let qp = e.target_qp;
    let shape = if choice.prefer_skip {
        PShape::Whole
    } else {
        choice.shape
    };

    // Motion vectors, their predictors and the reconstruction, partition by
    // partition and in coding order: a partition's predictor reads the motion
    // of the partitions before it (8.4.1.3), so the order is not free.
    let mut ctx = MvCtx {
        mb_x,
        mb_y,
        slice_id: e.slice_id,
        written: 0,
    };
    let (part_w, part_h) = shape.part_blocks();
    let mut mvd = [[0i16; 2]; 2];
    let mut inc = [[0usize; 2]; 2];
    for part in 0..shape.parts() {
        let ((bx, by), (pw, ph)) = shape.geometry(part);
        let mv = if choice.prefer_skip {
            choice.skip_mv
        } else {
            choice.mv[part]
        };
        let n = ctx.neighbours(pic, bx, by, part_w, 0);
        let mvp = predict_mv(&n, 0, part_w, part_h, part);
        // ctxIdxInc from the neighbouring partitions' absolute mvd
        // (9.3.3.1.1.7), read before this partition writes its own.
        let (gx, gy) = ((mb_x * 4 + bx) as i32, (mb_y * 4 + by) as i32);
        for comp in 0..2 {
            let sum = neighbour_mvd(pic, &ctx, gx - 1, gy, 0)[comp]
                + neighbour_mvd(pic, &ctx, gx, gy - 1, 0)[comp];
            inc[part][comp] = if sum > 32 {
                2
            } else if sum > 2 {
                1
            } else {
                0
            };
        }
        mvd[part] = [mv[0] - mvp[0], mv[1] - mvp[1]];
        compensate_part(pic, reference, mb_x, mb_y, (bx, by), (pw, ph), mv);
        for py in by..by + ph / 4 {
            for px in bx..bx + pw / 4 {
                write_block(
                    pic,
                    &mut ctx,
                    px,
                    py,
                    [mv, [0, 0]],
                    [0, -1],
                    [ref_id, -1],
                    0,
                );
                write_mvd(pic, mb_x, mb_y, px, py, 0, mvd[part]);
            }
        }
    }

    // Skipping is only *available* on the P_Skip vector of a whole macroblock,
    // and only worth taking when the residual it drops is worth less than the
    // bits it saves: on static content a macroblock whose residual is pure
    // quantisation churn costs bits every picture and improves nothing.
    let may_skip = choice.prefer_skip;
    let ssd_skip = may_skip.then(|| ssd_mb(pic, e, mb_x, mb_y));
    let mut chroma_lv = Levels::default();
    code_chroma(pic, e, mb_x, mb_y, qp, false, &mut chroma_lv);
    let mut lv = chroma_lv.clone();
    if e.t8x8_inter(mb_y) {
        // Both transform sizes on the same prediction; the cheaper
        // rate-distortion cost is coded.
        let mut pred = [0u8; 256];
        save_luma(pic, mb_x, mb_y, &mut pred);
        let mut lv4 = chroma_lv.clone();
        code_luma_4x4(pic, e, mb_x, mb_y, qp, false, &mut lv4);
        let bits4 = inter_mb_bits(pic, e, w, mb_addr, &nbr, shape, &mvd, &inc, &lv4);
        let cost4 = ssd_mb(pic, e, mb_x, mb_y) as f64 + e.lambda * bits4 as f64;
        let mut recon4 = [0u8; 256];
        save_luma(pic, mb_x, mb_y, &mut recon4);
        restore_luma(pic, mb_x, mb_y, &pred);
        let mut lv8 = chroma_lv;
        code_luma_8x8(pic, e, mb_x, mb_y, qp, &mut lv8);
        let bits8 = inter_mb_bits(pic, e, w, mb_addr, &nbr, shape, &mvd, &inc, &lv8);
        let cost8 = ssd_mb(pic, e, mb_x, mb_y) as f64 + e.lambda * bits8 as f64;
        if cost8 + e.lambda * e.t8x8_margin_bits < cost4 {
            lv = lv8;
        } else {
            lv = lv4;
            restore_luma(pic, mb_x, mb_y, &recon4);
        }
    } else {
        code_luma_4x4(pic, e, mb_x, mb_y, qp, false, &mut lv);
    }
    let empty = lv.cbp_luma == 0 && lv.cbp_chroma == 0;
    let skip = match ssd_skip {
        None => false,
        Some(_) if empty => true,
        Some(ssd_skip) => {
            let coded = if e.t8x8_inter(mb_y) {
                let bits = inter_mb_bits(pic, e, w, mb_addr, &nbr, shape, &mvd, &inc, &lv);
                ssd_mb(pic, e, mb_x, mb_y) as f64 + e.lambda * bits as f64
            } else {
                ssd_mb(pic, e, mb_x, mb_y) as f64 + lambda_ssd(qp) * estimate_bits(&lv) as f64
            };
            if ssd_skip as f64 <= coded {
                // Put the prediction back: the residual has been added to it.
                compensate_part(pic, reference, mb_x, mb_y, (0, 0), (16, 16), choice.skip_mv);
                true
            } else {
                false
            }
        }
    };

    let w4 = pic.mb_w * 4;
    if skip {
        for py in 0..4 {
            for px in 0..4 {
                let idx = (mb_y * 4 + py) * w4 + mb_x * 4 + px;
                pic.blk[idx] = BLK_SKIP;
                pic.mvd_abs[idx] = [0; 4];
            }
        }
        for dy in 0..4 {
            let base = (mb_y * 4 + dy) * w4 + mb_x * 4;
            pic.nz_y[base..base + 4].fill(0);
            pic.i4_modes[base..base + 4].fill(2);
        }
        for comp in 0..2 {
            for dy in 0..2 {
                let base = (mb_y * 2 + dy) * pic.mb_w * 2 + mb_x * 2;
                pic.nz_c[comp][base..base + 2].fill(0);
            }
        }
        pic.mb_qp[mb_addr] = e.qp as u8;
        pic.mb_flags[mb_addr] = FLAG_DECODED | FLAG_INTER | FLAG_SKIP;
        pic.mb_cbp[mb_addr] = 0;
        pic.mb_dc_cbf[mb_addr] = 0;
        pic.mb_slice[mb_addr] = e.slice_id;
        pic.decoded_mbs += 1;
        e.qp_delta_inc = 0;
        w.skipped_mb(e.skip_inc);
        return;
    }
    for dy in 0..4 {
        let base = (mb_y * 4 + dy) * w4 + mb_x * 4;
        pic.i4_modes[base..base + 4].fill(2);
    }

    // ---- syntax ----
    w.begin_mb(&e.mb_ctx);
    w.coded_mb(true, e.skip_inc);
    w.set_intra(false);
    w.mb_type(true, shape.mb_type());
    // num_ref_idx_l0_active is 1, so ref_idx_l0 is not coded.
    for part in 0..shape.parts() {
        for comp in 0..2 {
            w.mvd(comp, inc[part][comp], i32::from(mvd[part][comp]));
        }
    }
    w.coded_block_pattern(lv.cbp_luma, lv.cbp_chroma, false);
    // Every shape this encoder emits is 8x8-aligned, so the decoder reads the
    // flag whenever luma is coded.
    if lv.cbp_luma != 0 && e.transform_8x8 {
        w.transform_size_8x8_flag(lv.trans8);
    }
    finish_mb(
        pic,
        e,
        w,
        mb_addr,
        &nbr,
        &lv,
        MbRecord {
            intra: false,
            is_i16: false,
            chroma_mode: 0,
            qp,
        },
    );
}

/// What the residual writer needs to know about the macroblock it closes.
struct MbRecord {
    intra: bool,
    is_i16: bool,
    chroma_mode: u8,
    /// The QP the residual was quantised at.
    qp: i32,
}

/// mb_qp_delta, the residual blocks and the per-macroblock metadata: the
/// writing half of [`crate::decoder::finish_macroblock`].
fn finish_mb(
    pic: &mut Picture,
    e: &mut MbEnc<'_>,
    w: &mut EncEntropy,
    mb_addr: usize,
    nbr: &MbNeighbors,
    lv: &Levels,
    m: MbRecord,
) {
    let mb_x = mb_addr % pic.mb_w;
    let mb_y = mb_addr / pic.mb_w;
    let w4 = pic.mb_w * 4;
    let w2 = pic.mb_w * 2;
    let (bx0, by0) = (mb_x * 4, mb_y * 4);
    let (cx0, cy0) = (mb_x * 2, mb_y * 2);

    let coded = lv.cbp_luma != 0 || lv.cbp_chroma != 0 || m.is_i16;
    let qp_y = if coded {
        let delta = m.qp - e.qp;
        w.mb_qp_delta(delta);
        e.qp_delta_inc = u8::from(delta != 0);
        e.qp = m.qp;
        m.qp
    } else {
        // No residual: the QP prediction chain is untouched (7.4.5), and the
        // deblocking filter uses the chain value.
        e.qp_delta_inc = 0;
        e.qp
    };
    pic.mb_qp[mb_addr] = qp_y as u8;
    pic.mb_flags[mb_addr] = FLAG_DECODED
        | if m.is_i16 { FLAG_I16 } else { 0 }
        | if m.intra { 0 } else { FLAG_INTER }
        // The flag only reaches the stream when the decoder reads it (7.3.5).
        | if lv.trans8 && (m.intra || lv.cbp_luma != 0) {
            FLAG_TRANS8X8
        } else {
            0
        }
        | if m.intra && m.chroma_mode != 0 {
            FLAG_CHROMA_PRED
        } else {
            0
        };
    pic.mb_cbp[mb_addr] = lv.cbp_luma | (lv.cbp_chroma << 4);
    pic.mb_slice[mb_addr] = e.slice_id;
    pic.decoded_mbs += 1;

    let mut dc_cbf = 0u8;
    // Luma DC (Intra_16x16 only), then the luma blocks in Z-order.
    if m.is_i16 {
        let (na, nb) = luma_nz_pair(pic, nbr, bx0, by0);
        let tc = w.residual_block(&lv.dc, BlockCat::LumaDc, na, nb);
        dc_cbf |= u8::from(tc != 0);
    }
    if lv.trans8 {
        // The decoder's 8x8 branch: CABAC codes one cat-5 block and reports
        // its count in every 4x4 slot; CAVLC codes four interleaved 4x4
        // blocks, each with its own nC and count.
        for blk8 in 0..4 {
            let (bx, by) = (bx0 + (blk8 % 2) * 2, by0 + (blk8 / 2) * 2);
            if lv.cbp_luma & (1 << blk8) == 0 {
                for dy in 0..2 {
                    let base = (by + dy) * w4 + bx;
                    pic.nz_y[base..base + 2].fill(0);
                }
            } else if w.is_cabac() {
                let tc = w.residual_block(&lv.luma8[blk8], BlockCat::Luma8x8, None, None);
                for dy in 0..2 {
                    let base = (by + dy) * w4 + bx;
                    pic.nz_y[base..base + 2].fill(tc.min(16));
                }
            } else {
                for i4 in 0..4 {
                    let (dx, dy) = BLK4_POS[blk8 * 4 + i4];
                    let (sx, sy) = (bx0 + dx as usize, by0 + dy as usize);
                    let (na, nb) = luma_nz_pair(pic, nbr, sx, sy);
                    let sub = sub_block_4x4(&lv.luma8[blk8], i4);
                    pic.nz_y[sy * w4 + sx] = w.residual_block(&sub, BlockCat::Luma4x4, na, nb);
                }
            }
        }
    }
    for blk in 0..16 {
        if lv.trans8 {
            break;
        }
        let (dx, dy) = BLK4_POS[blk];
        let (bx, by) = (bx0 + dx as usize, by0 + dy as usize);
        let group = 1 << (blk >> 2);
        let coded_block = if m.is_i16 {
            lv.cbp_luma != 0
        } else {
            lv.cbp_luma & group != 0
        };
        let tc = if coded_block {
            let (na, nb) = luma_nz_pair(pic, nbr, bx, by);
            let (levels, cat) = if m.is_i16 {
                (&lv.luma[blk][..15], BlockCat::LumaAc)
            } else {
                (&lv.luma[blk][..16], BlockCat::Luma4x4)
            };
            w.residual_block(levels, cat, na, nb)
        } else {
            0
        };
        pic.nz_y[by * w4 + bx] = tc;
    }
    // Chroma DC then chroma AC, in bitstream order.
    if lv.cbp_chroma != 0 {
        for comp in 0..2 {
            let tc = w.residual_block(
                &lv.chroma_dc[comp][..4],
                BlockCat::ChromaDc(comp as u8),
                None,
                None,
            );
            dc_cbf |= u8::from(tc != 0) << (1 + comp);
        }
    }
    for comp in 0..2 {
        for blk in 0..4 {
            let (cx, cy) = (cx0 + (blk & 1), cy0 + (blk >> 1));
            let tc = if lv.cbp_chroma == 2 {
                let (na, nb) = chroma_nz_pair(pic, nbr, comp, cx, cy);
                w.residual_block(&lv.chroma[comp][blk][..15], BlockCat::ChromaAc, na, nb)
            } else {
                0
            };
            pic.nz_c[comp][cy * w2 + cx] = tc;
        }
    }
    pic.mb_dc_cbf[mb_addr] = dc_cbf;
}

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

use ec_core::BitWriter;
use ec_h264_syntax::SliceType;
use wide::i16x16;

use crate::decoder::{MbNeighbors, chroma_nz_pair, gather_nbr4, luma_nz_pair, mb_neighbors};
use crate::dpb::{BLK_SKIP, Picture};
use crate::entropy::{FLAG_CHROMA_PRED, FLAG_DECODED, FLAG_I16, FLAG_INTER, FLAG_SKIP};
use crate::inter::{RefPlane, integer_origin, mc_chroma, mc_luma};
use crate::mv::{MvCtx, predict_mv, write_block, write_intra_mb, write_mvd};
use crate::pred::{PlaneWindow, add_residual_4x4, pred_4x4, pred_16x16, pred_chroma_8x8};
use crate::tables::{BLK4_POS, CHROMA_QP};
use crate::transform::{
    LevelScale4x4, chroma_dc_transform_420, dequant_4x4, inverse_transform_4x4, luma_dc_transform,
    unzigzag, unzigzag_ac15,
};

use super::quant::{
    forward_4x4, forward_hadamard_2x2, forward_hadamard_4x4, quant_4x4, quant_chroma_dc,
    quant_luma_dc,
};
use super::vlc::{write_cbp, write_residual_block};

/// Speed/quality ladder. Two rungs, because the two the incumbent exposed are
/// the two edith actually picks between.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Preset {
    /// Diamond motion search, 16x16 partitions, intra 4x4 in I pictures only.
    #[default]
    Fast,
    /// Wider search with a hexagon refinement and intra 4x4 everywhere.
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
    /// Lagrangian multiplier in the SATD domain.
    pub lambda: i32,
    pub preset: Preset,
    /// Macroblocks skipped since the last coded one (`mb_skip_run`, 7.3.4).
    pub skip_run: u32,
    pub ls: LevelScale4x4,
}

/// Quantised levels of one macroblock, scan order per block.
#[derive(Default)]
struct Levels {
    /// Luma AC/4x4 blocks in Z-order; only `[..15]` is used under Intra_16x16.
    luma: [[i32; 16]; 16],
    /// Non-zero count per luma block, Z-order.
    luma_nz: [u8; 16],
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
    let mut sum = 0;
    for by in 0..4 {
        for bx in 0..4 {
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

#[allow(clippy::too_many_arguments)]
/// Sum of absolute differences of a 16x16 macroblock against a prediction of
/// any pitch, sixteen samples at a time: this is the motion search's inner
/// loop and nothing else in the encoder is called as often.
fn sad16(
    src: &[u8],
    sstride: usize,
    sx: usize,
    sy: usize,
    pred: &[u8],
    po: usize,
    pstride: usize,
) -> i32 {
    let mut acc = i16x16::ZERO;
    for row in 0..16 {
        let a = load16(src, (sy + row) * sstride + sx);
        let b = load16(pred, po + row * pstride);
        acc += (a - b).abs();
    }
    let lanes = acc.to_array();
    lanes.iter().map(|&v| i32::from(v)).sum()
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

/// Sum of absolute differences of a `w` x `h` block against a prediction with
/// its own pitch.
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

/// Code one macroblock: decide its mode, reconstruct it into `pic` and write
/// its syntax into `w`.
pub(crate) fn encode_mb(pic: &mut Picture, e: &mut MbEnc<'_>, w: &mut BitWriter, mb_addr: usize) {
    let mb_x = mb_addr % pic.mb_w;
    let mb_y = mb_addr / pic.mb_w;
    let nbr = mb_neighbors(pic, mb_x, mb_y, e.slice_id);

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
        Some(i) => intra_cost.best() + e.lambda * 8 < i.cost,
    };

    if go_intra {
        encode_intra_mb(pic, e, w, mb_addr, nbr, intra_cost);
    } else {
        let choice = inter.expect("a P macroblock has an inter choice");
        encode_inter_mb(pic, e, w, mb_addr, nbr, choice);
    }
}

/// The intra decision taken before any reconstruction: best Intra_16x16 mode,
/// and whether the 4x4 modes look cheaper.
struct IntraCost {
    i16_mode: u8,
    i16_cost: i32,
    i4_cost: i32,
    /// True when Intra_4x4 is allowed at all for this picture and preset.
    allow_i4: bool,
}

impl IntraCost {
    fn best(&self) -> i32 {
        if self.allow_i4 {
            self.i16_cost.min(self.i4_cost)
        } else {
            self.i16_cost
        }
    }

    fn use_i4(&self) -> bool {
        self.allow_i4 && self.i4_cost < self.i16_cost
    }
}

/// Estimate both intra costs. The 4x4 estimate predicts from the *source*
/// samples inside the macroblock (they are written into the plane first), which
/// is the standard approximation: reconstruction is not available until the
/// mode is chosen, and the two differ by less than the decision margin.
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

    let allow_i4 = e.slice_type == SliceType::I || e.preset == Preset::Balanced;
    let mut i4_cost = i32::MAX;
    if allow_i4 {
        // Source samples stand in for the not-yet-reconstructed neighbours.
        for y in 0..16 {
            let dst = origin + y * stride;
            let s = (sy + y) * e.src.stride + sx;
            pic.y.data[dst..dst + 16].copy_from_slice(&e.src.y[s..s + 16]);
        }
        let mut sum = 0;
        for blk in 0..16 {
            let (dx, dy) = BLK4_POS[blk];
            let (x, y) = (sx + dx as usize * 4, sy + dy as usize * 4);
            let n = gather_nbr4(pic, nbr, blk, x, y);
            let have_tl = match (dx, dy) {
                (0, 0) => nbr.d,
                (0, _) => nbr.a,
                (_, 0) => nbr.b,
                _ => true,
            };
            let allowed = modes_allowed(n.have_top, n.have_left, have_tl);
            let mut best4 = i32::MAX;
            let mut p = [0u8; 16];
            for mode in 0..9u8 {
                if !allowed[mode as usize] {
                    continue;
                }
                pred_4x4(mode, &n, &mut p);
                let c = satd4(&e.src.y, e.src.stride, x, y, &p, 4) + e.lambda * 3;
                best4 = best4.min(c);
            }
            sum += best4;
        }
        // Sixteen mode signals and a coded_block_pattern the 16x16 form does
        // not pay for.
        i4_cost = sum + e.lambda * 16;
    }
    IntraCost {
        i16_mode: best.1,
        i16_cost: best.0,
        i4_cost,
        allow_i4,
    }
}

/// The inter decision for a P macroblock.
struct InterChoice {
    mv: [i16; 2],
    mvp: [i16; 2],
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
    let mut buf = [0u8; 256];
    let sw = e.src.stride;
    // Whole-sample candidates read the reference plane in place; only the
    // sub-sample refinement pays for interpolation. The search runs on SAD,
    // which is what a search can afford; the winner is re-costed on SATD below
    // because the intra candidates are SATD and the two scales differ.
    let mut cost_of = |mv: [i16; 2], satd: bool| -> i32 {
        let bits = e.lambda
            * (se_bits(i32::from(mv[0] - mvp[0])) + se_bits(i32::from(mv[1] - mvp[1])))
            / 2;
        if !satd && mv[0] & 3 == 0 && mv[1] & 3 == 0 {
            let o = integer_origin(
                &plane,
                sx as i32 + (mv[0] >> 2) as i32,
                sy as i32 + (mv[1] >> 2) as i32,
                16,
                16,
            );
            return sad16(&e.src.y, sw, sx, sy, plane.data, o, plane.stride) + bits;
        }
        mc_luma(&plane, sx as i32, sy as i32, mv, 16, 16, 16, &mut buf);
        let d = if satd {
            satd16(&e.src.y, sw, sx, sy, &buf, 16, 0)
        } else {
            sad16(&e.src.y, sw, sx, sy, &buf, 0, 16)
        };
        d + bits
    };

    // Candidates: the predictor, the skip vector, zero, and the vectors of the
    // neighbouring partitions.
    let mut best = ([0i16; 2], i32::MAX);
    for cand in [mvp, skip_mv, [0, 0], n[0].mv, n[1].mv, n[2].mv] {
        let c = cost_of(cand, false);
        if c < best.1 {
            best = (cand, c);
        }
    }
    // A macroblock the prediction already fits is not searched further: on real
    // content most of them are, and the search is the encoder's hot loop.
    // A macroblock the prediction already fits within half a level a sample is
    // not searched further: on real content most of them are, and this doubles
    // the encoder's speed for 0.2 dB (measured, 1080p screen capture; camera
    // content shows no loss at all).
    const GOOD_ENOUGH: i32 = 16 * 16 / 2;
    if best.1 > GOOD_ENOUGH {
        // Diamond refinement at whole samples, coarse to fine.
        let range = match e.preset {
            Preset::Fast => 16i16,
            Preset::Balanced => 48,
        };
        let mut step = 4i16 * 4; // four whole samples, in quarter units
        while step >= 4 {
            let mut improved = true;
            while improved {
                improved = false;
                for (dx, dy) in [(step, 0), (-step, 0), (0, step), (0, -step)] {
                    let cand = [best.0[0] + dx, best.0[1] + dy];
                    if (cand[0] - mvp[0]).abs() > range * 4 || (cand[1] - mvp[1]).abs() > range * 4
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
        // Half then quarter sample.
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

    // Re-cost both candidates on SATD for the comparison against intra, and
    // for the skip decision (which trades a whole macroblock of bits).
    let searched = cost_of(best.0, true);
    let skip_cost = cost_of(skip_mv, true) - e.lambda * 4;
    InterChoice {
        mv: best.0,
        mvp,
        skip_mv,
        cost: searched.min(skip_cost),
        prefer_skip: skip_cost <= searched,
    }
}

/// Motion-compensate a whole macroblock into the picture planes.
fn compensate_mb(pic: &mut Picture, reference: &Picture, mb_x: usize, mb_y: usize, mv: [i16; 2]) {
    let (x0, y0) = ((mb_x * 16) as i32, (mb_y * 16) as i32);
    let plane = RefPlane {
        data: &reference.y.data,
        stride: reference.y.stride,
        origin: reference.y.origin,
        width: reference.y.width,
        height: reference.y.height,
        pad: reference.y.pad,
    };
    let stride = pic.y.stride;
    let origin = pic.y.at(mb_x * 16, mb_y * 16);
    mc_luma(
        &plane,
        x0,
        y0,
        mv,
        16,
        16,
        stride,
        &mut pic.y.data[origin..],
    );
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
        let origin = dst.at(mb_x * 8, mb_y * 8);
        mc_chroma(
            &plane,
            x0 / 2,
            y0 / 2,
            mv,
            8,
            8,
            stride,
            &mut dst.data[origin..],
        );
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
        forward_4x4(&mut d);
        let nz = quant_4x4(&d, qp, intra, false, &mut lv.luma[blk]);
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
            forward_4x4(&mut d);
            dc_lists[comp][blk] = d[0];
            let nz = quant_4x4(&d, qp_c, intra, true, &mut ac[comp][blk]);
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

/// Code an intra macroblock, Intra_16x16 or Intra_4x4.
fn encode_intra_mb(
    pic: &mut Picture,
    e: &mut MbEnc<'_>,
    w: &mut BitWriter,
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
    let mut lv = Levels::default();
    let use_i4 = cost.use_i4();
    let mut modes = [2u8; 16];
    let mut pred_modes = [2u8; 16];

    if use_i4 {
        // Per block: choose the mode against reconstructed neighbours, then
        // reconstruct it immediately — the next block predicts from it.
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
            let n = gather_nbr4(pic, &nbr, blk, x, y);
            let have_tl = match (dx, dy) {
                (0, 0) => nbr.d,
                (0, _) => nbr.a,
                (_, 0) => nbr.b,
                _ => true,
            };
            let allowed = modes_allowed(n.have_top, n.have_left, have_tl);
            let mut best = (i32::MAX, predicted.min(2));
            let mut p = [0u8; 16];
            for mode in 0..9u8 {
                if !allowed[mode as usize] {
                    continue;
                }
                pred_4x4(mode, &n, &mut p);
                let bits = if mode == predicted { 1 } else { 4 };
                let c = satd4(&e.src.y, e.src.stride, x, y, &p, 4) + e.lambda * bits;
                if c < best.0 {
                    best = (c, mode);
                }
            }
            let mode = best.1;
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
    } else {
        for dy in 0..4 {
            let base = (by0 + dy) * w4 + bx0;
            pic.i4_modes[base..base + 4].fill(2);
        }
        let stride = pic.y.stride;
        let origin = pic.y.at(mb_x * 16, mb_y * 16);
        let mut win = PlaneWindow {
            data: &mut pic.y.data,
            stride,
            origin,
        };
        pred_16x16(cost.i16_mode, &mut win, nbr.b, nbr.a);
        code_luma_i16(pic, e, mb_x, mb_y, qp, &mut lv);
    }

    let chroma_mode = choose_chroma_mode(pic, e, &nbr, mb_x, mb_y);
    code_chroma(pic, e, mb_x, mb_y, qp, true, &mut lv);

    // ---- syntax ----
    if e.slice_type == SliceType::P {
        w.write_ue(e.skip_run);
        e.skip_run = 0;
    }
    let intra_type = if use_i4 {
        0
    } else {
        1 + u32::from(cost.i16_mode)
            + 4 * u32::from(lv.cbp_chroma)
            + if lv.cbp_luma != 0 { 12 } else { 0 }
    };
    let offset = if e.slice_type == SliceType::P { 5 } else { 0 };
    w.write_ue(intra_type + offset);
    if use_i4 {
        for blk in 0..16 {
            if modes[blk] == pred_modes[blk] {
                w.write_bit(true);
            } else {
                w.write_bit(false);
                let rem = if modes[blk] > pred_modes[blk] {
                    modes[blk] - 1
                } else {
                    modes[blk]
                };
                w.write_bits(u32::from(rem), 3);
            }
        }
    }
    w.write_ue(u32::from(chroma_mode));
    if use_i4 {
        write_cbp(w, lv.cbp_luma, lv.cbp_chroma, true);
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

/// Rough bit cost of a macroblock's residual, for the skip decision: a level
/// costs its magnitude's exp-Golomb-ish width, a coded block its token.
fn estimate_bits(lv: &Levels) -> i64 {
    let mut bits = 10i64;
    let block = |b: &[i32]| -> i64 {
        let mut n = 0i64;
        for &l in b {
            if l != 0 {
                n += 2 * (64 - u64::from(l.unsigned_abs()).leading_zeros() as i64) + 1;
            }
        }
        if n > 0 { n + 6 } else { 2 }
    };
    for blk in 0..16 {
        if lv.cbp_luma & (1 << (blk >> 2)) != 0 {
            bits += block(&lv.luma[blk]);
        }
    }
    if lv.cbp_chroma != 0 {
        for comp in 0..2 {
            bits += block(&lv.chroma_dc[comp][..4]);
            if lv.cbp_chroma == 2 {
                for blk in 0..4 {
                    bits += block(&lv.chroma[comp][blk][..15]);
                }
            }
        }
    }
    bits
}

/// Lagrangian multiplier in the squared-error domain, paired with
/// [`super::lambda_for`] in the SATD one.
fn lambda_ssd(qp: i32) -> f64 {
    0.85 * ((f64::from(qp) - 12.0) / 3.0).exp2()
}

/// Code a P macroblock: skip, or one 16x16 partition.
fn encode_inter_mb(
    pic: &mut Picture,
    e: &mut MbEnc<'_>,
    w: &mut BitWriter,
    mb_addr: usize,
    nbr: MbNeighbors,
    choice: InterChoice,
) {
    let mb_x = mb_addr % pic.mb_w;
    let mb_y = mb_addr / pic.mb_w;
    let reference = e.reference.expect("a P macroblock has a reference");
    let ref_id = reference.id;
    let qp = e.target_qp;
    let mv = if choice.prefer_skip {
        choice.skip_mv
    } else {
        choice.mv
    };

    compensate_mb(pic, reference, mb_x, mb_y, mv);
    // Skipping is only *available* on the P_Skip vector, and only worth taking
    // when the residual it drops is worth less than the bits it saves: on
    // static content a macroblock whose residual is pure quantisation churn
    // costs bits every picture and improves nothing.
    let may_skip = mv == choice.skip_mv;
    let ssd_skip = may_skip.then(|| ssd_mb(pic, e, mb_x, mb_y));
    let mut lv = Levels::default();
    code_luma_4x4(pic, e, mb_x, mb_y, qp, false, &mut lv);
    code_chroma(pic, e, mb_x, mb_y, qp, false, &mut lv);
    let empty = lv.cbp_luma == 0 && lv.cbp_chroma == 0;
    let skip = match ssd_skip {
        None => false,
        Some(_) if empty => true,
        Some(ssd_skip) => {
            let coded =
                ssd_mb(pic, e, mb_x, mb_y) as f64 + lambda_ssd(qp) * estimate_bits(&lv) as f64;
            if ssd_skip as f64 <= coded {
                // Put the prediction back: the residual has been added to it.
                compensate_mb(pic, reference, mb_x, mb_y, mv);
                true
            } else {
                false
            }
        }
    };

    let mut ctx = MvCtx {
        mb_x,
        mb_y,
        slice_id: e.slice_id,
        written: 0,
    };
    let w4 = pic.mb_w * 4;
    if skip {
        for py in 0..4 {
            for px in 0..4 {
                write_block(
                    pic,
                    &mut ctx,
                    px,
                    py,
                    [mv, [0, 0]],
                    [0, -1],
                    [ref_id, -1],
                    BLK_SKIP,
                );
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
        e.skip_run += 1;
        return;
    }

    let mvd = [mv[0] - choice.mvp[0], mv[1] - choice.mvp[1]];
    for py in 0..4 {
        for px in 0..4 {
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
            write_mvd(pic, mb_x, mb_y, px, py, 0, mvd);
        }
    }
    for dy in 0..4 {
        let base = (mb_y * 4 + dy) * w4 + mb_x * 4;
        pic.i4_modes[base..base + 4].fill(2);
    }

    // ---- syntax ----
    w.write_ue(e.skip_run);
    e.skip_run = 0;
    w.write_ue(0); // mb_type P_L0_16x16
    // num_ref_idx_l0_active is 1, so ref_idx_l0 is not coded.
    w.write_se(i32::from(mvd[0]));
    w.write_se(i32::from(mvd[1]));
    write_cbp(w, lv.cbp_luma, lv.cbp_chroma, false);
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
    w: &mut BitWriter,
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
        w.write_se(m.qp - e.qp);
        e.qp = m.qp;
        m.qp
    } else {
        // No residual: the QP prediction chain is untouched (7.4.5), and the
        // deblocking filter uses the chain value.
        e.qp
    };
    pic.mb_qp[mb_addr] = qp_y as u8;
    pic.mb_flags[mb_addr] = FLAG_DECODED
        | if m.is_i16 { FLAG_I16 } else { 0 }
        | if m.intra { 0 } else { FLAG_INTER }
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
        let nc = nc_of(na, nb);
        let tc = write_residual_block(w, &lv.dc, 16, nc);
        dc_cbf |= u8::from(tc != 0);
    }
    for blk in 0..16 {
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
            let nc = nc_of(na, nb);
            let (levels, max) = if m.is_i16 {
                (&lv.luma[blk][..15], 15)
            } else {
                (&lv.luma[blk][..16], 16)
            };
            write_residual_block(w, levels, max, nc)
        } else {
            0
        };
        pic.nz_y[by * w4 + bx] = tc;
    }
    // Chroma DC then chroma AC, in bitstream order.
    if lv.cbp_chroma != 0 {
        for comp in 0..2 {
            let tc = write_residual_block(w, &lv.chroma_dc[comp][..4], 4, -1);
            dc_cbf |= u8::from(tc != 0) << (1 + comp);
        }
    }
    for comp in 0..2 {
        for blk in 0..4 {
            let (cx, cy) = (cx0 + (blk & 1), cy0 + (blk >> 1));
            let tc = if lv.cbp_chroma == 2 {
                let (na, nb) = chroma_nz_pair(pic, nbr, comp, cx, cy);
                let nc = nc_of(na, nb);
                write_residual_block(w, &lv.chroma[comp][blk][..15], 15, nc)
            } else {
                0
            };
            pic.nz_c[comp][cy * w2 + cx] = tc;
        }
    }
    pic.mb_dc_cbf[mb_addr] = dc_cbf;
}

/// nC from the two neighbouring non-zero counts (9.2.1).
#[inline]
fn nc_of(na: Option<u8>, nb: Option<u8>) -> i32 {
    match (na, nb) {
        (Some(a), Some(b)) => (i32::from(a) + i32::from(b) + 1) >> 1,
        (Some(a), None) => i32::from(a),
        (None, Some(b)) => i32::from(b),
        (None, None) => 0,
    }
}

//! In-loop deblocking of an MBAFF frame (clause 8.7 with MbaffFrameFlag 1).
//!
//! Macroblocks are filtered in macroblock address order: pair by pair, top
//! macroblock then bottom. The progressive filter in [`crate::decoder`] has
//! one fixed geometry; here the geometry depends on whether the macroblock
//! and its neighbours are frame or field macroblocks:
//!
//! * a field macroblock filters its horizontal edges along the rows of its own
//!   parity (a row step of two picture rows);
//! * the left macroblock edge pairs every row of the current macroblock with
//!   the sample to its left in the *same picture row*, which belongs to
//!   different left macroblocks from row to row when the pairs differ in mode,
//!   so its strength and QP are derived per row;
//! * the top edge of a frame macroblock under a field pair is filtered twice,
//!   once per parity, against the two field macroblocks of the pair above;
//! * `mixedModeEdgeFlag` edges never get a strength below 1.
//!
//! This is the plain per-sample reading of the clause, not the fast path: the
//! progressive filter stays the one the speed budget is spent on.

use crate::deblock::{chroma_qp, edge_params, filter_chroma_line, filter_luma_line};
use crate::decoder::{BsSide, bs_between, bs_side};
use crate::dpb::{PicPlane, Picture, SliceParams};
use crate::entropy::{FLAG_DECODED, FLAG_PCM, FLAG_TRANS8X8};
use crate::sample::Sample;

/// Boundary strength of clause 8.7.2.1 for MBAFF frames.
#[allow(clippy::too_many_arguments)]
fn bs_mbaff(
    p: &BsSide,
    q: &BsSide,
    mb_edge: bool,
    vertical: bool,
    p_field: bool,
    q_field: bool,
) -> u8 {
    if p.intra || q.intra {
        return if mb_edge && (vertical || (!p_field && !q_field)) {
            4
        } else {
            3
        };
    }
    if p.coded || q.coded {
        return 2;
    }
    if p_field != q_field {
        return 1; // mixedModeEdgeFlag
    }
    // Neither intra nor coded: only the motion comparison is left. A field
    // macroblock compares vertical vectors in field units (limit 2).
    bs_between(p, q, false, q_field, vertical)
}

/// One row (vertical edge) or column (horizontal edge) group of an edge: the
/// macroblock on the p side and the strength.
#[derive(Clone, Copy)]
struct Seg {
    p_slot: usize,
    bs: u8,
}

/// Filter an MBAFF frame in place.
pub(crate) fn deblock_mbaff<S: Sample>(pic: &mut Picture<S>) {
    let w = pic.mb_w;
    let pairs = w * pic.mb_h / 2;
    for pair in 0..pairs {
        let (px, pr) = (pair % w, pair / w);
        for b in 0..2 {
            deblock_mb(pic, px, 2 * pr + b);
        }
    }
}

#[allow(clippy::needless_range_loop)]
fn deblock_mb<S: Sample>(pic: &mut Picture<S>, px: usize, slot_y: usize) {
    let w = pic.mb_w;
    let slot = slot_y * w + px;
    if pic.mb_flags[slot] & FLAG_DECODED == 0 {
        return;
    }
    let sid = pic.mb_slice[slot];
    let sp: SliceParams = pic.slices[sid as usize];
    if sp.disable_deblock_idc == 1 {
        return;
    }
    let bd = pic.bd;
    let max = (1i32 << bd) - 1;
    let qp_bd = 6 * (bd as i32 - 8);
    let pr = slot_y / 2;
    let b = slot_y & 1;
    let q_field = pic.mb_field[slot] != 0;
    let (y0, step) = if q_field {
        (pr * 32 + b, 2usize)
    } else {
        (slot_y * 16, 1)
    };
    let cmb_h = pic.cmb_h;
    let cat = pic.cat;
    let c422 = cat == 2;
    let cy0 = if q_field {
        pr * 2 * cmb_h + b
    } else {
        slot_y * cmb_h
    };
    let (bx0, by0) = (px * 4, slot_y * 4);

    let decoded = |s: usize| pic.mb_flags[s] & FLAG_DECODED != 0;
    let cross_ok = |s: usize| sp.disable_deblock_idc != 2 || pic.mb_slice[s] == sid;
    let qp_of = |s: usize| -> i32 {
        if pic.mb_flags[s] & FLAG_PCM != 0 {
            0
        } else {
            i32::from(pic.mb_qp[s])
        }
    };
    let internal = |e: usize| e.is_multiple_of(2) || pic.mb_flags[slot] & FLAG_TRANS8X8 == 0;

    // ---- boundary strengths ----
    // Vertical edges: per luma row. Edge 0 is the left macroblock edge.
    let mut bs_v = [[0u8; 16]; 4];
    // The p-side macroblock of each row of the left edge (luma) and of each
    // chroma row.
    let mut left_p = [0usize; 16];
    let mut left_pc = [0usize; 16];
    let mut filter_left = false;
    if px > 0 {
        let lt = (2 * pr) * w + px - 1;
        if decoded(lt) && cross_ok(lt) {
            filter_left = true;
            let lf = pic.mb_field[lt] != 0;
            for k in 0..16 {
                let rp = if q_field { 2 * k + b } else { 16 * b + k };
                let (ps, prow) = if lf {
                    (lt + (rp & 1) * w, rp >> 1)
                } else {
                    (lt + (rp >= 16) as usize * w, rp % 16)
                };
                left_p[k] = ps;
                let pside = bs_side(pic, (px - 1) * 4 + 3, (ps / w) * 4 + prow / 4);
                let qside = bs_side(pic, bx0, by0 + k / 4);
                bs_v[0][k] = bs_mbaff(&pside, &qside, true, true, lf, q_field);
            }
            for k in 0..cmb_h {
                let rp = if q_field { 2 * k + b } else { cmb_h * b + k };
                left_pc[k] = if lf {
                    lt + (rp & 1) * w
                } else {
                    lt + (rp >= cmb_h) as usize * w
                };
            }
        }
    }
    let cur_intra = pic.mb_flags[slot] & crate::entropy::FLAG_INTER == 0;
    for e in 1..4 {
        if !internal(e) {
            continue;
        }
        for j in 0..4 {
            let bs = if cur_intra {
                3
            } else {
                bs_mbaff(
                    &bs_side(pic, bx0 + e - 1, by0 + j),
                    &bs_side(pic, bx0 + e, by0 + j),
                    false,
                    true,
                    q_field,
                    q_field,
                )
            };
            bs_v[e][j * 4..j * 4 + 4].fill(bs);
        }
    }
    // Horizontal edges: per 4-column segment. Edge 0 may be two edges.
    let mut top: [Option<[Seg; 4]>; 2] = [None, None];
    // Row step and first q row offset of the top edge passes.
    let mut top_two = false;
    let mut top_pstep = step; // row step of the top edge (field mode: 2)
    {
        let mut single = |p_slot: usize, p_field: bool, pstep: usize, pass: usize| {
            let mut segs = [Seg { p_slot, bs: 0 }; 4];
            for j in 0..4 {
                let pside = bs_side(pic, bx0 + j, (p_slot / w) * 4 + 3);
                let qside = bs_side(pic, bx0 + j, by0);
                segs[j].bs = bs_mbaff(&pside, &qside, true, false, p_field, q_field);
            }
            top[pass] = Some(segs);
            top_pstep = pstep;
        };
        if !q_field && b == 1 {
            single(slot - w, false, 1, 0);
        } else if pr > 0 {
            let at = (2 * (pr - 1)) * w + px;
            if decoded(at) && cross_ok(at) {
                let af = pic.mb_field[at] != 0;
                if !q_field {
                    if !af {
                        single(at + w, false, 1, 0);
                    } else {
                        top_two = true;
                        single(at, true, 2, 0);
                        single(at + w, true, 2, 1);
                    }
                } else if af {
                    single(at + b * w, true, 2, 0);
                } else {
                    single(at + w, false, 2, 0);
                }
            }
        }
    }
    let mut bs_h = [[0u8; 4]; 4];
    for e in 1..4 {
        // 4:2:2 chroma filters the horizontal edges luma skips under the 8x8
        // transform, on the strength those luma edges would have had.
        if !internal(e) && !c422 {
            continue;
        }
        for j in 0..4 {
            bs_h[e][j] = if cur_intra {
                3
            } else {
                bs_mbaff(
                    &bs_side(pic, bx0 + j, by0 + e - 1),
                    &bs_side(pic, bx0 + j, by0 + e),
                    false,
                    false,
                    q_field,
                    q_field,
                )
            };
        }
    }

    // ---- luma (and Cb/Cr for ChromaArrayType 3, coded like luma) ----
    for comp in 0..if cat == 3 { 3 } else { 1 } {
        let stride = plane_of(pic, comp).stride;
        let qpf = |s: usize| -> i32 {
            if comp == 0 {
                qp_of(s)
            } else {
                let sl = pic.slices[pic.mb_slice[s] as usize];
                let off = if comp == 1 {
                    sl.cb_qp_offset
                } else {
                    sl.cr_qp_offset
                };
                let q = if pic.mb_flags[s] & FLAG_PCM != 0 {
                    0
                } else {
                    i32::from(pic.mb_qp[s])
                };
                chroma_qp(q, off, qp_bd)
            }
        };
        let lossless = |s: usize| {
            pic.bypass_enabled
                && pic.mb_flags[s] & FLAG_PCM == 0
                && i32::from(pic.mb_qp[s]) + qp_bd == 0
        };
        let vstride = stride * step;
        let qp_q = qpf(slot);
        // Vertical edges.
        for e in 0..4 {
            if e == 0 && !filter_left {
                continue;
            }
            if e > 0 && !internal(e) {
                continue;
            }
            for k in 0..16 {
                let bs = bs_v[e][k];
                if bs == 0 {
                    continue;
                }
                let qp_p = if e == 0 { qpf(left_p[k]) } else { qp_q };
                let params = edge_params(
                    (qp_p + qp_q + 1) >> 1,
                    sp.alpha_offset,
                    sp.beta_offset,
                    bs,
                    bd,
                );
                if params.alpha == 0 || params.beta == 0 {
                    continue;
                }
                let idx = match comp {
                    0 => &pic.y,
                    1 => &pic.cb,
                    _ => &pic.cr,
                }
                .at(px * 16 + e * 4, y0 + k * step);
                let (pl, ql) = (e == 0 && lossless(left_p[k]), lossless(slot));
                protect_line(
                    &mut match comp {
                        0 => &mut pic.y,
                        1 => &mut pic.cb,
                        _ => &mut pic.cr,
                    }
                    .data,
                    idx,
                    1,
                    pl,
                    ql,
                    |d| filter_luma_line(d, max, idx, 1, &params),
                );
            }
        }
        // Horizontal edges.
        for e in 0..4 {
            if e > 0 && !internal(e) {
                continue;
            }
            let passes = if e == 0 {
                if top_two { 2 } else { 1 }
            } else {
                1
            };
            for pass in 0..passes {
                let segs: [Seg; 4] = if e == 0 {
                    match top[pass] {
                        Some(s) => s,
                        None => continue,
                    }
                } else {
                    let mut s = [Seg {
                        p_slot: slot,
                        bs: 0,
                    }; 4];
                    for j in 0..4 {
                        s[j].bs = bs_h[e][j];
                    }
                    s
                };
                let (q0_row, row_step) = if e == 0 {
                    (y0 + if top_two { pass } else { 0 }, top_pstep)
                } else {
                    (y0 + e * 4 * step, step)
                };
                let hs = stride * row_step;
                for j in 0..4 {
                    let bs = segs[j].bs;
                    if bs == 0 {
                        continue;
                    }
                    let qp_p = if e == 0 { qpf(segs[j].p_slot) } else { qp_q };
                    let params = edge_params(
                        (qp_p + qp_q + 1) >> 1,
                        sp.alpha_offset,
                        sp.beta_offset,
                        bs,
                        bd,
                    );
                    if params.alpha == 0 || params.beta == 0 {
                        continue;
                    }
                    for kx in 0..4 {
                        let idx = match comp {
                            0 => &pic.y,
                            1 => &pic.cb,
                            _ => &pic.cr,
                        }
                        .at(px * 16 + j * 4 + kx, q0_row);
                        let (pl, ql) = (e == 0 && lossless(segs[j].p_slot), lossless(slot));
                        protect_line(
                            &mut match comp {
                                0 => &mut pic.y,
                                1 => &mut pic.cb,
                                _ => &mut pic.cr,
                            }
                            .data,
                            idx,
                            hs,
                            pl,
                            ql,
                            |d| filter_luma_line(d, max, idx, hs, &params),
                        );
                    }
                }
            }
        }
        let _ = vstride;
    }

    // ---- chroma ----
    for comp in 0..if matches!(cat, 1 | 2) { 2 } else { 0 } {
        let off_sel = |s: &SliceParams| {
            if comp == 0 {
                s.cb_qp_offset
            } else {
                s.cr_qp_offset
            }
        };
        let cqp = |s: usize| -> i32 {
            let sl = pic.slices[pic.mb_slice[s] as usize];
            if pic.mb_flags[s] & FLAG_PCM != 0 {
                chroma_qp(0, off_sel(&sl), qp_bd)
            } else {
                chroma_qp(i32::from(pic.mb_qp[s]), off_sel(&sl), qp_bd)
            }
        };
        let cq_q = cqp(slot);
        let plane: &PicPlane<S> = if comp == 0 { &pic.cb } else { &pic.cr };
        let stride = plane.stride;
        // Vertical edges (chroma x = 0, 4 <-> luma edges 0, 2).
        for e in 0..2 {
            if e == 0 && !filter_left {
                continue;
            }
            for k in 0..cmb_h {
                // The luma row whose strength a chroma row inherits.
                let lrow = if c422 {
                    k
                } else if q_field && e == 0 && {
                    let lt = (2 * pr) * w + px - 1;
                    pic.mb_field[lt] == 0
                } {
                    k * 2
                } else {
                    (k >> 1) * 4 + (k & 1)
                };
                let bs = bs_v[e * 2][lrow];
                if bs == 0 {
                    continue;
                }
                let cq_p = if e == 0 { cqp(left_pc[k]) } else { cq_q };
                let params = edge_params(
                    (cq_p + cq_q + 1) >> 1,
                    sp.alpha_offset,
                    sp.beta_offset,
                    bs,
                    bd,
                );
                if params.alpha == 0 || params.beta == 0 {
                    continue;
                }
                let plane = if comp == 0 { &mut pic.cb } else { &mut pic.cr };
                let idx = plane.at(px * 8 + e * 4, cy0 + k * step);
                filter_chroma_line(&mut plane.data, max, idx, 1, &params);
            }
        }
        // Horizontal edges (chroma y = 0, 4 <-> luma edges 0, 2; for 4:2:2
        // chroma y = 0, 4, 8, 12 <-> luma edges 0..3).
        for e in 0..if c422 { 4 } else { 2 } {
            let passes = if e == 0 {
                if top_two { 2 } else { 1 }
            } else {
                1
            };
            for pass in 0..passes {
                let segs: [Seg; 4] = if e == 0 {
                    match top[pass] {
                        Some(s) => s,
                        None => continue,
                    }
                } else {
                    let mut s = [Seg {
                        p_slot: slot,
                        bs: 0,
                    }; 4];
                    for j in 0..4 {
                        s[j].bs = bs_h[if c422 { e } else { 2 }][j];
                    }
                    s
                };
                let (q0_row, row_step) = if e == 0 {
                    (cy0 + if top_two { pass } else { 0 }, top_pstep)
                } else {
                    (cy0 + (if c422 { e * 4 } else { 4 }) * step, step)
                };
                let hs = stride * row_step;
                for k in 0..8 {
                    let seg = segs[k / 2];
                    if seg.bs == 0 {
                        continue;
                    }
                    let cq_p = if e == 0 { cqp(seg.p_slot) } else { cq_q };
                    let params = edge_params(
                        (cq_p + cq_q + 1) >> 1,
                        sp.alpha_offset,
                        sp.beta_offset,
                        seg.bs,
                        bd,
                    );
                    if params.alpha == 0 || params.beta == 0 {
                        continue;
                    }
                    let plane = if comp == 0 { &mut pic.cb } else { &mut pic.cr };
                    let idx = plane.at(px * 8 + k, q0_row);
                    filter_chroma_line(&mut plane.data, max, idx, hs, &params);
                }
            }
        }
    }
}

fn plane_of<S: Sample>(pic: &Picture<S>, comp: usize) -> &PicPlane<S> {
    match comp {
        0 => &pic.y,
        1 => &pic.cb,
        _ => &pic.cr,
    }
}

/// Run one line filter, then put back the samples of a lossless side
/// (8.7.2.2: transform-bypass macroblocks are never modified).
fn protect_line<S: Sample>(
    data: &mut [S],
    idx: usize,
    step: usize,
    p_lossless: bool,
    q_lossless: bool,
    f: impl FnOnce(&mut [S]),
) {
    if !p_lossless && !q_lossless {
        f(data);
        return;
    }
    let mut saved = [data[idx]; 8];
    for (i, v) in saved.iter_mut().enumerate() {
        *v = data[idx + i * step - 4 * step];
    }
    f(data);
    for (i, v) in saved.iter().enumerate() {
        if (i < 4 && p_lossless) || (i >= 4 && q_lossless) {
            data[idx + i * step - 4 * step] = *v;
        }
    }
}

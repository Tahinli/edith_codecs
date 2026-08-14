//! The deblocking filter (Rec. ITU-T H.264 clause 8.7).
//!
//! The filter runs over the whole picture once every macroblock of it has been
//! constructed, in macroblock raster order, vertical edges before horizontal
//! ones. Running it at the end rather than per macroblock is the same result by
//! construction — intra prediction reads unfiltered samples, and the filter
//! itself only ever reads samples of edges it has already reached — and it
//! keeps the "unfiltered for prediction, filtered for output" rule impossible
//! to get wrong.

use ec_h264_syntax::pps::PicParameterSet;

use crate::picture::{MbKind, Picture};
use crate::tables::{ALPHA, BETA, TC0, qpc_from_qpi};

/// One line of samples across an edge: `p[3..0] | q[0..3]`.
#[derive(Debug, Clone, Copy)]
struct Line {
    p: [i32; 4],
    q: [i32; 4],
}

/// Filter every edge of the picture (clause 8.7).
pub fn deblock_picture(picture: &mut Picture, pps: &PicParameterSet) {
    for mb_y in 0..picture.height_mbs {
        for mb_x in 0..picture.width_mbs {
            deblock_macroblock(picture, pps, mb_x, mb_y);
        }
    }
}

fn deblock_macroblock(picture: &mut Picture, pps: &PicParameterSet, mb_x: usize, mb_y: usize) {
    let info = *picture.mb_at(mb_x, mb_y);
    if info.slice_id < 0 || info.disable_deblocking_filter_idc == 1 {
        return;
    }
    // Clause 8.7: an edge on the picture boundary is never filtered, and with
    // disable_deblocking_filter_idc equal to 2 neither is one that crosses into
    // another slice.
    let neighbour_in_slice = |dx: isize, dy: isize| -> bool {
        picture.mb_available_at(
            mb_x as isize * 16 + dx,
            mb_y as isize * 16 + dy,
            info.slice_id,
        )
    };
    let filter_left =
        mb_x > 0 && (info.disable_deblocking_filter_idc != 2 || neighbour_in_slice(-1, 0));
    let filter_top =
        mb_y > 0 && (info.disable_deblocking_filter_idc != 2 || neighbour_in_slice(0, -1));

    // Vertical edges, left to right.
    for k in 0..4usize {
        if k == 0 && !filter_left {
            continue;
        }
        let x = mb_x * 16 + k * 4;
        let bs = if k == 0 { 4 } else { 3 };
        let qp_other = qp_of(picture, x as isize - 1, (mb_y * 16) as isize);
        for row in 0..16usize {
            let y = mb_y * 16 + row;
            filter_luma(
                picture,
                pps,
                x,
                y,
                true,
                bs,
                info.qpy_for_filter(),
                qp_other,
                &info,
            );
        }
        // 4:2:0 chroma has edges only where the luma edge index is even.
        if k % 2 == 0 {
            let cx = mb_x * 8 + k * 2;
            for row in 0..8usize {
                let cy = mb_y * 8 + row;
                filter_chroma(
                    picture,
                    pps,
                    cx,
                    cy,
                    true,
                    bs,
                    info.qpy_for_filter(),
                    qp_other,
                    &info,
                );
            }
        }
    }

    // Horizontal edges, top to bottom.
    for k in 0..4usize {
        if k == 0 && !filter_top {
            continue;
        }
        let y = mb_y * 16 + k * 4;
        let bs = if k == 0 { 4 } else { 3 };
        let qp_other = qp_of(picture, (mb_x * 16) as isize, y as isize - 1);
        for col in 0..16usize {
            let x = mb_x * 16 + col;
            filter_luma(
                picture,
                pps,
                x,
                y,
                false,
                bs,
                info.qpy_for_filter(),
                qp_other,
                &info,
            );
        }
        if k % 2 == 0 {
            let cy = mb_y * 8 + k * 2;
            for col in 0..8usize {
                let cx = mb_x * 8 + col;
                filter_chroma(
                    picture,
                    pps,
                    cx,
                    cy,
                    false,
                    bs,
                    info.qpy_for_filter(),
                    qp_other,
                    &info,
                );
            }
        }
    }
}

/// `QPY` of the macroblock containing luma sample `(x, y)`, which clause 8.7.2
/// takes as 0 for an `I_PCM` macroblock.
fn qp_of(picture: &Picture, x: isize, y: isize) -> i32 {
    if x < 0 || y < 0 {
        return 0;
    }
    let info = picture.mb_at(x as usize / 16, y as usize / 16);
    if info.kind == MbKind::IPcm {
        0
    } else {
        info.qpy
    }
}

impl crate::picture::MbInfo {
    /// `qPq` for this macroblock: clause 8.7.2 filters an `I_PCM` macroblock as
    /// if its `QPY` were 0.
    fn qpy_for_filter(&self) -> i32 {
        if self.kind == MbKind::IPcm {
            0
        } else {
            self.qpy
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn filter_luma(
    picture: &mut Picture,
    _pps: &PicParameterSet,
    x: usize,
    y: usize,
    vertical: bool,
    bs: usize,
    qp_q: i32,
    qp_p: i32,
    info: &crate::picture::MbInfo,
) {
    let read = |i: isize| -> i32 {
        let (sx, sy) = offset(x, y, i, vertical);
        picture.luma_at(sx, sy) as i32
    };
    let line = Line {
        p: [read(-1), read(-2), read(-3), read(-4)],
        q: [read(0), read(1), read(2), read(3)],
    };
    let qp_av = (qp_p + qp_q + 1) >> 1;
    let index_a = (qp_av + info.filter_offset_a).clamp(0, 51) as usize;
    let index_b = (qp_av + info.filter_offset_b).clamp(0, 51) as usize;
    let (alpha, beta) = (ALPHA[index_a], BETA[index_b]);
    if !filter_samples_flag(&line, alpha, beta) {
        return;
    }
    let out = if bs == 4 {
        filter_bs4(&line, alpha, beta, false)
    } else {
        filter_bs_below_4(&line, beta, TC0[index_a][bs - 1], false)
    };
    for (i, value) in out.p.iter().enumerate().take(3) {
        let (sx, sy) = offset(x, y, -1 - i as isize, vertical);
        picture.set_luma(sx, sy, *value as u8);
    }
    for (i, value) in out.q.iter().enumerate().take(3) {
        let (sx, sy) = offset(x, y, i as isize, vertical);
        picture.set_luma(sx, sy, *value as u8);
    }
}

#[allow(clippy::too_many_arguments)]
fn filter_chroma(
    picture: &mut Picture,
    pps: &PicParameterSet,
    x: usize,
    y: usize,
    vertical: bool,
    bs: usize,
    qp_q: i32,
    qp_p: i32,
    info: &crate::picture::MbInfo,
) {
    for i_cb_cr in 0..2usize {
        let offset_index = if i_cb_cr == 0 {
            pps.chroma_qp_index_offset
        } else {
            pps.second_chroma_qp_index_offset
        };
        let qp_p_c = qpc_from_qpi((qp_p + offset_index).clamp(0, 51));
        let qp_q_c = qpc_from_qpi((qp_q + offset_index).clamp(0, 51));
        let read = |i: isize| -> i32 {
            let (sx, sy) = offset(x, y, i, vertical);
            picture.chroma_at(i_cb_cr, sx, sy) as i32
        };
        // Only p1..q1 are read for chroma, but the shared Line keeps the
        // formulas identical to the luma ones.
        let line = Line {
            p: [read(-1), read(-2), read(-2), read(-2)],
            q: [read(0), read(1), read(1), read(1)],
        };
        let qp_av = (qp_p_c + qp_q_c + 1) >> 1;
        let index_a = (qp_av + info.filter_offset_a).clamp(0, 51) as usize;
        let index_b = (qp_av + info.filter_offset_b).clamp(0, 51) as usize;
        let (alpha, beta) = (ALPHA[index_a], BETA[index_b]);
        if !filter_samples_flag(&line, alpha, beta) {
            continue;
        }
        let out = if bs == 4 {
            filter_bs4(&line, alpha, beta, true)
        } else {
            filter_bs_below_4(&line, beta, TC0[index_a][bs - 1], true)
        };
        let (px, py) = offset(x, y, -1, vertical);
        picture.set_chroma(i_cb_cr, px, py, out.p[0] as u8);
        let (qx, qy) = offset(x, y, 0, vertical);
        picture.set_chroma(i_cb_cr, qx, qy, out.q[0] as u8);
    }
}

/// Sample coordinates `i` steps across the edge at `(x, y)`: negative `i` is
/// the p side, non-negative the q side.
fn offset(x: usize, y: usize, i: isize, vertical: bool) -> (usize, usize) {
    if vertical {
        ((x as isize + i) as usize, y)
    } else {
        (x, (y as isize + i) as usize)
    }
}

/// `filterSamplesFlag` (clause 8.7.2.1) with `bS` already known to be non-zero.
fn filter_samples_flag(line: &Line, alpha: i32, beta: i32) -> bool {
    (line.p[0] - line.q[0]).abs() < alpha
        && (line.p[1] - line.p[0]).abs() < beta
        && (line.q[1] - line.q[0]).abs() < beta
}

/// Clause 8.7.2.3: filtering for edges with `bS < 4`.
fn filter_bs_below_4(line: &Line, beta: i32, tc0: i32, chroma: bool) -> Line {
    let (p, q) = (line.p, line.q);
    let ap = (p[2] - p[0]).abs();
    let aq = (q[2] - q[0]).abs();
    let tc = if chroma {
        tc0 + 1
    } else {
        tc0 + i32::from(ap < beta) + i32::from(aq < beta)
    };
    let delta = clip3(-tc, tc, (((q[0] - p[0]) << 2) + (p[1] - q[1]) + 4) >> 3);
    let mut out = *line;
    out.p[0] = clip1(p[0] + delta);
    out.q[0] = clip1(q[0] - delta);
    if !chroma && ap < beta {
        out.p[1] = p[1]
            + clip3(
                -tc0,
                tc0,
                (p[2] + ((p[0] + q[0] + 1) >> 1) - (p[1] << 1)) >> 1,
            );
    }
    if !chroma && aq < beta {
        out.q[1] = q[1]
            + clip3(
                -tc0,
                tc0,
                (q[2] + ((p[0] + q[0] + 1) >> 1) - (q[1] << 1)) >> 1,
            );
    }
    out
}

/// Clause 8.7.2.4: filtering for edges with `bS == 4`.
fn filter_bs4(line: &Line, alpha: i32, beta: i32, chroma: bool) -> Line {
    let (p, q) = (line.p, line.q);
    let ap = (p[2] - p[0]).abs();
    let aq = (q[2] - q[0]).abs();
    let strong = (p[0] - q[0]).abs() < ((alpha >> 2) + 2);
    let mut out = *line;
    if !chroma && ap < beta && strong {
        out.p[0] = (p[2] + 2 * p[1] + 2 * p[0] + 2 * q[0] + q[1] + 4) >> 3;
        out.p[1] = (p[2] + p[1] + p[0] + q[0] + 2) >> 2;
        out.p[2] = (2 * p[3] + 3 * p[2] + p[1] + p[0] + q[0] + 4) >> 3;
    } else {
        out.p[0] = (2 * p[1] + p[0] + q[1] + 2) >> 2;
    }
    if !chroma && aq < beta && strong {
        out.q[0] = (q[2] + 2 * q[1] + 2 * q[0] + 2 * p[0] + p[1] + 4) >> 3;
        out.q[1] = (q[2] + q[1] + q[0] + p[0] + 2) >> 2;
        out.q[2] = (2 * q[3] + 3 * q[2] + q[1] + q[0] + p[0] + 4) >> 3;
    } else {
        out.q[0] = (2 * q[1] + q[0] + p[1] + 2) >> 2;
    }
    out
}

fn clip3(low: i32, high: i32, value: i32) -> i32 {
    value.clamp(low, high)
}

fn clip1(value: i32) -> i32 {
    value.clamp(0, 255)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(p: [i32; 4], q: [i32; 4]) -> Line {
        Line { p, q }
    }

    #[test]
    fn a_real_edge_is_smoothed_and_a_ramp_is_left_alone() {
        // A step of 8 at the edge, well inside alpha and beta: the filter pulls
        // p0 and q0 towards each other and leaves p2/q2 untouched.
        let l = line([100, 100, 100, 100], [108, 108, 108, 108]);
        let out = filter_bs_below_4(&l, 4, 2, false);
        assert!(out.p[0] > 100 && out.q[0] < 108, "{out:?}");
        assert_eq!(out.p[2], 100);
        assert_eq!(out.q[2], 108);
        // A gradient steeper than alpha is a genuine edge in the picture and
        // must survive: filterSamplesFlag is false.
        let steep = line([10, 10, 10, 10], [200, 200, 200, 200]);
        assert!(!filter_samples_flag(&steep, ALPHA[30], BETA[30]));
    }

    #[test]
    fn bs4_strong_filter_touches_three_samples_each_side() {
        let l = line([100, 100, 100, 100], [120, 120, 120, 120]);
        let out = filter_bs4(&l, 255, 18, false);
        assert_ne!(out.p[2], l.p[2], "the strong filter reaches p2");
        assert_ne!(out.q[2], l.q[2]);
        assert!(out.p[0] > 100 && out.q[0] < 120);
        // Chroma only ever changes p0 and q0.
        let out = filter_bs4(&l, 255, 18, true);
        assert_eq!(out.p[1], l.p[1]);
        assert_eq!(out.q[1], l.q[1]);
        assert_eq!(out.p[0], (2 * 100 + 100 + 120 + 2) >> 2);
    }

    #[test]
    fn flat_picture_survives_the_filter_unchanged() {
        // Every filter in clause 8.7 has unit gain on a constant signal.
        let l = line([77; 4], [77; 4]);
        assert_eq!(filter_bs_below_4(&l, 18, 25, false).p[0], 77);
        assert_eq!(filter_bs_below_4(&l, 18, 25, false).q[1], 77);
        assert_eq!(filter_bs4(&l, 255, 18, false).p[0], 77);
        assert_eq!(filter_bs4(&l, 255, 18, false).p[2], 77);
        assert_eq!(filter_bs4(&l, 255, 18, true).q[0], 77);
    }

    #[test]
    fn deblocking_leaves_a_flat_picture_alone() {
        let mut picture = Picture::new(2, 2);
        for sample in picture.luma.iter_mut() {
            *sample = 128;
        }
        for mb in picture.mb.iter_mut() {
            mb.slice_id = 0;
            mb.qpy = 30;
        }
        let before = picture.luma.clone();
        let pps = crate::tests_support::flat_pps();
        deblock_picture(&mut picture, &pps);
        assert_eq!(picture.luma, before);
    }
}

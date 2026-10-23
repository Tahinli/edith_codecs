//! The in-loop deblocking filter of the decoder (8.7.2).
//!
//! The filter runs over row ranges of the picture ([`deblock_rows`]). In each range
//! all vertical edges go first, luma and chroma, then all horizontal edges over what
//! that pass left; see [`super::rowfilter`] for the order the ranges run in.
//! Luma edges sit on the 8x8 luma grid. Chroma of a 4:2:0 picture sits on its
//! own 8x8 grid (the luma grid of 16) and is filtered only where the boundary
//! strength is 2. Boundary strengths, filter switches and offsets come from the
//! block maps; the samples are read and rewritten in place.

use super::pic::{BlockInfo, CtbInfo, EDGE_TU, PicState, PuMotion, SliceFilter};
use crate::transform::chroma_qp_for;
use std::ops::Range;

/// beta' by Q (table 8-12, first column).
const BETA: [i32; 52] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
    20, 22, 24, 26, 28, 30, 32, 34, 36, 38, 40, 42, 44, 46, 48, 50, 52, 54, 56, 58, 60, 62, 64,
];

/// tC' by Q (table 8-12, second column).
const TC: [i32; 54] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 3,
    3, 3, 3, 4, 4, 4, 5, 5, 6, 6, 7, 8, 9, 10, 11, 13, 14, 16, 18, 20, 22, 24,
];

/// Everything the filter reads from the picture, borrowed apart from the
/// sample planes so that those can be rewritten while the maps are in use.
struct Maps<'a> {
    info: &'a [BlockInfo],
    motion: &'a [PuMotion],
    ctb: &'a [CtbInfo],
    slices: &'a [SliceFilter],
    w4: usize,
    ctb_log2: u32,
    ctb_cols: usize,
    width: usize,
    /// `BitDepthY` and `BitDepthC`.
    bit_depth: [u32; 2],
    /// `pps_cb_qp_offset` and `pps_cr_qp_offset`.
    cqp_offset: [i32; 2],
    /// The log2 chroma subsampling `(sub_x, sub_y)`.
    sub: (u32, u32),
    /// `chroma_format_idc`, which picks the chroma QP mapping (Table 8-10 or Min(qPi, 51)).
    chroma_format_idc: u32,
    pcm_lf_off: bool,
    /// `loop_filter_across_tiles_enabled_flag`.
    across_tiles: bool,
}

impl Maps<'_> {
    /// The index of the 4x4 unit containing luma position `(x, y)`.
    fn unit(&self, x: usize, y: usize) -> usize {
        (y >> 2) * self.w4 + (x >> 2)
    }

    /// The CTB raster address of luma position `(x, y)`.
    fn ctb_addr(&self, x: usize, y: usize) -> usize {
        (y >> self.ctb_log2) * self.ctb_cols + (x >> self.ctb_log2)
    }

    /// The slice filter parameters that govern the edge between the luma
    /// positions `p` and `q` (the slice of `q`), or `None` when filterEdgeFlag
    /// is 0: the slice of `q` has deblocking disabled, or the edge is a slice
    /// boundary that the slice of `q` does not filter across.
    fn filtered_edge(&self, p: (usize, usize), q: (usize, usize)) -> Option<SliceFilter> {
        let cq = self.ctb.get(self.ctb_addr(q.0, q.1))?;
        let sf = *self.slices.get(usize::from(cq.slice))?;
        if sf.deblock_disabled {
            return None;
        }
        let cp = self.ctb.get(self.ctb_addr(p.0, p.1))?;
        if cp.slice_addr != cq.slice_addr && !sf.loop_filter_across_slices {
            return None;
        }
        if cp.tile != cq.tile && !self.across_tiles {
            return None;
        }
        Some(sf)
    }

    /// Whether the samples of unit `u` must stay as they are: a lossless
    /// (transquant bypass) unit, or a PCM unit with the PCM loop filter off.
    fn locked(&self, u: usize) -> bool {
        let b = &self.info[u];
        b.bypass || (b.pcm && self.pcm_lf_off)
    }

    /// The boundary strength (8.7.2.4) across the edge between units `p` and
    /// `q`. `tu` says the edge is a transform block edge.
    fn strength(&self, p: usize, q: usize, tu: bool) -> u8 {
        let (bp, bq) = (&self.info[p], &self.info[q]);
        if bp.intra || bq.intra {
            2
        } else if tu && (bp.nz_luma || bq.nz_luma) {
            1
        } else {
            motion_strength(&self.motion[p], &self.motion[q])
        }
    }
}

/// The motion part of bS (8.7.2.4): 1 when the reference pictures or the
/// motion vectors of the two units differ enough, 0 otherwise.
fn motion_strength(p: &PuMotion, q: &PuMotion) -> u8 {
    let count = |m: &PuMotion| usize::from(m.pred[0]) + usize::from(m.pred[1]);
    let (np, nq) = (count(p), count(q));
    if np != nq {
        return 1;
    }
    match np {
        0 => 0,
        1 => {
            let lp = if p.pred[0] { 0 } else { 1 };
            let lq = if q.pred[0] { 0 } else { 1 };
            if p.ref_pic[lp] != q.ref_pic[lq] {
                1
            } else {
                u8::from(mv_far(p.mv[lp], q.mv[lq]))
            }
        }
        _ => {
            let same_set = (p.ref_pic[0] == q.ref_pic[0] && p.ref_pic[1] == q.ref_pic[1])
                || (p.ref_pic[0] == q.ref_pic[1] && p.ref_pic[1] == q.ref_pic[0]);
            if !same_set {
                1
            } else if p.ref_pic[0] != p.ref_pic[1] {
                // Compare the motion vectors that refer to the same picture.
                let (a, b) = if p.ref_pic[0] == q.ref_pic[0] {
                    (0, 1)
                } else {
                    (1, 0)
                };
                u8::from(mv_far(p.mv[0], q.mv[a]) || mv_far(p.mv[1], q.mv[b]))
            } else {
                // Both vectors of each unit refer to one picture: either
                // pairing may match, so bS is 1 only when both fail.
                let straight = mv_far(p.mv[0], q.mv[0]) || mv_far(p.mv[1], q.mv[1]);
                let crossed = mv_far(p.mv[0], q.mv[1]) || mv_far(p.mv[1], q.mv[0]);
                u8::from(straight && crossed)
            }
        }
    }
}

/// Whether two motion vectors differ by at least one integer sample (4 quarter
/// samples) in either component.
fn mv_far(a: [i32; 2], b: [i32; 2]) -> bool {
    a[0].abs_diff(b[0]) >= 4 || a[1].abs_diff(b[1]) >= 4
}

/// One line of samples across an edge: `at` is the index of q0 and `step` the
/// distance between neighbouring samples across the edge.
#[derive(Clone, Copy)]
struct Line {
    at: usize,
    step: usize,
}

impl Line {
    fn p(&self, plane: &[u16], i: usize) -> i32 {
        i32::from(plane[self.at - (i + 1) * self.step])
    }

    fn q(&self, plane: &[u16], i: usize) -> i32 {
        i32::from(plane[self.at + i * self.step])
    }

    fn set_p(&self, plane: &mut [u16], i: usize, value: i32, max: i32) {
        plane[self.at - (i + 1) * self.step] = value.clamp(0, max) as u16;
    }

    fn set_q(&self, plane: &mut [u16], i: usize, value: i32, max: i32) {
        plane[self.at + i * self.step] = value.clamp(0, max) as u16;
    }
}

/// The second-derivative activity of the p side and of the q side of a line.
fn activity(line: &Line, plane: &[u16]) -> (i32, i32) {
    let dp = (line.p(plane, 2) - 2 * line.p(plane, 1) + line.p(plane, 0)).abs();
    let dq = (line.q(plane, 2) - 2 * line.q(plane, 1) + line.q(plane, 0)).abs();
    (dp, dq)
}

/// Whether one line wants the strong filter (8.7.2.5.6).
fn strong_line(line: &Line, dpq: i32, plane: &[u16], beta: i32, tc: i32) -> bool {
    2 * dpq < (beta >> 2)
        && (line.p(plane, 3) - line.p(plane, 0)).abs() + (line.q(plane, 0) - line.q(plane, 3)).abs()
            < (beta >> 3)
        && (line.p(plane, 0) - line.q(plane, 0)).abs() < ((5 * tc + 1) >> 1)
}

/// Filter one four-line luma edge segment (8.7.2.5.3, 8.7.2.5.7). `beta` and
/// `tc` are already scaled to the bit depth. `locked` is the p and q side
/// switch of nDp and nDq: a locked side is read but not written.
fn luma_segment(
    plane: &mut [u16],
    lines: [Line; 4],
    beta: i32,
    tc: i32,
    locked: [bool; 2],
    max: i32,
) {
    let [no_p, no_q] = locked;
    let (dp0, dq0) = activity(&lines[0], plane);
    let (dp3, dq3) = activity(&lines[3], plane);
    let (dpq0, dpq3) = (dp0 + dq0, dp3 + dq3);
    if dpq0 + dpq3 >= beta {
        return;
    }
    let strong = strong_line(&lines[0], dpq0, plane, beta, tc)
        && strong_line(&lines[3], dpq3, plane, beta, tc);
    let side_p1 = dp0 + dp3 < ((beta + (beta >> 1)) >> 3);
    let side_q1 = dq0 + dq3 < ((beta + (beta >> 1)) >> 3);

    for line in &lines {
        let p: [i32; 4] = std::array::from_fn(|i| line.p(plane, i));
        let q: [i32; 4] = std::array::from_fn(|i| line.q(plane, i));
        if strong {
            let clip = |base: i32, v: i32| v.clamp(base - 2 * tc, base + 2 * tc);
            if !no_p {
                let p0 = clip(
                    p[0],
                    (p[2] + 2 * p[1] + 2 * p[0] + 2 * q[0] + q[1] + 4) >> 3,
                );
                let p1 = clip(p[1], (p[2] + p[1] + p[0] + q[0] + 2) >> 2);
                let p2 = clip(p[2], (2 * p[3] + 3 * p[2] + p[1] + p[0] + q[0] + 4) >> 3);
                line.set_p(plane, 0, p0, max);
                line.set_p(plane, 1, p1, max);
                line.set_p(plane, 2, p2, max);
            }
            if !no_q {
                let q0 = clip(
                    q[0],
                    (p[1] + 2 * p[0] + 2 * q[0] + 2 * q[1] + q[2] + 4) >> 3,
                );
                let q1 = clip(q[1], (p[0] + q[0] + q[1] + q[2] + 2) >> 2);
                let q2 = clip(q[2], (p[0] + q[0] + q[1] + 3 * q[2] + 2 * q[3] + 4) >> 3);
                line.set_q(plane, 0, q0, max);
                line.set_q(plane, 1, q1, max);
                line.set_q(plane, 2, q2, max);
            }
        } else {
            let delta = (9 * (q[0] - p[0]) - 3 * (q[1] - p[1]) + 8) >> 4;
            if delta.abs() >= tc * 10 {
                continue;
            }
            let delta = delta.clamp(-tc, tc);
            if !no_p {
                line.set_p(plane, 0, p[0] + delta, max);
                if side_p1 {
                    let dp =
                        ((((p[2] + p[0] + 1) >> 1) - p[1] + delta) >> 1).clamp(-(tc >> 1), tc >> 1);
                    line.set_p(plane, 1, p[1] + dp, max);
                }
            }
            if !no_q {
                line.set_q(plane, 0, q[0] - delta, max);
                if side_q1 {
                    let dq =
                        ((((q[2] + q[0] + 1) >> 1) - q[1] - delta) >> 1).clamp(-(tc >> 1), tc >> 1);
                    line.set_q(plane, 1, q[1] + dq, max);
                }
            }
        }
    }
}

/// Filter one chroma line across an edge (8.7.2.5.5). `tc` is already scaled.
fn chroma_line(plane: &mut [u16], line: Line, tc: i32, locked: [bool; 2], max: i32) {
    let (p0, p1) = (line.p(plane, 0), line.p(plane, 1));
    let (q0, q1) = (line.q(plane, 0), line.q(plane, 1));
    let delta = ((((q0 - p0) * 4) + p1 - q1 + 4) >> 3).clamp(-tc, tc);
    if !locked[0] {
        line.set_p(plane, 0, p0 + delta, max);
    }
    if !locked[1] {
        line.set_q(plane, 0, q0 - delta, max);
    }
}

/// Every luma edge of one direction, over the whole picture. Edges are on the
/// 8x8 grid and skipped at the picture boundary; each 4-sample segment takes
/// its parameters from the units on either side of it.
///
/// `plane` holds the rows from `row0` down, and `rows` are the picture rows whose
/// edges this call filters (a multiple of 8 at each end, or the picture's own
/// edge). A vertical pass only writes within each row, so bands of rows are
/// independent; the horizontal pass reads and writes across rows and is only
/// run whole.
fn luma_pass(m: &Maps, plane: &mut [u16], row0: usize, rows: Range<usize>, vertical: bool) {
    let w = m.width;
    let scale = 1i32 << m.bit_depth[0].saturating_sub(8);
    let max = (1i32 << m.bit_depth[0]) - 1;
    for y in rows.clone().step_by(4) {
        for x in (0..w).step_by(4) {
            let (p, lines) = if vertical {
                if x == 0 || !x.is_multiple_of(8) {
                    continue;
                }
                (
                    (x - 1, y),
                    std::array::from_fn(|i| Line {
                        at: (y + i - row0) * w + x,
                        step: 1,
                    }),
                )
            } else {
                if y == 0 || !y.is_multiple_of(8) {
                    continue;
                }
                (
                    (x, y - 1),
                    std::array::from_fn(|i| Line {
                        at: (y - row0) * w + x + i,
                        step: w,
                    }),
                )
            };
            let qi = m.unit(x, y);
            let edge = if vertical {
                m.info[qi].edge_v
            } else {
                m.info[qi].edge_h
            };
            if edge == 0 {
                continue;
            }
            let Some(sf) = m.filtered_edge(p, (x, y)) else {
                continue;
            };
            let pi = m.unit(p.0, p.1);
            let bs = m.strength(pi, qi, edge & EDGE_TU != 0);
            if bs == 0 {
                continue;
            }
            let qp_l = (i32::from(m.info[qi].qp) + i32::from(m.info[pi].qp) + 1) >> 1;
            let beta = BETA[(qp_l + 2 * sf.beta_offset_div2).clamp(0, 51) as usize] * scale;
            let tc = TC
                [(qp_l + 2 * (i32::from(bs) - 1) + 2 * sf.tc_offset_div2).clamp(0, 53) as usize]
                * scale;
            let locked = [m.locked(pi), m.locked(qi)];
            luma_segment(plane, lines, beta, tc, locked, max);
        }
    }
}

/// Every chroma edge of one direction (4:2:0). A chroma segment of four samples
/// takes its boundary strength and QP from the luma unit at its first row (or
/// column) scaled by two, as 8.7.2.5.2 does; only bS 2 is filtered.
///
/// Band semantics as in [`luma_pass`], in chroma rows: `planes` hold the rows from
/// `row0` down, and `rows` are chroma rows.
fn chroma_pass(
    m: &Maps,
    planes: &mut [&mut [u16]; 2],
    row0: usize,
    rows: Range<usize>,
    vertical: bool,
) {
    let cw = m.width >> m.sub.0;
    let scale = 1i32 << m.bit_depth[1].saturating_sub(8);
    let max = (1i32 << m.bit_depth[1]) - 1;
    for yc in rows.clone().step_by(4) {
        for xc in (0..cw).step_by(4) {
            let (xl, yl) = (xc << m.sub.0, yc << m.sub.1);
            let p = if vertical {
                if xc == 0 || !xc.is_multiple_of(8) {
                    continue;
                }
                (xl - 1, yl)
            } else {
                if yc == 0 || !yc.is_multiple_of(8) {
                    continue;
                }
                (xl, yl - 1)
            };
            let qi = m.unit(xl, yl);
            let edge = if vertical {
                m.info[qi].edge_v
            } else {
                m.info[qi].edge_h
            };
            if edge == 0 {
                continue;
            }
            let Some(sf) = m.filtered_edge(p, (xl, yl)) else {
                continue;
            };
            let pi = m.unit(p.0, p.1);
            if m.strength(pi, qi, edge & EDGE_TU != 0) != 2 {
                continue;
            }
            let qp_avg = (i32::from(m.info[qi].qp) + i32::from(m.info[pi].qp) + 1) >> 1;
            let locked = [m.locked(pi), m.locked(qi)];
            for (c, plane) in planes.iter_mut().enumerate() {
                let qpc =
                    chroma_qp_for(qp_avg, m.cqp_offset[c], m.bit_depth[1], m.chroma_format_idc);
                let tc = TC[(qpc + 2 + 2 * sf.tc_offset_div2).clamp(0, 53) as usize] * scale;
                for k in 0..4 {
                    let line = if vertical {
                        Line {
                            at: (yc + k - row0) * cw + xc,
                            step: 1,
                        }
                    } else {
                        Line {
                            at: (yc - row0) * cw + xc + k,
                            step: cw,
                        }
                    };
                    chroma_line(plane, line, tc, locked, max);
                }
            }
        }
    }
}

/// Deblock the whole picture in place (8.7.2). A picture without sample planes, or
/// with dimensions that are not multiples of 8 (never the case for a conforming
/// stream, whose dimensions are multiples of the minimum coding block), is left alone.
pub fn deblock_picture(pic: &mut PicState) {
    let h = pic.height;
    deblock_rows(pic, 0..h);
}

/// Deblock the luma rows `rows` (and the chroma rows of the same area) in place: all
/// vertical edges of the rows, then all horizontal edges of the rows. An edge belongs
/// to the rows that hold its `q` side, so a CTB row (its top edge included) is one such
/// range. It needs the rows above it to be final (the `p` side) and the rows below it to
/// be reconstructed, since intra prediction reads unfiltered samples. Edges whose `p`
/// side is in the rows above are filtered after those rows' own vertical edges, which
/// is the order 8.7.2 gives for the whole picture.
pub fn deblock_rows(pic: &mut PicState, rows: Range<usize>) {
    if !pic.has_pixels() {
        return;
    }
    let (w, h) = (pic.width, pic.height);
    let (sx, sy) = (pic.sub_x(), pic.sub_y());
    let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
    let info_ok = w.is_multiple_of(8) && h.is_multiple_of(8) && pic.info.len() >= (w / 4) * (h / 4);
    let planes = &mut pic.planes;
    if !info_ok
        || planes.y.len() != w * h
        || planes.cb.len() != cw * ch
        || planes.cr.len() != cw * ch
    {
        return;
    }
    let rows = rows.start.min(h)..rows.end.min(h);
    let crows = rows.start >> sy..rows.end.div_ceil(1 << sy);
    let maps = Maps {
        info: &pic.info,
        motion: &pic.motion,
        ctb: &pic.ctb,
        slices: &pic.slices,
        w4: pic.w4,
        ctb_log2: pic.ctb_log2,
        ctb_cols: pic.ctb_cols,
        width: w,
        bit_depth: [pic.bit_depth_y, pic.bit_depth_c],
        cqp_offset: [pic.pps_cb_qp_offset, pic.pps_cr_qp_offset],
        sub: (sx, sy),
        chroma_format_idc: pic.chroma_format_idc,
        pcm_lf_off: pic.pcm_loop_filter_disabled,
        across_tiles: pic.loop_filter_across_tiles,
    };
    for vertical in [true, false] {
        luma_pass(&maps, &mut planes.y, 0, rows.clone(), vertical);
        chroma_pass(
            &maps,
            &mut [planes.cb.as_mut_slice(), planes.cr.as_mut_slice()],
            0,
            crows.clone(),
            vertical,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 16x16 picture whose luma column `x` holds `value(x)`, with flat chroma,
    /// every unit intra at QP 37 and an edge on every unit.
    fn picture(value: impl Fn(usize) -> u16) -> PicState {
        let mut pic = PicState::new(16, 16, 4, 2, 8, 8, true);
        for y in 0..16 {
            for x in 0..16 {
                pic.planes.y[y * 16 + x] = value(x);
            }
        }
        for v in pic.planes.cb.iter_mut().chain(pic.planes.cr.iter_mut()) {
            *v = 128;
        }
        for u in pic.info.iter_mut() {
            *u = BlockInfo {
                intra: true,
                qp: 37,
                edge_v: EDGE_TU,
                edge_h: EDGE_TU,
                ..BlockInfo::default()
            };
        }
        pic.slices = vec![SliceFilter::default()];
        pic
    }

    /// A flat picture has nothing to smooth.
    #[test]
    fn a_flat_picture_is_unchanged() {
        let mut pic = picture(|_| 128);
        let before = pic.planes.clone();
        deblock_picture(&mut pic);
        assert_eq!(pic.planes, before);
    }

    /// A step of 30 across the 8x8 edge at x = 8 is small enough to smooth:
    /// the two samples at the edge move toward each other.
    #[test]
    fn a_step_across_an_8x8_edge_is_smoothed() {
        let mut pic = picture(|x| if x < 8 { 100 } else { 130 });
        deblock_picture(&mut pic);
        for y in 0..16 {
            let row = &pic.planes.y[y * 16..y * 16 + 16];
            assert!(row[7] > 100, "p0 not pulled up at row {y}");
            assert!(row[8] < 130, "q0 not pulled down at row {y}");
        }
    }

    /// A PCM unit on the p side with the PCM loop filter off keeps its samples;
    /// the q side is still filtered.
    #[test]
    fn a_pcm_unit_on_the_p_side_is_not_modified() {
        let mut pic = picture(|x| if x < 8 { 100 } else { 130 });
        pic.pcm_loop_filter_disabled = true;
        for y in (0..16).step_by(4) {
            // Unit column 1 covers luma x 4..8, the p side of the edge at x = 8.
            pic.info[(y >> 2) * pic.w4 + 1].pcm = true;
        }
        deblock_picture(&mut pic);
        for y in 0..16 {
            let row = &pic.planes.y[y * 16..y * 16 + 16];
            assert_eq!(row[7], 100, "p0 changed at row {y}");
            assert_eq!(row[6], 100, "p1 changed at row {y}");
            assert!(row[8] < 130, "q0 not filtered at row {y}");
        }
    }
}

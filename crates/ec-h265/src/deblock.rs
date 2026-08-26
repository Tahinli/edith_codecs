//! The in-loop deblocking filter (8.7.2).
//!
//! An intra-only stream predicts from its own *unfiltered* reconstruction
//! (8.4.4.2.1 reads the samples prior to deblocking), so the filter changes
//! nothing the encoder predicts from: it runs once over the finished picture,
//! and what it produces is what a decoder shows. That keeps the reconstruction
//! bit-identical to a decoder's output -- and the picture-hash SEI that rides
//! on it honest -- while buying back the block edges the transform left.
//!
//! Every edge in an intra picture is a boundary between two intra blocks, so
//! `bS` is 2 wherever an edge exists at all (8.7.2.4); the whole derivation
//! reduces to *where* the transform-block boundaries are, which is what
//! [`TuMap`] carries.

use crate::transform::chroma_qp;

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

/// The size of the transform block covering each 4x4 luma position, as a
/// `log2`, over a whole picture.
///
/// A transform block of side `S` is aligned to a multiple of `S`, so an edge at
/// `x` is that block's own left edge -- and therefore a filtered boundary --
/// exactly when `x % S == 0`. One number per 4x4 unit answers the question for
/// every edge without recording edges at all.
pub struct TuMap {
    log2: Vec<u8>,
    stride: usize,
}

impl TuMap {
    /// An empty map for a picture of `width` x `height` luma samples.
    pub fn new(width: usize, height: usize) -> TuMap {
        TuMap {
            log2: vec![2; (width / 4) * (height / 4)],
            stride: width / 4,
        }
    }

    /// Record that the transform block of side `1 << log2` covers the square at
    /// `(x, y)`.
    pub fn mark(&mut self, x: usize, y: usize, log2: u8) {
        let side = 1usize << log2;
        for by in (y..y + side).step_by(4) {
            for bx in (x..x + side).step_by(4) {
                let idx = (by / 4) * self.stride + bx / 4;
                if idx < self.log2.len() {
                    self.log2[idx] = log2;
                }
            }
        }
    }

    /// Copy one wavefront worker's band, `rows` 4x4 rows of it, into place.
    pub fn absorb_band(&mut self, band_y0: usize, rows: usize, band: &[u8]) {
        for row in 0..rows {
            let dst = (band_y0 / 4 + row) * self.stride;
            let src = row * self.stride;
            if dst + self.stride <= self.log2.len() && src + self.stride <= band.len() {
                self.log2[dst..dst + self.stride].copy_from_slice(&band[src..src + self.stride]);
            }
        }
    }

    /// Whether the vertical edge at luma column `x` is a transform-block
    /// boundary for the four rows starting at `y`.
    fn vertical_edge(&self, x: usize, y: usize) -> bool {
        x > 0 && x.is_multiple_of(1usize << self.log2[(y / 4) * self.stride + x / 4])
    }

    /// Whether the horizontal edge at luma row `y` is a transform-block
    /// boundary for the four columns starting at `x`.
    fn horizontal_edge(&self, x: usize, y: usize) -> bool {
        y > 0 && y.is_multiple_of(1usize << self.log2[(y / 4) * self.stride + x / 4])
    }
}

/// One line across an edge, as offsets from the boundary: `p3..p0` on one side,
/// `q0..q3` on the other. `step` is the distance between samples across the
/// edge, `at` the index of `q0`.
struct Line {
    at: usize,
    step: usize,
}

impl Line {
    fn p(&self, plane: &[u8], i: usize) -> i32 {
        i32::from(plane[self.at - (i + 1) * self.step])
    }

    fn q(&self, plane: &[u8], i: usize) -> i32 {
        i32::from(plane[self.at + i * self.step])
    }

    fn set_p(&self, plane: &mut [u8], i: usize, value: i32) {
        plane[self.at - (i + 1) * self.step] = value.clamp(0, 255) as u8;
    }

    fn set_q(&self, plane: &mut [u8], i: usize, value: i32) {
        plane[self.at + i * self.step] = value.clamp(0, 255) as u8;
    }
}

fn clip3(lo: i32, hi: i32, v: i32) -> i32 {
    v.clamp(lo, hi)
}

/// Filter one four-line luma edge segment (8.7.2.5.3).
fn luma_segment(plane: &mut [u8], lines: [Line; 4], beta: i32, tc: i32) {
    let d_of = |line: &Line, plane: &[u8]| {
        (
            (line.p(plane, 2) - 2 * line.p(plane, 1) + line.p(plane, 0)).abs(),
            (line.q(plane, 2) - 2 * line.q(plane, 1) + line.q(plane, 0)).abs(),
        )
    };
    let (dp0, dq0) = d_of(&lines[0], plane);
    let (dp3, dq3) = d_of(&lines[3], plane);
    let (dpq0, dpq3) = (dp0 + dq0, dp3 + dq3);
    let d = dpq0 + dpq3;
    if d >= beta {
        return;
    }
    let (dp, dq) = (dp0 + dp3, dq0 + dq3);

    // 8.7.2.5.6: both outer lines have to want the strong filter.
    let strong_on = |line: &Line, dpq: i32, plane: &[u8]| {
        2 * dpq < (beta >> 2)
            && (line.p(plane, 3) - line.p(plane, 0)).abs()
                + (line.q(plane, 0) - line.q(plane, 3)).abs()
                < (beta >> 3)
            && (line.p(plane, 0) - line.q(plane, 0)).abs() < ((5 * tc + 1) >> 1)
    };
    let strong = strong_on(&lines[0], dpq0, plane) && strong_on(&lines[3], dpq3, plane);
    let filter_p1 = dp < ((beta + (beta >> 1)) >> 3);
    let filter_q1 = dq < ((beta + (beta >> 1)) >> 3);

    for line in &lines {
        let p: [i32; 4] = std::array::from_fn(|i| line.p(plane, i));
        let q: [i32; 4] = std::array::from_fn(|i| line.q(plane, i));
        if strong {
            let clip = |base: i32, v: i32| clip3(base - 2 * tc, base + 2 * tc, v);
            line.set_p(
                plane,
                0,
                clip(
                    p[0],
                    (p[2] + 2 * p[1] + 2 * p[0] + 2 * q[0] + q[1] + 4) >> 3,
                ),
            );
            line.set_p(plane, 1, clip(p[1], (p[2] + p[1] + p[0] + q[0] + 2) >> 2));
            line.set_p(
                plane,
                2,
                clip(p[2], (2 * p[3] + 3 * p[2] + p[1] + p[0] + q[0] + 4) >> 3),
            );
            line.set_q(
                plane,
                0,
                clip(
                    q[0],
                    (p[1] + 2 * p[0] + 2 * q[0] + 2 * q[1] + q[2] + 4) >> 3,
                ),
            );
            line.set_q(plane, 1, clip(q[1], (p[0] + q[0] + q[1] + q[2] + 2) >> 2));
            line.set_q(
                plane,
                2,
                clip(q[2], (p[0] + q[0] + q[1] + 3 * q[2] + 2 * q[3] + 4) >> 3),
            );
        } else {
            let delta = (9 * (q[0] - p[0]) - 3 * (q[1] - p[1]) + 8) >> 4;
            if delta.abs() >= tc * 10 {
                continue;
            }
            let delta = clip3(-tc, tc, delta);
            line.set_p(plane, 0, p[0] + delta);
            line.set_q(plane, 0, q[0] - delta);
            if filter_p1 {
                let dp = clip3(
                    -(tc >> 1),
                    tc >> 1,
                    (((p[2] + p[0] + 1) >> 1) - p[1] + delta) >> 1,
                );
                line.set_p(plane, 1, p[1] + dp);
            }
            if filter_q1 {
                let dq = clip3(
                    -(tc >> 1),
                    tc >> 1,
                    (((q[2] + q[0] + 1) >> 1) - q[1] - delta) >> 1,
                );
                line.set_q(plane, 1, q[1] + dq);
            }
        }
    }
}

/// Filter one chroma line across an edge (8.7.2.5.5).
fn chroma_line(plane: &mut [u8], line: &Line, tc: i32) {
    let (p0, p1) = (line.p(plane, 0), line.p(plane, 1));
    let (q0, q1) = (line.q(plane, 0), line.q(plane, 1));
    let delta = clip3(-tc, tc, (((q0 - p0) << 2) + p1 - q1 + 4) >> 3);
    line.set_p(plane, 0, p0 + delta);
    line.set_q(plane, 0, q0 - delta);
}

/// Run the filter over a whole picture: every vertical edge first, then every
/// horizontal one over what the vertical pass left (8.7.2).
///
/// `qp` is the picture's quantisation parameter, which is every block's here --
/// nothing in this encoder writes `cu_qp_delta`.
pub fn deblock(
    rec_y: &mut [u8],
    rec_cb: &mut [u8],
    rec_cr: &mut [u8],
    width: usize,
    height: usize,
    qp: i32,
    tus: &TuMap,
) {
    // bS is 2 on every edge of an intra picture, so both look-ups are constant.
    let beta = BETA[clip3(0, 51, qp) as usize];
    let tc_luma = TC[clip3(0, 53, qp + 2) as usize];
    let tc_chroma = TC[clip3(0, 53, chroma_qp(qp, 0) + 2) as usize];
    if beta == 0 && tc_luma == 0 && tc_chroma == 0 {
        return;
    }
    let (cw, ch) = (width / 2, height / 2);

    for x in (8..width).step_by(8) {
        for y in (0..height).step_by(4) {
            if tus.vertical_edge(x, y) {
                let lines = std::array::from_fn(|i| Line {
                    at: (y + i) * width + x,
                    step: 1,
                });
                luma_segment(rec_y, lines, beta, tc_luma);
            }
        }
    }
    for x_c in (8..cw).step_by(8) {
        for y_c in (0..ch).step_by(2) {
            if tus.vertical_edge(2 * x_c, 2 * y_c) {
                for row in 0..2 {
                    let line = Line {
                        at: (y_c + row) * cw + x_c,
                        step: 1,
                    };
                    chroma_line(rec_cb, &line, tc_chroma);
                    chroma_line(rec_cr, &line, tc_chroma);
                }
            }
        }
    }

    for y in (8..height).step_by(8) {
        for x in (0..width).step_by(4) {
            if tus.horizontal_edge(x, y) {
                let lines = std::array::from_fn(|i| Line {
                    at: y * width + x + i,
                    step: width,
                });
                luma_segment(rec_y, lines, beta, tc_luma);
            }
        }
    }
    for y_c in (8..ch).step_by(8) {
        for x_c in (0..cw).step_by(2) {
            if tus.horizontal_edge(2 * x_c, 2 * y_c) {
                for col in 0..2 {
                    let line = Line {
                        at: y_c * cw + x_c + col,
                        step: cw,
                    };
                    chroma_line(rec_cb, &line, tc_chroma);
                    chroma_line(rec_cr, &line, tc_chroma);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A step across an 8x8 transform boundary, the case the filter exists for.
    fn step_picture(w: usize, h: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let mut y = vec![0u8; w * h];
        for row in 0..h {
            for col in 0..w {
                y[row * w + col] = if col < 8 { 100 } else { 130 };
            }
        }
        let c = vec![128u8; w / 2 * h / 2];
        (y, c.clone(), c)
    }

    /// The filter has to actually reach the samples: a gate that only proves
    /// bit-exactness against a decoder stays green if the filter never fires,
    /// because a decoder told the same thing does nothing either.
    #[test]
    fn an_edge_between_two_transform_blocks_is_filtered() {
        let (w, h) = (16, 16);
        let (mut y, mut cb, mut cr) = step_picture(w, h);
        let before = y.clone();
        let mut tus = TuMap::new(w, h);
        for by in (0..h).step_by(8) {
            for bx in (0..w).step_by(8) {
                tus.mark(bx, by, 3);
            }
        }
        deblock(&mut y, &mut cb, &mut cr, w, h, 32, &tus);
        assert_ne!(y, before, "the edge at x=8 was left alone");
        assert!(y[7] > before[7], "p0 should be pulled up toward q0");
        assert!(y[8] < before[8], "q0 should be pulled down toward p0");
    }

    /// The same picture inside one transform block has no interior boundary,
    /// so nothing may move — this is what keeps the map honest.
    #[test]
    fn no_transform_boundary_means_no_filtering() {
        let (w, h) = (16, 16);
        let (mut y, mut cb, mut cr) = step_picture(w, h);
        let before = y.clone();
        let mut tus = TuMap::new(w, h);
        tus.mark(0, 0, 4);
        deblock(&mut y, &mut cb, &mut cr, w, h, 32, &tus);
        assert_eq!(y, before);
    }
}

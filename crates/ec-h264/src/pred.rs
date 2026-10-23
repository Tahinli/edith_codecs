//! Intra prediction (spec 8.3.1, 8.3.3, 8.3.4), generic over the sample type
//! (`u8` for 8-bit, `u16` for High 10).
//!
//! Kernels operate on a small gathered neighbour set so the hot loop touches
//! the picture plane only twice per block: one gather, one write. All
//! arithmetic is in `i32` with a final clip to the sample range.

use crate::sample::Sample;

/// Neighbour samples of a 4x4 block, gathered once (spec 8.3.1.2's 13
/// samples). `top[4..8]` already has the top-right substitution rule applied:
/// when p[4..7, -1] is unavailable but p[3, -1] is, it is replicated.
#[derive(Debug, Clone, Copy)]
pub struct Nbr4<S: Sample = u8> {
    /// p[0..7, -1].
    pub top: [S; 8],
    /// p[-1, 0..3].
    pub left: [S; 4],
    /// p[-1, -1].
    pub top_left: S,
    /// p[x, -1] available.
    pub have_top: bool,
    /// p[-1, y] available.
    pub have_left: bool,
}

/// Predict a 4x4 block (spec 8.3.1.2.1-9) into `out` in raster order.
///
/// Modes 0..=8; conformant streams only signal modes whose required samples
/// are available, but unavailable inputs still produce defined output (DC
/// fallbacks) rather than reading junk.
pub fn pred_4x4<S: Sample>(mode: u8, bd: u32, n: &Nbr4<S>, out: &mut [S; 16]) {
    let t = |x: usize| n.top[x].to_i32();
    let l = |y: usize| n.left[y].to_i32();
    let tl = n.top_left.to_i32();
    match mode {
        // Vertical.
        0 => {
            for y in 0..4 {
                for x in 0..4 {
                    out[y * 4 + x] = n.top[x];
                }
            }
        }
        // Horizontal.
        1 => {
            for y in 0..4 {
                out[y * 4..y * 4 + 4].fill(n.left[y]);
            }
        }
        // DC (8-48..8-51).
        2 => {
            let v = match (n.have_top, n.have_left) {
                (true, true) => (t(0) + t(1) + t(2) + t(3) + l(0) + l(1) + l(2) + l(3) + 4) >> 3,
                (false, true) => (l(0) + l(1) + l(2) + l(3) + 2) >> 2,
                (true, false) => (t(0) + t(1) + t(2) + t(3) + 2) >> 2,
                (false, false) => 1 << (bd - 1),
            };
            out.fill(S::from_i32(v));
        }
        // Diagonal down-left (8-52, 8-53).
        3 => {
            for y in 0..4 {
                for x in 0..4 {
                    let v = if x == 3 && y == 3 {
                        (t(6) + 3 * t(7) + 2) >> 2
                    } else {
                        (t(x + y) + 2 * t(x + y + 1) + t(x + y + 2) + 2) >> 2
                    };
                    out[y * 4 + x] = S::from_i32(v);
                }
            }
        }
        // Diagonal down-right (8-54..8-56). Index -1 is the top-left sample.
        4 => {
            let te = |i: i32| if i < 0 { tl } else { t(i as usize) };
            let le = |i: i32| if i < 0 { tl } else { l(i as usize) };
            for y in 0..4i32 {
                for x in 0..4i32 {
                    let v = if x > y {
                        (te(x - y - 2) + 2 * te(x - y - 1) + te(x - y) + 2) >> 2
                    } else if x < y {
                        (le(y - x - 2) + 2 * le(y - x - 1) + le(y - x) + 2) >> 2
                    } else {
                        (t(0) + 2 * tl + l(0) + 2) >> 2
                    };
                    out[(y * 4 + x) as usize] = S::from_i32(v);
                }
            }
        }
        // Vertical-right (8-57..8-60).
        5 => {
            let te = |i: i32| if i < 0 { tl } else { t(i as usize) };
            let le = |i: i32| if i < 0 { tl } else { l(i as usize) };
            for y in 0..4i32 {
                for x in 0..4i32 {
                    let z = 2 * x - y;
                    let i = x - (y >> 1);
                    let v = if z >= 0 && z % 2 == 0 {
                        (te(i - 1) + te(i) + 1) >> 1
                    } else if z >= 0 {
                        (te(i - 2) + 2 * te(i - 1) + te(i) + 2) >> 2
                    } else if z == -1 {
                        (l(0) + 2 * tl + t(0) + 2) >> 2
                    } else {
                        (le(y - 1) + 2 * le(y - 2) + le(y - 3) + 2) >> 2
                    };
                    out[(y * 4 + x) as usize] = S::from_i32(v);
                }
            }
        }
        // Horizontal-down (8-61..8-64).
        6 => {
            let te = |i: i32| if i < 0 { tl } else { t(i as usize) };
            let le = |i: i32| if i < 0 { tl } else { l(i as usize) };
            for y in 0..4i32 {
                for x in 0..4i32 {
                    let z = 2 * y - x;
                    let i = y - (x >> 1);
                    let v = if z >= 0 && z % 2 == 0 {
                        (le(i - 1) + le(i) + 1) >> 1
                    } else if z >= 0 {
                        (le(i - 2) + 2 * le(i - 1) + le(i) + 2) >> 2
                    } else if z == -1 {
                        (l(0) + 2 * tl + t(0) + 2) >> 2
                    } else {
                        (te(x - 1) + 2 * te(x - 2) + te(x - 3) + 2) >> 2
                    };
                    out[(y * 4 + x) as usize] = S::from_i32(v);
                }
            }
        }
        // Vertical-left (8-65, 8-66).
        7 => {
            for y in 0..4usize {
                for x in 0..4usize {
                    let i = x + (y >> 1);
                    let v = if y % 2 == 0 {
                        (t(i) + t(i + 1) + 1) >> 1
                    } else {
                        (t(i) + 2 * t(i + 1) + t(i + 2) + 2) >> 2
                    };
                    out[y * 4 + x] = S::from_i32(v);
                }
            }
        }
        // Horizontal-up (8-67..8-70).
        _ => {
            for y in 0..4usize {
                for x in 0..4usize {
                    let z = x + 2 * y;
                    let i = y + (x >> 1);
                    let v = if z < 5 && z % 2 == 0 {
                        (l(i) + l(i + 1) + 1) >> 1
                    } else if z < 5 {
                        (l(i) + 2 * l(i + 1) + l(i + 2) + 2) >> 2
                    } else if z == 5 {
                        (l(2) + 3 * l(3) + 2) >> 2
                    } else {
                        l(3)
                    };
                    out[y * 4 + x] = S::from_i32(v);
                }
            }
        }
    }
}

/// Neighbour samples of an 8x8 luma block (spec 8.3.2.2's 25 samples), already
/// low-pass filtered by [`filter_nbr8`]. `top[8..16]` carries the top-right
/// substitution rule.
#[derive(Debug, Clone, Copy)]
pub struct Nbr8<S: Sample = u8> {
    /// p'[0..15, -1].
    pub top: [S; 16],
    /// p'[-1, 0..7].
    pub left: [S; 8],
    /// p'[-1, -1].
    pub top_left: S,
    /// p[x, -1] available.
    pub have_top: bool,
    /// p[-1, y] available.
    pub have_left: bool,
    /// p[-1, -1] available. Its own neighbour (the above-left macroblock for
    /// block 0), not the conjunction of the two runs.
    pub have_tl: bool,
}

/// Reference sample filtering for Intra_8x8 prediction (spec 8.3.2.2.1).
///
/// Input is the unfiltered neighbourhood; each of the three runs (top row,
/// corner, left column) is filtered only when it is wholly available, exactly
/// as the clause states, so an edge block keeps the raw samples the prediction
/// modes it is allowed to signal actually read.
pub fn filter_nbr8<S: Sample>(n: &Nbr8<S>) -> Nbr8<S> {
    let mut out = *n;
    let have_tl = n.have_tl;
    if n.have_top {
        let t = |i: usize| n.top[i].to_i32();
        out.top[0] = if have_tl {
            S::from_i32((n.top_left.to_i32() + 2 * t(0) + t(1) + 2) >> 2)
        } else {
            S::from_i32((3 * t(0) + t(1) + 2) >> 2)
        };
        for x in 1..15 {
            out.top[x] = S::from_i32((t(x - 1) + 2 * t(x) + t(x + 1) + 2) >> 2);
        }
        out.top[15] = S::from_i32((t(14) + 3 * t(15) + 2) >> 2);
    }
    if have_tl {
        // 8-82 to 8-84: which taps the corner takes depends on which of its two
        // adjacent samples exist.
        let (tl, t0, l0) = (n.top_left.to_i32(), n.top[0].to_i32(), n.left[0].to_i32());
        out.top_left = match (n.have_top, n.have_left) {
            (true, true) => S::from_i32((t0 + 2 * tl + l0 + 2) >> 2),
            (true, false) => S::from_i32((3 * tl + t0 + 2) >> 2),
            (false, true) => S::from_i32((3 * tl + l0 + 2) >> 2),
            (false, false) => n.top_left,
        };
    }
    if n.have_left {
        let l = |i: usize| n.left[i].to_i32();
        out.left[0] = if have_tl {
            S::from_i32((n.top_left.to_i32() + 2 * l(0) + l(1) + 2) >> 2)
        } else {
            S::from_i32((3 * l(0) + l(1) + 2) >> 2)
        };
        for y in 1..7 {
            out.left[y] = S::from_i32((l(y - 1) + 2 * l(y) + l(y + 1) + 2) >> 2);
        }
        out.left[7] = S::from_i32((l(6) + 3 * l(7) + 2) >> 2);
    }
    out
}

/// Predict an 8x8 luma block (spec 8.3.2.2.2 - 8.3.2.2.10) into `out` in raster
/// order. `n` must already be filtered by [`filter_nbr8`].
// x and y are sample coordinates that also index the neighbour runs; the
// clause's index arithmetic is what the loops spell out.
#[allow(clippy::needless_range_loop)]
pub fn pred_8x8<S: Sample>(mode: u8, bd: u32, n: &Nbr8<S>, out: &mut [S; 64]) {
    let t = |x: i32| n.top[x as usize].to_i32();
    let l = |y: i32| n.left[y as usize].to_i32();
    let tl = n.top_left.to_i32();
    // Index -1 of either run is the corner sample.
    let te = |i: i32| if i < 0 { tl } else { t(i) };
    let le = |i: i32| if i < 0 { tl } else { l(i) };
    match mode {
        // Vertical (8-89).
        0 => {
            for y in 0..8 {
                out[y * 8..y * 8 + 8].copy_from_slice(&n.top[..8]);
            }
        }
        // Horizontal (8-90).
        1 => {
            for y in 0..8 {
                out[y * 8..y * 8 + 8].fill(n.left[y]);
            }
        }
        // DC (8-91..8-94).
        2 => {
            let sum_t: i32 = (0..8).map(t).sum();
            let sum_l: i32 = (0..8).map(l).sum();
            let v = match (n.have_top, n.have_left) {
                (true, true) => (sum_t + sum_l + 8) >> 4,
                (true, false) => (sum_t + 4) >> 3,
                (false, true) => (sum_l + 4) >> 3,
                (false, false) => 1 << (bd - 1),
            };
            out.fill(S::from_i32(v));
        }
        // Diagonal down-left (8-95, 8-96).
        3 => {
            for y in 0..8i32 {
                for x in 0..8i32 {
                    let v = if x == 7 && y == 7 {
                        (t(14) + 3 * t(15) + 2) >> 2
                    } else {
                        (t(x + y) + 2 * t(x + y + 1) + t(x + y + 2) + 2) >> 2
                    };
                    out[(y * 8 + x) as usize] = S::from_i32(v);
                }
            }
        }
        // Diagonal down-right (8-97..8-99).
        4 => {
            for y in 0..8i32 {
                for x in 0..8i32 {
                    let v = if x > y {
                        (te(x - y - 2) + 2 * te(x - y - 1) + t(x - y) + 2) >> 2
                    } else if x < y {
                        (le(y - x - 2) + 2 * le(y - x - 1) + l(y - x) + 2) >> 2
                    } else {
                        (t(0) + 2 * tl + l(0) + 2) >> 2
                    };
                    out[(y * 8 + x) as usize] = S::from_i32(v);
                }
            }
        }
        // Vertical-right (8-100..8-103).
        5 => {
            for y in 0..8i32 {
                for x in 0..8i32 {
                    let z = 2 * x - y;
                    let i = x - (y >> 1);
                    let v = if z >= 0 && z % 2 == 0 {
                        (te(i - 1) + te(i) + 1) >> 1
                    } else if z >= 0 {
                        (te(i - 2) + 2 * te(i - 1) + te(i) + 2) >> 2
                    } else if z == -1 {
                        (l(0) + 2 * tl + t(0) + 2) >> 2
                    } else {
                        (l(y - 2 * x - 1) + 2 * l(y - 2 * x - 2) + le(y - 2 * x - 3) + 2) >> 2
                    };
                    out[(y * 8 + x) as usize] = S::from_i32(v);
                }
            }
        }
        // Horizontal-down (8-104..8-107).
        6 => {
            for y in 0..8i32 {
                for x in 0..8i32 {
                    let z = 2 * y - x;
                    let i = y - (x >> 1);
                    let v = if z >= 0 && z % 2 == 0 {
                        (le(i - 1) + le(i) + 1) >> 1
                    } else if z >= 0 {
                        (le(i - 2) + 2 * le(i - 1) + le(i) + 2) >> 2
                    } else if z == -1 {
                        (l(0) + 2 * tl + t(0) + 2) >> 2
                    } else {
                        (t(x - 2 * y - 1) + 2 * t(x - 2 * y - 2) + te(x - 2 * y - 3) + 2) >> 2
                    };
                    out[(y * 8 + x) as usize] = S::from_i32(v);
                }
            }
        }
        // Vertical-left (8-108, 8-109).
        7 => {
            for y in 0..8i32 {
                for x in 0..8i32 {
                    let i = x + (y >> 1);
                    let v = if y % 2 == 0 {
                        (t(i) + t(i + 1) + 1) >> 1
                    } else {
                        (t(i) + 2 * t(i + 1) + t(i + 2) + 2) >> 2
                    };
                    out[(y * 8 + x) as usize] = S::from_i32(v);
                }
            }
        }
        // Horizontal-up (8-110..8-113).
        _ => {
            for y in 0..8i32 {
                for x in 0..8i32 {
                    let z = x + 2 * y;
                    let i = y + (x >> 1);
                    let v = if z < 13 && z % 2 == 0 {
                        (l(i) + l(i + 1) + 1) >> 1
                    } else if z < 13 {
                        (l(i) + 2 * l(i + 1) + l(i + 2) + 2) >> 2
                    } else if z == 13 {
                        (l(6) + 3 * l(7) + 2) >> 2
                    } else {
                        l(7)
                    };
                    out[(y * 8 + x) as usize] = S::from_i32(v);
                }
            }
        }
    }
}

/// A borrowed window into a picture plane for whole-MB prediction: `data`
/// spans the plane, `stride` its pitch, `origin` the index of this MB's
/// top-left sample.
pub struct PlaneWindow<'a, S: Sample = u8> {
    /// The full plane.
    pub data: &'a mut [S],
    /// Row pitch.
    pub stride: usize,
    /// Index of the block's top-left sample.
    pub origin: usize,
}

/// Intra_16x16 prediction (spec 8.3.3), writing predictions straight into the
/// plane; residuals are added on top afterwards.
pub fn pred_16x16<S: Sample>(
    mode: u8,
    bd: u32,
    w: &mut PlaneWindow<'_, S>,
    have_top: bool,
    have_left: bool,
) {
    pred_nxn::<16, S>(mode, bd, w, have_top, have_left, 5, 5);
}

/// Chroma 8x8 prediction for 4:2:0 (spec 8.3.4). Chroma mode numbering
/// differs from luma 16x16: 0 = DC, 1 = Horizontal, 2 = Vertical, 3 = Plane.
pub fn pred_chroma_8x8<S: Sample>(
    mode: u8,
    bd: u32,
    w: &mut PlaneWindow<'_, S>,
    have_top: bool,
    have_left: bool,
) {
    pred_chroma_8x8_halves(mode, bd, w, have_top, [have_left; 2]);
}

/// [`pred_chroma_8x8`] with the availability of the left samples given for the
/// upper and lower four rows separately (constrained intra prediction in an
/// MBAFF frame can leave only one half available). Only DC prediction can use
/// a half; the other modes need both.
pub fn pred_chroma_8x8_halves<S: Sample>(
    mode: u8,
    bd: u32,
    w: &mut PlaneWindow<'_, S>,
    have_top: bool,
    left: [bool; 2],
) {
    let have_left = left[0] && left[1];
    match mode {
        0 => pred_chroma_dc(bd, w, have_top, left),
        1 => pred_nxn::<8, S>(1, bd, w, have_top, have_left, 0, 0),
        2 => pred_nxn::<8, S>(0, bd, w, have_top, have_left, 0, 0),
        _ => pred_nxn::<8, S>(3, bd, w, have_top, have_left, 0, 34),
    }
}

/// Shared V/H/DC/Plane engine for NxN whole-block prediction. `mode` uses the
/// 16x16 numbering (0 V, 1 H, 2 DC, 3 Plane). `dc_shift` = log2(2N),
/// `plane_scale` = 5 for luma, 34 for 4:2:0 chroma.
fn pred_nxn<const N: usize, S: Sample>(
    mode: u8,
    bd: u32,
    w: &mut PlaneWindow<'_, S>,
    have_top: bool,
    have_left: bool,
    dc_shift: u32,
    plane_scale: i32,
) {
    let stride = w.stride;
    let o = w.origin;
    match mode {
        0 => {
            // Vertical: replicate the row above.
            let (top, rest) = w.data.split_at_mut(o);
            let top_row = &top[o - stride..o - stride + N];
            for y in 0..N {
                rest[y * stride..y * stride + N].copy_from_slice(top_row);
            }
        }
        1 => {
            for y in 0..N {
                let v = w.data[o + y * stride - 1];
                w.data[o + y * stride..o + y * stride + N].fill(v);
            }
        }
        2 => {
            let mut sum = 0i32;
            if have_top {
                for x in 0..N {
                    sum += w.data[o - stride + x].to_i32();
                }
            }
            if have_left {
                for y in 0..N {
                    sum += w.data[o + y * stride - 1].to_i32();
                }
            }
            let v = match (have_top, have_left) {
                (true, true) => (sum + (1 << (dc_shift - 1))) >> dc_shift,
                (true, false) | (false, true) => (sum + (1 << (dc_shift - 2))) >> (dc_shift - 1),
                (false, false) => 1 << (bd - 1),
            };
            let v = S::from_i32(v);
            for y in 0..N {
                w.data[o + y * stride..o + y * stride + N].fill(v);
            }
        }
        _ => {
            // Plane (8-122..8-127 / 8-144..8-149). half = N/2.
            let half = (N / 2) as i32;
            let px = |x: i32, y: i32| -> i32 {
                let idx = (o as i32 + y * stride as i32 + x) as usize;
                w.data[idx].to_i32()
            };
            let mut h = 0i32;
            let mut v = 0i32;
            for i in 0..half {
                h += (i + 1) * (px(half + i, -1) - px(half - 2 - i, -1));
                v += (i + 1) * (px(-1, half + i) - px(-1, half - 2 - i));
            }
            let max = (1i32 << bd) - 1;
            let a = 16 * (px(-1, N as i32 - 1) + px(N as i32 - 1, -1));
            let b = (plane_scale * h + 32) >> 6;
            let c = (plane_scale * v + 32) >> 6;
            for y in 0..N as i32 {
                for x in 0..N as i32 {
                    let p = (a + b * (x - (half - 1)) + c * (y - (half - 1)) + 16) >> 5;
                    w.data[o + y as usize * stride + x as usize] = S::clip(p, max);
                }
            }
        }
    }
}

/// Chroma DC prediction (spec 8.3.4.1): each 4x4 sub-block averages specific
/// neighbour runs — corner blocks prefer their adjacent edge.
fn pred_chroma_dc<S: Sample>(bd: u32, w: &mut PlaneWindow<'_, S>, have_top: bool, left: [bool; 2]) {
    let stride = w.stride;
    let o = w.origin;
    let sum_top =
        |data: &[S], x0: usize| -> i32 { (0..4).map(|i| data[o - stride + x0 + i].to_i32()).sum() };
    let sum_left = |data: &[S], y0: usize| -> i32 {
        (0..4)
            .map(|i| data[o + (y0 + i) * stride - 1].to_i32())
            .sum()
    };
    #[allow(clippy::needless_range_loop)]
    for by in 0..2 {
        for bx in 0..2 {
            // The left samples this block reads are its own four rows.
            let have_left = left[by];
            // (0,0) and (1,1) use both edges; (1,0) prefers top; (0,1) left.
            let v = if bx == by {
                match (have_top, have_left) {
                    (true, true) => (sum_top(w.data, bx * 4) + sum_left(w.data, by * 4) + 4) >> 3,
                    (true, false) => (sum_top(w.data, bx * 4) + 2) >> 2,
                    (false, true) => (sum_left(w.data, by * 4) + 2) >> 2,
                    (false, false) => 1 << (bd - 1),
                }
            } else if bx == 1 {
                match (have_top, have_left) {
                    (true, _) => (sum_top(w.data, 4) + 2) >> 2,
                    (false, true) => (sum_left(w.data, 0) + 2) >> 2,
                    (false, false) => 1 << (bd - 1),
                }
            } else {
                match (have_left, have_top) {
                    (true, _) => (sum_left(w.data, 4) + 2) >> 2,
                    (false, true) => (sum_top(w.data, 0) + 2) >> 2,
                    (false, false) => 1 << (bd - 1),
                }
            };
            let v = S::from_i32(v);
            for y in 0..4 {
                let row = o + (by * 4 + y) * stride + bx * 4;
                w.data[row..row + 4].fill(v);
            }
        }
    }
}

/// Chroma 8x16 prediction for 4:2:2 (spec 8.3.4, MbHeightC 16). Mode numbering
/// as [`pred_chroma_8x8`]. `left` gives the availability of the left samples
/// for each group of four rows (only DC prediction can use a part of them).
pub fn pred_chroma_8x16<S: Sample>(
    mode: u8,
    bd: u32,
    w: &mut PlaneWindow<'_, S>,
    have_top: bool,
    left: [bool; 4],
) {
    let stride = w.stride;
    let o = w.origin;
    let have_left = left.iter().all(|&l| l);
    match mode {
        0 => {
            let sum_top = |data: &[S], x0: usize| -> i32 {
                (0..4).map(|i| data[o - stride + x0 + i].to_i32()).sum()
            };
            let sum_left = |data: &[S], y0: usize| -> i32 {
                (0..4)
                    .map(|i| data[o + (y0 + i) * stride - 1].to_i32())
                    .sum()
            };
            let mid = 1i32 << (bd - 1);
            for (by, &hl) in left.iter().enumerate() {
                for bx in 0..2 {
                    let v = if (bx == 0 && by == 0) || (bx > 0 && by > 0) {
                        match (have_top, hl) {
                            (true, true) => {
                                (sum_top(w.data, bx * 4) + sum_left(w.data, by * 4) + 4) >> 3
                            }
                            (true, false) => (sum_top(w.data, bx * 4) + 2) >> 2,
                            (false, true) => (sum_left(w.data, by * 4) + 2) >> 2,
                            (false, false) => mid,
                        }
                    } else if bx > 0 {
                        // by == 0: the top row is preferred.
                        match (have_top, hl) {
                            (true, _) => (sum_top(w.data, 4) + 2) >> 2,
                            (false, true) => (sum_left(w.data, 0) + 2) >> 2,
                            (false, false) => mid,
                        }
                    } else {
                        // bx == 0, by > 0: the left column is preferred.
                        match (hl, have_top) {
                            (true, _) => (sum_left(w.data, by * 4) + 2) >> 2,
                            (false, true) => (sum_top(w.data, 0) + 2) >> 2,
                            (false, false) => mid,
                        }
                    };
                    let v = S::from_i32(v);
                    for y in 0..4 {
                        let row = o + (by * 4 + y) * stride + bx * 4;
                        w.data[row..row + 4].fill(v);
                    }
                }
            }
        }
        1 => {
            for y in 0..16 {
                let v = w.data[o + y * stride - 1];
                w.data[o + y * stride..o + y * stride + 8].fill(v);
            }
        }
        2 => {
            let (top, rest) = w.data.split_at_mut(o);
            let top_row = &top[o - stride..o - stride + 8];
            for y in 0..16 {
                rest[y * stride..y * stride + 8].copy_from_slice(top_row);
            }
        }
        _ => {
            // Plane (8-139..8-144) with xCF 0 and yCF 4.
            let _ = (have_top, have_left);
            let px = |x: i32, y: i32| -> i32 {
                let idx = (o as i32 + y * stride as i32 + x) as usize;
                w.data[idx].to_i32()
            };
            let mut h = 0i32;
            let mut v = 0i32;
            for i in 0..4 {
                h += (i + 1) * (px(4 + i, -1) - px(2 - i, -1));
            }
            for i in 0..8 {
                v += (i + 1) * (px(-1, 8 + i) - px(-1, 6 - i));
            }
            let max = (1i32 << bd) - 1;
            let a = 16 * (px(-1, 15) + px(7, -1));
            let b = (34 * h + 32) >> 6;
            let c = (5 * v + 32) >> 6;
            for y in 0..16i32 {
                for x in 0..8i32 {
                    let p = (a + b * (x - 3) + c * (y - 7) + 16) >> 5;
                    w.data[o + y as usize * stride + x as usize] = S::clip(p, max);
                }
            }
        }
    }
}

/// Add a 4x4 residual to the plane with clamping (picture construction,
/// spec 8.5.14 path for 8-bit).
#[inline]
pub fn add_residual_4x4<S: Sample>(
    data: &mut [S],
    bd: u32,
    stride: usize,
    origin: usize,
    r: &[i32; 16],
) {
    let max = (1i32 << bd) - 1;
    for y in 0..4 {
        let row = origin + y * stride;
        for x in 0..4 {
            let p = data[row + x].to_i32() + r[y * 4 + x];
            data[row + x] = S::clip(p, max);
        }
    }
}

/// Add an 8x8 residual to the plane with clamping (spec 8.5.13 / picture
/// construction).
#[inline]
pub fn add_residual_8x8<S: Sample>(
    data: &mut [S],
    bd: u32,
    stride: usize,
    origin: usize,
    r: &[i32; 64],
) {
    let max = (1i32 << bd) - 1;
    for y in 0..8 {
        let row = origin + y * stride;
        for x in 0..8 {
            let p = data[row + x].to_i32() + r[y * 8 + x];
            data[row + x] = S::clip(p, max);
        }
    }
}

/// Add a residual of a macroblock coded with TransformBypassModeFlag when the
/// intra prediction is purely horizontal or vertical (8.5.15): the residual is
/// accumulated along the prediction direction, so each sample is the sample
/// before it (left for `horizontal`, above otherwise) plus its own residual.
///
/// `r` is `w * h` values in raster order. `first` says the block opens the
/// accumulation: its first column (row) starts from the prediction already in
/// the plane rather than from the reconstructed block beside (above) it. For an
/// Intra_4x4 or Intra_8x8 block that is always so; for Intra_16x16 and the
/// chroma of a 4:2:0 / 4:2:2 macroblock the accumulation spans the whole
/// macroblock, and so continues across the blocks inside it.
#[allow(clippy::too_many_arguments)]
pub fn add_residual_dpcm<S: Sample>(
    data: &mut [S],
    bd: u32,
    stride: usize,
    origin: usize,
    w: usize,
    h: usize,
    r: &[i32],
    horizontal: bool,
    first: bool,
) {
    let max = (1i32 << bd) - 1;
    for y in 0..h {
        let row = origin + y * stride;
        for x in 0..w {
            let base = if horizontal {
                if x == 0 && !first {
                    data[row - 1].to_i32()
                } else if x == 0 {
                    data[row].to_i32()
                } else {
                    data[row + x - 1].to_i32()
                }
            } else if y == 0 && !first {
                data[row - stride + x].to_i32()
            } else if y == 0 {
                data[row + x].to_i32()
            } else {
                data[row - stride + x].to_i32()
            };
            data[row + x] = S::clip(base + r[y * w + x], max);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A constant neighbourhood survives the reference filter and every 8x8
    /// mode reproduces it: the filters are normalised (1+2+1)/4 and the modes
    /// are weighted averages of the same samples.
    #[test]
    fn constant_neighbourhood_is_a_fixed_point() {
        let n = Nbr8 {
            top: [77; 16],
            left: [77; 8],
            top_left: 77,
            have_top: true,
            have_left: true,
            have_tl: true,
        };
        let f = filter_nbr8(&n);
        assert_eq!(f.top, [77; 16]);
        assert_eq!(f.left, [77; 8]);
        assert_eq!(f.top_left, 77);
        for mode in 0..9u8 {
            let mut out = [0u8; 64];
            pred_8x8(mode, 8, &f, &mut out);
            assert!(out.iter().all(|&v| v == 77), "mode {mode}: {out:?}");
        }
    }

    /// The reference filter of 8.3.2.2.1 with the corner unavailable uses the
    /// 3:1 end taps, and never runs over a run that is not wholly available.
    #[test]
    fn reference_filter_edges() {
        let mut top = [0u8; 16];
        for (x, t) in top.iter_mut().enumerate() {
            *t = (x * 4) as u8;
        }
        let n = Nbr8 {
            top,
            left: [100; 8],
            top_left: 9,
            have_top: true,
            have_left: false,
            have_tl: false,
        };
        let f = filter_nbr8(&n);
        // No left run: p'[0, -1] takes 8-79, and the left column is untouched.
        // 8-79 with p[0, -1] = 0 and p[1, -1] = 4.
        assert_eq!(f.top[0], ((4 + 2) >> 2) as u8);
        // 8-80 over p[0..2, -1] = 0, 4, 8.
        assert_eq!(f.top[1], ((2 * 4 + 8 + 2) >> 2) as u8);
        assert_eq!(f.top[15], ((56 + 3 * 60 + 2) >> 2) as u8);
        assert_eq!(f.left, [100; 8]);
        assert_eq!(f.top_left, 9);
        // DC with no left run averages the top run alone (8-93).
        let mut out = [0u8; 64];
        pred_8x8(2, 8, &f, &mut out);
        let sum: i32 = (0..8).map(|i| i32::from(f.top[i])).sum();
        assert!(out.iter().all(|&v| i32::from(v) == (sum + 4) >> 3));
    }

    /// Vertical and horizontal 8x8 copy the filtered runs.
    #[test]
    fn vertical_horizontal_8x8() {
        let mut top = [0u8; 16];
        for (x, t) in top.iter_mut().enumerate() {
            *t = (x * 3) as u8;
        }
        let n = Nbr8 {
            top,
            left: [1, 2, 3, 4, 5, 6, 7, 8],
            top_left: 0,
            have_top: true,
            have_left: true,
            have_tl: true,
        };
        let mut out = [0u8; 64];
        pred_8x8(0, 8, &n, &mut out);
        assert_eq!(&out[..8], &n.top[..8]);
        assert_eq!(&out[56..], &n.top[..8]);
        pred_8x8(1, 8, &n, &mut out);
        assert_eq!(out[0], 1);
        assert_eq!(out[63], 8);
    }

    #[test]
    fn dc_4x4_fallbacks() {
        let n = Nbr4 {
            top: [10; 8],
            left: [20; 4],
            top_left: 0,
            have_top: true,
            have_left: true,
        };
        let mut out = [0u8; 16];
        pred_4x4(2, 8, &n, &mut out);
        assert!(out.iter().all(|&v| v == 15)); // (40 + 80 + 4) >> 3
        let n2 = Nbr4 {
            have_left: false,
            ..n
        };
        pred_4x4(2, 8, &n2, &mut out);
        assert!(out.iter().all(|&v| v == 10));
        let n3 = Nbr4 {
            have_top: false,
            have_left: false,
            ..n
        };
        pred_4x4(2, 8, &n3, &mut out);
        assert!(out.iter().all(|&v| v == 128));
    }

    #[test]
    fn vertical_and_horizontal_4x4() {
        let n = Nbr4 {
            top: [1, 2, 3, 4, 5, 6, 7, 8],
            left: [9, 10, 11, 12],
            top_left: 0,
            have_top: true,
            have_left: true,
        };
        let mut out = [0u8; 16];
        pred_4x4(0, 8, &n, &mut out);
        assert_eq!(&out[0..4], &[1, 2, 3, 4]);
        assert_eq!(&out[12..16], &[1, 2, 3, 4]);
        pred_4x4(1, 8, &n, &mut out);
        assert_eq!(out[0], 9);
        assert_eq!(out[15], 12);
    }

    /// Diagonal down-left of a constant edge stays constant; the (3,3)
    /// special tap agrees.
    #[test]
    fn ddl_constant_edge() {
        let n = Nbr4 {
            top: [50; 8],
            left: [0; 4],
            top_left: 0,
            have_top: true,
            have_left: false,
        };
        let mut out = [0u8; 16];
        pred_4x4(3, 8, &n, &mut out);
        assert!(out.iter().all(|&v| v == 50));
    }

    /// Plane prediction of a linear ramp reproduces the ramp.
    #[test]
    fn plane_16x16_ramp() {
        let stride = 24usize;
        let mut data = vec![0u8; stride * 24];
        // Fill neighbours: top row and left column of a 16x16 at (1, 1).
        let origin = stride + 1;
        for (x, d) in data.iter_mut().enumerate().take(17) {
            *d = (10 + 2 * x) as u8; // includes top-left at x=0
        }
        for y in 0..17 {
            data[y * stride] = (10 + 3 * y) as u8;
        }
        let mut w = PlaneWindow {
            data: &mut data,
            stride,
            origin,
        };
        pred_16x16(3, 8, &mut w, true, true);
        // A perfect plane through those edges: value at (x, y) approximately
        // 12 + 2x + 3y. Check corners within rounding of the spec filter.
        let v = |x: usize, y: usize| i32::from(data[origin + y * stride + x]);
        assert!((v(0, 0) - 17).abs() <= 3, "{}", v(0, 0));
        assert!((v(15, 0) - (12 + 30)).abs() <= 3);
        assert!((v(0, 15) - (12 + 45)).abs() <= 3);
        assert!((v(15, 15) - (12 + 30 + 45)).abs() <= 3);
    }

    #[test]
    fn chroma_dc_corner_preferences() {
        let stride = 16usize;
        let mut data = vec![0u8; stride * 16];
        let origin = stride + 1;
        // Top neighbours 40, left neighbours 80.
        for x in 0..8 {
            data[1 + x] = 40;
        }
        for y in 0..8 {
            data[(1 + y) * stride] = 80;
        }
        let mut w = PlaneWindow {
            data: &mut data,
            stride,
            origin,
        };
        pred_chroma_8x8(0, 8, &mut w, true, true);
        let v = |x: usize, y: usize| data[origin + y * stride + x];
        assert_eq!(v(0, 0), 60); // both edges
        assert_eq!(v(7, 0), 40); // top preferred
        assert_eq!(v(0, 7), 80); // left preferred
        assert_eq!(v(7, 7), 60); // both edges
    }

    #[test]
    fn add_residual_clamps() {
        let mut data = vec![250u8; 16];
        let mut r = [0i32; 16];
        r[0] = 100;
        r[1] = -300;
        add_residual_4x4(&mut data, 8, 4, 0, &r);
        assert_eq!(data[0], 255);
        assert_eq!(data[1], 0);
        assert_eq!(data[2], 250);
    }
}

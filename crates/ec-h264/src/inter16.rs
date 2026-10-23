//! Fractional-sample interpolation (clause 8.4.2.2) for planes of `u16`
//! samples, written straight from the equations: each partition reads a
//! clamped window of the reference and computes in `i32`. The 8-bit decoder
//! has its own SIMD kernels in [`crate::inter`]; this is the High 10 path.

use crate::dpb::PicPlane;

/// Largest window a 16x16 luma partition reads: 21 x 21.
const WIN: usize = 21 * 21;

/// Copy the `ww` x `wh` window whose top-left sample is `(x0, y0)` out of the
/// plane, clamping coordinates into the picture (Equations 8-239/8-240).
fn window(p: &PicPlane<u16>, x0: i32, y0: i32, ww: usize, wh: usize, buf: &mut [i32]) {
    let (pw, ph) = (p.width as i32, p.height as i32);
    for r in 0..wh {
        let y = (y0 + r as i32).clamp(0, ph - 1) as usize;
        let row = p.at(0, y);
        for c in 0..ww {
            let x = (x0 + c as i32).clamp(0, pw - 1) as usize;
            buf[r * ww + c] = i32::from(p.data[row + x]);
        }
    }
}

/// Luma prediction samples (8.4.2.2.1) of a `w` x `h` partition at `(x, y)`
/// displaced by `mv` (quarter samples), into `out` at pitch `w`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn luma_pred(
    p: &PicPlane<u16>,
    x: i32,
    y: i32,
    mv: [i16; 2],
    w: usize,
    h: usize,
    max: i32,
    out: &mut [i32; 256],
) {
    let (fx, fy) = ((mv[0] & 3) as i32, (mv[1] & 3) as i32);
    let (xi, yi) = (x + (i32::from(mv[0]) >> 2), y + (i32::from(mv[1]) >> 2));
    let (ww, wh) = (w + 5, h + 5);
    let mut win = [0i32; WIN];
    window(p, xi - 2, yi - 2, ww, wh, &mut win);
    let clip = |v: i32| v.clamp(0, max);
    for row in 0..h {
        for col in 0..w {
            let s = |dx: i32, dy: i32| {
                win[((row as i32 + 2 + dy) as usize) * ww + (col as i32 + 2 + dx) as usize]
            };
            let hf = |dy: i32| {
                s(-2, dy) - 5 * s(-1, dy) + 20 * s(0, dy) + 20 * s(1, dy) - 5 * s(2, dy) + s(3, dy)
            };
            let vf = |dx: i32| {
                s(dx, -2) - 5 * s(dx, -1) + 20 * s(dx, 0) + 20 * s(dx, 1) - 5 * s(dx, 2) + s(dx, 3)
            };
            let g = |dx: i32, dy: i32| s(dx, dy);
            let b = |dy: i32| clip((hf(dy) + 16) >> 5);
            let hs = |dx: i32| clip((vf(dx) + 16) >> 5);
            let j = || {
                let t = |k: i32| hf(k);
                clip((t(-2) - 5 * t(-1) + 20 * t(0) + 20 * t(1) - 5 * t(2) + t(3) + 512) >> 10)
            };
            let avg = |a: i32, b: i32| (a + b + 1) >> 1;
            out[row * w + col] = match (fx, fy) {
                (0, 0) => g(0, 0),
                (1, 0) => avg(g(0, 0), b(0)),
                (2, 0) => b(0),
                (3, 0) => avg(g(1, 0), b(0)),
                (0, 1) => avg(g(0, 0), hs(0)),
                (0, 2) => hs(0),
                (0, 3) => avg(g(0, 1), hs(0)),
                (1, 1) => avg(b(0), hs(0)),
                (3, 1) => avg(b(0), hs(1)),
                (1, 3) => avg(hs(0), b(1)),
                (3, 3) => avg(hs(1), b(1)),
                (2, 1) => avg(b(0), j()),
                (2, 3) => avg(b(1), j()),
                (1, 2) => avg(hs(0), j()),
                (3, 2) => avg(hs(1), j()),
                _ => j(),
            };
        }
    }
}

/// Chroma prediction samples (8.4.2.2.2, Equation 8-270) of a `w` x `h` block
/// at `(x, y)` displaced by `mv` (eighth samples), into `out` at pitch `w`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn chroma_pred(
    p: &PicPlane<u16>,
    x: i32,
    y: i32,
    mv: [i16; 2],
    w: usize,
    h: usize,
    out: &mut [i32; 256],
) {
    let (fx, fy) = ((mv[0] & 7) as i32, (mv[1] & 7) as i32);
    let (xi, yi) = (x + (i32::from(mv[0]) >> 3), y + (i32::from(mv[1]) >> 3));
    let (ww, wh) = (w + 1, h + 1);
    let mut win = [0i32; 9 * 17];
    window(p, xi, yi, ww, wh, &mut win);
    for row in 0..h {
        for col in 0..w {
            let a = win[row * ww + col];
            let b = win[row * ww + col + 1];
            let c = win[(row + 1) * ww + col];
            let d = win[(row + 1) * ww + col + 1];
            out[row * w + col] = ((8 - fx) * (8 - fy) * a
                + fx * (8 - fy) * b
                + (8 - fx) * fy * c
                + fx * fy * d
                + 32)
                >> 6;
        }
    }
}

//! Motion compensation (8.5.3.3): fractional sample interpolation and the
//! prediction-unit driver that turns a motion vector into final samples.
//!
//! Samples are `u16` planes of 8 to 10 bits, 4:2:0 chroma. The interpolation is
//! separable and keeps the spec's intermediate precision: `predSamplesLX` values
//! live in `i32` buffers. Reference coordinates are clamped to the picture per
//! sample (the spec's `Clip3`); the source window is gathered once per block so
//! the filter loops need no bounds logic. Scratch buffers are reused across calls
//! through [`McScratch`], so the hot path does not allocate after warm-up.

use super::cu_syntax::Motion;
use super::weighted::{ExplicitWeights, Weight, store};
use ec_core::error::{Error, Result};

/// The largest prediction block side (a 64x64 PU).
pub const MAX_PU: usize = 64;

/// Table 8-11: the luma 8-tap filters, indexed by quarter-sample fraction. Index 0
/// is the full-sample position, which the interpolation never filters.
pub const LUMA_FILTER: [[i32; 8]; 4] = [
    [0, 0, 0, 64, 0, 0, 0, 0],
    [-1, 4, -10, 58, 17, -5, 1, 0],
    [-1, 4, -11, 40, 40, -11, 4, -1],
    [0, 1, -5, 17, 58, -10, 4, -1],
];

/// Table 8-12: the chroma 4-tap filters, indexed by eighth-sample fraction. Index 0
/// is the full-sample position, which the interpolation never filters.
pub const CHROMA_FILTER: [[i32; 4]; 8] = [
    [0, 64, 0, 0],
    [-2, 58, 10, -2],
    [-4, 54, 16, -2],
    [-6, 46, 28, -4],
    [-4, 36, 36, -4],
    [-4, 28, 46, -6],
    [-2, 16, 54, -4],
    [-2, 10, 58, -2],
];

/// One sample plane of a reference picture.
#[derive(Clone, Copy, Debug)]
pub struct PlaneRef<'a> {
    /// Samples, row-major, `stride` apart.
    pub samples: &'a [u16],
    /// Distance between row starts.
    pub stride: usize,
    /// Plane width in samples: the clamping bound of the reference coordinates.
    pub width: usize,
    /// Plane height in samples.
    pub height: usize,
    /// `BitDepthY` or `BitDepthC`, 8 to 10.
    pub bit_depth: u32,
}

/// One writable sample plane of the picture being predicted.
#[derive(Debug)]
pub struct PlaneMut<'a> {
    /// Samples, row-major, `stride` apart.
    pub samples: &'a mut [u16],
    /// Distance between row starts.
    pub stride: usize,
    /// `BitDepthY` or `BitDepthC`, 8 to 10.
    pub bit_depth: u32,
    /// The first row of `samples`, in the plane's own rows (zero for a whole plane).
    pub origin: usize,
}

/// The Y, Cb and Cr planes of one reference picture.
pub type RefPicture<'a> = [PlaneRef<'a>; 3];

/// A rectangle inside one plane, in samples of that plane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    /// Left column.
    pub x: usize,
    /// Top row.
    pub y: usize,
    /// Width in samples.
    pub width: usize,
    /// Height in samples.
    pub height: usize,
}

/// Reusable scratch buffers for the motion compensation of one decoder.
#[derive(Debug, Default)]
pub struct McScratch {
    /// The gathered reference window.
    src: Vec<u16>,
    /// The horizontally filtered rows of a two-dimensional interpolation.
    tmp: Vec<i32>,
    /// `predSamplesL0` and `predSamplesL1` of the current component.
    pred: [Vec<i32>; 2],
}

impl McScratch {
    /// Empty buffers; they grow to the largest block seen and stay there.
    pub fn new() -> McScratch {
        McScratch::default()
    }
}

/// Predict one prediction unit into the three output planes.
///
/// `refs[list][ref_idx]` is the reference picture each list's `refIdxLX` names.
/// `pu` is the luma rectangle; the chroma rectangle is half of it in each
/// direction (4:2:0). `weights` selects explicit weighted prediction; `None` is
/// the default weighted prediction. Each output plane receives the clipped final
/// samples of the rectangle.
pub fn predict_pu(
    refs: [&[RefPicture<'_>]; 2],
    motion: &Motion,
    weights: Option<&ExplicitWeights>,
    pu: Block,
    out: &mut [PlaneMut<'_>; 3],
    scratch: &mut McScratch,
) -> Result<()> {
    predict_pu_fmt(refs, motion, weights, pu, (1, 1), out, scratch)
}

/// [`predict_pu`] for any chroma subsampling. `sub` is the log2 `(sub_x, sub_y)` of the
/// chroma planes: `(1, 1)` for 4:2:0, `(1, 0)` for 4:2:2, `(0, 0)` for 4:4:4. The chroma
/// vector is then `mvLX * 2 / SubWidthC` and `mvLX * 2 / SubHeightC` in 1/8 chroma samples
/// (8.5.3.2.10), and the chroma rectangle is `pu` shifted by `sub`.
#[allow(clippy::too_many_arguments)] // the PU's refs, motion, weights and output
pub fn predict_pu_fmt(
    refs: [&[RefPicture<'_>]; 2],
    motion: &Motion,
    weights: Option<&ExplicitWeights>,
    pu: Block,
    sub: (u32, u32),
    out: &mut [PlaneMut<'_>; 3],
    scratch: &mut McScratch,
) -> Result<()> {
    let used = motion.pred_flags;
    if !used[0] && !used[1] {
        return Err(Error::corrupt(
            "HEVC motion compensation: a prediction unit with no list in use",
        ));
    }
    let mut pics: [Option<&RefPicture<'_>>; 2] = [None, None];
    let mut ref_idx = [0usize; 2];
    for list in 0..2 {
        if !used[list] {
            continue;
        }
        let idx = usize::try_from(motion.ref_idx[list])
            .ok()
            .filter(|&i| i < refs[list].len())
            .ok_or_else(|| {
                Error::corrupt(format!(
                    "HEVC motion compensation: refIdxL{list} = {} has no picture",
                    motion.ref_idx[list]
                ))
            })?;
        ref_idx[list] = idx;
        pics[list] = Some(&refs[list][idx]);
    }
    let McScratch { src, tmp, pred } = scratch;
    for comp in 0..3 {
        let luma = comp == 0;
        let blk = if luma {
            pu
        } else {
            Block {
                x: pu.x >> sub.0,
                y: pu.y >> sub.1,
                width: pu.width >> sub.0,
                height: pu.height >> sub.1,
            }
        };
        check_out(&out[comp], blk)?;
        for (list, buf) in pred.iter_mut().enumerate() {
            let Some(pic) = pics[list] else { continue };
            buf.resize(blk.width * blk.height, 0);
            // Chroma vector (8.5.3.2.10): mvCLX = mvLX * 2 / SubWidthC (and SubHeightC),
            // in 1/8 chroma samples.
            let mv = if luma {
                motion.mv[list]
            } else {
                let m = motion.mv[list];
                [(m[0] * 2) >> sub.0, (m[1] * 2) >> sub.1]
            };
            interp_component(&pic[comp], blk, mv, luma, src, tmp, buf)?;
        }
        let explicit = match weights {
            Some(ew) => {
                let mut w = [Weight::default(); 2];
                for (list, slot) in w.iter_mut().enumerate() {
                    if used[list] {
                        *slot = ew.lookup(list, ref_idx[list], comp)?;
                    }
                }
                Some((ew.log2_denom[comp], w))
            }
            None => None,
        };
        let preds = [
            used[0].then(|| pred[0].as_slice()),
            used[1].then(|| pred[1].as_slice()),
        ];
        store(&mut out[comp], blk, preds, explicit)?;
    }
    Ok(())
}

/// The luma fractional sample interpolation (8.5.3.3.3.1) of one block.
///
/// `mv` is the motion vector in quarter samples. The `blk.width * blk.height`
/// results of `predSamplesLX` are written to `out` row by row, at 14-bit precision.
pub fn interp_luma(
    plane: &PlaneRef<'_>,
    blk: Block,
    mv: [i32; 2],
    scratch: &mut McScratch,
    out: &mut [i32],
) -> Result<()> {
    check_plane(plane)?;
    check_block(blk, out.len())?;
    let McScratch { src, tmp, .. } = scratch;
    interp_component(plane, blk, mv, true, src, tmp, out)
}

/// The chroma fractional sample interpolation (8.5.3.3.3.2) of one block, 4:2:0.
///
/// `mv` is the luma motion vector in quarter samples, which in 4:2:0 is the
/// chroma motion vector in eighth samples. `blk` is in chroma samples.
pub fn interp_chroma(
    plane: &PlaneRef<'_>,
    blk: Block,
    mv: [i32; 2],
    scratch: &mut McScratch,
    out: &mut [i32],
) -> Result<()> {
    check_plane(plane)?;
    check_block(blk, out.len())?;
    let McScratch { src, tmp, .. } = scratch;
    interp_component(plane, blk, mv, false, src, tmp, out)
}

/// Interpolate one block of one component, luma or 4:2:0 chroma.
fn interp_component(
    plane: &PlaneRef<'_>,
    blk: Block,
    mv: [i32; 2],
    luma: bool,
    src: &mut Vec<u16>,
    tmp: &mut Vec<i32>,
    out: &mut [i32],
) -> Result<()> {
    if luma {
        let (x_int, y_int) = (
            blk.x as i64 + i64::from(mv[0] >> 2),
            blk.y as i64 + i64::from(mv[1] >> 2),
        );
        let fx = (mv[0] & 3 != 0).then(|| &LUMA_FILTER[(mv[0] & 3) as usize]);
        let fy = (mv[1] & 3 != 0).then(|| &LUMA_FILTER[(mv[1] & 3) as usize]);
        interpolate(
            plane,
            (x_int, y_int),
            (blk.width, blk.height),
            [fx, fy],
            src,
            tmp,
            out,
        );
    } else {
        let (x_int, y_int) = (
            blk.x as i64 + i64::from(mv[0] >> 3),
            blk.y as i64 + i64::from(mv[1] >> 3),
        );
        let fx = (mv[0] & 7 != 0).then(|| &CHROMA_FILTER[(mv[0] & 7) as usize]);
        let fy = (mv[1] & 7 != 0).then(|| &CHROMA_FILTER[(mv[1] & 7) as usize]);
        interpolate(
            plane,
            (x_int, y_int),
            (blk.width, blk.height),
            [fx, fy],
            src,
            tmp,
            out,
        );
    }
    Ok(())
}

/// The shared separable interpolation for a `T`-tap filter (`T` = 8 luma, 4 chroma).
///
/// `at` is the integer position of the block's top-left sample in the reference
/// plane, `filters` the horizontal and vertical coefficients (`None` when that
/// direction is at a full-sample position). The equations of 8.5.3.3.3 are
/// `shift1 = Min(4, BitDepth - 8)`, `shift2 = 6`, `shift3 = Max(2, 14 - BitDepth)`.
fn interpolate<const T: usize>(
    plane: &PlaneRef<'_>,
    at: (i64, i64),
    size: (usize, usize),
    filters: [Option<&[i32; T]>; 2],
    src: &mut Vec<u16>,
    tmp: &mut Vec<i32>,
    out: &mut [i32],
) {
    let (w, h) = size;
    let pad = T / 2 - 1;
    let bw = w + T - 1;
    let bh = h + T - 1;
    let (x0, y0) = (at.0 - pad as i64, at.1 - pad as i64);
    // A window wholly inside the plane is read in place; otherwise it is gathered
    // with the reference coordinates clamped.
    let inside =
        x0 >= 0 && y0 >= 0 && x0 as usize + bw <= plane.width && y0 as usize + bh <= plane.height;
    let (view, vs): (&[u16], usize) = if inside {
        (
            &plane.samples[y0 as usize * plane.stride + x0 as usize..],
            plane.stride,
        )
    } else {
        gather(plane, x0, y0, bw, bh, src);
        (src.as_slice(), bw)
    };
    let bd = plane.bit_depth;
    let shift1 = (bd - 8).min(4);
    let shift3 = 14u32.saturating_sub(bd).max(2);
    let out = &mut out[..w * h];
    match filters {
        [None, None] => {
            for (r, out_row) in out.chunks_exact_mut(w).enumerate() {
                let row = &view[(r + pad) * vs + pad..][..w];
                for (o, &s) in out_row.iter_mut().zip(row) {
                    *o = i32::from(s) << shift3;
                }
            }
        }
        [Some(cx), None] => {
            for (r, out_row) in out.chunks_exact_mut(w).enumerate() {
                filter_row(&view[(r + pad) * vs..][..bw], cx, shift1, out_row);
            }
        }
        [None, Some(cy)] => {
            for (r, out_row) in out.chunks_exact_mut(w).enumerate() {
                filter_col(&view[r * vs + pad..], vs, cy, shift1, out_row);
            }
        }
        [Some(cx), Some(cy)] => {
            tmp.resize(bh * w, 0);
            for (r, tmp_row) in tmp.chunks_exact_mut(w).enumerate() {
                filter_row(&view[r * vs..][..bw], cx, shift1, tmp_row);
            }
            for (r, out_row) in out.chunks_exact_mut(w).enumerate() {
                filter_col(&tmp[r * w..], w, cy, 6, out_row);
            }
        }
    }
}

/// One horizontal filter pass: `out[c] = (sum_i coef[i] * row[c + i]) >> shift`.
///
/// The taps run in the outer loop so each pass is a plain multiply-add over a
/// contiguous run of outputs, which the compiler vectorises. `out.len()` is at most
/// [`MAX_PU`], the accumulator size.
fn filter_row<const T: usize>(row: &[u16], coef: &[i32; T], shift: u32, out: &mut [i32]) {
    filter_taps(coef, row, 1, shift, out);
}

/// One vertical filter pass: `out[c] = (sum_i coef[i] * src[i * stride + c]) >> shift`.
/// Same shape as [`filter_row`], with the taps `stride` apart.
fn filter_col<S: Copy + Into<i32>, const T: usize>(
    src: &[S],
    stride: usize,
    coef: &[i32; T],
    shift: u32,
    out: &mut [i32],
) {
    filter_taps(coef, src, stride, shift, out);
}

/// The shared tap loop of [`filter_row`] and [`filter_col`]. The taps run in the
/// outer loop so each pass is a plain multiply-add over a contiguous run of outputs.
fn filter_taps<S: Copy + Into<i32>, const T: usize>(
    coef: &[i32; T],
    src: &[S],
    stride: usize,
    shift: u32,
    out: &mut [i32],
) {
    let n = out.len();
    let mut acc = [0i32; MAX_PU];
    let acc = &mut acc[..n];
    for (i, &k) in coef.iter().enumerate() {
        for (a, &s) in acc.iter_mut().zip(&src[i * stride..][..n]) {
            *a += k * s.into();
        }
    }
    for (o, &a) in out.iter_mut().zip(acc.iter()) {
        *o = a >> shift;
    }
}

/// Copy the `bw x bh` reference window whose top-left sample is `(x0, y0)` into
/// `src`, clamping every coordinate into the plane (the spec's `Clip3` on
/// `xInt` and `yInt`, applied per sample). A window inside the plane horizontally is
/// copied a row at a time.
fn gather(plane: &PlaneRef<'_>, x0: i64, y0: i64, bw: usize, bh: usize, src: &mut Vec<u16>) {
    src.resize(bw * bh, 0);
    let max_x = plane.width as i64 - 1;
    let max_y = plane.height as i64 - 1;
    let inside_x = x0 >= 0 && x0 + bw as i64 - 1 <= max_x;
    for (r, dst_row) in src.chunks_exact_mut(bw).enumerate() {
        let sy = (y0 + r as i64).clamp(0, max_y) as usize;
        let row = &plane.samples[sy * plane.stride..sy * plane.stride + plane.width];
        if inside_x {
            let x = x0 as usize;
            dst_row.copy_from_slice(&row[x..x + bw]);
        } else {
            for (c, d) in dst_row.iter_mut().enumerate() {
                *d = row[(x0 + c as i64).clamp(0, max_x) as usize];
            }
        }
    }
}

/// Reject a plane whose geometry or bit depth the interpolation cannot read.
pub(super) fn check_plane(p: &PlaneRef<'_>) -> Result<()> {
    if p.width == 0
        || p.height == 0
        || p.stride < p.width
        || p.samples.len() < p.stride * (p.height - 1) + p.width
    {
        return Err(Error::corrupt(
            "HEVC motion compensation: reference plane geometry",
        ));
    }
    check_depth(p.bit_depth)
}

/// Reject a bit depth outside the 8 to 10 bits this module handles.
pub(super) fn check_depth(bit_depth: u32) -> Result<()> {
    if (8..=12).contains(&bit_depth) {
        Ok(())
    } else {
        Err(Error::unsupported(
            format!("bit depth {bit_depth}"),
            "motion compensation handles 8 to 10 bits",
        ))
    }
}

/// Reject a block that is empty, larger than a PU, or too big for its buffer.
fn check_block(blk: Block, out_len: usize) -> Result<()> {
    if blk.width == 0
        || blk.height == 0
        || blk.width > MAX_PU
        || blk.height > MAX_PU
        || blk.width * blk.height > out_len
    {
        return Err(Error::corrupt(
            "HEVC motion compensation: block size or output buffer",
        ));
    }
    Ok(())
}

/// Reject an output rectangle that does not fit inside its plane.
pub(super) fn check_out(dst: &PlaneMut<'_>, blk: Block) -> Result<()> {
    if blk.width == 0 || blk.height == 0 || blk.x + blk.width > dst.stride || blk.y < dst.origin {
        return Err(Error::corrupt(
            "HEVC motion compensation: output rectangle outside the plane",
        ));
    }
    // The last sample written is the bottom-right one.
    let last = (blk.y + blk.height - 1 - dst.origin) * dst.stride + blk.x + blk.width - 1;
    if last >= dst.samples.len() {
        return Err(Error::corrupt(
            "HEVC motion compensation: output rectangle outside the plane",
        ));
    }
    check_depth(dst.bit_depth)
}

#[cfg(test)]
#[path = "mc_tests.rs"]
mod tests;

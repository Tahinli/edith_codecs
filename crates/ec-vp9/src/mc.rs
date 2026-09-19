//! Motion compensation: the decoder's inter predictor (`dec_build_inter_predictors`,
//! libvpx `vp9/decoder/vp9_decodeframe.c:593`), the on-the-fly reference border
//! extension (`build_mc_border`, :473), the UMV-border MV clamp
//! (`clamp_mv_to_umv_border_sb`, `vp9/common/vp9_reconinter.c:88`), the
//! switchable filter kernels (`vp9/common/vp9_filter.c`) and the sub-pixel
//! convolve (`vpx_dsp/vpx_convolve.c`).
//!
//! The VP9 decoder does NOT extend the reference frame's borders in place: it
//! builds a bordered copy of the block's reference window per prediction
//! (`extend_and_predict`) whenever the window leaves the frame or the frame's
//! dimensions are not a multiple of 8. This module ports that path, unscaled
//! only — a reference slot whose size differs from the current frame is
//! refused by `decode` before any predictor runs.
//!
//! Indexing note: libvpx's convolve helpers take a raw pointer and subtract
//! `SUBPEL_TAPS / 2 - 1` from it, so they read taps *before* the block's first
//! sample. Rust slices cannot go negative, so every helper here takes a buffer
//! plus an `origin` (the block's first sample) and indexes with `isize`.

/// `SUBPEL_BITS` / `SUBPEL_MASK` / `SUBPEL_SHIFTS` / `SUBPEL_TAPS`
/// (`vpx_dsp/vpx_filter.h:23-26`).
const SUBPEL_BITS: i32 = 4;
const SUBPEL_MASK: i32 = (1 << SUBPEL_BITS) - 1;
const SUBPEL_SHIFTS: i32 = 1 << SUBPEL_BITS;
const TAPS: usize = 8;
/// `FILTER_BITS` (`vpx_dsp/vpx_filter.h:21`).
const FILTER_BITS: i32 = 7;
/// `VP9_INTERP_EXTEND` (`vpx_scale/yv12config.h:25`).
const INTERP_EXTEND: i32 = 4;

/// `vp9_filter_kernels` (`vp9/common/vp9_filter.c:79`), in libvpx's filter
/// numbering: 0 `EIGHTTAP`, 1 `EIGHTTAP_SMOOTH`, 2 `EIGHTTAP_SHARP`,
/// 3 `BILINEAR` (the 4-tap index 4 is unreachable for profile 0).
pub(crate) const FILTERS: [[[i16; TAPS]; SUBPEL_SHIFTS as usize]; 4] = [
    // sub_pel_filters_8: Lagrangian interpolation filter.
    [
        [0, 0, 0, 128, 0, 0, 0, 0],
        [0, 1, -5, 126, 8, -3, 1, 0],
        [-1, 3, -10, 122, 18, -6, 2, 0],
        [-1, 4, -13, 118, 27, -9, 3, -1],
        [-1, 4, -16, 112, 37, -11, 4, -1],
        [-1, 5, -18, 105, 48, -14, 4, -1],
        [-1, 5, -19, 97, 58, -16, 5, -1],
        [-1, 6, -19, 88, 68, -18, 5, -1],
        [-1, 6, -19, 78, 78, -19, 6, -1],
        [-1, 5, -18, 68, 88, -19, 6, -1],
        [-1, 5, -16, 58, 97, -19, 5, -1],
        [-1, 4, -14, 48, 105, -18, 5, -1],
        [-1, 4, -11, 37, 112, -16, 4, -1],
        [-1, 3, -9, 27, 118, -13, 4, -1],
        [0, 2, -6, 18, 122, -10, 3, -1],
        [0, 1, -3, 8, 126, -5, 1, 0],
    ],
    // sub_pel_filters_8lp: freqmultiplier = 0.5 (EIGHTTAP_SMOOTH).
    [
        [0, 0, 0, 128, 0, 0, 0, 0],
        [-3, -1, 32, 64, 38, 1, -3, 0],
        [-2, -2, 29, 63, 41, 2, -3, 0],
        [-2, -2, 26, 63, 43, 4, -4, 0],
        [-2, -3, 24, 62, 46, 5, -4, 0],
        [-2, -3, 21, 60, 49, 7, -4, 0],
        [-1, -4, 18, 59, 51, 9, -4, 0],
        [-1, -4, 16, 57, 53, 12, -4, -1],
        [-1, -4, 14, 55, 55, 14, -4, -1],
        [-1, -4, 12, 53, 57, 16, -4, -1],
        [0, -4, 9, 51, 59, 18, -4, -1],
        [0, -4, 7, 49, 60, 21, -3, -2],
        [0, -4, 5, 46, 62, 24, -3, -2],
        [0, -4, 4, 43, 63, 26, -2, -2],
        [0, -3, 2, 41, 63, 29, -2, -2],
        [0, -3, 1, 38, 64, 32, -1, -3],
    ],
    // sub_pel_filters_8s: DCT based filter (EIGHTTAP_SHARP).
    [
        [0, 0, 0, 128, 0, 0, 0, 0],
        [-1, 3, -7, 127, 8, -3, 1, 0],
        [-2, 5, -13, 125, 17, -6, 3, -1],
        [-3, 7, -17, 121, 27, -10, 5, -2],
        [-4, 9, -20, 115, 37, -13, 6, -2],
        [-4, 10, -23, 108, 48, -16, 8, -3],
        [-4, 10, -24, 100, 59, -19, 9, -3],
        [-4, 11, -24, 90, 70, -21, 10, -4],
        [-4, 11, -23, 80, 80, -23, 11, -4],
        [-4, 10, -21, 70, 90, -24, 11, -4],
        [-3, 9, -19, 59, 100, -24, 10, -4],
        [-3, 8, -16, 48, 108, -23, 10, -4],
        [-2, 6, -13, 37, 115, -20, 9, -4],
        [-2, 5, -10, 27, 121, -17, 7, -3],
        [-1, 3, -6, 17, 125, -13, 5, -2],
        [0, 1, -3, 8, 127, -7, 3, -1],
    ],
    // bilinear_filters.
    [
        [0, 0, 0, 128, 0, 0, 0, 0],
        [0, 0, 0, 120, 8, 0, 0, 0],
        [0, 0, 0, 112, 16, 0, 0, 0],
        [0, 0, 0, 104, 24, 0, 0, 0],
        [0, 0, 0, 96, 32, 0, 0, 0],
        [0, 0, 0, 88, 40, 0, 0, 0],
        [0, 0, 0, 80, 48, 0, 0, 0],
        [0, 0, 0, 72, 56, 0, 0, 0],
        [0, 0, 0, 64, 64, 0, 0, 0],
        [0, 0, 0, 56, 72, 0, 0, 0],
        [0, 0, 0, 48, 80, 0, 0, 0],
        [0, 0, 0, 40, 88, 0, 0, 0],
        [0, 0, 0, 32, 96, 0, 0, 0],
        [0, 0, 0, 24, 104, 0, 0, 0],
        [0, 0, 0, 16, 112, 0, 0, 0],
        [0, 0, 0, 8, 120, 0, 0, 0],
    ],
];

use crate::{Sample, clip_pixel_bd, round_pow2};

/// Read `buf[i]` for a possibly negative `i`. Every caller keeps `i` inside
/// the buffer (the border copy guarantees the tap margins exist).
#[inline]
fn at(buf: &[Sample], i: isize) -> Sample {
    buf[i as usize]
}

/// `clamp_mv_to_umv_border_sb` (`vp9/common/vp9_reconinter.c:88`), returning
/// the MV in 1/16-pel (`MV_PRECISION_Q4`) units.
///
/// `mb_*` are the block's distances to the frame edges in 1/8-pel units
/// (`xd->mb_to_*_edge`, i.e. `±mi * 64`); `bw`/`bh` are the PLANE block's
/// pixel dimensions. `clamp_mv(mv, min_col, max_col, min_row, max_row)`:
/// columns are bounded by the left/right edges, rows by the top/bottom.
pub(crate) fn clamp_mv_to_umv_border_sb(
    mv: (i32, i32),
    bw: i32,
    bh: i32,
    mb_left: i32,
    mb_right: i32,
    mb_top: i32,
    mb_bottom: i32,
    ss_x: usize,
    ss_y: usize,
) -> (i32, i32) {
    let spel_left = (INTERP_EXTEND + bw) << SUBPEL_BITS;
    let spel_right = spel_left - SUBPEL_SHIFTS;
    let spel_top = (INTERP_EXTEND + bh) << SUBPEL_BITS;
    let spel_bottom = spel_top - SUBPEL_SHIFTS;
    let xs = 1 << (1 - ss_x);
    let ys = 1 << (1 - ss_y);
    let col = (mv.1 * xs).clamp(mb_left * xs - spel_left, mb_right * xs + spel_right);
    let row = (mv.0 * ys).clamp(mb_top * ys - spel_top, mb_bottom * ys + spel_bottom);
    (row, col)
}

/// `build_mc_border` (`vp9/decoder/vp9_decodeframe.c:473`): copy the
/// `b_w` x `b_h` window whose top-left is frame pixel `(x, y)` into `dst`,
/// replicating the frame's first/last row and column for reads outside
/// `[0, w)` x `[0, h)`.
#[allow(clippy::too_many_arguments)]
fn build_mc_border(
    src: &[Sample],
    src_stride: usize,
    dst: &mut [Sample],
    dst_stride: usize,
    x: i32,
    mut y: i32,
    b_w: usize,
    mut b_h: usize,
    w: i32,
    h: i32,
) {
    // C computes `ref_row = src - x - y * src_stride`; for negative y the
    // pointer arithmetic leaves the buffer and the row-index equivalent below
    // is the same read.
    let mut row = if y >= h {
        (h - 1) as usize
    } else if y > 0 {
        y as usize
    } else {
        0
    };
    let mut out = 0usize;
    loop {
        let left = if x < 0 { (-x).min(b_w as i32) } else { 0 } as usize;
        let right = if x + b_w as i32 > w {
            (x + b_w as i32 - w).min(b_w as i32)
        } else {
            0
        } as usize;
        let copy = b_w - left - right;

        let base = row * src_stride;
        if left > 0 {
            let v = src[base];
            dst[out..out + left].fill(v);
        }
        if copy > 0 {
            let from = (x + left as i32) as usize;
            dst[out + left..out + left + copy]
                .copy_from_slice(&src[base + from..base + from + copy]);
        }
        if right > 0 {
            let v = src[base + (w - 1) as usize];
            dst[out + left + copy..out + left + copy + right].fill(v);
        }

        out += dst_stride;
        y += 1;
        if y > 0 && y < h {
            row += 1;
        }
        b_h -= 1;
        if b_h == 0 {
            break;
        }
    }
}

/// `convolve_horiz` / `convolve_avg_horiz` (`vpx_dsp/vpx_convolve.c:22`).
/// `origin` is the block's first sample in `buf`; taps are read from
/// `origin + y*stride + (x_q4 >> SUBPEL_BITS) - (SUBPEL_TAPS / 2 - 1) + k`.
#[allow(clippy::too_many_arguments)]
fn convolve_horiz(
    buf: &[Sample],
    stride: usize,
    origin: isize,
    dst: &mut [Sample],
    dst_stride: usize,
    f: &[[i16; TAPS]; SUBPEL_SHIFTS as usize],
    x0_q4: i32,
    w: usize,
    rows: usize,
    avg: bool,
    bd: u8,
) {
    for y in 0..rows {
        let mut x_q4 = x0_q4;
        for x in 0..w {
            let base = origin + y as isize * stride as isize + (x_q4 >> SUBPEL_BITS) as isize
                - (TAPS as isize / 2 - 1);
            let filt = &f[(x_q4 & SUBPEL_MASK) as usize];
            let mut sum = 0i32;
            for k in 0..TAPS {
                sum += at(buf, base + k as isize) as i32 * filt[k] as i32;
            }
            let v = clip_pixel_bd(round_pow2(sum, FILTER_BITS), bd) as i32;
            let d = y * dst_stride + x;
            dst[d] = if avg {
                round_pow2(dst[d] as i32 + v, 1) as Sample
            } else {
                v as Sample
            };
            x_q4 += SUBPEL_SHIFTS;
        }
    }
}

/// `convolve_vert` / `convolve_avg_vert` (`vpx_dsp/vpx_convolve.c:80`).
fn convolve_vert(
    buf: &[Sample],
    stride: usize,
    origin: isize,
    dst: &mut [Sample],
    dst_stride: usize,
    f: &[[i16; TAPS]; SUBPEL_SHIFTS as usize],
    y0_q4: i32,
    w: usize,
    h: usize,
    avg: bool,
    bd: u8,
) {
    for x in 0..w {
        let mut y_q4 = y0_q4;
        for y in 0..h {
            let base = origin
                + ((y_q4 >> SUBPEL_BITS) as isize - (TAPS as isize / 2 - 1)) * stride as isize
                + x as isize;
            let filt = &f[(y_q4 & SUBPEL_MASK) as usize];
            let mut sum = 0i32;
            for k in 0..TAPS {
                sum += at(buf, base + k as isize * stride as isize) as i32 * filt[k] as i32;
            }
            let v = clip_pixel_bd(round_pow2(sum, FILTER_BITS), bd) as i32;
            let d = y * dst_stride + x;
            dst[d] = if avg {
                round_pow2(dst[d] as i32 + v, 1) as Sample
            } else {
                v as Sample
            };
            y_q4 += SUBPEL_SHIFTS;
        }
    }
}

/// `inter_predictor` + `sf->predict` (`vp9/common/vp9_reconinter.h:23`,
/// `vp9/common/vp9_scale.c:79`): for the unscaled case `predict[0][0]` is
/// copy/avg, `predict[0][1][*]` the vertical kernel, `predict[1][0][*]` the
/// horizontal one and `predict[1][1][*]` the 2D convolve.
///
/// `w` x `h` output samples written at `dst`; `buf`/`origin` locate the
/// block's first reference sample. `temp` is the `vpx_convolve8` intermediate
/// (stride-64 scratch of `64 * 135` bytes plus a `64 * 64` second scratch for
/// the averaging variant).
#[allow(clippy::too_many_arguments)]
fn inter_predictor(
    buf: &[Sample],
    stride: usize,
    origin: isize,
    dst: &mut [Sample],
    dst_stride: usize,
    f: &[[i16; TAPS]; SUBPEL_SHIFTS as usize],
    subpel_x: i32,
    subpel_y: i32,
    w: usize,
    h: usize,
    avg: bool,
    temp: &mut [Sample],
    bd: u8,
) {
    if subpel_x == 0 && subpel_y == 0 {
        for y in 0..h {
            for x in 0..w {
                let s = at(buf, origin + (y * stride + x) as isize) as i32;
                let d = y * dst_stride + x;
                dst[d] = if avg {
                    round_pow2(dst[d] as i32 + s, 1) as Sample
                } else {
                    s as Sample
                };
            }
        }
        return;
    }
    if subpel_y == 0 {
        convolve_horiz(
            buf, stride, origin, dst, dst_stride, f, subpel_x, w, h, avg, bd,
        );
        return;
    }
    if subpel_x == 0 {
        convolve_vert(
            buf, stride, origin, dst, dst_stride, f, subpel_y, w, h, avg, bd,
        );
        return;
    }
    // vpx_convolve8_c: horizontal pass into a stride-64 intermediate with
    // `intermediate_height = (((h - 1) * y_step_q4 + y0_q4) >> SUBPEL_BITS) +
    // SUBPEL_TAPS` — for the unscaled `y_step_q4 = 16` that is `h + SUBPEL_TAPS
    // - 1`. The horizontal pass starts `SUBPEL_TAPS / 2 - 1` rows ABOVE the
    // block (C passes `src - src_stride * (SUBPEL_TAPS / 2 - 1)`).
    let intermediate_height =
        (((h as i32 - 1) * SUBPEL_SHIFTS + subpel_y) >> SUBPEL_BITS) as usize + TAPS;
    let temp_origin = (TAPS as isize / 2 - 1) * 64;
    convolve_horiz(
        buf,
        stride,
        origin - (TAPS as isize / 2 - 1) * stride as isize,
        temp,
        64,
        f,
        subpel_x,
        w,
        intermediate_height,
        false,
        bd,
    );
    if avg {
        // vpx_convolve8_avg_c: full 2D into a second scratch, then average
        // with the destination (which holds the first reference's block).
        // `temp[..64 * 135]` is the horizontal intermediate; the region after
        // it is the 2D result.
        let (horiz, mid2) = temp.split_at_mut(64 * 135);
        convolve_vert(
            horiz,
            64,
            temp_origin,
            mid2,
            w,
            f,
            subpel_y,
            w,
            h,
            false,
            bd,
        );
        for y in 0..h {
            for x in 0..w {
                let d = y * dst_stride + x;
                dst[d] = round_pow2(dst[d] as i32 + mid2[y * w + x] as i32, 1) as Sample;
            }
        }
        return;
    }
    convolve_vert(
        temp,
        64,
        temp_origin,
        dst,
        dst_stride,
        f,
        subpel_y,
        w,
        h,
        false,
        bd,
    );
}

/// One reference plane of a stored frame.
pub(crate) struct RefPlane<'a> {
    /// Plane base — the frame's first visible sample.
    pub data: &'a [Sample],
    /// Row pitch.
    pub stride: usize,
    /// `y_crop_width` / `uv_crop_width`: the coded extent, which is what
    /// `build_mc_border` replicates past.
    pub width: usize,
    /// `y_crop_height` / `uv_crop_height`.
    pub height: usize,
    /// Bits per sample of the reference frame.
    pub bd: u8,
}

/// `dec_build_inter_predictors` (`vp9/decoder/vp9_decodeframe.c:593`),
/// unscaled: predict one `w` x `h` plane block whose top-left is plane pixel
/// `(x, y)` inside the block at MI `(mi_row, mi_col)`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_inter_predictors(
    dst: &mut [Sample],
    dst_stride: usize,
    refp: &RefPlane<'_>,
    mi_col: usize,
    mi_row: usize,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    mv: (i32, i32),
    mb_left: i32,
    mb_right: i32,
    mb_top: i32,
    mb_bottom: i32,
    ss_x: usize,
    ss_y: usize,
    kernel: usize,
    avg: bool,
    scratch: &mut Vec<Sample>,
    temp: &mut Vec<Sample>,
) {
    let bd = refp.bd;
    let mv_q4 = clamp_mv_to_umv_border_sb(
        mv, w as i32, h as i32, mb_left, mb_right, mb_top, mb_bottom, ss_x, ss_y,
    );

    // Co-ordinate of the containing block, pixel precision, plus the
    // block-relative offset (`x`, `y`).
    let x0_pre = (-mb_left >> (3 + ss_x)) + x as i32;
    let y0_pre = (-mb_top >> (3 + ss_y)) + y as i32;
    let x0_16 = (x0_pre << SUBPEL_BITS) + mv_q4.1;
    let y0_16 = (y0_pre << SUBPEL_BITS) + mv_q4.0;
    let subpel_x = mv_q4.1 & SUBPEL_MASK;
    let subpel_y = mv_q4.0 & SUBPEL_MASK;

    let frame_width = refp.width as i32;
    let frame_height = refp.height as i32;
    // `buf_ptr = ref_frame + y0 * stride + x0` is taken AFTER the MV integer
    // part but BEFORE the tap padding below, so the direct read keeps its own
    // copy of the coordinates.
    let x0_mv = x0_pre + (mv_q4.1 >> SUBPEL_BITS);
    let y0_mv = y0_pre + (mv_q4.0 >> SUBPEL_BITS);
    let mut x0 = x0_mv;
    let mut y0 = y0_mv;

    let f = &FILTERS[kernel];
    let _ = mi_col;
    let _ = mi_row;

    // `is_scaled || scaled_mv.col || scaled_mv.row || (frame_width & 7) ||
    // (frame_height & 7)`: the border copy is skipped only when the window is
    // provably inside the frame.
    if mv_q4.1 != 0 || mv_q4.0 != 0 || (frame_width & 7) != 0 || (frame_height & 7) != 0 {
        let mut x1 = ((x0_16 + (w as i32 - 1) * SUBPEL_SHIFTS) >> SUBPEL_BITS) + 1;
        let mut y1 = ((y0_16 + (h as i32 - 1) * SUBPEL_SHIFTS) >> SUBPEL_BITS) + 1;
        let mut x_pad = false;
        let mut y_pad = false;
        if subpel_x != 0 {
            x0 -= INTERP_EXTEND - 1;
            x1 += INTERP_EXTEND;
            x_pad = true;
        }
        if subpel_y != 0 {
            y0 -= INTERP_EXTEND - 1;
            y1 += INTERP_EXTEND;
            y_pad = true;
        }
        if x0 < 0
            || x0 > frame_width - 1
            || x1 < 0
            || x1 > frame_width - 1
            || y0 < 0
            || y0 > frame_height - 1
            || y1 < 0
            || y1 > frame_height - 1
        {
            let b_w = (x1 - x0 + 1) as usize;
            let b_h = (y1 - y0 + 1) as usize;
            let border_offset = usize::from(y_pad) * 3 * b_w + usize::from(x_pad) * 3;
            scratch.resize(b_w * b_h, 0);
            build_mc_border(
                refp.data,
                refp.stride,
                scratch,
                b_w,
                x0,
                y0,
                b_w,
                b_h,
                frame_width,
                frame_height,
            );
            inter_predictor(
                scratch,
                b_w,
                border_offset as isize,
                dst,
                dst_stride,
                f,
                subpel_x,
                subpel_y,
                w,
                h,
                avg,
                temp,
                bd,
            );
            return;
        }
    }
    // Inside the frame: read directly. The `x0`/`y0` checks above guarantee
    // the `SUBPEL_TAPS / 2 - 1` tap margin that `inter_predictor` reads
    // backwards also lies inside the visible plane.
    let origin = y0_mv as isize * refp.stride as isize + x0_mv as isize;
    inter_predictor(
        refp.data,
        refp.stride,
        origin,
        dst,
        dst_stride,
        f,
        subpel_x,
        subpel_y,
        w,
        h,
        avg,
        temp,
        bd,
    );
}

/// `average_split_mvs` (`vp9/common/vp9_reconinter.c:113`), luma (`ss_idx 0`):
/// the sub-block's own MV.
pub(crate) fn split_mv(bmi: &[[(i32, i32); 2]; 4], block: usize, ref_idx: usize) -> (i32, i32) {
    bmi[block][ref_idx]
}

/// `average_split_mvs` for 4:2:0 chroma (`ss_idx 3`): `mi_mv_pred_q4`, the
/// q4-rounded average over the four sub-blocks.
///
/// `round_mv_comp_q4(v) = (v < 0 ? v - 2 : v + 2) / 4` — C's truncating
/// division, which Rust's `/` on integers reproduces.
pub(crate) fn average_split_mvs_chroma(bmi: &[[(i32, i32); 2]; 4], ref_idx: usize) -> (i32, i32) {
    let q4 = |v: i32| if v < 0 { (v - 2) / 4 } else { (v + 2) / 4 };
    let mut row = 0;
    let mut col = 0;
    for b in bmi.iter() {
        row += b[ref_idx].0;
        col += b[ref_idx].1;
    }
    (q4(row), q4(col))
}

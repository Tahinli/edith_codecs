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
//! dimensions are not a multiple of 8. A reference slot whose size differs
//! from the current frame is SCALED: [`ScaleFactors`] carries
//! `vp9_setup_scale_factors_for_frame`, the block/MV are mapped into the
//! reference with `vp9_scale_mv`, and the convolve steps by `x_step_q4` /
//! `y_step_q4` (`sf->predict`, `vpx_scaled_*`).
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

/// `REF_SCALE_SHIFT` / `REF_NO_SCALE` / `REF_INVALID_SCALE`
/// (`vp9/common/vp9_scale.h:21-23`): the fixed-point precision of a reference
/// frame's scale factor.
const REF_SCALE_SHIFT: i32 = 14;
const REF_NO_SCALE: i32 = 1 << REF_SCALE_SHIFT;
const REF_INVALID_SCALE: i32 = -1;

/// `scaled_x` / `scaled_y` (`vp9_scale.c:22`).
fn scale_value(val: i32, scale_fp: i32) -> i32 {
    (((val as i64) * scale_fp as i64) >> REF_SCALE_SHIFT) as i32
}

/// `struct scale_factors` (`vp9/common/vp9_scale.h:27`) set up by
/// `vp9_setup_scale_factors_for_frame` (`vp9_scale.c:44`): the scale from a
/// reference frame's coded size to this frame's, its derived 1/16-pel step
/// (`x_step_q4`/`y_step_q4`; 16 means "no scaling on that axis") and
/// `vp9_scale_mv`.
#[derive(Clone, Copy)]
pub(crate) struct ScaleFactors {
    x_scale_fp: i32,
    y_scale_fp: i32,
    x_step_q4: i32,
    y_step_q4: i32,
}

impl ScaleFactors {
    /// `vp9_setup_scale_factors_for_frame(other_w, other_h, this_w, this_h)`:
    /// `other_*` is the REFERENCE's coded size, `this_*` the frame being
    /// decoded. An out-of-range ratio leaves `REF_INVALID_SCALE`, which
    /// libvpx turns into `Reference frame has invalid dimensions` when the
    /// reference is actually predicted from.
    pub(crate) fn setup(other_w: usize, other_h: usize, this_w: usize, this_h: usize) -> Self {
        // `valid_ref_frame_size` (`vp9_scale.h:61`): at most 2x down, 16x up.
        let valid = 2 * this_w >= other_w
            && 2 * this_h >= other_h
            && this_w <= 16 * other_w
            && this_h <= 16 * other_h;
        if !valid {
            return ScaleFactors {
                x_scale_fp: REF_INVALID_SCALE,
                y_scale_fp: REF_INVALID_SCALE,
                x_step_q4: 0,
                y_step_q4: 0,
            };
        }
        let x_scale_fp = (((other_w as i64) << REF_SCALE_SHIFT) / this_w as i64) as i32;
        let y_scale_fp = (((other_h as i64) << REF_SCALE_SHIFT) / this_h as i64) as i32;
        ScaleFactors {
            x_scale_fp,
            y_scale_fp,
            x_step_q4: scale_value(SUBPEL_SHIFTS, x_scale_fp),
            y_step_q4: scale_value(SUBPEL_SHIFTS, y_scale_fp),
        }
    }

    /// `vp9_is_valid_scale`.
    pub(crate) fn is_valid(&self) -> bool {
        self.x_scale_fp != REF_INVALID_SCALE && self.y_scale_fp != REF_INVALID_SCALE
    }

    /// `vp9_is_scaled`.
    pub(crate) fn is_scaled(&self) -> bool {
        self.is_valid() && (self.x_scale_fp != REF_NO_SCALE || self.y_scale_fp != REF_NO_SCALE)
    }

    fn scale_x(&self, val: i32) -> i32 {
        scale_value(val, self.x_scale_fp)
    }

    fn scale_y(&self, val: i32) -> i32 {
        scale_value(val, self.y_scale_fp)
    }

    /// `vp9_scale_mv` (`vp9_scale.c:29`): the block MV scaled to the
    /// reference's resolution plus the sub-pixel offset of the block's own
    /// scaled position. Returns `(row, col)` in 1/16-pel units.
    ///
    /// `x`/`y` are the block's coordinates in the CURRENT frame (libvpx
    /// passes `mi_x + x`, `mi_y + y` — luma units even for chroma planes; the
    /// sub-8x8 chroma offsets are always 0, so the mismatch never bites).
    pub(crate) fn scale_mv(&self, mv: (i32, i32), x: i32, y: i32) -> (i32, i32) {
        let x_off_q4 = self.scale_x(x << SUBPEL_BITS) & SUBPEL_MASK;
        let y_off_q4 = self.scale_y(y << SUBPEL_BITS) & SUBPEL_MASK;
        (self.scale_y(mv.0) + y_off_q4, self.scale_x(mv.1) + x_off_q4)
    }
}

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

/// `clip_pixel` (`vpx_dsp/vpx_dsp_common.h`).
fn clip_pixel(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// `ROUND_POWER_OF_TWO(v, n)`.
fn round_pow2(v: i32, n: i32) -> i32 {
    (v + (1 << (n - 1))) >> n
}

/// Read `buf[i]` for a possibly negative `i`. Every caller keeps `i` inside
/// the buffer (the border copy guarantees the tap margins exist).
#[inline]
fn at(buf: &[u8], i: isize) -> u8 {
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
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
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
/// `x_step` advances `x_q4` per output sample — `SUBPEL_SHIFTS` for an
/// unscaled reference, `sf->x_step_q4` (the scaled sampler, also used by
/// `vpx_scaled_horiz`) otherwise.
#[allow(clippy::too_many_arguments)]
fn convolve_horiz(
    buf: &[u8],
    stride: usize,
    origin: isize,
    dst: &mut [u8],
    dst_stride: usize,
    f: &[[i16; TAPS]; SUBPEL_SHIFTS as usize],
    x0_q4: i32,
    x_step: i32,
    w: usize,
    rows: usize,
    avg: bool,
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
            let v = clip_pixel(round_pow2(sum, FILTER_BITS)) as i32;
            let d = y * dst_stride + x;
            dst[d] = if avg {
                round_pow2(dst[d] as i32 + v, 1) as u8
            } else {
                v as u8
            };
            x_q4 += x_step;
        }
    }
}

/// `convolve_vert` / `convolve_avg_vert` (`vpx_dsp/vpx_convolve.c:80`).
/// `y_step` advances `y_q4` per output sample (`SUBPEL_SHIFTS` unscaled,
/// `sf->y_step_q4` for `vpx_scaled_vert`).
#[allow(clippy::too_many_arguments)]
fn convolve_vert(
    buf: &[u8],
    stride: usize,
    origin: isize,
    dst: &mut [u8],
    dst_stride: usize,
    f: &[[i16; TAPS]; SUBPEL_SHIFTS as usize],
    y0_q4: i32,
    y_step: i32,
    w: usize,
    h: usize,
    avg: bool,
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
            let v = clip_pixel(round_pow2(sum, FILTER_BITS)) as i32;
            let d = y * dst_stride + x;
            dst[d] = if avg {
                round_pow2(dst[d] as i32 + v, 1) as u8
            } else {
                v as u8
            };
            y_q4 += y_step;
        }
    }
}

/// `inter_predictor` + `sf->predict` (`vp9/common/vp9_reconinter.h:23`,
/// `vp9/common/vp9_scale.c:79`): the kernel is picked by
/// `predict[subpel_x != 0][subpel_y != 0][avg]`. Unscaled that is
/// copy/avg, the vertical, the horizontal and the 2D convolve; with a scaled
/// reference the table collapses to the scaled variants (`vpx_scaled_horiz` /
/// `vpx_scaled_vert` — the same walk with `x_step_q4` / `y_step_q4` — and
/// `vpx_scaled_2d`), reproduced here by the step-aware convolve helpers.
///
/// `w` x `h` output samples written at `dst`; `buf`/`origin` locate the
/// block's first reference sample. `temp` is the `vpx_convolve8` intermediate
/// (stride-64 scratch of `64 * 135` bytes plus a `64 * 64` second scratch for
/// the averaging variant).
#[allow(clippy::too_many_arguments)]
fn inter_predictor(
    buf: &[u8],
    stride: usize,
    origin: isize,
    dst: &mut [u8],
    dst_stride: usize,
    f: &[[i16; TAPS]; SUBPEL_SHIFTS as usize],
    subpel_x: i32,
    subpel_y: i32,
    x_step: i32,
    y_step: i32,
    w: usize,
    h: usize,
    avg: bool,
    temp: &mut [u8],
) {
    let scaled_x = x_step != SUBPEL_SHIFTS;
    let scaled_y = y_step != SUBPEL_SHIFTS;
    if scaled_x || scaled_y {
        // sf->predict with a scaled axis (vp9_scale.c:79-110): an unscaled
        // axis keeps its 1D kernel only when the other axis has no subpel
        // offset; anything else is the 2D scaled convolve.
        if !scaled_x && subpel_x == 0 {
            convolve_vert(
                buf, stride, origin, dst, dst_stride, f, subpel_y, y_step, w, h, avg,
            );
            return;
        }
        if !scaled_y && subpel_y == 0 {
            convolve_horiz(
                buf, stride, origin, dst, dst_stride, f, subpel_x, x_step, w, h, avg,
            );
            return;
        }
        convolve_2d(
            buf, stride, origin, dst, dst_stride, f, subpel_x, subpel_y, x_step, y_step, w, h, avg,
            temp,
        );
        return;
    }
    if subpel_x == 0 && subpel_y == 0 {
        for y in 0..h {
            for x in 0..w {
                let s = at(buf, origin + (y * stride + x) as isize) as i32;
                let d = y * dst_stride + x;
                dst[d] = if avg {
                    round_pow2(dst[d] as i32 + s, 1) as u8
                } else {
                    s as u8
                };
            }
        }
        return;
    }
    if subpel_y == 0 {
        convolve_horiz(
            buf,
            stride,
            origin,
            dst,
            dst_stride,
            f,
            subpel_x,
            SUBPEL_SHIFTS,
            w,
            h,
            avg,
        );
        return;
    }
    if subpel_x == 0 {
        convolve_vert(
            buf,
            stride,
            origin,
            dst,
            dst_stride,
            f,
            subpel_y,
            SUBPEL_SHIFTS,
            w,
            h,
            avg,
        );
        return;
    }
    convolve_2d(
        buf, stride, origin, dst, dst_stride, f, subpel_x, subpel_y, x_step, y_step, w, h, avg,
        temp,
    );
}

/// `vpx_convolve8_c` / `vpx_convolve8_avg_c` (`vpx_dsp/vpx_convolve.c:158`) =
/// `vpx_scaled_2d` / `vpx_scaled_avg_2d`: horizontal pass into a stride-64
/// intermediate with `intermediate_height = (((h - 1) * y_step_q4 + y0_q4) >>
/// SUBPEL_BITS) + SUBPEL_TAPS`, then the vertical pass. The horizontal pass
/// starts `SUBPEL_TAPS / 2 - 1` rows ABOVE the block (C passes
/// `src - src_stride * (SUBPEL_TAPS / 2 - 1)`).
#[allow(clippy::too_many_arguments)]
fn convolve_2d(
    buf: &[u8],
    stride: usize,
    origin: isize,
    dst: &mut [u8],
    dst_stride: usize,
    f: &[[i16; TAPS]; SUBPEL_SHIFTS as usize],
    subpel_x: i32,
    subpel_y: i32,
    x_step: i32,
    y_step: i32,
    w: usize,
    h: usize,
    avg: bool,
    temp: &mut [u8],
) {
    let intermediate_height = (((h as i32 - 1) * y_step + subpel_y) >> SUBPEL_BITS) as usize + TAPS;
    let temp_origin = (TAPS as isize / 2 - 1) * 64;
    convolve_horiz(
        buf,
        stride,
        origin - (TAPS as isize / 2 - 1) * stride as isize,
        temp,
        64,
        f,
        subpel_x,
        x_step,
        w,
        intermediate_height,
        false,
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
            y_step,
            w,
            h,
            false,
        );
        for y in 0..h {
            for x in 0..w {
                let d = y * dst_stride + x;
                dst[d] = round_pow2(dst[d] as i32 + mid2[y * w + x] as i32, 1) as u8;
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
        y_step,
        w,
        h,
        false,
    );
}

/// One reference plane of a stored frame.
pub(crate) struct RefPlane<'a> {
    /// Plane base — the frame's first visible sample.
    pub data: &'a [u8],
    /// Row pitch.
    pub stride: usize,
    /// `y_crop_width` / `uv_crop_width`: the coded extent, which is what
    /// `build_mc_border` replicates past.
    pub width: usize,
    /// `y_crop_height` / `uv_crop_height`.
    pub height: usize,
}

/// `dec_build_inter_predictors` (`vp9/decoder/vp9_decodeframe.c:593`):
/// predict one `w` x `h` plane block whose top-left is plane pixel `(x, y)`
/// inside the block at MI `(mi_row, mi_col)`. `sf` is the reference's scale
/// factors; when they scale, the block is mapped into the reference frame
/// (`scale_value_x/y`), the MV is scaled with it (`vp9_scale_mv`) and the
/// convolve steps by `x_step_q4` / `y_step_q4`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_inter_predictors(
    dst: &mut [u8],
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
    sf: &ScaleFactors,
    scratch: &mut Vec<u8>,
    temp: &mut Vec<u8>,
) {
    let is_scaled = sf.is_scaled();
    let mv_q4 = clamp_mv_to_umv_border_sb(
        mv, w as i32, h as i32, mb_left, mb_right, mb_top, mb_bottom, ss_x, ss_y,
    );

    // Co-ordinate of the containing block, pixel precision, plus the
    // block-relative offset (`x`, `y`).
    let x_pre = (-mb_left >> (3 + ss_x)) + x as i32;
    let y_pre = (-mb_top >> (3 + ss_y)) + y as i32;
    let (x_step, y_step, scaled_mv);
    let (x0_16, y0_16, x0_pre, y0_pre);
    if is_scaled {
        // The block is mapped into the reference frame at 1/16-pel (the
        // `x0_16` used for the border extent) and at pixel precision (`x0`,
        // which shifts by the scaled MV's integer part below).
        let x0 = sf.scale_x(x_pre);
        let y0 = sf.scale_y(y_pre);
        scaled_mv = sf.scale_mv(
            mv_q4,
            mi_col as i32 * 8 + x as i32,
            mi_row as i32 * 8 + y as i32,
        );
        x_step = sf.x_step_q4;
        y_step = sf.y_step_q4;
        x0_16 = sf.scale_x(x_pre << SUBPEL_BITS) + scaled_mv.1;
        y0_16 = sf.scale_y(y_pre << SUBPEL_BITS) + scaled_mv.0;
        x0_pre = x0;
        y0_pre = y0;
    } else {
        scaled_mv = mv_q4;
        x_step = SUBPEL_SHIFTS;
        y_step = SUBPEL_SHIFTS;
        x0_16 = (x_pre << SUBPEL_BITS) + scaled_mv.1;
        y0_16 = (y_pre << SUBPEL_BITS) + scaled_mv.0;
        x0_pre = x_pre;
        y0_pre = y_pre;
    }
    let subpel_x = scaled_mv.1 & SUBPEL_MASK;
    let subpel_y = scaled_mv.0 & SUBPEL_MASK;

    let frame_width = refp.width as i32;
    let frame_height = refp.height as i32;
    // `buf_ptr = ref_frame + y0 * stride + x0` is taken AFTER the MV integer
    // part but BEFORE the tap padding below, so the direct read keeps its own
    // copy of the coordinates.
    let x0_mv = x0_pre + (scaled_mv.1 >> SUBPEL_BITS);
    let y0_mv = y0_pre + (scaled_mv.0 >> SUBPEL_BITS);
    let mut x0 = x0_mv;
    let mut y0 = y0_mv;

    let f = &FILTERS[kernel];

    // `is_scaled || scaled_mv.col || scaled_mv.row || (frame_width & 7) ||
    // (frame_height & 7)`: the border copy is skipped only when the window is
    // provably inside the frame.
    if is_scaled
        || scaled_mv.1 != 0
        || scaled_mv.0 != 0
        || (frame_width & 7) != 0
        || (frame_height & 7) != 0
    {
        let mut x1 = ((x0_16 + (w as i32 - 1) * x_step) >> SUBPEL_BITS) + 1;
        let mut y1 = ((y0_16 + (h as i32 - 1) * y_step) >> SUBPEL_BITS) + 1;
        let mut x_pad = false;
        let mut y_pad = false;
        if subpel_x != 0 || x_step != SUBPEL_SHIFTS {
            x0 -= INTERP_EXTEND - 1;
            x1 += INTERP_EXTEND;
            x_pad = true;
        }
        if subpel_y != 0 || y_step != SUBPEL_SHIFTS {
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
                x_step,
                y_step,
                w,
                h,
                avg,
                temp,
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
        x_step,
        y_step,
        w,
        h,
        avg,
        temp,
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

//! Local warp (`WARPED_CAUSAL`) motion prediction, spec 7.11.3.6-.8 /
//! `av1/common/warped_motion.c`: estimate a 2x3 affine model from up to
//! `LEAST_SQUARES_SAMPLES_MAX` neighbour-block motion samples
//! ([`find_projection`]), then filter the reference through it with an 8x8
//! tiled, two-pass 8-tap warped filter ([`warp_affine`]). 8-bit,
//! non-compound only (this decoder's warp arm only ever reaches a
//! single-ref, non-skip-mode block, spec `is_motion_variation_allowed_bsize`
//! -- `conv_params->is_compound` is always false on that path, so libaom's
//! compound/highbd branches are dropped).

/// `WARPEDMODEL_PREC_BITS` (`av1/common/mv.h`).
const WARPEDMODEL_PREC_BITS: u32 = 16;
/// `WARPEDMODEL_TRANS_CLAMP`.
const WARPEDMODEL_TRANS_CLAMP: i64 = 128 << WARPEDMODEL_PREC_BITS;
/// `WARPEDMODEL_NONDIAGAFFINE_CLAMP`.
const WARPEDMODEL_NONDIAGAFFINE_CLAMP: i64 = 1 << (WARPEDMODEL_PREC_BITS - 3);
/// `WARPEDPIXEL_PREC_BITS`.
const WARPEDPIXEL_PREC_BITS: u32 = 6;
/// `WARPEDPIXEL_PREC_SHIFTS`.
const WARPEDPIXEL_PREC_SHIFTS: i32 = 1 << WARPEDPIXEL_PREC_BITS;
/// `WARPEDDIFF_PREC_BITS`.
const WARPEDDIFF_PREC_BITS: u32 = WARPEDMODEL_PREC_BITS - WARPEDPIXEL_PREC_BITS;
/// `WARP_PARAM_REDUCE_BITS`.
const WARP_PARAM_REDUCE_BITS: u32 = 6;
/// `MI_SIZE` (4x4 luma pixels).
const MI_SIZE: i32 = 4;
/// `FILTER_BITS`.
const FILTER_BITS: i32 = 7;
/// `LEAST_SQUARES_SAMPLES_MAX`.
pub const LEAST_SQUARES_SAMPLES_MAX: usize = 8;
/// `LS_MV_MAX` (max mv in 1/8-pel, `find_affine_int`'s own sample gate).
const LS_MV_MAX: i32 = 256;
/// `LS_STEP`.
const LS_STEP: i32 = 8;

/// `Round2Signed` (spec 4.7): halves round away from zero's own sign.
fn round2_signed(value: i64, n: u32) -> i64 {
    if value < 0 {
        -round2(-value, n)
    } else {
        round2(value, n)
    }
}

fn round2(value: i64, n: u32) -> i64 {
    (value + (1i64 << n >> 1)) >> n
}

/// `get_msb` (`aom_ports/bitops.h`): index of the highest set bit.
fn get_msb64(n: u64) -> i32 {
    debug_assert!(n != 0);
    63 - n.leading_zeros() as i32
}

/// `resolve_divisor_64`: decomposes `d` such that `1/d = mult / 2^shift`.
fn resolve_divisor_64(d: u64) -> (i64, i32) {
    const DIV_LUT_PREC_BITS: i32 = 14;
    const DIV_LUT_BITS: i32 = 8;
    let mut shift = get_msb64(d);
    let e = d as i64 - (1i64 << shift);
    let f = if shift > DIV_LUT_BITS {
        round2(e, (shift - DIV_LUT_BITS) as u32)
    } else {
        e << (DIV_LUT_BITS - shift)
    };
    debug_assert!((0..=256).contains(&f));
    shift += DIV_LUT_PREC_BITS;
    (DIV_LUT[f as usize] as i64, shift)
}

/// `resolve_divisor_32`, same decomposition for a 32-bit divisor (this port
/// only ever needs the 64-bit form, `av1_get_shear_params`'s `mat[2]` is
/// magnitude-bounded well under 32 bits too, so reuses [`resolve_divisor_64`]
/// -- bit-identical for any value that fits both).
fn resolve_divisor_32(d: u32) -> (i64, i32) {
    resolve_divisor_64(d as u64)
}

fn clamp(v: i64, lo: i64, hi: i64) -> i64 {
    v.clamp(lo, hi)
}

/// One recorded least-squares sample: `(pts1, pts2)`, each `(x, y)` in
/// 1/8-pel, relative to the current block's own top-left (spec
/// `record_samples`).
#[derive(Clone, Copy)]
pub struct Sample {
    pub pts1: (i32, i32),
    pub pts2: (i32, i32),
}

/// `record_samples` (`mvref_common.c`): one neighbour's own sample, `bw`/`bh`
/// its block size in luma pixels, `(nb_mv_col, nb_mv_row)` its own motion
/// vector (1/8-pel).
#[allow(clippy::too_many_arguments)]
pub fn record_sample(
    bw: i32,
    bh: i32,
    nb_mv: (i32, i32),
    row_offset: i32,
    sign_r: i32,
    col_offset: i32,
    sign_c: i32,
) -> Sample {
    let x = col_offset * MI_SIZE + sign_c * bw / 2 - 1;
    let y = row_offset * MI_SIZE + sign_r * bh / 2 - 1;
    let pts1 = (x * 8, y * 8);
    let pts2 = (pts1.0 + nb_mv.1, pts1.1 + nb_mv.0);
    Sample { pts1, pts2 }
}

/// The resolved affine warp model, spec 7.11.3.6's `LocalWarpParams` --
/// `wmmat[2..6]` (the 2x2 linear part) plus the shear decomposition
/// `av1_get_shear_params` derives from it. `wmmat[0]`/`[1]` are the
/// translation terms.
#[derive(Clone, Copy, Debug)]
pub struct WarpParams {
    pub wmmat: [i32; 6],
    pub alpha: i16,
    pub beta: i16,
    pub gamma: i16,
    pub delta: i16,
}

/// `LS_SQUARE`/`LS_PRODUCT1`/`LS_PRODUCT2` (`warped_motion.c`): the
/// downshifted quadratic terms `find_affine_int`'s normal-equation matrix
/// accumulates, exploiting `LS_STEP == 8` (bottom 2 bits of every term are 0).
fn ls_square(a: i64) -> i64 {
    (a * a * 4 + a * 4 * LS_STEP as i64 + (LS_STEP * LS_STEP * 2) as i64) >> 4
}
fn ls_product1(a: i64, b: i64) -> i64 {
    (a * b * 4 + (a + b) * 2 * LS_STEP as i64 + (LS_STEP * LS_STEP) as i64) >> 4
}
fn ls_product2(a: i64, b: i64) -> i64 {
    (a * b * 4 + (a + b) * 2 * LS_STEP as i64 + (LS_STEP * LS_STEP * 2) as i64) >> 4
}

fn get_mult_shift_ndiag(px: i64, i_det: i64, shift: i32) -> i32 {
    let v = px * i_det;
    clamp(
        round2_signed(v, shift as u32),
        -WARPEDMODEL_NONDIAGAFFINE_CLAMP + 1,
        WARPEDMODEL_NONDIAGAFFINE_CLAMP - 1,
    ) as i32
}

fn get_mult_shift_diag(px: i64, i_det: i64, shift: i32) -> i32 {
    let v = px * i_det;
    clamp(
        round2_signed(v, shift as u32),
        (1i64 << WARPEDMODEL_PREC_BITS) - WARPEDMODEL_NONDIAGAFFINE_CLAMP + 1,
        (1i64 << WARPEDMODEL_PREC_BITS) + WARPEDMODEL_NONDIAGAFFINE_CLAMP - 1,
    ) as i32
}

/// `find_affine_int` (`warped_motion.c`): the least-squares affine fit over
/// `samples`, `bw`/`bh` the current block's own size in luma pixels, `(mvx,
/// mvy)` its own coded motion vector (1/8-pel, spec's own `mvx`/`mvy` order:
/// `col`, `row`), `mi_row`/`mi_col` its own `mi` position. Returns `None`
/// when the accumulated matrix is singular (`Det == 0`, spec's "found = 0"
/// path -- caller still falls back to the block's own translational MV,
/// matching libaom's own shear-validity fallback).
fn find_affine_int(
    samples: &[Sample],
    bw: i32,
    bh: i32,
    mvx: i32,
    mvy: i32,
    mi_row: i32,
    mi_col: i32,
) -> Option<[i32; 6]> {
    let mut a = [[0i64; 2]; 2];
    let mut bx = [0i64; 2];
    let mut by = [0i64; 2];

    let rsuy = bh / 2 - 1;
    let rsux = bw / 2 - 1;
    let suy = rsuy * 8;
    let sux = rsux * 8;
    let duy = suy + mvy;
    let dux = sux + mvx;

    for s in samples {
        let dx = s.pts2.0 - dux;
        let dy = s.pts2.1 - duy;
        let sx = s.pts1.0 - sux;
        let sy = s.pts1.1 - suy;
        if (sx - dx).abs() < LS_MV_MAX && (sy - dy).abs() < LS_MV_MAX {
            let (sx, sy, dx, dy) = (sx as i64, sy as i64, dx as i64, dy as i64);
            a[0][0] += ls_square(sx);
            a[0][1] += ls_product1(sx, sy);
            a[1][1] += ls_square(sy);
            bx[0] += ls_product2(sx, dx);
            bx[1] += ls_product1(sy, dx);
            by[0] += ls_product1(sx, dy);
            by[1] += ls_product2(sy, dy);
        }
    }

    let det = a[0][0] * a[1][1] - a[0][1] * a[0][1];
    if det == 0 {
        return None;
    }

    let (mut i_det, mut shift) = resolve_divisor_64(det.unsigned_abs());
    if det < 0 {
        i_det = -i_det;
    }
    shift -= WARPEDMODEL_PREC_BITS as i32;
    if shift < 0 {
        i_det <<= -shift;
        shift = 0;
    }

    let px0 = a[1][1] * bx[0] - a[0][1] * bx[1];
    let px1 = -a[0][1] * bx[0] + a[0][0] * bx[1];
    let py0 = a[1][1] * by[0] - a[0][1] * by[1];
    let py1 = -a[0][1] * by[0] + a[0][0] * by[1];

    let mut wmmat = [0i32; 6];
    wmmat[2] = get_mult_shift_diag(px0, i_det, shift);
    wmmat[3] = get_mult_shift_ndiag(px1, i_det, shift);
    wmmat[4] = get_mult_shift_ndiag(py0, i_det, shift);
    wmmat[5] = get_mult_shift_diag(py1, i_det, shift);

    let isuy = (mi_row * MI_SIZE + rsuy) as i64;
    let isux = (mi_col * MI_SIZE + rsux) as i64;
    let vx = mvx as i64 * (1i64 << (WARPEDMODEL_PREC_BITS - 3))
        - (isux * (wmmat[2] as i64 - (1i64 << WARPEDMODEL_PREC_BITS)) + isuy * wmmat[3] as i64);
    let vy = mvy as i64 * (1i64 << (WARPEDMODEL_PREC_BITS - 3))
        - (isux * wmmat[4] as i64 + isuy * (wmmat[5] as i64 - (1i64 << WARPEDMODEL_PREC_BITS)));
    wmmat[0] = clamp(vx, -WARPEDMODEL_TRANS_CLAMP, WARPEDMODEL_TRANS_CLAMP - 1) as i32;
    wmmat[1] = clamp(vy, -WARPEDMODEL_TRANS_CLAMP, WARPEDMODEL_TRANS_CLAMP - 1) as i32;
    Some(wmmat)
}

/// `av1_get_shear_params`: `None` on an invalid model (`mat[2] <= 0`, or the
/// reduced shear terms fail `is_affine_shear_allowed` -- spec's own
/// "warp filter cannot represent this" fallback).
fn get_shear_params(wmmat: [i32; 6]) -> Option<(i16, i16, i16, i16)> {
    if wmmat[2] <= 0 {
        return None;
    }
    let mat = wmmat.map(|v| v as i64);
    let mut alpha = clamp(mat[2] - (1 << WARPEDMODEL_PREC_BITS), i16::MIN as i64, i16::MAX as i64);
    let beta = clamp(mat[3], i16::MIN as i64, i16::MAX as i64);
    let (y_mag, shift) = resolve_divisor_32(mat[2].unsigned_abs() as u32);
    let y = if mat[2] < 0 { -y_mag } else { y_mag };
    let v = (mat[4] * (1 << WARPEDMODEL_PREC_BITS)) * y;
    let mut gamma = clamp(round2_signed(v, shift as u32), i16::MIN as i64, i16::MAX as i64);
    let v = (mat[3] * mat[4]) * y;
    let mut delta = clamp(
        mat[5] - round2_signed(v, shift as u32) - (1 << WARPEDMODEL_PREC_BITS),
        i16::MIN as i64,
        i16::MAX as i64,
    );

    alpha = round2_signed(alpha, WARP_PARAM_REDUCE_BITS) << WARP_PARAM_REDUCE_BITS;
    let beta = round2_signed(beta, WARP_PARAM_REDUCE_BITS) << WARP_PARAM_REDUCE_BITS;
    gamma = round2_signed(gamma, WARP_PARAM_REDUCE_BITS) << WARP_PARAM_REDUCE_BITS;
    delta = round2_signed(delta, WARP_PARAM_REDUCE_BITS) << WARP_PARAM_REDUCE_BITS;

    if (4 * alpha.abs() + 7 * beta.abs() >= (1 << WARPEDMODEL_PREC_BITS))
        || (4 * gamma.abs() + 4 * delta.abs() >= (1 << WARPEDMODEL_PREC_BITS))
    {
        return None;
    }
    Some((alpha as i16, beta as i16, gamma as i16, delta as i16))
}

/// `av1_selectSamples` (`mvref_common.c`): keeps only the samples whose own
/// MV agrees with the block's coded `mv` (`(row, col)`) within
/// `clamp(max(bw, bh), 16, 112)` (1/8-pel), run only when more than one
/// sample was found (`mbmi->num_proj_ref > 1` gate, `decodemv.c`). Always
/// keeps at least one (the C code's own `AOMMAX(ret, 1)` floor -- this port
/// mirrors it by never truncating below index 1 when the filter would
/// otherwise empty the vec).
pub fn select_samples(mv: (i32, i32), samples: &mut Vec<Sample>, bw: i32, bh: i32) {
    let thresh = bw.max(bh).clamp(16, 112);
    let mut kept: Vec<Sample> = samples
        .iter()
        .copied()
        .filter(|s| {
            let dx = s.pts2.0 - s.pts1.0 - mv.1;
            let dy = s.pts2.1 - s.pts1.1 - mv.0;
            dx.abs() + dy.abs() <= thresh
        })
        .collect();
    if kept.is_empty() {
        kept.push(samples[0]);
    }
    *samples = kept;
}

/// `av1_find_projection`: estimates the affine model, `None` when either the
/// least-squares fit is singular or the resulting shear is not
/// filter-representable -- both spec fallback cases collapse to the same
/// "use the block's own translational MV instead" caller behaviour.
pub fn find_projection(
    samples: &[Sample],
    bw: i32,
    bh: i32,
    mvx: i32,
    mvy: i32,
    mi_row: i32,
    mi_col: i32,
) -> Option<WarpParams> {
    let wmmat = find_affine_int(samples, bw, bh, mvx, mvy, mi_row, mi_col)?;
    let (alpha, beta, gamma, delta) = get_shear_params(wmmat)?;
    Some(WarpParams {
        wmmat,
        alpha,
        beta,
        gamma,
        delta,
    })
}

fn clip_pixel(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// `av1_warp_affine_c` (`warped_motion.c`), 8-bit, non-compound only:
/// filters `reference` (row-major, `stride`, true extent `width`x`height`)
/// through `params`' affine model into `dst` (row-major, `p_stride`), an
/// `p_width`x`p_height` block whose top-left sits at `(p_col, p_row)` in the
/// (possibly chroma-subsampled) plane `dst` belongs to.
#[allow(clippy::too_many_arguments)]
pub fn warp_affine(
    params: &WarpParams,
    reference: &[u8],
    width: i32,
    height: i32,
    stride: i32,
    dst: &mut [u8],
    p_col: i32,
    p_row: i32,
    p_width: i32,
    p_height: i32,
    p_stride: i32,
    subsampling_x: i32,
    subsampling_y: i32,
) {
    let mat = &params.wmmat;
    let (alpha, beta, gamma, delta) = (
        params.alpha as i64,
        params.beta as i64,
        params.gamma as i64,
        params.delta as i64,
    );
    const BD: i32 = 8;
    let reduce_bits_horiz = 3; // conv_params->round_0, non-compound 8-bit (InterRound0)
    let reduce_bits_vert = 2 * FILTER_BITS - reduce_bits_horiz; // InterRound1
    let offset_bits_horiz = BD + FILTER_BITS - 1;
    let offset_bits_vert = BD + 2 * FILTER_BITS - reduce_bits_horiz;

    let mut i = p_row;
    while i < p_row + p_height {
        let mut j = p_col;
        while j < p_col + p_width {
            let src_x = ((j + 4) << subsampling_x) as i64;
            let src_y = ((i + 4) << subsampling_y) as i64;
            let dst_x = mat[2] as i64 * src_x + mat[3] as i64 * src_y + mat[0] as i64;
            let dst_y = mat[4] as i64 * src_x + mat[5] as i64 * src_y + mat[1] as i64;
            let x4 = dst_x >> subsampling_x;
            let y4 = dst_y >> subsampling_y;

            let ix4 = (x4 >> WARPEDMODEL_PREC_BITS) as i32;
            let mut sx4 = (x4 & ((1 << WARPEDMODEL_PREC_BITS) - 1)) as i64;
            let iy4 = (y4 >> WARPEDMODEL_PREC_BITS) as i32;
            let mut sy4 = (y4 & ((1 << WARPEDMODEL_PREC_BITS) - 1)) as i64;

            sx4 += alpha * -4 + beta * -4;
            sy4 += gamma * -4 + delta * -4;
            sx4 &= !((1i64 << WARP_PARAM_REDUCE_BITS) - 1);
            sy4 &= !((1i64 << WARP_PARAM_REDUCE_BITS) - 1);

            let mut tmp = [[0i32; 8]; 15];
            for k in -7..8i32 {
                let iy = (iy4 + k).clamp(0, height - 1);
                let mut sx = sx4 + beta * (k as i64 + 4);
                for l in -4..4i32 {
                    let ix = ix4 + l - 3;
                    let offs =
                        (round2(sx, WARPEDDIFF_PREC_BITS) as i32 + WARPEDPIXEL_PREC_SHIFTS) as usize;
                    let coeffs = &AV1_WARPED_FILTER[offs];
                    let mut sum: i32 = 1 << offset_bits_horiz;
                    for (m, &c) in coeffs.iter().enumerate() {
                        let sample_x = (ix + m as i32).clamp(0, width - 1);
                        sum += i32::from(reference[(iy * stride + sample_x) as usize]) * c as i32;
                    }
                    sum = round2(sum as i64, reduce_bits_horiz as u32) as i32;
                    tmp[(k + 7) as usize][(l + 4) as usize] = sum;
                    sx += alpha;
                }
            }

            let k_hi = (4i32).min(p_row + p_height - i - 4);
            let mut k = -4i32;
            while k < k_hi {
                let mut sy = sy4 + delta * (k as i64 + 4);
                let l_hi = (4i32).min(p_col + p_width - j - 4);
                let mut l = -4i32;
                while l < l_hi {
                    let offs =
                        (round2(sy, WARPEDDIFF_PREC_BITS) as i32 + WARPEDPIXEL_PREC_SHIFTS) as usize;
                    let coeffs = &AV1_WARPED_FILTER[offs];
                    let mut sum: i32 = 1 << offset_bits_vert;
                    for m in 0..8i32 {
                        sum += tmp[(k + m + 4) as usize][(l + 4) as usize] * coeffs[m as usize] as i32;
                    }
                    sum = round2(sum as i64, reduce_bits_vert as u32) as i32;
                    let out_row = (i - p_row + k + 4) as usize;
                    let out_col = (j - p_col + l + 4) as usize;
                    dst[out_row * p_stride as usize + out_col] =
                        clip_pixel(sum - (1 << (BD - 1)) - (1 << BD));
                    sy += gamma;
                    l += 1;
                }
                k += 1;
            }
            j += 8;
        }
        i += 8;
    }
}

/// `av1_warped_filter` (`warped_motion.c`): 193 rows (`WARPEDPIXEL_PREC_SHIFTS
/// * 3 + 1`), one per 1/64-pel fraction across `[-1, 2)`, each an 8-tap
/// kernel (row 192 is a dummy replicate of row 191, transcribed verbatim).
#[rustfmt::skip]
const AV1_WARPED_FILTER: [[i16; 8]; 193] = [
    [0, 0, 127, 1, 0, 0, 0, 0],
    [0, -1, 127, 2, 0, 0, 0, 0],
    [1, -3, 127, 4, -1, 0, 0, 0],
    [1, -4, 126, 6, -2, 1, 0, 0],
    [1, -5, 126, 8, -3, 1, 0, 0],
    [1, -6, 125, 11, -4, 1, 0, 0],
    [1, -7, 124, 13, -4, 1, 0, 0],
    [2, -8, 123, 15, -5, 1, 0, 0],
    [2, -9, 122, 18, -6, 1, 0, 0],
    [2, -10, 121, 20, -6, 1, 0, 0],
    [2, -11, 120, 22, -7, 2, 0, 0],
    [2, -12, 119, 25, -8, 2, 0, 0],
    [3, -13, 117, 27, -8, 2, 0, 0],
    [3, -13, 116, 29, -9, 2, 0, 0],
    [3, -14, 114, 32, -10, 3, 0, 0],
    [3, -15, 113, 35, -10, 2, 0, 0],
    [3, -15, 111, 37, -11, 3, 0, 0],
    [3, -16, 109, 40, -11, 3, 0, 0],
    [3, -16, 108, 42, -12, 3, 0, 0],
    [4, -17, 106, 45, -13, 3, 0, 0],
    [4, -17, 104, 47, -13, 3, 0, 0],
    [4, -17, 102, 50, -14, 3, 0, 0],
    [4, -17, 100, 52, -14, 3, 0, 0],
    [4, -18, 98, 55, -15, 4, 0, 0],
    [4, -18, 96, 58, -15, 3, 0, 0],
    [4, -18, 94, 60, -16, 4, 0, 0],
    [4, -18, 91, 63, -16, 4, 0, 0],
    [4, -18, 89, 65, -16, 4, 0, 0],
    [4, -18, 87, 68, -17, 4, 0, 0],
    [4, -18, 85, 70, -17, 4, 0, 0],
    [4, -18, 82, 73, -17, 4, 0, 0],
    [4, -18, 80, 75, -17, 4, 0, 0],
    [4, -18, 78, 78, -18, 4, 0, 0],
    [4, -17, 75, 80, -18, 4, 0, 0],
    [4, -17, 73, 82, -18, 4, 0, 0],
    [4, -17, 70, 85, -18, 4, 0, 0],
    [4, -17, 68, 87, -18, 4, 0, 0],
    [4, -16, 65, 89, -18, 4, 0, 0],
    [4, -16, 63, 91, -18, 4, 0, 0],
    [4, -16, 60, 94, -18, 4, 0, 0],
    [3, -15, 58, 96, -18, 4, 0, 0],
    [4, -15, 55, 98, -18, 4, 0, 0],
    [3, -14, 52, 100, -17, 4, 0, 0],
    [3, -14, 50, 102, -17, 4, 0, 0],
    [3, -13, 47, 104, -17, 4, 0, 0],
    [3, -13, 45, 106, -17, 4, 0, 0],
    [3, -12, 42, 108, -16, 3, 0, 0],
    [3, -11, 40, 109, -16, 3, 0, 0],
    [3, -11, 37, 111, -15, 3, 0, 0],
    [2, -10, 35, 113, -15, 3, 0, 0],
    [3, -10, 32, 114, -14, 3, 0, 0],
    [2, -9, 29, 116, -13, 3, 0, 0],
    [2, -8, 27, 117, -13, 3, 0, 0],
    [2, -8, 25, 119, -12, 2, 0, 0],
    [2, -7, 22, 120, -11, 2, 0, 0],
    [1, -6, 20, 121, -10, 2, 0, 0],
    [1, -6, 18, 122, -9, 2, 0, 0],
    [1, -5, 15, 123, -8, 2, 0, 0],
    [1, -4, 13, 124, -7, 1, 0, 0],
    [1, -4, 11, 125, -6, 1, 0, 0],
    [1, -3, 8, 126, -5, 1, 0, 0],
    [1, -2, 6, 126, -4, 1, 0, 0],
    [0, -1, 4, 127, -3, 1, 0, 0],
    [0, 0, 2, 127, -1, 0, 0, 0],
    [0, 0, 0, 127, 1, 0, 0, 0],
    [0, 0, -1, 127, 2, 0, 0, 0],
    [0, 1, -3, 127, 4, -2, 1, 0],
    [0, 1, -5, 127, 6, -2, 1, 0],
    [0, 2, -6, 126, 8, -3, 1, 0],
    [-1, 2, -7, 126, 11, -4, 2, -1],
    [-1, 3, -8, 125, 13, -5, 2, -1],
    [-1, 3, -10, 124, 16, -6, 3, -1],
    [-1, 4, -11, 123, 18, -7, 3, -1],
    [-1, 4, -12, 122, 20, -7, 3, -1],
    [-1, 4, -13, 121, 23, -8, 3, -1],
    [-2, 5, -14, 120, 25, -9, 4, -1],
    [-1, 5, -15, 119, 27, -10, 4, -1],
    [-1, 5, -16, 118, 30, -11, 4, -1],
    [-2, 6, -17, 116, 33, -12, 5, -1],
    [-2, 6, -17, 114, 35, -12, 5, -1],
    [-2, 6, -18, 113, 38, -13, 5, -1],
    [-2, 7, -19, 111, 41, -14, 6, -2],
    [-2, 7, -19, 110, 43, -15, 6, -2],
    [-2, 7, -20, 108, 46, -15, 6, -2],
    [-2, 7, -20, 106, 49, -16, 6, -2],
    [-2, 7, -21, 104, 51, -16, 7, -2],
    [-2, 7, -21, 102, 54, -17, 7, -2],
    [-2, 8, -21, 100, 56, -18, 7, -2],
    [-2, 8, -22, 98, 59, -18, 7, -2],
    [-2, 8, -22, 96, 62, -19, 7, -2],
    [-2, 8, -22, 94, 64, -19, 7, -2],
    [-2, 8, -22, 91, 67, -20, 8, -2],
    [-2, 8, -22, 89, 69, -20, 8, -2],
    [-2, 8, -22, 87, 72, -21, 8, -2],
    [-2, 8, -21, 84, 74, -21, 8, -2],
    [-2, 8, -22, 82, 77, -21, 8, -2],
    [-2, 8, -21, 79, 79, -21, 8, -2],
    [-2, 8, -21, 77, 82, -22, 8, -2],
    [-2, 8, -21, 74, 84, -21, 8, -2],
    [-2, 8, -21, 72, 87, -22, 8, -2],
    [-2, 8, -20, 69, 89, -22, 8, -2],
    [-2, 8, -20, 67, 91, -22, 8, -2],
    [-2, 7, -19, 64, 94, -22, 8, -2],
    [-2, 7, -19, 62, 96, -22, 8, -2],
    [-2, 7, -18, 59, 98, -22, 8, -2],
    [-2, 7, -18, 56, 100, -21, 8, -2],
    [-2, 7, -17, 54, 102, -21, 7, -2],
    [-2, 7, -16, 51, 104, -21, 7, -2],
    [-2, 6, -16, 49, 106, -20, 7, -2],
    [-2, 6, -15, 46, 108, -20, 7, -2],
    [-2, 6, -15, 43, 110, -19, 7, -2],
    [-2, 6, -14, 41, 111, -19, 7, -2],
    [-1, 5, -13, 38, 113, -18, 6, -2],
    [-1, 5, -12, 35, 114, -17, 6, -2],
    [-1, 5, -12, 33, 116, -17, 6, -2],
    [-1, 4, -11, 30, 118, -16, 5, -1],
    [-1, 4, -10, 27, 119, -15, 5, -1],
    [-1, 4, -9, 25, 120, -14, 5, -2],
    [-1, 3, -8, 23, 121, -13, 4, -1],
    [-1, 3, -7, 20, 122, -12, 4, -1],
    [-1, 3, -7, 18, 123, -11, 4, -1],
    [-1, 3, -6, 16, 124, -10, 3, -1],
    [-1, 2, -5, 13, 125, -8, 3, -1],
    [-1, 2, -4, 11, 126, -7, 2, -1],
    [0, 1, -3, 8, 126, -6, 2, 0],
    [0, 1, -2, 6, 127, -5, 1, 0],
    [0, 1, -2, 4, 127, -3, 1, 0],
    [0, 0, 0, 2, 127, -1, 0, 0],
    [0, 0, 0, 1, 127, 0, 0, 0],
    [0, 0, 0, -1, 127, 2, 0, 0],
    [0, 0, 1, -3, 127, 4, -1, 0],
    [0, 0, 1, -4, 126, 6, -2, 1],
    [0, 0, 1, -5, 126, 8, -3, 1],
    [0, 0, 1, -6, 125, 11, -4, 1],
    [0, 0, 1, -7, 124, 13, -4, 1],
    [0, 0, 2, -8, 123, 15, -5, 1],
    [0, 0, 2, -9, 122, 18, -6, 1],
    [0, 0, 2, -10, 121, 20, -6, 1],
    [0, 0, 2, -11, 120, 22, -7, 2],
    [0, 0, 2, -12, 119, 25, -8, 2],
    [0, 0, 3, -13, 117, 27, -8, 2],
    [0, 0, 3, -13, 116, 29, -9, 2],
    [0, 0, 3, -14, 114, 32, -10, 3],
    [0, 0, 3, -15, 113, 35, -10, 2],
    [0, 0, 3, -15, 111, 37, -11, 3],
    [0, 0, 3, -16, 109, 40, -11, 3],
    [0, 0, 3, -16, 108, 42, -12, 3],
    [0, 0, 4, -17, 106, 45, -13, 3],
    [0, 0, 4, -17, 104, 47, -13, 3],
    [0, 0, 4, -17, 102, 50, -14, 3],
    [0, 0, 4, -17, 100, 52, -14, 3],
    [0, 0, 4, -18, 98, 55, -15, 4],
    [0, 0, 4, -18, 96, 58, -15, 3],
    [0, 0, 4, -18, 94, 60, -16, 4],
    [0, 0, 4, -18, 91, 63, -16, 4],
    [0, 0, 4, -18, 89, 65, -16, 4],
    [0, 0, 4, -18, 87, 68, -17, 4],
    [0, 0, 4, -18, 85, 70, -17, 4],
    [0, 0, 4, -18, 82, 73, -17, 4],
    [0, 0, 4, -18, 80, 75, -17, 4],
    [0, 0, 4, -18, 78, 78, -18, 4],
    [0, 0, 4, -17, 75, 80, -18, 4],
    [0, 0, 4, -17, 73, 82, -18, 4],
    [0, 0, 4, -17, 70, 85, -18, 4],
    [0, 0, 4, -17, 68, 87, -18, 4],
    [0, 0, 4, -16, 65, 89, -18, 4],
    [0, 0, 4, -16, 63, 91, -18, 4],
    [0, 0, 4, -16, 60, 94, -18, 4],
    [0, 0, 3, -15, 58, 96, -18, 4],
    [0, 0, 4, -15, 55, 98, -18, 4],
    [0, 0, 3, -14, 52, 100, -17, 4],
    [0, 0, 3, -14, 50, 102, -17, 4],
    [0, 0, 3, -13, 47, 104, -17, 4],
    [0, 0, 3, -13, 45, 106, -17, 4],
    [0, 0, 3, -12, 42, 108, -16, 3],
    [0, 0, 3, -11, 40, 109, -16, 3],
    [0, 0, 3, -11, 37, 111, -15, 3],
    [0, 0, 2, -10, 35, 113, -15, 3],
    [0, 0, 3, -10, 32, 114, -14, 3],
    [0, 0, 2, -9, 29, 116, -13, 3],
    [0, 0, 2, -8, 27, 117, -13, 3],
    [0, 0, 2, -8, 25, 119, -12, 2],
    [0, 0, 2, -7, 22, 120, -11, 2],
    [0, 0, 1, -6, 20, 121, -10, 2],
    [0, 0, 1, -6, 18, 122, -9, 2],
    [0, 0, 1, -5, 15, 123, -8, 2],
    [0, 0, 1, -4, 13, 124, -7, 1],
    [0, 0, 1, -4, 11, 125, -6, 1],
    [0, 0, 1, -3, 8, 126, -5, 1],
    [0, 0, 1, -2, 6, 126, -4, 1],
    [0, 0, 0, -1, 4, 127, -3, 1],
    [0, 0, 0, 0, 2, 127, -1, 0],
    [0, 0, 0, 0, 2, 127, -1, 0],
];

/// `div_lut` (`warped_motion.c`).
#[rustfmt::skip]
const DIV_LUT: [u16; 257] = [
    16384, 16320, 16257, 16194, 16132, 16070, 16009, 15948, 15888, 15828, 15768,
    15709, 15650, 15592, 15534, 15477, 15420, 15364, 15308, 15252, 15197, 15142,
    15087, 15033, 14980, 14926, 14873, 14821, 14769, 14717, 14665, 14614, 14564,
    14513, 14463, 14413, 14364, 14315, 14266, 14218, 14170, 14122, 14075, 14028,
    13981, 13935, 13888, 13843, 13797, 13752, 13707, 13662, 13618, 13574, 13530,
    13487, 13443, 13400, 13358, 13315, 13273, 13231, 13190, 13148, 13107, 13066,
    13026, 12985, 12945, 12906, 12866, 12827, 12788, 12749, 12710, 12672, 12633,
    12596, 12558, 12520, 12483, 12446, 12409, 12373, 12336, 12300, 12264, 12228,
    12193, 12157, 12122, 12087, 12053, 12018, 11984, 11950, 11916, 11882, 11848,
    11815, 11782, 11749, 11716, 11683, 11651, 11619, 11586, 11555, 11523, 11491,
    11460, 11429, 11398, 11367, 11336, 11305, 11275, 11245, 11215, 11185, 11155,
    11125, 11096, 11067, 11038, 11009, 10980, 10951, 10923, 10894, 10866, 10838,
    10810, 10782, 10755, 10727, 10700, 10673, 10645, 10618, 10592, 10565, 10538,
    10512, 10486, 10460, 10434, 10408, 10382, 10356, 10331, 10305, 10280, 10255,
    10230, 10205, 10180, 10156, 10131, 10107, 10082, 10058, 10034, 10010, 9986,
    9963, 9939, 9916, 9892, 9869, 9846, 9823, 9800, 9777, 9754, 9732,
    9709, 9687, 9664, 9642, 9620, 9598, 9576, 9554, 9533, 9511, 9489,
    9468, 9447, 9425, 9404, 9383, 9362, 9341, 9321, 9300, 9279, 9259,
    9239, 9218, 9198, 9178, 9158, 9138, 9118, 9098, 9079, 9059, 9039,
    9020, 9001, 8981, 8962, 8943, 8924, 8905, 8886, 8867, 8849, 8830,
    8812, 8793, 8775, 8756, 8738, 8720, 8702, 8684, 8666, 8648, 8630,
    8613, 8595, 8577, 8560, 8542, 8525, 8508, 8490, 8473, 8456, 8439,
    8422, 8405, 8389, 8372, 8355, 8339, 8322, 8306, 8289, 8273, 8257,
    8240, 8224, 8208, 8192,
];

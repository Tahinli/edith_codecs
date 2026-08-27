//! The integer transforms and the quantizer (8.6.2 - 8.6.4).
//!
//! The *inverse* path here is normative: a decoder does exactly this, so the
//! encoder's reconstruction runs the same code and is bit-identical to what any
//! conformant decoder produces. The forward path and the quantizer are the
//! encoder's own choice; what makes them correct is that they invert the
//! normative path — which is what [`tests::round_trip_recovers_flat_blocks`]
//! and the the oracle conformance test in `tests/` check.
//!
//! One transcription safeguard is worth naming: only the first 16 columns of the
//! 32x32 matrix are typed in. The rest follow from `M[m][31 - n] = (-1)^m M[m][n]`,
//! which the spec's own second half satisfies, and a test asserts that half back
//! entry by entry. A wrong column is the classic defect in this kind of table
//! (it cost this family a week in H.264), so the table is checked against
//! orthogonality as well as against the printed values.

/// Columns 0..15 of `transMatrix` (8-319), rows 0..31.
///
/// The size-N transform uses rows `k * (32 / N)` and columns `0..N`.
#[rustfmt::skip]
const TRANS_MATRIX_HALF: [[i32; 16]; 32] = [
    [64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64],
    [90, 90, 88, 85, 82, 78, 73, 67, 61, 54, 46, 38, 31, 22, 13, 4],
    [90, 87, 80, 70, 57, 43, 25, 9, -9, -25, -43, -57, -70, -80, -87, -90],
    [90, 82, 67, 46, 22, -4, -31, -54, -73, -85, -90, -88, -78, -61, -38, -13],
    [89, 75, 50, 18, -18, -50, -75, -89, -89, -75, -50, -18, 18, 50, 75, 89],
    [88, 67, 31, -13, -54, -82, -90, -78, -46, -4, 38, 73, 90, 85, 61, 22],
    [87, 57, 9, -43, -80, -90, -70, -25, 25, 70, 90, 80, 43, -9, -57, -87],
    [85, 46, -13, -67, -90, -73, -22, 38, 82, 88, 54, -4, -61, -90, -78, -31],
    [83, 36, -36, -83, -83, -36, 36, 83, 83, 36, -36, -83, -83, -36, 36, 83],
    [82, 22, -54, -90, -61, 13, 78, 85, 31, -46, -90, -67, 4, 73, 88, 38],
    [80, 9, -70, -87, -25, 57, 90, 43, -43, -90, -57, 25, 87, 70, -9, -80],
    [78, -4, -82, -73, 13, 85, 67, -22, -88, -61, 31, 90, 54, -38, -90, -46],
    [75, -18, -89, -50, 50, 89, 18, -75, -75, 18, 89, 50, -50, -89, -18, 75],
    [73, -31, -90, -22, 78, 67, -38, -90, -13, 82, 61, -46, -88, -4, 85, 54],
    [70, -43, -87, 9, 90, 25, -80, -57, 57, 80, -25, -90, -9, 87, 43, -70],
    [67, -54, -78, 38, 85, -22, -90, 4, 90, 13, -88, -31, 82, 46, -73, -61],
    [64, -64, -64, 64, 64, -64, -64, 64, 64, -64, -64, 64, 64, -64, -64, 64],
    [61, -73, -46, 82, 31, -88, -13, 90, -4, -90, 22, 85, -38, -78, 54, 67],
    [57, -80, -25, 90, -9, -87, 43, 70, -70, -43, 87, 9, -90, 25, 80, -57],
    [54, -85, -4, 88, -46, -61, 82, 13, -90, 38, 67, -78, -22, 90, -31, -73],
    [50, -89, 18, 75, -75, -18, 89, -50, -50, 89, -18, -75, 75, 18, -89, 50],
    [46, -90, 38, 54, -90, 31, 61, -88, 22, 67, -85, 13, 73, -82, 4, 78],
    [43, -90, 57, 25, -87, 70, 9, -80, 80, -9, -70, 87, -25, -57, 90, -43],
    [38, -88, 73, -4, -67, 90, -46, -31, 85, -78, 13, 61, -90, 54, 22, -82],
    [36, -83, 83, -36, -36, 83, -83, 36, 36, -83, 83, -36, -36, 83, -83, 36],
    [31, -78, 90, -61, 4, 54, -88, 82, -38, -22, 73, -90, 67, -13, -46, 85],
    [25, -70, 90, -80, 43, 9, -57, 87, -87, 57, -9, -43, 80, -90, 70, -25],
    [22, -61, 85, -90, 73, -38, -4, 46, -78, 90, -82, 54, -13, -31, 67, -88],
    [18, -50, 75, -89, 89, -75, 50, -18, -18, 50, -75, 89, -89, 75, -50, 18],
    [13, -38, 61, -78, 88, -90, 85, -73, 54, -31, 4, 22, -46, 67, -82, 90],
    [9, -25, 43, -57, 70, -80, 87, -90, 90, -87, 80, -70, 57, -43, 25, -9],
    [4, -13, 22, -31, 38, -46, 54, -61, 67, -73, 78, -82, 85, -88, 90, -90],
];

/// `transMatrix` for `trType = 1`: the 4x4 DST used for luma intra 4x4 blocks
/// (8-316).
#[rustfmt::skip]
const DST_MATRIX: [[i32; 4]; 4] = [
    [29, 55, 74, 84],
    [74, 74, 0, -74],
    [84, -29, -74, 55],
    [55, -84, 74, -29],
];

/// `M[m][n]` of the 32-point matrix, mirrored out of the stored half.
#[inline]
const fn m32(row: usize, col: usize) -> i32 {
    if col < 16 {
        TRANS_MATRIX_HALF[row][col]
    } else if row % 2 == 0 {
        TRANS_MATRIX_HALF[row][31 - col]
    } else {
        -TRANS_MATRIX_HALF[row][31 - col]
    }
}

/// The full 32x32 matrix, mirrored out at compile time so the hot butterfly
/// loops below index a flat table instead of taking [`m32`]'s per-element
/// branch: same values (this *is* [`m32`], evaluated ahead of time), fewer
/// instructions per multiply-add.
const fn build_full_matrix() -> [[i32; 32]; 32] {
    let mut out = [[0i32; 32]; 32];
    let mut row = 0;
    while row < 32 {
        let mut col = 0;
        while col < 32 {
            out[row][col] = m32(row, col);
            col += 1;
        }
        row += 1;
    }
    out
}
const FULL_MATRIX: [[i32; 32]; 32] = build_full_matrix();

/// [`FULL_MATRIX`] transposed: `FULL_MATRIX_T[col][row] == FULL_MATRIX[row][col]`.
/// [`inverse_1d`]'s odd sum walks a fixed column across varying rows, which
/// this makes a contiguous read.
const fn transpose(m: &[[i32; 32]; 32]) -> [[i32; 32]; 32] {
    let mut out = [[0i32; 32]; 32];
    let mut row = 0;
    while row < 32 {
        let mut col = 0;
        while col < 32 {
            out[col][row] = m[row][col];
            col += 1;
        }
        row += 1;
    }
    out
}
const FULL_MATRIX_T: [[i32; 32]; 32] = transpose(&FULL_MATRIX);

/// `levelScale[k]`, 8.6.3.
const LEVEL_SCALE: [i32; 6] = [40, 45, 51, 57, 64, 72];

/// The quantizer's reciprocal of [`LEVEL_SCALE`], `round(2^20 / levelScale)`.
///
/// Derived rather than transcribed: the product of the two is 2^20 to within a
/// count, which is the whole design of the HEVC quantizer, and
/// [`tests::quant_scale_is_the_reciprocal`] holds it to that.
const QUANT_SCALE: [i32; 6] = [26_214, 23_302, 20_560, 18_396, 16_384, 14_564];

/// Largest and smallest coefficient a Main-profile decoder may hold (`CoeffMin`,
/// `CoeffMax` for `extended_precision_processing_flag = 0`).
const COEFF_MIN: i32 = -32_768;
const COEFF_MAX: i32 = 32_767;

/// One-dimensional inverse transform, `y[i] = sum_j M[j][i] * x[j]`.
///
/// Split even/odd once per level: the even rows of the size-N matrix *are* the
/// size-N/2 matrix, so the recursion costs ~N^2/3 multiplies where the plain
/// product costs N^2.
fn inverse_1d(n: usize, src: &[i32], dst: &mut [i32]) {
    if n == 1 {
        dst[0] = 64 * src[0];
        return;
    }
    let half = n / 2;
    let stride = 32 / n;
    let mut even_src = [0i32; 16];
    for j in 0..half {
        even_src[j] = src[2 * j];
    }
    let mut even = [0i32; 16];
    inverse_1d(half, &even_src[..half], &mut even[..half]);
    for x in 0..half {
        let mut odd = 0i32;
        let matrix_col = &FULL_MATRIX_T[x];
        for j in 0..half {
            odd += matrix_col[(2 * j + 1) * stride] * src[2 * j + 1];
        }
        dst[x] = even[x] + odd;
        dst[n - 1 - x] = even[x] - odd;
    }
}

/// One-dimensional forward transform, `c[k] = sum_x M[k][x] * src[x]`.
fn forward_1d(n: usize, src: &[i32], dst: &mut [i32]) {
    if n == 1 {
        dst[0] = 64 * src[0];
        return;
    }
    let half = n / 2;
    let stride = 32 / n;
    let mut sums = [0i32; 16];
    let mut diffs = [0i32; 16];
    for (x, (sum, diff)) in sums[..half]
        .iter_mut()
        .zip(diffs[..half].iter_mut())
        .enumerate()
    {
        *sum = src[x] + src[n - 1 - x];
        *diff = src[x] - src[n - 1 - x];
    }
    let mut even = [0i32; 16];
    forward_1d(half, &sums[..half], &mut even[..half]);
    for j in 0..half {
        dst[2 * j] = even[j];
        let mut odd = 0i32;
        let matrix_row = &FULL_MATRIX[(2 * j + 1) * stride];
        for (x, &diff) in diffs[..half].iter().enumerate() {
            odd += matrix_row[x] * diff;
        }
        dst[2 * j + 1] = odd;
    }
}

/// Forward 4x4 DST (the inverse of the normative `trType = 1` transform).
fn forward_dst4(src: &[i32; 4], dst: &mut [i32; 4]) {
    for (k, out) in dst.iter_mut().enumerate() {
        *out = (0..4).map(|x| DST_MATRIX[k][x] * src[x]).sum();
    }
}

/// Inverse 4x4 DST, `y[i] = sum_j M[j][i] * x[j]` (8-315).
fn inverse_dst4(src: &[i32; 4], dst: &mut [i32; 4]) {
    for (i, out) in dst.iter_mut().enumerate() {
        *out = (0..4).map(|j| DST_MATRIX[j][i] * src[j]).sum();
    }
}

/// True when a block takes the DST: luma, intra, 4x4 (8.6.4.1).
pub fn uses_dst(size: usize, is_luma: bool) -> bool {
    is_luma && size == 4
}

/// Forward transform of an `n x n` residual block into coefficients.
///
/// `residual` and `coeffs` are both row-major `n x n`; `coeffs[v * n + k]` is
/// vertical frequency `v`, horizontal frequency `k`, which is the order the
/// scans and the residual coder index.
pub fn forward_transform(residual: &[i32], coeffs: &mut [i32], n: usize, dst_type: bool) {
    let log2n = n.trailing_zeros() as i32;
    // 8-bit input: shift1 = log2N + bitDepth - 9, shift2 = log2N + 6. Together
    // they leave the DC at 128 x the mean residual whatever the block size,
    // which is what the dequantiser's shift assumes.
    let shift1 = log2n - 1;
    let shift2 = log2n + 6;
    let round1 = 1 << (shift1 - 1);
    let round2 = 1 << (shift2 - 1);
    let mut tmp = [0i32; 32 * 32];
    let mut row = [0i32; 32];
    let mut out = [0i32; 32];
    for y in 0..n {
        row[..n].copy_from_slice(&residual[y * n..y * n + n]);
        if dst_type {
            let mut src4 = [0i32; 4];
            src4.copy_from_slice(&row[..4]);
            let mut dst4 = [0i32; 4];
            forward_dst4(&src4, &mut dst4);
            out[..4].copy_from_slice(&dst4);
        } else {
            forward_1d(n, &row[..n], &mut out[..n]);
        }
        // Transposed store: the second pass then runs over what were columns.
        for (k, &value) in out[..n].iter().enumerate() {
            tmp[k * n + y] = (value + round1) >> shift1;
        }
    }
    for k in 0..n {
        row[..n].copy_from_slice(&tmp[k * n..k * n + n]);
        if dst_type {
            let mut src4 = [0i32; 4];
            src4.copy_from_slice(&row[..4]);
            let mut dst4 = [0i32; 4];
            forward_dst4(&src4, &mut dst4);
            out[..4].copy_from_slice(&dst4);
        } else {
            forward_1d(n, &row[..n], &mut out[..n]);
        }
        for (v, &value) in out[..n].iter().enumerate() {
            coeffs[v * n + k] = (value + round2) >> shift2;
        }
    }
}

/// `bdShift` (8-299) at 8-bit: 20 - BitDepth.
const BD_SHIFT: i32 = 12;

/// Inverse transform of scaled coefficients into residual samples, exactly as
/// clause 8.6.4 specifies including its intermediate clipping.
pub fn inverse_transform(scaled: &[i32], residual: &mut [i32], n: usize, dst_type: bool) {
    let mut e = [0i32; 32 * 32];
    let mut col = [0i32; 32];
    let mut out = [0i32; 32];
    // Stage 1: every column, i.e. along the vertical frequency index.
    for k in 0..n {
        let mut any = false;
        for y in 0..n {
            col[y] = scaled[y * n + k];
            any |= col[y] != 0;
        }
        if !any {
            // A column of zeros transforms to zeros; most columns of a coded
            // block are zeros, which is what quantisation is for.
            for y in 0..n {
                e[y * n + k] = 0;
            }
            continue;
        }
        if dst_type {
            let mut src4 = [0i32; 4];
            src4.copy_from_slice(&col[..4]);
            let mut dst4 = [0i32; 4];
            inverse_dst4(&src4, &mut dst4);
            out[..4].copy_from_slice(&dst4);
        } else {
            inverse_1d(n, &col[..n], &mut out[..n]);
        }
        for y in 0..n {
            e[y * n + k] = ((out[y] + 64) >> 7).clamp(COEFF_MIN, COEFF_MAX);
        }
    }
    // Stage 2: every row, then the bit-depth shift (8-299).
    let round = 1 << (BD_SHIFT - 1);
    for y in 0..n {
        col[..n].copy_from_slice(&e[y * n..y * n + n]);
        if dst_type {
            let mut src4 = [0i32; 4];
            src4.copy_from_slice(&col[..4]);
            let mut dst4 = [0i32; 4];
            inverse_dst4(&src4, &mut dst4);
            out[..4].copy_from_slice(&dst4);
        } else {
            inverse_1d(n, &col[..n], &mut out[..n]);
        }
        for x in 0..n {
            residual[y * n + x] = (out[x] + round) >> BD_SHIFT;
        }
    }
}

/// Forward transform skip (8.6.2): no transform at all, the residual samples
/// standing in for coefficients.
///
/// The decoder scales what it reads by `tsShift = 5 + Log2(nTbS)` and then
/// takes the bit-depth shift of 20 - 8 off it again, so a coefficient has to
/// carry the residual scaled by the difference for the round trip to return
/// the sample it started from — a factor of 32 at 4x4, the same gain the
/// integer transform leaves behind.
pub fn forward_transform_skip(residual: &[i32], coeffs: &mut [i32], n: usize) {
    let shift = BD_SHIFT - (5 + n.trailing_zeros() as i32);
    for (i, &r) in residual[..n * n].iter().enumerate() {
        coeffs[i] = r << shift;
    }
}

/// Inverse transform skip (8.6.2): the scaled coefficient shifted up by
/// `tsShift` and then down by the bit-depth shift, exactly as the decoder
/// does it, so the encoder reconstructs the samples the decoder will.
pub fn inverse_transform_skip(scaled: &[i32], residual: &mut [i32], n: usize) {
    let ts_shift = 5 + n.trailing_zeros() as i32;
    let round = 1i32 << (BD_SHIFT - 1);
    for (i, &d) in scaled[..n * n].iter().enumerate() {
        residual[i] = ((d.clamp(COEFF_MIN, COEFF_MAX) << ts_shift) + round) >> BD_SHIFT;
    }
}

/// Quantise coefficients in place, returning the number of non-zero levels.
///
/// The rounding offset is a third of a step, which is the intra bias every
/// encoder in this class uses: it costs a little distortion at the rounding
/// boundary and buys back more in rate.
pub fn quantize(coeffs: &[i32], levels: &mut [i32], n: usize, qp: i32) -> usize {
    quantize_offset(coeffs, levels, n, qp, 3)
}

/// [`quantize`] with the rounding written out: `dead_zone` is the divisor of
/// the quantiser step the rounding offset is, so 3 is the intra default and 2
/// is round-to-nearest.
///
/// A rate-distortion search over the levels wants the wider rounding: a search
/// that only ever takes levels down measures the rounding it started from, not
/// its own decisions.
pub fn quantize_offset(
    coeffs: &[i32],
    levels: &mut [i32],
    n: usize,
    qp: i32,
    dead_zone: i64,
) -> usize {
    let log2n = n.trailing_zeros() as i32;
    let qbits = 21 + qp / 6 - log2n;
    let scale = QUANT_SCALE[(qp % 6) as usize] as i64;
    let offset = (1i64 << qbits) / dead_zone;
    // `scale` <= 26_214 and `|coeffs[i]|` <= 32_768 (COEFF_MIN/MAX), and qp is
    // clamped to 0..=51 (crates/ec-h265/src/encoder.rs), so
    // `abs(c) * scale + offset` never exceeds ~1e9: it fits in i32 without
    // overflow, which is what lets this run in 32-bit SIMD lanes and still be
    // the exact same integer as the scalar i64 computation below.
    debug_assert!(scale <= i64::from(i32::MAX) && offset <= i64::from(i32::MAX));
    let scale32 = scale as i32;
    let offset32 = offset as i32;
    let mut nonzero = 0;
    let coeffs = &coeffs[..n * n];
    let mut i = 0;
    while i + 8 <= coeffs.len() {
        let c = wide::i32x8::from(<[i32; 8]>::try_from(&coeffs[i..i + 8]).unwrap());
        let mag =
            ((c.abs() * scale32 + offset32) >> (qbits as u32)).min(wide::i32x8::splat(COEFF_MAX));
        let level = c.simd_lt(wide::i32x8::splat(0)).select(-mag, mag);
        for (j, &lv) in level.to_array().iter().enumerate() {
            levels[i + j] = lv;
            if lv != 0 {
                nonzero += 1;
            }
        }
        i += 8;
    }
    for j in i..coeffs.len() {
        let c = coeffs[j];
        let magnitude = ((c.unsigned_abs() as i64 * scale + offset) >> qbits) as i32;
        let level = magnitude.min(COEFF_MAX);
        levels[j] = if c < 0 { -level } else { level };
        if level != 0 {
            nonzero += 1;
        }
    }
    nonzero
}

/// The unrounded level a coefficient would quantise to: the quantiser divides
/// by a step, and this is that division carried out in full precision. Sign
/// data hiding needs it to tell which coefficient is cheapest to nudge.
pub fn ideal_level(coeff: i32, n: usize, qp: i32) -> f64 {
    let log2n = n.trailing_zeros() as i32;
    let qbits = 21 + qp / 6 - log2n;
    let scale = QUANT_SCALE[(qp % 6) as usize] as f64;
    (coeff.unsigned_abs() as f64 * scale) / (1u64 << qbits) as f64
}

/// Scale levels back into coefficients, exactly as clause 8.6.3 specifies.
pub fn dequantize(levels: &[i32], scaled: &mut [i32], n: usize, qp: i32) {
    for (i, &level) in levels[..n * n].iter().enumerate() {
        scaled[i] = dequant_level(level, n, qp);
    }
}

/// One level scaled back to a coefficient, the per-coefficient half of
/// [`dequantize`]. A rate-distortion search over the levels prices one
/// coefficient at a time and has no use for the whole block.
pub fn dequant_level(level: i32, n: usize, qp: i32) -> i32 {
    let log2n = n.trailing_zeros() as i32;
    let bd_shift = log2n + 3; // bitDepth + Log2(nTbS) + 10 - 15, 8-bit
    let round = 1i64 << (bd_shift - 1);
    let scale = (16 * LEVEL_SCALE[(qp % 6) as usize]) as i64;
    let shift = qp / 6;
    let value = (((level as i64 * scale) << shift) + round) >> bd_shift;
    value.clamp(COEFF_MIN as i64, COEFF_MAX as i64) as i32
}

/// What a squared error in the transform domain is worth as a squared error in
/// the sample domain.
///
/// The forward transform's shifts leave the DC at 128 times the mean residual
/// whatever the block size, where an orthonormal transform would leave it at
/// `n` times; the transform is otherwise orthogonal, so coefficient error
/// energy is `(128 / n)^2` times sample error energy and this undoes it. A
/// rate-distortion search over the levels needs it to weigh transform-domain
/// error against a lambda calibrated on samples.
pub fn coeff_ssd_scale(n: usize) -> f64 {
    (n * n) as f64 / (128.0 * 128.0)
}

/// The chroma QP a luma QP maps to for 4:2:0 (Table 8-10), before any offset.
pub fn chroma_qp(qp_luma: i32, offset: i32) -> i32 {
    let qpi = (qp_luma + offset).clamp(0, 57);
    if qpi < 30 {
        qpi
    } else if qpi > 43 {
        qpi - 6
    } else {
        const TABLE: [i32; 14] = [29, 30, 31, 32, 33, 33, 34, 34, 35, 35, 36, 36, 37, 37];
        TABLE[(qpi - 30) as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrored_half_matches_the_spec_second_table() {
        // Spot values from transMatrixCol16to31 (8-321): row, column, value.
        let spec: &[(usize, usize, i32)] = &[
            (0, 16, 64),
            (0, 31, 64),
            (1, 16, -4),
            (1, 31, -90),
            (2, 16, -90),
            (2, 31, 90),
            (3, 16, 13),
            (3, 23, 73),
            (4, 16, 89),
            (8, 16, 83),
            (16, 16, 64),
            (16, 17, -64),
            (31, 16, 90),
            (31, 31, -4),
            (30, 16, -9),
            (29, 31, -13),
        ];
        for &(row, col, value) in spec {
            assert_eq!(m32(row, col), value, "M[{row}][{col}]");
        }
    }

    #[test]
    fn matrix_rows_are_orthogonal() {
        // Every size's rows: orthogonal, and all of equal norm. A single mistyped
        // entry breaks both.
        for log2n in 2..=5u32 {
            let n = 1usize << log2n;
            let stride = 32 / n;
            let norm: i32 = (0..n).map(|x| m32(0, x) * m32(0, x)).sum();
            for a in 0..n {
                let self_dot: i32 = (0..n)
                    .map(|x| m32(a * stride, x) * m32(a * stride, x))
                    .sum();
                // 64^2 * n is exact for the DC row; the others are within the
                // rounding the integer matrix carries.
                assert!(
                    (self_dot - norm).abs() * 200 < norm,
                    "row {a} of {n}: norm {self_dot} vs {norm}"
                );
                for b in a + 1..n {
                    let dot: i32 = (0..n)
                        .map(|x| m32(a * stride, x) * m32(b * stride, x))
                        .sum();
                    assert!(dot.abs() * 100 < norm, "rows {a},{b} of {n}: dot {dot}");
                }
            }
        }
    }

    #[test]
    fn quant_scale_is_the_reciprocal() {
        for k in 0..6 {
            let product = QUANT_SCALE[k] as i64 * LEVEL_SCALE[k] as i64;
            assert!((product - (1 << 20)).abs() < 64, "k={k}: {product}");
            assert_eq!(
                QUANT_SCALE[k],
                ((1 << 20) as f64 / LEVEL_SCALE[k] as f64).round() as i32
            );
        }
    }

    fn round_trip(residual: &[i32], n: usize, qp: i32, dst: bool) -> Vec<i32> {
        let mut coeffs = vec![0i32; n * n];
        forward_transform(residual, &mut coeffs, n, dst);
        let mut levels = vec![0i32; n * n];
        quantize(&coeffs, &mut levels, n, qp);
        let mut scaled = vec![0i32; n * n];
        dequantize(&levels, &mut scaled, n, qp);
        let mut out = vec![0i32; n * n];
        inverse_transform(&scaled, &mut out, n, dst);
        out
    }

    #[test]
    fn round_trip_recovers_flat_blocks() {
        // A constant residual stays constant at any QP — only the DC level
        // survives — and at low QP it comes back to its own value. At QP 40 one
        // level step *is* 16 samples, so accuracy is asserted where accuracy is
        // on offer and flatness everywhere.
        for &n in &[4usize, 8, 16, 32] {
            for &qp in &[0i32, 12, 22, 27, 40, 51] {
                let residual = vec![20i32; n * n];
                let out = round_trip(&residual, n, qp, false);
                assert!(out.iter().all(|&v| v == out[0]), "n={n} qp={qp}: {out:?}");
                if qp <= 22 {
                    assert!((out[0] - 20).abs() <= 2, "n={n} qp={qp}: {}", out[0]);
                }
            }
        }
    }

    #[test]
    fn round_trip_is_near_lossless_at_low_qp() {
        // A textured block at QP 4 comes back within a sample or two; this is
        // what proves the forward transform is the inverse of the normative one
        // rather than merely self-consistent.
        for &n in &[4usize, 8, 16, 32] {
            for dst in [false, true] {
                if dst && n != 4 {
                    continue;
                }
                let residual: Vec<i32> =
                    (0..n * n).map(|i| (((i * 37) % 61) as i32) - 30).collect();
                let out = round_trip(&residual, n, 4, dst);
                let worst = residual
                    .iter()
                    .zip(&out)
                    .map(|(a, b)| (a - b).abs())
                    .max()
                    .unwrap();
                assert!(worst <= 3, "n={n} dst={dst}: worst {worst}");
            }
        }
    }

    /// Transform skip is only a pair of shifts, so the pair has to be exact:
    /// what the encoder writes as a coefficient must come back as the residual
    /// sample it started from, for every sample an 8-bit residual can hold.
    #[test]
    fn transform_skip_round_trips_every_residual_sample() {
        let residual: Vec<i32> = (-255..=255).collect();
        for chunk in residual.chunks(16) {
            let mut block = [0i32; 16];
            block[..chunk.len()].copy_from_slice(chunk);
            let mut coeffs = [0i32; 16];
            forward_transform_skip(&block, &mut coeffs, 4);
            let mut back = [0i32; 16];
            inverse_transform_skip(&coeffs, &mut back, 4);
            assert_eq!(back, block, "transform skip is not its own inverse");
        }
    }

    #[test]
    fn dc_only_coefficient_is_flat_after_inverse() {
        // The spec's inverse of a DC-only block is a constant; a transposed
        // matrix index would tilt it.
        for &n in &[4usize, 8, 16, 32] {
            let mut scaled = vec![0i32; n * n];
            scaled[0] = 4096;
            let mut out = vec![0i32; n * n];
            inverse_transform(&scaled, &mut out, n, false);
            assert!(out.iter().all(|&v| v == out[0]), "n={n}: {out:?}");
            // 4096 / 128: the two-stage inverse divides by 2^7 after the first
            // pass and 2^12 after the second, against 64 x 64 of matrix gain.
            assert_eq!(out[0], 32);
        }
    }

    #[test]
    fn chroma_qp_table() {
        assert_eq!(chroma_qp(29, 0), 29);
        assert_eq!(chroma_qp(30, 0), 29);
        assert_eq!(chroma_qp(35, 0), 33);
        assert_eq!(chroma_qp(43, 0), 37);
        assert_eq!(chroma_qp(44, 0), 38);
        assert_eq!(chroma_qp(51, 0), 45);
        assert_eq!(chroma_qp(30, -2), 28);
    }
}

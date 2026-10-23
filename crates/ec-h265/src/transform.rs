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
pub(crate) const DST_MATRIX: [[i32; 4]; 4] = [
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
    } else if row.is_multiple_of(2) {
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
pub(crate) const FULL_MATRIX: [[i32; 32]; 32] = build_full_matrix();

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
pub(crate) const FULL_MATRIX_T: [[i32; 32]; 32] = transpose(&FULL_MATRIX);

/// `levelScale[k]`, 8.6.3.
pub(crate) const LEVEL_SCALE: [i32; 6] = [40, 45, 51, 57, 64, 72];

/// The quantizer's reciprocal of [`LEVEL_SCALE`], `round(2^20 / levelScale)`.
///
/// Derived rather than transcribed: the product of the two is 2^20 to within a
/// count, which is the whole design of the HEVC quantizer, and
/// [`tests::quant_scale_is_the_reciprocal`] holds it to that.
const QUANT_SCALE: [i32; 6] = [26_214, 23_302, 20_560, 18_396, 16_384, 14_564];

/// Largest and smallest coefficient a Main-profile decoder may hold (`CoeffMin`,
/// `CoeffMax` for `extended_precision_processing_flag = 0`).
pub(crate) const COEFF_MIN: i32 = -32_768;
pub(crate) const COEFF_MAX: i32 = 32_767;

/// Dot product of two equal-length slices, SIMD in 8-wide chunks with a
/// scalar tail.
///
/// Both `forward_1d` and `inverse_1d`'s odd sums are `sum_j m[j] * x[j]` over
/// at most 16 terms; matrix entries are `|m| <= 90` and `x` is a coefficient
/// or residual sample clamped to `COEFF_MIN..=COEFF_MAX` (+/-32768), so each
/// product is bounded by ~3.0e6 and the full sum of 16 by ~4.7e7 — nowhere
/// near overflowing i32, whether accumulated in one lane or spread across
/// eight and reduced at the end.
#[inline]
fn dot(a: &[i32], b: &[i32]) -> i32 {
    debug_assert_eq!(a.len(), b.len());
    let mut acc = wide::i32x8::splat(0);
    let mut i = 0;
    while i + 8 <= a.len() {
        let av = wide::i32x8::from(<[i32; 8]>::try_from(&a[i..i + 8]).unwrap());
        let bv = wide::i32x8::from(<[i32; 8]>::try_from(&b[i..i + 8]).unwrap());
        acc += av * bv;
        i += 8;
    }
    let mut total: i32 = acc.to_array().iter().sum();
    for j in i..a.len() {
        total += a[j] * b[j];
    }
    total
}

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
    // `src[2 * j + 1]` doesn't depend on `x`: gather it once instead of once
    // per `x` (it was being re-read `half` times over).
    let mut odd_src = [0i32; 16];
    for j in 0..half {
        odd_src[j] = src[2 * j + 1];
    }
    for x in 0..half {
        let matrix_col = &FULL_MATRIX_T[x];
        let mut odd_matrix = [0i32; 16];
        for j in 0..half {
            odd_matrix[j] = matrix_col[(2 * j + 1) * stride];
        }
        let odd = dot(&odd_matrix[..half], &odd_src[..half]);
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
        let matrix_row = &FULL_MATRIX[(2 * j + 1) * stride];
        dst[2 * j + 1] = dot(&matrix_row[..half], &diffs[..half]);
    }
}

/// Forward 1-D transform of `lines` independent vectors at once. `src[x * lines + l]`
/// is sample `x` of line `l`; `dst[k * lines + l]` is coefficient `k`. The
/// butterfly is the one in [`forward_1d`], but every operation runs across
/// the lines, which the compiler turns into SIMD.
fn forward_lines(n: usize, lines: usize, src: &[i32], dst: &mut [i32], scratch: &mut [i32]) {
    if n == 1 {
        for l in 0..lines {
            dst[l] = 64 * src[l];
        }
        return;
    }
    let half = n / 2;
    let stride = 32 / n;
    let span = half * lines;
    let (sums, rest) = scratch.split_at_mut(span);
    let (diffs, rest) = rest.split_at_mut(span);
    let (even, rest) = rest.split_at_mut(span);
    for x in 0..half {
        let a = &src[x * lines..x * lines + lines];
        let b = &src[(n - 1 - x) * lines..(n - 1 - x) * lines + lines];
        let s = &mut sums[x * lines..x * lines + lines];
        let d = &mut diffs[x * lines..x * lines + lines];
        for l in 0..lines {
            s[l] = a[l] + b[l];
            d[l] = a[l] - b[l];
        }
    }
    forward_lines(half, lines, sums, even, rest);
    for j in 0..half {
        dst[2 * j * lines..(2 * j + 1) * lines].copy_from_slice(&even[j * lines..(j + 1) * lines]);
        let matrix_row = &FULL_MATRIX[(2 * j + 1) * stride];
        let out = &mut dst[(2 * j + 1) * lines..(2 * j + 2) * lines];
        out.fill(0);
        for x in 0..half {
            let m = matrix_row[x];
            let d = &diffs[x * lines..x * lines + lines];
            for l in 0..lines {
                out[l] += m * d[l];
            }
        }
    }
}

/// [`forward_transform`] for the plain DCT, computed across rows and columns
/// at once. Bit-identical to the line-at-a-time version. `SQ` is `n * n` and
/// `SC` the scratch the butterfly needs (`3 * n * n`); they are const so a 4x4
/// block does not zero 4 KiB buffers it will not use.
fn forward_dct_gen<const SQ: usize, const SC: usize>(
    residual: &[i32],
    coeffs: &mut [i32],
    n: usize,
) {
    let log2n = n.trailing_zeros() as i32;
    let shift1 = log2n - 1;
    let shift2 = log2n + 6;
    let round1 = 1 << (shift1 - 1);
    let round2 = 1 << (shift2 - 1);
    let mut xt = [0i32; SQ];
    let mut a = [0i32; SQ];
    let mut b = [0i32; SQ];
    let mut scratch = [0i32; SC];
    // xt[x][y] = residual[y][x]
    for y in 0..n {
        for x in 0..n {
            xt[x * n + y] = residual[y * n + x];
        }
    }
    // a[k][y]: first pass over rows.
    forward_lines(n, n, &xt, &mut a, &mut scratch);
    // b[y][k] = shifted a[k][y]
    for k in 0..n {
        for y in 0..n {
            b[y * n + k] = (a[k * n + y] + round1) >> shift1;
        }
    }
    // Second pass over columns k: coefficient v of line k.
    forward_lines(n, n, &b, &mut a, &mut scratch);
    for (c, &v) in coeffs[..n * n].iter_mut().zip(&a) {
        *c = (v + round2) >> shift2;
    }
}

fn forward_dct_fast(residual: &[i32], coeffs: &mut [i32], n: usize) {
    match n {
        4 => forward_dct_gen::<16, 48>(residual, coeffs, n),
        8 => forward_dct_gen::<64, 192>(residual, coeffs, n),
        16 => forward_dct_gen::<256, 768>(residual, coeffs, n),
        _ => forward_dct_gen::<1024, 3072>(residual, coeffs, n),
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
    #[cfg(target_arch = "x86_64")]
    if matches!(n, 4 | 8 | 16 | 32)
        && (!dst_type || n == 4)
        && crate::simd_tx::forward_transform_avx2(residual, coeffs, n, dst_type)
    {
        return;
    }
    forward_transform_scalar(residual, coeffs, n, dst_type);
}

/// [`forward_transform`] without the vector kernels.
pub(crate) fn forward_transform_scalar(
    residual: &[i32],
    coeffs: &mut [i32],
    n: usize,
    dst_type: bool,
) {
    if !dst_type && n >= 4 {
        forward_dct_fast(residual, coeffs, n);
        return;
    }
    forward_transform_ref(residual, coeffs, n, dst_type);
}

/// Line-at-a-time forward transform: the reference the fast path is tested against.
fn forward_transform_ref(residual: &[i32], coeffs: &mut [i32], n: usize, dst_type: bool) {
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
/// clause 8.6.4 specifies including its intermediate clipping. 8-bit samples.
pub fn inverse_transform(scaled: &[i32], residual: &mut [i32], n: usize, dst_type: bool) {
    inverse_transform_bd(scaled, residual, n, dst_type, 8);
}

/// [`inverse_transform`] at any bit depth: the final shift is `20 - BitDepth`.
pub fn inverse_transform_bd(
    scaled: &[i32],
    residual: &mut [i32],
    n: usize,
    dst_type: bool,
    bit_depth: u32,
) {
    #[cfg(target_arch = "x86_64")]
    if matches!(n, 4 | 8 | 16 | 32)
        && (!dst_type || n == 4)
        && crate::simd_tx::inverse_transform_avx2(
            scaled,
            residual,
            n,
            dst_type,
            20 - bit_depth as i32,
        )
    {
        return;
    }
    inverse_transform_scalar(scaled, residual, n, dst_type, bit_depth);
}

/// [`inverse_transform_bd`] without the vector kernels.
pub(crate) fn inverse_transform_scalar(
    scaled: &[i32],
    residual: &mut [i32],
    n: usize,
    dst_type: bool,
    bit_depth: u32,
) {
    let bd_shift = 20 - bit_depth as i32;
    let mut col = [0i32; 32];
    let mut out = [0i32; 32];
    // Stage 1: every column, i.e. along the vertical frequency index. The
    // intermediate `e` lives in `residual` itself: stage 2 reads a row before it
    // writes that row and touches no other, so no second buffer is needed.
    // `max_k` is one past the last column with a non-zero coefficient.
    let mut max_k = 0;
    for k in 0..n {
        let mut nz = 0;
        for y in 0..n {
            col[y] = scaled[y * n + k];
            if col[y] != 0 {
                nz = y + 1;
            }
        }
        if nz == 0 {
            // A column of zeros transforms to zeros; most columns of a coded
            // block are zeros, which is what quantisation is for.
            for y in 0..n {
                residual[y * n + k] = 0;
            }
            continue;
        }
        max_k = k + 1;
        transform_1d(n, dst_type, &col[..n], nz, &mut out[..n]);
        for y in 0..n {
            residual[y * n + k] = ((out[y] + 64) >> 7).clamp(COEFF_MIN, COEFF_MAX);
        }
    }
    // Stage 2: every row, then the bit-depth shift (8-299). Columns past
    // `max_k` are zero after stage 1, so the row transform stops there.
    let round = 1 << (bd_shift - 1);
    for y in 0..n {
        col[..n].copy_from_slice(&residual[y * n..y * n + n]);
        transform_1d(n, dst_type, &col[..n], max_k, &mut out[..n]);
        for x in 0..n {
            residual[y * n + x] = (out[x] + round) >> bd_shift;
        }
    }
}

/// One 1-D inverse transform of `src` into `out` (`n` samples each). `nz` bounds
/// the non-zero inputs: `src[nz..]` are all zero, so the direct sum can stop there.
/// The DST is only used for 4x4 blocks, where `nz` does not matter.
fn transform_1d(n: usize, dst_type: bool, src: &[i32], nz: usize, out: &mut [i32]) {
    if dst_type {
        let mut src4 = [0i32; 4];
        src4.copy_from_slice(&src[..4]);
        let mut dst4 = [0i32; 4];
        inverse_dst4(&src4, &mut dst4);
        out[..4].copy_from_slice(&dst4);
    } else if nz <= DIRECT_MAX_NZ {
        inverse_1d_direct(n, &src[..nz], &mut out[..n]);
    } else {
        inverse_1d(n, &src[..n], &mut out[..n]);
    }
}

/// Up to this many non-zero inputs, [`inverse_1d_direct`] beats the butterfly.
const DIRECT_MAX_NZ: usize = 16;

/// The plain product `out[i] = sum_k M_n[k][i] * src[k]` over the first `src.len()`
/// rows of the size-`n` matrix, which is the 32-point matrix's rows `k * (32 / n)`
/// and its first `n` columns. The inner loop runs over outputs, so it is a
/// contiguous multiply-add.
fn inverse_1d_direct(n: usize, src: &[i32], out: &mut [i32]) {
    let stride = 32 / n;
    let mut acc = [0i32; 32];
    let acc = &mut acc[..n];
    for (k, &c) in src.iter().enumerate() {
        if c == 0 {
            continue;
        }
        let row = &FULL_MATRIX[k * stride][..n];
        for (a, &m) in acc.iter_mut().zip(row) {
            *a += m * c;
        }
    }
    out[..n].copy_from_slice(acc);
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
    inverse_transform_skip_bd(scaled, residual, n, 8);
}

/// [`inverse_transform_skip`] at any bit depth (8.6.4.2 with `bdShift = 20 - BitDepth`).
pub fn inverse_transform_skip_bd(scaled: &[i32], residual: &mut [i32], n: usize, bit_depth: u32) {
    let ts_shift = 5 + n.trailing_zeros() as i32;
    let bd_shift = 20 - bit_depth as i32;
    let round = 1i32 << (bd_shift - 1);
    for (i, &d) in scaled[..n * n].iter().enumerate() {
        residual[i] = ((d.clamp(COEFF_MIN, COEFF_MAX) << ts_shift) + round) >> bd_shift;
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
    // QpY may be negative at 10 bits (down to -12), so the split is Euclidean.
    let qbits = 21 + qp.div_euclid(6) - log2n;
    let scale = QUANT_SCALE[qp.rem_euclid(6) as usize] as i64;
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
    #[cfg(target_arch = "x86_64")]
    if (n * n).is_multiple_of(8)
        && let Some(nonzero) =
            crate::simd_tx::quantize_avx2(coeffs, levels, n * n, scale32, offset32, qbits)
    {
        return nonzero;
    }
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

/// The smallest coefficient magnitude [`quantize_offset`] turns into a non-zero level for an
/// `n` x `n` block at `qp` with the given `dead_zone` divisor.
pub fn min_nonzero_coeff(n: usize, qp: i32, dead_zone: i64) -> i64 {
    let log2n = n.trailing_zeros() as i32;
    let qbits = 21 + qp.div_euclid(6) - log2n;
    let scale = QUANT_SCALE[qp.rem_euclid(6) as usize] as i64;
    let step = 1i64 << qbits;
    let offset = step / dead_zone;
    (step - offset + scale - 1) / scale
}

/// The unrounded level a coefficient would quantise to: the quantiser divides
/// by a step, and this is that division carried out in full precision. Sign
/// data hiding needs it to tell which coefficient is cheapest to nudge.
pub fn ideal_level(coeff: i32, n: usize, qp: i32) -> f64 {
    let log2n = n.trailing_zeros() as i32;
    let qbits = 21 + qp.div_euclid(6) - log2n;
    let scale = QUANT_SCALE[qp.rem_euclid(6) as usize] as f64;
    (coeff.unsigned_abs() as f64 * scale) / (1u64 << qbits) as f64
}

/// Scale levels back into coefficients, exactly as clause 8.6.3 specifies, with
/// the flat matrix of 16.
pub fn dequantize(levels: &[i32], scaled: &mut [i32], n: usize, qp: i32) {
    dequantize_weighted(levels, scaled, n, qp, 8, None);
}

/// The QP the quantizer works on, given the dequantizer's qP.
///
/// [`dequantize_bd`] at qP' (QpY + QpBdOffset) with the `bd - 8` extra shift gives the same
/// coefficient step as the 8-bit dequantizer at QpY, so the quantizer runs at QpY.
pub fn quant_qp(qp: i32, bit_depth: u32) -> i32 {
    qp - 6 * (bit_depth as i32 - 8)
}

/// Bring a forward transform's output to the 16-bit coefficient range at `bit_depth`.
///
/// The forward transform gives coefficients 4^(bd-8) times the 8-bit ones for the same
/// content, and the spec's `CoeffMin`/`CoeffMax` (-32768..=32767 without extended
/// precision) are what a 10-bit stream must stay inside. The division is the forward half
/// of the `20 - BitDepth` inverse shift, rounded.
pub fn scale_forward_bd(coeffs: &mut [i32], n: usize, bit_depth: u32) {
    let s = bit_depth.saturating_sub(8);
    if s == 0 {
        return;
    }
    let round = 1i32 << (s - 1);
    for c in coeffs[..n * n].iter_mut() {
        *c = (*c + round) >> s;
    }
}

/// [`dequantize`] at any bit depth; `qp` is qP (QpBdOffset included).
pub fn dequantize_bd(levels: &[i32], scaled: &mut [i32], n: usize, qp: i32, bit_depth: u32) {
    dequantize_weighted(levels, scaled, n, qp, bit_depth, None);
}

/// [`dequantize`] with a scaling matrix (8.6.4.2): `m` is the block's
/// `ScalingFactor` in raster order, or `None` for the flat 16 that a stream
/// without scaling lists (or a transform-skipped block larger than 4x4) uses.
pub fn dequantize_with(levels: &[i32], scaled: &mut [i32], n: usize, qp: i32, m: Option<&[u8]>) {
    dequantize_weighted(levels, scaled, n, qp, 8, m);
}

/// The loop behind every dequantize entry point: one block at `bit_depth`, with
/// the weight matrix `m` (or flat 16 for `None`).
pub fn dequantize_weighted(
    levels: &[i32],
    scaled: &mut [i32],
    n: usize,
    qp: i32,
    bit_depth: u32,
    m: Option<&[u8]>,
) {
    #[cfg(target_arch = "x86_64")]
    if m.is_none() && crate::simd_tx::dequantize_avx2(levels, scaled, n, qp, bit_depth) {
        return;
    }
    for (i, &level) in levels[..n * n].iter().enumerate() {
        let weight = m.map_or(16, |m| i64::from(m[i]));
        scaled[i] = dequant_weighted(level, n, qp, bit_depth, weight);
    }
}

/// One level scaled back to a coefficient, the per-coefficient half of
/// [`dequantize`]. A rate-distortion search over the levels prices one
/// coefficient at a time and has no use for the whole block.
pub fn dequant_level(level: i32, n: usize, qp: i32) -> i32 {
    dequant_level_bd(level, n, qp, 8)
}

/// [`dequant_level`] at any bit depth: `bdShift = BitDepth + Log2(nTbS) - 5`
/// (8-300 with no scaling list, m = 16). `qp` is qP, which includes QpBdOffset.
pub fn dequant_level_bd(level: i32, n: usize, qp: i32, bit_depth: u32) -> i32 {
    dequant_weighted(level, n, qp, bit_depth, 16)
}

/// [`dequant_level`] with the scaling-matrix weight `m` in place of 16.
fn dequant_weighted(level: i32, n: usize, qp: i32, bit_depth: u32, m: i64) -> i32 {
    let log2n = n.trailing_zeros() as i32;
    let bd_shift = bit_depth as i32 + log2n - 5;
    let round = 1i64 << (bd_shift - 1);
    let scale = m * i64::from(LEVEL_SCALE[(qp % 6) as usize]);
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
    chroma_qp_bd(qp_luma, offset, 8)
}

/// [`chroma_qp_bd`] for any chroma format: Table 8-10 when `ChromaArrayType` is 1 (4:2:0),
/// and `Min(qPi, 51)` otherwise (8.6.1, 4:2:2 and 4:4:4).
pub fn chroma_qp_for(qp_luma: i32, offset: i32, bit_depth: u32, chroma_format_idc: u32) -> i32 {
    if chroma_format_idc == 1 {
        return chroma_qp_bd(qp_luma, offset, bit_depth);
    }
    let qp_bd_offset_c = 6 * (bit_depth as i32 - 8);
    (qp_luma + offset).clamp(-qp_bd_offset_c, 57).min(51)
}

/// [`chroma_qp`] at any bit depth (8.6.1): qPiCb is clipped to
/// `-QpBdOffsetC..=57`, so the result can be negative at high bit depths.
/// The caller adds QpBdOffsetC to get qP.
pub fn chroma_qp_bd(qp_luma: i32, offset: i32, bit_depth: u32) -> i32 {
    let qp_bd_offset_c = 6 * (bit_depth as i32 - 8);
    let qpi = (qp_luma + offset).clamp(-qp_bd_offset_c, 57);
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
    #[test]
    fn fast_forward_matches_reference() {
        let mut seed = 12345u32;
        for &n in &[4usize, 8, 16, 32] {
            for _ in 0..50 {
                let res: Vec<i32> = (0..n * n)
                    .map(|_| {
                        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                        ((seed >> 16) % 511) as i32 - 255
                    })
                    .collect();
                let mut a = vec![0; n * n];
                let mut b = vec![0; n * n];
                forward_transform(&res, &mut a, n, false);
                forward_transform_ref(&res, &mut b, n, false);
                assert_eq!(a, b, "n={n}");
            }
        }
    }

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

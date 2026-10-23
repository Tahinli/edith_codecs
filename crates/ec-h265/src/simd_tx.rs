//! AVX2 versions of the transform and quantiser kernels in [`crate::transform`].
//!
//! Every function here returns exactly what its scalar twin in `transform.rs` returns (integer
//! arithmetic with no rounding differences), so the encoder output does not change. The
//! entry points are dispatched at run time by `multiversion`: the AVX2 clone runs the kernels,
//! the baseline clone reports "not handled" and the caller keeps the scalar path. The transforms
//! are the same even/odd butterflies as the scalar ones, run across "lines" (all columns, or all
//! rows of the transposed block, at once) so every multiply-add is a full-width vector operation;
//! the inverse also skips the zero rows and columns a quantised block is mostly made of.
//!
//! No raw pointers: every load and store goes through a slice and `bytemuck`, so each access is
//! bounds-checked.

use crate::transform::{COEFF_MAX, COEFF_MIN, DST_MATRIX, FULL_MATRIX, FULL_MATRIX_T, LEVEL_SCALE};

#[cfg(target_arch = "x86_64")]
pub(crate) use x86::*;

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::*;
    use multiversion::target::match_target;
    use std::arch::x86_64::*;

    /// Loads the first four `i32` of `s`.
    #[inline(always)]
    fn ld4(s: &[i32]) -> __m128i {
        bytemuck::cast(<[i32; 4]>::try_from(&s[..4]).unwrap())
    }

    /// Stores four `i32` at the front of `d`.
    #[inline(always)]
    fn st4(d: &mut [i32], v: __m128i) {
        let a: [i32; 4] = bytemuck::cast(v);
        d[..4].copy_from_slice(&a);
    }

    /// 4-point forward DST of four lines at once (`lines == 4`).
    #[target_feature(enable = "avx2")]
    #[allow(clippy::needless_range_loop)]
    fn dst_fwd4(src: &[i32], dst: &mut [i32]) {
        assert!(src.len() >= 16 && dst.len() >= 16);
        let r = [ld4(&src[0..]), ld4(&src[4..]), ld4(&src[8..]), ld4(&src[12..])];
        for k in 0..4 {
            let mut acc = _mm_mullo_epi32(_mm_set1_epi32(DST_MATRIX[k][0]), r[0]);
            for x in 1..4 {
                acc = _mm_add_epi32(acc, _mm_mullo_epi32(_mm_set1_epi32(DST_MATRIX[k][x]), r[x]));
            }
            st4(&mut dst[4 * k..], acc);
        }
    }

    /// 4-point inverse DST of four lines at once.
    #[target_feature(enable = "avx2")]
    #[allow(clippy::needless_range_loop)]
    fn dst_inv4(src: &[i32], dst: &mut [i32]) {
        assert!(src.len() >= 16 && dst.len() >= 16);
        let r = [ld4(&src[0..]), ld4(&src[4..]), ld4(&src[8..]), ld4(&src[12..])];
        for i in 0..4 {
            let mut acc = _mm_mullo_epi32(_mm_set1_epi32(DST_MATRIX[0][i]), r[0]);
            for j in 1..4 {
                acc = _mm_add_epi32(acc, _mm_mullo_epi32(_mm_set1_epi32(DST_MATRIX[j][i]), r[j]));
            }
            st4(&mut dst[4 * i..], acc);
        }
    }

    macro_rules! vec_kernels {
        ($V:ty, $W:expr, $load:ident, $store:ident, $add:ident, $sub:ident, $mul:ident, $set1:ident) => {
            /// Forward butterfly across `lines` vectors (`lines` a multiple of the vector
            /// width): `src[x * lines + l]` is sample `x` of line `l`, `dst[k * lines + l]`
            /// coefficient `k`.
            #[target_feature(enable = "avx2")]
            fn forward_lines(
                n: usize,
                lines: usize,
                src: &[i32],
                dst: &mut [i32],
                scratch: &mut [i32],
            ) {
                assert!(lines % $W == 0 && src.len() >= n * lines && dst.len() >= n * lines);
                if n == 1 {
                    for l in 0..lines {
                        dst[l] = 64 * src[l];
                    }
                    return;
                }
                let half = n / 2;
                let stride = 32 / n;
                let span = half * lines;
                assert!(scratch.len() >= 3 * span);
                let (sums, rest) = scratch.split_at_mut(span);
                let (diffs, rest) = rest.split_at_mut(span);
                let (even, rest) = rest.split_at_mut(span);
                for x in 0..half {
                    let pa = x * lines;
                    let pb = (n - 1 - x) * lines;
                    let mut c = 0;
                    while c < lines {
                        let a = $load(&src[pa + c..]);
                        let b = $load(&src[pb + c..]);
                        $store(&mut sums[x * lines + c..], $add(a, b));
                        $store(&mut diffs[x * lines + c..], $sub(a, b));
                        c += $W;
                    }
                }
                forward_lines(half, lines, sums, even, rest);
                for j in 0..half {
                    dst[2 * j * lines..(2 * j + 1) * lines]
                        .copy_from_slice(&even[j * lines..(j + 1) * lines]);
                    let row = &FULL_MATRIX[(2 * j + 1) * stride];
                    let mut c = 0;
                    while c < lines {
                        let mut acc = $mul($set1(row[0]), $load(&diffs[c..]));
                        for x in 1..half {
                            let d = $load(&diffs[x * lines + c..]);
                            acc = $add(acc, $mul($set1(row[x]), d));
                        }
                        $store(&mut dst[(2 * j + 1) * lines + c..], acc);
                        c += $W;
                    }
                }
            }

            /// Inverse butterfly across `lines` vectors. Only the first `nz` rows of `src`
            /// (`src[j * lines + l]`) may be non-zero. `dst[x * lines + l]` is output `x`.
            #[target_feature(enable = "avx2")]
            fn inverse_lines(
                n: usize,
                lines: usize,
                nz: usize,
                src: &[i32],
                dst: &mut [i32],
                scratch: &mut [i32],
            ) {
                assert!(lines % $W == 0 && src.len() >= n * lines && dst.len() >= n * lines);
                if n == 1 {
                    for l in 0..lines {
                        dst[l] = 64 * src[l];
                    }
                    return;
                }
                let half = n / 2;
                let stride = 32 / n;
                let span = half * lines;
                let nz_even = nz.div_ceil(2);
                let nz_odd = nz / 2;
                assert!(scratch.len() >= 2 * span && nz <= n);
                let (even_src, rest) = scratch.split_at_mut(span);
                let (even, rest) = rest.split_at_mut(span);
                for j in 0..nz_even {
                    even_src[j * lines..(j + 1) * lines]
                        .copy_from_slice(&src[2 * j * lines..(2 * j + 1) * lines]);
                }
                inverse_lines(half, lines, nz_even, even_src, even, rest);
                for x in 0..half {
                    let col = &FULL_MATRIX_T[x];
                    let mut c = 0;
                    while c < lines {
                        let e = $load(&even[x * lines + c..]);
                        let (hi, lo) = if nz_odd == 0 {
                            (e, e)
                        } else {
                            let mut odd = $mul($set1(col[stride]), $load(&src[lines + c..]));
                            for j in 1..nz_odd {
                                let s = $load(&src[(2 * j + 1) * lines + c..]);
                                odd = $add(odd, $mul($set1(col[(2 * j + 1) * stride]), s));
                            }
                            ($add(e, odd), $sub(e, odd))
                        };
                        $store(&mut dst[x * lines + c..], hi);
                        $store(&mut dst[(n - 1 - x) * lines + c..], lo);
                        c += $W;
                    }
                }
            }

            /// Forward 2-D transform, `residual` and `coeffs` row-major `n x n`. `SQ = n * n`,
            /// `SC = 3 * SQ`.
            #[target_feature(enable = "avx2")]
            fn forward_2d<const SQ: usize, const SC: usize>(
                residual: &[i32],
                coeffs: &mut [i32],
                n: usize,
                dst: bool,
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
                transpose_op(residual, &mut xt, n, n, 0, 0, false);
                if dst {
                    dst_fwd4(&xt, &mut a);
                } else {
                    forward_lines(n, n, &xt, &mut a, &mut scratch);
                }
                transpose_op(&a, &mut b, n, n, round1, shift1, false);
                if dst {
                    dst_fwd4(&b, &mut a);
                } else {
                    forward_lines(n, n, &b, &mut a, &mut scratch);
                }
                for (c, &v) in coeffs[..n * n].iter_mut().zip(&a) {
                    *c = (v + round2) >> shift2;
                }
            }

            /// Inverse 2-D transform (8.6.4.2) with the intermediate clip and the final
            /// `bd_shift`. `SQ = n * n`, `SC = 2 * SQ`.
            #[target_feature(enable = "avx2")]
            fn inverse_2d<const SQ: usize, const SC: usize>(
                scaled: &[i32],
                residual: &mut [i32],
                n: usize,
                dst: bool,
                bd_shift: i32,
            ) {
                let mut colacc = [0i32; 32];
                let mut nz_rows = 0;
                for y in 0..n {
                    let row = &scaled[y * n..y * n + n];
                    let mut any = 0;
                    for k in 0..n {
                        any |= row[k];
                        colacc[k] |= row[k];
                    }
                    if any != 0 {
                        nz_rows = y + 1;
                    }
                }
                if nz_rows == 0 {
                    residual[..n * n].fill(0);
                    return;
                }
                let mut max_k = 0;
                for k in 0..n {
                    if colacc[k] != 0 {
                        max_k = k + 1;
                    }
                }
                let mut tmp = [0i32; SQ];
                let mut tt = [0i32; SQ];
                let mut o2 = [0i32; SQ];
                let mut scratch = [0i32; SC];
                if dst {
                    dst_inv4(scaled, &mut tmp);
                    max_k = n;
                } else {
                    inverse_lines(n, n, nz_rows, scaled, &mut tmp, &mut scratch);
                }
                transpose_op(&tmp, &mut tt, n, max_k, 64, 7, true);
                if dst {
                    dst_inv4(&tt, &mut o2);
                } else {
                    inverse_lines(n, n, max_k, &tt, &mut o2, &mut scratch);
                }
                let round = 1 << (bd_shift - 1);
                transpose_op(&o2, residual, n, n, round, bd_shift, false);
            }
        };
    }

    mod v256 {
        use super::*;

        /// Loads the first eight `i32` of `s`.
        #[inline(always)]
        fn ld8(s: &[i32]) -> __m256i {
            bytemuck::cast(<[i32; 8]>::try_from(&s[..8]).unwrap())
        }

        /// Stores eight `i32` at the front of `d`.
        #[inline(always)]
        fn st8(d: &mut [i32], v: __m256i) {
            let a: [i32; 8] = bytemuck::cast(v);
            d[..8].copy_from_slice(&a);
        }

        /// `dst[c * n + y] = op(src[y * n + c])` for `c < cols` (rounded up to 8) and `y < n`,
        /// where `op(v) = (v + r) >> sh`, clamped to the 16-bit coefficient range if `clamp`.
        #[target_feature(enable = "avx2")]
        fn transpose_op(
            src: &[i32],
            dst: &mut [i32],
            n: usize,
            cols: usize,
            r: i32,
            sh: i32,
            clamp: bool,
        ) {
            assert!(n.is_multiple_of(8) && cols <= n && src.len() >= n * n && dst.len() >= n * n);
            let vr = _mm256_set1_epi32(r);
            let cnt = _mm_cvtsi32_si128(sh);
            let lo = _mm256_set1_epi32(COEFF_MIN);
            let hi = _mm256_set1_epi32(COEFF_MAX);
            let mut c0 = 0;
            while c0 < cols {
                let mut y0 = 0;
                while y0 < n {
                    let mut r8 = [_mm256_setzero_si256(); 8];
                    for (i, row) in r8.iter_mut().enumerate() {
                        let v = ld8(&src[(y0 + i) * n + c0..]);
                        let mut v = _mm256_sra_epi32(_mm256_add_epi32(v, vr), cnt);
                        if clamp {
                            v = _mm256_min_epi32(_mm256_max_epi32(v, lo), hi);
                        }
                        *row = v;
                    }
                    let t0 = _mm256_unpacklo_epi32(r8[0], r8[1]);
                    let t1 = _mm256_unpackhi_epi32(r8[0], r8[1]);
                    let t2 = _mm256_unpacklo_epi32(r8[2], r8[3]);
                    let t3 = _mm256_unpackhi_epi32(r8[2], r8[3]);
                    let t4 = _mm256_unpacklo_epi32(r8[4], r8[5]);
                    let t5 = _mm256_unpackhi_epi32(r8[4], r8[5]);
                    let t6 = _mm256_unpacklo_epi32(r8[6], r8[7]);
                    let t7 = _mm256_unpackhi_epi32(r8[6], r8[7]);
                    let u0 = _mm256_unpacklo_epi64(t0, t2);
                    let u1 = _mm256_unpackhi_epi64(t0, t2);
                    let u2 = _mm256_unpacklo_epi64(t1, t3);
                    let u3 = _mm256_unpackhi_epi64(t1, t3);
                    let u4 = _mm256_unpacklo_epi64(t4, t6);
                    let u5 = _mm256_unpackhi_epi64(t4, t6);
                    let u6 = _mm256_unpacklo_epi64(t5, t7);
                    let u7 = _mm256_unpackhi_epi64(t5, t7);
                    let o = [
                        _mm256_permute2x128_si256(u0, u4, 0x20),
                        _mm256_permute2x128_si256(u1, u5, 0x20),
                        _mm256_permute2x128_si256(u2, u6, 0x20),
                        _mm256_permute2x128_si256(u3, u7, 0x20),
                        _mm256_permute2x128_si256(u0, u4, 0x31),
                        _mm256_permute2x128_si256(u1, u5, 0x31),
                        _mm256_permute2x128_si256(u2, u6, 0x31),
                        _mm256_permute2x128_si256(u3, u7, 0x31),
                    ];
                    for (i, v) in o.into_iter().enumerate() {
                        st8(&mut dst[(c0 + i) * n + y0..], v);
                    }
                    y0 += 8;
                }
                c0 += 8;
            }
        }
        vec_kernels!(
            __m256i,
            8,
            ld8,
            st8,
            _mm256_add_epi32,
            _mm256_sub_epi32,
            _mm256_mullo_epi32,
            _mm256_set1_epi32
        );

        #[target_feature(enable = "avx2")]
        pub(super) fn fwd(residual: &[i32], coeffs: &mut [i32], n: usize) {
            match n {
                8 => forward_2d::<64, 192>(residual, coeffs, n, false),
                16 => forward_2d::<256, 768>(residual, coeffs, n, false),
                _ => forward_2d::<1024, 3072>(residual, coeffs, n, false),
            }
        }

        #[target_feature(enable = "avx2")]
        pub(super) fn inv(scaled: &[i32], residual: &mut [i32], n: usize, bd_shift: i32) {
            match n {
                8 => inverse_2d::<64, 128>(scaled, residual, n, false, bd_shift),
                16 => inverse_2d::<256, 512>(scaled, residual, n, false, bd_shift),
                _ => inverse_2d::<1024, 2048>(scaled, residual, n, false, bd_shift),
            }
        }
    }

    mod v128 {
        use super::*;

        /// Scalar twin of the 256-bit `transpose_op` for 4x4 blocks.
        #[target_feature(enable = "avx2")]
        fn transpose_op(
            src: &[i32],
            dst: &mut [i32],
            n: usize,
            cols: usize,
            r: i32,
            sh: i32,
            clamp: bool,
        ) {
            for c in 0..cols {
                for y in 0..n {
                    let v = (src[y * n + c] + r) >> sh;
                    dst[c * n + y] = if clamp {
                        v.clamp(COEFF_MIN, COEFF_MAX)
                    } else {
                        v
                    };
                }
            }
        }
        vec_kernels!(
            __m128i,
            4,
            ld4,
            st4,
            _mm_add_epi32,
            _mm_sub_epi32,
            _mm_mullo_epi32,
            _mm_set1_epi32
        );

        #[target_feature(enable = "avx2")]
        pub(super) fn fwd(residual: &[i32], coeffs: &mut [i32], dst: bool) {
            forward_2d::<16, 48>(residual, coeffs, 4, dst)
        }

        #[target_feature(enable = "avx2")]
        pub(super) fn inv(scaled: &[i32], residual: &mut [i32], dst: bool, bd_shift: i32) {
            inverse_2d::<16, 32>(scaled, residual, 4, dst, bd_shift)
        }
    }

    /// AVX2 forward 2-D transform (DCT, or the 4x4 DST) into `coeffs`. Returns `false` and
    /// leaves `coeffs` untouched when the CPU has no AVX2; the caller then runs the scalar path.
    #[multiversion::multiversion(targets("x86_64+avx2"))]
    pub(crate) fn forward_transform_avx2(
        residual: &[i32],
        coeffs: &mut [i32],
        n: usize,
        dst: bool,
    ) -> bool {
        assert!(residual.len() >= n * n && coeffs.len() >= n * n && matches!(n, 4 | 8 | 16 | 32));
        assert!(!dst || n == 4);
        match_target! {
            "x86_64+avx2" => {
                if n == 4 {
                    v128::fwd(residual, coeffs, dst)
                } else {
                    v256::fwd(residual, coeffs, n)
                }
                true
            }
            _ => false
        }
    }

    /// AVX2 inverse 2-D transform. Returns `false` when the CPU has no AVX2 (see
    /// [`forward_transform_avx2`]).
    #[multiversion::multiversion(targets("x86_64+avx2"))]
    pub(crate) fn inverse_transform_avx2(
        scaled: &[i32],
        residual: &mut [i32],
        n: usize,
        dst: bool,
        bd_shift: i32,
    ) -> bool {
        assert!(scaled.len() >= n * n && residual.len() >= n * n && matches!(n, 4 | 8 | 16 | 32));
        assert!(!dst || n == 4);
        match_target! {
            "x86_64+avx2" => {
                if n == 4 {
                    v128::inv(scaled, residual, dst, bd_shift)
                } else {
                    v256::inv(scaled, residual, n, bd_shift)
                }
                true
            }
            _ => {
                let _ = bd_shift;
                false
            }
        }
    }

    #[target_feature(enable = "avx2")]
    fn quantize_inner(
        coeffs: &[i32],
        levels: &mut [i32],
        count: usize,
        scale: i32,
        offset: i32,
        qbits: i32,
    ) -> usize {
        let coeffs = &coeffs[..count];
        let levels = &mut levels[..count];
        let vscale = _mm256_set1_epi32(scale);
        let voff = _mm256_set1_epi32(offset);
        let vmax = _mm256_set1_epi32(COEFF_MAX);
        let cnt = _mm_cvtsi32_si128(qbits);
        let zero = _mm256_setzero_si256();
        let mut nonzero = 0usize;
        let mut i = 0;
        while i + 8 <= count {
            let c = bytemuck::cast::<[i32; 8], __m256i>(
                <[i32; 8]>::try_from(&coeffs[i..i + 8]).unwrap(),
            );
            let m = _mm256_add_epi32(_mm256_mullo_epi32(_mm256_abs_epi32(c), vscale), voff);
            let m = _mm256_min_epi32(_mm256_srl_epi32(m, cnt), vmax);
            let neg = _mm256_srai_epi32(c, 31);
            let lv = _mm256_sub_epi32(_mm256_xor_si256(m, neg), neg);
            let stored: [i32; 8] = bytemuck::cast(lv);
            levels[i..i + 8].copy_from_slice(&stored);
            let z = _mm256_movemask_ps(_mm256_castsi256_ps(_mm256_cmpeq_epi32(lv, zero)));
            nonzero += 8 - (z as u32).count_ones() as usize;
            i += 8;
        }
        nonzero
    }

    /// AVX2 [`crate::transform::quantize_offset`] inner loop for `count` (a multiple of 8)
    /// coefficients; same arithmetic as the scalar i32-lane path. `None` when the CPU has no
    /// AVX2.
    #[multiversion::multiversion(targets("x86_64+avx2"))]
    pub(crate) fn quantize_avx2(
        coeffs: &[i32],
        levels: &mut [i32],
        count: usize,
        scale: i32,
        offset: i32,
        qbits: i32,
    ) -> Option<usize> {
        assert!(count.is_multiple_of(8) && coeffs.len() >= count && levels.len() >= count);
        assert!((0..32).contains(&qbits));
        match_target! {
            "x86_64+avx2" => Some(quantize_inner(coeffs, levels, count, scale, offset, qbits)),
            _ => {
                let _ = (scale, offset);
                None
            }
        }
    }

    #[target_feature(enable = "avx2")]
    fn dequant_inner(
        levels: &[i32],
        scaled: &mut [i32],
        count: usize,
        mul: i32,
        round: i32,
        shift: i32,
    ) {
        let levels = &levels[..count];
        let scaled = &mut scaled[..count];
        let vmul = _mm256_set1_epi32(mul);
        let vround = _mm256_set1_epi32(round);
        let vmin = _mm256_set1_epi32(COEFF_MIN);
        let vmax = _mm256_set1_epi32(COEFF_MAX);
        let cnt = _mm_cvtsi32_si128(shift);
        let mut i = 0;
        while i + 8 <= count {
            let l = bytemuck::cast::<[i32; 8], __m256i>(
                <[i32; 8]>::try_from(&levels[i..i + 8]).unwrap(),
            );
            let v = _mm256_add_epi32(_mm256_mullo_epi32(l, vmul), vround);
            let v = _mm256_sra_epi32(v, cnt);
            let v = _mm256_min_epi32(_mm256_max_epi32(v, vmin), vmax);
            let stored: [i32; 8] = bytemuck::cast(v);
            scaled[i..i + 8].copy_from_slice(&stored);
            i += 8;
        }
    }

    /// Flat-matrix dequantiser (`m = 16`), or `false` when the CPU has no AVX2 or the block
    /// could overflow 32 bits (the caller then runs the 64-bit scalar loop). Same value as
    /// `dequant_weighted`.
    #[multiversion::multiversion(targets("x86_64+avx2"))]
    pub(crate) fn dequantize_avx2(
        levels: &[i32],
        scaled: &mut [i32],
        n: usize,
        qp: i32,
        bit_depth: u32,
    ) -> bool {
        let count = n * n;
        if qp < 0 || !count.is_multiple_of(8) {
            return false;
        }
        let log2n = n.trailing_zeros() as i32;
        let bd_shift = bit_depth as i32 + log2n - 5;
        if bd_shift < 1 {
            return false;
        }
        let round = 1i64 << (bd_shift - 1);
        let mul = (16 * i64::from(LEVEL_SCALE[(qp % 6) as usize])) << (qp / 6).min(40);
        let max_abs = levels[..count]
            .iter()
            .map(|l| l.unsigned_abs())
            .max()
            .unwrap_or(0);
        if (u64::from(max_abs) * mul as u64 + round as u64) >= (1u64 << 31) {
            return false;
        }
        assert!(scaled.len() >= count);
        match_target! {
            "x86_64+avx2" => {
                dequant_inner(levels, scaled, count, mul as i32, round as i32, bd_shift);
                true
            }
            _ => false
        }
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;
    use crate::transform::{
        dequant_level_bd, forward_transform_scalar, inverse_transform_scalar, quantize_offset,
    };

    /// The AVX2 kernels run only where the CPU has the feature; the tests skip elsewhere.
    fn has_avx2() -> bool {
        std::is_x86_feature_detected!("avx2")
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 16) as u32
        }
        fn range(&mut self, lo: i32, hi: i32) -> i32 {
            lo + (self.next() % (hi - lo + 1) as u32) as i32
        }
    }

    #[test]
    fn forward_matches_scalar() {
        if !has_avx2() {
            return;
        }
        let mut rng = Rng(0x1234_5678_9abc_def1);
        for iter in 0..400 {
            for &n in &[4usize, 8, 16, 32] {
                for dst in [false, true] {
                    if dst && n != 4 {
                        continue;
                    }
                    let lim = if iter % 2 == 0 { 255 } else { 1023 };
                    let r: Vec<i32> = (0..n * n)
                        .map(|_| match iter % 5 {
                            3 => {
                                if rng.next() & 1 == 0 {
                                    lim
                                } else {
                                    -lim
                                }
                            }
                            4 => rng.range(-3, 3),
                            _ => rng.range(-lim, lim),
                        })
                        .collect();
                    let mut a = vec![0; n * n];
                    let mut b = vec![0; n * n];
                    forward_transform_scalar(&r, &mut a, n, dst);
                    assert!(forward_transform_avx2(&r, &mut b, n, dst));
                    assert_eq!(a, b, "n={n} dst={dst}");
                }
            }
        }
    }

    #[test]
    fn inverse_matches_scalar() {
        if !has_avx2() {
            return;
        }
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for iter in 0..1500 {
            for &n in &[4usize, 8, 16, 32] {
                for dst in [false, true] {
                    if dst && n != 4 {
                        continue;
                    }
                    let rows = rng.range(0, n as i32) as usize;
                    let cols = rng.range(0, n as i32) as usize;
                    let ext = iter % 7 == 0;
                    let c: Vec<i32> = (0..n * n)
                        .map(|i| {
                            let (y, x) = (i / n, i % n);
                            if y >= rows || x >= cols || rng.next().is_multiple_of(3) {
                                0
                            } else if ext {
                                if rng.next() & 1 == 0 { 32767 } else { -32768 }
                            } else if iter % 2 == 0 {
                                rng.range(-300, 300)
                            } else {
                                rng.range(-32768, 32767)
                            }
                        })
                        .collect();
                    for bd in [8u32, 10] {
                        let mut a = vec![0; n * n];
                        let mut b = vec![7; n * n];
                        inverse_transform_scalar(&c, &mut a, n, dst, bd);
                        assert!(inverse_transform_avx2(&c, &mut b, n, dst, 20 - bd as i32));
                        assert_eq!(a, b, "n={n} dst={dst} bd={bd}");
                    }
                }
            }
        }
    }

    #[test]
    fn quant_dequant_match_scalar() {
        if !has_avx2() {
            return;
        }
        let mut rng = Rng(42);
        for _ in 0..300 {
            for &n in &[4usize, 8, 16, 32] {
                for qp in (-12..=51).step_by(3) {
                    for dz in [2i64, 3, 6, 12] {
                        let c: Vec<i32> = (0..n * n)
                            .map(|_| match rng.next() % 4 {
                                0 => rng.range(-32768, 32767),
                                _ => rng.range(-400, 400),
                            })
                            .collect();
                        let mut lv = vec![0; n * n];
                        let nz = quantize_offset(&c, &mut lv, n, qp, dz);
                        let log2n = n.trailing_zeros() as i32;
                        let qbits = 21 + qp.div_euclid(6) - log2n;
                        let scale = [26_214i64, 23_302, 20_560, 18_396, 16_384, 14_564]
                            [qp.rem_euclid(6) as usize];
                        let off = (1i64 << qbits) / dz;
                        let mut want_nz = 0;
                        for (i, &v) in c.iter().enumerate() {
                            let m = ((v.unsigned_abs() as i64 * scale + off) >> qbits) as i32;
                            let m = m.min(32767);
                            let w = if v < 0 { -m } else { m };
                            assert_eq!(lv[i], w, "quant n={n} qp={qp} dz={dz}");
                            want_nz += usize::from(w != 0);
                        }
                        assert_eq!(nz, want_nz);
                    }
                }
                for qp in 0..=75 {
                    for bd in [8u32, 10] {
                        let big = qp % 2 == 0;
                        let lv: Vec<i32> = (0..n * n)
                            .map(|_| {
                                if big {
                                    rng.range(-32768, 32767)
                                } else {
                                    rng.range(-40, 40)
                                }
                            })
                            .collect();
                        let mut out = vec![0; n * n];
                        crate::transform::dequantize_bd(&lv, &mut out, n, qp, bd);
                        for (i, &l) in lv.iter().enumerate() {
                            assert_eq!(out[i], dequant_level_bd(l, n, qp, bd), "dq n={n} qp={qp}");
                        }
                    }
                }
            }
        }
    }

    /// Timing only (`cargo test --release -- --ignored --nocapture simd_tx`).
    #[test]
    #[ignore]
    fn timing() {
        use std::time::Instant;
        let mut rng = Rng(7);
        for &n in &[4usize, 8, 16, 32] {
            let r: Vec<i32> = (0..n * n).map(|_| rng.range(-200, 200)).collect();
            let c: Vec<i32> = (0..n * n)
                .map(|i| {
                    if i / n < 4 && i % n < 4 {
                        rng.range(-500, 500)
                    } else {
                        0
                    }
                })
                .collect();
            let reps = 2_000_000 / (n * n);
            let mut o = vec![0; n * n];
            let t = Instant::now();
            for _ in 0..reps {
                forward_transform_scalar(&r, &mut o, n, false);
                std::hint::black_box(&o);
            }
            let fs = t.elapsed();
            let t = Instant::now();
            for _ in 0..reps {
                forward_transform_avx2(&r, &mut o, n, false);
                std::hint::black_box(&o);
            }
            let fa = t.elapsed();
            let t = Instant::now();
            for _ in 0..reps {
                inverse_transform_scalar(&c, &mut o, n, false, 10);
                std::hint::black_box(&o);
            }
            let is = t.elapsed();
            let t = Instant::now();
            for _ in 0..reps {
                inverse_transform_avx2(&c, &mut o, n, false, 10);
                std::hint::black_box(&o);
            }
            let ia = t.elapsed();
            println!(
                "n={n} fwd scalar {:.0}ns avx2 {:.0}ns | inv(sparse 4x4) scalar {:.0}ns avx2 {:.0}ns",
                fs.as_nanos() as f64 / reps as f64,
                fa.as_nanos() as f64 / reps as f64,
                is.as_nanos() as f64 / reps as f64,
                ia.as_nanos() as f64 / reps as f64
            );
        }
    }
}

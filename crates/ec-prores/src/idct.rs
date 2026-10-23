//! ffmpeg's `ff_prores_idct_10` / `ff_prores_idct_12` (libavcodec/simple_idct.c, the
//! `simple_idct_template.c` integer IDCT), reproduced with its 16-bit intermediates.
//! `W4`-based rounding, the DC-only row shortcut and the `+8192` column bias all matter
//! for byte-exactness.
//!
//! Blocks arrive dequantised and *transposed* (`block[col * 8 + row]`): the entropy decoder
//! scatters coefficients straight into that layout, so the first (row) pass runs with one
//! 8-lane vector per frequency column. [`idct_put`] is the 10-bit SIMD path built on
//! `wide`'s `pmaddwd` (`i16x8::dot`); 12-bit data (4444) goes through the scalar
//! [`reference`] code, which the unit tests also use to check the SIMD path.

use wide::{i16x8, i32x4, i32x8};

/// Scalar implementation in raster layout, a line-for-line port of ffmpeg's.
pub mod reference {
    #![allow(dead_code)]

    pub struct K {
        /// `a` (even part) coefficient rows for r2, r4, r6.
        pub a2: [i32; 4],
        pub a4: [i32; 4],
        pub a6: [i32; 4],
        /// `b` (odd part) coefficient rows for r1, r3, r5, r7.
        pub b1: [i32; 4],
        pub b3: [i32; 4],
        pub b5: [i32; 4],
        pub b7: [i32; 4],
        pub w4: i32,
        pub row_shift: u32,
        pub col_shift: u32,
    }

    const fn k(w1: i32, w2: i32, w3: i32, w4: i32, w5: i32, w6: i32, w7: i32, row_shift: u32, col_shift: u32) -> K {
        K {
            a2: [w2, w6, -w6, -w2],
            a4: [w4, -w4, -w4, w4],
            a6: [w6, -w2, w2, -w6],
            b1: [w1, w3, w5, w7],
            b3: [w3, -w7, -w1, -w5],
            b5: [w5, -w1, w7, w3],
            b7: [w7, -w5, w3, -w1],
            w4,
            row_shift,
            col_shift,
        }
    }

    pub static K10: K = k(22725, 21407, 19265, 16384, 12873, 8867, 4520, 13 + 2, 18);
    pub static K12: K = k(45451, 42813, 38531, 32767, 25746, 17734, 9041, 16, 17);

    #[inline(always)]
    fn row_pass(k: &K, row: &mut [i16; 8]) {
        let r: [i32; 8] = [
            row[0] as i32, row[1] as i32, row[2] as i32, row[3] as i32,
            row[4] as i32, row[5] as i32, row[6] as i32, row[7] as i32,
        ];
        if (r[1] | r[2] | r[3] | r[4] | r[5] | r[6] | r[7]) == 0 {
            // DC only: (dc + 1) >> 1 in both bit depths.
            *row = [(r[0].wrapping_add(1) >> 1) as i16; 8];
            return;
        }
        let base = k.w4.wrapping_mul(r[0]).wrapping_add(1 << (k.row_shift - 1));
        let mut a = [base; 4];
        let mut b = [0i32; 4];
        for j in 0..4 {
            a[j] = a[j]
                .wrapping_add(k.a2[j].wrapping_mul(r[2]))
                .wrapping_add(k.a4[j].wrapping_mul(r[4]))
                .wrapping_add(k.a6[j].wrapping_mul(r[6]));
            b[j] = k.b1[j]
                .wrapping_mul(r[1])
                .wrapping_add(k.b3[j].wrapping_mul(r[3]))
                .wrapping_add(k.b5[j].wrapping_mul(r[5]))
                .wrapping_add(k.b7[j].wrapping_mul(r[7]));
        }
        let s = k.row_shift;
        for j in 0..4 {
            row[j] = (a[j].wrapping_add(b[j]) >> s) as i16;
            row[7 - j] = (a[j].wrapping_sub(b[j]) >> s) as i16;
        }
    }

    /// Column pass over all eight columns; returns the eight output rows (pre-clip).
    #[inline(always)]
    fn col_pass(k: &K, blk: &[i16; 64]) -> [[i16; 8]; 8] {
        let bias = (1i32 << (k.col_shift - 1)) / k.w4;
        let mut c = [[0i32; 8]; 8];
        for r in 0..8 {
            for j in 0..8 {
                c[r][j] = blk[r * 8 + j] as i32;
            }
        }
        let mut a = [[0i32; 8]; 4];
        let mut b = [[0i32; 8]; 4];
        for j in 0..8 {
            // The +8192 lands in the 16-bit first row before the multiply.
            let c0 = (blk[j].wrapping_add(8192)) as i32;
            let base = k.w4.wrapping_mul(c0.wrapping_add(bias));
            for i in 0..4 {
                a[i][j] = base
                    .wrapping_add(k.a2[i].wrapping_mul(c[2][j]))
                    .wrapping_add(k.a4[i].wrapping_mul(c[4][j]))
                    .wrapping_add(k.a6[i].wrapping_mul(c[6][j]));
                b[i][j] = k.b1[i]
                    .wrapping_mul(c[1][j])
                    .wrapping_add(k.b3[i].wrapping_mul(c[3][j]))
                    .wrapping_add(k.b5[i].wrapping_mul(c[5][j]))
                    .wrapping_add(k.b7[i].wrapping_mul(c[7][j]));
            }
        }
        let s = k.col_shift;
        let mut out = [[0i16; 8]; 8];
        for i in 0..4 {
            for j in 0..8 {
                out[i][j] = (a[i][j].wrapping_add(b[i][j]) >> s) as i16;
                out[7 - i][j] = (a[i][j].wrapping_sub(b[i][j]) >> s) as i16;
            }
        }
        out
    }

    /// Run the IDCT on an already dequantised `block` and store the clipped 8x8 result at
    /// `dst[0]`, row pitch `stride`. With `dc_only` only `block[0]` is read (the block's other
    /// coefficients are zero), which is exact: the shortcut rows and columns of the full
    /// computation collapse to one value.
    #[inline(always)]
    pub fn idct_put_ref<const B12: bool>(dst: &mut [u8], stride: usize, block: &mut [i16; 64], dc_only: bool) {
        let k = if B12 { &K12 } else { &K10 };
        let (lo, hi): (i16, i16) = if B12 { (4, 4091) } else { (4, 1019) };
        if dc_only {
            let t = (i32::from(block[0]).wrapping_add(1) >> 1) as i16;
            let c0 = t.wrapping_add(8192) as i32;
            let bias = (1i32 << (k.col_shift - 1)) / k.w4;
            let v = ((k.w4.wrapping_mul(c0.wrapping_add(bias)) >> k.col_shift) as i16).clamp(lo, hi) as u16;
            for r in 0..8 {
                for c in dst[r * stride * 2..r * stride * 2 + 16].as_chunks_mut::<2>().0.iter_mut() {
                    *c = v.to_le_bytes();
                }
            }
            return;
        }
        for r in 0..8 {
            let row: &mut [i16; 8] = (&mut block[r * 8..r * 8 + 8]).try_into().unwrap();
            row_pass(k, row);
        }
        let out = col_pass(k, block);
        for r in 0..8 {
            let d = &mut dst[r * stride * 2..r * stride * 2 + 16];
            for j in 0..8 {
                d[2 * j..2 * j + 2].copy_from_slice(&(out[r][j].clamp(lo, hi) as u16).to_le_bytes());
            }
        }
    }
}

use reference::{K10, K12};
use archmage::{SimdToken, X64V1Token};
use reference::K;

#[inline(always)]
fn pairs(a: i16x8, b: i16x8) -> (i16x8, i16x8) {
    let (a, b) = (a.to_array(), b.to_array());
    (
        i16x8::new([a[0], b[0], a[1], b[1], a[2], b[2], a[3], b[3]]),
        i16x8::new([a[4], b[4], a[5], b[5], a[6], b[6], a[7], b[7]]),
    )
}

#[inline(always)]
fn cpair(x: i32, y: i32) -> i16x8 {
    let (x, y) = (x as i16, y as i16);
    i16x8::new([x, y, x, y, x, y, x, y])
}

/// One 1-D butterfly across eight lanes. `v[k]` is input coefficient `k` for every lane; `base`
/// is the pass constant added to the `W4*v0` term. Returns, per output position, the
/// `(a + b, a - b)` sums in lane halves: `out[i]` = (a_i + b_i) and `out[7 - i]` = (a_i - b_i).
#[inline(always)]
fn butterfly(k: &K, v: [i16x8; 8], base: i32) -> [[i32x4; 2]; 8] {
    let (p04l, p04h) = pairs(v[0], v[4]);
    let (p26l, p26h) = pairs(v[2], v[6]);
    let (p13l, p13h) = pairs(v[1], v[3]);
    let (p57l, p57h) = pairs(v[5], v[7]);
    let base = i32x4::splat(base);
    let mut out = [[i32x4::splat(0); 2]; 8];
    for i in 0..4 {
        let c04 = cpair(k.w4, k.a4[i]);
        let c26 = cpair(k.a2[i], k.a6[i]);
        let c13 = cpair(k.b1[i], k.b3[i]);
        let c57 = cpair(k.b5[i], k.b7[i]);
        let al = p04l.dot(c04) + p26l.dot(c26) + base;
        let ah = p04h.dot(c04) + p26h.dot(c26) + base;
        let bl = p13l.dot(c13) + p57l.dot(c57);
        let bh = p13h.dot(c13) + p57h.dot(c57);
        out[i] = [al + bl, ah + bh];
        out[7 - i] = [al - bl, ah - bh];
    }
    out
}

#[inline(always)]
fn narrow(lo: i32x4, hi: i32x4, shift: u32) -> i16x8 {
    let (l, h) = ((lo >> shift).to_array(), (hi >> shift).to_array());
    i16x8::from_i32x8_truncate(i32x8::new([l[0], l[1], l[2], l[3], h[0], h[1], h[2], h[3]]))
}

#[inline(always)]
fn transpose(m: [i16x8; 8]) -> [i16x8; 8] {
    let a = m.map(|v| v.to_array());
    let mut t = [[0i16; 8]; 8];
    for r in 0..8 {
        for c in 0..8 {
            t[c][r] = a[r][c];
        }
    }
    t.map(i16x8::new)
}


#[cfg(target_arch = "x86_64")]
mod sse {
    use super::reference::{K, K10};
    use archmage::{X64V1Token, arcane};
    use std::arch::x86_64::*;

    #[target_feature(enable = "sse2")]
    fn cpair(x: i32, y: i32) -> __m128i {
        _mm_set1_epi32(((y as i16 as u16 as u32) << 16 | (x as i16 as u16 as u32)) as i32)
    }

    /// See `butterfly` in the portable path: `out[i]` = a_i + b_i, `out[7 - i]` = a_i - b_i,
    /// each as (low, high) halves of the eight lanes.
    #[target_feature(enable = "sse2")]
    #[inline]
    fn butterfly(k: &K, v: [__m128i; 8], base: i32) -> [[__m128i; 2]; 8] {
        let p04l = _mm_unpacklo_epi16(v[0], v[4]);
        let p04h = _mm_unpackhi_epi16(v[0], v[4]);
        let p26l = _mm_unpacklo_epi16(v[2], v[6]);
        let p26h = _mm_unpackhi_epi16(v[2], v[6]);
        let p13l = _mm_unpacklo_epi16(v[1], v[3]);
        let p13h = _mm_unpackhi_epi16(v[1], v[3]);
        let p57l = _mm_unpacklo_epi16(v[5], v[7]);
        let p57h = _mm_unpackhi_epi16(v[5], v[7]);
        let base = _mm_set1_epi32(base);
        let mut out = [[_mm_setzero_si128(); 2]; 8];
        for i in 0..4 {
            let c04 = cpair(k.w4, k.a4[i]);
            let c26 = cpair(k.a2[i], k.a6[i]);
            let c13 = cpair(k.b1[i], k.b3[i]);
            let c57 = cpair(k.b5[i], k.b7[i]);
            let al = _mm_add_epi32(_mm_add_epi32(_mm_madd_epi16(p04l, c04), _mm_madd_epi16(p26l, c26)), base);
            let ah = _mm_add_epi32(_mm_add_epi32(_mm_madd_epi16(p04h, c04), _mm_madd_epi16(p26h, c26)), base);
            let bl = _mm_add_epi32(_mm_madd_epi16(p13l, c13), _mm_madd_epi16(p57l, c57));
            let bh = _mm_add_epi32(_mm_madd_epi16(p13h, c13), _mm_madd_epi16(p57h, c57));
            out[i] = [_mm_add_epi32(al, bl), _mm_add_epi32(ah, bh)];
            out[7 - i] = [_mm_sub_epi32(al, bl), _mm_sub_epi32(ah, bh)];
        }
        out
    }

    /// Arithmetic shift of both halves, truncated to 16 bits (matches `as i16`).
    #[target_feature(enable = "sse2")]
    #[inline]
    fn narrow(lo: __m128i, hi: __m128i, shift: i32) -> __m128i {
        let l = _mm_srai_epi32::<16>(_mm_slli_epi32::<16>(_mm_sra_epi32(lo, _mm_cvtsi32_si128(shift))));
        let h = _mm_srai_epi32::<16>(_mm_slli_epi32::<16>(_mm_sra_epi32(hi, _mm_cvtsi32_si128(shift))));
        _mm_packs_epi32(l, h)
    }

    #[target_feature(enable = "sse2")]
    #[inline]
    fn transpose(m: [__m128i; 8]) -> [__m128i; 8] {
        let a0 = _mm_unpacklo_epi16(m[0], m[1]);
        let a1 = _mm_unpackhi_epi16(m[0], m[1]);
        let a2 = _mm_unpacklo_epi16(m[2], m[3]);
        let a3 = _mm_unpackhi_epi16(m[2], m[3]);
        let a4 = _mm_unpacklo_epi16(m[4], m[5]);
        let a5 = _mm_unpackhi_epi16(m[4], m[5]);
        let a6 = _mm_unpacklo_epi16(m[6], m[7]);
        let a7 = _mm_unpackhi_epi16(m[6], m[7]);
        let b0 = _mm_unpacklo_epi32(a0, a2);
        let b1 = _mm_unpackhi_epi32(a0, a2);
        let b2 = _mm_unpacklo_epi32(a1, a3);
        let b3 = _mm_unpackhi_epi32(a1, a3);
        let b4 = _mm_unpacklo_epi32(a4, a6);
        let b5 = _mm_unpackhi_epi32(a4, a6);
        let b6 = _mm_unpacklo_epi32(a5, a7);
        let b7 = _mm_unpackhi_epi32(a5, a7);
        [
            _mm_unpacklo_epi64(b0, b4),
            _mm_unpackhi_epi64(b0, b4),
            _mm_unpacklo_epi64(b1, b5),
            _mm_unpackhi_epi64(b1, b5),
            _mm_unpacklo_epi64(b2, b6),
            _mm_unpackhi_epi64(b2, b6),
            _mm_unpacklo_epi64(b3, b7),
            _mm_unpackhi_epi64(b3, b7),
        ]
    }

    /// Safe entry: the `X64V1Token` (SSE2, the x86_64 baseline) is what makes the SSE2 body callable.
    #[arcane]
    pub fn idct_put_10(_t: X64V1Token, dst: &mut [u8], stride: usize, block: &[i16; 64]) {
        assert!(dst.len() >= (7 * stride + 8) * 2);
        {
            let k = &K10;
            let v: [__m128i; 8] = bytemuck::cast(*block);
            let o = butterfly(k, v, 1 << (k.row_shift - 1));
            let ac = _mm_or_si128(
                _mm_or_si128(_mm_or_si128(v[1], v[2]), _mm_or_si128(v[3], v[4])),
                _mm_or_si128(_mm_or_si128(v[5], v[6]), v[7]),
            );
            let dc_only = _mm_cmpeq_epi16(ac, _mm_setzero_si128());
            let dc = _mm_add_epi16(_mm_srai_epi16::<1>(v[0]), _mm_and_si128(v[0], _mm_set1_epi16(1)));
            let mut rows = [_mm_setzero_si128(); 8];
            for q in 0..8 {
                let full = narrow(o[q][0], o[q][1], k.row_shift as i32);
                rows[q] = _mm_or_si128(_mm_and_si128(dc_only, dc), _mm_andnot_si128(dc_only, full));
            }
            let mut w = transpose(rows);
            w[0] = _mm_add_epi16(w[0], _mm_set1_epi16(8192));
            let bias = (1i32 << (k.col_shift - 1)) / k.w4;
            let o = butterfly(k, w, k.w4.wrapping_mul(bias));
            let (lo, hi) = (_mm_set1_epi16(4), _mm_set1_epi16(1019));
            for r in 0..8 {
                let px = _mm_min_epi16(_mm_max_epi16(narrow(o[r][0], o[r][1], k.col_shift as i32), lo), hi);
                dst[r * stride * 2..r * stride * 2 + 16].copy_from_slice(&bytemuck::cast::<__m128i, [u8; 16]>(px));
            }
        }
    }
}

/// 10-bit SIMD IDCT of a transposed, dequantised block, stored clipped at `dst`.
#[inline(always)]
fn idct_put_10(dst: &mut [u8], stride: usize, block: &[i16; 64]) {
    let k = &K10;
    let v: [i16x8; 8] = std::array::from_fn(|c| i16x8::new(block[c * 8..c * 8 + 8].try_into().unwrap()));
    // Row pass: lanes are block rows.
    let o = butterfly(k, v, 1 << (k.row_shift - 1));
    let ac = v[1] | v[2] | v[3] | v[4] | v[5] | v[6] | v[7];
    let dc_only = ac.simd_eq(i16x8::splat(0));
    let dc = (v[0] >> 1) + (v[0] & i16x8::splat(1));
    let mut rows = [i16x8::splat(0); 8];
    for p in 0..8 {
        let full = narrow(o[p][0], o[p][1], k.row_shift);
        rows[p] = dc_only.select(dc, full);
    }
    // rows[p] lane i = block'[i][p]; the column pass wants one vector per image row.
    let w = transpose(rows);
    // Column pass: lanes are columns. The +8192 wraps in 16 bits before the multiply.
    let mut w = w;
    w[0] += i16x8::splat(8192);
    let bias = (1i32 << (k.col_shift - 1)) / k.w4;
    let o = butterfly(k, w, k.w4.wrapping_mul(bias));
    let (lo, hi) = (i16x8::splat(4), i16x8::splat(1019));
    for r in 0..8 {
        let px = narrow(o[r][0], o[r][1], k.col_shift).max(lo).min(hi);
        let a = px.to_array();
        let d = &mut dst[r * stride * 2..r * stride * 2 + 16];
        for j in 0..8 {
            d[2 * j..2 * j + 2].copy_from_slice(&(a[j] as u16).to_le_bytes());
        }
    }
}

/// Run the IDCT on a dequantised `block` in transposed layout and store the clipped 8x8 result
/// at `dst[0]`, row pitch `stride`. A block whose AC coefficients are all zero takes a shortcut that reads only `block[0]`, which is exact: the shortcut rows and columns of the full
/// computation collapse to one value.
#[inline(always)]
pub fn idct_put<const B12: bool>(dst: &mut [u8], stride: usize, block: &mut [i16; 64]) {
    let dc_only = block[1..].iter().fold(0, |a, &x| a | x) == 0;
    let k = if B12 { &K12 } else { &K10 };
    let (lo, hi): (i16, i16) = if B12 { (4, 4091) } else { (4, 1019) };
    if dc_only {
        let t = (i32::from(block[0]).wrapping_add(1) >> 1) as i16;
        let c0 = t.wrapping_add(8192) as i32;
        let bias = (1i32 << (k.col_shift - 1)) / k.w4;
        let v = ((k.w4.wrapping_mul(c0.wrapping_add(bias)) >> k.col_shift) as i16).clamp(lo, hi) as u16;
        for r in 0..8 {
            for c in dst[r * stride * 2..r * stride * 2 + 16].as_chunks_mut::<2>().0.iter_mut() {
                *c = v.to_le_bytes();
            }
        }
        return;
    }
    if B12 {
        let mut raster = [0i16; 64];
        for c in 0..8 {
            for r in 0..8 {
                raster[r * 8 + c] = block[c * 8 + r];
            }
        }
        reference::idct_put_ref::<true>(dst, stride, &mut raster, false);
    } else {
        match X64V1Token::summon() {
            Some(t) => sse::idct_put_10(t, dst, stride, block),
            None => idct_put_10(dst, stride, block),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(s: &mut u64) -> u64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }

    fn check<const B12: bool>() {
        let mut s = 0x1234_5678_9abc_def1u64;
        for case in 0..200_000 {
            // Mixed sparsity and amplitude, including values that overflow 16-bit intermediates.
            let mut raster = [0i16; 64];
            let n = (rng(&mut s) % 65) as usize;
            let amp = match case % 4 {
                0 => 40,
                1 => 600,
                2 => 8000,
                _ => 32767,
            };
            for _ in 0..n {
                let at = (rng(&mut s) % 64) as usize;
                let at = if case % 3 == 0 { at % 12 } else { at };
                raster[at] = ((rng(&mut s) % (2 * amp + 1)) as i64 - amp as i64) as i16;
            }
            if case % 11 == 0 {
                raster[1..].fill(0);
            }
            let mut t = [0i16; 64];
            for r in 0..8 {
                for c in 0..8 {
                    t[c * 8 + r] = raster[r * 8 + c];
                }
            }
            let (mut a, mut b) = ([0u8; 128], [0u8; 128]);
            let mut r1 = raster;
            reference::idct_put_ref::<B12>(&mut a, 8, &mut r1, false);
            idct_put::<B12>(&mut b, 8, &mut t);
            assert_eq!(a, b, "case {case} raster {raster:?}");
        }
    }

    #[test]
    fn simd_matches_reference_10() {
        check::<false>();
    }

    #[test]
    fn simd_matches_reference_12() {
        check::<true>();
    }
}

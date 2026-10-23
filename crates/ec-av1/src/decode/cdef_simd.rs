use super::{
    CDEF_BORDER, CDEF_DIRECTIONS, CDEF_PRI_TAPS, CDEF_SEC_TAPS, CDEF_VERY_LARGE,
    cdef_constrain_shift,
};
use std::arch::x86_64::*;

/// Eight `i16` samples starting at `at`, as one 128-bit register.
#[target_feature(enable = "sse2")]
#[inline]
fn load8(w: &[i16], at: usize) -> __m128i {
    let s: [i16; 8] = w[at..at + 8].try_into().unwrap();
    bytemuck::cast(s)
}

/// The window's rows `i` and `i + 1` at one tap offset, as one 256-bit
/// register (low half = row `i`).
#[target_feature(enable = "avx2")]
#[inline]
fn ld2(w: &[i16], off: isize, bstride: isize) -> __m256i {
    let lo = load8(w, off as usize);
    let hi = load8(w, (off + bstride) as usize);
    _mm256_inserti128_si256(_mm256_castsi128_si256(lo), hi, 1)
}

/// Spec 7.15.3's `constrain`, lane-parallel:
/// `sign(diff) * clamp(threshold - (|diff| >> shift), 0, |diff|)`.
/// `_mm256_sign_epi16` reproduces `signum` exactly, zero included (the
/// clamped magnitude is already 0 where `diff` is).
#[target_feature(enable = "avx2")]
#[inline]
fn constrain(p: __m256i, x: __m256i, thr: __m256i, shift: __m128i) -> __m256i {
    let diff = _mm256_sub_epi16(p, x);
    let a = _mm256_abs_epi16(diff);
    let t = _mm256_sub_epi16(thr, _mm256_sra_epi16(a, shift));
    let t = _mm256_min_epi16(_mm256_max_epi16(t, _mm256_setzero_si256()), a);
    _mm256_sign_epi16(t, diff)
}

/// `dst[off..off + 8] += v`, lane-wise on `i32`.
#[target_feature(enable = "avx2")]
#[inline]
fn add8(dst: &mut [i32; 16], off: usize, v: __m256i) {
    let cur: [i32; 8] = dst[off..off + 8].try_into().unwrap();
    let sum: [i32; 8] = bytemuck::cast(_mm256_add_epi32(bytemuck::cast(cur), v));
    dst[off..off + 8].copy_from_slice(&sum);
}

/// `dst[off..off + 4] += v`, lane-wise on `i32`.
#[target_feature(enable = "sse2")]
#[inline]
fn add4(dst: &mut [i32; 16], off: usize, v: __m128i) {
    let cur: [i32; 4] = dst[off..off + 4].try_into().unwrap();
    let sum: [i32; 4] = bytemuck::cast(_mm_add_epi32(bytemuck::cast(cur), v));
    dst[off..off + 4].copy_from_slice(&sum);
}

/// [`super::cdef_find_dir_partials`] in AVX2 (libaom
/// `cdef_find_dir_avx2`'s shape): eight i32 lanes per row instead of the
/// four the auto-vectorised scalar form gets without the feature.
///
/// `win` must hold [`super::cdef_gather`]'s 8x8 window grown by
/// `CDEF_BORDER` at row stride `bstride`.
#[target_feature(enable = "avx2")]
pub fn find_dir_partials_avx2(win: &[i16], bstride: usize, coeff_shift: i32) -> [[i32; 16]; 8] {
    let mut partial = [[0i32; 16]; 8];
    let shift = _mm_cvtsi32_si128(coeff_shift);
    let c128 = _mm256_set1_epi32(128);
    let revidx = _mm256_setr_epi32(7, 6, 5, 4, 3, 2, 1, 0);
    let mut p6 = _mm256_setzero_si256();
    for i in 0..8usize {
        let base = (i + CDEF_BORDER) * bstride + CDEF_BORDER;
        let raw: [i16; 8] = win[base..base + 8].try_into().unwrap();
        let raw: __m128i = bytemuck::cast(raw);
        let row = _mm256_sub_epi32(_mm256_sra_epi32(_mm256_cvtepi16_epi32(raw), shift), c128);
        let rev = _mm256_permutevar8x32_epi32(row, revidx);
        // `hadd(row, row)` is [p0, p1, p0, p1 | p2, p3, p2, p3], so the
        // four pairwise sums are the two lanes' low halves joined.
        let h = _mm256_hadd_epi32(row, row);
        let pair =
            _mm_unpacklo_epi64(_mm256_castsi256_si128(h), _mm256_extracti128_si256(h, 1));
        let rpair = _mm_shuffle_epi32(pair, 0b00_01_10_11);
        let hs = _mm_hadd_epi32(pair, pair);
        let total = _mm_cvtsi128_si32(_mm_hadd_epi32(hs, hs));
        add8(&mut partial[0], i, row);
        add4(&mut partial[1], i, pair);
        partial[2][i] = total;
        add4(&mut partial[3], i, rpair);
        add8(&mut partial[4], i, rev);
        add8(&mut partial[5], 3 - i / 2, row);
        p6 = _mm256_add_epi32(p6, row);
        add8(&mut partial[7], i / 2, row);
    }
    let p6: [i32; 8] = bytemuck::cast(p6);
    partial[6][..8].copy_from_slice(&p6);
    partial
}

/// [`super::cdef_filter_block_scalar`]'s arithmetic, two rows per
/// iteration. `bw` is 8 (luma) or 4 (4:2:0 chroma) and `bh` matches; for
/// `bw == 4` the upper four lanes of each half are window samples read
/// past the row (never past the buffer -- `w` is padded to 256 entries),
/// computed and then dropped by the 64-bit store.
///
/// `src` must be [`super::cdef_gather`]'s 256-entry window buffer (the tap
/// loads read up to 152 entries past its start, i.e. past the
/// `(bw + 4) x (bh + 4)` samples themselves) and `dst` the `bh` rows of
/// `bw` samples at `(ox, oy)`.
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
pub fn filter_block_avx2(
    src: &[i16],
    dst: &mut [u16],
    stride: usize,
    ox: usize,
    oy: usize,
    bw: usize,
    bh: usize,
    pri_strength: i32,
    sec_strength: i32,
    dir: usize,
    pri_damping: i32,
    sec_damping: i32,
    enable_primary: bool,
    enable_secondary: bool,
    coeff_shift: i32,
    smax: i32,
) {
    let bstride = bw + 2 * CDEF_BORDER;
    // The caller's window is already `i16` and 256 entries long, so the
    // kernel taps it in place: the largest two-row load is
    // (bh + 3) * bstride + 8 <= 152 for the 8x8 luma block, 72 for the
    // 4x4 chroma one, both inside the buffer's zeroed tail.
    let w = src;
    let clipping_required = enable_primary && enable_secondary;
    let pri_taps = CDEF_PRI_TAPS[((pri_strength >> coeff_shift) & 1) as usize];
    let pri_shift = _mm_cvtsi32_si128(cdef_constrain_shift(pri_strength.max(1), pri_damping));
    let sec_shift = _mm_cvtsi32_si128(cdef_constrain_shift(sec_strength.max(1), sec_damping));
    let pri_thr = _mm256_set1_epi16(pri_strength as i16);
    let sec_thr = _mm256_set1_epi16(sec_strength as i16);
    let very_large = _mm256_set1_epi16(CDEF_VERY_LARGE as i16);
    let zero = _mm256_setzero_si256();
    let eight = _mm256_set1_epi16(8);
    let smaxv = _mm256_set1_epi16(smax as i16);
    let (pri_dir, sec_dir0, sec_dir1) = (
        CDEF_DIRECTIONS[dir],
        CDEF_DIRECTIONS[(dir + 2) & 7],
        CDEF_DIRECTIONS[(dir + 6) & 7],
    );
    let bs = bstride as isize;
    let mut i = 0usize;
    while i < bh {
        let base = (i + CDEF_BORDER) as isize * bs + CDEF_BORDER as isize;
        let x = ld2(w, base, bs);
        let mut sum = zero;
        let mut max = x;
        let mut min = x;
        for k in 0..2usize {
            if enable_primary {
                let (tr, tc) = pri_dir[k];
                let off = tr as isize * bs + tc as isize;
                let p0 = ld2(w, base + off, bs);
                let p1 = ld2(w, base - off, bs);
                let tap = _mm256_set1_epi16(pri_taps[k] as i16);
                sum = _mm256_add_epi16(
                    sum,
                    _mm256_mullo_epi16(tap, constrain(p0, x, pri_thr, pri_shift)),
                );
                sum = _mm256_add_epi16(
                    sum,
                    _mm256_mullo_epi16(tap, constrain(p1, x, pri_thr, pri_shift)),
                );
                if clipping_required {
                    // The sentinel is excluded from the max exactly as the
                    // scalar `if p != CDEF_VERY_LARGE` does: masking it to
                    // 0 cannot win a max whose seed `x` is non-negative.
                    max = _mm256_max_epi16(
                        max,
                        _mm256_andnot_si256(_mm256_cmpeq_epi16(p0, very_large), p0),
                    );
                    max = _mm256_max_epi16(
                        max,
                        _mm256_andnot_si256(_mm256_cmpeq_epi16(p1, very_large), p1),
                    );
                    min = _mm256_min_epi16(min, _mm256_min_epi16(p0, p1));
                }
            }
            if enable_secondary {
                let (tr0, tc0) = sec_dir0[k];
                let (tr1, tc1) = sec_dir1[k];
                let off0 = tr0 as isize * bs + tc0 as isize;
                let off1 = tr1 as isize * bs + tc1 as isize;
                let s0 = ld2(w, base + off0, bs);
                let s1 = ld2(w, base - off0, bs);
                let s2 = ld2(w, base + off1, bs);
                let s3 = ld2(w, base - off1, bs);
                if clipping_required {
                    for s in [s0, s1, s2, s3] {
                        max = _mm256_max_epi16(
                            max,
                            _mm256_andnot_si256(_mm256_cmpeq_epi16(s, very_large), s),
                        );
                    }
                    min = _mm256_min_epi16(
                        min,
                        _mm256_min_epi16(_mm256_min_epi16(s0, s1), _mm256_min_epi16(s2, s3)),
                    );
                }
                let c = _mm256_add_epi16(
                    _mm256_add_epi16(
                        constrain(s0, x, sec_thr, sec_shift),
                        constrain(s1, x, sec_thr, sec_shift),
                    ),
                    _mm256_add_epi16(
                        constrain(s2, x, sec_thr, sec_shift),
                        constrain(s3, x, sec_thr, sec_shift),
                    ),
                );
                sum = _mm256_add_epi16(
                    sum,
                    _mm256_mullo_epi16(_mm256_set1_epi16(CDEF_SEC_TAPS[k] as i16), c),
                );
            }
        }
        // `-1` where `sum < 0`, i.e. the scalar `- i32::from(sum < 0)`.
        let neg = _mm256_cmpgt_epi16(zero, sum);
        let mut y = _mm256_add_epi16(
            x,
            _mm256_srai_epi16(_mm256_add_epi16(_mm256_add_epi16(eight, sum), neg), 4),
        );
        if clipping_required {
            y = _mm256_min_epi16(_mm256_max_epi16(y, min), max);
        }
        y = _mm256_min_epi16(_mm256_max_epi16(y, zero), smaxv);
        let (lo, hi) = (_mm256_castsi256_si128(y), _mm256_extracti128_si256(y, 1));
        let o0 = (oy + i) * stride + ox;
        let o1 = o0 + stride;
        // Stores as the original 128-bit and 64-bit writes: 8 samples for
        // luma, the low 4 for 4-wide chroma.
        let lo: [u16; 8] = bytemuck::cast(lo);
        let hi: [u16; 8] = bytemuck::cast(hi);
        if bw == 8 {
            dst[o0..o0 + 8].copy_from_slice(&lo);
            dst[o1..o1 + 8].copy_from_slice(&hi);
        } else {
            dst[o0..o0 + 4].copy_from_slice(&lo[..4]);
            dst[o1..o1 + 4].copy_from_slice(&hi[..4]);
        }
        i += 2;
    }
}

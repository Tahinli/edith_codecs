use std::arch::x86_64::*;

/// Bounds-checked unaligned load of 16 `u16` samples at `at`.
#[inline]
fn load(data: &[u16], at: usize) -> __m256i {
    let a: [u16; 16] = data[at..at + 16].try_into().unwrap();
    bytemuck::cast(a)
}

/// Bounds-checked unaligned store of 16 `u16` samples at `at`.
#[inline]
fn store(data: &mut [u16], at: usize, v: __m256i) {
    let a: [u16; 16] = bytemuck::cast(v);
    data[at..at + 16].copy_from_slice(&a);
}

/// `a <= b` lane-wise, as a full-width mask.
///
/// # Safety
/// Requires AVX2.
#[target_feature(enable = "avx2")]
#[inline]
fn le(a: __m256i, b: __m256i) -> __m256i {
    _mm256_cmpeq_epi16(_mm256_max_epi16(a, b), b)
}

/// `|a - b|` lane-wise.
///
/// # Safety
/// Requires AVX2.
#[target_feature(enable = "avx2")]
#[inline]
fn ad(a: __m256i, b: __m256i) -> __m256i {
    _mm256_abs_epi16(_mm256_sub_epi16(a, b))
}

/// `signed_char_clamp_high`, lane-wise.
///
/// # Safety
/// Requires AVX2.
#[target_feature(enable = "avx2")]
#[inline]
fn sclamp(v: __m256i, lo: __m256i, hi: __m256i) -> __m256i {
    _mm256_min_epi16(_mm256_max_epi16(v, lo), hi)
}

/// 16 rows of 16 `i16` transposed in place (unpack ladder, then the
/// 128-bit lane swap AVX2's in-lane unpacks leave behind).
///
/// # Safety
/// Requires AVX2.
#[target_feature(enable = "avx2")]
fn transpose16(t: &mut [__m256i; 16]) {
    let mut a = [_mm256_setzero_si256(); 16];
    for k in 0..8 {
        a[2 * k] = _mm256_unpacklo_epi16(t[2 * k], t[2 * k + 1]);
        a[2 * k + 1] = _mm256_unpackhi_epi16(t[2 * k], t[2 * k + 1]);
    }
    let mut b = [_mm256_setzero_si256(); 16];
    for g in 0..4 {
        let s = 4 * g;
        b[s] = _mm256_unpacklo_epi32(a[s], a[s + 2]);
        b[s + 1] = _mm256_unpackhi_epi32(a[s], a[s + 2]);
        b[s + 2] = _mm256_unpacklo_epi32(a[s + 1], a[s + 3]);
        b[s + 3] = _mm256_unpackhi_epi32(a[s + 1], a[s + 3]);
    }
    let mut c = [_mm256_setzero_si256(); 16];
    for g in 0..2 {
        let s = 8 * g;
        for j in 0..4 {
            c[s + 2 * j] = _mm256_unpacklo_epi64(b[s + j], b[s + j + 4]);
            c[s + 2 * j + 1] = _mm256_unpackhi_epi64(b[s + j], b[s + j + 4]);
        }
    }
    for i in 0..8 {
        t[i] = _mm256_permute2x128_si256(c[i], c[i + 8], 0x20);
        t[i + 8] = _mm256_permute2x128_si256(c[i], c[i + 8], 0x31);
    }
}

/// The wide kernels' tap weights, in tap order and with the spec's
/// rounding divisor as the row sum: `filter6` over `p2..q2`, `filter8`
/// over `p3..q3`, `filter14` over `p6..q6`.
const W6: [[i16; 6]; 4] = [
    [3, 2, 2, 1, 0, 0],
    [1, 2, 2, 2, 1, 0],
    [0, 1, 2, 2, 2, 1],
    [0, 0, 1, 2, 2, 3],
];
const W8: [[i16; 8]; 6] = [
    [3, 2, 1, 1, 1, 0, 0, 0],
    [2, 1, 2, 1, 1, 1, 0, 0],
    [1, 1, 1, 2, 1, 1, 1, 0],
    [0, 1, 1, 1, 2, 1, 1, 1],
    [0, 0, 1, 1, 1, 2, 1, 2],
    [0, 0, 0, 1, 1, 1, 2, 3],
];
#[allow(dead_code)] // superseded by the running-sum rows; kept as the weights' statement
const W14: [[i16; 14]; 12] = [
    [7, 2, 2, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0],
    [5, 2, 2, 2, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0],
    [4, 1, 2, 2, 2, 1, 1, 1, 1, 1, 0, 0, 0, 0],
    [3, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 0, 0, 0],
    [2, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 0, 0],
    [1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 0],
    [0, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1],
    [0, 0, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 2],
    [0, 0, 0, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 3],
    [0, 0, 0, 0, 1, 1, 1, 1, 1, 2, 2, 2, 1, 4],
    [0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 2, 2, 2, 5],
    [0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 2, 2, 7],
];

/// One weighted tap sum, rounded by `shift` (3 for the divide-by-8 rows,
/// 4 for `filter14`'s divide-by-16 ones). The sum is non-negative and
/// below `1 << 16`, so the logical shift is the arithmetic one.
///
/// # Safety
/// Requires AVX2.
///
/// lane-deblock: the tap count, the weights and the shift are all const
/// here (they were slice parameters, which left the weight `match` as a
/// runtime branch and a `movzwl` per tap -- `testl`/`je`/`movzwl` were
/// 34% of this kernel's samples, and the spilled tap slices another 17%).
#[target_feature(enable = "avx2")]
#[inline] // rust#145574: `inline(always)` is rejected on a `target_feature` fn
fn wsum<const N: usize, const SHIFT: i32>(taps: &[__m256i; N], w: &[i16; N]) -> __m256i {
    let mut s = _mm256_setzero_si256();
    for i in 0..N {
        match w[i] {
            0 => {}
            1 => s = _mm256_add_epi16(s, taps[i]),
            2 => s = _mm256_add_epi16(s, _mm256_add_epi16(taps[i], taps[i])),
            k => s = _mm256_add_epi16(s, _mm256_mullo_epi16(taps[i], _mm256_set1_epi16(k))),
        }
    }
    let s = _mm256_add_epi16(s, _mm256_set1_epi16(1 << (SHIFT - 1)));
    if SHIFT == 3 {
        _mm256_srli_epi16(s, 3)
    } else {
        _mm256_srli_epi16(s, 4)
    }
}

/// [`super::filter_edge`]'s body for 16 lines held as `t[0..16]`, `q0` in
/// `t[8]`; the filtered taps are written back in place. Returns `false`
/// when the filter mask is empty on every one of the 16 lines -- nothing
/// was written and the caller can skip the whole write-back.
///
/// # Safety
/// Requires AVX2.
///
/// lane-deblock2: `LEN` is a const parameter, so each edge length gets
/// its own kernel holding only its own taps, and every wide row is
/// clamped straight into `t` instead of into a shared twelve-entry
/// result array (which the register allocator spilled -- 35% of this
/// kernel's samples were `vmovdqa` to the stack).
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
fn filter16<const LEN: u8>(
    t: &mut [__m256i; 16],
    blimit: i32,
    limit: i32,
    hev_thr: i32,
    flat_thresh: i32,
    scale: i32,
    smax: i32,
) -> bool {
    let lim = _mm256_set1_epi16(limit as i16);
    let blim = _mm256_set1_epi16(blimit as i16);
    let hthr = _mm256_set1_epi16(hev_thr as i16);
    let fthr = _mm256_set1_epi16(flat_thresh as i16);
    let zero = _mm256_setzero_si256();
    let smaxv = _mm256_set1_epi16(smax as i16);
    macro_rules! put {
        ($i:expr, $v:expr) => {
            t[$i] = _mm256_min_epi16(_mm256_max_epi16($v, zero), smaxv);
        };
    }
    let (p6, p5, p4, p3, p2, p1, p0) = (t[1], t[2], t[3], t[4], t[5], t[6], t[7]);
    let (q0, q1, q2, q3, q4, q5, q6) = (t[8], t[9], t[10], t[11], t[12], t[13], t[14]);

    // `highbd_filter_mask`: the neighbour steps this kernel's length
    // reaches, plus the shared `blimit` term.
    let d1 = _mm256_max_epi16(ad(p1, p0), ad(q1, q0));
    let mut md = d1;
    if LEN >= 6 {
        md = _mm256_max_epi16(md, _mm256_max_epi16(ad(p2, p1), ad(q2, q1)));
    }
    if LEN >= 8 {
        md = _mm256_max_epi16(md, _mm256_max_epi16(ad(p3, p2), ad(q3, q2)));
    }
    let bl = _mm256_add_epi16(
        _mm256_slli_epi16(ad(p0, q0), 1),
        _mm256_srli_epi16(ad(p1, q1), 1),
    );
    let mask = _mm256_and_si256(le(md, lim), le(bl, blim));
    // lane-deblock2: an empty mask leaves every tap at its loaded value
    // (the flat masks are `& mask`, so they are empty too, and the final
    // clamp is the identity on samples that came out of the plane), so
    // the whole write-back -- including the vertical edge's second
    // transpose -- is dead.
    if _mm256_testz_si256(mask, mask) != 0 {
        return false;
    }
    let hev = _mm256_cmpgt_epi16(d1, hthr);

    // `filter4`, in the centred domain.
    let centre = _mm256_set1_epi16((128 * scale) as i16);
    let lo = _mm256_set1_epi16((-128 * scale) as i16);
    let hi = _mm256_set1_epi16((128 * scale - 1) as i16);
    let ps1 = _mm256_sub_epi16(p1, centre);
    let ps0 = _mm256_sub_epi16(p0, centre);
    let qs0 = _mm256_sub_epi16(q0, centre);
    let qs1 = _mm256_sub_epi16(q1, centre);
    let outer = _mm256_and_si256(hev, sclamp(_mm256_sub_epi16(ps1, qs1), lo, hi));
    let f = sclamp(
        _mm256_add_epi16(
            outer,
            _mm256_mullo_epi16(_mm256_set1_epi16(3), _mm256_sub_epi16(qs0, ps0)),
        ),
        lo,
        hi,
    );
    let f1 = _mm256_srai_epi16(sclamp(_mm256_add_epi16(f, _mm256_set1_epi16(4)), lo, hi), 3);
    let f2 = _mm256_srai_epi16(sclamp(_mm256_add_epi16(f, _mm256_set1_epi16(3)), lo, hi), 3);
    let tap = _mm256_andnot_si256(
        hev,
        _mm256_srai_epi16(_mm256_add_epi16(f1, _mm256_set1_epi16(1)), 1),
    );
    let n_p1 = _mm256_add_epi16(sclamp(_mm256_add_epi16(ps1, tap), lo, hi), centre);
    let n_p0 = _mm256_add_epi16(sclamp(_mm256_add_epi16(ps0, f2), lo, hi), centre);
    let n_q0 = _mm256_add_epi16(sclamp(_mm256_sub_epi16(qs0, f1), lo, hi), centre);
    let n_q1 = _mm256_add_epi16(sclamp(_mm256_sub_epi16(qs1, tap), lo, hi), centre);
    let b_p1 = _mm256_blendv_epi8(p1, n_p1, mask);
    let b_p0 = _mm256_blendv_epi8(p0, n_p0, mask);
    let b_q0 = _mm256_blendv_epi8(q0, n_q0, mask);
    let b_q1 = _mm256_blendv_epi8(q1, n_q1, mask);
    if LEN == 4 {
        put!(6, b_p1);
        put!(7, b_p0);
        put!(8, b_q0);
        put!(9, b_q1);
        return true;
    }

    // `highbd_flat_mask4`, over the same reach as this length's mask.
    let mut fl = _mm256_max_epi16(
        _mm256_max_epi16(ad(p1, p0), ad(q1, q0)),
        _mm256_max_epi16(ad(p2, p0), ad(q2, q0)),
    );
    if LEN >= 8 {
        fl = _mm256_max_epi16(fl, _mm256_max_epi16(ad(p3, p0), ad(q3, q0)));
    }
    let flat = _mm256_and_si256(le(fl, fthr), mask);
    let any_flat = _mm256_testz_si256(flat, flat) == 0;
    if LEN == 6 {
        if any_flat {
            let taps = [p2, p1, p0, q0, q1, q2];
            put!(
                6,
                _mm256_blendv_epi8(b_p1, wsum::<_, 3>(&taps, &W6[0]), flat)
            );
            put!(
                7,
                _mm256_blendv_epi8(b_p0, wsum::<_, 3>(&taps, &W6[1]), flat)
            );
            put!(
                8,
                _mm256_blendv_epi8(b_q0, wsum::<_, 3>(&taps, &W6[2]), flat)
            );
            put!(
                9,
                _mm256_blendv_epi8(b_q1, wsum::<_, 3>(&taps, &W6[3]), flat)
            );
        } else {
            put!(6, b_p1);
            put!(7, b_p0);
            put!(8, b_q0);
            put!(9, b_q1);
        }
        return true;
    }

    // `filter8`'s six rows over `p2..q2`, the wide kernel's base.
    let (o0, o1, o2, o3, o4, o5) = if any_flat {
        let taps = [p3, p2, p1, p0, q0, q1, q2, q3];
        (
            _mm256_blendv_epi8(p2, wsum::<_, 3>(&taps, &W8[0]), flat),
            _mm256_blendv_epi8(b_p1, wsum::<_, 3>(&taps, &W8[1]), flat),
            _mm256_blendv_epi8(b_p0, wsum::<_, 3>(&taps, &W8[2]), flat),
            _mm256_blendv_epi8(b_q0, wsum::<_, 3>(&taps, &W8[3]), flat),
            _mm256_blendv_epi8(b_q1, wsum::<_, 3>(&taps, &W8[4]), flat),
            _mm256_blendv_epi8(q2, wsum::<_, 3>(&taps, &W8[5]), flat),
        )
    } else {
        (p2, b_p1, b_p0, b_q0, b_q1, q2)
    };
    macro_rules! put8 {
        () => {
            put!(5, o0);
            put!(6, o1);
            put!(7, o2);
            put!(8, o3);
            put!(9, o4);
            put!(10, o5);
        };
    }
    if LEN == 8 {
        put8!();
        return true;
    }

    let fl2 = _mm256_max_epi16(
        _mm256_max_epi16(
            _mm256_max_epi16(ad(p4, p0), ad(q4, q0)),
            _mm256_max_epi16(ad(p5, p0), ad(q5, q0)),
        ),
        _mm256_max_epi16(ad(p6, p0), ad(q6, q0)),
    );
    let flat2 = _mm256_and_si256(le(fl2, fthr), flat);
    if _mm256_testz_si256(flat2, flat2) != 0 {
        // No line takes the wide rows: `t[2..5]`/`t[11..14]` keep the
        // taps they were loaded with, exactly as the blend would.
        put8!();
        return true;
    }
    // lane-deblock: `filter14`'s twelve rows as a RUNNING sum.
    // Consecutive `W14` rows differ by three or four taps (row j+1 - row
    // j is a constant vector with two negative and two positive
    // entries), so each output costs one add, one sub and the rounding
    // shift instead of a fresh 14-tap weighted sum -- 168 tap operations
    // become 44. Exact: every partial sum is taken mod 2^16 by
    // `add/sub_epi16` and every ROW sum is a real `W14` row, at most
    // `16 * 4095 + 8 < 2^16`, so the unsigned rounding shift sees the
    // true value. `deblock_simd_kernel_matches_the_scalar_filter` pins
    // it against `filter_edge` at 8/10/12-bit.
    let mut s = _mm256_add_epi16(
        _mm256_add_epi16(
            _mm256_mullo_epi16(p6, _mm256_set1_epi16(7)),
            _mm256_slli_epi16(_mm256_add_epi16(p5, p4), 1),
        ),
        _mm256_add_epi16(
            _mm256_add_epi16(_mm256_add_epi16(p3, p2), _mm256_add_epi16(p1, p0)),
            q0,
        ),
    );
    let add2 = |a, b| _mm256_add_epi16(a, b);
    macro_rules! row14 {
        ($idx:literal, $base:expr) => {
            put!(
                $idx,
                _mm256_blendv_epi8(
                    $base,
                    _mm256_srli_epi16(_mm256_add_epi16(s, _mm256_set1_epi16(8)), 4),
                    flat2,
                )
            );
        };
        ($idx:literal, $base:expr, $plus:expr, $minus:expr) => {
            s = _mm256_sub_epi16(_mm256_add_epi16(s, $plus), $minus);
            row14!($idx, $base);
        };
    }
    row14!(2, p5);
    row14!(3, p4, add2(p3, q1), _mm256_slli_epi16(p6, 1));
    row14!(4, p3, add2(p2, q2), add2(p6, p5));
    row14!(5, o0, add2(p1, q3), add2(p6, p4));
    row14!(6, o1, add2(p0, q4), add2(p6, p3));
    row14!(7, o2, add2(q0, q5), add2(p6, p2));
    row14!(8, o3, add2(q1, q6), add2(p6, p1));
    row14!(9, o4, add2(q2, q6), add2(p5, p0));
    row14!(10, o5, add2(q3, q6), add2(p4, q0));
    row14!(11, q3, add2(q4, q6), add2(p3, q1));
    row14!(12, q4, add2(q5, q6), add2(p2, q2));
    row14!(13, q5, _mm256_slli_epi16(q6, 1), add2(p1, q3));
    true
}

/// 16 edge lines at `base`. `vertical` names the edge: a vertical edge's
/// 16 lines are 16 consecutive rows and its taps are consecutive samples
/// (loaded as a 16x16 tile and transposed), a horizontal edge's are 16
/// consecutive columns with each tap a whole row segment.
///
/// # Safety
/// Requires AVX2. For a vertical edge `base % stride >= 8`, `base %
/// stride + 8 <= stride` and 16 rows from `base` must be inside `data`;
/// for a horizontal edge `base >= 8 * stride` and `base + 8 * stride`
/// must be inside it.
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
pub fn edge16(
    data: &mut [u16],
    base: usize,
    stride: usize,
    vertical: bool,
    len: u8,
    blimit: i32,
    limit: i32,
    hev_thr: i32,
    flat_thresh: i32,
    scale: i32,
    smax: i32,
) {
        let mut t = [_mm256_setzero_si256(); 16];
    // The taps this length reads, as `t` indices.
    let (lo, hi) = match len {
        4 => (6, 10),
        6 => (5, 11),
        8 => (4, 12),
        _ => (1, 15),
    };
    if vertical {
        // Index loop mirrors the spec's per-position form over several offsets; an iterator would obscure it.
        #[allow(clippy::needless_range_loop)]
        for j in 0..16 {
            t[j] = load(data, base + j * stride - 8);
        }
        transpose16(&mut t);
    } else {
        let top = base - 8 * stride;
        // Index loop mirrors the spec's per-position form over several offsets; an iterator would obscure it.
        #[allow(clippy::needless_range_loop)]
        for i in lo..hi {
            t[i] = load(data, top + i * stride);
        }
    }
    // lane-deblock2: one monomorphised kernel per edge length, and an
    // empty filter mask returns without any write-back at all.
    let modified = match len {
        4 => filter16::<4>(&mut t, blimit, limit, hev_thr, flat_thresh, scale, smax),
        6 => filter16::<6>(&mut t, blimit, limit, hev_thr, flat_thresh, scale, smax),
        8 => filter16::<8>(&mut t, blimit, limit, hev_thr, flat_thresh, scale, smax),
        _ => filter16::<14>(&mut t, blimit, limit, hev_thr, flat_thresh, scale, smax),
    };
    if !modified {
        return;
    }
    if vertical {
        transpose16(&mut t);
        // Index loop mirrors the spec's per-position form over several offsets; an iterator would obscure it.
        #[allow(clippy::needless_range_loop)]
        for j in 0..16 {
            store(data, base + j * stride - 8, t[j]);
        }
    } else {
        let top = base - 8 * stride;
        let (a, b) = match len {
            4 | 6 => (6, 10),
            8 => (5, 11),
            _ => (2, 14),
        };
        // Index loop mirrors the spec's per-position form over several offsets; an iterator would obscure it.
        #[allow(clippy::needless_range_loop)]
        for i in a..b {
            store(data, top + i * stride, t[i]);
        }
    }
}

//! Motion-compensation inter predictors: reference border extension, 6-tap
//! luma prediction, bilinear chroma prediction, and the UMV-border motion
//! vector clamps.
//!
//! Normative source: [RFC 6386] §18.3 (sub-pixel interpolation, two
//! separable 1-D passes, each sample `clamp255((a + 64) >> 7)`). The loop
//! structure, intermediate widths and the border clamps follow the libvpx
//! reference implementation (`vp8/common/filter.c`, `vp8/common/
//! reconinter.c`, `vp8/common/extend.c`) sample-for-sample; the per-pass
//! 0..=255 clamping of the 6-tap intermediate is mandated by the RFC's
//! `interp()` and is what the integer test vectors below pin down.
//!
//! Reference planes arrive already border-extended by [`extend_plane`]
//! (edge replication, `border` = 32 like libvpx's `VP8BORDERINPIXELS`);
//! `src` starts at the first byte of the bordered buffer (top-left border
//! corner) and visible pixel `(px, py)` therefore lives at
//! `src[(py + border) * stride + (px + border)]`.
//!
//! Motion vectors are `(row, col)` pairs in eighth-pel units (`i16`), and
//! the C arithmetic right shifts / two's-complement `& 7` phase extraction
//! are reproduced exactly (`i32 >> 3` and `& 7` on negatives).
//!
//! [RFC 6386]: https://www.rfc-editor.org/rfc/rfc6386

/// `VP8_FILTER_WEIGHT` (filter.c): tap weights sum to this, DC passes.
const VP8_FILTER_WEIGHT: i32 = 128;
/// `VP8_FILTER_SHIFT` (filter.c): normalize the 7-bit-precision result.
const VP8_FILTER_SHIFT: i32 = 7;

/// `vp8_sub_pel_filters[8][6]` (filter.c), indexed by the 0..8 sub-pel phase.
const SUB_PEL_FILTERS: [[i32; 6]; 8] = [
    [0, 0, 128, 0, 0, 0],   // degenerate whole-pel (RFC 6386 §18.3 "filters[0]")
    [0, -6, 123, 12, -1, 0],
    [2, -11, 108, 36, -8, 1],
    [0, -9, 93, 50, -6, 0],
    [3, -16, 77, 77, -16, 3],
    [0, -6, 50, 93, -9, 0],
    [1, -8, 36, 108, -11, 2],
    [0, -1, 12, 123, -6, 0],
];

/// `vp8_bilinear_filters[8][2]` (filter.c); chroma prediction is always
/// bilinear (RFC 6386 §19.2).
const BILINEAR_FILTERS: [[i32; 2]; 8] = [
    [128, 0],
    [112, 16],
    [96, 32],
    [80, 48],
    [64, 64],
    [48, 80],
    [32, 96],
    [16, 112],
];

#[inline]
fn clamp255(v: i32) -> i32 {
    if v < 0 {
        0
    } else if v > 255 {
        255
    } else {
        v
    }
}

/// Extends the visible `vis_w` x `vis_h` region of `plane` into the `border`
/// ring on all four sides by edge replication: every out-of-image sample
/// becomes the nearest in-image sample.
///
/// Port of `copy_and_extend_plane` (extend.c) with `et = el = eb = er =
/// border` and `interleave_step = 1`, in place: first the left/right border
/// of every visible row is filled from the row's first/last column, then the
/// (already side-extended) top and bottom rows are copied into the top and
/// bottom borders — which gives the corners their corner pixels, matching
/// the C exactly.
pub(crate) fn extend_plane(plane: &mut [u8], stride: usize, vis_w: usize, vis_h: usize, border: usize) {
    debug_assert!(vis_w >= 1 && vis_h >= 1);
    debug_assert!(stride >= border * 2 + vis_w);

    let line = border + vis_w + border;

    // extend.c: "copy the left and right most columns out".
    for r in 0..vis_h {
        let base = (border + r) * stride + border;
        let first = plane[base];
        let last = plane[base + vis_w - 1];
        plane[base - border..base].fill(first);
        plane[base + vis_w..base + vis_w + border].fill(last);
    }

    // extend.c: "Now copy the top and bottom lines into each line of the
    // respective borders" — src rows are the already side-extended lines.
    let top = border * stride;
    let bottom = (border + vis_h - 1) * stride;
    for i in 0..border {
        plane.copy_within(top..top + line, (border - 1 - i) * stride);
        plane.copy_within(bottom..bottom + line, (border + vis_h + i) * stride);
    }
}

/// Luma predictor for one `bw` x `bh` block (`bw`,`bh` in 4..=16, multiples
/// of 4) whose visible-image top-left pixel is `(x, y)`.
///
/// Semantics of `vp8_build_inter_predictors_b` /
/// `vp8_build_inter16x16_predictors_mby` + `vp8_sixtap_predict*` (reconinter.c,
/// filter.c): full-pel start `(x + (mv.1 >> 3), y + (mv.0 >> 3))`, phases
/// `fx = mv.1 & 7`, `fy = mv.0 & 7`; a whole-pel MV is a plain copy,
/// otherwise the separable 6-tap filter runs horizontally into an `i32`
/// intermediate — clamped to 0..=255 per pass like libvpx's `int` `FData` —
/// then vertically, both passes computing `(dot + 64) >> 7` and clamping.
///
/// When `need_clamp` is set, the MV is first passed through
/// [`clamp_mv_to_umv_border`].
pub(crate) fn predict_luma(
    src: &[u8],
    stride: usize,
    border: usize,
    vis_w: usize,
    vis_h: usize,
    x: usize,
    y: usize,
    mv: (i16, i16),
    need_clamp: bool,
    dst: &mut [u8],
    dst_stride: usize,
    bw: usize,
    bh: usize,
    bilinear: bool,
) {
    debug_assert!(bw >= 4 && bw <= 16 && bw % 4 == 0);
    debug_assert!(bh >= 4 && bh <= 16 && bh % 4 == 0);

    let mut mv = (i32::from(mv.0), i32::from(mv.1));
    if need_clamp {
        clamp_mv_to_umv_border(&mut mv, x >> 4, y >> 4, vis_w, vis_h);
    }

    let fx = (mv.1 & 7) as usize;
    let fy = (mv.0 & 7) as usize;
    let sx = border as i32 + x as i32 + (mv.1 >> 3);
    let sy = border as i32 + y as i32 + (mv.0 >> 3);
    // 6-tap reads reach 2 before / 3 after the start; the libvpx clamp
    // arithmetic keeps those inside the 32-px border.
    debug_assert!(sx >= 2 && sy >= 2, "reference read outside bordered plane");
    let (sx, sy) = (sx as usize, sy as usize);

    if bilinear {
        bilinear_block(src, stride, sx, sy, fx, fy, dst, dst_stride, bw, bh);
    } else {
        sixtap_block(src, stride, sx, sy, fx, fy, dst, dst_stride, bw, bh);
    }
}

/// Six-tap separable sub-pel predictor (libvpx vp8_sixtap_predict*,
/// filter.c). `(sx, sy)` is the full-pel block origin in the bordered
/// reference, `(fx, fy)` the sub-pel phases 0..7 (0 = copy).
///
/// Both passes share one clamped intermediate buffer: the first pass is
/// clamped to 0..=255 per pass (libvpx `clamp_pixel`), so it is stored as
/// `u8` at a fixed 16-lane stride. The AVX2 kernels always compute all 16
/// lanes of an intermediate row (reads stay inside the 32-px border for
/// every block shape) and the vertical pass discards the tail lanes with
/// its `bw`-byte store; the scalar reference only fills the first `bw`.
fn sixtap_block(
    src: &[u8],
    stride: usize,
    sx: usize,
    sy: usize,
    fx: usize,
    fy: usize,
    dst: &mut [u8],
    dst_stride: usize,
    bw: usize,
    bh: usize,
) {
    if fx == 0 && fy == 0 {
        // reconinter.c build_inter_predictors_b / vp8_copy_mem*.
        for r in 0..bh {
            let s = (sy + r) * stride + sx;
            dst[r * dst_stride..r * dst_stride + bw].copy_from_slice(&src[s..s + bw]);
        }
        return;
    }

    let hf = &SUB_PEL_FILTERS[fx];
    let vf = &SUB_PEL_FILTERS[fy];
    let rows = bh + 5;
    let mut tmp = [0u8; 21 * 16]; // C: int FData[21 * 24], clamped to u8

    #[cfg(target_arch = "x86_64")]
    {
        if avx2_supported() {
            // SAFETY: AVX2 availability checked by `avx2_supported` just
            // above; the border/bounds contracts of both kernels hold for
            // every caller (`bw`/`bh` in 4..=16, bordered source plane).
            #[allow(unsafe_code)]
            unsafe {
                simd::sixtap_h(src, stride, sx, sy, hf, &mut tmp, rows);
                simd::sixtap_v(&tmp, vf, dst, dst_stride, bw, bh);
            }
            return;
        }
    }

    sixtap_h_scalar(src, stride, sx, sy, hf, &mut tmp, bw, rows);
    sixtap_v_scalar(&tmp, vf, dst, dst_stride, bw, bh);
}

/// Scalar first pass of [`sixtap_block`] (filter.c
/// `filter_block2d_first_pass` on `src - 2*stride`): `rows` = `bh + 5`
/// horizontal lines, each sample `(dot + 64) >> 7` clamped to 0..=255.
/// Phase 0 is the exact identity (`(p * 128 + 64) >> 7 == p`), so the
/// uniform loop reproduces the C for every phase combination.
fn sixtap_h_scalar(
    src: &[u8],
    stride: usize,
    sx: usize,
    sy: usize,
    hf: &[i32; 6],
    tmp: &mut [u8; 21 * 16],
    bw: usize,
    rows: usize,
) {
    for i in 0..rows {
        let row = (sy + i - 2) * stride + sx - 2;
        for j in 0..bw {
            let p = row + j;
            let acc = i32::from(src[p]) * hf[0]
                + i32::from(src[p + 1]) * hf[1]
                + i32::from(src[p + 2]) * hf[2]
                + i32::from(src[p + 3]) * hf[3]
                + i32::from(src[p + 4]) * hf[4]
                + i32::from(src[p + 5]) * hf[5]
                + (VP8_FILTER_WEIGHT >> 1); /* Rounding */
            tmp[i * 16 + j] = clamp255(acc >> VP8_FILTER_SHIFT) as u8;
        }
    }
}

/// Scalar second pass of [`sixtap_block`] (filter.c
/// `filter_block2d_second_pass` on `FData + 2*bw`): taps read the
/// intermediate rows `r .. r + 5` at the fixed 16-lane stride.
fn sixtap_v_scalar(
    tmp: &[u8; 21 * 16],
    vf: &[i32; 6],
    dst: &mut [u8],
    dst_stride: usize,
    bw: usize,
    bh: usize,
) {
    for r in 0..bh {
        for c in 0..bw {
            let acc = i32::from(tmp[r * 16 + c]) * vf[0]
                + i32::from(tmp[(r + 1) * 16 + c]) * vf[1]
                + i32::from(tmp[(r + 2) * 16 + c]) * vf[2]
                + i32::from(tmp[(r + 3) * 16 + c]) * vf[3]
                + i32::from(tmp[(r + 4) * 16 + c]) * vf[4]
                + i32::from(tmp[(r + 5) * 16 + c]) * vf[5]
                + (VP8_FILTER_WEIGHT >> 1); /* Rounding */
            dst[r * dst_stride + c] = clamp255(acc >> VP8_FILTER_SHIFT) as u8;
        }
    }
}

/// Cached AVX2 availability (the per-block detection load is cheap, but
/// MC blocks are the hottest call in an inter decode).
#[cfg(target_arch = "x86_64")]
fn avx2_supported() -> bool {
    static AVX2: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| std::arch::is_x86_feature_detected!("avx2"));
    *AVX2
}

/// Explicit `std::arch::x86_64` kernels for the six-tap and bilinear
/// passes (the 18% of a decode the profile pins on `sixtap_block` and its
/// chroma siblings). Nothing here is spec logic: each kernel is the scalar
/// function's arithmetic lane-parallel, with the same rounding, the same
/// truncation points and the same clamps -- the equivalence tests run
/// every kernel against its scalar reference over all shapes, phases and
/// worst-case input bytes.
///
/// Exactness notes shared by the six-tap kernels:
/// - `_mm*_madd_epi16` multiplies *pairs of samples* by a packed tap pair
///   and accumulates in `i32`; every product is exact (widened samples
///   are 0..=255, `|byte * tap| <= 255 * 128 < 2^15`) and the six-product
///   sum is exact in `i32` (`|dot + 64| <= 255 * 160 + 64 < 2^23`).
/// - The even/odd column split exists because `madd` pairs *adjacent*
///   samples: the even accumulator's `madd` lane m is output `2m`, the
///   odd one output `2m+1` (load offset +1 shifts the sample window).
/// - `packs_epi32`'s `i32 -> i16` saturation can never fire: after
///   `(dot + 64) >> 7` the range is `[-76, 319]`.
/// - `packus_epi16`'s `u8` saturation *is* filter.c's `clamp_pixel`:
///   values below 0 saturate to 0, above 255 to 255, the rest unchanged.
#[cfg(target_arch = "x86_64")]
#[allow(unsafe_code)]
#[allow(unsafe_op_in_unsafe_fn)] // each kernel's whole body is its contract
mod simd {
    use super::{VP8_FILTER_SHIFT, VP8_FILTER_WEIGHT};
    use std::arch::x86_64::*;

    /// Tap pair `[t0, t1]` packed into one `i32`, the operand shape
    /// `_mm*_madd_epi16` wants: lane `j` of `madd(samples, set1(pair))`
    /// is `s[2j] * t0 + s[2j+1] * t1` (ec-av1 `mc::simd::tap_pairs`).
    #[inline]
    fn tap_pair(a: i32, b: i32) -> i32 {
        (b << 16) | (a as u16 as i32)
    }

    /// Six-tap first pass, all 16 lanes of every intermediate row (see
    /// [`sixtap_block`] for the layout).
    ///
    /// # Safety
    /// Requires AVX2. `src` must be a bordered MC plane such that for
    /// every `i < rows`, the 21 bytes starting at
    /// `(sy + i - 2) * stride + sx - 2` are in bounds (`sx, sy >= 2`).
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn sixtap_h(
        src: &[u8],
        stride: usize,
        sx: usize,
        sy: usize,
        hf: &[i32; 6],
        tmp: &mut [u8; 21 * 16],
        rows: usize,
    ) {
        debug_assert!(sx >= 2 && sy >= 2);
        debug_assert!((sy + rows - 2) * stride + sx + 19 <= src.len());
        let tv = [
            _mm256_set1_epi32(tap_pair(hf[0], hf[1])),
            _mm256_set1_epi32(tap_pair(hf[2], hf[3])),
            _mm256_set1_epi32(tap_pair(hf[4], hf[5])),
        ];
        let rnd = _mm256_set1_epi32(VP8_FILTER_WEIGHT >> 1);
        for i in 0..rows {
            let sp = src.as_ptr().add((sy + i - 2) * stride + sx - 2);
            // `cvtepu8_epi16` of the 16-byte window at byte offset `2k`
            // puts each even output's tap pair in one `madd` lane pair:
            // `e` accumulates outputs 0,2,..,14, `o` (window +1 byte)
            // outputs 1,3,..,15.
            let mut e = _mm256_setzero_si256();
            let mut o = _mm256_setzero_si256();
            for k in 0..3 {
                let se = _mm256_cvtepu8_epi16(_mm_loadu_si128(sp.add(2 * k).cast()));
                e = _mm256_add_epi32(e, _mm256_madd_epi16(se, tv[k]));
                let so = _mm256_cvtepu8_epi16(_mm_loadu_si128(sp.add(2 * k + 1).cast()));
                o = _mm256_add_epi32(o, _mm256_madd_epi16(so, tv[k]));
            }
            e = _mm256_srai_epi32(_mm256_add_epi32(e, rnd), VP8_FILTER_SHIFT);
            o = _mm256_srai_epi32(_mm256_add_epi32(o, rnd), VP8_FILTER_SHIFT);
            // `unpacklo/hi` + `packs` interleave even/odd outputs back in
            // order (0..8 in the low 128-bit half, 8..16 in the high
            // half), then `packus` narrows both halves in sequence -- its
            // `u8` saturation is filter.c's `clamp_pixel` itself.
            let lo = _mm256_unpacklo_epi32(e, o);
            let hi = _mm256_unpackhi_epi32(e, o);
            let i16 = _mm256_packs_epi32(lo, hi);
            let u8 = _mm_packus_epi16(
                _mm256_castsi256_si128(i16),
                _mm256_extracti128_si256::<1>(i16),
            );
            _mm_storeu_si128(tmp.as_mut_ptr().add(i * 16).cast(), u8);
        }
    }

    /// Six-tap second pass: `bh` output rows of `bw` (4/8/16) samples from
    /// the 16-lane-strided intermediate.
    ///
    /// # Safety
    /// Requires AVX2. `dst` must be writable for `bh` rows of `bw` bytes
    /// at `dst_stride`; `bw` is 4, 8 or 16.
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn sixtap_v(
        tmp: &[u8; 21 * 16],
        vf: &[i32; 6],
        dst: &mut [u8],
        dst_stride: usize,
        bw: usize,
        bh: usize,
    ) {
        debug_assert!(matches!(bw, 4 | 8 | 16));
        debug_assert!(dst.len() >= (bh - 1) * dst_stride + bw);
        let tv = [
            _mm256_set1_epi32(tap_pair(vf[0], vf[1])),
            _mm256_set1_epi32(tap_pair(vf[2], vf[3])),
            _mm256_set1_epi32(tap_pair(vf[4], vf[5])),
        ];
        let rnd = _mm256_set1_epi32(VP8_FILTER_WEIGHT >> 1);
        for r in 0..bh {
            let ip = tmp.as_ptr().add(r * 16);
            // Rows `r + 2k` / `r + 2k + 1` widened to samples, interleaved
            // by `unpack_epi16` so one `madd` covers two taps of every
            // output; `lo` holds even columns, `hi` odd ones (one run per
            // 128-bit lane).
            let mut lo = _mm256_setzero_si256();
            let mut hi = _mm256_setzero_si256();
            for k in 0..3 {
                let a = _mm256_cvtepu8_epi16(_mm_loadu_si128(ip.add(2 * k * 16).cast()));
                let b = _mm256_cvtepu8_epi16(_mm_loadu_si128(ip.add((2 * k + 1) * 16).cast()));
                lo = _mm256_add_epi32(lo, _mm256_madd_epi16(_mm256_unpacklo_epi16(a, b), tv[k]));
                hi = _mm256_add_epi32(hi, _mm256_madd_epi16(_mm256_unpackhi_epi16(a, b), tv[k]));
            }
            lo = _mm256_srai_epi32(_mm256_add_epi32(lo, rnd), VP8_FILTER_SHIFT);
            hi = _mm256_srai_epi32(_mm256_add_epi32(hi, rnd), VP8_FILTER_SHIFT);
            // `packs` reorders to outputs 0..8 | 8..16 across the two
            // 128-bit halves, in column order.
            let i16 = _mm256_packs_epi32(lo, hi);
            let u8 = _mm_packus_epi16(
                _mm256_castsi256_si128(i16),
                _mm256_extracti128_si256::<1>(i16),
            );
            let dp = dst.as_mut_ptr().add(r * dst_stride);
            match bw {
                16 => _mm_storeu_si128(dp.cast(), u8),
                8 => _mm_storel_epi64(dp.cast(), u8),
                4 => _mm_storeu_si32(dp.cast(), u8),
                _ => unreachable!("bw is 4, 8 or 16"),
            }
        }
    }

    /// Bilinear first pass, 8 output columns per step (`bw` is 4, 8 or
    /// 16; `bw = 16` runs the loop twice). The `i16` accumulator is exact:
    /// taps are non-negative and sum to 128, so every product and the sum
    /// stay `<= 255 * 128 + 64 < 2^15` -- this *is* the C's `unsigned
    /// short` intermediate, narrowed at a provably-in-range point.
    ///
    /// # Safety
    /// Requires AVX2. For every `i < rows`, the 17 bytes starting at
    /// `(sy + i) * stride + sx` must be in bounds of `src`.
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn bilinear_h(
        src: &[u8],
        stride: usize,
        sx: usize,
        sy: usize,
        hf: &[i32; 2],
        tmp: &mut [u8; 17 * 16],
        rows: usize,
    ) {
        debug_assert!((sy + rows - 1) * stride + sx + 16 < src.len());
        let f0 = _mm_set1_epi16(hf[0] as i16);
        let f1 = _mm_set1_epi16(hf[1] as i16);
        let rnd = _mm_set1_epi16((VP8_FILTER_WEIGHT / 2) as i16);
        for i in 0..rows {
            let sp = src.as_ptr().add((sy + i) * stride + sx);
            for c in (0..16).step_by(8) {
                // Lane j pairs the samples `j` and `j + 1` (tap `[0]` on
                // the window at `c`, tap `[1]` shifted by one byte).
                let a = _mm_cvtepu8_epi16(_mm_loadu_si128(sp.add(c).cast()));
                let b = _mm_cvtepu8_epi16(_mm_loadu_si128(sp.add(c + 1).cast()));
                let acc = _mm_add_epi16(
                    _mm_add_epi16(_mm_mullo_epi16(a, f0), _mm_mullo_epi16(b, f1)),
                    rnd,
                );
                let u8 = _mm_packus_epi16(_mm_srai_epi16::<VP8_FILTER_SHIFT>(acc), acc);
                _mm_storel_epi64(tmp.as_mut_ptr().add(i * 16 + c).cast(), u8);
            }
        }
    }

    /// Bilinear second pass: rows `r` / `r + 1` of the intermediate, same
    /// exact-`i16` shape as [`bilinear_h`].
    ///
    /// # Safety
    /// Requires AVX2. `dst` must be writable for `bh` rows of `bw` bytes
    /// at `dst_stride`; `bw` is 4, 8 or 16.
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn bilinear_v(
        tmp: &[u8; 17 * 16],
        vf: &[i32; 2],
        dst: &mut [u8],
        dst_stride: usize,
        bw: usize,
        bh: usize,
    ) {
        debug_assert!(matches!(bw, 4 | 8 | 16));
        debug_assert!(dst.len() >= (bh - 1) * dst_stride + bw);
        let f0 = _mm_set1_epi16(vf[0] as i16);
        let f1 = _mm_set1_epi16(vf[1] as i16);
        let rnd = _mm_set1_epi16((VP8_FILTER_WEIGHT / 2) as i16);
        for r in 0..bh {
            let ip = tmp.as_ptr().add(r * 16);
            let dp = dst.as_mut_ptr().add(r * dst_stride);
            for c in (0..bw).step_by(8) {
                let a = _mm_cvtepu8_epi16(_mm_loadl_epi64(ip.add(c).cast()));
                let b = _mm_cvtepu8_epi16(_mm_loadl_epi64(ip.add(16 + c).cast()));
                let acc = _mm_add_epi16(
                    _mm_add_epi16(_mm_mullo_epi16(a, f0), _mm_mullo_epi16(b, f1)),
                    rnd,
                );
                let u8 = _mm_packus_epi16(_mm_srai_epi16::<VP8_FILTER_SHIFT>(acc), acc);
                match bw - c {
                    8 | 16 => _mm_storel_epi64(dp.add(c).cast(), u8),
                    _ => _mm_storeu_si32(dp.add(c).cast(), u8),
                }
            }
        }
    }
}

/// Two-tap bilinear separable predictor (libvpx vp8_bilinear_predict*,
/// filter.c). The dot products are provably in 0..=255 (taps are
/// non-negative and sum to 128 over 0..=255 inputs), which is why libvpx's
/// bilinear passes store `unsigned short` intermediates and skip the
/// clamp: the intermediate is stored as `u8` at a fixed 16-lane stride
/// (representation-only change, same argument as [`sixtap_block`]).
fn bilinear_block(
    src: &[u8],
    stride: usize,
    sx: usize,
    sy: usize,
    fx: usize,
    fy: usize,
    dst: &mut [u8],
    dst_stride: usize,
    bw: usize,
    bh: usize,
) {
    if fx == 0 && fy == 0 {
        for r in 0..bh {
            let s = (sy + r) * stride + sx;
            dst[r * dst_stride..r * dst_stride + bw].copy_from_slice(&src[s..s + bw]);
        }
        return;
    }

    let hf = &BILINEAR_FILTERS[fx];
    let vf = &BILINEAR_FILTERS[fy];
    let rows = bh + 1;
    let mut tmp = [0u8; 17 * 16]; // C: unsigned short FData[17 * 16], <= 255

    #[cfg(target_arch = "x86_64")]
    {
        if avx2_supported() {
            // SAFETY: AVX2 checked by `avx2_supported`; border/bounds
            // contracts hold for every caller (see [`sixtap_block`]).
            #[allow(unsafe_code)] // SAFETY: avx2_supported() checked above
            unsafe {
                simd::bilinear_h(src, stride, sx, sy, hf, &mut tmp, rows);
                simd::bilinear_v(&tmp, vf, dst, dst_stride, bw, bh);
            }
            return;
        }
    }

    bilinear_h_scalar(src, stride, sx, sy, hf, &mut tmp, bw, rows);
    bilinear_v_scalar(&tmp, vf, dst, dst_stride, bw, bh);
}

/// Scalar first pass of [`bilinear_block`] (filter.c
/// `filter_block2d_bil_first_pass`): `bh + 1` rows, taps `[0]` and `[1]`.
/// Phase 0 is the exact identity, as in [`sixtap_h_scalar`].
fn bilinear_h_scalar(
    src: &[u8],
    stride: usize,
    sx: usize,
    sy: usize,
    hf: &[i32; 2],
    tmp: &mut [u8; 17 * 16],
    bw: usize,
    rows: usize,
) {
    for i in 0..rows {
        let row = (sy + i) * stride + sx;
        for j in 0..bw {
            let p = row + j;
            let acc = i32::from(src[p]) * hf[0] + i32::from(src[p + 1]) * hf[1]
                + (VP8_FILTER_WEIGHT / 2); /* Rounding */
            tmp[i * 16 + j] = (acc >> VP8_FILTER_SHIFT) as u8;
        }
    }
}

/// Scalar second pass of [`bilinear_block`] (filter.c
/// `filter_block2d_bil_second_pass`): taps `[0]` and `[1]` read rows
/// `r` / `r + 1` of the intermediate at the fixed 16-lane stride.
fn bilinear_v_scalar(
    tmp: &[u8; 17 * 16],
    vf: &[i32; 2],
    dst: &mut [u8],
    dst_stride: usize,
    bw: usize,
    bh: usize,
) {
    for r in 0..bh {
        for c in 0..bw {
            let acc = i32::from(tmp[r * 16 + c]) * vf[0]
                + i32::from(tmp[(r + 1) * 16 + c]) * vf[1]
                + (VP8_FILTER_WEIGHT / 2); /* Rounding */
            dst[r * dst_stride + c] = (acc >> VP8_FILTER_SHIFT) as u8;
        }
    }
}

/// Chroma predictor for one `bw` x `bh` block whose visible-image top-left
/// chroma pixel is `(x, y)`.
///
/// `mv` is the chroma MV in eighth-CHROMA-pel units (the caller derives it
/// from luma per libvpx: round-half-away-from-zero division by 2 (whole MB)
/// or sum-then-divide-by-8 (SPLITMV), `& fullpixel_mask`).
///
/// The interpolation filter is the FRAME's MC filter, exactly like luma:
/// `vp8_setup_version` picks six-tap for version 0 (and reserved 4-7) and
/// bilinear for versions 1-3 (decodeframe.c:851-860 assigns BOTH
/// `subpixel_predict` and `subpixel_predict8x8` from that one switch, and
/// reconinter.c uses them for the chroma predicts). `bilinear` is that
/// per-frame selection.
///
/// When `need_clamp` is set, the MV is first passed through
/// [`clamp_uvmv_to_umv_border`].
pub(crate) fn predict_chroma(
    src: &[u8],
    uv_stride: usize,
    border: usize,
    vis_w: usize,
    vis_h: usize,
    x: usize,
    y: usize,
    mv: (i16, i16),
    need_clamp: bool,
    dst: &mut [u8],
    dst_stride: usize,
    bw: usize,
    bh: usize,
    bilinear: bool,
) {
    debug_assert!(bw >= 2 && bw <= 16 && bw % 2 == 0);
    debug_assert!(bh >= 2 && bh <= 16 && bh % 2 == 0);

    let mut mv = (i32::from(mv.0), i32::from(mv.1));
    if need_clamp {
        // A right/bottom chroma quadrant starts at x == vis_w; it still
        // lives in the last MB, so clamp the derived MB index.
        let mb_cols = (vis_w + 15) >> 4;
        let mb_rows = (vis_h + 15) >> 4;
        let mbc = (x >> 3).min(mb_cols - 1);
        let mbr = (y >> 3).min(mb_rows - 1);
        clamp_uvmv_to_umv_border(&mut mv, mbc, mbr, vis_w, vis_h);
    }

    let fx = (mv.1 & 7) as usize;
    let fy = (mv.0 & 7) as usize;
    let sx = border as i32 + x as i32 + (mv.1 >> 3);
    let sy = border as i32 + y as i32 + (mv.0 >> 3);
    // 6-tap reads reach 2 before / 3 after the origin.
    debug_assert!(sx >= 2 && sy >= 2, "reference read outside bordered plane");
    let (sx, sy) = (sx as usize, sy as usize);

    if bilinear {
        bilinear_block(src, uv_stride, sx, sy, fx, fy, dst, dst_stride, bw, bh);
    } else {
        sixtap_block(src, uv_stride, sx, sy, fx, fy, dst, dst_stride, bw, bh);
    }
}

/// The macroblock-level UMV border edges in eighth-pel units, exactly the
/// decoder's `xd->mb_to_{left,right,top,bottom}_edge` (decodeframe.c:503-532):
///
/// ```c
/// xd->mb_to_top_edge    = -((mb_row * 16) << 3);
/// xd->mb_to_bottom_edge = ((mb_rows - 1 - mb_row) * 16) << 3;
/// xd->mb_to_left_edge   = -((mb_col * 16) << 3);
/// xd->mb_to_right_edge  = ((mb_cols - 1 - mb_col) * 16) << 3;
/// ```
///
/// with `mb_rows`/`mb_cols` the full (partial MB inclusive) MB counts of the
/// visible frame. Returns `(left, right, top, bottom)`.
fn mb_edges(mb_col: usize, mb_row: usize, vis_w: usize, vis_h: usize) -> (i32, i32, i32, i32) {
    let mb_cols = (vis_w + 15) >> 4;
    let mb_rows = (vis_h + 15) >> 4;
    debug_assert!(mb_col < mb_cols && mb_row < mb_rows);
    let left = -(((mb_col * 16) as i32) << 3);
    let right = (((mb_cols - 1 - mb_col) * 16) as i32) << 3;
    let top = -(((mb_row * 16) as i32) << 3);
    let bottom = (((mb_rows - 1 - mb_row) * 16) as i32) << 3;
    (left, right, top, bottom)
}

/// Port of `clamp_mv_to_umv_border` (reconinter.c:257-278), luma MV in
/// eighth-pel units:
///
/// ```c
/// if (mv->col < (xd->mb_to_left_edge - (19 << 3))) {
///   mv->col = xd->mb_to_left_edge - (16 << 3);
/// } else if (mv->col > xd->mb_to_right_edge + (18 << 3)) {
///   mv->col = xd->mb_to_right_edge + (16 << 3);
/// }
/// /* same for row with top/bottom edges */
/// ```
///
/// 19 px slack toward left/top (16 px + 3 taps right of the central pixel),
/// 18 px toward right/bottom (16 px + 2 taps left of it); the clamp anchor
/// is 16 px inside the border. `mb_col`/`mb_row` locate the block's
/// macroblock; libvpx clamps with these MB-level edges for whole-MB MVs and,
/// when `need_to_clamp_mvs` is set, for each SPLITMV subblock MV too
/// (reconinter.c:370-375, 397-400).
fn clamp_mv_to_umv_border(mv: &mut (i32, i32), mb_col: usize, mb_row: usize, vis_w: usize, vis_h: usize) {
    let (left, right, top, bottom) = mb_edges(mb_col, mb_row, vis_w, vis_h);
    if mv.1 < left - (19 << 3) {
        mv.1 = left - (16 << 3);
    } else if mv.1 > right + (18 << 3) {
        mv.1 = right + (16 << 3);
    }

    if mv.0 < top - (19 << 3) {
        mv.0 = top - (16 << 3);
    } else if mv.0 > bottom + (18 << 3) {
        mv.0 = bottom + (16 << 3);
    }
}

/// Port of `clamp_uvmv_to_umv_border` (reconinter.c:281-295), chroma MV in
/// eighth-CHROMA-pel units. The comparison doubles the chroma MV back into
/// luma eighth-pel units (`2 * mv->col`, the "different divisor": the
/// chroma border reach is half the luma one) and the clamp target is the
/// luma anchor halved:
///
/// ```c
/// mv->col = (2 * mv->col < (xd->mb_to_left_edge - (19 << 3)))
///               ? (xd->mb_to_left_edge - (16 << 3)) >> 1
///               : mv->col;
/// mv->col = (2 * mv->col > xd->mb_to_right_edge + (18 << 3))
///               ? (xd->mb_to_right_edge + (16 << 3)) >> 1
///               : mv->col;
/// /* same for row */
/// ```
///
/// The C applies the two ternaries in sequence (right check sees the
/// left-clamped value, which cannot trip it); the sequential `if`s here are
/// that verbatim. `(left - (16 << 3))` is a multiple of 256 so `>> 1` is
/// exact. Callers must re-apply this clamp AFTER full-pel rounding of a
/// derived chroma MV: rounding to full-pel can move the MV outside the
/// border tap window even though the luma MV was already clamped
/// (reconinter.c:336-339 — the 16x16 path clamps the chroma MV
/// unconditionally for exactly this reason).
fn clamp_uvmv_to_umv_border(mv: &mut (i32, i32), mb_col: usize, mb_row: usize, vis_w: usize, vis_h: usize) {
    let (left, right, top, bottom) = mb_edges(mb_col, mb_row, vis_w, vis_h);

    if 2 * mv.1 < left - (19 << 3) {
        mv.1 = (left - (16 << 3)) >> 1;
    }
    if 2 * mv.1 > right + (18 << 3) {
        mv.1 = (right + (16 << 3)) >> 1;
    }

    if 2 * mv.0 < top - (19 << 3) {
        mv.0 = (top - (16 << 3)) >> 1;
    }
    if 2 * mv.0 > bottom + (18 << 3) {
        mv.0 = (bottom + (16 << 3)) >> 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic plane fill shared by every test (LCG, same sequence in
    /// the hand-computed literals below).
    fn lcg_plane(seed: u32, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        let mut s = seed;
        for b in out.iter_mut() {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            *b = (s >> 16) as u8;
        }
        out
    }

    const BORDER: usize = 32;
    const STRIDE: usize = 104; // 40 visible + 2 * 32 border
    const VIS_W: usize = 40; // 3 MB columns, last one partial
    const VIS_H: usize = 36; // 3 MB rows, last one partial
    const ROWS: usize = VIS_H + 2 * BORDER;

    fn plane() -> Vec<u8> {
        lcg_plane(0x1234, STRIDE * ROWS)
    }

    /// Reads visible-image pixel (`vx`, `vy`) out of a bordered plane.
    fn vis(plane: &[u8], vx: usize, vy: usize) -> u8 {
        plane[(vy + BORDER) * STRIDE + (vx + BORDER)]
    }

    #[test]
    fn extend_plane_replicates_edges_and_corners() {
        let mut p = plane();
        // Distinct visible values, sentinel everywhere else.
        for r in 0..VIS_H {
            for c in 0..VIS_W {
                p[(BORDER + r) * STRIDE + BORDER + c] = ((r * 7 + c * 3) % 251 + 4) as u8;
            }
        }
        p[..BORDER * STRIDE].fill(0xEE);
        p[(BORDER + VIS_H) * STRIDE..].fill(0xEE);
        for r in 0..VIS_H {
            let row = &mut p[(BORDER + r) * STRIDE..(BORDER + r) * STRIDE + STRIDE];
            row[..BORDER].fill(0xEE);
            row[BORDER + VIS_W..].fill(0xEE);
        }

        extend_plane(&mut p, STRIDE, VIS_W, VIS_H, BORDER);

        // Every sample of the bordered plane equals the nearest in-image sample.
        for r in 0..ROWS {
            for c in 0..STRIDE {
                let vr = r.saturating_sub(BORDER).min(VIS_H - 1);
                let vc = c.saturating_sub(BORDER).min(VIS_W - 1);
                assert_eq!(p[r * STRIDE + c], vis(&p, vc, vr), "sample ({r},{c})");
            }
        }
        // Explicit corners (worst case of the rule above).
        assert_eq!(p[0], vis(&p, 0, 0)); // top-left
        assert_eq!(p[line_len() - 1], vis(&p, VIS_W - 1, 0)); // top-right
        assert_eq!(p[(ROWS - 1) * STRIDE], vis(&p, 0, VIS_H - 1)); // bottom-left
        assert_eq!(
            p[ROWS * STRIDE - 1],
            vis(&p, VIS_W - 1, VIS_H - 1) // bottom-right
        );
        // The visible region itself is untouched.
        for r in 0..VIS_H {
            for c in 0..VIS_W {
                assert_eq!(
                    p[(BORDER + r) * STRIDE + BORDER + c],
                    ((r * 7 + c * 3) % 251 + 4) as u8
                );
            }
        }
    }

    fn line_len() -> usize {
        BORDER + VIS_W + BORDER
    }

    #[test]
    fn integer_mv_predict_is_a_plain_copy() {
        let p = plane();
        let mut dst = [0u8; 16 * 16];

        // Whole-pel luma MV (row=16, col=-8) → full-pel offset (x-1, y+2).
        // mv.0 is the row component, mv.1 the column component. x=y=8 keeps
        // every read inside the visible region of the unextended plane.
        predict_luma(
            &p, STRIDE, BORDER, VIS_W, VIS_H, 8, 8, (16, -8), false, &mut dst, 16, 16, 16,
        false);
        for r in 0..16 {
            for c in 0..16 {
                assert_eq!(dst[r * 16 + c], vis(&p, 8 - 1 + c, 8 + 2 + r), "({r},{c})");
            }
        }

        // Zero MV → identity copy.
        predict_luma(
            &p, STRIDE, BORDER, VIS_W, VIS_H, 8, 8, (0, 0), false, &mut dst, 16, 16, 16,
        false);
        for r in 0..16 {
            for c in 0..16 {
                assert_eq!(dst[r * 16 + c], vis(&p, 8 + c, 8 + r), "({r},{c})");
            }
        }

        // Chroma copy path, whole-pel chroma MV (row=-16, col=+8)
        // → full-pel offset (x+1, y-2).
        let mut dst8 = [0u8; 8 * 8];
        predict_chroma(
            &p, STRIDE, BORDER, VIS_W, VIS_H, 4, 4, (-16, 8), false, &mut dst8, 8, 8, 8,
        false);
        for r in 0..8 {
            for c in 0..8 {
                assert_eq!(dst8[r * 8 + c], vis(&p, 4 + 1 + c, 4 - 2 + r), "({r},{c})");
            }
        }
    }

    #[test]
    fn sixtap_pure_horizontal_matches_hand_computed_values() {
        let p = plane();
        let mut dst = [0u8; 4 * 4];

        // mv = (0, 4): full-pel start (16, 12), fx = 4, fy = 0.
        predict_luma(
            &p, STRIDE, BORDER, VIS_W, VIS_H, 16, 12, (0, 4), false, &mut dst, 4, 4, 4, false,
        );

        // Hand-computed from vp8_sub_pel_filters[4] = { 3, -16, 77, 77, -16, 3 }
        // and vp8_sub_pel_filters[0] = { 0, 0, 128, 0, 0, 0 } (vertical is the
        // exact identity over the clamped horizontal intermediate).
        // out[0][0]: taps are visible pixels (12, 14..19) = [183, 106, 2, 87, 75, 198]:
        //   183*3 - 106*16 + 2*77 + 87*77 - 75*16 + 198*3 + 64 = 5164
        //   5164 >> 7 = 40, already in 0..=255.
        // out[1][2]: taps (13, 17..22) = [153, 188, 219, 13, 86, 194]:
        //   153*3 - 188*16 + 219*77 + 13*77 - 86*16 + 194*3 + 64 = 14585
        //   14585 >> 7 = 113.
        assert_eq!(dst[0], 40);
        assert_eq!(dst[1 * 4 + 2], 113);

        // Full block against the tap-table reference formula.
        for r in 0..4 {
            for c in 0..4 {
                let taps: Vec<i32> = (0..6)
                    .map(|k| i32::from(vis(&p, 16 + c - 2 + k as usize, 12 + r)))
                    .collect();
                let acc: i32 = taps
                    .iter()
                    .zip(SUB_PEL_FILTERS[4])
                    .map(|(t, f)| t * f)
                    .sum::<i32>()
                    + 64;
                assert_eq!(
                    dst[r * 4 + c],
                    (acc >> 7).clamp(0, 255) as u8,
                    "out[{r}][{c}] taps={taps:?} acc={acc}"
                );
            }
        }
    }

    #[test]
    fn sixtap_combined_phase_16x16_matches_hand_computed_values() {
        let p = plane();
        let mut dst = [0u8; 16 * 16];

        // mv = (14, 18): full-pel start (10, 9), fx = 2, fy = 6.
        predict_luma(
            &p, STRIDE, BORDER, VIS_W, VIS_H, 8, 8, (14, 18), false, &mut dst, 16, 16, 16, false,
        );

        // vp8_sub_pel_filters[2] = { 2, -11, 108, 36, -8, 1 }, each
        // intermediate clamped to 0..=255 (e.g. tmp[3][10] taps
        // [75, 190, 18, 15, 104, 40], raw dot -184, -184 >> 7 = -2, clamped 0);
        // vertical pass with vp8_sub_pel_filters[6] = { 1, -8, 36, 108, -11, 2 }.
        //
        // out[0][0]: horizontal taps are visible pixels (7, 8..13), giving the
        //   post-clamp intermediate column h_tmp = [41, 186, 229, 210, 95, 185]:
        //   41*1 - 186*8 + 229*36 + 210*108 - 95*11 + 185*2 + 64 = 28866
        //   28866 >> 7 = 225, already in 0..=255.
        //
        // out[5][9]: horizontal taps are visible pixels (11, 15..20),
        //   h_tmp = [205, 8, 14, 189, 108, 4]:
        //   205*1 - 8*8 + 14*36 + 189*108 - 108*11 + 4*2 + 64 = 19941
        //   19941 >> 7 = 155.
        assert_eq!(dst[0], 225);
        assert_eq!(dst[5 * 16 + 9], 155);

        // Full block against the two-pass reference formula.
        let mut tmp = [0i32; 21 * 16];
        for i in 0..21 {
            for j in 0..16 {
                let acc: i32 = (0..6)
                    .map(|k| i32::from(vis(&p, 10 - 2 + j + k, 9 - 2 + i)))
                    .zip(SUB_PEL_FILTERS[2])
                    .map(|(t, f)| t * f)
                    .sum::<i32>()
                    + 64;
                tmp[i * 16 + j] = (acc >> 7).clamp(0, 255);
            }
        }
        for r in 0..16 {
            for c in 0..16 {
                let acc: i32 = (0..6)
                    .map(|k| tmp[(r + k) * 16 + c])
                    .zip(SUB_PEL_FILTERS[6])
                    .map(|(t, f)| t * f)
                    .sum::<i32>()
                    + 64;
                assert_eq!(
                    dst[r * 16 + c],
                    (acc >> 7).clamp(0, 255) as u8,
                    "out[{r}][{c}]"
                );
            }
        }
    }

    #[test]
    fn bilinear_chroma_matches_hand_computed_values() {
        let p = plane();
        let mut dst = [0u8; 8 * 8];

        // mv = (11, 5): full-pel start (4, 7), fx = 5, fy = 3.
        predict_chroma(
            &p, STRIDE, BORDER, VIS_W, VIS_H, 4, 6, (11, 5), false, &mut dst, 8, 8, 8, true,
        );

        // Horizontal taps { 48, 80 } (phase 5), vertical { 80, 48 } (phase 3),
        // both passes (dot + 64) >> 7, no clamp (provably in range).
        //
        // out[0][0]: tmp[0][0] = (160*48 + 96*80 + 64) >> 7 = 120 from visible
        //   pixels (4,7),(5,7); tmp[1][0] = (107*48 + 42*80 + 64) >> 7 = 66
        //   from (4,8),(5,8).
        //   120*80 + 66*48 + 64 = 12832, 12832 >> 7 = 100.
        // out[3][7]: tmp[3][7] = 84, tmp[4][7] = 189;
        //   84*80 + 189*48 + 64 = 15856, 15856 >> 7 = 123.
        assert_eq!(dst[0], 100);
        assert_eq!(dst[3 * 8 + 7], 123);

        // Full block against the two-pass reference formula.
        let mut tmp = [0u16; 9 * 8];
        for i in 0..9 {
            for j in 0..8 {
                tmp[i * 8 + j] = ((i32::from(vis(&p, 4 + j, 7 + i)) * 48
                    + i32::from(vis(&p, 4 + j + 1, 7 + i)) * 80
                    + 64)
                    >> 7) as u16;
            }
        }
        for r in 0..8 {
            for c in 0..8 {
                let acc = i32::from(tmp[r * 8 + c]) * 80 + i32::from(tmp[(r + 1) * 8 + c]) * 48 + 64;
                assert_eq!(dst[r * 8 + c], (acc >> 7) as u8, "out[{r}][{c}]");
            }
        }
    }

    #[test]
    fn umv_border_clamps_match_libvpx_arithmetic() {
        let p = plane();
        let mut a = [0u8; 16 * 16];
        let mut b = [0u8; 16 * 16];

        // Single-MB frame (vis 16x16): mb_to_left_edge = mb_to_top_edge = 0,
        // mb_to_right_edge = mb_to_bottom_edge = 0.
        //
        // Luma: mv (-1000, -1000) < 0 - 152 → both components set to -128
        // (left - (16 << 3)); prediction must equal an explicit (-128, -128).
        predict_luma(
            &p, STRIDE, BORDER, 16, 16, 0, 0, (-1000, -1000), true, &mut a, 16, 16, 16,
        false);
        predict_luma(
            &p, STRIDE, BORDER, 16, 16, 0, 0, (-128, -128), false, &mut b, 16, 16, 16,
        false);
        assert_eq!(a, b);

        // Strict threshold: (-152, -152) is NOT clamped (condition is strict
        // `<`), (-153, -153) IS clamped to (-128, -128).
        predict_luma(
            &p, STRIDE, BORDER, 16, 16, 0, 0, (-152, -152), true, &mut a, 16, 16, 16,
        false);
        predict_luma(
            &p, STRIDE, BORDER, 16, 16, 0, 0, (-152, -152), false, &mut b, 16, 16, 16,
        false);
        assert_eq!(a, b);
        predict_luma(
            &p, STRIDE, BORDER, 16, 16, 0, 0, (-153, -153), true, &mut a, 16, 16, 16,
        false);
        predict_luma(
            &p, STRIDE, BORDER, 16, 16, 0, 0, (-128, -128), false, &mut b, 16, 16, 16,
        false);
        assert_eq!(a, b);

        // Right/bottom: mv (4000, 4000) > 0 + 144 → set to +128.
        predict_luma(
            &p, STRIDE, BORDER, 16, 16, 0, 0, (4000, 4000), true, &mut a, 16, 16, 16,
        false);
        predict_luma(
            &p, STRIDE, BORDER, 16, 16, 0, 0, (128, 128), false, &mut b, 16, 16, 16,
        false);
        assert_eq!(a, b);

        // Chroma: 2 * (-2000) < -152 → (-128 >> 1) = -64 per component.
        let mut ca = [0u8; 8 * 8];
        let mut cb = [0u8; 8 * 8];
        predict_chroma(
            &p, STRIDE, BORDER, 8, 8, 0, 0, (-2000, -2000), true, &mut ca, 8, 8, 8,
        false);
        predict_chroma(
            &p, STRIDE, BORDER, 8, 8, 0, 0, (-64, -64), false, &mut cb, 8, 8, 8,
        false);
        assert_eq!(ca, cb);

        // Chroma right/bottom: 2 * 2000 > 144 → (128 >> 1) = 64.
        predict_chroma(
            &p, STRIDE, BORDER, 8, 8, 0, 0, (2000, 2000), true, &mut ca, 8, 8, 8,
        false);
        predict_chroma(
            &p, STRIDE, BORDER, 8, 8, 0, 0, (64, 64), false, &mut cb, 8, 8, 8,
        false);
        assert_eq!(ca, cb);

        // An in-range MV with need_clamp=true must pass through untouched.
        predict_luma(
            &p, STRIDE, BORDER, 16, 16, 0, 0, (10, -10), true, &mut a, 16, 16, 16, false,
        );
        predict_luma(
            &p, STRIDE, BORDER, 16, 16, 0, 0, (10, -10), false, &mut b, 16, 16, 16, false,
        );
        assert_eq!(a, b);
    }

    /// Fills a bordered plane with a 0x00/0xFF checkerboard: maximizes the
    /// six-tap dot products' excursions in both directions (negative taps
    /// over 0xFF while positive taps sit on 0x00 and vice versa), so the
    /// clamp/saturation paths of both implementations are exercised.
    fn checkerboard_plane() -> Vec<u8> {
        let mut out = vec![0u8; STRIDE * ROWS];
        for r in 0..ROWS {
            for c in 0..STRIDE {
                out[r * STRIDE + c] = if (r + c) & 1 == 0 { 0xFF } else { 0x00 };
            }
        }
        out
    }

    /// The AVX2 kernels must be lane-for-lane identical to the scalar
    /// reference (the libvpx transcription) on every block shape, every
    /// phase pair, at both a central and a right/bottom-edge origin where
    /// the wide loads reach furthest into the border.
    #[test]
    #[cfg(target_arch = "x86_64")]
    fn simd_sixtap_matches_the_scalar_reference_everywhere() {
        if !avx2_supported() {
            return;
        }
        let planes = [plane(), checkerboard_plane()];
        let origins = [
            (BORDER + 2, BORDER + 3),
            (BORDER + VIS_W - 16, BORDER + VIS_H - 16),
        ];
        for p in &planes {
            for &(sx, sy) in &origins {
                for bw in [4usize, 8, 16] {
                    for bh in [4usize, 8, 16] {
                        for fx in 0..8usize {
                            for fy in 0..8usize {
                                if fx == 0 && fy == 0 {
                                    continue; // whole-pel copy, shared path
                                }
                                let mut want = [0u8; 16 * 16];
                                let mut got = [0u8; 16 * 16];
                                let mut tmp = [0u8; 21 * 16];
                                sixtap_h_scalar(
                                    p, STRIDE, sx, sy, &SUB_PEL_FILTERS[fx], &mut tmp, bw, bh + 5,
                                );
                                sixtap_v_scalar(&tmp, &SUB_PEL_FILTERS[fy], &mut want, 16, bw, bh);
                                // SAFETY: avx2_supported() checked above.
                                #[allow(unsafe_code)]
                                unsafe {
                                    let mut tmp2 = [0u8; 21 * 16];
                                    simd::sixtap_h(
                                        p, STRIDE, sx, sy, &SUB_PEL_FILTERS[fx], &mut tmp2, bh + 5,
                                    );
                                    simd::sixtap_v(&tmp2, &SUB_PEL_FILTERS[fy], &mut got, 16, bw, bh);
                                }
                                let n = bh * 16;
                                assert_eq!(
                                    &got[..n],
                                    &want[..n],
                                    "sixtap bw={bw} bh={bh} fx={fx} fy={fy} origin=({sx},{sy})"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// Same gate for the bilinear kernels.
    #[test]
    #[cfg(target_arch = "x86_64")]
    fn simd_bilinear_matches_the_scalar_reference_everywhere() {
        if !avx2_supported() {
            return;
        }
        let planes = [plane(), checkerboard_plane()];
        let origins = [
            (BORDER + 2, BORDER + 3),
            (BORDER + VIS_W - 16, BORDER + VIS_H - 16),
        ];
        for p in &planes {
            for &(sx, sy) in &origins {
                for bw in [4usize, 8, 16] {
                    for bh in [4usize, 8, 16] {
                        for fx in 0..8usize {
                            for fy in 0..8usize {
                                if fx == 0 && fy == 0 {
                                    continue;
                                }
                                let mut want = [0u8; 16 * 16];
                                let mut got = [0u8; 16 * 16];
                                let mut tmp = [0u8; 17 * 16];
                                bilinear_h_scalar(
                                    p, STRIDE, sx, sy, &BILINEAR_FILTERS[fx], &mut tmp, bw, bh + 1,
                                );
                                bilinear_v_scalar(&tmp, &BILINEAR_FILTERS[fy], &mut want, 16, bw, bh);
                                // SAFETY: avx2_supported() checked above.
                                #[allow(unsafe_code)]
                                unsafe {
                                    let mut tmp2 = [0u8; 17 * 16];
                                    simd::bilinear_h(
                                        p, STRIDE, sx, sy, &BILINEAR_FILTERS[fx], &mut tmp2, bh + 1,
                                    );
                                    simd::bilinear_v(&tmp2, &BILINEAR_FILTERS[fy], &mut got, 16, bw, bh);
                                }
                                for r in 0..bh {
                                    assert_eq!(
                                        got[r * 16..r * 16 + bw],
                                        want[r * 16..r * 16 + bw],
                                        "bilinear bw={bw} bh={bh} fx={fx} fy={fy} origin=({sx},{sy}) row={r}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

//! Sub-pel motion compensation, spec 7.11.3 ("Motion vector scaling process"
//! through "Block inter predictor process"), the REGULAR (`EIGHTTAP`) filter
//! only, 8-bit samples only.
//!
//! A block's inter prediction is separable: an 8-tap horizontal pass over the
//! reference, rounded down by `InterRound0`, feeds an 8-tap vertical pass
//! over that intermediate, rounded down by `InterRound1`. Both fractions run
//! in 1/16-pel steps -- luma motion vectors are stored at 1/8-pel and always
//! land on an even step; chroma vectors, once scaled for subsampling, can
//! land on any of the 16.

/// `Round2` (spec 4.7): halves round up.
fn round2(value: i32, shift: u32) -> i32 {
    if shift == 0 {
        value
    } else {
        (value + (1 << (shift - 1))) >> shift
    }
}

/// The kernels' intermediate is `i16` (lane-mc), and every SIMD store lands
/// through `packs_epi32`, which saturates -- so the scalar arms must too, or
/// the two would disagree on an out-of-range sum. Motion compensation never
/// produces one (`horizontal_intermediate_fits_i16` proves the bound from the
/// tap sums at 8/10-bit; 12-bit is refused in stream.rs), and restoration's
/// wiener pass clamps its own output into i16 immediately after.
fn sat16(value: i32) -> i16 {
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

/// `InterRound0` (spec 7.11.3.2), 8-bit, non-compound: the shift the
/// horizontal pass's sum is brought down by before it is stored as the
/// intermediate.
const INTER_ROUND_0: u32 = 3;
/// `InterRound1` (spec 7.11.3.2), 8-bit, non-compound: the shift the vertical
/// pass's sum is brought down by to land back on an 8-bit sample. Composed
/// with `INTER_ROUND_0` the two shifts are the filter's own gain (each pass
/// sums to 128, `128 * 128 == 1 << 14 == 1 << (INTER_ROUND_0 + INTER_ROUND_1)`)
/// -- so an identity or DC input comes back exactly, by construction of the
/// table rather than by a fitted constant.
const INTER_ROUND_1: u32 = 11;

/// `Subpel_Filters[EIGHTTAP]` (spec 7.11.3.3): 16 rows, one per 1/16-pel
/// fraction, each an 8-tap kernel centred so tap index 3 lands on the integer
/// sample (fraction 0 is the identity tap `[.., 128, ..]`).
const SUBPEL_FILTERS: [[i32; 8]; 16] = [
    [0, 0, 0, 128, 0, 0, 0, 0],
    [0, 2, -6, 126, 8, -2, 0, 0],
    [0, 2, -10, 122, 18, -4, 0, 0],
    [0, 2, -12, 116, 28, -8, 2, 0],
    [0, 2, -14, 110, 38, -10, 2, 0],
    [0, 2, -14, 102, 48, -12, 2, 0],
    [0, 2, -16, 94, 58, -12, 2, 0],
    [0, 2, -14, 84, 66, -12, 2, 0],
    [0, 2, -14, 76, 76, -14, 2, 0],
    [0, 2, -12, 66, 84, -14, 2, 0],
    [0, 2, -12, 58, 94, -16, 2, 0],
    [0, 2, -12, 48, 102, -14, 2, 0],
    [0, 2, -10, 38, 110, -14, 2, 0],
    [0, 2, -8, 28, 116, -12, 2, 0],
    [0, 0, -4, 18, 122, -10, 2, 0],
    [0, 0, -2, 8, 126, -6, 2, 0],
];

/// `Subpel_Filters[EIGHTTAP]`'s narrow-block counterpart (spec 7.11.3.4's
/// filter selection is per-axis: a `predict` axis whose output block
/// dimension is 4 or less reads this table instead of [`SUBPEL_FILTERS`]),
/// mirroring `av1_sub_pel_filters_4` (`filter.h`): the same 8-slot shape but
/// zeroed at taps 0/1/6/7, so a 4-wide (or 4-tall) block's filter never
/// reaches a full 3 samples past its own edge. Every 4:2:0 chroma block
/// under an 8x8 (or smaller) luma leaf hits this on at least one axis --
/// this is not an edge case, it is the common chroma shape.
const SUBPEL_FILTERS_4: [[i32; 8]; 16] = [
    [0, 0, 0, 128, 0, 0, 0, 0],
    [0, 0, -4, 126, 8, -2, 0, 0],
    [0, 0, -8, 122, 18, -4, 0, 0],
    [0, 0, -10, 116, 28, -6, 0, 0],
    [0, 0, -12, 110, 38, -8, 0, 0],
    [0, 0, -12, 102, 48, -10, 0, 0],
    [0, 0, -14, 94, 58, -10, 0, 0],
    [0, 0, -12, 84, 66, -10, 0, 0],
    [0, 0, -12, 76, 76, -12, 0, 0],
    [0, 0, -10, 66, 84, -12, 0, 0],
    [0, 0, -10, 58, 94, -14, 0, 0],
    [0, 0, -10, 48, 102, -12, 0, 0],
    [0, 0, -8, 38, 110, -12, 0, 0],
    [0, 0, -6, 28, 116, -10, 0, 0],
    [0, 0, -4, 18, 122, -8, 0, 0],
    [0, 0, -2, 8, 126, -4, 0, 0],
];

/// `Subpel_Filters[EIGHTTAP_SMOOTH]` (`filter.h`'s `av1_sub_pel_filters_8smooth`).
const SUBPEL_FILTERS_SMOOTH: [[i32; 8]; 16] = [
    [0, 0, 0, 128, 0, 0, 0, 0],
    [0, 2, 28, 62, 34, 2, 0, 0],
    [0, 0, 26, 62, 36, 4, 0, 0],
    [0, 0, 22, 62, 40, 4, 0, 0],
    [0, 0, 20, 60, 42, 6, 0, 0],
    [0, 0, 18, 58, 44, 8, 0, 0],
    [0, 0, 16, 56, 46, 10, 0, 0],
    [0, -2, 16, 54, 48, 12, 0, 0],
    [0, -2, 14, 52, 52, 14, -2, 0],
    [0, 0, 12, 48, 54, 16, -2, 0],
    [0, 0, 10, 46, 56, 16, 0, 0],
    [0, 0, 8, 44, 58, 18, 0, 0],
    [0, 0, 6, 42, 60, 20, 0, 0],
    [0, 0, 4, 40, 62, 22, 0, 0],
    [0, 0, 4, 36, 62, 26, 0, 0],
    [0, 0, 2, 34, 62, 28, 2, 0],
];

/// `SUBPEL_FILTERS_SMOOTH`'s narrow-block counterpart (`av1_sub_pel_filters_4smooth`).
const SUBPEL_FILTERS_SMOOTH_4: [[i32; 8]; 16] = [
    [0, 0, 0, 128, 0, 0, 0, 0],
    [0, 0, 30, 62, 34, 2, 0, 0],
    [0, 0, 26, 62, 36, 4, 0, 0],
    [0, 0, 22, 62, 40, 4, 0, 0],
    [0, 0, 20, 60, 42, 6, 0, 0],
    [0, 0, 18, 58, 44, 8, 0, 0],
    [0, 0, 16, 56, 46, 10, 0, 0],
    [0, 0, 14, 54, 48, 12, 0, 0],
    [0, 0, 12, 52, 52, 12, 0, 0],
    [0, 0, 12, 48, 54, 14, 0, 0],
    [0, 0, 10, 46, 56, 16, 0, 0],
    [0, 0, 8, 44, 58, 18, 0, 0],
    [0, 0, 6, 42, 60, 20, 0, 0],
    [0, 0, 4, 40, 62, 22, 0, 0],
    [0, 0, 4, 36, 62, 26, 0, 0],
    [0, 0, 2, 34, 62, 30, 0, 0],
];

/// `Subpel_Filters[EIGHTTAP_SHARP]` (`av1_sub_pel_filters_8sharp`). A block
/// whose dimension is 4 or less does NOT read this table: spec 7.11.3.4's
/// `filterIdx` sends both EIGHTTAP and EIGHTTAP_SHARP to filter index 4, and
/// libaom agrees -- `av1_interp_4tap[MULTITAP_SHARP]` is `av1_sub_pel_filters_4`,
/// the REGULAR narrow kernel (`filter.h:243`), not a sharp one (lane-gmaffine r5).
const SUBPEL_FILTERS_SHARP: [[i32; 8]; 16] = [
    [0, 0, 0, 128, 0, 0, 0, 0],
    [-2, 2, -6, 126, 8, -2, 2, 0],
    [-2, 6, -12, 124, 16, -6, 4, -2],
    [-2, 8, -18, 120, 26, -10, 6, -2],
    [-4, 10, -22, 116, 38, -14, 6, -2],
    [-4, 10, -22, 108, 48, -18, 8, -2],
    [-4, 10, -24, 100, 60, -20, 8, -2],
    [-4, 10, -24, 90, 70, -22, 10, -2],
    [-4, 12, -24, 80, 80, -24, 12, -4],
    [-2, 10, -22, 70, 90, -24, 10, -4],
    [-2, 8, -20, 60, 100, -24, 10, -4],
    [-2, 8, -18, 48, 108, -22, 10, -4],
    [-2, 6, -14, 38, 116, -22, 10, -4],
    [-2, 6, -10, 26, 120, -18, 8, -2],
    [-2, 4, -6, 16, 124, -12, 6, -2],
    [0, 2, -2, 8, 126, -6, 2, -2],
];

/// `Subpel_Filters[BILINEAR]` (`av1_bilinear_filters`) -- also never swapped
/// for a narrow-block table (it is already only 2 non-zero taps).
const SUBPEL_FILTERS_BILINEAR: [[i32; 8]; 16] = [
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
];

/// Which of the spec's four `interpolation_filter` kernels a block's motion
/// compensation reads (spec 6.8.9 / `frame_header`'s `interpolation_filter`).
/// A frame codes exactly one of these when not `Switchable`; a `Switchable`
/// frame codes one per block per direction (spec 5.11.20's
/// `read_interp_filter`, see `decode::decode_inter_block`), so `predict`'s
/// callers resolve `Switchable` to a concrete pair of kinds themselves --
/// this type only ever names the resolved kernel, never `Switchable` itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterpFilterKind {
    /// The `Regular` interpolation kernel.
    Regular,
    /// The `Smooth` interpolation kernel.
    Smooth,
    /// The `Sharp` interpolation kernel.
    Sharp,
    /// The `Bilinear` interpolation kernel.
    Bilinear,
}

impl InterpFilterKind {
    fn tables(self) -> (&'static [[i32; 8]; 16], &'static [[i32; 8]; 16]) {
        match self {
            InterpFilterKind::Regular => (&SUBPEL_FILTERS, &SUBPEL_FILTERS_4),
            InterpFilterKind::Smooth => (&SUBPEL_FILTERS_SMOOTH, &SUBPEL_FILTERS_SMOOTH_4),
            InterpFilterKind::Sharp => (&SUBPEL_FILTERS_SHARP, &SUBPEL_FILTERS_4),
            InterpFilterKind::Bilinear => (&SUBPEL_FILTERS_BILINEAR, &SUBPEL_FILTERS_BILINEAR),
        }
    }

    /// The three-symbol alphabet spec 5.11.20's `interp_filter[dir]` reads
    /// under a `Switchable` frame decodes to (`EIGHTTAP`/`_SMOOTH`/`_SHARP`
    /// in that order -- `BILINEAR` is never a `switchable_interp` outcome).
    ///
    /// # Panics
    /// Panics when `symbol` is not `0..=2`.
    pub fn from_switchable_symbol(symbol: usize) -> InterpFilterKind {
        match symbol {
            0 => InterpFilterKind::Regular,
            1 => InterpFilterKind::Smooth,
            2 => InterpFilterKind::Sharp,
            _ => panic!("switchable_interp's alphabet is exactly 3 symbols"),
        }
    }

    /// The frame header's own `interpolation_filter` (spec 6.8.9), for the
    /// non-`Switchable` values a fixed-filter frame codes directly.
    ///
    /// # Panics
    /// Panics on `Switchable`, which is not a concrete kernel -- callers
    /// resolve it per block instead (spec 5.11.20).
    pub fn from_header(filter: ec_av1_syntax::InterpolationFilter) -> InterpFilterKind {
        match filter {
            ec_av1_syntax::InterpolationFilter::Eighttap => InterpFilterKind::Regular,
            ec_av1_syntax::InterpolationFilter::EighttapSmooth => InterpFilterKind::Smooth,
            ec_av1_syntax::InterpolationFilter::EighttapSharp => InterpFilterKind::Sharp,
            ec_av1_syntax::InterpolationFilter::Bilinear => InterpFilterKind::Bilinear,
            ec_av1_syntax::InterpolationFilter::Switchable => {
                panic!("Switchable is resolved per block, not as one frame-wide kernel")
            }
        }
    }
}

/// A reference sample at `(x, y)`, clamped to the frame's true (unpadded)
/// extent -- `true_width`/`true_height`, which can be narrower than
/// `stride` -- rather than read out of range (spec 7.11.3.4's `Clip3`
/// against the frame's edges): a motion vector that points past the true
/// edge repeats the true edge's sample instead of panicking or picking up
/// whatever this encoder's own uncoded padding columns/rows hold.
#[allow(clippy::too_many_arguments)]
fn sample(
    reference: &[u16],
    stride: usize,
    true_width: usize,
    true_height: usize,
    x: i32,
    y: i32,
) -> i32 {
    let cx = x.clamp(0, true_width as i32 - 1) as usize;
    let cy = y.clamp(0, true_height as i32 - 1) as usize;
    i32::from(reference[cy * stride + cx])
}

/// lane-perf5: which explicit-SIMD kernels this CPU can run. The scalar
/// functions below stay the reference implementation -- every SIMD kernel is
/// checked against them by `simd_matches_scalar_*` -- and every non-x86_64
/// build, plus any x86_64 CPU without AVX2 or SSE4.1, decodes through them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)] // every variant is constructed only on x86_64
pub(crate) enum SimdLevel {
    Scalar,
    /// `_mm_madd_epi16` (SSE2) horizontal pass, `_mm_mullo_epi32` (SSE4.1)
    /// vertical pass -- the pair is gated on SSE4.1, the later of the two.
    Sse41,
    Avx2,
}

/// The level, detected once per process (spec-irrelevant: pure dispatch).
pub(crate) fn simd_level() -> SimdLevel {
    static LEVEL: std::sync::OnceLock<SimdLevel> = std::sync::OnceLock::new();
    *LEVEL.get_or_init(|| {
        #[cfg(target_arch = "x86_64")]
        {
            if std::arch::is_x86_feature_detected!("avx2") {
                return SimdLevel::Avx2;
            }
            if std::arch::is_x86_feature_detected!("sse4.1") {
                return SimdLevel::Sse41;
            }
        }
        SimdLevel::Scalar
    })
}

/// The horizontal pass's inner loop over a contiguous window (`src` holds the
/// row's whole footprint, `out.len() + 7` samples): SIMD where available,
/// [`hpass_contig_scalar`] otherwise. Both produce the same `i32`s -- the
/// SIMD form multiplies the same `i16` pairs, sums them in `i32` and applies
/// the same `Round2(sum, InterRound0)`.
#[allow(unsafe_code)]
#[inline]
pub(crate) fn hpass_contig(src: &[u16], t16: &[i16; 8], out: &mut [i16]) {
    let n = out.len();
    hpass_rows(src, 0, 1, n, t16, out);
}

/// lane-mc2: `rows` rows of [`hpass_contig`], `src_stride` apart in `src` and
/// packed `block_w` apart in `out`. The tap vectors and the SIMD dispatch are
/// set up once for the whole block instead of once per row.
#[allow(unsafe_code)]
#[inline]
pub(crate) fn hpass_rows(
    src: &[u16],
    src_stride: usize,
    rows: usize,
    block_w: usize,
    t16: &[i16; 8],
    out: &mut [i16],
) {
    #[cfg(target_arch = "x86_64")]
    {
        match simd_level() {
            SimdLevel::Avx2 => {
                // SAFETY: the AVX2 arm is reached only when the CPU reports
                // avx2; each row reads at most `block_w + 7` samples.
                unsafe { simd::hpass_avx2(src, src_stride, rows, block_w, t16, out) };
                return;
            }
            SimdLevel::Sse41 => {
                // SAFETY: as above; the kernel needs only SSE2, which every
                // x86_64 CPU has.
                unsafe { simd::hpass_sse2(src, src_stride, rows, block_w, t16, out) };
                return;
            }
            SimdLevel::Scalar => {}
        }
    }
    for r in 0..rows {
        hpass_contig_scalar(
            &src[r * src_stride..],
            t16,
            &mut out[r * block_w..(r + 1) * block_w],
        );
    }
}

/// The scalar reference for [`hpass_contig`] (lane-perf2's loop).
fn hpass_contig_scalar(src: &[u16], t16: &[i16; 8], out: &mut [i16]) {
    // Both factors are 16-bit (a tap is |x| <= 128, a sample fits the bit
    // depth), so the products are exactly the i32 ones but the widening
    // multiply is one instruction per pair.
    for (o, w) in out.iter_mut().zip(src.windows(8)) {
        let mut sum = 0i32;
        for t in 0..8 {
            sum += i32::from(t16[t]) * i32::from(w[t] as i16);
        }
        *o = sat16(round2(sum, INTER_ROUND_0));
    }
}

/// lane-perf5: explicit `std::arch::x86_64` kernels for the two motion
/// compensation passes -- the 18.7% of a 1080p decode that lane-perf4 left in
/// `hpass_row` + `vpass_row`. Nothing here is spec logic: each kernel is the
/// scalar function's arithmetic, lane-parallel, with identical rounding.
#[allow(unsafe_code)]
#[cfg(target_arch = "x86_64")]
#[allow(unsafe_op_in_unsafe_fn)] // each kernel's whole body is its contract
mod simd {
    use super::{round2, sat16, INTER_ROUND_0, INTER_ROUND_1};
    use std::arch::x86_64::*;

    /// `[t0,t1]`, `[t2,t3]`, `[t4,t5]`, `[t6,t7]` packed as `i32`s, the shape
    /// `_mm*_madd_epi16` wants: lane `j` of `madd(samples, set1(pair))` is
    /// `s[2j]*t_even + s[2j+1]*t_odd`, i.e. two taps of one output at once.
    #[inline]
    fn taps16(taps: &[i32; 8]) -> [i16; 8] {
        [
            taps[0] as i16, taps[1] as i16, taps[2] as i16, taps[3] as i16,
            taps[4] as i16, taps[5] as i16, taps[6] as i16, taps[7] as i16,
        ]
    }

    #[inline]
    fn tap_pairs(t: &[i16; 8]) -> [i32; 4] {
        let pack = |a: i16, b: i16| (a as u16 as i32) | ((b as i32) << 16);
        [pack(t[0], t[1]), pack(t[2], t[3]), pack(t[4], t[5]), pack(t[6], t[7])]
    }

    /// Horizontal pass, AVX2: 16 outputs per iteration. Each 256-bit `madd`
    /// covers 8 outputs two taps at a time, but only every other output (the
    /// pairs are consecutive samples), so even and odd output columns are
    /// accumulated in two registers and interleaved back into order by
    /// `unpack` + `permute2x128`.
    ///
    /// # Safety
    /// Requires AVX2. `src.len()` must be at least `out.len() + 7`.
    #[target_feature(enable = "avx2")]
    pub unsafe fn hpass_avx2(
        src: &[u16],
        src_stride: usize,
        rows: usize,
        block_w: usize,
        t16: &[i16; 8],
        out: &mut [i16],
    ) {
        debug_assert!(out.len() >= rows * block_w);
        debug_assert!(src.len() >= (rows - 1) * src_stride + block_w + 7);
        let tp = tap_pairs(t16);
        let tv = [
            _mm256_set1_epi32(tp[0]),
            _mm256_set1_epi32(tp[1]),
            _mm256_set1_epi32(tp[2]),
            _mm256_set1_epi32(tp[3]),
        ];
        let rnd = _mm256_set1_epi32(1 << (INTER_ROUND_0 - 1));
        for r in 0..rows {
            let sp = src.as_ptr().add(r * src_stride);
            let op = out.as_mut_ptr().add(r * block_w);
            let mut c = 0usize;
            while c + 16 <= block_w {
                let mut e = _mm256_setzero_si256();
                let mut o = _mm256_setzero_si256();
                for k in 0..4 {
                    let se = _mm256_loadu_si256(sp.add(c + 2 * k).cast());
                    e = _mm256_add_epi32(e, _mm256_madd_epi16(se, tv[k]));
                    let so = _mm256_loadu_si256(sp.add(c + 2 * k + 1).cast());
                    o = _mm256_add_epi32(o, _mm256_madd_epi16(so, tv[k]));
                }
                e = _mm256_srai_epi32(_mm256_add_epi32(e, rnd), INTER_ROUND_0 as i32);
                o = _mm256_srai_epi32(_mm256_add_epi32(o, rnd), INTER_ROUND_0 as i32);
                let lo = _mm256_unpacklo_epi32(e, o);
                let hi = _mm256_unpackhi_epi32(e, o);
                // `lo`/`hi` already hold the 16 outputs in order, four per
                // 128-bit lane; `packs_epi32` narrows lane-wise, so the store
                // is c..c+16 in order. Saturation never fires: see
                // `horizontal_intermediate_fits_i16`.
                _mm256_storeu_si256(op.add(c).cast(), _mm256_packs_epi32(lo, hi));
                c += 16;
            }
            if c < block_w {
                let s = r * src_stride + c;
                let o = r * block_w + c;
                hpass_sse2(&src[s..], 0, 1, block_w - c, t16, &mut out[o..o + block_w - c]);
            }
        }
    }

    /// Vertical pass, AVX2: the intermediate is `i16` (lane-mc: it provably
    /// fits, see `horizontal_intermediate_fits_i16`), so a row PAIR
    /// interleaved by `unpack_epi16` is exactly `_mm256_madd_epi16`'s shape --
    /// two taps of 16 columns per instruction, where the old `i32` form spent
    /// one `mullo_epi32` per tap per 8 columns. `acc` is left unrounded, as
    /// the scalar form leaves it.
    ///
    /// # Safety
    /// Requires AVX2. `intermediate` must hold `(row + 8) * block_w` elements
    /// and `acc` at least `block_w`.
    #[target_feature(enable = "avx2")]
    pub unsafe fn vpass_avx2(
        intermediate: &[i16],
        block_w: usize,
        src_stride: usize,
        row: usize,
        taps: &[i32; 8],
        acc: &mut [i32],
    ) {
        debug_assert!(intermediate.len() >= (row + 7) * src_stride + block_w && acc.len() >= block_w);
        let tp = tap_pairs(&taps16(taps));
        let ip = intermediate.as_ptr().add(row * src_stride);
        let ap = acc.as_mut_ptr();
        let mut c = 0usize;
        while c + 16 <= block_w {
            // `lo` accumulates columns c+0..4 and c+8..12 (one per 128-bit
            // lane), `hi` the other two runs; the two `permute2x128`s put the
            // 16 sums back in column order.
            let mut lo = _mm256_setzero_si256();
            let mut hi = _mm256_setzero_si256();
            for k in 0..4 {
                if tp[k] == 0 {
                    continue;
                }
                let tv = _mm256_set1_epi32(tp[k]);
                let a = _mm256_loadu_si256(ip.add(2 * k * src_stride + c).cast());
                let b = _mm256_loadu_si256(ip.add((2 * k + 1) * src_stride + c).cast());
                lo = _mm256_add_epi32(lo, _mm256_madd_epi16(_mm256_unpacklo_epi16(a, b), tv));
                hi = _mm256_add_epi32(hi, _mm256_madd_epi16(_mm256_unpackhi_epi16(a, b), tv));
            }
            _mm256_storeu_si256(ap.add(c).cast(), _mm256_permute2x128_si256(lo, hi, 0x20));
            _mm256_storeu_si256(ap.add(c + 8).cast(), _mm256_permute2x128_si256(lo, hi, 0x31));
            c += 16;
        }
        vpass_tail_sse(ip, block_w, src_stride, &tp, taps, ap, c);
    }

    /// [`vpass_avx2`]'s 8- and 4-column steps, 128-bit: every AV1 block width
    /// is a multiple of 4, and the 8x8 luma block (the most common inter
    /// shape on real 4K content) lands here whole.
    ///
    /// # Safety
    /// `ip` must be readable for `8 * block_w` elements from `row`'s start and
    /// `ap` writable for `block_w`; `c` is the column already done.
    #[inline]
    unsafe fn vpass_tail_sse(
        ip: *const i16,
        block_w: usize,
        src_stride: usize,
        tp: &[i32; 4],
        taps: &[i32; 8],
        ap: *mut i32,
        mut c: usize,
    ) {
        while c + 8 <= block_w {
            let mut lo = _mm_setzero_si128();
            let mut hi = _mm_setzero_si128();
            for k in 0..4 {
                if tp[k] == 0 {
                    continue;
                }
                let tv = _mm_set1_epi32(tp[k]);
                let a = _mm_loadu_si128(ip.add(2 * k * src_stride + c).cast());
                let b = _mm_loadu_si128(ip.add((2 * k + 1) * src_stride + c).cast());
                lo = _mm_add_epi32(lo, _mm_madd_epi16(_mm_unpacklo_epi16(a, b), tv));
                hi = _mm_add_epi32(hi, _mm_madd_epi16(_mm_unpackhi_epi16(a, b), tv));
            }
            _mm_storeu_si128(ap.add(c).cast(), lo);
            _mm_storeu_si128(ap.add(c + 4).cast(), hi);
            c += 8;
        }
        while c + 4 <= block_w {
            let mut lo = _mm_setzero_si128();
            for k in 0..4 {
                if tp[k] == 0 {
                    continue;
                }
                let tv = _mm_set1_epi32(tp[k]);
                let a = _mm_loadl_epi64(ip.add(2 * k * src_stride + c).cast());
                let b = _mm_loadl_epi64(ip.add((2 * k + 1) * src_stride + c).cast());
                lo = _mm_add_epi32(lo, _mm_madd_epi16(_mm_unpacklo_epi16(a, b), tv));
            }
            _mm_storeu_si128(ap.add(c).cast(), lo);
            c += 4;
        }
        // Every AV1 block width is a multiple of 4, but the encoder's own
        // motion search prices odd widths through `predict`, so the last
        // columns still need the scalar form.
        while c < block_w {
            let mut sum = 0i32;
            for (t, &tap) in taps.iter().enumerate() {
                sum += tap * i32::from(*ip.add(t * src_stride + c));
            }
            *ap.add(c) = sum;
            c += 1;
        }
    }

    /// [`vpass_avx2`] with the output rounding fused in: the sums never reach
    /// memory as `i32`, they are rounded by `INTER_ROUND_1`, clamped and
    /// stored as the block's own `u16` samples. `packus_epi32` is the clamp's
    /// lower half (a negative sum saturates to 0) and `min_epu16` its upper,
    /// which is exactly `round2(sum, INTER_ROUND_1).clamp(0, max)`.
    ///
    /// # Safety
    /// Requires AVX2. `intermediate` must hold `(row + 8) * block_w` elements
    /// and `dst` at least `block_w`.
    #[target_feature(enable = "avx2")]
    pub unsafe fn vpass_row_u16_avx2(
        intermediate: &[i16],
        block_w: usize,
        src_stride: usize,
        rows: usize,
        taps: &[i32; 8],
        max: i32,
        dst: &mut [u16],
    ) {
        debug_assert!(intermediate.len() >= (rows + 6) * src_stride + block_w && dst.len() >= rows * block_w);
        let tp = tap_pairs(&taps16(taps));
        let rnd = _mm256_set1_epi32(1 << (INTER_ROUND_1 - 1));
        let mx = _mm256_set1_epi16(max as i16);
        for row in 0..rows {
        let ip = intermediate.as_ptr().add(row * src_stride);
        let dp = dst.as_mut_ptr().add(row * block_w);
        let mut c = 0usize;
        while c + 16 <= block_w {
            let mut lo = rnd;
            let mut hi = rnd;
            for k in 0..4 {
                if tp[k] == 0 {
                    continue;
                }
                let tv = _mm256_set1_epi32(tp[k]);
                let a = _mm256_loadu_si256(ip.add(2 * k * src_stride + c).cast());
                let b = _mm256_loadu_si256(ip.add((2 * k + 1) * src_stride + c).cast());
                lo = _mm256_add_epi32(lo, _mm256_madd_epi16(_mm256_unpacklo_epi16(a, b), tv));
                hi = _mm256_add_epi32(hi, _mm256_madd_epi16(_mm256_unpackhi_epi16(a, b), tv));
            }
            lo = _mm256_srai_epi32(lo, INTER_ROUND_1 as i32);
            hi = _mm256_srai_epi32(hi, INTER_ROUND_1 as i32);
            // `packus` narrows lane-wise, and `lo`/`hi` hold the columns four
            // per lane in order, so this is c..c+16 in order.
            let v = _mm256_min_epu16(_mm256_packus_epi32(lo, hi), mx);
            _mm256_storeu_si256(dp.add(c).cast(), v);
            c += 16;
        }
        vpass_u16_tail_sse(ip, block_w, src_stride, &tp, taps, max, dp, c);
        }
    }

    /// [`vpass_row_u16_avx2`]'s 8- and 4-column steps (SSE4.1 for
    /// `packus_epi32`), and the scalar remainder.
    ///
    /// # Safety
    /// Requires SSE4.1; same slice contract as [`vpass_row_u16_avx2`].
    #[target_feature(enable = "sse4.1")]
    unsafe fn vpass_u16_tail_sse(
        ip: *const i16,
        block_w: usize,
        src_stride: usize,
        tp: &[i32; 4],
        taps: &[i32; 8],
        max: i32,
        dp: *mut u16,
        mut c: usize,
    ) {
        let rnd = _mm_set1_epi32(1 << (INTER_ROUND_1 - 1));
        let mx = _mm_set1_epi16(max as i16);
        while c + 4 <= block_w {
            let wide = c + 8 <= block_w;
            let mut lo = rnd;
            let mut hi = rnd;
            for k in 0..4 {
                if tp[k] == 0 {
                    continue;
                }
                let tv = _mm_set1_epi32(tp[k]);
                let (pa, pb) = (ip.add(2 * k * src_stride + c), ip.add((2 * k + 1) * src_stride + c));
                let (a, b) = if wide {
                    (_mm_loadu_si128(pa.cast()), _mm_loadu_si128(pb.cast()))
                } else {
                    (_mm_loadl_epi64(pa.cast()), _mm_loadl_epi64(pb.cast()))
                };
                lo = _mm_add_epi32(lo, _mm_madd_epi16(_mm_unpacklo_epi16(a, b), tv));
                if wide {
                    hi = _mm_add_epi32(hi, _mm_madd_epi16(_mm_unpackhi_epi16(a, b), tv));
                }
            }
            let v = _mm_min_epu16(
                _mm_packus_epi32(_mm_srai_epi32(lo, INTER_ROUND_1 as i32), _mm_srai_epi32(hi, INTER_ROUND_1 as i32)),
                mx,
            );
            if wide {
                _mm_storeu_si128(dp.add(c).cast(), v);
                c += 8;
            } else {
                _mm_storel_epi64(dp.add(c).cast(), v);
                c += 4;
            }
        }
        while c < block_w {
            let mut sum = 0i32;
            for (t, &tap) in taps.iter().enumerate() {
                sum += tap * i32::from(*ip.add(t * src_stride + c));
            }
            *dp.add(c) = round2(sum, INTER_ROUND_1).clamp(0, max) as u16;
            c += 1;
        }
    }

    /// [`vpass_row_u16_avx2`] without the 16-column step.
    ///
    /// # Safety
    /// Requires SSE4.1; same slice contract as [`vpass_row_u16_avx2`].
    #[target_feature(enable = "sse4.1")]
    pub unsafe fn vpass_row_u16_sse41(
        intermediate: &[i16],
        block_w: usize,
        src_stride: usize,
        rows: usize,
        taps: &[i32; 8],
        max: i32,
        dst: &mut [u16],
    ) {
        debug_assert!(intermediate.len() >= (rows + 6) * src_stride + block_w && dst.len() >= rows * block_w);
        let tp = tap_pairs(&taps16(taps));
        for row in 0..rows {
            vpass_u16_tail_sse(
                intermediate.as_ptr().add(row * src_stride), block_w, src_stride, &tp, taps, max,
                dst.as_mut_ptr().add(row * block_w), 0,
            );
        }
    }

    /// Vertical pass, SSE2: [`vpass_avx2`] without the 16-column step.
    ///
    /// # Safety
    /// SSE2 is baseline x86_64; same slice contract as [`vpass_avx2`].
    pub unsafe fn vpass_sse41(
        intermediate: &[i16],
        block_w: usize,
        src_stride: usize,
        row: usize,
        taps: &[i32; 8],
        acc: &mut [i32],
    ) {
        debug_assert!(intermediate.len() >= (row + 7) * src_stride + block_w && acc.len() >= block_w);
        let tp = tap_pairs(&taps16(taps));
        vpass_tail_sse(intermediate.as_ptr().add(row * src_stride), block_w, src_stride, &tp, taps, acc.as_mut_ptr(), 0);
    }

    /// Horizontal pass, SSE2: [`hpass_avx2`]'s scheme at 8 outputs per
    /// iteration; the remainder (a width-4 block, or a 4-column tail) is the
    /// scalar loop, which is where 4-tap kernels mostly land anyway.
    ///
    /// # Safety
    /// `src.len()` must be at least `out.len() + 7`. SSE2 is baseline x86_64.
    pub unsafe fn hpass_sse2(
        src: &[u16],
        src_stride: usize,
        rows: usize,
        block_w: usize,
        t16: &[i16; 8],
        out: &mut [i16],
    ) {
        debug_assert!(out.len() >= rows * block_w);
        debug_assert!(src.len() >= (rows - 1) * src_stride + block_w + 7);
        let tp = tap_pairs(t16);
        let tv = [
            _mm_set1_epi32(tp[0]),
            _mm_set1_epi32(tp[1]),
            _mm_set1_epi32(tp[2]),
            _mm_set1_epi32(tp[3]),
        ];
        let rnd = _mm_set1_epi32(1 << (INTER_ROUND_0 - 1));
        for r in 0..rows {
            let sp = src.as_ptr().add(r * src_stride);
            let op = out.as_mut_ptr().add(r * block_w);
            let mut c = 0usize;
            while c + 8 <= block_w {
                let mut e = _mm_setzero_si128();
                let mut o = _mm_setzero_si128();
                for k in 0..4 {
                    let se = _mm_loadu_si128(sp.add(c + 2 * k).cast());
                    e = _mm_add_epi32(e, _mm_madd_epi16(se, tv[k]));
                    let so = _mm_loadu_si128(sp.add(c + 2 * k + 1).cast());
                    o = _mm_add_epi32(o, _mm_madd_epi16(so, tv[k]));
                }
                e = _mm_srai_epi32(_mm_add_epi32(e, rnd), INTER_ROUND_0 as i32);
                o = _mm_srai_epi32(_mm_add_epi32(o, rnd), INTER_ROUND_0 as i32);
                let lo = _mm_unpacklo_epi32(e, o);
                let hi = _mm_unpackhi_epi32(e, o);
                _mm_storeu_si128(op.add(c).cast(), _mm_packs_epi32(lo, hi));
                c += 8;
            }
            while c < block_w {
                let mut sum = 0i32;
                for t in 0..8 {
                    sum += i32::from(t16[t]) * i32::from(*sp.add(c + t) as i16);
                }
                *op.add(c) = sat16(round2(sum, INTER_ROUND_0));
                c += 1;
            }
        }
    }
}

/// lane-perf2: one row of the separable filter's horizontal pass, dav1d
/// style. When the row's whole 8-tap footprint (`x0-3 .. x0+block_w+4`) lies
/// inside the reference's true extent -- the overwhelmingly common case --
/// the row is one contiguous slice and the taps are a fixed `[i32; 8]`, so
/// the per-sample `Clip3` and the per-tap table load both leave the inner
/// loop and it auto-vectorises. The edge case keeps the exact spec form
/// (`sample`'s clamp per tap); both arms compute the same sums.
#[inline]
fn hpass_row(reference: &[u16], row_base: usize, true_width: usize, x0: i32, taps: &[i32; 8], out: &mut [i16]) {
    let block_w = out.len();
    let start = x0 - 3;
    if start >= 0 && start as usize + block_w + 7 <= true_width {
        let s = row_base + start as usize;
        if let Some(src) = reference.get(s..s + block_w + 7) {
            // Both factors are 16-bit (a tap is |x| <= 128, a sample fits the
            // bit depth), so the products are exactly the i32 ones above but
            // the widening multiply is one instruction per pair.
            let t16 = [
                taps[0] as i16, taps[1] as i16, taps[2] as i16, taps[3] as i16,
                taps[4] as i16, taps[5] as i16, taps[6] as i16, taps[7] as i16,
            ];
            hpass_contig(src, &t16, out);
            return;
        }
    }
    for (c, o) in out.iter_mut().enumerate() {
        let mut sum = 0i32;
        for (t, &tap) in taps.iter().enumerate() {
            let x = (x0 + c as i32 + t as i32 - 3).clamp(0, true_width as i32 - 1) as usize;
            sum += tap * i32::from(reference[row_base + x]);
        }
        *o = sat16(round2(sum, INTER_ROUND_0));
    }
}

/// lane-perf2: the unscaled horizontal pass, `block_h + 7` rows of
/// [`hpass_row`] with the vertical clamp hoisted out of the row.
#[allow(clippy::too_many_arguments)]
fn horizontal_pass_unscaled(
    reference: &[u16],
    stride: usize,
    true_width: usize,
    true_height: usize,
    x0: i32,
    y0: i32,
    taps: &[i32; 8],
    block_w: usize,
    rows: usize,
    intermediate: &mut [i16],
) {
    // Fraction 0 is the identity tap (`128` at slot 3), and `Round2(128 * s,
    // InterRound0) == 16 * s` exactly, so a whole-pel horizontal position
    // needs no filter at all -- one widened row read.
    let identity = taps[3] == 128 && taps.iter().enumerate().all(|(i, &t)| i == 3 || t == 0);
    // lane-mc2: when the whole `rows x (block_w+7)` footprint lies inside the
    // reference -- the overwhelmingly common case -- every per-row test in
    // the general loop below (the vertical clamp, the horizontal interior
    // test, the slice bounds check, the tap narrowing and the SIMD dispatch)
    // has the same answer for every row, so it is hoisted out and the loop
    // becomes a walk of `stride`. Same sums, same order.
    let start = x0 - 3;
    let y_top = y0 - 3;
    if !identity
        && start >= 0
        && y_top >= 0
        && start as usize + block_w + 7 <= true_width
        && y_top as usize + rows <= true_height
    {
        let base = y_top as usize * stride + start as usize;
        let span = block_w + 7;
        if base + (rows - 1) * stride + span <= reference.len() {
            let t16 = [
                taps[0] as i16, taps[1] as i16, taps[2] as i16, taps[3] as i16,
                taps[4] as i16, taps[5] as i16, taps[6] as i16, taps[7] as i16,
            ];
            hpass_rows(
                &reference[base..],
                stride,
                rows,
                block_w,
                &t16,
                &mut intermediate[..rows * block_w],
            );
            return;
        }
    }
    for r in 0..rows {
        let y = (y0 - 3 + r as i32).clamp(0, true_height as i32 - 1) as usize;
        let out = &mut intermediate[r * block_w..(r + 1) * block_w];
        if identity {
            let gain = 128 >> INTER_ROUND_0;
            if x0 >= 0 && x0 as usize + block_w <= true_width {
                if let Some(src) = reference.get(y * stride + x0 as usize..y * stride + x0 as usize + block_w) {
                    for (o, &s) in out.iter_mut().zip(src) {
                        *o = (gain * i32::from(s)) as i16;
                    }
                    continue;
                }
            }
            for (c, o) in out.iter_mut().enumerate() {
                let x = (x0 + c as i32).clamp(0, true_width as i32 - 1) as usize;
                *o = (gain * i32::from(reference[y * stride + x])) as i16;
            }
            continue;
        }
        hpass_row(reference, y * stride, true_width, x0, taps, out);
    }
}

/// lane-perf2: one output row of the vertical pass, accumulated tap-major so
/// the inner loop walks two contiguous `i32` slices (vectorisable) instead of
/// striding the intermediate by `block_w` per tap. A zero tap is skipped --
/// adding `0 * x` is exactly nothing, and the 4-tap kernels zero four of the
/// eight slots. `acc` is left holding the unrounded sums.
#[inline]
#[allow(unsafe_code)]
pub(crate) fn vpass_row(intermediate: &[i16], block_w: usize, src_stride: usize, row: usize, taps: &[i32; 8], acc: &mut [i32]) {
    #[cfg(target_arch = "x86_64")]
    {
        match simd_level() {
            // SAFETY: the arm is reached only when the CPU reports the
            // feature; the kernel reads `intermediate[(row+t)*block_w ..
            // + block_w]` for t in 0..8 and writes `acc[..block_w]`, which
            // the debug asserts pin.
            SimdLevel::Avx2 => {
                unsafe { simd::vpass_avx2(intermediate, block_w, src_stride, row, taps, acc) };
                return;
            }
            SimdLevel::Sse41 => {
                unsafe { simd::vpass_sse41(intermediate, block_w, src_stride, row, taps, acc) };
                return;
            }
            SimdLevel::Scalar => {}
        }
    }
    vpass_row_scalar(intermediate, block_w, src_stride, row, taps, acc);
}

/// lane-mc: [`vpass_row`] with the output rounding fused in, for the two
/// entry points whose destination is `u16` samples. The unrounded `i32` sums
/// used to be stored to a scratch row and read straight back by a rounding
/// loop; here they never leave the register.
#[inline]
#[allow(unsafe_code)]
fn vpass_row_u16(
    intermediate: &[i16],
    block_w: usize,
    src_stride: usize,
    rows: usize,
    taps: &[i32; 8],
    max: i32,
    dst: &mut [u16],
) {
    #[cfg(target_arch = "x86_64")]
    {
        match simd_level() {
            // SAFETY: the arm is reached only when the CPU reports the
            // feature; the kernel reads `intermediate[(row+t)*block_w ..
            // + block_w]` for t in 0..8 and writes `dst[..block_w]`, which
            // the debug asserts pin.
            SimdLevel::Avx2 => {
                unsafe { simd::vpass_row_u16_avx2(intermediate, block_w, src_stride, rows, taps, max, dst) };
                return;
            }
            SimdLevel::Sse41 => {
                unsafe { simd::vpass_row_u16_sse41(intermediate, block_w, src_stride, rows, taps, max, dst) };
                return;
            }
            SimdLevel::Scalar => {}
        }
    }
    for row in 0..rows {
        vpass_row_u16_scalar(
            &intermediate[row * src_stride..], block_w, src_stride, 0, taps, max,
            &mut dst[row * block_w..(row + 1) * block_w],
        );
    }
}

/// The scalar reference for [`vpass_row_u16`].
fn vpass_row_u16_scalar(
    intermediate: &[i16],
    block_w: usize,
    src_stride: usize,
    row: usize,
    taps: &[i32; 8],
    max: i32,
    dst: &mut [u16],
) {
    for (c, d) in dst[..block_w].iter_mut().enumerate() {
        let mut sum = 0i32;
        for (t, &tap) in taps.iter().enumerate() {
            sum += tap * i32::from(intermediate[(row + t) * src_stride + c]);
        }
        *d = round2(sum, INTER_ROUND_1).clamp(0, max) as u16;
    }
}

/// The scalar reference for [`vpass_row`] (lane-perf2's tap-major loop).
fn vpass_row_scalar(intermediate: &[i16], block_w: usize, src_stride: usize, row: usize, taps: &[i32; 8], acc: &mut [i32]) {
    acc[..block_w].fill(0);
    for (t, &tap) in taps.iter().enumerate() {
        if tap == 0 {
            continue;
        }
        let base = (row + t) * src_stride;
        for (a, &s) in acc[..block_w].iter_mut().zip(&intermediate[base..base + block_w]) {
            *a += tap * i32::from(s);
        }
    }
}

thread_local! {
    /// lane-perf2: the separable filter's intermediate buffer, reused across
    /// calls -- a 4K inter frame runs `predict*` hundreds of thousands of
    /// times and each call used to `vec![0i32; (block_h+7) * block_w]`. The
    /// horizontal pass writes every element it later reads, so no clearing is
    /// needed; the tail `block_w` elements are the vertical pass's row
    /// accumulator.
    static MC_SCRATCH: std::cell::RefCell<(Vec<i16>, Vec<i32>)> =
        const { std::cell::RefCell::new((Vec::new(), Vec::new())) };
}

/// Runs `f` on a scratch slice of `rows * block_w` intermediate elements plus
/// a `block_w` accumulator (see [`MC_SCRATCH`]).
fn with_scratch<R>(rows: usize, block_w: usize, f: impl FnOnce(&mut [i16], &mut [i32]) -> R) -> R {
    MC_SCRATCH.with(|s| {
        let mut buf = s.borrow_mut();
        let (inter, acc) = &mut *buf;
        let need = rows * block_w;
        if inter.len() < need {
            inter.resize(need, 0);
        }
        if acc.len() < block_w {
            acc.resize(block_w, 0);
        }
        f(&mut inter[..need], &mut acc[..block_w])
    })
}

/// Predicts one `block_w * block_h` block into `dst`, row-major, from
/// `reference` (row-major, stride `stride`, true content
/// `true_width` x `true_height` -- a reference frame's decoded picture
/// buffer can be wider/taller than the true frame it holds, when this
/// encoder pads a picture to a whole number of superblocks; a spec decoder
/// never codes syntax for those extra columns/rows, so motion compensation
/// must clamp to the true extent, not the buffer's own stride).
///
/// `x_q4` / `y_q4` are the block's top-left position in the reference, in
/// 1/16-pel units (an integer MV has its low 4 bits zero); this function
/// splits each into its whole-sample part and its 0..16 fraction itself, so
/// the caller need not pre-split a motion vector.
///
/// # Panics
/// Panics when `dst` is not `block_w * block_h` long, or the reference is
/// empty.
#[allow(clippy::too_many_arguments)] // one reference plane, one position, one block shape
pub(crate) fn predict(
    reference: &[u16],
    stride: usize,
    true_width: usize,
    true_height: usize,
    x_q4: i32,
    y_q4: i32,
    block_w: usize,
    block_h: usize,
    dst: &mut [u16], fctx: &crate::decode::FrameCtx,
) {
    predict_with_filter(
        reference,
        stride,
        true_width,
        true_height,
        x_q4,
        y_q4,
        block_w,
        block_h,
        InterpFilterKind::Regular,
        dst, fctx,
    );
}

/// [`predict`], selecting the interpolation filter kernel explicitly (spec
/// 6.8.9's `interpolation_filter`) instead of always `Regular` -- the same
/// kernel both directions.
#[allow(clippy::too_many_arguments)]
pub(crate) fn predict_with_filter(
    reference: &[u16],
    stride: usize,
    true_width: usize,
    true_height: usize,
    x_q4: i32,
    y_q4: i32,
    block_w: usize,
    block_h: usize,
    filter_kind: InterpFilterKind,
    dst: &mut [u16], fctx: &crate::decode::FrameCtx,
) {
    predict_with_filters(
        reference,
        stride,
        true_width,
        true_height,
        x_q4,
        y_q4,
        block_w,
        block_h,
        filter_kind,
        filter_kind,
        dst, fctx,
    );
}

/// [`predict_with_filter`], with the horizontal (`interp_filter[0]`) and
/// vertical (`interp_filter[1]`) kernels chosen independently -- spec
/// 5.11.20's per-block `SWITCHABLE` read is per-direction (`enable_dual_filter`),
/// so the two passes can genuinely differ.
#[allow(clippy::too_many_arguments)]
pub(crate) fn predict_with_filters(
    reference: &[u16],
    stride: usize,
    true_width: usize,
    true_height: usize,
    x_q4: i32,
    y_q4: i32,
    block_w: usize,
    block_h: usize,
    h_kind: InterpFilterKind,
    v_kind: InterpFilterKind,
    dst: &mut [u16], fctx: &crate::decode::FrameCtx,
) {
    predict_with_filters_kern(
        reference, stride, true_width, true_height, x_q4, y_q4, block_w,
        block_h, block_w, block_h, h_kind, v_kind, dst, fctx,
    );
}

/// [`predict_with_filters`] with the 4-tap decision taken from the block's
/// TRUE width/height (`kern_w`/`kern_h`) rather than the destination buffer's.
/// libaom's `av1_get_interp_filter_params_with_block_size` (reconinter.h,
/// called per axis from `inter_predictor` with `w` then `h`) picks the narrow
/// kernel from the PREDICTION block's own dimensions; our inter path predicts
/// a rectangular block over its enclosing square buffer, so a rect block whose
/// chroma is 4 wide or 4 tall (luma 8x16 / 16x8 and taller kin) must still ask
/// for the 4-tap kernel on that axis -- lane-rectchroma2 r1.
#[allow(clippy::too_many_arguments)]
pub(crate) fn predict_with_filters_kern(
    reference: &[u16],
    stride: usize,
    true_width: usize,
    true_height: usize,
    x_q4: i32,
    y_q4: i32,
    block_w: usize,
    block_h: usize,
    kern_w: usize,
    kern_h: usize,
    h_kind: InterpFilterKind,
    v_kind: InterpFilterKind,
    dst: &mut [u16], fctx: &crate::decode::FrameCtx,
) {
    assert_eq!(dst.len(), block_w * block_h, "the destination is the block");
    assert!(!reference.is_empty(), "a reference plane has samples");
    INTER_PRED_HITS.with(|c| c.set(c.get() + 1));

    #[cfg(test)]
    let stage_t = std::time::Instant::now();

    let x0 = x_q4.div_euclid(16);
    let xfrac = x_q4.rem_euclid(16) as usize;
    let y0 = y_q4.div_euclid(16);
    let yfrac = y_q4.rem_euclid(16) as usize;
    if crate::envflags::env_flag!("EC_MC_TRACE") && block_w >= 4 {
        eprintln!(
            "EC_MC_CALL x_q4={x_q4} y_q4={y_q4} w={block_w} h={block_h} xfrac={xfrac} yfrac={yfrac} hk={h_kind:?} vk={v_kind:?} tw={true_width} th={true_height} stride={stride}"
        );
    }

    // Whole-pel fast path: `SUBPEL_FILTERS[0]` is the identity tap (`128` at
    // its centre, everything else `0`), and both rounding shifts are exactly
    // that filter's own gain (see `INTER_ROUND_1`'s doc comment), so the two
    // 8-tap passes reproduce the reference sample byte-for-byte here --
    // `integer_mv_is_identity` below pins that. Skipping straight to a
    // clamped copy is bit-identical, not an approximation: it matters
    // because the motion search's own log/diamond stage (`motion.rs`) is
    // whole-pel-only and calls `predict` for every candidate it prices, so
    // this path is what most of a search's `predict` calls actually hit
    // (measured: `stage_timing_breakdown_inter` attributed 403 of a 720p
    // inter frame's 408ms motion-search bucket to `predict`, almost all of
    // it whole-pel candidates from stage 1).
    if xfrac == 0 && yfrac == 0 {
        for row in 0..block_h {
            let y = (y0 + row as i32).clamp(0, true_height as i32 - 1) as usize;
            let row_base = y * stride;
            let out = &mut dst[row * block_w..(row + 1) * block_w];
            // lane-perf2: an interior row is one contiguous copy.
            if x0 >= 0 && x0 as usize + block_w <= true_width {
                if let Some(src) = reference.get(row_base + x0 as usize..row_base + x0 as usize + block_w) {
                    // The common widths get a constant-length copy: at 8
                    // samples a `memcpy` call costs more than the move it
                    // makes, and 8x8 is the most common inter block on real
                    // 4K content.
                    match block_w {
                        4 => out[..4].copy_from_slice(&src[..4]),
                        8 => out[..8].copy_from_slice(&src[..8]),
                        16 => out[..16].copy_from_slice(&src[..16]),
                        _ => out.copy_from_slice(src),
                    }
                    continue;
                }
            }
            for (col, o) in out.iter_mut().enumerate() {
                let x = (x0 + col as i32).clamp(0, true_width as i32 - 1) as usize;
                *o = reference[row_base + x];
            }
        }
        #[cfg(test)]
        crate::encode::stage_add(1, stage_t.elapsed());
        return;
    }

    let (h_wide, h_narrow) = h_kind.tables();
    let (v_wide, v_narrow) = v_kind.tables();
    note_narrow_kern(block_w, block_h, kern_w, kern_h);
    let h_filter = if kern_w <= 4 {
        &h_narrow[xfrac]
    } else {
        &h_wide[xfrac]
    };
    let v_filter = if kern_h <= 4 {
        &v_narrow[yfrac]
    } else {
        &v_wide[yfrac]
    };

    // The vertical pass reads 3 rows above and 4 below the block, so the
    // horizontal pass must produce that many extra intermediate rows.
    // A whole-pel vertical position needs neither the 3 rows above nor the 4
    // below, and its vertical pass is the identity tap: `Round2(128 * a,
    // InterRound1) == Round2(a, InterRound1 - 7)`. Passing `y0 + 3` puts the
    // block's own first row where the pass would have put the 4th.
    let rows = if yfrac == 0 { block_h } else { block_h + 7 };
    let max = crate::decode::sample_max(fctx);
    // lane-mc2: a whole-pel horizontal position makes the horizontal pass the
    // identity tap, whose output is `s << 4` (`Round2(128 * s, InterRound0)`
    // with the tap 128). The vertical pass then computes
    // `Round2(sum(tap * (s << 4)), InterRound1)`, which is the same 8-tap sum
    // taken straight off the reference rows with every tap scaled by 16 --
    // bit-identical, and it skips materialising `block_h + 7` intermediate
    // rows. Interior blocks only; an edge block still needs the clamped read.
    if xfrac != 0 || yfrac == 0 {
        // fall through to the two-pass path
    } else {
        let top = y0 - 3;
        if top >= 0
            && x0 >= 0
            && x0 as usize + block_w <= true_width
            && top as usize + block_h + 7 <= true_height
        {
            let base = top as usize * stride + x0 as usize;
            if base + (block_h + 6) * stride + block_w <= reference.len() {
                let mut v16 = [0i32; 8];
                for (o, &t) in v16.iter_mut().zip(v_filter.iter()) {
                    *o = t * 16;
                }
                // SAFETY: samples are below `2^15` at every supported bit
                // depth, so the same bytes read as `i16` are the same values;
                // the length is unchanged.
                #[allow(unsafe_code)]
                let src: &[i16] =
                    unsafe { std::slice::from_raw_parts(reference.as_ptr().cast::<i16>(), reference.len()) };
                vpass_row_u16(&src[base..], block_w, stride, block_h, &v16, max, dst);
                #[cfg(test)]
                crate::encode::stage_add(1, stage_t.elapsed());
                return;
            }
        }
    }
    with_scratch(rows, block_w, |intermediate, _acc| {
        horizontal_pass_unscaled(
            reference, stride, true_width, true_height, x0,
            if yfrac == 0 { y0 + 3 } else { y0 }, h_filter, block_w, rows,
            intermediate,
        );
        if yfrac == 0 {
            for (d, &a) in dst.iter_mut().zip(intermediate.iter()) {
                *d = round2(i32::from(a), INTER_ROUND_1 - 7).clamp(0, max) as u16;
            }
            return;
        }
        vpass_row_u16(intermediate, block_w, block_w, block_h, v_filter, max, dst);
    });

    #[cfg(test)]
    crate::encode::stage_add(1, stage_t.elapsed());
}

/// `REF_SCALE_SHIFT`'s "no scaling" value (spec 7.11.3.3, libaom `scale.h`
/// `REF_NO_SCALE`): `x_scale_fp == REF_NO_SCALE` means the stored reference's
/// luma width equals the current frame's, so [`predict_scaled`] reduces
/// exactly to [`predict_with_filters`] (verified in
/// `predict_scaled_at_no_scale_matches_predict_with_filters` below).
pub const REF_NO_SCALE: i64 = 1 << 14;

/// The horizontal scale ratio spec 7.11.3.3 derives from luma widths only
/// (libaom `av1_setup_scale_factors_for_frame`, luma widths, `REF_SCALE_SHIFT
/// == 14`): `other_width` is the stored reference picture's own luma width,
/// `this_width` the current frame's coded luma width. AV1 superres never
/// scales height (r8's derivation), so there is no vertical counterpart.
pub fn scale_factor(other_width: usize, this_width: usize) -> i64 {
    (((other_width as i64) << 14) + this_width as i64 / 2) / this_width as i64
}

thread_local! {
    /// lane-superres r10: firing count for the bypass gate (class
    /// `gate-blind-to-feature`) -- how many times [`predict_scaled`] itself
    /// ran, so the gate can hard-assert the scaled MC path actually fired
    /// rather than trust a pixel match alone (an unscaled reference would
    /// pass the pixels and prove nothing).
    static PREDICT_SCALED_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`PREDICT_SCALED_HITS`].
pub fn predict_scaled_hits() -> usize {
    PREDICT_SCALED_HITS.with(|c| c.get())
}

thread_local! {
    /// lane-hbdinter: how many inter predictions this thread has produced
    /// (single-reference, scaled, and the compound intermediate entry). The
    /// 10-bit inter gate hard-asserts this moved, so a stream that coded
    /// only intra blocks cannot pass it by construction (class
    /// `gate-blind-to-feature`).
    static INTER_PRED_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };

    /// lane-rectchroma2 r1: how often the 4-tap decision came out DIFFERENT
    /// from what the destination buffer's own dimensions would have given --
    /// i.e. how often a rectangular block predicted over its enclosing square
    /// buffer asked for libaom's narrow kernel on an axis. Zero means a gate
    /// never exercised the rect-chroma shape at all.
    static RECT_NARROW_KERN_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`RECT_NARROW_KERN_HITS`].
pub fn rect_narrow_kern_hits() -> usize {
    RECT_NARROW_KERN_HITS.with(|c| c.get())
}

/// Resets [`RECT_NARROW_KERN_HITS`] (per-attempt counting, class
/// `counter-from-refused-stream`).
pub fn reset_rect_narrow_kern_hits() {
    RECT_NARROW_KERN_HITS.with(|c| c.set(0));
}

/// Bumps [`RECT_NARROW_KERN_HITS`] when the block's true dims pick a kernel
/// the destination buffer's dims would not have.
fn note_narrow_kern(block_w: usize, block_h: usize, kern_w: usize, kern_h: usize) {
    if (kern_w <= 4) != (block_w <= 4) || (kern_h <= 4) != (block_h <= 4) {
        RECT_NARROW_KERN_HITS.with(|c| c.set(c.get() + 1));
    }
}

/// Current value of [`INTER_PRED_HITS`].
pub fn inter_pred_hits() -> usize {
    INTER_PRED_HITS.with(|c| c.get())
}

fn round_pow2_64(value: i64, shift: u32) -> i64 {
    if shift == 0 {
        value
    } else {
        (value + (1i64 << (shift - 1))) >> shift
    }
}

/// `ROUND_POWER_OF_TWO_SIGNED` (libaom `common/common.h`): halves round away
/// from zero's *magnitude* -- negative inputs are rounded like their
/// negation, not truncated toward zero the way a plain arithmetic shift
/// would.
fn round_pow2_signed_64(value: i64, shift: u32) -> i64 {
    if value < 0 {
        -round_pow2_64(-value, shift)
    } else {
        round_pow2_64(value, shift)
    }
}

/// Spec 7.11.3.3's horizontal pass on a (possibly) scaled reference, shared
/// by [`predict_scaled`] and [`predict_compound_intermediate`] -- the two
/// differ only in their VERTICAL rounding ([`INTER_ROUND_1`] vs
/// [`INTER_ROUND_1_COMPOUND`]), never here. Each output column walks the
/// reference by `x_step_qn` (the Q14 scale rounded down to the Q10
/// `SCALE_SUBPEL_BITS` grid) and picks its own filter phase from the low
/// bits, so `x_scale_fp == REF_NO_SCALE` reduces to the ordinary stride-1
/// whole-pel walk with one fixed phase (pinned by
/// `predict_scaled_at_no_scale_matches_predict_with_filters`).
#[allow(clippy::too_many_arguments)]
fn horizontal_scaled_pass(
    reference: &[u16],
    stride: usize,
    true_width: usize,
    true_height: usize,
    x_q4: i32,
    y0: i32,
    x_scale_fp: i64,
    block_w: usize,
    kern_w: usize,
    rows: usize,
    h_kind: InterpFilterKind,
    intermediate: &mut [i16],
) {
    let x_step_qn = round_pow2_64(x_scale_fp, 4);
    let off = (x_scale_fp - REF_NO_SCALE) * 8;
    let pos_x_q10 = round_pow2_signed_64(x_q4 as i64 * x_scale_fp + off, 8) + 32;
    let (h_wide, h_narrow) = h_kind.tables();
    for c in 0..block_w {
        let x_qn = pos_x_q10 + c as i64 * x_step_qn;
        let int_pel = (x_qn >> 10) as i32;
        let filter_idx = ((x_qn & 1023) >> 6) as usize;
        let h_filter = if kern_w <= 4 {
            &h_narrow[filter_idx]
        } else {
            &h_wide[filter_idx]
        };
        for r in 0..rows {
            let y = y0 - 3 + r as i32;
            let mut sum = 0;
            for (t, &tap) in h_filter.iter().enumerate() {
                let x = int_pel + t as i32 - 3;
                sum += tap * sample(reference, stride, true_width, true_height, x, y);
            }
            intermediate[r * block_w + c] = round2(sum, INTER_ROUND_0) as i16;
        }
    }
}

/// lane-perf2: the horizontal pass every two-pass entry point takes.
/// `horizontal_scaled_pass` was carrying the UNSCALED work too (every
/// compound prediction goes through it whatever the reference's scale), and
/// its per-column walk re-derives an integer position and a filter phase
/// that are constant at `REF_NO_SCALE`, reads the reference column-major,
/// and clamps per tap -- 14% of decode self time. At `REF_NO_SCALE` the
/// scaled walk reduces algebraically to `x0 = x_q4 / 16`, phase `x_q4 % 16`
/// (`pos_x_q10 == x_q4 * 64 + 32`, so `int_pel == x0` and
/// `(x_qn & 1023) >> 6 == xfrac` for every column), which is exactly
/// [`horizontal_pass_unscaled`] -- and
/// `predict_scaled_at_no_scale_matches_predict_with_filters` pins that
/// equality end to end. A genuinely scaled reference (superres) still takes
/// the general path.
#[allow(clippy::too_many_arguments)]
fn horizontal_pass(
    reference: &[u16],
    stride: usize,
    true_width: usize,
    true_height: usize,
    x_q4: i32,
    y0: i32,
    x_scale_fp: i64,
    block_w: usize,
    kern_w: usize,
    rows: usize,
    h_kind: InterpFilterKind,
    intermediate: &mut [i16],
) {
    if x_scale_fp == REF_NO_SCALE {
        let (h_wide, h_narrow) = h_kind.tables();
        let xfrac = x_q4.rem_euclid(16) as usize;
        let taps = if kern_w <= 4 { &h_narrow[xfrac] } else { &h_wide[xfrac] };
        horizontal_pass_unscaled(
            reference,
            stride,
            true_width,
            true_height,
            x_q4.div_euclid(16),
            y0,
            taps,
            block_w,
            rows,
            intermediate,
        );
        return;
    }
    horizontal_scaled_pass(
        reference, stride, true_width, true_height, x_q4, y0, x_scale_fp, block_w, kern_w,
        rows, h_kind, intermediate,
    );
}

/// [`predict_with_filters`]'s scaled-reference counterpart (spec 7.11.3.3):
/// used only when the stored reference's luma width differs from the current
/// frame's (`use_superres` on a non-key frame). `x_scale_fp` is
/// [`scale_factor`]'s ratio ([`REF_NO_SCALE`] reproduces
/// `predict_with_filters` bit-exact, pinned by
/// `predict_scaled_at_no_scale_matches_predict_with_filters`). Per r8's
/// derivation AV1 superres never scales height, so `y_q4`'s whole-sample /
/// fraction split and the vertical pass are untouched -- only the horizontal
/// pass's per-column integer position and filter phase come from a scaled
/// walk (`x_qn`) instead of a fixed stride-1 one.
///
/// # Panics
/// Panics when `dst` is not `block_w * block_h` long, or the reference is
/// empty.
#[allow(clippy::too_many_arguments)]
pub(crate) fn predict_scaled(
    reference: &[u16],
    stride: usize,
    true_width: usize,
    true_height: usize,
    x_q4: i32,
    y_q4: i32,
    x_scale_fp: i64,
    block_w: usize,
    block_h: usize,
    h_kind: InterpFilterKind,
    v_kind: InterpFilterKind,
    dst: &mut [u16], fctx: &crate::decode::FrameCtx,
) {
    predict_scaled_kern(
        reference, stride, true_width, true_height, x_q4, y_q4, x_scale_fp,
        block_w, block_h, block_w, block_h, h_kind, v_kind, dst, fctx,
    );
}

/// [`predict_scaled`] with the 4-tap decision from the block's true dims
/// (see [`predict_with_filters_kern`]).
#[allow(clippy::too_many_arguments)]
pub(crate) fn predict_scaled_kern(
    reference: &[u16],
    stride: usize,
    true_width: usize,
    true_height: usize,
    x_q4: i32,
    y_q4: i32,
    x_scale_fp: i64,
    block_w: usize,
    block_h: usize,
    kern_w: usize,
    kern_h: usize,
    h_kind: InterpFilterKind,
    v_kind: InterpFilterKind,
    dst: &mut [u16], fctx: &crate::decode::FrameCtx,
) {
    assert_eq!(dst.len(), block_w * block_h, "the destination is the block");
    assert!(!reference.is_empty(), "a reference plane has samples");
    INTER_PRED_HITS.with(|c| c.set(c.get() + 1));
    PREDICT_SCALED_HITS.with(|c| c.set(c.get() + 1));

    let y0 = y_q4.div_euclid(16);
    let yfrac = y_q4.rem_euclid(16) as usize;

    // SCALE_SUBPEL_BITS == 10: x_step_qn is x_scale_fp (Q14) rounded down to
    // Q10; off/pos_x_q10 fold the block's own x_q4 (Q4) into that same Q10
    // grid (spec 7.11.3.3's `dec_calc_subpel_params`).
    let (v_wide, v_narrow) = v_kind.tables();
    note_narrow_kern(block_w, block_h, kern_w, kern_h);
    let v_filter = if kern_h <= 4 {
        &v_narrow[yfrac]
    } else {
        &v_wide[yfrac]
    };

    let rows = if yfrac == 0 { block_h } else { block_h + 7 };
    let max = crate::decode::sample_max(fctx);
    with_scratch(rows, block_w, |intermediate, _acc| {
        horizontal_pass(
            reference, stride, true_width, true_height, x_q4,
            if yfrac == 0 { y0 + 3 } else { y0 }, x_scale_fp, block_w,
            kern_w, rows, h_kind, intermediate,
        );
        if yfrac == 0 {
            for (d, &a) in dst.iter_mut().zip(intermediate.iter()) {
                *d = round2(i32::from(a), INTER_ROUND_1 - 7).clamp(0, max) as u16;
            }
            return;
        }
        vpass_row_u16(intermediate, block_w, block_w, block_h, v_filter, max, dst);
    });
}

/// [`predict_with_filters`] or [`predict_scaled`], on this reference's own
/// `x_scale_fp` -- the dispatch every inter path needs once a reference can be
/// scaled (superres). `REF_NO_SCALE` takes the ordinary stride-1 path, which
/// keeps [`predict_scaled_hits`] a true count of scaled predictions.
#[allow(clippy::too_many_arguments)]
pub(crate) fn predict_maybe_scaled(
    reference: &[u16],
    stride: usize,
    true_width: usize,
    true_height: usize,
    x_q4: i32,
    y_q4: i32,
    x_scale_fp: i64,
    block_w: usize,
    block_h: usize,
    h_kind: InterpFilterKind,
    v_kind: InterpFilterKind,
    dst: &mut [u16], fctx: &crate::decode::FrameCtx,
) {
    if x_scale_fp == REF_NO_SCALE {
        predict_with_filters(
            reference, stride, true_width, true_height, x_q4, y_q4, block_w, block_h, h_kind,
            v_kind, dst, fctx,
        );
    } else {
        predict_scaled(
            reference, stride, true_width, true_height, x_q4, y_q4, x_scale_fp, block_w,
            block_h, h_kind, v_kind, dst, fctx,
        );
    }
}

/// `InterRound1` for a compound ref (spec 7.11.3.2's `isCompound` branch,
/// `COMPOUND_ROUND1_BITS` in libaom's `convolve.h`): 4 bits shallower than
/// [`INTER_ROUND_1`] so the vertical pass lands in the `CONV_BUF` domain
/// (still above 8-bit pixel range) instead of a finished sample --
/// `predict_compound_intermediate` below stops there and leaves the final
/// clip to whichever combine step (simple/distance-weighted average, or a
/// future masked blend) runs on the pair of intermediates.
const INTER_ROUND_1_COMPOUND: u32 = 7;

/// `InterPostRound` (spec 7.11.3.2): `2 * FILTER_BITS - (InterRound0 +
/// InterRound1)` with the *compound* `InterRound1` above -- `2*7 - (3+7) ==
/// 4`. [`combine_compound`] folds this together with `DIST_PRECISION_BITS`
/// (below) into one final shift.
const INTER_POST_ROUND: u32 = 4;

/// `DIST_PRECISION_BITS` (spec 7.11.3.15 / libaom `enums.h`): the two
/// weights [`combine_compound`] takes always sum to `1 << 4 == 16`, whether
/// they are the simple-average split (8/8) or a distance-weighted split
/// from [`crate::compound::dist_wtd_comp_weight_assign`].
const DIST_PRECISION_BITS: u32 = 4;

/// [`predict_with_filters`]'s compound counterpart: same two-pass separable
/// filter, but the vertical pass rounds by [`INTER_ROUND_1_COMPOUND`]
/// instead of [`INTER_ROUND_1`] and is never clipped to a pixel -- the
/// `CONV_BUF` intermediate domain spec 7.11.3.2 keeps a compound block's two
/// per-reference predictions in until [`combine_compound`] blends them.
/// `dst` receives one `i32` per sample, row-major, same shape as
/// [`predict_with_filters`]'s `u8` `dst`.
///
/// # Panics
/// Panics when `dst` is not `block_w * block_h` long, or the reference is
/// empty.
#[allow(clippy::too_many_arguments)]
pub fn predict_compound_intermediate(
    reference: &[u16],
    stride: usize,
    true_width: usize,
    true_height: usize,
    x_q4: i32,
    y_q4: i32,
    x_scale_fp: i64,
    block_w: usize,
    block_h: usize,
    h_kind: InterpFilterKind,
    v_kind: InterpFilterKind,
    dst: &mut [i32],
) {
    predict_compound_intermediate_kern(
        reference, stride, true_width, true_height, x_q4, y_q4, x_scale_fp,
        block_w, block_h, block_w, block_h, h_kind, v_kind, dst,
    );
}

/// [`predict_compound_intermediate`] with the 4-tap decision from the block's
/// true dims (see [`predict_with_filters_kern`]).
#[allow(clippy::too_many_arguments)]
pub fn predict_compound_intermediate_kern(
    reference: &[u16],
    stride: usize,
    true_width: usize,
    true_height: usize,
    x_q4: i32,
    y_q4: i32,
    x_scale_fp: i64,
    block_w: usize,
    block_h: usize,
    kern_w: usize,
    kern_h: usize,
    h_kind: InterpFilterKind,
    v_kind: InterpFilterKind,
    dst: &mut [i32],
) {
    assert_eq!(dst.len(), block_w * block_h, "the destination is the block");
    assert!(!reference.is_empty(), "a reference plane has samples");
    INTER_PRED_HITS.with(|c| c.set(c.get() + 1));

    if x_scale_fp != REF_NO_SCALE {
        PREDICT_SCALED_HITS.with(|c| c.set(c.get() + 1));
    }

    let y0 = y_q4.div_euclid(16);
    let yfrac = y_q4.rem_euclid(16) as usize;
    if crate::envflags::env_flag!("EC_MC_TRACE") {
        eprintln!("EC_MC_COMP x_q4={x_q4} y_q4={y_q4} w={block_w} h={block_h} hk={h_kind:?} vk={v_kind:?}");
    }

    let (v_wide, v_narrow) = v_kind.tables();
    note_narrow_kern(block_w, block_h, kern_w, kern_h);
    let v_filter = if kern_h <= 4 {
        &v_narrow[yfrac]
    } else {
        &v_wide[yfrac]
    };

    let rows = if yfrac == 0 { block_h } else { block_h + 7 };
    // lane-mc2: the unscaled whole-pel horizontal position is the identity
    // tap, whose intermediate is `s << 4`, so the vertical pass is the same
    // 8-tap sum taken straight off the reference rows with the taps scaled by
    // 16 (see `predict_with_filters_kern`). Interior blocks only.
    let x0 = x_q4.div_euclid(16);
    if x_scale_fp == REF_NO_SCALE && x_q4.rem_euclid(16) == 0 && yfrac != 0 && block_w <= 128 {
        let top = y0 - 3;
        if top >= 0
            && x0 >= 0
            && x0 as usize + block_w <= true_width
            && top as usize + block_h + 7 <= true_height
        {
            let base = top as usize * stride + x0 as usize;
            if base + (block_h + 6) * stride + block_w <= reference.len() {
                let mut v16 = [0i32; 8];
                for (o, &t) in v16.iter_mut().zip(v_filter.iter()) {
                    *o = t * 16;
                }
                // SAFETY: samples are below `2^15` at every supported bit
                // depth, so the same bytes read as `i16` are the same values.
                #[allow(unsafe_code)]
                let src: &[i16] =
                    unsafe { std::slice::from_raw_parts(reference.as_ptr().cast::<i16>(), reference.len()) };
                let mut accbuf = [0i32; 128];
                let acc = &mut accbuf[..block_w];
                for row in 0..block_h {
                    vpass_row(&src[base..], block_w, stride, row, &v16, acc);
                    for (d, &a) in dst[row * block_w..(row + 1) * block_w].iter_mut().zip(acc.iter()) {
                        *d = round2(a, INTER_ROUND_1_COMPOUND);
                    }
                }
                return;
            }
        }
    }
    with_scratch(rows, block_w, |intermediate, acc| {
        horizontal_pass(
            reference, stride, true_width, true_height, x_q4,
            if yfrac == 0 { y0 + 3 } else { y0 }, x_scale_fp, block_w,
            kern_w, rows, h_kind, intermediate,
        );
        if yfrac == 0 {
            // `Round2(128 * a, INTER_ROUND_1_COMPOUND) == a`.
            for (d, &a) in dst.iter_mut().zip(intermediate.iter()) {
                *d = i32::from(a);
            }
            return;
        }
        for row in 0..block_h {
            vpass_row(intermediate, block_w, block_w, row, v_filter, acc);
            for (d, &a) in dst[row * block_w..(row + 1) * block_w].iter_mut().zip(acc.iter()) {
                *d = round2(a, INTER_ROUND_1_COMPOUND);
            }
        }
    });
}

/// Blends two [`predict_compound_intermediate`] outputs into a finished
/// 8-bit block (spec 7.11.3.15's weighted-average combine, the
/// `comp_group_idx == 0` path -- `fwd_weight`/`bck_weight` are either the
/// simple-average split `(8, 8)` or [`crate::compound::dist_wtd_comp_weight_assign`]'s
/// output; both always sum to `1 << DIST_PRECISION_BITS`). Masked compound
/// (`comp_group_idx == 1`, wedge/diffwtd) is a different combine this
/// function does not cover -- decode.rs still refuses those by name.
pub(crate) fn combine_compound(
    pred0: &[i32],
    pred1: &[i32],
    fwd_weight: i32,
    bck_weight: i32,
    dst: &mut [u16], fctx: &crate::decode::FrameCtx,
) {
    assert_eq!(pred0.len(), pred1.len(), "both refs predict the same block");
    assert_eq!(dst.len(), pred0.len(), "the destination is the block");
    for i in 0..dst.len() {
        // libaom (`av1_highbd_dist_wtd_convolve_2d_c`, and its lowbd twin)
        // applies the two shifts SEPARATELY: a truncating `>>
        // DIST_PRECISION_BITS` on the weighted sum, then `ROUND_POWER_OF_TWO`
        // by `round_bits == INTER_POST_ROUND`. Folding them into one
        // `Round2(sum, 8)` is NOT the same function -- it rounds up one LSB
        // whenever `(sum >> 4) + 8` is one below a multiple of 16 and the
        // dropped low 4 bits are non-zero.
        let sum = pred0[i] * fwd_weight + pred1[i] * bck_weight;
        dst[i] = round2(sum >> DIST_PRECISION_BITS, INTER_POST_ROUND)
            .clamp(0, crate::decode::sample_max(fctx)) as u16;
    }
}

/// lane-maskcomp r2: `build_compound_diffwtd_mask` (reconinter.c) --
/// `DIFFWTD_38`/`DIFFWTD_38_INV` (`mask_type` bit: 0/1). Operates directly on
/// the two [`predict_compound_intermediate`] outputs: libaom's own
/// `av1_build_compound_diffwtd_mask_d16_c` runs on its CONV_BUF domain (which
/// carries a constant `round_offset` bias baked into every sample by its
/// convolve stage), but `abs(src0 - src1)` cancels an equal bias on both
/// sides exactly, so the unbiased `i32` intermediates this crate already
/// produces give the identical `diff` libaom computes. `round` is
/// `2*FILTER_BITS - round_0 - round_1 + (bd-8)` == [`INTER_POST_ROUND`] for
/// 8-bit content (`round_0=3`, compound `round_1=7`, `bd=8`).
pub(crate) fn diffwtd_mask(pred0: &[i32], pred1: &[i32], inv: bool, mask: &mut [u8], fctx: &crate::decode::FrameCtx) {
    assert_eq!(pred0.len(), pred1.len(), "both refs predict the same block");
    assert_eq!(mask.len(), pred0.len(), "one mask byte per pixel");
    // libaom `diffwtd_mask_d16` (`reconinter.c:307`): `round = 2*FILTER_BITS
    // - round_0 - round_1 + (bd - 8)`, i.e. INTER_POST_ROUND plus the
    // bit-depth headroom the CONV_BUF domain carries at 10/12-bit. `round_0`
    // and `round_1` themselves only move at 12-bit (`convolve.h:83`'s
    // `intbufrange > 16`), so the whole bit-depth dependence here is `bd - 8`.
    let round = INTER_POST_ROUND + u32::from(crate::decode::bit_depth(fctx)).saturating_sub(8);
    for i in 0..mask.len() {
        let diff = round2((pred0[i] - pred1[i]).abs(), round);
        let m = (38 + diff / 16).clamp(0, 64);
        mask[i] = if inv { (64 - m) as u8 } else { m as u8 };
    }
}

/// lane-maskcomp r2: `aom_lowbd_blend_a64_d16_mask_c` (blend_a64_mask.c),
/// algebraically simplified to this crate's unbiased `i32` intermediate
/// domain the same way [`diffwtd_mask`]'s doc comment derives -- libaom's
/// `res -= round_offset` step cancels exactly against the bias `m + (64-m)
/// == 64` contributes equally from both inputs, leaving `(m*pred0 +
/// (64-m)*pred1) >> 6` (a plain, unrounded shift, matching the C `>>` on
/// `int32_t`) then one final [`round2`] by [`INTER_POST_ROUND`]. `mask` is
/// always the LUMA-resolution mask (`mask_stride` == luma block width);
/// `subsampled` selects the 2x2-average chroma read (spec 7.11.3.14 /
/// libaom's `subw == 1 && subh == 1` branch) vs. the direct luma read.
#[allow(clippy::too_many_arguments)]
pub(crate) fn blend_masked_compound(
    pred0: &[i32],
    pred1: &[i32],
    mask: &[u8],
    mask_stride: usize,
    w: usize,
    h: usize,
    subsampled: bool,
    dst: &mut [u16], fctx: &crate::decode::FrameCtx,
) {
    assert_eq!(pred0.len(), w * h, "pred0 is the destination-sized block");
    assert_eq!(pred1.len(), w * h, "pred1 is the destination-sized block");
    assert_eq!(dst.len(), w * h, "the destination is the block");
    for i in 0..h {
        for j in 0..w {
            let m = if subsampled {
                let idx = (2 * i) * mask_stride + 2 * j;
                round2(
                    i32::from(mask[idx])
                        + i32::from(mask[idx + 1])
                        + i32::from(mask[idx + mask_stride])
                        + i32::from(mask[idx + mask_stride + 1]),
                    2,
                )
            } else {
                i32::from(mask[i * mask_stride + j])
            };
            let res = (m * pred0[i * w + j] + (64 - m) * pred1[i * w + j]) >> 6;
            dst[i * w + j] = round2(res, INTER_POST_ROUND).clamp(0, crate::decode::sample_max(fctx)) as u16;
        }
    }
}

/// Whole-pel identity case: [`predict_compound_intermediate`]'s two-pass
/// filter reduces to `16 * source_sample` exactly (the fraction-0 row is a
/// single `128` tap both passes, and `128*128 == 1 << 14`, `14 - 7 == 7 ==
/// INTER_ROUND_1_COMPOUND`, so the extra `2^4` of gain the compound path
/// keeps over [`predict`]'s 11-bit round survives untouched) --
/// [`combine_compound`] then divides that back out exactly with an 8/8
/// simple-average weight split, so a compound block whose two references
/// (and MVs) happen to coincide reproduces the plain source sample, not an
/// off-by-one from double rounding.
#[test]
fn compound_intermediate_whole_pel_identity_round_trips_through_combine() {
    let fctx = &crate::decode::FrameCtx::new();
    let reference = vec![100u16; 16 * 16];
    let mut pred0 = vec![0i32; 16];
    predict_compound_intermediate(
        &reference,
        16,
        16,
        16,
        0,
        0,
        REF_NO_SCALE,
        4,
        4,
        InterpFilterKind::Regular,
        InterpFilterKind::Regular,
        &mut pred0,
    );
    assert!(pred0.iter().all(|&v| v == 1600), "{pred0:?}");

    let pred1 = pred0.clone();
    let mut dst = vec![0u16; 16];
    combine_compound(&pred0, &pred1, 8, 8, &mut dst, fctx);
    assert!(dst.iter().all(|&v| v == 100), "{dst:?}");
}

#[cfg(test)]
mod tests {
    use super::{
        InterpFilterKind, REF_NO_SCALE, predict, predict_scaled, predict_with_filter,
        predict_with_filters,
    };

    /// Regression pin for the lane-av1flake ±1 defect: a real aomenc chroma
    /// inter block at the frame's top-left corner (mv q4 = (row 7, col 15),
    /// bw=bh=16) predicted the wrong value because this decoder always used
    /// the REGULAR filter table -- aomdec's own `av1_convolve_2d_sr_c` trace
    /// showed this block actually used `EIGHTTAP_SMOOTH`
    /// (`0 0 2 34 62 28 2 0` at xfrac=15, not REGULAR's `0 0 -2 8 126 -6 2
    /// 0`). Window and expected output dumped straight from an
    /// instrumented aomdec's `av1_convolve_2d_sr_c` on the pinned stream.
    #[test]
    fn smooth_filter_matches_aomdec_chroma_inter_block() {
    let fctx = &crate::decode::FrameCtx::new();
        #[rustfmt::skip]
        let window: [[u8; 24]; 24] = [
            [114,114,114,114,114,114,114,114,115,115,115,115,116,116,116,116,117,117,117,116,115,115,114,114],
            [114,114,114,114,114,114,114,114,115,115,115,115,116,116,116,116,117,117,117,116,115,115,114,114],
            [114,114,114,114,114,114,114,114,115,115,115,115,116,116,116,116,117,117,117,116,115,115,114,114],
            [114,114,114,114,114,114,114,114,115,115,115,115,116,116,116,116,117,117,117,116,115,115,114,114],
            [114,114,114,114,114,114,114,114,115,115,115,115,116,116,116,116,117,117,117,116,115,115,114,114],
            [114,114,114,114,114,114,114,114,114,115,115,115,116,116,116,116,116,117,117,116,116,115,115,115],
            [114,114,114,114,114,114,114,114,114,114,115,115,115,115,116,116,116,116,116,116,115,115,115,115],
            [113,113,113,113,113,113,113,113,114,114,114,114,115,115,115,115,116,116,116,116,115,115,115,115],
            [113,113,113,113,113,113,113,113,113,113,114,114,114,114,115,115,115,115,115,115,115,115,115,115],
            [112,112,112,112,112,112,112,112,112,113,113,113,113,114,114,114,114,114,115,115,115,115,115,115],
            [111,111,111,111,111,111,111,111,112,112,112,112,113,113,113,113,114,114,114,114,115,115,115,115],
            [110,110,110,110,110,110,110,110,111,111,111,111,112,112,112,113,113,113,113,113,114,114,114,115],
            [109,109,109,109,109,109,109,110,110,110,110,111,111,111,111,112,112,112,112,113,113,114,114,114],
            [108,108,108,108,108,109,109,109,109,109,110,110,110,110,111,111,111,111,111,112,112,113,113,113],
            [108,108,108,108,108,108,108,108,108,108,109,109,109,110,110,110,110,110,111,111,112,112,112,112],
            [107,107,107,107,107,107,107,107,107,108,108,108,109,109,109,109,109,110,110,110,110,111,111,111],
            [106,106,106,106,106,106,106,107,107,107,107,108,108,108,108,109,109,109,109,109,109,110,110,110],
            [106,106,106,106,106,106,106,106,106,107,107,107,107,108,108,108,108,109,109,109,109,109,109,109],
            [105,105,105,105,105,105,105,105,106,106,106,107,107,107,107,108,108,108,108,108,108,108,108,109],
            [104,104,104,104,104,104,104,104,104,105,105,106,106,106,107,107,107,107,107,107,107,108,108,108],
            [102,102,102,102,102,103,103,103,103,104,104,105,105,105,106,106,107,107,107,107,107,107,107,107],
            [101,101,101,101,101,101,101,102,102,103,103,104,104,104,105,105,106,106,106,106,106,107,107,107],
            [100,100,100,100,100,100,100,101,101,102,102,103,103,104,104,105,105,105,106,106,106,106,106,106],
            [99,99,99,99,99,99,100,100,100,101,101,102,103,103,104,104,105,105,105,105,105,105,105,106],
        ];
        // The dumped window spans real cols/rows -4..19; the block itself
        // starts at real (0, 0), so pad the reference plane so col/row 4 of
        // this buffer is real col/row 0, and hand `predict_with_filter` the
        // real (0, 0) coordinates -- clamping then reproduces the identical
        // extended border the dump already captured.
        let stride = 24;
        let mut reference = vec![0u16; stride * 24];
        for (r, row) in window.iter().enumerate() {
            for (c, &v) in row.iter().enumerate() {
                reference[r * stride + c] = u16::from(v);
            }
        }
        // predict()'s own edge clamp starts at true (0,0); shift so the
        // dumped window's row/col 4 lands there by biasing x_q4/y_q4's
        // whole-pel part by -4 and reading from a plane whose true origin
        // is the window's row/col 4 -- simplest is to pass true_width /
        // true_height covering the whole dumped window and offset the
        // whole-pel part of x_q4/y_q4 by +4 to land on real (0,0).
        let x_q4 = (4 + 0) * 16 + 15; // real block x0=0, xfrac=15
        let y_q4 = (4 + 0) * 16 + 7; // real block y0=0, yfrac=7
        let mut dst = vec![0u16; 16 * 16];
        predict_with_filter(
            &reference,
            stride,
            24,
            24,
            x_q4,
            y_q4,
            16,
            16,
            InterpFilterKind::Smooth,
            &mut dst, fctx,
        );
        #[rustfmt::skip]
        let expected_row0: [u16; 16] =
            [114,114,114,114,115,115,115,116,116,116,116,116,117,117,116,116];
        assert_eq!(
            &dst[0..16],
            &expected_row0,
            "row0 vs aomdec's real av1_convolve_2d_sr_c output"
        );
        #[rustfmt::skip]
        let expected_row15: [u16; 16] =
            [103,103,104,104,104,105,105,106,106,106,107,107,107,107,107,107];
        assert_eq!(
            &dst[15 * 16..15 * 16 + 16],
            &expected_row15,
            "row15 vs aomdec"
        );
    }

    /// lane-gmaffine r5 root cause: a block 4 or fewer samples wide reads
    /// `Subpel_Filters[4]` -- the REGULAR narrow kernel -- for BOTH
    /// `EIGHTTAP` and `EIGHTTAP_SHARP` (spec 7.11.3.4's `filterIdx`, libaom
    /// `av1_interp_4tap[MULTITAP_SHARP] == av1_sub_pel_filters_4`,
    /// `filter.h:243`). We used the 8-tap sharp kernel there, which is
    /// invisible above 4 samples and so only ever bit the 4x4 chroma of an
    /// 8x8 (or smaller) luma leaf.
    #[test]
    fn a_narrow_block_reads_the_regular_four_tap_kernel_under_every_sharpness() {
        use crate::mc::{SUBPEL_FILTERS_4, SUBPEL_FILTERS_BILINEAR, SUBPEL_FILTERS_SMOOTH_4};
        for kind in [
            InterpFilterKind::Regular,
            InterpFilterKind::Smooth,
            InterpFilterKind::Sharp,
            InterpFilterKind::Bilinear,
        ] {
            let (wide, narrow) = kind.tables();
            let want: &[[i32; 8]; 16] = match kind {
                InterpFilterKind::Regular | InterpFilterKind::Sharp => &SUBPEL_FILTERS_4,
                InterpFilterKind::Smooth => &SUBPEL_FILTERS_SMOOTH_4,
                InterpFilterKind::Bilinear => &SUBPEL_FILTERS_BILINEAR,
            };
            assert_eq!(narrow, want, "{kind:?}: wrong narrow-block kernel");
            if kind == InterpFilterKind::Sharp {
                assert_ne!(wide, narrow, "sharp's wide kernel is not its narrow one");
            }
        }
    }

    #[test]
    fn integer_mv_is_identity() {
    let fctx = &crate::decode::FrameCtx::new();
        let width = 12;
        let height = 12;
        let reference: Vec<u16> = (0..width * height).map(|i| (i * 7 % 251) as u16).collect();
        let mut dst = vec![0u16; 6 * 6];
        predict(
            &reference,
            width,
            width,
            height,
            3 * 16,
            4 * 16,
            6,
            6,
            &mut dst, fctx,
        );
        for row in 0..6 {
            for col in 0..6 {
                assert_eq!(
                    dst[row * 6 + col],
                    reference[(4 + row) * width + 3 + col],
                    "integer MV must reproduce the reference block byte-for-byte"
                );
            }
        }
    }

    #[test]
    fn half_pel_reproduces_the_ramp_midpoint() {
    let fctx = &crate::decode::FrameCtx::new();
        // Step 2 so every half-pel position lands on an exact integer.
        let width = 16;
        let height = 16;
        let reference: Vec<u16> = (0..width * height)
            .map(|i| (2 * (i % width)) as u16)
            .collect();
        let mut dst = vec![0u16; 4 * 4];
        // Half-pel in x only: fraction 8/16, integer part at column 5.
        predict(
            &reference,
            width,
            width,
            height,
            5 * 16 + 8,
            5 * 16,
            4,
            4,
            &mut dst, fctx,
        );
        for row in 0..4 {
            for col in 0..4 {
                let midpoint = 2 * (5 + col) + 1; // halfway between col and col+1
                assert_eq!(
                    dst[row * 4 + col] as i32,
                    midpoint as i32,
                    "half-pel MV over a linear ramp must land exactly on the midpoint"
                );
            }
        }
    }

    #[test]
    fn constant_plane_has_unit_dc_gain_at_every_subpel_position() {
    let fctx = &crate::decode::FrameCtx::new();
        let width = 20;
        let height = 20;
        let reference = vec![142u16; width * height];
        for yfrac in 0..16 {
            for xfrac in 0..16 {
                let mut dst = vec![0u16; 5 * 5];
                predict(
                    &reference,
                    width,
                    width,
                    height,
                    5 * 16 + xfrac,
                    5 * 16 + yfrac,
                    5,
                    5,
                    &mut dst, fctx,
                );
                assert!(
                    dst.iter().all(|&v| v == 142),
                    "DC gain must be exactly 1 at fraction ({xfrac}, {yfrac}), got {dst:?}"
                );
            }
        }
    }

    #[test]
    fn horizontal_and_vertical_only_filtering_differ() {
    let fctx = &crate::decode::FrameCtx::new();
        // A plane linear in both x and y with different, even coefficients so
        // each axis's half-pel interpolation lands on an exact integer: this
        // pins the actual value each pass produces, not just that the two
        // differ, so swapping which pass reads which axis is caught even if
        // it happened to produce two merely-different numbers.
        let width = 10;
        let height = 10;
        let reference: Vec<u16> = (0..height)
            .flat_map(|y| (0..width).map(move |x| (20 * x + 4 * y) as u16))
            .collect();

        let mut horiz = vec![0u16; 4 * 4];
        predict(
            &reference,
            width,
            width,
            height,
            3 * 16 + 8,
            3 * 16,
            4,
            4,
            &mut horiz, fctx,
        );
        let mut vert = vec![0u16; 4 * 4];
        predict(
            &reference,
            width,
            width,
            height,
            3 * 16,
            3 * 16 + 8,
            4,
            4,
            &mut vert, fctx,
        );

        for row in 0..4 {
            for col in 0..4 {
                // Horizontal half-pel: x interpolates to x0+col+0.5, y stays put.
                assert_eq!(
                    horiz[row * 4 + col] as i32,
                    20 * (3 + col as i32) + 10 + 4 * (3 + row as i32),
                    "horizontal-only MV must interpolate along x only"
                );
                // Vertical half-pel: y interpolates to y0+row+0.5, x stays put.
                assert_eq!(
                    vert[row * 4 + col] as i32,
                    20 * (3 + col as i32) + 4 * (3 + row as i32) + 2,
                    "vertical-only MV must interpolate along y only"
                );
            }
        }
        assert_ne!(
            horiz, vert,
            "horizontal-only and vertical-only sub-pel MVs must give different outputs"
        );
    }

    #[test]
    fn mv_past_the_edge_clamps_instead_of_panicking() {
    let fctx = &crate::decode::FrameCtx::new();
        let width = 8;
        let height = 8;
        let reference: Vec<u16> = (0..width * height).map(|i| (i * 3 % 200) as u16).collect();
        let mut dst = vec![0u16; 4 * 4];
        // Whole-sample position far above and to the left of the plane, with
        // an odd fraction so both filter passes exercise the clamp.
        predict(
            &reference,
            width,
            width,
            height,
            -50 * 16 + 5,
            -50 * 16 + 5,
            4,
            4,
            &mut dst, fctx,
        );
        let expected = reference[0]; // the whole plane clamps to the top-left corner
        assert!(
            dst.iter().all(|&v| v == expected),
            "an MV past the top-left edge must clamp to the corner sample, got {dst:?}"
        );

        let mut dst2 = vec![0u16; 4 * 4];
        predict(
            &reference,
            width,
            width,
            height,
            50 * 16 + 5,
            50 * 16 + 5,
            4,
            4,
            &mut dst2, fctx,
        );
        let expected2 = reference[height * width - 1]; // clamps to the bottom-right corner
        assert!(
            dst2.iter().all(|&v| v == expected2),
            "an MV past the bottom-right edge must clamp to the corner sample, got {dst2:?}"
        );
    }

    #[test]
    fn predict_scaled_at_no_scale_matches_predict_with_filters() {
    let fctx = &crate::decode::FrameCtx::new();
        // Algebraic pin from the r8/r9 derivation: x_scale_fp == REF_NO_SCALE
        // must reduce predict_scaled's per-column scaled walk to the exact
        // same int_pel/filter_idx sequence predict_with_filters computes from
        // a fixed stride-1 walk -- every subpel fraction, every block width.
        let width = 24;
        let height = 24;
        let reference: Vec<u16> = (0..width * height).map(|i| (i * 7 % 251) as u16).collect();
        for block_w in [4usize, 8, 16] {
            for x_frac in 0..16i32 {
                let x_q4 = 3 * 16 + x_frac;
                let y_q4 = 2 * 16 + 5;
                let mut expected = vec![0u16; block_w * 8];
                predict_with_filters(
                    &reference,
                    width,
                    width,
                    height,
                    x_q4,
                    y_q4,
                    block_w,
                    8,
                    InterpFilterKind::Regular,
                    InterpFilterKind::Regular,
                    &mut expected, fctx,
                );
                let mut got = vec![0u16; block_w * 8];
                predict_scaled(
                    &reference,
                    width,
                    width,
                    height,
                    x_q4,
                    y_q4,
                    REF_NO_SCALE,
                    block_w,
                    8,
                    InterpFilterKind::Regular,
                    InterpFilterKind::Regular,
                    &mut got, fctx,
                );
                assert_eq!(
                    got, expected,
                    "block_w={block_w} x_frac={x_frac}: REF_NO_SCALE must reproduce predict_with_filters exactly"
                );
            }
        }
    }

    /// lane-perf5: every explicit-SIMD kernel is checked against the scalar
    /// reference it replaces, over the whole domain a decode can hand it --
    /// all block widths (4..=128), all 16 sub-pel phases, all four filter
    /// kinds with both their wide and narrow (4-tap) tables, and 8/10/12-bit
    /// sample ranges. The dispatch itself is exercised through
    /// [`super::hpass_contig`] (whatever this CPU selected).
    #[allow(unsafe_code)]
    #[test]
    fn simd_matches_scalar_horizontal_pass() {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut rng = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let kinds = [
            InterpFilterKind::Regular,
            InterpFilterKind::Smooth,
            InterpFilterKind::Sharp,
            InterpFilterKind::Bilinear,
        ];
        let mut checked = 0usize;
        for depth_max in [255u16, 1023, 4095] {
            for &kind in &kinds {
                let (wide, narrow) = kind.tables();
                for table in [wide, narrow] {
                    for frac in 0..16usize {
                        let taps = &table[frac];
                        let t16 = [
                            taps[0] as i16, taps[1] as i16, taps[2] as i16, taps[3] as i16,
                            taps[4] as i16, taps[5] as i16, taps[6] as i16, taps[7] as i16,
                        ];
                        for w in [4usize, 8, 12, 16, 20, 32, 64, 128] {
                            let src: Vec<u16> = (0..w + 7)
                                .map(|_| (rng() % (u64::from(depth_max) + 1)) as u16)
                                .collect();
                            let mut want = vec![0i16; w];
                            super::hpass_contig_scalar(&src, &t16, &mut want);
                            let mut got = vec![0i16; w];
                            super::hpass_contig(&src, &t16, &mut got);
                            assert_eq!(got, want, "dispatch w={w} frac={frac} max={depth_max}");
                            #[cfg(target_arch = "x86_64")]
                            {
                                let mut sse = vec![0i16; w];
                                unsafe { super::simd::hpass_sse2(&src, 0, 1, sse.len(), &t16, &mut sse) };
                                assert_eq!(sse, want, "sse w={w} frac={frac} max={depth_max}");
                                if std::arch::is_x86_feature_detected!("avx2") {
                                    let mut avx = vec![0i16; w];
                                    unsafe { super::simd::hpass_avx2(&src, 0, 1, avx.len(), &t16, &mut avx) };
                                    assert_eq!(avx, want, "avx2 w={w} frac={frac} max={depth_max}");
                                }
                            }
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(checked, 3 * 4 * 2 * 16 * 8);
    }

    /// lane-mc: the intermediate the horizontal pass writes is `i16`, which
    /// is a claim about the FILTER TABLES, not about a sample: every kernel's
    /// positive taps sum to at most `S+` and its negative ones to `S-`, so a
    /// row's sum lies in `[max_sample * S-, max_sample * S+]` and the stored
    /// value is that over `1 << INTER_ROUND_0`. This enumerates all four
    /// filter kinds, both their wide and narrow tables and all 16 phases at
    /// the deepest sample this decoder accepts (10-bit; `stream.rs` refuses
    /// 12-bit by name) and pins the result inside `i16` -- so the SIMD
    /// stores' `packs_epi32` never saturates on a motion-compensation path.
    #[test]
    fn horizontal_intermediate_fits_i16() {
        let kinds = [
            InterpFilterKind::Regular,
            InterpFilterKind::Smooth,
            InterpFilterKind::Sharp,
            InterpFilterKind::Bilinear,
        ];
        let max_sample = 1023i32; // 10-bit
        let (mut worst_lo, mut worst_hi) = (0i32, 0i32);
        for &kind in &kinds {
            let (wide, narrow) = kind.tables();
            for table in [wide, narrow] {
                for taps in table {
                    let pos: i32 = taps.iter().filter(|&&t| t > 0).sum();
                    let neg: i32 = taps.iter().filter(|&&t| t < 0).sum();
                    worst_hi = worst_hi.max(super::round2(max_sample * pos, super::INTER_ROUND_0));
                    worst_lo = worst_lo.min(super::round2(max_sample * neg, super::INTER_ROUND_0));
                }
            }
        }
        assert!(
            worst_hi <= i32::from(i16::MAX) && worst_lo >= i32::from(i16::MIN),
            "the 10-bit intermediate range [{worst_lo}, {worst_hi}] must fit i16"
        );
    }

    /// lane-perf5: the vertical pass's SIMD kernels against the scalar
    /// reference, over the same domain as
    /// [`simd_matches_scalar_horizontal_pass`] -- all widths, all phases,
    /// both wide and narrow (4-tap) tables of all four filter kinds, and
    /// intermediates spanning the 8/10/12-bit ranges the horizontal pass can
    /// produce (a 12-bit sample times the filter gain, both signs).
    #[allow(unsafe_code)]
    /// lane-mc2: with `xfrac == 0` the horizontal pass is the identity tap,
    /// so its intermediate is exactly `s << 4` and the vertical pass reads it
    /// with the taps scaled by 16. The fast path in
    /// [`predict_with_filters_kern`] skips the intermediate and reads the
    /// reference rows directly; this pins that it reproduces the two-pass
    /// form value-for-value at every vertical fraction and every block width
    /// (including the odd width the encoder's motion search prices).
    #[test]
    fn whole_pel_horizontal_matches_the_two_pass_form() {
        let fctx = &crate::decode::FrameCtx::new();
        let width = 160usize;
        let height = 48usize;
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut rng = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let reference: Vec<u16> = (0..width * height).map(|_| (rng() % 256) as u16).collect();
        let mut checked = 0usize;
        for &kind in &[InterpFilterKind::Regular, InterpFilterKind::Smooth, InterpFilterKind::Sharp] {
            let (v_wide, v_narrow) = kind.tables();
            for (bw, bh) in [(4usize, 4usize), (5, 4), (8, 8), (16, 16), (12, 8), (32, 16)] {
                for yfrac in 1..16i32 {
                    let (x0, y0) = (7i32, 9i32);
                    let taps = if bh <= 4 { &v_narrow[yfrac as usize] } else { &v_wide[yfrac as usize] };
                    let mut want = vec![0u16; bw * bh];
                    for row in 0..bh {
                        for col in 0..bw {
                            let mut sum = 0i32;
                            for (t, &tap) in taps.iter().enumerate() {
                                let y = y0 - 3 + row as i32 + t as i32;
                                let s = i32::from(reference[y as usize * width + x0 as usize + col]);
                                // The identity horizontal tap's output.
                                sum += tap * (s << 4);
                            }
                            want[row * bw + col] = super::round2(sum, super::INTER_ROUND_1).clamp(0, 255) as u16;
                        }
                    }
                    let mut got = vec![0u16; bw * bh];
                    super::predict_with_filters_kern(
                        &reference, width, width, height, x0 * 16, y0 * 16 + yfrac,
                        bw, bh, bw, bh, kind, kind, &mut got, fctx,
                    );
                    assert_eq!(got, want, "kind={kind:?} bw={bw} bh={bh} yfrac={yfrac}");
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 3 * 6 * 15);
    }

    #[allow(unsafe_code)]
    #[test]
    fn simd_matches_scalar_vertical_pass() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut rng = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let kinds = [
            InterpFilterKind::Regular,
            InterpFilterKind::Smooth,
            InterpFilterKind::Sharp,
            InterpFilterKind::Bilinear,
        ];
        let mut checked = 0usize;
        // The horizontal pass's own range at 8- and 10-bit (12-bit is
        // refused in stream.rs), plus the saturating store's extreme.
        for span in [255i32 * 16, 1023 * 20, 32767] {
            for &kind in &kinds {
                let (wide, narrow) = kind.tables();
                for table in [wide, narrow] {
                    for frac in 0..16usize {
                        let taps = &table[frac];
                        for w in [4usize, 5, 8, 12, 16, 20, 32, 64, 128] {
                            let rows = 12usize;
                            // lane-mc2: `st != w` is the fused whole-pel
                            // horizontal path, which runs these kernels
                            // straight over a reference plane's stride.
                            for st in [w, w + 3] {
                            let inter: Vec<i16> = (0..rows * st)
                                .map(|_| ((rng() % (2 * span as u64 + 1)) as i32 - span) as i16)
                                .collect();
                            for row in [0usize, 1, 4] {
                                let mut want = vec![0i32; w];
                                super::vpass_row_scalar(&inter, w, st, row, taps, &mut want);
                                let mut got = vec![0i32; w];
                                super::vpass_row(&inter, w, st, row, taps, &mut got);
                                assert_eq!(got, want, "dispatch w={w} frac={frac} row={row}");
                                #[cfg(target_arch = "x86_64")]
                                {
                                    if std::arch::is_x86_feature_detected!("sse4.1") {
                                        let mut sse = vec![0i32; w];
                                        unsafe { super::simd::vpass_sse41(&inter, w, st, row, taps, &mut sse) };
                                        assert_eq!(sse, want, "sse4.1 w={w} frac={frac} row={row}");
                                    }
                                    if std::arch::is_x86_feature_detected!("avx2") {
                                        let mut avx = vec![0i32; w];
                                        unsafe { super::simd::vpass_avx2(&inter, w, st, row, taps, &mut avx) };
                                        assert_eq!(avx, want, "avx2 w={w} frac={frac} row={row}");
                                    }
                                }
                                // lane-mc: the rounding-fused form, over both
                                // sample maxima this decoder accepts -- its
                                // `packus`/`min_epu16` pair must reproduce
                                // `round2(_, InterRound1).clamp(0, max)`.
                                for max in [255i32, 1023] {
                                    let mut want_u16 = vec![0u16; w];
                                    super::vpass_row_u16_scalar(&inter, w, st, row, taps, max, &mut want_u16);
                                    for (d, &a) in want_u16.iter().zip(want.iter()) {
                                        assert_eq!(
                                            i32::from(*d),
                                            super::round2(a, super::INTER_ROUND_1).clamp(0, max),
                                            "the fused reference is the two-step one"
                                        );
                                    }
                                    let mut got_u16 = vec![0u16; w];
                                    super::vpass_row_u16(&inter[row * st..], w, st, 1, taps, max, &mut got_u16);
                                    assert_eq!(got_u16, want_u16, "fused w={w} frac={frac} row={row} max={max}");
                                    #[cfg(target_arch = "x86_64")]
                                    {
                                        if std::arch::is_x86_feature_detected!("sse4.1") {
                                            let mut sse = vec![0u16; w];
                                            unsafe { super::simd::vpass_row_u16_sse41(&inter[row * st..], w, st, 1, taps, max, &mut sse) };
                                            assert_eq!(sse, want_u16, "fused sse4.1 w={w} max={max}");
                                        }
                                        if std::arch::is_x86_feature_detected!("avx2") {
                                            let mut avx = vec![0u16; w];
                                            unsafe { super::simd::vpass_row_u16_avx2(&inter[row * st..], w, st, 1, taps, max, &mut avx) };
                                            assert_eq!(avx, want_u16, "fused avx2 w={w} max={max}");
                                        }
                                    }
                                }
                                checked += 1;
                            }
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(checked, 3 * 4 * 2 * 16 * 9 * 3 * 2);
    }
}

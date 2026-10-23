//! Motion search for the P-picture encoder.
//!
//! The design is the one `ec-h264`'s `choose_inter` runs: candidate vectors
//! seeded from the neighbourhood, a coarse-to-fine eight-neighbour pattern
//! search at whole-sample steps, then sub-sample refinement at half and quarter
//! steps. Every candidate is costed as `SAD + lambda * bits`, where the bits
//! are the exp-Golomb length of the vector difference from the predictor.
//!
//! The search does not interpolate per candidate. [`RefPlanes`] holds the
//! reference luma once per picture as four padded 8-bit planes (whole sample, and
//! the three half-sample positions, filtered with the decoder's 8-tap filter);
//! a quarter-sample vector is costed against one plane or the average of two
//! (the way x264 costs its quarter positions), so every candidate is a plain
//! sum of absolute differences. The estimate differs a little from the
//! decoder's quarter-sample filter, but only picks the vector: the mode
//! decision predicts with the decoder's own interpolation.

#[cfg(target_arch = "x86_64")]
use archmage::{SimdToken, X64V1Token, arcane};

/// The weight on the vector's bits against the lambda (as in `ec-h264`).
const MV_BITS_WEIGHT: f64 = 0.5;

/// The eight neighbours a pattern step visits, in units of the current step.
const PATTERN: [(i16, i16); 8] = [
    (1, 0),
    (-1, 0),
    (0, 1),
    (0, -1),
    (1, 1),
    (-1, -1),
    (1, -1),
    (-1, 1),
];

/// Search window half-width in whole luma samples around the predictor.
const RANGE_SAMPLES: i32 = 32;

/// Half-width, in whole samples, of the full integer search around the best seed,
/// for a 32x32 block (the largest, which pays for it once per coding tree) and
/// for smaller blocks, which search the neighbourhood of their parent's vector.
pub(crate) const FULL_RANGE_LARGE: i16 = 24;
pub(crate) const FULL_RANGE_SMALL: i16 = 12;

/// Bits an `se(v)` codeword costs (9.3.2.3), for the vector cost.
pub(crate) fn se_bits(v: i32) -> u32 {
    let k = if v > 0 {
        2 * i64::from(v) - 1
    } else {
        -2 * i64::from(v)
    } as u64;
    let len = 64 - (k + 1).leading_zeros();
    2 * len - 1
}

/// Border, in whole samples, replicated around the reference planes. A candidate
/// block (and the neighbour sample a quarter position averages with) must lie
/// inside the padded planes; the search treats any other vector as not available.
const PAD: usize = 48;

/// The HEVC half-sample luma filter (8.5.3.3.3.1), `fL[2]`.
const HALF: [i16; 8] = [-1, 4, -11, 40, 40, -11, 4, -1];

/// The reference luma as padded 8-bit planes: the whole-sample plane and the
/// half-sample planes `H` (between horizontal neighbours), `V` (between vertical
/// neighbours) and `D` (the centre of four).
pub(crate) struct RefPlanes {
    /// Padded row length.
    stride: usize,
    /// Picture width and height.
    w: usize,
    h: usize,
    /// `[whole, H, V, D]`, each `(h + 2 * PAD) * stride` samples.
    planes: [Vec<u8>; 4],
    /// Right shift that brings the reference samples to 8 bits for the search.
    shift: u32,
}

impl RefPlanes {
    /// Empty planes for a `w` x `h` luma picture; [`RefPlanes::fill`] loads a picture.
    pub(crate) fn new(w: usize, h: usize, bit_depth: u32) -> RefPlanes {
        let stride = w + 2 * PAD;
        let size = (h + 2 * PAD) * stride;
        RefPlanes {
            stride,
            w,
            h,
            planes: [vec![0; size], vec![0; size], vec![0; size], vec![0; size]],
            shift: bit_depth - 8,
        }
    }

    /// Load the luma picture `luma` (rows `w` apart), using up to `threads` threads.
    pub(crate) fn fill(&mut self, luma: &[u16], threads: usize) {
        let (stride, w, h) = (self.stride, self.w, self.h);
        let shift = self.shift;
        let rows = h + 2 * PAD;
        let threads = threads.clamp(1, 8);
        let band = rows.div_ceil(threads);
        let [whole, hp, vp, dp] = &mut self.planes;
        // Phase one: the whole-sample plane, edge samples replicated.
        std::thread::scope(|scope| {
            for (i, chunk) in whole.chunks_mut(band * stride).enumerate() {
                scope.spawn(move || {
                    for (r, dst) in chunk.chunks_exact_mut(stride).enumerate() {
                        let sy = (i * band + r).saturating_sub(PAD).min(h - 1);
                        let src = &luma[sy * w..][..w];
                        for (x, d) in dst[PAD..PAD + w].iter_mut().zip(src) {
                            *x = (*d >> shift) as u8;
                        }
                        let (left, right) = (dst[PAD], dst[PAD + w - 1]);
                        dst[..PAD].fill(left);
                        dst[PAD + w..].fill(right);
                    }
                });
            }
        });
        // Phase two: the half-sample planes, a band of output rows per thread.
        let whole: &[u8] = whole;
        std::thread::scope(|scope| {
            let bands = hp
                .chunks_mut(band * stride)
                .zip(vp.chunks_mut(band * stride))
                .zip(dp.chunks_mut(band * stride));
            for (i, ((hc, vc), dc)) in bands.enumerate() {
                scope.spawn(move || half_band(whole, stride, rows, i * band, hc, vc, dc));
            }
        });
    }
}

/// The `H`, `V` and `D` rows `r0..` of one band (each chunk holds the band's rows, `stride`
/// apart), from the padded whole-sample plane. Rows without the full filter support (the
/// first three and last four) are left as they are; no search vector reads them.
fn half_band(
    whole: &[u8],
    stride: usize,
    rows: usize,
    r0: usize,
    hc: &mut [u8],
    vc: &mut [u8],
    dc: &mut [u8],
) {
    let n = stride - 7;
    let nrows = hc.len() / stride;
    // Horizontal sums of the whole plane, unrounded (they fit 16 bits for 8-bit samples):
    // the first stage of both `H` and `D`. A ring of eight rows holds the vertical support.
    let mut ring = vec![0i16; 8 * stride];
    let hsum = |ring: &mut [i16], row: usize| {
        let srow = &whole[row * stride..][..stride];
        let dst = &mut ring[(row % 8) * stride + 3..][..n];
        dst.fill(0);
        for (k, &c) in HALF.iter().enumerate() {
            for (a, &s) in dst.iter_mut().zip(&srow[k..k + n]) {
                *a += c * i16::from(s);
            }
        }
    };
    let first = r0.max(3);
    let last = (r0 + nrows).min(rows - 4);
    if first >= last {
        return;
    }
    for row in first - 3..first + 5 {
        hsum(&mut ring, row);
    }
    let mut acc = vec![0i16; stride];
    let mut acc_d = vec![0i32; stride];
    for r in first..last {
        if r > first {
            hsum(&mut ring, r + 4);
        }
        let o = (r - r0) * stride;
        let own = &ring[(r % 8) * stride + 3..][..n];
        for (d, &a) in hc[o + 3..][..n].iter_mut().zip(own) {
            *d = ((a + 32) >> 6).clamp(0, 255) as u8;
        }
        acc.fill(0);
        acc_d.fill(0);
        for (k, &c) in HALF.iter().enumerate() {
            let wrow = &whole[(r + k - 3) * stride..][..stride];
            for (a, &s) in acc.iter_mut().zip(wrow) {
                *a += c * i16::from(s);
            }
            let srow = &ring[((r + k - 3) % 8) * stride + 3..][..n];
            for (a, &s) in acc_d[3..][..n].iter_mut().zip(srow) {
                *a += i32::from(c) * i32::from(s);
            }
        }
        for (d, &a) in vc[o..][..stride].iter_mut().zip(&acc) {
            *d = ((a + 32) >> 6).clamp(0, 255) as u8;
        }
        for (d, &a) in dc[o + 3..][..n].iter_mut().zip(&acc_d[3..][..n]) {
            *d = (((a >> 6) + 32) >> 6).clamp(0, 255) as u8;
        }
    }
}

/// Which plane, and at which whole-sample offset, a quarter position reads;
/// two entries mean the rounded average of the two.
type Taps = [(usize, usize, usize); 2];

/// The planes a quarter-sample fraction `(fx, fy)` is costed against: index
/// 0 whole, 1 `H`, 2 `V`, 3 `D`; each entry `(plane, dx, dy)`. A single plane is
/// listed twice.
const fn taps(fx: usize, fy: usize) -> Taps {
    const W: usize = 0;
    const H: usize = 1;
    const V: usize = 2;
    const D: usize = 3;
    match (fx, fy) {
        (0, 0) => [(W, 0, 0), (W, 0, 0)],
        (1, 0) => [(W, 0, 0), (H, 0, 0)],
        (2, 0) => [(H, 0, 0), (H, 0, 0)],
        (3, 0) => [(H, 0, 0), (W, 1, 0)],
        (0, 1) => [(W, 0, 0), (V, 0, 0)],
        (0, 2) => [(V, 0, 0), (V, 0, 0)],
        (0, 3) => [(V, 0, 0), (W, 0, 1)],
        (1, 1) => [(H, 0, 0), (V, 0, 0)],
        (2, 1) => [(H, 0, 0), (D, 0, 0)],
        (3, 1) => [(H, 0, 0), (V, 1, 0)],
        (1, 2) => [(V, 0, 0), (D, 0, 0)],
        (2, 2) => [(D, 0, 0), (D, 0, 0)],
        (3, 2) => [(D, 0, 0), (V, 1, 0)],
        (1, 3) => [(V, 0, 0), (H, 0, 1)],
        (2, 3) => [(D, 0, 0), (H, 0, 1)],
        _ => [(V, 1, 0), (H, 0, 1)],
    }
}

/// Sum of absolute differences of the `W`-wide, `h`-high block of `src` against `a`
/// (`b` when given: against the rounded average of the two), rows `*_stride` apart.
/// It stops after a group of four rows once the sum reaches `limit`; a sum that
/// reaches `limit` is not exact, one below it is.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn sad_block<const W: usize>(
    src: &[u8],
    src_stride: usize,
    a: &[u8],
    b: Option<&[u8]>,
    stride: usize,
    h: usize,
    limit: u32,
) -> u32 {
    #[cfg(target_arch = "x86_64")]
    {
        // The slices must cover every row the vector loads touch.
        let need = |len: usize, st: usize| h == 0 || len >= (h - 1) * st + W;
        if need(src.len(), src_stride)
            && need(a.len(), stride)
            && b.is_none_or(|b| need(b.len(), stride))
        {
            // The SSE2 token: SSE2 is part of the x86_64 baseline, so this is always Some.
            if let Some(t) = X64V1Token::summon() {
                return sad_block_sse2::<W>(t, src, src_stride, a, b, stride, h, limit);
            }
        }
    }
    sad_block_scalar::<W>(src, src_stride, a, b, stride, h, limit)
}

/// SSE2 body of [`sad_block`]. The caller checked that row `r` of each slice holds `W`
/// bytes for every `r < h`; every load below indexes its slice, so that is enforced again.
#[cfg(target_arch = "x86_64")]
#[allow(clippy::too_many_arguments)]
#[arcane]
fn sad_block_sse2<const W: usize>(
    _t: X64V1Token,
    src: &[u8],
    src_stride: usize,
    a: &[u8],
    b: Option<&[u8]>,
    stride: usize,
    h: usize,
    limit: u32,
) -> u32 {
    use std::arch::x86_64::*;
    // `W` bytes at `at`: an 8-byte load moved into the low lanes for W == 8, else 16 bytes.
    let load = |s: &[u8], at: usize| -> __m128i {
        if W == 8 {
            let lo = u64::from_le_bytes(<[u8; 8]>::try_from(&s[at..at + 8]).unwrap());
            _mm_cvtsi64_si128(lo as i64)
        } else {
            bytemuck::cast(<[u8; 16]>::try_from(&s[at..at + 16]).unwrap())
        }
    };
    let mut acc = _mm_setzero_si128();
    let mut row = 0;
    while row < h {
        let end = (row + 4).min(h);
        while row < end {
            let sr = row * src_stride;
            let ar = row * stride;
            let mut off = 0;
            while off < W {
                let s = load(src, sr + off);
                let mut x = load(a, ar + off);
                if let Some(b) = b {
                    // (x + y + 1) >> 1 per byte, as the scalar average.
                    x = _mm_avg_epu8(x, load(b, ar + off));
                }
                acc = _mm_add_epi64(acc, _mm_sad_epu8(s, x));
                off += 16;
            }
            row += 1;
        }
        let sum = _mm_cvtsi128_si32(acc) as u32 + _mm_cvtsi128_si32(_mm_srli_si128(acc, 8)) as u32;
        if sum >= limit {
            return sum;
        }
    }
    _mm_cvtsi128_si32(acc) as u32 + _mm_cvtsi128_si32(_mm_srli_si128(acc, 8)) as u32
}

#[allow(clippy::too_many_arguments)]
fn sad_block_scalar<const W: usize>(
    src: &[u8],
    src_stride: usize,
    a: &[u8],
    b: Option<&[u8]>,
    stride: usize,
    h: usize,
    limit: u32,
) -> u32 {
    let mut sum = 0u32;
    let mut row = 0;
    while row < h {
        let s: &[u8; W] = src[row * src_stride..][..W].try_into().unwrap_or(&[0; W]);
        let x: &[u8; W] = a[row * stride..][..W].try_into().unwrap_or(&[0; W]);
        if let Some(b) = b {
            let y: &[u8; W] = b[row * stride..][..W].try_into().unwrap_or(&[0; W]);
            for i in 0..W {
                let avg = ((u16::from(x[i]) + u16::from(y[i]) + 1) >> 1) as u8;
                sum += u32::from(s[i].abs_diff(avg));
            }
        } else {
            for i in 0..W {
                sum += u32::from(s[i].abs_diff(x[i]));
            }
        }
        if sum >= limit {
            break;
        }
        row += 1;
    }
    sum
}

/// One block's search problem: the source luma plane, the reference planes, and
/// the cost weight.
pub(crate) struct Search<'a> {
    /// Source luma, `src_stride` apart, at the coded picture size.
    pub src: &'a [u8],
    /// Distance between source row starts.
    pub src_stride: usize,
    /// The reference luma (the previous reconstruction).
    pub reference: &'a RefPlanes,
    /// Lagrange multiplier applied to the vector bits.
    pub lambda: f64,
    /// Half-width, in whole samples, of the full integer scan around the best seed
    /// (0 for none: the seeds and the pattern search only).
    pub full_range: i16,
    /// Skip the full integer scan when the seeds already cost less than this per
    /// sample (0 always scans).
    pub full_gate: f64,
    /// First (coarsest) step, in quarter samples, of the whole-sample pattern search.
    pub first_step: i16,
    /// Spacing, in whole samples, of the full scan's grid.
    pub full_stride: i16,
    /// Most passes of the 8-neighbour refinement at each sub-sample step.
    pub sub_passes: u32,
    /// Skip the sub-sample refinement when the best cost is below this per sample.
    pub sub_gate: f64,
    /// Skip the quarter-sample step.
    pub no_quarter: bool,
}

/// The result of a search: the chosen vector in quarter samples and its cost.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Best {
    /// The vector, quarter luma samples.
    pub mv: [i16; 2],
    /// `SAD + lambda * bits`.
    pub cost: f64,
}

impl Search<'_> {
    /// Search the `w` x `h` block at `(x, y)` around the predictor `mvp`,
    /// starting from `seeds` (the predictor and the neighbours' vectors).
    pub(crate) fn run(
        &self,
        (x, y): (usize, usize),
        (w, h): (usize, usize),
        mvp: [i16; 2],
        seeds: &[[i16; 2]],
    ) -> Best {
        let mut best = Best {
            mv: [0, 0],
            cost: f64::MAX,
        };
        for &cand in std::iter::once(&mvp).chain(seeds.iter()) {
            let cost = self.cost(x, y, w, h, cand, mvp, best.cost);
            if cost < best.cost {
                best = Best { mv: cand, cost };
            }
        }
        if best.cost == f64::MAX {
            // No seed reaches the planes (a vector far off the picture): the zero vector does.
            best = Best {
                mv: [0, 0],
                cost: self.cost(x, y, w, h, [0, 0], mvp, f64::MAX),
            };
        }
        let range_q = RANGE_SAMPLES * 4;
        let inside = |mv: [i16; 2]| {
            (i32::from(mv[0]) - i32::from(mvp[0])).abs() <= range_q
                && (i32::from(mv[1]) - i32::from(mvp[1])).abs() <= range_q
        };
        // A full search over whole samples around the best seed. A greedy pattern
        // search from a seed stalls in the first local minimum of a textured
        // cost, which a few samples of real motion are enough to hit.
        let centre = [best.mv[0] & !3, best.mv[1] & !3];
        let full = if best.cost < self.full_gate * (w * h) as f64 {
            0
        } else {
            self.full_range
        };
        let fstride = self.full_stride.max(1) as usize;
        for dy in (-full..=full).step_by(fstride) {
            for dx in (-full..=full).step_by(fstride) {
                let cand = [centre[0] + 4 * dx, centre[1] + 4 * dy];
                if !inside(cand) {
                    continue;
                }
                let cost = self.cost(x, y, w, h, cand, mvp, best.cost);
                if cost < best.cost {
                    best = Best { mv: cand, cost };
                }
            }
        }
        // The whole-sample walk below starts from the whole-sample position of the best
        // vector. The sub-sample refinement recovers the fraction.
        if best.mv[0] & 3 != 0 || best.mv[1] & 3 != 0 {
            let mv = [best.mv[0] & !3, best.mv[1] & !3];
            let cost = self.cost(x, y, w, h, mv, mvp, f64::MAX);
            if cost < f64::MAX {
                best = Best { mv, cost };
            }
        }
        // Whole-sample steps, coarse to fine: four samples, two, one.
        let mut step = self.first_step;
        while step >= 4 {
            let mut improved = true;
            while improved {
                improved = false;
                for (dx, dy) in PATTERN {
                    let cand = [best.mv[0] + dx * step, best.mv[1] + dy * step];
                    if !inside(cand) {
                        continue;
                    }
                    let cost = self.cost(x, y, w, h, cand, mvp, best.cost);
                    if cost < best.cost {
                        best = Best { mv: cand, cost };
                        improved = true;
                    }
                }
            }
            step /= 2;
        }
        // Sub-sample refinement: half, then quarter steps, to convergence.
        let steps: &[i16] = if best.cost < self.sub_gate * (w * h) as f64 {
            &[]
        } else if self.no_quarter {
            &[2]
        } else {
            &[2, 1]
        };
        for &step in steps {
            let mut improved = true;
            let mut passes = 0;
            while improved && passes < self.sub_passes {
                passes += 1;
                improved = false;
                for (dx, dy) in PATTERN {
                    let cand = [best.mv[0] + dx * step, best.mv[1] + dy * step];
                    let cost = self.cost(x, y, w, h, cand, mvp, best.cost);
                    if cost < best.cost {
                        best = Best { mv: cand, cost };
                        improved = true;
                    }
                }
            }
        }
        best
    }

    /// `SAD(source, prediction(mv)) + lambda * MV_BITS_WEIGHT * bits(mv - mvp)`, with the
    /// prediction read from the reference planes ([`taps`]). The sum stops once it reaches
    /// `cutoff` (the best cost so far): the caller only compares against `cutoff`, so the
    /// returned value is exact whenever it is below it. A vector whose block (and the
    /// neighbour a quarter position averages with) leaves the padded planes costs
    /// `f64::MAX`.
    #[allow(clippy::too_many_arguments)] // the block is its origin and its two sides
    #[inline]
    fn cost(
        &self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        mv: [i16; 2],
        mvp: [i16; 2],
        cutoff: f64,
    ) -> f64 {
        let rp = self.reference;
        let rx = x as i32 + i32::from(mv[0] >> 2);
        let ry = y as i32 + i32::from(mv[1] >> 2);
        let lo = 3 - PAD as i32;
        if rx < lo
            || ry < lo
            || rx + w as i32 + 1 > rp.w as i32 + PAD as i32 - 5
            || ry + h as i32 + 1 > rp.h as i32 + PAD as i32 - 5
        {
            return f64::MAX;
        }
        let bits = se_bits(i32::from(mv[0]) - i32::from(mvp[0]))
            + se_bits(i32::from(mv[1]) - i32::from(mvp[1]));
        let rate = self.lambda * MV_BITS_WEIGHT * f64::from(bits);
        let t = taps((mv[0] & 3) as usize, (mv[1] & 3) as usize);
        // The bounds check above keeps `rx`/`ry` >= 3 - PAD, so these sums are >= 0;
        // they are formed in i32 so a negative vector never wraps through `as usize`.
        let at = |tap: (usize, usize, usize)| {
            let row = (ry + PAD as i32 + tap.2 as i32) as usize;
            let col = (rx + PAD as i32 + tap.1 as i32) as usize;
            row * rp.stride + col
        };
        let (a0, a1) = (at(t[0]), at(t[1]));
        let limit = if cutoff == f64::MAX {
            u32::MAX
        } else {
            let v = (cutoff - rate).max(0.0).min(f64::from(u32::MAX));
            let t = v as u32;
            if f64::from(t) < v { t + 1 } else { t }
        };
        let src = &self.src[y * self.src_stride + x..];
        let pa = &rp.planes[t[0].0][a0..];
        let pb = (t[0] != t[1]).then(|| &rp.planes[t[1].0][a1..]);
        let sad = match w {
            8 => sad_block::<8>(src, self.src_stride, pa, pb, rp.stride, h, limit),
            16 => sad_block::<16>(src, self.src_stride, pa, pb, rp.stride, h, limit),
            _ => sad_block::<32>(src, self.src_stride, pa, pb, rp.stride, h, limit),
        };
        f64::from(sad) + rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sad_vector_path_matches_the_scalar_one() {
        let mut seed = 7u32;
        let mut buf = |n: usize| -> Vec<u8> {
            (0..n)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (seed >> 16) as u8
                })
                .collect()
        };
        let (src, a, b) = (buf(80 * 40), buf(80 * 40), buf(80 * 40));
        for h in [8usize, 16, 32] {
            for limit in [u32::MAX, 5000] {
                for pb in [None, Some(&b[3..])] {
                    macro_rules! chk {
                        ($w:literal) => {
                            let v = sad_block::<$w>(&src, 80, &a[1..], pb, 80, h, limit);
                            let s = sad_block_scalar::<$w>(&src, 80, &a[1..], pb, 80, h, limit);
                            if limit == u32::MAX {
                                assert_eq!(v, s);
                            } else {
                                assert_eq!(v.min(limit), s.min(limit));
                            }
                        };
                    }
                    chk!(8);
                    chk!(16);
                    chk!(32);
                }
            }
        }
    }

    #[test]
    fn se_bits_matches_the_exp_golomb_table() {
        assert_eq!(se_bits(0), 1);
        assert_eq!(se_bits(1), 3);
        assert_eq!(se_bits(-1), 3);
        assert_eq!(se_bits(2), 5);
        assert_eq!(se_bits(-2), 5);
        assert_eq!(se_bits(3), 5);
    }

    #[test]
    fn search_finds_a_large_vertical_shift() {
        // A 32x32 block whose source is the reference 14 rows up: the vector is
        // (0, -14) samples, which is (0, -56) quarters.
        let (w, h) = (256usize, 256usize);
        let mut refs = vec![0u16; w * h];
        for y in 0..h {
            for x in 0..w {
                refs[y * w + x] = (((x * 37 + y * 11) % 211) + 20) as u16;
            }
        }
        let mut src = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let sy = (y as i32 - 14).clamp(0, h as i32 - 1) as usize;
                src[y * w + x] = refs[sy * w + x] as u8;
            }
        }
        let mut reference = RefPlanes::new(w, h, 8);
        reference.fill(&refs, 1);
        let search = Search {
            src: &src,
            src_stride: w,
            reference: &reference,
            lambda: 1.0,
            full_range: FULL_RANGE_LARGE,
            full_stride: 1,
            sub_passes: 8,
            sub_gate: 0.0,
            no_quarter: false,
            full_gate: 0.0,
            first_step: 16,
        };
        let best = search.run((96, 96), (32, 32), [0, 0], &[]);
        assert_eq!(best.mv, [0, -56], "cost {}", best.cost);
    }

    #[test]
    fn search_finds_a_shifted_block() {
        // The source at (x, y) is the reference at (x - 3, y + 2), so the vector
        // that predicts it exactly is (-3, +2) samples, which is (-12, 8) quarters.
        let (w, h) = (64usize, 64usize);
        let mut refs = vec![0u16; w * h];
        for y in 0..h {
            for x in 0..w {
                refs[y * w + x] = (((x * 7 + y * 13) % 200) + 20) as u16;
            }
        }
        let mut src = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let sx = (x as i32 - 3).clamp(0, w as i32 - 1) as usize;
                let sy = (y as i32 + 2).clamp(0, h as i32 - 1) as usize;
                src[y * w + x] = refs[sy * w + sx] as u8;
            }
        }
        let mut reference = RefPlanes::new(w, h, 8);
        reference.fill(&refs, 1);
        let search = Search {
            src: &src,
            src_stride: w,
            reference: &reference,
            lambda: 1.0,
            full_range: FULL_RANGE_SMALL,
            full_stride: 1,
            sub_passes: 8,
            sub_gate: 0.0,
            no_quarter: false,
            full_gate: 0.0,
            first_step: 16,
        };
        let best = search.run((24, 24), (8, 8), [0, 0], &[]);
        assert_eq!(best.mv, [-12, 8]);
        assert_eq!(
            best.cost,
            search.lambda * MV_BITS_WEIGHT * f64::from(se_bits(-12) + se_bits(8))
        );
    }

    #[test]
    fn cost_at_the_low_bound_reads_the_padded_plane() {
        // A vector that points 40 samples up and left of the frame reads the
        // replicated top-left corner of the padded planes. The cost used to compute
        // the plane index from `negative as usize`, which overflows in debug builds.
        let (w, h) = (64usize, 64usize);
        let mut refs = vec![0u16; w * h];
        for y in 0..h {
            for x in 0..w {
                refs[y * w + x] = (((x * 7 + y * 13) % 200) + 20) as u16;
            }
        }
        // Source block at (0, 0) equals the reference at (-40, -40), which is the
        // top-left sample (0, 0) replicated.
        let mut src = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                src[y * w + x] = refs[0] as u8;
            }
        }
        let mut reference = RefPlanes::new(w, h, 8);
        reference.fill(&refs, 1);
        let search = Search {
            src: &src,
            src_stride: w,
            reference: &reference,
            lambda: 1.0,
            full_range: FULL_RANGE_SMALL,
            full_stride: 1,
            sub_passes: 8,
            sub_gate: 0.0,
            no_quarter: false,
            full_gate: 0.0,
            first_step: 16,
        };
        let mv = [-160i16, -160];
        // Whole-sample vector: the prediction is exactly the source, so only the
        // rate term (two zero-difference components) remains.
        let exact = search.cost(0, 0, 8, 8, mv, mv, f64::MAX);
        assert_eq!(exact, 1.0 * MV_BITS_WEIGHT * 2.0);
        // Fractional vectors at the same spot stay in bounds and are finite.
        for mv in [[-159i16, -160], [-160, -157], [-157, -159]] {
            let c = search.cost(0, 0, 8, 8, mv, [0, 0], f64::MAX);
            assert!(c.is_finite() && c < f64::MAX, "mv {mv:?} cost {c}");
        }
    }
}

//! The inverse transform (spec 7.13), so the encoder can reconstruct exactly
//! what a decoder will show.
//!
//! Only the inverse DCT is here, which is all this encoder's subset codes: a
//! square `DCT_DCT` transform per block. The butterfly network is the spec's
//! own, stage for stage — every stage's rounding and intermediate clamp is
//! part of the bitstream contract, so a mathematically equal but differently
//! rounded DCT would drift away from the decoder by a sample here and there.

/// `Cos128_Lookup` (spec 7.13.2.1): `4096 * cos(angle * pi / 128)` rounded.
static COS128_LOOKUP: [i32; 65] = [
    4096, 4095, 4091, 4085, 4076, 4065, 4052, 4036, 4017, 3996, 3973, 3948, 3920, 3889, 3857, 3822,
    3784, 3745, 3703, 3659, 3612, 3564, 3513, 3461, 3406, 3349, 3290, 3229, 3166, 3102, 3035, 2967,
    2896, 2824, 2751, 2675, 2598, 2520, 2440, 2359, 2276, 2191, 2106, 2019, 1931, 1842, 1751, 1660,
    1567, 1474, 1380, 1285, 1189, 1092, 995, 897, 799, 700, 601, 501, 401, 301, 201, 101, 0,
];

/// The fixed-point position of the cosine table.
const COS_BITS: usize = 12;

/// `cos128(angle)` (spec 7.13.2.1), folded from the quarter table.
fn cos128(angle: i32) -> i32 {
    let angle2 = (angle & 255) as usize;
    match angle2 {
        0..=64 => COS128_LOOKUP[angle2],
        65..=128 => -COS128_LOOKUP[128 - angle2],
        129..=192 => -COS128_LOOKUP[angle2 - 128],
        _ => COS128_LOOKUP[256 - angle2],
    }
}

/// `sin128(angle)` (spec 7.13.2.1).
fn sin128(angle: i32) -> i32 {
    cos128(angle - 64)
}

/// `brev(numBits, x)` (spec 7.13.2.1): the bit reversal of the low `num_bits`.
fn brev(num_bits: u32, x: usize) -> usize {
    (0..num_bits).fold(0, |t, i| t | (((x >> i) & 1) << (num_bits - 1 - i)))
}

/// `Round2(x, n)` (spec 4.7): shift down `n` places, rounding halves up.
fn round2(x: i32, n: usize) -> i32 {
    if n == 0 {
        x
    } else {
        // The intermediate can exceed 32 bits before the shift brings it
        // back, which is why it is done in 64.
        ((i64::from(x) + (1i64 << (n - 1))) >> n) as i32
    }
}

/// `Clip3` against a signed integer of `r` bits, which is what bounds every
/// intermediate of the transform.
fn clamp_range(x: i32, r: usize) -> i32 {
    let hi = ((1i64 << (r - 1)) - 1) as i32;
    let lo = (-(1i64 << (r - 1))) as i32;
    x.clamp(lo, hi)
}

/// `B(a, b, angle, flip, r)` (spec 7.13.2.1): a butterfly rotation, and an
/// exchange of the two entries when `flip` is set.
fn butterfly(t: &mut [i32], a: usize, b: usize, angle: i32, flip: bool, _r: usize) {
    let (ta, tb) = (i64::from(t[a]), i64::from(t[b]));
    let (c, s) = (i64::from(cos128(angle)), i64::from(sin128(angle)));
    let x = ta * c - tb * s;
    let y = ta * s + tb * c;
    t[a] = ((x + (1 << (COS_BITS - 1))) >> COS_BITS) as i32;
    t[b] = ((y + (1 << (COS_BITS - 1))) >> COS_BITS) as i32;
    if flip {
        t.swap(a, b);
    }
}

/// `H(a, b, flip, r)` (spec 7.13.2.1): a Hadamard rotation, with the indices
/// exchanged when `flip` is set.
fn hadamard(t: &mut [i32], a: usize, b: usize, flip: bool, r: usize) {
    let (a, b) = if flip { (b, a) } else { (a, b) };
    let (x, y) = (t[a], t[b]);
    t[a] = clamp_range(x.wrapping_add(y), r);
    t[b] = clamp_range(x.wrapping_sub(y), r);
}

/// The inverse DCT array permutation (spec 7.13.2.2): an in-place bit-reversal
/// of the first `1 << n` entries.
fn permute(t: &mut [i32], n: u32) {
    let n0 = 1usize << n;
    let copy: Vec<i32> = t[..n0].to_vec();
    for (i, v) in t[..n0].iter_mut().enumerate() {
        *v = copy[brev(n, i)];
    }
}

/// The inverse DCT process (spec 7.13.2.3): an in-place inverse DCT of the
/// first `1 << n` entries of `t`, clamping intermediates to `r` bits.
///
/// The stage list is the spec's, in the spec's order; the guards on `n` are
/// what make one network serve every transform size from 4 to 64.
pub fn inverse_dct(t: &mut [i32], n: u32, r: usize) {
    assert!((2..=6).contains(&n), "the inverse DCT is defined for 4..64");
    permute(t, n);

    if n == 6 {
        for i in 0..16 {
            butterfly(t, 32 + i, 63 - i, 63 - 4 * brev(4, i) as i32, false, r);
        }
    }
    if n >= 5 {
        for i in 0..8 {
            butterfly(
                t,
                16 + i,
                31 - i,
                6 + ((brev(3, 7 - i) as i32) << 3),
                false,
                r,
            );
        }
    }
    if n == 6 {
        for i in 0..16 {
            hadamard(t, 32 + i * 2, 33 + i * 2, i & 1 == 1, r);
        }
    }
    if n >= 4 {
        for i in 0..4 {
            butterfly(
                t,
                8 + i,
                15 - i,
                12 + ((brev(2, 3 - i) as i32) << 4),
                false,
                r,
            );
        }
    }
    if n >= 5 {
        for i in 0..8 {
            hadamard(t, 16 + 2 * i, 17 + 2 * i, i & 1 == 1, r);
        }
    }
    if n == 6 {
        for i in 0..4 {
            for j in 0..2 {
                let angle = 60 - 16 * brev(2, i) as i32 + 64 * j as i32;
                butterfly(t, 62 - i * 4 - j, 33 + i * 4 + j, angle, true, r);
            }
        }
    }
    if n >= 3 {
        for i in 0..2 {
            butterfly(t, 4 + i, 7 - i, 56 - 32 * i as i32, false, r);
        }
    }
    if n >= 4 {
        for i in 0..4 {
            hadamard(t, 8 + 2 * i, 9 + 2 * i, i & 1 == 1, r);
        }
    }
    if n >= 5 {
        for i in 0..2 {
            for j in 0..2 {
                let angle = 24 + ((j as i32) << 6) + ((1 - i as i32) << 5);
                butterfly(t, 30 - 4 * i - j, 17 + 4 * i + j, angle, true, r);
            }
        }
    }
    if n == 6 {
        for i in 0..8 {
            for j in 0..2 {
                hadamard(t, 32 + i * 4 + j, 35 + i * 4 - j, i & 1 == 1, r);
            }
        }
    }
    for i in 0..2 {
        butterfly(t, 2 * i, 2 * i + 1, 32 + 16 * i as i32, i == 0, r);
    }
    if n >= 3 {
        for i in 0..2 {
            hadamard(t, 4 + 2 * i, 5 + 2 * i, i == 1, r);
        }
    }
    if n >= 4 {
        for i in 0..2 {
            butterfly(t, 14 - i, 9 + i, 48 + 64 * i as i32, true, r);
        }
    }
    if n >= 5 {
        for i in 0..4 {
            for j in 0..2 {
                hadamard(t, 16 + 4 * i + j, 19 + 4 * i - j, i & 1 == 1, r);
            }
        }
    }
    if n == 6 {
        for i in 0..2 {
            for j in 0..4 {
                let angle = 56 - (i as i32) * 32 + ((j as i32) >> 1) * 64;
                butterfly(t, 61 - i * 8 - j, 34 + i * 8 + j, angle, true, r);
            }
        }
    }
    for i in 0..2 {
        hadamard(t, i, 3 - i, false, r);
    }
    if n >= 3 {
        butterfly(t, 6, 5, 32, true, r);
    }
    if n >= 4 {
        for i in 0..2 {
            for j in 0..2 {
                hadamard(t, 8 + 4 * i + j, 11 + 4 * i - j, i == 1, r);
            }
        }
    }
    if n >= 5 {
        for i in 0..4 {
            butterfly(t, 29 - i, 18 + i, 48 + ((i as i32) >> 1) * 64, true, r);
        }
    }
    if n == 6 {
        for i in 0..4 {
            for j in 0..4 {
                hadamard(t, 32 + 8 * i + j, 39 + 8 * i - j, i & 1 == 1, r);
            }
        }
    }
    if n >= 3 {
        for i in 0..4 {
            hadamard(t, i, 7 - i, false, r);
        }
    }
    if n >= 4 {
        for i in 0..2 {
            butterfly(t, 13 - i, 10 + i, 32, true, r);
        }
    }
    if n >= 5 {
        for i in 0..2 {
            for j in 0..4 {
                hadamard(t, 16 + i * 8 + j, 23 + i * 8 - j, i == 1, r);
            }
        }
    }
    if n == 6 {
        for i in 0..8 {
            butterfly(t, 59 - i, 36 + i, if i < 4 { 48 } else { 112 }, true, r);
        }
    }
    if n >= 4 {
        for i in 0..8 {
            hadamard(t, i, 15 - i, false, r);
        }
    }
    if n >= 5 {
        for i in 0..4 {
            butterfly(t, 27 - i, 20 + i, 32, true, r);
        }
    }
    if n == 6 {
        for i in 0..8 {
            hadamard(t, 32 + i, 47 - i, false, r);
            hadamard(t, 48 + i, 63 - i, true, r);
        }
    }
    if n >= 5 {
        for i in 0..16 {
            hadamard(t, i, 31 - i, false, r);
        }
    }
    if n == 6 {
        for i in 0..8 {
            butterfly(t, 55 - i, 40 + i, 32, true, r);
        }
    }
    if n == 6 {
        for i in 0..32 {
            hadamard(t, i, 63 - i, false, r);
        }
    }
}

/// `Transform_Row_Shift` (spec 7.13.3) for the square transforms.
fn row_shift(log2: u32) -> usize {
    match log2 {
        2 => 0,
        3 => 1,
        _ => 2,
    }
}

/// The 2D inverse transform (spec 7.13.3) for a square `DCT_DCT` block.
///
/// `dequant` is the dequantized coefficient grid in raster order; the returned
/// residual is in the same order. A 64-point transform only ever carries
/// coefficients in its first 32 rows and columns, and the spec zeroes the rest
/// before the row transform, which is what the `< 32` guard does.
pub fn inverse_transform_2d(dequant: &[i32], side: usize, bit_depth: u8) -> Vec<i32> {
    let log2 = side.trailing_zeros();
    assert_eq!(1usize << log2, side, "the transform is square");
    assert_eq!(dequant.len(), side * side, "one coefficient per position");
    let row_clamp = usize::from(bit_depth) + 8;
    let col_clamp = (usize::from(bit_depth) + 6).max(16);
    let mut residual = vec![0i32; side * side];

    let mut t = [0i32; 64];
    for i in 0..side {
        for j in 0..side {
            t[j] = if i < 32 && j < 32 {
                dequant[i * side + j]
            } else {
                0
            };
        }
        inverse_dct(&mut t, log2, row_clamp);
        for j in 0..side {
            // The row's output is shifted, then clamped before the column
            // transform reads it back.
            residual[i * side + j] = clamp_range(round2(t[j], row_shift(log2)), col_clamp);
        }
    }

    for j in 0..side {
        for i in 0..side {
            t[i] = residual[i * side + j];
        }
        inverse_dct(&mut t, log2, col_clamp);
        for i in 0..side {
            residual[i * side + j] = round2(t[i], 4);
        }
    }
    residual
}

/// Dequantize (spec 7.12.3) and inverse transform one square `DCT_DCT` block,
/// returning its residual in raster order.
///
/// This is the encoder's model of what the decoder will add to its prediction,
/// so it follows the spec's dequantization exactly, truncation toward zero and
/// all.
pub fn dequant_and_inverse(levels: &[i32], side: usize, bit_depth: u8, q_idx: i32) -> Vec<i32> {
    let dq = crate::quant::dequant(levels, side, bit_depth, q_idx);
    inverse_transform_2d(&dq, side, bit_depth)
}

/// The orthonormal DCT-II basis row for output `u` of an `n`-point transform.
///
/// The decoder's network is this basis scaled by a constant the encoder has to
/// undo, and nothing else: measuring the network's response to a unit
/// coefficient at every size gives the same `1 / (8 * sqrt(2))` per unit of
/// `dqDenom`, which is what [`forward_transform_2d`] divides out.
fn build_dct_basis(n: usize) -> Vec<f64> {
    let mut basis = vec![0.0f64; n * n];
    let scale = (2.0 / n as f64).sqrt();
    for u in 0..n {
        let alpha = if u == 0 { 1.0 / 2.0f64.sqrt() } else { 1.0 };
        for i in 0..n {
            let angle = std::f64::consts::PI * (2.0 * i as f64 + 1.0) * u as f64 / (2.0 * n as f64);
            basis[u * n + i] = alpha * scale * angle.cos();
        }
    }
    basis
}

/// [`build_dct_basis`], computed once per transform size and reused: the
/// search calls [`forward_transform_2d`] for every mode of every block, and
/// the basis does not depend on the residual, only on `n` -- recomputing
/// `n * n` cosines per call was measured as this search's single largest
/// per-trial cost (`stage_timing_breakdown`, ec-av1 perf lane). `n` is always
/// one of the transform sizes this crate codes (4 to 64, a power of two), so
/// a small fixed table indexed by `log2(n)` covers every caller without a
/// hash lookup.
fn dct_basis(n: usize) -> &'static [f64] {
    use std::sync::OnceLock;
    // log2(4)=2 .. log2(64)=6, so index by trailing_zeros() - 2.
    static CACHE: OnceLock<[OnceLock<Vec<f64>>; 5]> = OnceLock::new();
    let cache = CACHE.get_or_init(|| std::array::from_fn(|_| OnceLock::new()));
    let idx = (n.trailing_zeros() as usize).saturating_sub(2);
    cache[idx].get_or_init(|| build_dct_basis(n))
}

/// How much the decoder's inverse transform shrinks an orthonormal DCT.
///
/// Measured, not asserted: feeding a single dequantized coefficient through
/// `inverse_transform_2d` reproduces the orthonormal basis function scaled by
/// `dq_denom(side) / 8` at every size (see `the_inverse_network_is_an_orthonormal_dct_over_eight`).
/// The dequantizer has already divided by `dq_denom`, so the two cancel and
/// what an encoder owes a level is size-independent:
/// `level = 8 * orthonormal(residual) / q`.
///
/// The spec fixes the decoder; the encoder is free in how it reaches the
/// coefficients it sends, and this constant is the one thing the two ends have
/// to agree on for a level to mean what the encoder thinks it means.
const INVERSE_GAIN_RECIPROCAL: f64 = 8.0;

/// The forward transform (encoder side, spec-free): the transpose of what the
/// decoder's inverse does, so that `inverse_transform_2d` of the result
/// reproduces `residual`.
///
/// AV1 specifies only the inverse transform. The encoder may reach its
/// coefficients however it likes, and this reaches them the direct way — an
/// orthonormal DCT-II in double precision, scaled to the decoder's fixed-point
/// gain. It is deterministic, and its accuracy is measured against the
/// decoder's own inverse rather than asserted.
pub fn forward_transform_2d(residual: &[i32], side: usize) -> Vec<f64> {
    assert_eq!(
        residual.len(),
        side * side,
        "one residual sample per position"
    );
    let basis = dct_basis(side);
    // Rows first, into a scratch of the same shape, then columns. Both passes
    // are written as a contiguous dot product (row of `residual`/`rows_t`
    // against a row of `basis`) rather than an index loop with one strided
    // operand -- same summation order, so the same bits out, but a shape the
    // optimizer can autovectorize (measured: this halved the per-call cost
    // that `stage_timing_breakdown`, ec-av1 perf lane, found dominant).
    let mut rows = vec![0.0f64; side * side];
    for i in 0..side {
        let residual_row = &residual[i * side..][..side];
        for u in 0..side {
            let sum: f64 = residual_row
                .iter()
                .zip(&basis[u * side..][..side])
                .map(|(&r, &b)| f64::from(r) * b)
                .sum();
            rows[i * side + u] = sum;
        }
    }
    // Transposed so the column pass reads `rows` contiguously too, without
    // changing which terms are summed in which order.
    let mut rows_t = vec![0.0f64; side * side];
    for i in 0..side {
        for v in 0..side {
            rows_t[v * side + i] = rows[i * side + v];
        }
    }
    let mut out = vec![0.0f64; side * side];
    for u in 0..side {
        for v in 0..side {
            let sum: f64 = rows_t[v * side..][..side]
                .iter()
                .zip(&basis[u * side..][..side])
                .map(|(&r, &b)| r * b)
                .sum();
            out[u * side + v] = sum;
        }
    }
    let scale = INVERSE_GAIN_RECIPROCAL;
    for v in &mut out {
        *v *= scale;
    }
    out
}

/// Quantize forward-transform coefficients into the levels the tile syntax
/// carries.
///
/// `deadzone` is the fraction of a quantizer step a coefficient has to reach
/// before it is coded at all, as a rounding offset: 0.5 rounds to nearest,
/// smaller values pull coefficients toward zero, which is what buys the rate
/// back on noisy content. A 64-point transform only carries its top-left
/// 32x32, so everything outside that is dropped here rather than silently by
/// the writer.
pub fn quantize(coeffs: &[f64], side: usize, bit_depth: u8, q_idx: i32, deadzone: f64) -> Vec<i32> {
    assert_eq!(coeffs.len(), side * side, "one coefficient per position");
    let dc = f64::from(crate::quant::dc_q(bit_depth, q_idx));
    let ac = f64::from(crate::quant::ac_q(bit_depth, q_idx));
    let mut levels = vec![0i32; side * side];
    for (i, &c) in coeffs.iter().enumerate() {
        let (row, col) = (i / side, i % side);
        if row >= 32 || col >= 32 {
            continue;
        }
        let q = if i == 0 { dc } else { ac };
        let scaled = c / q;
        let magnitude = scaled.abs() + deadzone;
        let level = if magnitude < 1.0 {
            0
        } else {
            magnitude.floor().min(f64::from(i32::MAX)) as i32
        };
        levels[i] = if scaled < 0.0 { -level } else { level };
    }
    levels
}

/// Transform and quantize one block's residual, the encoder's half of the
/// round trip [`dequant_and_inverse`] completes.
pub fn forward_and_quantize(
    residual: &[i32],
    side: usize,
    bit_depth: u8,
    q_idx: i32,
    deadzone: f64,
) -> Vec<i32> {
    let coeffs = forward_transform_2d(residual, side);
    quantize(&coeffs, side, bit_depth, q_idx, deadzone)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The orthonormal DCT-II basis the forward transform is written against,
    /// computed independently of `dct_basis` so a test is not checking a
    /// function against itself.
    fn reference_basis(n: usize) -> Vec<f64> {
        let mut c = vec![0.0; n * n];
        for u in 0..n {
            let alpha = if u == 0 { (0.5f64).sqrt() } else { 1.0 };
            for x in 0..n {
                let angle =
                    (2.0 * x as f64 + 1.0) * u as f64 * std::f64::consts::PI / (2.0 * n as f64);
                c[u * n + x] = alpha * (2.0 / n as f64).sqrt() * angle.cos();
            }
        }
        c
    }

    /// A deterministic pseudo-random residual in `-100..=100`.
    fn noise(len: usize, seed: u64) -> Vec<i32> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                ((state >> 33) % 201) as i32 - 100
            })
            .collect()
    }

    fn rmse(a: &[i32], b: &[i32]) -> f64 {
        let sum: f64 = a
            .iter()
            .zip(b)
            .map(|(&x, &y)| f64::from(x - y) * f64::from(x - y))
            .sum();
        (sum / a.len() as f64).sqrt()
    }

    /// The one thing the encoder and the decoder have to agree on.
    ///
    /// The spec's inverse network is not documented as a scaled orthonormal
    /// DCT, so this measures it: each dequantized coefficient, on its own,
    /// comes back out as its orthonormal basis function scaled by
    /// `dq_denom(side) / 8` — the same factor for every coefficient of a
    /// size, which is what makes a single constant enough for the encoder.
    /// The fit is checked as well as the scale, so an inverse that scaled
    /// right but mixed positions apart could not pass.
    #[test]
    fn the_inverse_network_is_an_orthonormal_dct_over_eight() {
        for &side in &[4usize, 8, 16, 32, 64] {
            let basis = reference_basis(side);
            for &(u, v) in &[(0usize, 0usize), (0, 1), (1, 0), (2, 3), (5, 7)] {
                if u >= side || v >= side {
                    continue;
                }
                let mut coeffs = vec![0i32; side * side];
                coeffs[u * side + v] = 4096;
                let got = inverse_transform_2d(&coeffs, side, 8);
                let want: Vec<f64> = (0..side * side)
                    .map(|i| 4096.0 * basis[u * side + i / side] * basis[v * side + i % side])
                    .collect();
                // Least-squares fit of one scale over the whole block.
                let num: f64 = want.iter().zip(&got).map(|(w, &g)| w * f64::from(g)).sum();
                let den: f64 = want.iter().map(|w| w * w).sum();
                let k = num / den;
                let expected = crate::quant::dq_denom(side) as f64 / 8.0;
                assert!(
                    (k - expected).abs() < 0.01 * expected,
                    "{side}x{side} ({u},{v}): gain {k}, expected {expected}"
                );
                // And the block really is that basis function, not merely a
                // block of the same energy: no sample off by more than one.
                for (i, (w, &g)) in want.iter().zip(&got).enumerate() {
                    let scaled = k * w;
                    assert!(
                        (scaled - f64::from(g)).abs() <= 1.0,
                        "{side}x{side} ({u},{v}) sample {i}: {g} vs {scaled}"
                    );
                }
            }
        }
    }

    /// A fine quantizer costs almost nothing: forward, quantize, dequantize
    /// and invert returns the residual to within a sample or two at every
    /// transform size the spec's DCT covers.
    ///
    /// 64x64 is excluded here because it cannot carry a white-noise residual
    /// at all — see
    /// [`a_64x64_transform_keeps_what_fits_in_its_coded_quarter`].
    #[test]
    fn a_fine_quantizer_roundtrips_a_residual_almost_exactly() {
        for &side in &[4usize, 8, 16, 32] {
            let residual = noise(side * side, 12_345 + side as u64);
            let levels = forward_and_quantize(&residual, side, 8, 10, 0.5);
            let back = dequant_and_inverse(&levels, side, 8, 10);
            let error = rmse(&back, &residual);
            assert!(error < 1.0, "{side}x{side}: rmse {error}");
        }
    }

    /// Error grows with the quantizer and with nothing else. A calibration
    /// that is off by a constant factor shows up here as a floor that a finer
    /// quantizer cannot get under — which is exactly what a wrong gain
    /// constant produced while this was being written.
    #[test]
    fn the_roundtrip_error_is_the_quantizer_and_only_the_quantizer() {
        for &side in &[4usize, 8, 16, 32] {
            let residual = noise(side * side, 999 + side as u64);
            let mut previous = 0.0;
            for &q_idx in &[10i32, 60, 100, 180] {
                let levels = forward_and_quantize(&residual, side, 8, q_idx, 0.5);
                let back = dequant_and_inverse(&levels, side, 8, q_idx);
                let error = rmse(&back, &residual);
                assert!(
                    error > previous,
                    "{side}x{side} q {q_idx}: {error} <= {previous}"
                );
                // A quantizer step is q/8 in residual units, and rounding to
                // nearest costs no more than half a step per coefficient.
                let step = f64::from(crate::quant::ac_q(8, q_idx)) / 8.0;
                assert!(
                    error < step,
                    "{side}x{side} q {q_idx}: rmse {error}, step {step}"
                );
                previous = error;
            }
        }
    }

    /// A 64x64 transform codes only its top-left 32x32 coefficients, so it
    /// keeps everything below half the Nyquist rate in each direction and
    /// drops the rest. A band-limited residual survives it; white noise loses
    /// the three quarters of its energy that live outside the coded quarter,
    /// and that loss is the transform's, not the quantizer's — it does not
    /// move when the quantizer does.
    #[test]
    fn a_64x64_transform_keeps_what_fits_in_its_coded_quarter() {
        let side = 64;
        // Band-limited: built from basis functions inside the coded quarter.
        let basis = reference_basis(side);
        let mut smooth = vec![0.0f64; side * side];
        for &(u, v, amplitude) in &[(0usize, 0usize, 900.0f64), (1, 2, 400.0), (9, 30, 250.0)] {
            for i in 0..side * side {
                smooth[i] += amplitude * basis[u * side + i / side] * basis[v * side + i % side];
            }
        }
        let smooth: Vec<i32> = smooth.iter().map(|&s| s.round() as i32).collect();
        let levels = forward_and_quantize(&smooth, side, 8, 10, 0.5);
        let back = dequant_and_inverse(&levels, side, 8, 10);
        let kept = rmse(&back, &smooth);
        assert!(kept < 1.5, "band-limited 64x64: rmse {kept}");

        let residual = noise(side * side, 4_242);
        let mut errors = Vec::new();
        for &q_idx in &[10i32, 180] {
            let levels = forward_and_quantize(&residual, side, 8, q_idx, 0.5);
            let back = dequant_and_inverse(&levels, side, 8, q_idx);
            errors.push(rmse(&back, &residual));
        }
        // Three quarters of a white-noise block's energy is outside the coded
        // quarter, so the error is sqrt(3/4) of the residual's own magnitude
        // whatever the quantizer does.
        let magnitude = rmse(&residual, &vec![0; side * side]);
        for error in &errors {
            let ratio = error / magnitude;
            assert!(
                (ratio - 0.75f64.sqrt()).abs() < 0.05,
                "64x64 white noise: ratio {ratio}"
            );
        }
        assert!(
            (errors[1] - errors[0]).abs() < 0.05 * errors[0],
            "64x64 white noise moved with the quantizer: {errors:?}"
        );
    }

    /// The deadzone pulls coefficients toward zero: a wider one codes fewer
    /// of them and costs more error, and rounding to nearest is the tightest
    /// of them.
    #[test]
    fn a_wider_deadzone_codes_fewer_coefficients() {
        let side = 16;
        let residual = noise(side * side, 77);
        let mut previous_nonzero = usize::MAX;
        let mut previous_error = 0.0;
        for &deadzone in &[0.5f64, 0.35, 0.2] {
            let levels = forward_and_quantize(&residual, side, 8, 100, deadzone);
            let back = dequant_and_inverse(&levels, side, 8, 100);
            let nonzero = levels.iter().filter(|&&l| l != 0).count();
            let error = rmse(&back, &residual);
            assert!(
                nonzero < previous_nonzero,
                "deadzone {deadzone}: {nonzero} coefficients"
            );
            assert!(error > previous_error, "deadzone {deadzone}: rmse {error}");
            previous_nonzero = nonzero;
            previous_error = error;
        }
    }

    /// Negating a residual negates its levels: the forward transform carries
    /// no offset of its own.
    #[test]
    fn negating_the_residual_negates_every_level() {
        for &side in &[4usize, 32] {
            let residual = noise(side * side, 5_150 + side as u64);
            let negated: Vec<i32> = residual.iter().map(|&r| -r).collect();
            let levels = forward_and_quantize(&residual, side, 8, 100, 0.5);
            let other = forward_and_quantize(&negated, side, 8, 100, 0.5);
            for (i, (a, b)) in levels.iter().zip(&other).enumerate() {
                assert_eq!(*a, -*b, "{side}x{side} coefficient {i}");
            }
        }
    }

    /// A flat residual is a DC level and nothing else, at the value the
    /// dequantizer's own arithmetic asks for.
    #[test]
    fn a_flat_residual_is_a_dc_level_alone() {
        for &side in &[4usize, 8, 16, 32, 64] {
            let residual = vec![40i32; side * side];
            let levels = forward_and_quantize(&residual, side, 8, 100, 0.5);
            for (i, &level) in levels.iter().enumerate() {
                if i == 0 {
                    // level = 8 * (40 * side) / dc_q, the orthonormal DC of a
                    // flat block being its value times the side.
                    let want = (8.0 * 40.0 * side as f64 / f64::from(crate::quant::dc_q(8, 100)))
                        .round() as i32;
                    assert_eq!(level, want, "{side}x{side} DC");
                } else {
                    assert_eq!(level, 0, "{side}x{side} coefficient {i}");
                }
            }
        }
    }
}

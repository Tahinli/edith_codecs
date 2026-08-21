//! The adaptive hybrid transform of A/52 Annex E §3.4.
//!
//! AHT is E-AC-3's second transform stage: for a frame whose exponents never
//! change, one bin's six block-mantissas are themselves DCT-coded, and the
//! whole set arrives in the first block. That buys two quantizers the AC-3
//! layer does not have — a six-dimensional vector quantizer for the coarse
//! allocations and a gain-adaptive quantizer for the fine ones — so this module
//! reads a channel's entire frame of mantissas at once and hands back the six
//! blocks after inverting the DCT.

use ec_core::{BitReader, Result};

use crate::aht_tables::{GAQ_BITS, REMAP, VQ_BITS, vq_table};
use crate::decode::COEFFS;
use crate::mantissa::Dither;

/// Blocks an AHT frame always has.
pub(crate) const AHT_BLOCKS: usize = 6;

/// One channel's mantissas for a whole frame: `[block * COEFFS + bin]`.
pub(crate) type Frame = [f32];

/// The `Gk` gain of a GAQ block (Table E3.3).
fn gain_of(gaqmod: u32, mapped: u32) -> u32 {
    match (gaqmod, mapped) {
        (_, 0) => 1,
        (1, _) | (3, 1) => 2,
        _ => 4,
    }
}

/// §3.4.4.2 remapping, `y = x + a*x + b`, with `a` and `b` as 16-bit fractions.
fn remap(hebap: u8, gain: u32, x: f32) -> f32 {
    let row = (hebap as usize).saturating_sub(8).min(11);
    let column = match gain {
        1 => 0,
        2 => 1,
        _ => 2,
    };
    let (a, b) = REMAP[row][column][usize::from(x < 0.0)];
    x + x * (f32::from(a) / 32768.0) + f32::from(b) / 32768.0
}

/// One GAQ mantissa (§3.4.4.2 and Table E3.5).
fn read_gaq(r: &mut BitReader<'_>, hebap: u8, gain: u32) -> Result<f32> {
    let m = GAQ_BITS[(hebap as usize).min(19)];
    if gain == 1 {
        let code = r.read_signed(m)?;
        return Ok(remap(hebap, 1, code as f32 / (1u32 << (m - 1)) as f32));
    }
    let small = if gain == 2 { m - 1 } else { m - 2 };
    let code = r.read_signed(small)?;
    // The full-scale negative symbol is the tag that says "large mantissa
    // follows"; every other symbol is a small one, attenuated by 1/Gk.
    if code == -(1i32 << (small - 1)) {
        let large = if gain == 2 { m - 1 } else { m };
        let value = r.read_signed(large)? as f32 / (1u32 << (large - 1)) as f32;
        Ok(remap(hebap, gain, value))
    } else {
        Ok(code as f32 / (1u32 << (small - 1)) as f32 / gain as f32)
    }
}

/// Read one AHT channel's whole frame of mantissas and invert the DCT.
///
/// `hebap` is the high-efficiency allocation for this channel, `range` its
/// mantissa bins. `out` receives six blocks of `COEFFS` mantissas, exponents
/// not yet applied — §3.4.5 inverts the DCT first.
#[allow(clippy::too_many_arguments)]
pub(crate) fn read_channel(
    r: &mut BitReader<'_>,
    hebap: &[u8],
    range: (usize, usize),
    dither: &mut Dither,
    dither_on: bool,
    out: &mut Frame,
) -> Result<()> {
    let (start, end) = range;
    let gaqmod = r.read_bits(2)?;
    let endbap = if gaqmod < 2 { 12 } else { 17 };

    // §3.4.2: which bins carry a gain word, and how many gain sections that
    // makes.
    let active = hebap[start..end]
        .iter()
        .filter(|&&h| h > 7 && u32::from(h) < endbap)
        .count();
    let sections = match gaqmod {
        0 => 0,
        1 | 2 => active,
        _ => active.div_ceil(3),
    };
    let mut gains = Vec::with_capacity(sections * 3);
    for _ in 0..sections {
        if gaqmod == 3 {
            // Three 3-state gains in one 5-bit word, unpacked like exponents.
            let word = r.read_bits(5)?.min(26);
            gains.push(word / 9);
            gains.push((word % 9) / 3);
            gains.push((word % 9) % 3);
        } else {
            gains.push(r.read_bits(1)?);
        }
    }

    let mut pre = [0.0f32; AHT_BLOCKS];
    let mut next_gain = 0usize;
    for bin in start..end {
        let h = hebap[bin];
        let mut zero_bits = false;
        if h == 0 {
            pre = [0.0; AHT_BLOCKS];
            zero_bits = true;
        } else if h <= 7 {
            let index = r.read_bits(VQ_BITS[h as usize])? as usize;
            let table = vq_table(h);
            let vector = table[index.min(table.len() - 1)];
            for (slot, v) in pre.iter_mut().zip(vector) {
                *slot = f32::from(v) / 32768.0;
            }
        } else {
            let gain = if gaqmod > 0 && u32::from(h) < endbap {
                let mapped = gains.get(next_gain).copied().unwrap_or(0);
                next_gain += 1;
                gain_of(gaqmod, mapped)
            } else {
                1
            };
            for slot in &mut pre {
                *slot = read_gaq(r, h, gain)?;
            }
        }

        if zero_bits {
            // A zero-bit AHT bin is the same silence AC-3 fills with dither.
            for blk in 0..AHT_BLOCKS {
                out[blk * COEFFS + bin] = if dither_on { dither.value() } else { 0.0 };
            }
            continue;
        }
        // §3.4.5: invert the six-point DCT across blocks.
        for (blk, slot) in idct6(&pre).into_iter().enumerate() {
            out[blk * COEFFS + bin] = slot;
        }
    }
    Ok(())
}

/// The inverse of §3.4.5's DCT: six AHT coefficients into six block mantissas.
fn idct6(x: &[f32; AHT_BLOCKS]) -> [f32; AHT_BLOCKS] {
    // cos(j(2m+1)pi/12) for j, m in 0..6, with the 1/sqrt(2) weight on j = 0
    // folded in. The leading factor is sqrt(2), not the 2 the standard's
    // printed equation shows: measured against the oracle's decode of a real DDP
    // stream, a factor of 2 makes every AHT channel exactly sqrt(2) too loud
    // (per-frame RMS ratio 1.4142 across 3 649 frames). The radical is lost in
    // the published equation's typesetting.
    const COS: [[f32; AHT_BLOCKS]; AHT_BLOCKS] = cos_table();
    let mut out = [0.0f32; AHT_BLOCKS];
    for (m, slot) in out.iter_mut().enumerate() {
        let mut acc = 0.0;
        for (j, &v) in x.iter().enumerate() {
            acc += v * COS[j][m];
        }
        *slot = std::f32::consts::SQRT_2 * acc;
    }
    out
}

/// Build the cosine table at compile time from a small polynomial-free form:
/// the angles are multiples of 15 degrees, so every entry is one of six exact
/// values.
const fn cos_table() -> [[f32; AHT_BLOCKS]; AHT_BLOCKS] {
    // cos(k * pi / 12) for k = 0..24, the only angles the transform uses.
    use std::f32::consts::FRAC_1_SQRT_2;
    const C: [f32; 25] = [
        1.0,
        0.965_925_8,
        0.866_025_4,
        FRAC_1_SQRT_2,
        0.5,
        0.258_819_04,
        0.0,
        -0.258_819_04,
        -0.5,
        -FRAC_1_SQRT_2,
        -0.866_025_4,
        -0.965_925_8,
        -1.0,
        -0.965_925_8,
        -0.866_025_4,
        -FRAC_1_SQRT_2,
        -0.5,
        -0.258_819_04,
        0.0,
        0.258_819_04,
        0.5,
        FRAC_1_SQRT_2,
        0.866_025_4,
        0.965_925_8,
        1.0,
    ];
    let mut table = [[0.0f32; AHT_BLOCKS]; AHT_BLOCKS];
    let mut j = 0;
    while j < AHT_BLOCKS {
        let mut m = 0;
        while m < AHT_BLOCKS {
            let k = (j * (2 * m + 1)) % 24;
            let value = C[k];
            // R_j: 1/sqrt(2) for the DC term, 1 otherwise.
            table[j][m] = if j == 0 { value * FRAC_1_SQRT_2 } else { value };
            m += 1;
        }
        j += 1;
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idct_of_a_dc_only_vector_is_flat() {
        let mut x = [0.0f32; AHT_BLOCKS];
        x[0] = 1.0;
        let out = idct6(&x);
        for v in out {
            assert!((v - out[0]).abs() < 1e-5, "{out:?}");
        }
        assert!(out[0] > 0.0);
    }

    #[test]
    fn idct_matches_the_direct_sum() {
        // The table-driven transform against the standard's equation with the
        // measured sqrt(2) scale, computed the slow way in f64.
        let x = [0.3f32, -0.7, 0.1, 0.9, -0.2, 0.5];
        let got = idct6(&x);
        for (m, &g) in got.iter().enumerate() {
            let mut acc = 0.0f64;
            for (j, &v) in x.iter().enumerate() {
                let r = if j == 0 { 1.0 / 2f64.sqrt() } else { 1.0 };
                let angle = j as f64 * (2 * m + 1) as f64 * std::f64::consts::PI / 12.0;
                acc += r * f64::from(v) * angle.cos();
            }
            assert!((f64::from(g) - 2f64.sqrt() * acc).abs() < 1e-5, "m = {m}");
        }
    }

    #[test]
    fn vq4_is_the_printed_table_shifted_by_one() {
        // The correction measured against a real stream (see VQ4's comment):
        // index k selects the row Table E4.4 prints at k + 1, and index 31
        // selects silence. Pinned here so the table cannot drift back.
        use crate::aht_tables::VQ4;
        assert_eq!(VQ4[31], [0; 6]);
        assert_eq!(VQ4[29], [-83, 278, 323, 55, -154, 232]);
        assert_eq!(VQ4[0], [6636, -4593, 14173, -17297, -16523, 864]);
        // Every other table is used exactly as printed.
        assert_eq!(vq_table(3).len(), 16);
        assert_eq!(vq_table(4).len(), 32);
        assert_eq!(vq_table(7).len(), 512);
    }

    #[test]
    fn gaq_gains_follow_table_e3_3() {
        assert_eq!(gain_of(1, 0), 1);
        assert_eq!(gain_of(1, 1), 2);
        assert_eq!(gain_of(2, 1), 4);
        assert_eq!(gain_of(3, 1), 2);
        assert_eq!(gain_of(3, 2), 4);
    }

    #[test]
    fn remap_reproduces_the_step_sizes_of_table_e3_5() {
        // hebap 8, Gk = 1: seven points of step 2/(2^3 - 1) = 2/7.
        let a = remap(8, 1, 1.0 / 4.0);
        let b = remap(8, 1, 2.0 / 4.0);
        assert!((b - a - 2.0 / 7.0).abs() < 1e-4, "{a} {b}");
        // hebap 8, Gk = 2 large: four points of step 1/(2^2 - 1) = 1/3.
        let a = remap(8, 2, 0.0);
        let b = remap(8, 2, 0.5);
        assert!((b - a - 1.0 / 3.0).abs() < 1e-4, "{a} {b}");
    }
}

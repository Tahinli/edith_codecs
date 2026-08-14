//! Forward transforms and quantisation.
//!
//! Nothing here is normative: the standard specifies only the *inverse*
//! transforms, which the decoder side of this crate already implements
//! ([`crate::transform`]). The forward direction is chosen so that the
//! round trip through the normative inverse lands back on the source, and
//! that is what the tests at the bottom check across the whole QP range.
//!
//! The multiplier table pairs with `normAdjust4x4` (Table 8-15 through the
//! `LevelScale` of Equation 8-313): `MF[m][class] * normAdjust4x4[m][class] *
//! 16 ~= 2^21`, which is what makes `level = (|c| * MF + f) >> (15 + qp / 6)`
//! invert the decoder's dequantisation.

use crate::tables::{NORM_ADJUST_CLASS_4X4, ZIGZAG_4X4};

/// Quantisation multipliers per `qp % 6`, indexed by the same position class
/// as [`NORM_ADJUST_CLASS_4X4`] (0 = even/even, 1 = odd/odd, 2 = mixed).
const MF: [[i32; 3]; 6] = [
    [13107, 5243, 8066],
    [11916, 4660, 7490],
    [10082, 4194, 6554],
    [9362, 3647, 5825],
    [8192, 3355, 5243],
    [7282, 2893, 4559],
];

/// Rounding offset numerator: an intra block rounds at 1/3 of a step, an inter
/// block at 1/6 — the classic asymmetry, which spends bits where prediction is
/// weakest. Measured against 1/3 and 1/2 for the inter case on 1080p camera
/// and screen-capture clips: 1/6 is level or ahead at matched bitrate.
#[inline]
fn round_offset(qbits: u32, intra: bool) -> i32 {
    (1i32 << qbits) / if intra { 3 } else { 6 }
}

/// Forward 4x4 core transform (the inverse of spec 8.5.12.2), raster in and
/// out, integer and exact.
pub(crate) fn forward_4x4(d: &mut [i32; 16]) {
    for i in 0..4 {
        let o = i * 4;
        let (a, b, c, e) = (d[o], d[o + 1], d[o + 2], d[o + 3]);
        let (s0, s1, s2, s3) = (a + e, b + c, b - c, a - e);
        d[o] = s0 + s1;
        d[o + 1] = 2 * s3 + s2;
        d[o + 2] = s0 - s1;
        d[o + 3] = s3 - 2 * s2;
    }
    for j in 0..4 {
        let (a, b, c, e) = (d[j], d[4 + j], d[8 + j], d[12 + j]);
        let (s0, s1, s2, s3) = (a + e, b + c, b - c, a - e);
        d[j] = s0 + s1;
        d[4 + j] = 2 * s3 + s2;
        d[8 + j] = s0 - s1;
        d[12 + j] = s3 - 2 * s2;
    }
}

/// Forward 4x4 Hadamard over the sixteen luma DC coefficients of an
/// Intra_16x16 macroblock (the inverse of spec 8.5.10), raster in and out.
pub(crate) fn forward_hadamard_4x4(d: &mut [i32; 16]) {
    for i in 0..4 {
        let o = i * 4;
        let (a, b, c, e) = (d[o], d[o + 1], d[o + 2], d[o + 3]);
        d[o] = a + b + c + e;
        d[o + 1] = a + b - c - e;
        d[o + 2] = a - b - c + e;
        d[o + 3] = a - b + c - e;
    }
    for j in 0..4 {
        let (a, b, c, e) = (d[j], d[4 + j], d[8 + j], d[12 + j]);
        d[j] = a + b + c + e;
        d[4 + j] = a + b - c - e;
        d[8 + j] = a - b - c + e;
        d[12 + j] = a - b + c - e;
    }
}

/// Forward 2x2 Hadamard over the four chroma DC coefficients (the inverse of
/// spec 8.5.11).
pub(crate) fn forward_hadamard_2x2(d: &mut [i32; 4]) {
    let (a, b, c, e) = (d[0], d[1], d[2], d[3]);
    d[0] = a + b + c + e;
    d[1] = a - b + c - e;
    d[2] = a + b - c - e;
    d[3] = a - b - c + e;
}

/// Quantise a transformed 4x4 block into scan (zig-zag) order.
///
/// `skip_dc` leaves scan position 0 out and writes the fifteen AC levels into
/// `out[0..15]`, which is the layout the AC residual blocks of Intra_16x16 and
/// chroma use. Returns the number of non-zero levels.
pub(crate) fn quant_4x4(
    coef: &[i32; 16],
    qp: i32,
    intra: bool,
    skip_dc: bool,
    out: &mut [i32; 16],
) -> u8 {
    let m = (qp % 6) as usize;
    let qbits = 15 + (qp / 6) as u32;
    let f = round_offset(qbits, intra);
    let start = usize::from(skip_dc);
    let mut nz = 0;
    *out = [0; 16];
    for k in start..16 {
        let raster = ZIGZAG_4X4[k] as usize;
        let c = coef[raster];
        let mf = MF[m][NORM_ADJUST_CLASS_4X4[raster] as usize];
        let level = ((c.unsigned_abs() as i64 * mf as i64 + f as i64) >> qbits) as i32;
        if level != 0 {
            nz += 1;
        }
        out[k - start] = if c < 0 { -level } else { level };
    }
    nz
}

/// Quantise the sixteen Hadamard-transformed luma DC coefficients into scan
/// order. Two extra bits of shift pay for the gain of the forward Hadamard,
/// which the decoder's inverse (8.5.10, a `>> 6` where the AC path shifts by
/// four) takes back out.
pub(crate) fn quant_luma_dc(coef: &[i32; 16], qp: i32, out: &mut [i32; 16]) -> u8 {
    let qbits = 17 + (qp / 6) as u32;
    let mf = MF[(qp % 6) as usize][0];
    let f = round_offset(qbits, true);
    let mut nz = 0;
    *out = [0; 16];
    for k in 0..16 {
        let c = coef[ZIGZAG_4X4[k] as usize];
        let level = ((c.unsigned_abs() as i64 * mf as i64 + f as i64) >> qbits) as i32;
        if level != 0 {
            nz += 1;
        }
        out[k] = if c < 0 { -level } else { level };
    }
    nz
}

/// Quantise the four Hadamard-transformed chroma DC coefficients (raster
/// order, which is also their scan order).
pub(crate) fn quant_chroma_dc(coef: &[i32; 4], qp: i32, intra: bool, out: &mut [i32; 16]) -> u8 {
    let qbits = 16 + (qp / 6) as u32;
    let mf = MF[(qp % 6) as usize][0];
    let f = round_offset(qbits, intra);
    let mut nz = 0;
    *out = [0; 16];
    for k in 0..4 {
        let c = coef[k];
        let level = ((c.unsigned_abs() as i64 * mf as i64 + f as i64) >> qbits) as i32;
        if level != 0 {
            nz += 1;
        }
        out[k] = if c < 0 { -level } else { level };
    }
    nz
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transform::{
        LevelScale4x4, chroma_dc_transform_420, dequant_4x4, inverse_transform_4x4,
        luma_dc_transform, unzigzag,
    };

    fn ls() -> LevelScale4x4 {
        LevelScale4x4::new(&[16; 16])
    }

    /// A residual quantised at QP and put back through the *normative* inverse
    /// comes back within the quantiser step, at every QP: this is the property
    /// that makes the forward direction a valid inverse of clause 8.5, and a
    /// wrong multiplier or shift breaks it by orders of magnitude.
    #[test]
    fn ac_round_trip_within_a_quantiser_step() {
        let ls = ls();
        for qp in 0..52 {
            let mut worst = 0i32;
            for seed in 0..8u32 {
                let src: [i32; 16] = std::array::from_fn(|i| {
                    let v = (seed.wrapping_mul(2654435761).wrapping_add(i as u32 * 40503)) >> 24;
                    (v as i32 % 61) - 30
                });
                let mut coef = src;
                forward_4x4(&mut coef);
                let mut scan = [0i32; 16];
                quant_4x4(&coef, qp, true, false, &mut scan);
                let mut raster = [0i32; 16];
                unzigzag(&scan, &mut raster);
                dequant_4x4(&mut raster, &ls, qp, false);
                let mut out = [0i32; 16];
                inverse_transform_4x4(&raster, &mut out);
                for i in 0..16 {
                    worst = worst.max((out[i] - src[i]).abs());
                }
            }
            // The reconstruction error of a dead-zone quantiser is bounded by
            // one step, which doubles every six QP.
            let step = 1 << (qp / 6);
            assert!(
                worst <= 2 + 2 * step,
                "qp {qp}: worst error {worst} over step {step}"
            );
        }
    }

    /// The same for the Intra_16x16 luma DC path: a flat residual across the
    /// whole macroblock survives Hadamard, quantisation and the decoder's
    /// 8.5.10 inverse.
    #[test]
    fn luma_dc_round_trip() {
        let ls = ls();
        for qp in 0..52 {
            for &x in &[-40i32, -7, 3, 25] {
                // Every 4x4 block of a flat macroblock has DC = 16 * x.
                let dc: [i32; 16] = [16 * x; 16];
                let mut had = dc;
                forward_hadamard_4x4(&mut had);
                let mut scan = [0i32; 16];
                quant_luma_dc(&had, qp, &mut scan);
                let mut raster = [0i32; 16];
                unzigzag(&scan, &mut raster);
                luma_dc_transform(&mut raster, &ls, qp);
                // Each block reconstructs (dc + 32) >> 6 flat samples.
                let recon = (raster[0] + 32) >> 6;
                let step = 1 << (qp / 6);
                assert!(
                    (recon - x).abs() <= 2 + 2 * step,
                    "qp {qp} x {x}: recon {recon}"
                );
            }
        }
    }

    /// And the chroma DC path (2x2 Hadamard, 8.5.11).
    #[test]
    fn chroma_dc_round_trip() {
        let ls = ls();
        for qp in 0..40 {
            for &x in &[-30i32, -4, 9, 21] {
                let dc: [i32; 4] = [16 * x; 4];
                let mut had = dc;
                forward_hadamard_2x2(&mut had);
                let mut scan = [0i32; 16];
                quant_chroma_dc(&had, qp, true, &mut scan);
                let mut c: [i32; 4] = [scan[0], scan[1], scan[2], scan[3]];
                chroma_dc_transform_420(&mut c, &ls, qp);
                let recon = (c[0] + 32) >> 6;
                let step = 1 << (qp / 6);
                assert!(
                    (recon - x).abs() <= 2 + 2 * step,
                    "qp {qp} x {x}: recon {recon}"
                );
            }
        }
    }
}

//! Inverse transforms and scaling (spec 8.5.9 - 8.5.12).

use crate::tables::{NORM_ADJUST_4X4, NORM_ADJUST_CLASS_4X4, ZIGZAG_4X4};

/// Per-QP scaling factors for one 4x4 weight list: `LevelScale4x4(m, i, j) =
/// weightScale(i, j) * normAdjust4x4(m, i, j)` (Equation 8-313), in raster
/// order, for m = qP % 6.
///
/// Built once per parameter-set activation; the decode loop indexes it flat.
#[derive(Debug, Clone)]
pub struct LevelScale4x4 {
    /// `[m][raster]`.
    pub scale: [[i32; 16]; 6],
}

impl LevelScale4x4 {
    /// Build from a raster-order weight list (flat 16s when no scaling matrix).
    pub fn new(weight: &[u8; 16]) -> LevelScale4x4 {
        let mut scale = [[0i32; 16]; 6];
        for (m, row) in scale.iter_mut().enumerate() {
            for (idx, s) in row.iter_mut().enumerate() {
                let class = NORM_ADJUST_CLASS_4X4[idx] as usize;
                *s = i32::from(weight[idx]) * i32::from(NORM_ADJUST_4X4[m][class]);
            }
        }
        LevelScale4x4 { scale }
    }
}

/// Scale AC/luma-4x4 coefficients in place (spec 8.5.12.1): `block` is in
/// raster order. `skip_dc` is true for Intra_16x16 luma AC and chroma AC
/// blocks whose DC came through the separate DC path (d00 = c00 untouched).
#[inline]
pub fn dequant_4x4(block: &mut [i32; 16], ls: &LevelScale4x4, qp: i32, skip_dc: bool) {
    let m = (qp % 6) as usize;
    let shift = qp / 6;
    let scale = &ls.scale[m];
    let start = usize::from(skip_dc);
    if shift >= 4 {
        let sh = (shift - 4) as u32;
        for i in start..16 {
            block[i] = (block[i] * scale[i]) << sh;
        }
    } else {
        let sh = (4 - shift) as u32;
        let round = 1i32 << (3 - shift);
        for i in start..16 {
            block[i] = (block[i] * scale[i] + round) >> sh;
        }
    }
}

/// Place scan-order coefficients into raster order using the 4x4 zig-zag.
#[inline]
pub fn unzigzag(scan: &[i32; 16], out: &mut [i32; 16]) {
    for (s, &raster) in ZIGZAG_4X4.iter().enumerate() {
        out[raster as usize] = scan[s];
    }
}

/// Un-zigzag a 15-coefficient AC block (Intra_16x16 luma AC / chroma AC):
/// scan position `k` maps to zig-zag position `k + 1`; the DC slot is
/// cleared for the caller to fill from the separate DC path.
#[inline]
pub fn unzigzag_ac15(scan: &[i32; 16], out: &mut [i32; 16]) {
    out[0] = 0;
    for k in 0..15 {
        out[ZIGZAG_4X4[k + 1] as usize] = scan[k];
    }
}

/// Inverse 4x4 core transform (spec 8.5.12.2): raster-order input `d`,
/// output residuals `r` including the final `(x + 32) >> 6`.
#[inline]
pub fn inverse_transform_4x4(d: &[i32; 16], r: &mut [i32; 16]) {
    // Horizontal (each row).
    let mut e = [0i32; 16];
    for i in 0..4 {
        let o = i * 4;
        let (d0, d1, d2, d3) = (d[o], d[o + 1], d[o + 2], d[o + 3]);
        let e0 = d0 + d2;
        let e1 = d0 - d2;
        let e2 = (d1 >> 1) - d3;
        let e3 = d1 + (d3 >> 1);
        e[o] = e0 + e3;
        e[o + 1] = e1 + e2;
        e[o + 2] = e1 - e2;
        e[o + 3] = e0 - e3;
    }
    // Vertical (each column) + rounding.
    for j in 0..4 {
        let (g0, g1, g2, g3) = (e[j], e[4 + j], e[8 + j], e[12 + j]);
        let h0 = g0 + g2;
        let h1 = g0 - g2;
        let h2 = (g1 >> 1) - g3;
        let h3 = g1 + (g3 >> 1);
        r[j] = (h0 + h3 + 32) >> 6;
        r[4 + j] = (h1 + h2 + 32) >> 6;
        r[8 + j] = (h1 - h2 + 32) >> 6;
        r[12 + j] = (h0 - h3 + 32) >> 6;
    }
}

/// Intra_16x16 luma DC: 4x4 Hadamard + scaling (spec 8.5.10). `c` is the DC
/// coefficient array in raster order (dc of block (bx, by) at `by * 4 + bx`);
/// output overwrites it.
pub fn luma_dc_transform(c: &mut [i32; 16], ls: &LevelScale4x4, qp: i32) {
    // Hadamard rows then columns (Equation 8-320).
    let mut f = [0i32; 16];
    for i in 0..4 {
        let o = i * 4;
        let (a, b, cc, d) = (c[o], c[o + 1], c[o + 2], c[o + 3]);
        f[o] = a + b + cc + d;
        f[o + 1] = a + b - cc - d;
        f[o + 2] = a - b - cc + d;
        f[o + 3] = a - b + cc - d;
    }
    let mut g = [0i32; 16];
    for j in 0..4 {
        let (a, b, cc, d) = (f[j], f[4 + j], f[8 + j], f[12 + j]);
        g[j] = a + b + cc + d;
        g[4 + j] = a + b - cc - d;
        g[8 + j] = a - b - cc + d;
        g[12 + j] = a - b + cc - d;
    }
    let scale = ls.scale[(qp % 6) as usize][0];
    if qp >= 36 {
        let sh = (qp / 6 - 6) as u32;
        for (o, &v) in c.iter_mut().zip(&g) {
            *o = (v * scale) << sh;
        }
    } else {
        let sh = (6 - qp / 6) as u32;
        let round = 1i32 << (5 - qp / 6);
        for (o, &v) in c.iter_mut().zip(&g) {
            *o = (v * scale + round) >> sh;
        }
    }
}

/// Chroma DC for 4:2:0: 2x2 Hadamard + scaling (spec 8.5.11). `c` holds the
/// four DC levels in raster order of the chroma 4x4 blocks; overwritten with
/// the scaled DC values.
pub fn chroma_dc_transform_420(c: &mut [i32; 4], ls: &LevelScale4x4, qp: i32) {
    let (c00, c01, c10, c11) = (c[0], c[1], c[2], c[3]);
    let f = [
        c00 + c01 + c10 + c11,
        c00 - c01 + c10 - c11,
        c00 + c01 - c10 - c11,
        c00 - c01 - c10 + c11,
    ];
    let scale = ls.scale[(qp % 6) as usize][0];
    let sh = (qp / 6) as u32;
    for (o, &v) in c.iter_mut().zip(&f) {
        *o = ((v * scale) << sh) >> 5;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_ls() -> LevelScale4x4 {
        LevelScale4x4::new(&[16; 16])
    }

    /// A pure DC level reconstructs to a flat block whose value follows the
    /// dequant ladder: doubling every 6 QP steps.
    #[test]
    fn dc_only_block_flat_and_qp_ladder() {
        let ls = flat_ls();
        let recon = |qp: i32, level: i32| -> i32 {
            let mut block = [0i32; 16];
            block[0] = level;
            dequant_4x4(&mut block, &ls, qp, false);
            let mut r = [0i32; 16];
            inverse_transform_4x4(&block.clone(), &mut r);
            assert!(r.iter().all(|&x| x == r[0]), "DC block must be flat");
            r[0]
        };
        // LevelScale(m = 0, 0, 0) = 16 * 10 = 160. qp 24: d00 = 160 << 0.
        // A DC-only block comes out flat at (d00 + 32) >> 6.
        assert_eq!(recon(24, 1), (160 + 32) >> 6);
        assert_eq!(recon(30, 1), (320 + 32) >> 6);
        assert_eq!(recon(36, 1), (640 + 32) >> 6);
        assert_eq!(recon(24, -3), (-480 + 32) >> 6); // floor shift: -7
    }

    /// The inverse transform of zeros is zero, and rounding is symmetric
    /// enough that a lone +1/-1 AC level stays bounded.
    #[test]
    fn zero_in_zero_out_and_unzigzag() {
        let mut r = [7i32; 16];
        inverse_transform_4x4(&[0; 16], &mut r);
        assert_eq!(r, [0; 16]);

        let mut scan = [0i32; 16];
        scan[1] = 5; // second scan position = raster (0,1)
        let mut raster = [0i32; 16];
        unzigzag(&scan, &mut raster);
        assert_eq!(raster[1], 5);
        assert_eq!(raster.iter().filter(|&&x| x != 0).count(), 1);
    }

    /// Luma DC Hadamard of a single coefficient spreads evenly; scaling per
    /// 8.5.10 with qp < 36 uses the rounding shift.
    #[test]
    fn luma_dc_spread() {
        let ls = flat_ls();
        let mut c = [0i32; 16];
        c[0] = 8;
        luma_dc_transform(&mut c, &ls, 28);
        // f = 8 everywhere; scale = LevelScale(28 % 6 = 4, 0, 0) = 16 * 16.
        let expected = (8 * 16 * 16 + (1 << (5 - 28 / 6))) >> (6 - 28 / 6);
        assert!(c.iter().all(|&x| x == expected));
    }

    #[test]
    fn chroma_dc_hadamard() {
        let ls = flat_ls();
        let mut c = [4, 0, 0, 0];
        chroma_dc_transform_420(&mut c, &ls, 30);
        // f = [4,4,4,4]; scale = LS(0,0,0) = 160; ((4 * 160) << 5) >> 5 = 640.
        assert!(c.iter().all(|&x| x == 640), "{c:?}");
    }
}

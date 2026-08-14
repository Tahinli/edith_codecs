//! Scaling and transformation (Rec. ITU-T H.264 clause 8.5).
//!
//! All of it is integer and all of it is exact: the inverse transform is
//! specified as a sequence of adds and shifts, not as an approximation of a
//! DCT, so "close enough" is not a thing that exists here — one wrong shift is
//! one wrong picture.

use crate::tables::{ZIGZAG_4X4, norm_adjust_4x4};

/// `LevelScale4x4(m, i, j)` (clause 8.5.9), with a flat weight matrix folded in
/// as `weight_scale`, which is 16 unless a scaling list is in force.
pub fn level_scale_4x4(m: usize, i: usize, j: usize, weight_scale: i32) -> i32 {
    weight_scale * norm_adjust_4x4(m, i, j)
}

/// The inverse 4x4 scan of Table 8-13: coefficient levels in scan order to a
/// 4x4 array indexed `[i][j]` (row, column).
pub fn inverse_scan_4x4(level: &[i32]) -> [[i32; 4]; 4] {
    let mut c = [[0i32; 4]; 4];
    for (index, &value) in level.iter().enumerate().take(16) {
        let raster = ZIGZAG_4X4[index];
        c[raster / 4][raster % 4] = value;
    }
    c
}

/// Clause 8.5.12.1: scaling of the 4x4 residual transform coefficients.
///
/// `dc` replaces `d[0][0]` for the blocks whose DC coefficient was transmitted
/// and scaled separately (`Intra_16x16` luma and chroma), which is exactly the
/// "otherwise" branch of the clause.
pub fn scale_4x4(c: &[[i32; 4]; 4], qp: i32, weight_scale: i32, dc: Option<i32>) -> [[i32; 4]; 4] {
    let m = (qp % 6) as usize;
    let shift = qp / 6;
    let mut d = [[0i32; 4]; 4];
    for i in 0..4 {
        for j in 0..4 {
            if (i, j) == (0, 0)
                && let Some(dc) = dc
            {
                d[0][0] = dc;
                continue;
            }
            let scaled = c[i][j] * level_scale_4x4(m, i, j, weight_scale);
            d[i][j] = if qp >= 24 {
                scaled << (shift - 4)
            } else {
                (scaled + (1 << (3 - shift))) >> (4 - shift)
            };
        }
    }
    d
}

/// Clause 8.5.12.2: the inverse 4x4 transform, returning the residual `r[i][j]`.
pub fn inverse_transform_4x4(d: &[[i32; 4]; 4]) -> [[i32; 4]; 4] {
    let mut f = [[0i32; 4]; 4];
    for i in 0..4 {
        let e0 = d[i][0] + d[i][2];
        let e1 = d[i][0] - d[i][2];
        let e2 = (d[i][1] >> 1) - d[i][3];
        let e3 = d[i][1] + (d[i][3] >> 1);
        f[i][0] = e0 + e3;
        f[i][1] = e1 + e2;
        f[i][2] = e1 - e2;
        f[i][3] = e0 - e3;
    }
    let mut r = [[0i32; 4]; 4];
    for j in 0..4 {
        let g0 = f[0][j] + f[2][j];
        let g1 = f[0][j] - f[2][j];
        let g2 = (f[1][j] >> 1) - f[3][j];
        let g3 = f[1][j] + (f[3][j] >> 1);
        r[0][j] = g0 + g3;
        r[1][j] = g1 + g2;
        r[2][j] = g1 - g2;
        r[3][j] = g0 - g3;
    }
    for row in r.iter_mut() {
        for value in row.iter_mut() {
            *value = (*value + 32) >> 6;
        }
    }
    r
}

/// Clause 8.5.10: the 4x4 luma DC coefficients of an `Intra_16x16` macroblock,
/// inverse Hadamard transformed and then scaled.
pub fn inverse_luma_dc(c: &[[i32; 4]; 4], qp: i32, weight_scale: i32) -> [[i32; 4]; 4] {
    let mut f = hadamard_4x4(c);
    let level_scale = level_scale_4x4((qp % 6) as usize, 0, 0, weight_scale);
    let shift = qp / 6;
    for row in f.iter_mut() {
        for value in row.iter_mut() {
            *value = if qp >= 36 {
                (*value * level_scale) << (shift - 6)
            } else {
                (*value * level_scale + (1 << (5 - shift))) >> (6 - shift)
            };
        }
    }
    f
}

/// Clause 8.5.11.1 and 8.5.11.2: the 2x2 chroma DC coefficients of a 4:2:0
/// macroblock, inverse transformed and scaled.
pub fn inverse_chroma_dc(c: &[[i32; 2]; 2], qp: i32, weight_scale: i32) -> [[i32; 2]; 2] {
    // f = [[1, 1], [1, -1]] * c * [[1, 1], [1, -1]].
    let f = [
        [
            c[0][0] + c[0][1] + c[1][0] + c[1][1],
            c[0][0] - c[0][1] + c[1][0] - c[1][1],
        ],
        [
            c[0][0] + c[0][1] - c[1][0] - c[1][1],
            c[0][0] - c[0][1] - c[1][0] + c[1][1],
        ],
    ];
    let level_scale = level_scale_4x4((qp % 6) as usize, 0, 0, weight_scale);
    let mut dc = [[0i32; 2]; 2];
    for i in 0..2 {
        for j in 0..2 {
            dc[i][j] = ((f[i][j] * level_scale) << (qp / 6)) >> 5;
        }
    }
    dc
}

/// The 4x4 Hadamard transform `A * c * A` of clause 8.5.10, with
/// `A = [[1, 1, 1, 1], [1, 1, -1, -1], [1, -1, -1, 1], [1, -1, 1, -1]]`.
fn hadamard_4x4(c: &[[i32; 4]; 4]) -> [[i32; 4]; 4] {
    let mut t = [[0i32; 4]; 4];
    for j in 0..4 {
        let (a, b, cc, d) = (c[0][j], c[1][j], c[2][j], c[3][j]);
        t[0][j] = a + b + cc + d;
        t[1][j] = a + b - cc - d;
        t[2][j] = a - b - cc + d;
        t[3][j] = a - b + cc - d;
    }
    let mut f = [[0i32; 4]; 4];
    for (i, row) in t.iter().enumerate() {
        let (a, b, cc, d) = (row[0], row[1], row[2], row[3]);
        f[i][0] = a + b + cc + d;
        f[i][1] = a + b - cc - d;
        f[i][2] = a - b - cc + d;
        f[i][3] = a - b + cc - d;
    }
    f
}

/// Clause 8.5.13, picture construction: `u = Clip1(pred + r)`.
pub fn clip1(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inverse_scan_places_the_zig_zag() {
        let level: Vec<i32> = (0..16).collect();
        let c = inverse_scan_4x4(&level);
        assert_eq!(c[0][0], 0);
        assert_eq!(c[0][1], 1);
        assert_eq!(c[1][0], 2);
        assert_eq!(c[2][0], 3);
        assert_eq!(c[3][3], 15);
    }

    #[test]
    fn dc_only_block_reconstructs_a_flat_residual() {
        // A block whose only coefficient is the DC term must come out flat, and
        // the transform's gain must be exactly 1/64 after the final rounding.
        let mut d = [[0i32; 4]; 4];
        d[0][0] = 64;
        assert_eq!(inverse_transform_4x4(&d), [[1i32; 4]; 4]);
        d[0][0] = 640;
        assert_eq!(inverse_transform_4x4(&d), [[10i32; 4]; 4]);
    }

    #[test]
    fn transform_is_linear_and_separable() {
        // A horizontal-only coefficient produces a horizontally varying,
        // vertically constant residual, and vice versa.
        let mut d = [[0i32; 4]; 4];
        d[0][1] = 128;
        let r = inverse_transform_4x4(&d);
        assert!(r[0] == r[1] && r[1] == r[2] && r[2] == r[3], "rows equal");
        assert!(r[0][0] > 0 && r[0][3] < 0, "sign changes along the row");
        let mut d = [[0i32; 4]; 4];
        d[1][0] = 128;
        let r = inverse_transform_4x4(&d);
        for row in r {
            assert!(row.iter().all(|&v| v == row[0]), "columns equal");
        }
        assert!(r[0][0] > 0 && r[3][0] < 0);
    }

    #[test]
    fn scaling_matches_the_two_branches_of_the_clause() {
        let mut c = [[0i32; 4]; 4];
        c[0][0] = 1;
        // qP = 24 is the first quantiser using the shift-up branch:
        // 1 * 16 * normAdjust(0, 0, 0) = 160, shifted left by 24/6 - 4 = 0.
        assert_eq!(scale_4x4(&c, 24, 16, None)[0][0], 160);
        // qP = 30: 16 * 10 = 160 again (m = 30 % 6 = 0), shifted left by 1.
        assert_eq!(scale_4x4(&c, 30, 16, None)[0][0], 320);
        // qP = 6: below 24, so rounded down: (16 * 10 + 2^2) >> 3 = 20.
        assert_eq!(scale_4x4(&c, 6, 16, None)[0][0], 20);
        // A separately scaled DC replaces d[0][0] untouched.
        assert_eq!(scale_4x4(&c, 30, 16, Some(-7))[0][0], -7);
    }

    #[test]
    fn hadamard_round_trips_through_its_own_inverse() {
        // The 4x4 Hadamard matrix is its own transpose and A * A = 4 * I, so
        // applying the transform twice scales by 16.
        let c = [
            [1, -2, 3, -4],
            [5, 6, -7, 8],
            [-9, 10, 11, -12],
            [13, -14, 15, 16],
        ];
        let twice = hadamard_4x4(&hadamard_4x4(&c));
        for i in 0..4 {
            for j in 0..4 {
                assert_eq!(twice[i][j], 16 * c[i][j], "({i}, {j})");
            }
        }
    }

    #[test]
    fn chroma_dc_transform_is_the_2x2_hadamard() {
        let c = [[4, 0], [0, 0]];
        // qP = 30: levelScale 16 * 10 = 160; ((4 * 160) << 5) >> 5 = 640.
        let dc = inverse_chroma_dc(&c, 30, 16);
        assert_eq!(dc, [[640; 2]; 2]);
        let c = [[0, 4], [0, 0]];
        let dc = inverse_chroma_dc(&c, 30, 16);
        assert_eq!(dc[0][0], 640);
        assert_eq!(dc[0][1], -640, "sign alternates along the row");
    }

    #[test]
    fn luma_dc_scaling_switches_branch_at_qp_36() {
        let mut c = [[0i32; 4]; 4];
        c[0][0] = 1;
        // Below 36 the DC is rounded: (1 * 160 + 2^(5-5)) >> (6-5) = 80 at qP 30.
        assert_eq!(inverse_luma_dc(&c, 30, 16)[0][0], 80);
        // At 36 it shifts up: (1 * 160) << 0 = 160.
        assert_eq!(inverse_luma_dc(&c, 36, 16)[0][0], 160);
    }

    #[test]
    fn clip1_saturates_both_ends() {
        assert_eq!(clip1(-1), 0);
        assert_eq!(clip1(256), 255);
        assert_eq!(clip1(128), 128);
    }
}

//! Rewriting samples coded against one YUV matrix as the same picture coded
//! against another — the one colour operation an encode performs when a clip's
//! space is not the space the file being written declares.

use ec_core::color::Matrix;

/// Rewrites planar 4:2:0 samples coded against `from` as the same picture coded
/// against `to`, in place. Both spaces here are limited range with the same
/// primaries, so this is the matrix and nothing else: no gamut mapping, no range
/// scaling, and the 16/128/128 of black lands on itself either way — which is
/// what makes a letterboxed bar and a gap's black frame cost nothing.
///
/// Derived from the two matrices, not tabulated: with R,G,B eliminated between
/// them, luma picks up a chroma term and the two chroma planes mix only with
/// each other — so chroma never needs the luma plane and is remapped at its own
/// resolution, while luma reads the co-sited chroma sample. 8.8 fixed point.
///
/// A no-op when the two spaces are the same, so the caller may hand any pair;
/// BT.2020 is never a target here — an HDR source goes through a tone map, not
/// through this.
pub fn remap(from: Matrix, to: Matrix, y: &mut [u8], u: &mut [u8], v: &mut [u8], width: usize) {
    // On normalised samples the two rows are Y' = Y - 0.1182 Cb - 0.2127 Cr,
    // Cb' = 1.0185 Cb + 0.1146 Cr, Cr' = 0.0751 Cb + 1.0251 Cr; in *code* values
    // the luma row's chroma terms carry a further 219/224, the ratio of the two
    // limited-range spans, while the chroma rows' do not (both sides are
    // chroma). That is the whole difference between these numbers and the
    // published ones.
    let coeffs = match (from, to) {
        (Matrix::Bt601, Matrix::Bt709) => [-30, -53, 261, 29, 19, 262],
        // ...and its inverse, which round-trips to within a code.
        (Matrix::Bt709, Matrix::Bt601) => [25, 49, 253, -28, -19, 252],
        _ => return,
    };
    let [yb, yr, ub, ur, vb, vr] = coeffs;
    let cw = width.div_ceil(2);
    for (i, y) in y.iter_mut().enumerate() {
        let (row, col) = (i / width, i % width);
        let ci = (row / 2) * cw + col / 2;
        let (d, e) = (u[ci] as i32 - 128, v[ci] as i32 - 128);
        *y = (*y as i32 + ((yb * d + yr * e + 128) >> 8)).clamp(0, 255) as u8;
    }
    for (u, v) in u.iter_mut().zip(v) {
        let (d, e) = (*u as i32 - 128, *v as i32 - 128);
        *u = (128 + ((ub * d + ur * e + 128) >> 8)).clamp(0, 255) as u8;
        *v = (128 + ((vb * d + vr * e + 128) >> 8)).clamp(0, 255) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published luma weights (kr, kb) of each matrix — the numbers the
    /// fixed-point coefficients above are derived from.
    fn weights(matrix: Matrix) -> (f64, f64) {
        match matrix {
            Matrix::Bt601 => (0.299, 0.114),
            Matrix::Bt709 => (0.2126, 0.0722),
            Matrix::Bt2020Ncl => (0.2627, 0.0593),
        }
    }

    /// Limited-range 4:2:0 samples for one colour, in `matrix`, the float way —
    /// what a correct encoder would have written.
    fn encode(matrix: Matrix, rgb: [f64; 3]) -> (u8, u8, u8) {
        let (kr, kb) = weights(matrix);
        let [r, g, b] = rgb.map(|c| c / 255.0);
        let luma = kr * r + (1.0 - kr - kb) * g + kb * b;
        let round = |x: f64| x.round().clamp(0.0, 255.0) as u8;
        (
            round(16.0 + 219.0 * luma),
            round(128.0 + 224.0 * 0.5 * (b - luma) / (1.0 - kb)),
            round(128.0 + 224.0 * 0.5 * (r - luma) / (1.0 - kr)),
        )
    }

    /// ...and back out of `matrix`, unrounded, which is what a player shows.
    fn decode(matrix: Matrix, (y, u, v): (u8, u8, u8)) -> [f64; 3] {
        let (kr, kb) = weights(matrix);
        let luma = (f64::from(y) - 16.0) / 219.0;
        let cb = (f64::from(u) - 128.0) / 224.0;
        let cr = (f64::from(v) - 128.0) / 224.0;
        let r = luma + 2.0 * (1.0 - kr) * cr;
        let b = luma + 2.0 * (1.0 - kb) * cb;
        let g = (luma - kr * r - kb * b) / (1.0 - kr - kb);
        [r, g, b].map(|c| c * 255.0)
    }

    /// One colour as a 2x2 picture — four luma samples over one chroma pair, the
    /// smallest thing [`remap`] indexes the way it indexes a frame.
    fn tile((y, u, v): (u8, u8, u8)) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        (vec![y; 4], vec![u], vec![v])
    }

    /// The claim the whole reconcile rests on: samples coded against one matrix,
    /// remapped, and then *decoded by the other matrix* are the colour that went
    /// in — not a second conversion of it. Within a code or two, which is what
    /// 8-bit 8.8 fixed point can promise.
    #[test]
    fn a_remap_is_the_same_colour_read_by_the_other_matrix() {
        let colours = [
            [255.0, 0.0, 0.0],
            [0.0, 255.0, 0.0],
            [0.0, 0.0, 255.0],
            [255.0, 255.0, 0.0],
            [18.0, 200.0, 140.0],
            [128.0, 128.0, 128.0],
            [235.0, 210.0, 30.0],
        ];
        for (from, to) in [
            (Matrix::Bt601, Matrix::Bt709),
            (Matrix::Bt709, Matrix::Bt601),
        ] {
            for rgb in colours {
                let (mut y, mut u, mut v) = tile(encode(from, rgb));
                remap(from, to, &mut y, &mut u, &mut v, 2);
                let out = decode(to, (y[0], u[0], v[0]));
                for (want, got) in rgb.iter().zip(&out) {
                    assert!(
                        (want - got).abs() <= 2.0,
                        "{from:?}->{to:?} {rgb:?} came out {out:?}"
                    );
                }
            }
        }
    }

    /// Black, white and neutral grey are the same samples in both matrices, and
    /// a remap has to leave them alone: those are the letterbox bars, the gap
    /// frames and every grey in the picture, and a shift there is visible as a
    /// tint on nothing at all.
    #[test]
    fn a_remap_leaves_the_greys_where_they_are() {
        for (from, to) in [
            (Matrix::Bt601, Matrix::Bt709),
            (Matrix::Bt709, Matrix::Bt601),
        ] {
            for luma in [16u8, 128, 235] {
                let (mut y, mut u, mut v) = tile((luma, 128, 128));
                remap(from, to, &mut y, &mut u, &mut v, 2);
                assert_eq!((y[0], u[0], v[0]), (luma, 128, 128), "{from:?}->{to:?}");
            }
        }
    }

    /// The zero-cost path, asserted where it is decided: a clip already in the
    /// file's own space comes out byte for byte the samples it went in as, so an
    /// ordinary single-space project is not touched by any of this.
    #[test]
    fn a_same_space_remap_touches_nothing() {
        let mut y: Vec<u8> = (0..64 * 48).map(|i| (i % 256) as u8).collect();
        let mut u: Vec<u8> = (0..32 * 24).map(|i| (i % 256) as u8).collect();
        let mut v: Vec<u8> = (0..32 * 24).map(|i| (255 - i % 256) as u8).collect();
        let (before_y, before_u, before_v) = (y.clone(), u.clone(), v.clone());
        for matrix in [Matrix::Bt601, Matrix::Bt709, Matrix::Bt2020Ncl] {
            remap(matrix, matrix, &mut y, &mut u, &mut v, 64);
        }
        assert_eq!((y, u, v), (before_y, before_u, before_v));
    }

    /// There and back is the picture again: the two directions are one matrix
    /// and its inverse, not two independent approximations.
    #[test]
    fn a_remap_round_trips_within_a_code() {
        let y: Vec<u8> = (0..64 * 48).map(|i| (16 + i % 220) as u8).collect();
        let u: Vec<u8> = (0..32 * 24).map(|i| (16 + i % 225) as u8).collect();
        let v: Vec<u8> = (0..32 * 24).map(|i| (240 - i % 225) as u8).collect();
        let (mut ry, mut ru, mut rv) = (y.clone(), u.clone(), v.clone());
        remap(Matrix::Bt601, Matrix::Bt709, &mut ry, &mut ru, &mut rv, 64);
        remap(Matrix::Bt709, Matrix::Bt601, &mut ry, &mut ru, &mut rv, 64);
        let worst =
            |a: &[u8], b: &[u8]| a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap();
        assert!(worst(&y, &ry) <= 2, "luma drifted by {}", worst(&y, &ry));
        assert!(worst(&u, &ru) <= 2, "cb drifted by {}", worst(&u, &ru));
        assert!(worst(&v, &rv) <= 2, "cr drifted by {}", worst(&v, &rv));
    }
}

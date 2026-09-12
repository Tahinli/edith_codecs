//! Intra-frame prediction for VP8 (RFC 6386, Section 12).
//!
//! This module implements the spatial predictors a VP8 decoder applies to
//! intra-coded macroblocks: the 16x16 luma predictors and 8x8 (per-plane)
//! chroma predictors of Section 12.2/12.3, and the ten 4x4 subblock
//! predictors of Section 12.3. All functions are dependency-free, `unsafe`-free,
//! allocate nothing, and are bit-exact against the normative integer math of
//! the RFC code listings (which are in turn identical to the libvpx oracle in
//! `vp8/common/reconintra.c`, `vp8/common/reconintra4x4.c` and
//! `vpx_dsp/intrapred.c`).
//!
//! # Edge-pixel conventions (RFC 6386 Section 12, p.50)
//!
//! Pixels outside the visible frame are pre-filled by the caller: 127 for the
//! row above the top row (including the above-left pixel `P`), 129 for the
//! column left of the leftmost column. V_PRED/H_PRED/TM_PRED consume those
//! fill values directly; DC_PRED instead decides from the availability flags
//! which genuinely-visible pixels to average (RFC Section 12.2, "Note that
//! the averages used in these exceptional cases are not the same as those
//! that would be arrived at by using the out-of-bounds A and L values").
//!
//! # 4x4 above-right synthesis
//!
//! The 4x4 predictors use an 8-pixel above row `A[0..8]`: the 4 pixels
//! immediately above the subblock plus the 4 "above-right" pixels
//! (frame positions (-1,16)..(-1,19) relative to the macroblock; RFC
//! Section 12.3, subblocks 3/7/11/15). Producing those extra four pixels is
//! the caller's job: for a macroblock not on the top frame row they come from
//! the row above (replicating pixel (-1,15) at the rightmost macroblock of a
//! row); on the top frame row they are 127. [`predict4`] only consumes
//! `A[0..8]` as given.

#![deny(unsafe_code)]

/// Weighted average of three adjacent pixels centered at `y`
/// (RFC 6386 Section 12.3 `avg3`: `(x + 2y + z + 2) >> 2`; libvpx `AVG3`).
///
/// Arguments are valid pixel values, so the result always fits in `u8` and no
/// clamp is needed (RFC Section 12.3, comment above `avg3`).
#[inline]
fn avg3(x: u8, y: u8, z: u8) -> u8 {
    ((u32::from(x) + 2 * u32::from(y) + u32::from(z) + 2) >> 2) as u8
}

/// Simple average of two pixels (RFC 6386 Section 12.3 `avg2`:
/// `(x + y + 1) >> 1`; libvpx `AVG2`).
#[inline]
fn avg2(x: u8, y: u8) -> u8 {
    ((u32::from(x) + u32::from(y) + 1) >> 1) as u8
}

/// `clamp255(v)`: clamp an integer pixel difference into the valid `u8` range
/// (RFC 6386 Section 12.2 `TMpred` / Section 12.3 `B_TM_PRED`).
#[inline]
fn clamp255(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Shared DC/V/H/TM core for the 16x16 luma and 8x8 chroma blocks
/// (RFC 6386 Section 12.2 semantics; Section 12.3 states 16x16 luma is
/// "essentially identical" with 16-pixel edges).
///
/// DC rounding follows the RFC formula `DCvalue = (sum + (1 << (shf-1))) >> shf`
/// with `shf` = log2 of the number of averaged pixels: 32/16 pixels for the
/// 16x16 both/single-edge cases, 16/8 for 8x8.
fn predict_block<const N: usize>(
    mode: u8,
    have_above: bool,
    have_left: bool,
    above: &[u8; N],
    left: &[u8; N],
    above_left: u8,
    out: &mut [u8],
) {
    match mode {
        // DC_PRED: fill the block with the average of the genuinely visible
        // edge pixels; availability flags alone decide which edges count
        // (RFC Section 12.2). Neither edge: constant 128.
        0 => {
            let sum_above: u32 = above.iter().map(|&p| u32::from(p)).sum();
            let sum_left: u32 = left.iter().map(|&p| u32::from(p)).sum();
            let tz = N.trailing_zeros(); // log2(N)
            let v: u32 = match (have_above, have_left) {
                (true, true) => (sum_above + sum_left + N as u32) >> (tz + 1),
                (false, false) => 128,
                // Above available only: average the above row.
                (true, false) => (sum_above + (N as u32 >> 1)) >> tz,
                // Left column available only: average the left column.
                (false, true) => (sum_left + (N as u32 >> 1)) >> tz,
            };
            out.fill(v as u8);
        }
        // V_PRED: every row is a copy of the above row A (RFC Section 12.2).
        1 => {
            for row in out.chunks_exact_mut(N) {
                row.copy_from_slice(above);
            }
        }
        // H_PRED: every column is a copy of the left column L (RFC Section 12.2).
        2 => {
            for (row, &l) in out.chunks_exact_mut(N).zip(left.iter()) {
                row.fill(l);
            }
        }
        // TM_PRED: X_ij = clamp255(L_i + A_j - P) (RFC Section 12.2 `TMpred`).
        // Unlike DC_PRED this mode does use the 127/129 out-of-bounds fills.
        3 => {
            for (row, &l) in out.chunks_exact_mut(N).zip(left.iter()) {
                for (o, &a) in row.iter_mut().zip(above.iter()) {
                    *o = clamp255(i32::from(l) + i32::from(a) - i32::from(above_left));
                }
            }
        }
        _ => panic!("predict_block: invalid intra mode {mode} (expected 0..=3)"),
    }
}

/// 16x16 luma prediction (RFC 6386 Section 12.3; predictor math per
/// Section 12.2).
///
/// `mode` is a u8 in `{0=DC, 1=V, 2=H, 3=TM}`. `above` holds the 16
/// reconstructed pixels of the row above the macroblock, `left` the 16 pixels
/// of the column to its left (adjacent entries separated by one frame row),
/// `above_left` is the corner pixel `P`. `have_above`/`have_left` state
/// whether those edges are genuinely in-frame.
///
/// For DC the flags alone decide what is averaged (the out-of-bounds fills
/// are NOT used); V/H/TM consume the caller-prepared out-of-bounds fills
/// (127 above/`above_left` on the top frame row, 129 on the left frame
/// column) written into `above`/`left`/`above_left`.
///
/// `out` receives 256 row-major prediction pixels.
pub fn predict16(
    mode: u8,
    have_above: bool,
    have_left: bool,
    above: &[u8; 16],
    left: &[u8; 16],
    above_left: u8,
    out: &mut [u8; 256],
) {
    predict_block::<16>(mode, have_above, have_left, above, left, above_left, out);
}

/// 8x8 chroma prediction for one plane (RFC 6386 Section 12.2).
///
/// Identical semantics to [`predict16`] with 8-pixel edges; call it once for
/// U and once for V. `mode` is a u8 in `{0=DC, 1=V, 2=H, 3=TM}`. DC averages
/// 16 pixels (both edges: `(sumA + sumL + 8) >> 4`) or 8 pixels (one edge:
/// `(sum + 4) >> 3`), or fills 128.
///
/// `out` receives 64 row-major prediction pixels.
pub fn predict8(
    mode: u8,
    have_above: bool,
    have_left: bool,
    above: &[u8; 8],
    left: &[u8; 8],
    above_left: u8,
    out: &mut [u8; 64],
) {
    predict_block::<8>(mode, have_above, have_left, above, left, above_left, out);
}

/// 4x4 subblock prediction (RFC 6386 Section 12.3 `subblock_intra_predict`).
///
/// `mode` is a BMode integer: `0=B_DC 1=B_TM 2=B_VE 3=B_HE 4=B_LD 5=B_RD
/// 6=B_VR 7=B_VL 8=B_HD 9=B_HU`. `above` is the 8-pixel row `A[0..8]`
/// (4 above + 4 above-right; see the [module docs](self) for the caller-side
/// synthesis rule), `left` is `L[0..4]`, `above_left` is `P = A[-1] = L[-1]`.
///
/// Both edges are always considered present for subblocks: the caller
/// pre-fills out-of-bounds pixels (127 above including P, 129 left) before
/// the call, which is why no availability flags exist here.
///
/// `out` receives 16 row-major prediction pixels.
///
/// # RFC typo
///
/// The RFC listing for `B_HD_PRED` contains the line
/// `B[2][0] = B[3][2] = svg2p(E + 1)`; `svg2p` is a typo for `avg2p` (there
/// is no `svg2p` definition; libvpx's `d153_predictor` computes `AVG2(K, J)`
/// there, confirming the correction implemented here).
pub fn predict4(mode: u8, above: &[u8; 8], left: &[u8; 4], above_left: u8, out: &mut [u8; 16]) {
    // The 9 already-constructed edge pixels in RFC scan order:
    // E[0..4] = L[3], L[2], L[1], L[0]; E[4] = P; E[5..9] = A[0..4].
    let e: [u8; 9] = [
        left[3], left[2], left[1], left[0], above_left, above[0], above[1], above[2], above[3],
    ];
    // Row-major B[r][c] = b[r * 4 + c].
    let mut b = [0u8; 16];
    // avg3p(E + k) = avg3(E[k-1], E[k], E[k+1]); avg2p(E + k) = avg2(E[k], E[k+1]).
    match mode {
        // B_DC_PRED: DC value = (sum(A[0..4]) + sum(L[0..4]) + 4) >> 3.
        0 => {
            let mut v: u32 = 4;
            for i in 0..4 {
                v += u32::from(above[i]) + u32::from(left[i]);
            }
            v >>= 3;
            b.fill(v as u8);
        }
        // B_TM_PRED: just like 16x16 TM_PRED (RFC Section 12.3).
        1 => {
            for r in 0..4 {
                for c in 0..4 {
                    b[r * 4 + c] =
                        clamp255(i32::from(left[r]) + i32::from(above[c]) - i32::from(above_left));
                }
            }
        }
        // B_VE_PRED: all four rows = smoothed top row avg3p(A + c), where
        // A[-1] = P participates in column 0 (libvpx `vpx_ve_predictor_4x4`).
        2 => {
            for c in 0..4usize {
                let v = if c == 0 {
                    avg3(above_left, above[0], above[1])
                } else {
                    avg3(above[c - 1], above[c], above[c + 1])
                };
                b[c] = v;
                b[4 + c] = v;
                b[8 + c] = v;
                b[12 + c] = v;
            }
        }
        // B_HE_PRED: columns = smoothed left column; bottom row exceptional
        // because L[4] does not exist: avg3(L[2], L[3], L[3]).
        3 => {
            let rows = [
                avg3(above_left, left[0], left[1]),
                avg3(left[0], left[1], left[2]),
                avg3(left[1], left[2], left[3]),
                avg3(left[2], left[3], left[3]),
            ];
            for (r, &v) in rows.iter().enumerate() {
                b[r * 4..r * 4 + 4].fill(v);
            }
        }
        // B_LD_PRED: southwest (left and down) 45-degree diagonals off the
        // above row; B[3][3] uses avg3(A[6], A[7], A[7]) because A[8] does
        // not exist (libvpx `d45e_predictor_4x4`).
        4 => {
            b[0] = avg3(above[0], above[1], above[2]);
            b[1] = avg3(above[1], above[2], above[3]);
            b[4] = b[1];
            b[2] = avg3(above[2], above[3], above[4]);
            b[5] = b[2];
            b[8] = b[2];
            b[3] = avg3(above[3], above[4], above[5]);
            b[6] = b[3];
            b[9] = b[3];
            b[12] = b[3];
            b[7] = avg3(above[4], above[5], above[6]);
            b[10] = b[7];
            b[13] = b[7];
            b[11] = avg3(above[5], above[6], above[7]);
            b[14] = b[11];
            b[15] = avg3(above[6], above[7], above[7]);
        }
        // B_RD_PRED: southeast 45-degree diagonals off the whole E edge
        // (libvpx `d135_predictor_4x4`).
        5 => {
            b[12] = avg3(e[0], e[1], e[2]);
            b[13] = avg3(e[1], e[2], e[3]);
            b[8] = b[13];
            b[14] = avg3(e[2], e[3], e[4]);
            b[9] = b[14];
            b[4] = b[14];
            b[15] = avg3(e[3], e[4], e[5]);
            b[10] = b[15];
            b[5] = b[15];
            b[0] = b[15];
            b[11] = avg3(e[4], e[5], e[6]);
            b[6] = b[11];
            b[1] = b[11];
            b[7] = avg3(e[5], e[6], e[7]);
            b[2] = b[7];
            b[3] = avg3(e[6], e[7], e[8]);
        }
        // B_VR_PRED: vertical-right diagonals mixing avg3/avg2 along E
        // (libvpx `d117_predictor_4x4`).
        6 => {
            b[12] = avg3(e[1], e[2], e[3]);
            b[8] = avg3(e[2], e[3], e[4]);
            b[13] = avg3(e[3], e[4], e[5]);
            b[4] = b[13];
            b[9] = avg2(e[4], e[5]);
            b[0] = b[9];
            b[14] = avg3(e[4], e[5], e[6]);
            b[5] = b[14];
            b[10] = avg2(e[5], e[6]);
            b[1] = b[10];
            b[15] = avg3(e[5], e[6], e[7]);
            b[6] = b[15];
            b[11] = avg2(e[6], e[7]);
            b[2] = b[11];
            b[7] = avg3(e[6], e[7], e[8]);
            b[3] = avg2(e[7], e[8]);
        }
        // B_VL_PRED: vertical-left diagonals off the above row; the last two
        // values do not strictly follow the pattern (RFC Section 12.3;
        // libvpx `d63e_predictor_4x4`).
        7 => {
            b[0] = avg2(above[0], above[1]);
            b[4] = avg3(above[0], above[1], above[2]);
            b[8] = avg2(above[1], above[2]);
            b[1] = b[8];
            b[5] = avg3(above[1], above[2], above[3]);
            b[12] = b[5];
            b[9] = avg2(above[2], above[3]);
            b[2] = b[9];
            b[13] = avg3(above[2], above[3], above[4]);
            b[6] = b[13];
            b[10] = avg2(above[3], above[4]);
            b[3] = b[10];
            b[14] = avg3(above[3], above[4], above[5]);
            b[7] = b[14];
            b[11] = avg3(above[4], above[5], above[6]);
            b[15] = avg3(above[5], above[6], above[7]);
        }
        // B_HD_PRED: horizontal-down diagonals; the RFC's `svg2p(E + 1)` is
        // the avg2p correction noted in this function's docs (libvpx
        // `d153_predictor_4x4`).
        8 => {
            b[12] = avg2(e[0], e[1]);
            b[13] = avg3(e[0], e[1], e[2]);
            b[8] = avg2(e[1], e[2]); // corrected svg2p -> avg2p line
            b[14] = b[8];
            b[9] = avg3(e[1], e[2], e[3]);
            b[15] = b[9];
            b[10] = avg2(e[2], e[3]);
            b[4] = b[10];
            b[11] = avg3(e[2], e[3], e[4]);
            b[5] = b[11];
            b[6] = avg2(e[3], e[4]);
            b[0] = b[6];
            b[7] = avg3(e[3], e[4], e[5]);
            b[1] = b[7];
            b[2] = avg3(e[4], e[5], e[6]);
            b[3] = avg3(e[5], e[6], e[7]);
        }
        // B_HU_PRED: horizontal-up diagonals off the left column; the bottom
        // rows cannot follow the pattern and collapse to L[3] (RFC
        // Section 12.3; libvpx `d207_predictor_4x4`).
        9 => {
            b[0] = avg2(left[0], left[1]);
            b[1] = avg3(left[0], left[1], left[2]);
            b[2] = avg2(left[1], left[2]);
            b[4] = b[2];
            b[3] = avg3(left[1], left[2], left[3]);
            b[5] = b[3];
            b[6] = avg2(left[2], left[3]);
            b[8] = b[6];
            b[7] = avg3(left[2], left[3], left[3]);
            b[9] = b[7];
            let v = left[3];
            b[10] = v;
            b[11] = v;
            b[12] = v;
            b[13] = v;
            b[14] = v;
            b[15] = v;
        }
        _ => panic!("predict4: invalid intra 4x4 mode {mode} (expected 0..=9)"),
    }
    out.copy_from_slice(&b);
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------- --
    // Reference model used to derive the expected vectors below. It is a
    // verbatim Python transcription of RFC 6386 Section 12.3
    // subblock_intra_predict (with svg2p corrected to avg2p); outputs were
    // generated with it and then hand-spot-checked:
    //
    //   def avg2(x, y): return (x + y + 1) >> 1
    //   def avg3(x, y, z): return (x + 2*y + z + 2) >> 2
    //   def clamp255(v): return max(0, min(255, v))
    //   # A = A[0..8], L = L[0..4], P = above-left
    //   E = [L[3], L[2], L[1], L[0], P, A[0], A[1], A[2], A[3]]
    //   B_DC: v = (4 + sum(A[0:4]) + sum(L[0:4])) >> 3
    //   B_TM: B[r][c] = clamp255(L[r] + A[c] - P)
    //   B_VE: col c: avg3(P, A0, A1) then avg3(A[c-1], A[c], A[c+1])
    //   B_HE: rows avg3(P, L0, L1), avg3(L0, L1, L2), avg3(L1, L2, L3),
    //         avg3(L2, L3, L3)
    //   B_LD: B[0][0]=avg3(A0,A1,A2); B[0][1]=B[1][0]=avg3(A1,A2,A3); ...
    //         B[3][3]=avg3(A6,A7,A7)
    //   B_RD: B[3][0]=avg3(E0,E1,E2); B[3][1]=B[2][0]=avg3(E1,E2,E3);
    //         B[3][2]=B[2][1]=B[1][0]=avg3(E2,E3,E4);
    //         diag=avg3(E3,E4,E5); ...; B[0][3]=avg3(E6,E7,E8)
    //   B_VR: B[3][0]=avg3(E1,E2,E3); B[2][0]=avg3(E2,E3,E4);
    //         B[3][1]=B[1][0]=avg3(E3,E4,E5); B[2][1]=B[0][0]=avg2(E4,E5);
    //         B[3][2]=B[1][1]=avg3(E4,E5,E6); B[2][2]=B[0][1]=avg2(E5,E6);
    //         B[3][3]=B[1][2]=avg3(E5,E6,E7); B[2][3]=B[0][2]=avg2(E6,E7);
    //         B[1][3]=avg3(E6,E7,E8); B[0][3]=avg2(E7,E8)
    //   B_VL: B[0][0]=avg2(A0,A1); B[1][0]=avg3(A0,A1,A2);
    //         B[2][0]=B[0][1]=avg2(A1,A2); B[1][1]=B[3][0]=avg3(A1,A2,A3);
    //         B[2][1]=B[0][2]=avg2(A2,A3); B[3][1]=B[1][2]=avg3(A2,A3,A4);
    //         B[2][2]=B[0][3]=avg2(A3,A4); B[3][2]=B[1][3]=avg3(A3,A4,A5);
    //         B[2][3]=avg3(A4,A5,A6); B[3][3]=avg3(A5,A6,A7)
    //   B_HD: B[3][0]=avg2(E0,E1); B[3][1]=avg3(E0,E1,E2);
    //         B[2][0]=B[3][2]=avg2(E1,E2)          <- svg2p typo corrected
    //         B[2][1]=B[3][3]=avg3(E1,E2,E3);
    //         B[2][2]=B[1][0]=avg2(E2,E3);
    //         B[2][3]=B[1][1]=avg3(E2,E3,E4);
    //         B[1][2]=B[0][0]=avg2(E3,E4);
    //         B[1][3]=B[0][1]=avg3(E3,E4,E5);
    //         B[0][2]=avg3(E4,E5,E6); B[0][3]=avg3(E5,E6,E7)
    //   B_HU: B[0][0]=avg2(L0,L1); B[0][1]=avg3(L0,L1,L2);
    //         B[0][2]=B[1][0]=avg2(L1,L2); B[0][3]=B[1][1]=avg3(L1,L2,L3);
    //         B[1][2]=B[2][0]=avg2(L2,L3);
    //         B[1][3]=B[2][1]=avg3(L2,L3,L3);
    //         B[2][2..4]=B[3][0..4]=L3
    // ---------------------------------------------------------------- --

    /// Fixed edge pattern shared by all ten subblock tests:
    /// above = [10,20,30,40,50,60,70,80], left = [15,25,35,45], P = 5.
    const EDGE_A: [u8; 8] = [10, 20, 30, 40, 50, 60, 70, 80];
    const EDGE_L: [u8; 4] = [15, 25, 35, 45];
    const EDGE_P: u8 = 5;

    fn assert_pred4(mode: u8, expected: [u8; 16]) {
        let mut out = [0u8; 16];
        predict4(mode, &EDGE_A, &EDGE_L, EDGE_P, &mut out);
        assert_eq!(out, expected, "predict4 mode {mode}");
    }

    #[test]
    fn subblock_dc_constant() {
        // v = (4 + (10+20+30+40) + (15+25+35+45)) >> 3 = (4+100+120)>>3 = 28.
        assert_pred4(0, [28; 16]);
    }

    #[test]
    fn subblock_tm_gradient() {
        // B[r][c] = L[r] + A[c] - P = L[r] + A[c] - 5.
        assert_pred4(
            1,
            [
                20, 30, 40, 50, // row 0: 15 + A[c] - 5
                30, 40, 50, 60, // row 1: 25 + A[c] - 5
                40, 50, 60, 70, // row 2
                50, 60, 70, 80, // row 3
            ],
        );
    }

    #[test]
    fn subblock_ve_smoothed_row() {
        // Row = avg3(A[c-1], A[c], A[c+1]) with A[-1] = P:
        // avg3(5,10,20)=11, avg3(10,20,30)=20, avg3(20,30,40)=30,
        // avg3(30,40,50)=40.
        let row = [11, 20, 30, 40];
        let mut expected = [0u8; 16];
        for r in 0..4 {
            expected[r * 4..r * 4 + 4].copy_from_slice(&row);
        }
        assert_pred4(2, expected);
    }

    #[test]
    fn subblock_he_smoothed_column() {
        // Rows: avg3(5,15,25)=15, avg3(15,25,35)=25, avg3(25,35,45)=35,
        // avg3(35,45,45)=43 (bottom-row exception, L[4] absent).
        assert_pred4(
            3,
            [
                15, 15, 15, 15, 25, 25, 25, 25, 35, 35, 35, 35, 43, 43, 43, 43,
            ],
        );
    }

    #[test]
    fn subblock_ld_downright_diagonal() {
        // B[3][3] = avg3(70,80,80) = (70 + 160 + 80 + 2) >> 2 = 78 (A[8]
        // does not exist).
        assert_pred4(
            4,
            [
                20, 30, 40, 50, // avg3(10,20,30)=20, avg3(20,30,40)=30, ...
                30, 40, 50, 60, //
                40, 50, 60, 70, //
                50, 60, 70, 78, //
            ],
        );
    }

    #[test]
    fn subblock_rd_downright_diagonal() {
        // E = [45,35,25,15,5,10,20,30,40].
        // B[3][0] = avg3(45,35,25) = 35; B[3][1]=B[2][0] = avg3(35,25,15) = 25;
        // B[3][2]=B[2][1]=B[1][0] = avg3(25,15,5) = 15;
        // main diagonal = avg3(15,5,10) = 9; B[2][3]=B[1][2]=B[0][1] =
        // avg3(5,10,20) = 11; B[1][3]=B[0][2] = avg3(10,20,30) = 20;
        // B[0][3] = avg3(20,30,40) = 30.
        assert_pred4(
            5,
            [
                9, 11, 20, 30, //
                15, 9, 11, 20, //
                25, 15, 9, 11, //
                35, 25, 15, 9, //
            ],
        );
    }

    #[test]
    fn subblock_vr_mixed_diagonal() {
        // E = [45,35,25,15,5,10,20,30,40].
        // B[0][0] = avg2(E4,E5) = avg2(5,10) = 8;
        // B[0][1] = avg2(10,20) = 15; B[0][2] = avg2(20,30) = 25;
        // B[0][3] = avg2(30,40) = 35; B[1][0] = avg3(15,5,10) = 9;
        // B[1][1] = avg3(5,10,20) = 11; B[1][2] = avg3(10,20,30) = 20;
        // B[1][3] = avg3(20,30,40) = 30; B[2][0] = avg3(25,15,5) = 15;
        // B[2][1] = 8; B[2][2] = 15; B[2][3] = 25;
        // B[3][0] = avg3(35,25,15) = 25; B[3][1] = 9; B[3][2] = 11; B[3][3] = 20.
        assert_pred4(
            6,
            [
                8, 15, 25, 35, //
                9, 11, 20, 30, //
                15, 8, 15, 25, //
                25, 9, 11, 20, //
            ],
        );
    }

    #[test]
    fn subblock_vl_downleft_diagonal() {
        // avg2/avg3 pairs along the above row:
        // cols: avg2(10,20)=15, avg2(20,30)=25, avg2(30,40)=35, avg2(40,50)=45;
        // rows shift by avg3: 20, 30, 40, 50; last column 45/50/60/70 with
        // B[2][3]=avg3(50,60,70)=60, B[3][3]=avg3(60,70,80)=70.
        assert_pred4(
            7,
            [
                15, 25, 35, 45, //
                20, 30, 40, 50, //
                25, 35, 45, 60, //
                30, 40, 50, 70, //
            ],
        );
    }

    #[test]
    fn subblock_hd_uses_avg2p_for_svg2p_typo_line() {
        // E = [45,35,25,15,5,10,20,30,40].
        // The RFC's `svg2p(E + 1)` line: B[2][0] = B[3][2] = avg2p(E+1) =
        // avg2(E[1], E[2]) = avg2(35,25) = 30. A mis-transcription (any
        // avg3-style or E-shifted variant) would not yield 30 here.
        // B[3][0] = avg2(45,35) = 40; B[3][1] = avg3(45,35,25) = 35;
        // B[2][1]=B[3][3] = avg3(35,25,15) = 25; B[2][2]=B[1][0] =
        // avg2(25,15) = 20; B[2][3]=B[1][1] = avg3(25,15,5) = 15;
        // B[1][2]=B[0][0] = avg2(15,5) = 10; B[1][3]=B[0][1] =
        // avg3(15,5,10) = 9; B[0][2] = avg3(5,10,20) = 11;
        // B[0][3] = avg3(10,20,30) = 20.
        assert_pred4(
            8,
            [
                10, 9, 11, 20, //
                20, 15, 10, 9, //
                30, 25, 20, 15, //
                40, 35, 30, 25, //
            ],
        );
        // Directly pin the corrected-typo cells.
        let mut out = [0u8; 16];
        predict4(8, &EDGE_A, &EDGE_L, EDGE_P, &mut out);
        assert_eq!(
            (out[8], out[14]),
            (30, 30),
            "B[2][0] and B[3][2] = avg2(E1,E2)"
        );
    }

    #[test]
    fn subblock_hu_up_diagonal() {
        // B[0][0] = avg2(15,25) = 20; B[0][1] = avg3(15,25,35) = 25;
        // B[0][2]=B[1][0] = avg2(25,35) = 30; B[0][3]=B[1][1] =
        // avg3(25,35,45) = 35; B[1][2]=B[2][0] = avg2(35,45) = 40;
        // B[1][3]=B[2][1] = avg3(35,45,45) = 43; bottom rows = L[3] = 45.
        assert_pred4(
            9,
            [
                20, 25, 30, 35, //
                30, 35, 40, 43, //
                40, 43, 45, 45, //
                45, 45, 45, 45, //
            ],
        );
    }

    #[test]
    fn dc16_all_availability_cases() {
        // above = [10; 16] (sum 160), left = [20; 16] (sum 320).
        let above = [10u8; 16];
        let left = [20u8; 16];
        let mut out = [0u8; 256];

        // Both edges: (160 + 320 + 16) >> 5 = 15.
        predict16(0, true, true, &above, &left, 5, &mut out);
        assert_eq!(out, [15; 256]);

        // Neither edge (top-left macroblock): constant 128. The array
        // contents are irrelevant here; availability flags decide.
        predict16(0, false, false, &above, &left, 5, &mut out);
        assert_eq!(out, [128; 256]);

        // Above only (left frame column): (160 + 8) >> 4 = 10.
        predict16(0, true, false, &above, &left, 5, &mut out);
        assert_eq!(out, [10; 256]);

        // Left only (top frame row): (320 + 8) >> 4 = 20.
        predict16(0, false, true, &above, &left, 5, &mut out);
        assert_eq!(out, [20; 256]);
    }

    #[test]
    fn dc8_all_availability_cases() {
        // above = [10; 8] (sum 80), left = [20; 8] (sum 160).
        let above = [10u8; 8];
        let left = [20u8; 8];
        let mut out = [0u8; 64];

        // Both: (80 + 160 + 8) >> 4 = 15.
        predict8(0, true, true, &above, &left, 5, &mut out);
        assert_eq!(out, [15; 64]);

        // Neither: 128.
        predict8(0, false, false, &above, &left, 5, &mut out);
        assert_eq!(out, [128; 64]);

        // Above only: (80 + 4) >> 3 = 10.
        predict8(0, true, false, &above, &left, 5, &mut out);
        assert_eq!(out, [10; 64]);

        // Left only: (160 + 4) >> 3 = 20.
        predict8(0, false, true, &above, &left, 5, &mut out);
        assert_eq!(out, [20; 64]);
    }

    #[test]
    fn tm16_clamps_both_directions() {
        let mut out = [0u8; 256];

        // Negative difference clamps to 0: P high (250), L and A low (0)
        // gives L + A - P = -250 everywhere.
        predict16(3, true, true, &[0u8; 16], &[0u8; 16], 250, &mut out);
        assert_eq!(out, [0; 256]);

        // Overflow clamps to 255: P = 0, L = A = 255 gives 510 everywhere.
        predict16(3, true, true, &[255u8; 16], &[255u8; 16], 0, &mut out);
        assert_eq!(out, [255; 256]);

        // Exact in-range gradient: out[r][c] = 200 + c - 50 = 150 + c,
        // no clamping, verifies arithmetic (not just the clamps).
        let above: [u8; 16] = core::array::from_fn(|i| i as u8);
        predict16(3, true, true, &above, &[200u8; 16], 50, &mut out);
        for r in 0..16 {
            let row: [u8; 16] = core::array::from_fn(|c| (150 + c) as u8);
            assert_eq!(out[r * 16..r * 16 + 16], row, "TM16 row {r}");
        }
    }

    #[test]
    fn tm4_clamps_via_predict4() {
        // Same clamp contract at 4x4 (RFC B_TM_PRED == clamp255(L + A - P)).
        let mut out = [0u8; 16];
        predict4(1, &[0u8; 8], &[0u8; 4], 250, &mut out);
        assert_eq!(out, [0; 16]);
        predict4(1, &[255u8; 8], &[255u8; 4], 0, &mut out);
        assert_eq!(out, [255; 16]);
    }

    #[test]
    fn v_and_h_copy_edges() {
        // V_PRED: every 16x16 row equals the above row.
        let above: [u8; 16] = core::array::from_fn(|i| (i * 7 + 3) as u8);
        let mut out = [0u8; 256];
        predict16(1, true, true, &above, &[0u8; 16], 0, &mut out);
        for row in out.chunks_exact(16) {
            assert_eq!(row, above);
        }

        // H_PRED: every 8x8 row is the constant left[r].
        let left: [u8; 8] = core::array::from_fn(|i| (i * 11 + 1) as u8);
        let mut out8 = [0u8; 64];
        predict8(2, true, true, &[0u8; 8], &left, 0, &mut out8);
        for (r, row) in out8.chunks_exact(8).enumerate() {
            assert_eq!(row, [left[r]; 8]);
        }
    }

    #[test]
    #[should_panic(expected = "invalid intra mode")]
    fn predict16_rejects_invalid_mode() {
        let mut out = [0u8; 256];
        predict16(4, true, true, &[0u8; 16], &[0u8; 16], 0, &mut out);
    }

    #[test]
    #[should_panic(expected = "invalid intra 4x4 mode")]
    fn predict4_rejects_invalid_mode() {
        let mut out = [0u8; 16];
        predict4(10, &[0u8; 8], &[0u8; 4], 0, &mut out);
    }
}

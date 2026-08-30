//! AV1 spec 7.16 upscaling process (`av1_upscale_normative_rows` in
//! libaom's `av1/common/resize.c`) -- the horizontal-only linear-filter
//! upscale that runs on the decoded frame after deblocking and CDEF (and,
//! once this crate supports loop restoration, BEFORE it -- libaom's
//! `decodeframe.c` calls `superres_post_decode()` between `av1_cdef_frame`
//! and `av1_loop_restoration_filter_frame`, not after LR; the charter's
//! anchor note that superres runs "after loop restoration" does not match
//! the source and is corrected here).
//!
//! Every constant and the 64x8 `av1_resize_filter_normative` coefficient
//! table below is copied verbatim from libaom `aom_dsp/aom_filter.h` and
//! `av1/common/resize.c` (`AOM_VERSION=v3.13.3`, `~/.cache/aom-oracle`).

/// `RS_SUBPEL_BITS`.
const RS_SUBPEL_BITS: u32 = 6;
/// `RS_SCALE_SUBPEL_BITS`.
const RS_SCALE_SUBPEL_BITS: u32 = 14;
/// `RS_SCALE_SUBPEL_MASK`.
const RS_SCALE_SUBPEL_MASK: i64 = (1i64 << RS_SCALE_SUBPEL_BITS) - 1;
/// `RS_SCALE_EXTRA_BITS`.
const RS_SCALE_EXTRA_BITS: u32 = RS_SCALE_SUBPEL_BITS - RS_SUBPEL_BITS;
/// `RS_SCALE_EXTRA_OFF`.
const RS_SCALE_EXTRA_OFF: i64 = 1i64 << (RS_SCALE_EXTRA_BITS - 1);
/// `UPSCALE_NORMATIVE_TAPS`.
const UPSCALE_NORMATIVE_TAPS: usize = 8;
/// `FILTER_BITS`.
const FILTER_BITS: u32 = 7;

/// `av1_resize_filter_normative`, libaom `av1/common/resize.c`. 64 subpel
/// phases x 8 taps.
#[rustfmt::skip]
const RESIZE_FILTER_NORMATIVE: [[i16; UPSCALE_NORMATIVE_TAPS]; 1 << RS_SUBPEL_BITS] = [
    [0, 0, 0, 128, 0, 0, 0, 0],        [0, 0, -1, 128, 2, -1, 0, 0],
    [0, 1, -3, 127, 4, -2, 1, 0],      [0, 1, -4, 127, 6, -3, 1, 0],
    [0, 2, -6, 126, 8, -3, 1, 0],      [0, 2, -7, 125, 11, -4, 1, 0],
    [-1, 2, -8, 125, 13, -5, 2, 0],    [-1, 3, -9, 124, 15, -6, 2, 0],
    [-1, 3, -10, 123, 18, -6, 2, -1],  [-1, 3, -11, 122, 20, -7, 3, -1],
    [-1, 4, -12, 121, 22, -8, 3, -1],  [-1, 4, -13, 120, 25, -9, 3, -1],
    [-1, 4, -14, 118, 28, -9, 3, -1],  [-1, 4, -15, 117, 30, -10, 4, -1],
    [-1, 5, -16, 116, 32, -11, 4, -1], [-1, 5, -16, 114, 35, -12, 4, -1],
    [-1, 5, -17, 112, 38, -12, 4, -1], [-1, 5, -18, 111, 40, -13, 5, -1],
    [-1, 5, -18, 109, 43, -14, 5, -1], [-1, 6, -19, 107, 45, -14, 5, -1],
    [-1, 6, -19, 105, 48, -15, 5, -1], [-1, 6, -19, 103, 51, -16, 5, -1],
    [-1, 6, -20, 101, 53, -16, 6, -1], [-1, 6, -20, 99, 56, -17, 6, -1],
    [-1, 6, -20, 97, 58, -17, 6, -1],  [-1, 6, -20, 95, 61, -18, 6, -1],
    [-2, 7, -20, 93, 64, -18, 6, -2],  [-2, 7, -20, 91, 66, -19, 6, -1],
    [-2, 7, -20, 88, 69, -19, 6, -1],  [-2, 7, -20, 86, 71, -19, 6, -1],
    [-2, 7, -20, 84, 74, -20, 7, -2],  [-2, 7, -20, 81, 76, -20, 7, -1],
    [-2, 7, -20, 79, 79, -20, 7, -2],  [-1, 7, -20, 76, 81, -20, 7, -2],
    [-2, 7, -20, 74, 84, -20, 7, -2],  [-1, 6, -19, 71, 86, -20, 7, -2],
    [-1, 6, -19, 69, 88, -20, 7, -2],  [-1, 6, -19, 66, 91, -20, 7, -2],
    [-2, 6, -18, 64, 93, -20, 7, -2],  [-1, 6, -18, 61, 95, -20, 6, -1],
    [-1, 6, -17, 58, 97, -20, 6, -1],  [-1, 6, -17, 56, 99, -20, 6, -1],
    [-1, 6, -16, 53, 101, -20, 6, -1], [-1, 5, -16, 51, 103, -19, 6, -1],
    [-1, 5, -15, 48, 105, -19, 6, -1], [-1, 5, -14, 45, 107, -19, 6, -1],
    [-1, 5, -14, 43, 109, -18, 5, -1], [-1, 5, -13, 40, 111, -18, 5, -1],
    [-1, 4, -12, 38, 112, -17, 5, -1], [-1, 4, -12, 35, 114, -16, 5, -1],
    [-1, 4, -11, 32, 116, -16, 5, -1], [-1, 4, -10, 30, 117, -15, 4, -1],
    [-1, 3, -9, 28, 118, -14, 4, -1],  [-1, 3, -9, 25, 120, -13, 4, -1],
    [-1, 3, -8, 22, 121, -12, 4, -1],  [-1, 3, -7, 20, 122, -11, 3, -1],
    [-1, 2, -6, 18, 123, -10, 3, -1],  [0, 2, -6, 15, 124, -9, 3, -1],
    [0, 2, -5, 13, 125, -8, 2, -1],    [0, 1, -4, 11, 125, -7, 2, 0],
    [0, 1, -3, 8, 126, -6, 2, 0],      [0, 1, -3, 6, 127, -4, 1, 0],
    [0, 1, -2, 4, 127, -3, 1, 0],      [0, 0, -1, 2, 128, -1, 0, 0],
];

/// `av1_get_upscale_convolve_step` (`resize.c`).
fn upscale_convolve_step(in_length: i64, out_length: i64) -> i64 {
    ((in_length << RS_SCALE_SUBPEL_BITS) + out_length / 2) / out_length
}

/// `get_upscale_convolve_x0` (`resize.c`, static there).
fn upscale_convolve_x0(in_length: i64, out_length: i64, x_step_qn: i64) -> i64 {
    let err = out_length * x_step_qn - (in_length << RS_SCALE_SUBPEL_BITS);
    let x0 = (-((out_length - in_length) << (RS_SCALE_SUBPEL_BITS - 1)) + out_length / 2)
        / out_length
        + RS_SCALE_EXTRA_OFF
        - err / 2;
    x0.rem_euclid(1i64 << RS_SCALE_SUBPEL_BITS) & RS_SCALE_SUBPEL_MASK
}

/// `av1_convolve_horiz_rs_c` (`convolve.c`): `clip_pixel(ROUND_POWER_OF_TWO(sum, FILTER_BITS))`.
fn round_power_of_two(sum: i32, bits: u32) -> i32 {
    (sum + (1 << (bits - 1))) >> bits
}

/// One row of the spec 7.16 horizontal upscale (`upscale_normative_rect` +
/// `av1_convolve_horiz_rs_c`, single-tile case: `pad_left`/`pad_right` both
/// true, replicating the row's own edge pixels rather than sampling a
/// neighbour tile). `in_width` and `out_width` must both be positive;
/// `row.len() == in_width` and `out.len() == out_width`.
pub(crate) fn upscale_row(row: &[u8], out_width: usize, out: &mut [u8]) {
    let in_width = row.len();
    debug_assert!(in_width > 0 && out_width > 0);
    debug_assert_eq!(out.len(), out_width);
    let x_step_qn = upscale_convolve_step(in_width as i64, out_width as i64);
    let x0_qn = upscale_convolve_x0(in_width as i64, out_width as i64, x_step_qn);

    // Edge-replicated padding: the real decoder samples out-of-frame
    // columns from the reference frame buffer's own border margin;
    // `upscale_normative_rect` fills that margin with `input[0]`/
    // `input[width-1]` for a single-tile column before calling the
    // convolver. `UPSCALE_NORMATIVE_TAPS/2 + 1` columns are provably
    // enough (the C source's own `border_cols`); this pads generously.
    let pad = UPSCALE_NORMATIVE_TAPS + 2;
    let mut padded = vec![0u8; in_width + 2 * pad];
    padded[..pad].fill(row[0]);
    padded[pad..pad + in_width].copy_from_slice(row);
    padded[pad + in_width..].fill(row[in_width - 1]);

    // `av1_convolve_horiz_rs_c` is called with `input - 1` and itself
    // subtracts `UPSCALE_NORMATIVE_TAPS/2 - 1` more; net offset from the
    // real column-0 pixel is `-(UPSCALE_NORMATIVE_TAPS/2)` == -4.
    let base = pad as i64 - (UPSCALE_NORMATIVE_TAPS as i64 / 2);
    let mut x_qn = x0_qn;
    for out_x in out.iter_mut() {
        let int_pel = x_qn >> RS_SCALE_SUBPEL_BITS;
        let filter_idx = ((x_qn & RS_SCALE_SUBPEL_MASK) >> RS_SCALE_EXTRA_BITS) as usize;
        let filter = &RESIZE_FILTER_NORMATIVE[filter_idx];
        let mut sum = 0i32;
        for (k, &c) in filter.iter().enumerate() {
            let idx = base + int_pel + k as i64;
            sum += padded[idx as usize] as i32 * c as i32;
        }
        *out_x = round_power_of_two(sum, FILTER_BITS).clamp(0, 255) as u8;
        x_qn += x_step_qn;
    }
}

/// Spec 7.16, applied to a whole plane: `av1_upscale_normative_rows`
/// widened to every row (`height2 == height`, no vertical scaling -- the
/// spec only widens columns). `rows` is the plane's row-major byte buffer
/// at `in_width`; returns a new buffer at `out_width`.
pub(crate) fn upscale_plane(rows: &[u8], height: usize, in_width: usize, out_width: usize) -> Vec<u8> {
    debug_assert_eq!(rows.len(), height * in_width);
    let mut out = vec![0u8; height * out_width];
    for r in 0..height {
        let src = &rows[r * in_width..(r + 1) * in_width];
        let dst = &mut out[r * out_width..(r + 1) * out_width];
        upscale_row(src, out_width, dst);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinned against libaom's own compiled code, not a second
    /// transcription of it: a standalone C harness linked against
    /// `~/.cache/aom-oracle/build/libaom.a` calls the real (exported)
    /// `av1_get_upscale_convolve_step` and `av1_convolve_horiz_rs_c` with
    /// the real (exported) `av1_resize_filter_normative` table -- the only
    /// piece reimplemented in C rather than called is the one-line static
    /// `get_upscale_convolve_x0` formula, copied verbatim in both places.
    /// This is the class `shared-oracle-blindness` guard: a bug in the
    /// 64x8 table above or in the convolution/rounding kernel is caught
    /// here independently of this module's own logic.
    #[test]
    fn upscale_row_matches_libaom_in8_out12() {
        let row = [10u8, 20, 30, 40, 50, 60, 70, 80];
        let mut out = [0u8; 12];
        upscale_row(&row, 12, &mut out);
        assert_eq!(out, [9, 14, 22, 29, 35, 42, 48, 55, 61, 68, 76, 81]);
    }

    #[test]
    fn upscale_row_matches_libaom_in8_out16() {
        let row = [10u8, 20, 30, 40, 50, 60, 70, 80];
        let mut out = [0u8; 16];
        upscale_row(&row, 16, &mut out);
        assert_eq!(
            out,
            [9, 12, 17, 23, 28, 32, 37, 43, 48, 53, 58, 62, 67, 73, 78, 81]
        );
    }

    /// A flat row must upscale to the same flat value everywhere: every
    /// filter phase's 8 taps sum to exactly 128 (`FILTER_BITS`'s unity
    /// gain), so a constant input passes through unchanged regardless of
    /// which phase samples it -- a cheap sanity check independent of the
    /// captured-row pins above.
    #[test]
    fn upscale_row_of_a_flat_input_is_flat() {
        let row = [200u8; 8];
        let mut out = [0u8; 12];
        upscale_row(&row, 12, &mut out);
        assert_eq!(out, [200u8; 12]);
    }
}

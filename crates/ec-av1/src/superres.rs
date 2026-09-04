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
/// `bit_depth` only picks the output clamp (`clip_pixel`/`clip_pixel_highbd`
/// in libaom's `av1_convolve_horiz_rs_c`); the filter, its `Round2` and the
/// edge padding are bit-depth independent (spec 7.16).
pub(crate) fn upscale_row(
    row: &[u16],
    real_right_margin: &[u16],
    out_width: usize,
    out: &mut [u16],
    bit_depth: u32,
) {
    let in_width = row.len();
    debug_assert!(in_width > 0 && out_width > 0);
    debug_assert_eq!(out.len(), out_width);
    let x_step_qn = upscale_convolve_step(in_width as i64, out_width as i64);
    let x0_qn = upscale_convolve_x0(in_width as i64, out_width as i64, x_step_qn);

    // Right-edge padding: r1 assumed a pure edge-replicate of `row`'s own
    // last column (matching `upscale_normative_rect`'s single-tile
    // in-memory fill), but the real decoder's reconstructed buffer already
    // holds genuine decoded samples past `frame_width` out to the
    // mi-aligned `true_width` (the coding block straddling the frame edge)
    // -- libaom's border extension replicates from THAT last real column,
    // not from `frame_width - 1`. `real_right_margin` (from
    // `decode::take_last_frame_wide_margin`) supplies those real
    // pixels first; only once it runs out (or is empty, e.g. `fw` was
    // already mi-aligned) does this fall back to r1's replicate. Pinned
    // column-by-column against real libaom via `scripts/superres-pin-
    // harness.c`'s `row6-realedgeval` case (r3).
    let pad = UPSCALE_NORMATIVE_TAPS + 2;
    let mut padded = vec![0u16; in_width + 2 * pad];
    padded[..pad].fill(row[0]);
    padded[pad..pad + in_width].copy_from_slice(row);
    let real_n = real_right_margin.len().min(pad);
    padded[pad + in_width..pad + in_width + real_n].copy_from_slice(&real_right_margin[..real_n]);
    let replicate_from = real_right_margin.last().copied().unwrap_or(row[in_width - 1]);
    padded[pad + in_width + real_n..].fill(replicate_from);

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
        *out_x = round_power_of_two(sum, FILTER_BITS).clamp(0, (1 << bit_depth) - 1) as u16;
        x_qn += x_step_qn;
    }
}

/// Every row of one decoded plane, upscaled by spec 7.16 from `in_w` to
/// `out_w` (`av1_upscale_normative_rows`; the process is horizontal-only,
/// so the row count is unchanged). `data`/`stride` are the decoder's own
/// reconstruction buffer, whose columns `[in_w, true_w)` hold the real
/// decoded samples of the coding block straddling the frame edge -- libaom's
/// border extension replicates from that last real column, so they are
/// handed to [`upscale_row`] as its right margin (see its doc).
pub(crate) fn upscale_plane_strided(
    data: &[u16],
    stride: usize,
    in_w: usize,
    in_h: usize,
    true_w: usize,
    out_w: usize,
    bit_depth: u32,
) -> Vec<u16> {
    let mut out = vec![0u16; in_h * out_w];
    for r in 0..in_h {
        let row = &data[r * stride..r * stride + in_w];
        let margin = &data[r * stride + in_w..r * stride + true_w.max(in_w)];
        upscale_row(row, margin, out_w, &mut out[r * out_w..(r + 1) * out_w], bit_depth);
    }
    out
}

thread_local! {
    /// Firing count for the superres gate (class `gate-blind-to-feature`):
    /// how many whole pictures actually ran through [`upscale_plane_strided`].
    static SUPERRES_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`SUPERRES_HITS`].
#[allow(dead_code)]
pub(crate) fn superres_hits() -> usize {
    SUPERRES_HITS.with(|c| c.get())
}

/// Counts one whole upscaled picture (all three planes) for [`SUPERRES_HITS`].
pub(crate) fn note_upscaled_picture() {
    SUPERRES_HITS.with(|c| c.set(c.get() + 1));
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
        let row = [10u16, 20, 30, 40, 50, 60, 70, 80];
        let mut out = [0u16; 12];
        upscale_row(&row, &[], 12, &mut out, 8);
        assert_eq!(out, [9, 14, 22, 29, 35, 42, 48, 55, 61, 68, 76, 81]);
    }

    #[test]
    fn upscale_row_matches_libaom_in8_out16() {
        let row = [10u16, 20, 30, 40, 50, 60, 70, 80];
        let mut out = [0u16; 16];
        upscale_row(&row, &[], 16, &mut out, 8);
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
        let row = [200u16; 8];
        let mut out = [0u16; 12];
        upscale_row(&row, &[], 12, &mut out, 8);
        assert_eq!(out, [200u16; 12]);
    }

    /// r3: the real 43->64 failing case, pinned against real libaom
    /// including the right-edge margin (`scripts/superres-pin-harness.c`'s
    /// `row6-realedgeval` case -- input row + real trailing decoded pixel
    /// 140, not a replicate of the frame-edge pixel 141). Column 62 (0
    /// indexed) is exactly the pixel this gate mismatched by 1 before the
    /// margin fix.
    #[test]
    fn upscale_row_with_real_margin_matches_libaom_in43_out64() {
        let row: [u16; 43] = [
            102, 102, 103, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116,
            117, 118, 119, 120, 121, 122, 123, 125, 126, 127, 127, 128, 129, 130, 130, 131, 131,
            133, 134, 136, 137, 138, 139, 140, 140, 141,
        ];
        let real_margin = [140u16];
        let mut out = [0u16; 64];
        upscale_row(&row, &real_margin, 64, &mut out, 8);
        assert_eq!(out[62], 141, "column 62 must match libaom, not the old replicate-padded 140");
    }
}

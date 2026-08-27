//! Sub-pel motion compensation, spec 7.11.3 ("Motion vector scaling process"
//! through "Block inter predictor process"), the REGULAR (`EIGHTTAP`) filter
//! only, 8-bit samples only.
//!
//! A block's inter prediction is separable: an 8-tap horizontal pass over the
//! reference, rounded down by `InterRound0`, feeds an 8-tap vertical pass
//! over that intermediate, rounded down by `InterRound1`. Both fractions run
//! in 1/16-pel steps -- luma motion vectors are stored at 1/8-pel and always
//! land on an even step; chroma vectors, once scaled for subsampling, can
//! land on any of the 16.

/// `Round2` (spec 4.7): halves round up.
fn round2(value: i32, shift: u32) -> i32 {
    if shift == 0 {
        value
    } else {
        (value + (1 << (shift - 1))) >> shift
    }
}

/// `InterRound0` (spec 7.11.3.2), 8-bit, non-compound: the shift the
/// horizontal pass's sum is brought down by before it is stored as the
/// intermediate.
const INTER_ROUND_0: u32 = 3;
/// `InterRound1` (spec 7.11.3.2), 8-bit, non-compound: the shift the vertical
/// pass's sum is brought down by to land back on an 8-bit sample. Composed
/// with `INTER_ROUND_0` the two shifts are the filter's own gain (each pass
/// sums to 128, `128 * 128 == 1 << 14 == 1 << (INTER_ROUND_0 + INTER_ROUND_1)`)
/// -- so an identity or DC input comes back exactly, by construction of the
/// table rather than by a fitted constant.
const INTER_ROUND_1: u32 = 11;

/// `Subpel_Filters[EIGHTTAP]` (spec 7.11.3.3): 16 rows, one per 1/16-pel
/// fraction, each an 8-tap kernel centred so tap index 3 lands on the integer
/// sample (fraction 0 is the identity tap `[.., 128, ..]`).
const SUBPEL_FILTERS: [[i32; 8]; 16] = [
    [0, 0, 0, 128, 0, 0, 0, 0],
    [0, 2, -6, 126, 8, -2, 0, 0],
    [0, 2, -10, 122, 18, -4, 0, 0],
    [0, 2, -12, 116, 28, -8, 2, 0],
    [0, 2, -14, 110, 38, -10, 2, 0],
    [0, 2, -14, 102, 48, -12, 2, 0],
    [0, 2, -16, 94, 58, -12, 2, 0],
    [0, 2, -14, 84, 66, -12, 2, 0],
    [0, 2, -14, 76, 76, -14, 2, 0],
    [0, 2, -12, 66, 84, -14, 2, 0],
    [0, 2, -12, 58, 94, -16, 2, 0],
    [0, 2, -12, 48, 102, -14, 2, 0],
    [0, 2, -10, 38, 110, -14, 2, 0],
    [0, 2, -8, 28, 116, -12, 2, 0],
    [0, 0, -4, 18, 122, -10, 2, 0],
    [0, 0, -2, 8, 126, -6, 2, 0],
];

/// A reference sample at `(x, y)`, clamped to the frame's true (unpadded)
/// extent -- `true_width`/`true_height`, which can be narrower than
/// `stride` -- rather than read out of range (spec 7.11.3.4's `Clip3`
/// against the frame's edges): a motion vector that points past the true
/// edge repeats the true edge's sample instead of panicking or picking up
/// whatever this encoder's own uncoded padding columns/rows hold.
#[allow(clippy::too_many_arguments)]
fn sample(
    reference: &[u8],
    stride: usize,
    true_width: usize,
    true_height: usize,
    x: i32,
    y: i32,
) -> i32 {
    let cx = x.clamp(0, true_width as i32 - 1) as usize;
    let cy = y.clamp(0, true_height as i32 - 1) as usize;
    i32::from(reference[cy * stride + cx])
}

/// Predicts one `block_w * block_h` block into `dst`, row-major, from
/// `reference` (row-major, stride `stride`, true content
/// `true_width` x `true_height` -- a reference frame's decoded picture
/// buffer can be wider/taller than the true frame it holds, when this
/// encoder pads a picture to a whole number of superblocks; a spec decoder
/// never codes syntax for those extra columns/rows, so motion compensation
/// must clamp to the true extent, not the buffer's own stride).
///
/// `x_q4` / `y_q4` are the block's top-left position in the reference, in
/// 1/16-pel units (an integer MV has its low 4 bits zero); this function
/// splits each into its whole-sample part and its 0..16 fraction itself, so
/// the caller need not pre-split a motion vector.
///
/// # Panics
/// Panics when `dst` is not `block_w * block_h` long, or the reference is
/// empty.
#[allow(clippy::too_many_arguments)] // one reference plane, one position, one block shape
pub fn predict(
    reference: &[u8],
    stride: usize,
    true_width: usize,
    true_height: usize,
    x_q4: i32,
    y_q4: i32,
    block_w: usize,
    block_h: usize,
    dst: &mut [u8],
) {
    assert_eq!(dst.len(), block_w * block_h, "the destination is the block");
    assert!(!reference.is_empty(), "a reference plane has samples");

    #[cfg(test)]
    let stage_t = std::time::Instant::now();

    let x0 = x_q4.div_euclid(16);
    let xfrac = x_q4.rem_euclid(16) as usize;
    let y0 = y_q4.div_euclid(16);
    let yfrac = y_q4.rem_euclid(16) as usize;

    let h_filter = &SUBPEL_FILTERS[xfrac];
    let v_filter = &SUBPEL_FILTERS[yfrac];

    // The vertical pass reads 3 rows above and 4 below the block, so the
    // horizontal pass must produce that many extra intermediate rows.
    let rows = block_h + 7;
    let mut intermediate = vec![0i32; rows * block_w];
    for r in 0..rows {
        let y = y0 - 3 + r as i32;
        for c in 0..block_w {
            let mut sum = 0;
            for (t, &tap) in h_filter.iter().enumerate() {
                let x = x0 + c as i32 + t as i32 - 3;
                sum += tap * sample(reference, stride, true_width, true_height, x, y);
            }
            intermediate[r * block_w + c] = round2(sum, INTER_ROUND_0);
        }
    }

    for row in 0..block_h {
        for col in 0..block_w {
            let mut sum = 0;
            for (t, &tap) in v_filter.iter().enumerate() {
                sum += tap * intermediate[(row + t) * block_w + col];
            }
            dst[row * block_w + col] = round2(sum, INTER_ROUND_1).clamp(0, 255) as u8;
        }
    }

    #[cfg(test)]
    crate::encode::stage_add(1, stage_t.elapsed());
}

#[cfg(test)]
mod tests {
    use super::predict;

    #[test]
    fn integer_mv_is_identity() {
        let width = 12;
        let height = 12;
        let reference: Vec<u8> = (0..width * height).map(|i| (i * 7 % 251) as u8).collect();
        let mut dst = vec![0u8; 6 * 6];
        predict(
            &reference,
            width,
            width,
            height,
            3 * 16,
            4 * 16,
            6,
            6,
            &mut dst,
        );
        for row in 0..6 {
            for col in 0..6 {
                assert_eq!(
                    dst[row * 6 + col],
                    reference[(4 + row) * width + 3 + col],
                    "integer MV must reproduce the reference block byte-for-byte"
                );
            }
        }
    }

    #[test]
    fn half_pel_reproduces_the_ramp_midpoint() {
        // Step 2 so every half-pel position lands on an exact integer.
        let width = 16;
        let height = 16;
        let reference: Vec<u8> = (0..width * height)
            .map(|i| (2 * (i % width)) as u8)
            .collect();
        let mut dst = vec![0u8; 4 * 4];
        // Half-pel in x only: fraction 8/16, integer part at column 5.
        predict(
            &reference,
            width,
            width,
            height,
            5 * 16 + 8,
            5 * 16,
            4,
            4,
            &mut dst,
        );
        for row in 0..4 {
            for col in 0..4 {
                let midpoint = 2 * (5 + col) + 1; // halfway between col and col+1
                assert_eq!(
                    dst[row * 4 + col] as i32,
                    midpoint as i32,
                    "half-pel MV over a linear ramp must land exactly on the midpoint"
                );
            }
        }
    }

    #[test]
    fn constant_plane_has_unit_dc_gain_at_every_subpel_position() {
        let width = 20;
        let height = 20;
        let reference = vec![142u8; width * height];
        for yfrac in 0..16 {
            for xfrac in 0..16 {
                let mut dst = vec![0u8; 5 * 5];
                predict(
                    &reference,
                    width,
                    width,
                    height,
                    5 * 16 + xfrac,
                    5 * 16 + yfrac,
                    5,
                    5,
                    &mut dst,
                );
                assert!(
                    dst.iter().all(|&v| v == 142),
                    "DC gain must be exactly 1 at fraction ({xfrac}, {yfrac}), got {dst:?}"
                );
            }
        }
    }

    #[test]
    fn horizontal_and_vertical_only_filtering_differ() {
        // A plane linear in both x and y with different, even coefficients so
        // each axis's half-pel interpolation lands on an exact integer: this
        // pins the actual value each pass produces, not just that the two
        // differ, so swapping which pass reads which axis is caught even if
        // it happened to produce two merely-different numbers.
        let width = 10;
        let height = 10;
        let reference: Vec<u8> = (0..height)
            .flat_map(|y| (0..width).map(move |x| (20 * x + 4 * y) as u8))
            .collect();

        let mut horiz = vec![0u8; 4 * 4];
        predict(
            &reference,
            width,
            width,
            height,
            3 * 16 + 8,
            3 * 16,
            4,
            4,
            &mut horiz,
        );
        let mut vert = vec![0u8; 4 * 4];
        predict(
            &reference,
            width,
            width,
            height,
            3 * 16,
            3 * 16 + 8,
            4,
            4,
            &mut vert,
        );

        for row in 0..4 {
            for col in 0..4 {
                // Horizontal half-pel: x interpolates to x0+col+0.5, y stays put.
                assert_eq!(
                    horiz[row * 4 + col] as i32,
                    20 * (3 + col as i32) + 10 + 4 * (3 + row as i32),
                    "horizontal-only MV must interpolate along x only"
                );
                // Vertical half-pel: y interpolates to y0+row+0.5, x stays put.
                assert_eq!(
                    vert[row * 4 + col] as i32,
                    20 * (3 + col as i32) + 4 * (3 + row as i32) + 2,
                    "vertical-only MV must interpolate along y only"
                );
            }
        }
        assert_ne!(
            horiz, vert,
            "horizontal-only and vertical-only sub-pel MVs must give different outputs"
        );
    }

    #[test]
    fn mv_past_the_edge_clamps_instead_of_panicking() {
        let width = 8;
        let height = 8;
        let reference: Vec<u8> = (0..width * height).map(|i| (i * 3 % 200) as u8).collect();
        let mut dst = vec![0u8; 4 * 4];
        // Whole-sample position far above and to the left of the plane, with
        // an odd fraction so both filter passes exercise the clamp.
        predict(
            &reference,
            width,
            width,
            height,
            -50 * 16 + 5,
            -50 * 16 + 5,
            4,
            4,
            &mut dst,
        );
        let expected = reference[0]; // the whole plane clamps to the top-left corner
        assert!(
            dst.iter().all(|&v| v == expected),
            "an MV past the top-left edge must clamp to the corner sample, got {dst:?}"
        );

        let mut dst2 = vec![0u8; 4 * 4];
        predict(
            &reference,
            width,
            width,
            height,
            50 * 16 + 5,
            50 * 16 + 5,
            4,
            4,
            &mut dst2,
        );
        let expected2 = reference[height * width - 1]; // clamps to the bottom-right corner
        assert!(
            dst2.iter().all(|&v| v == expected2),
            "an MV past the bottom-right edge must clamp to the corner sample, got {dst2:?}"
        );
    }
}

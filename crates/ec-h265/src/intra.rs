//! Intra sample prediction, all 35 modes (8.4.2 - 8.4.4.2).
//!
//! Reference samples arrive as `corner` (`p[-1][-1]`), `top` (`p[x][-1]`) and
//! `left` (`p[-1][y]`), each up to `2 * nTbS` long, with a bitmask saying which
//! of them are real. Substitution, smoothing and the mode itself are then
//! exactly the decoder's, because the encoder reconstructs with the same code:
//! a prediction that differs from the decoder's by one sample is a drift that
//! the picture hash would catch but the eye would not.

/// Reference samples for one transform block, before or after filtering.
#[derive(Clone, Copy)]
pub struct Refs {
    /// `p[-1][-1]`.
    pub corner: u8,
    /// `p[x][-1]`, `x = 0..2 * nTbS - 1`.
    pub top: [u8; 64],
    /// `p[-1][y]`, `y = 0..2 * nTbS - 1`.
    pub left: [u8; 64],
}

impl Default for Refs {
    fn default() -> Self {
        Refs {
            corner: 128,
            top: [128; 64],
            left: [128; 64],
        }
    }
}

/// Which reference samples are real, one bit per sample.
#[derive(Clone, Copy, Default)]
pub struct Availability {
    /// Whether `p[-1][-1]` is available.
    pub corner: bool,
    /// Bit `x` set when `p[x][-1]` is available.
    pub top: u64,
    /// Bit `y` set when `p[-1][y]` is available.
    pub left: u64,
}

impl Availability {
    /// True when nothing at all is available and the whole block predicts flat.
    pub fn is_empty(&self, n: usize) -> bool {
        let mask = if 2 * n >= 64 {
            u64::MAX
        } else {
            (1u64 << (2 * n)) - 1
        };
        !self.corner && self.top & mask == 0 && self.left & mask == 0
    }
}

/// `intraPredAngle` per mode (Table 8-5), indexed by `predModeIntra`.
#[rustfmt::skip]
const ANGLE: [i32; 35] = [
    0, 0, 32, 26, 21, 17, 13, 9, 5, 2, 0, -2, -5, -9, -13, -17, -21, -26,
    -32, -26, -21, -17, -13, -9, -5, -2, 0, 2, 5, 9, 13, 17, 21, 26, 32,
];

/// `invAngle` per mode (Table 8-6); only modes 11..25 use it.
#[rustfmt::skip]
const INV_ANGLE: [i32; 35] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    -4096, -1638, -910, -630, -482, -390, -315, -256, -315, -390, -482, -630, -910, -1638, -4096,
    0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// Reference sample substitution (8.4.4.2.2): fill the holes from the nearest
/// available sample, walking up the left edge and then along the top.
pub fn substitute(refs: &mut Refs, avail: &Availability, n: usize, bit_depth: u32) {
    let span = 2 * n;
    if avail.is_empty(n) {
        let mid = 1u8 << (bit_depth - 1);
        refs.corner = mid;
        refs.top[..span].fill(mid);
        refs.left[..span].fill(mid);
        return;
    }
    // Step 1: the bottom-left sample, from the first available going up the
    // left edge, then across the top.
    if avail.left & (1 << (span - 1)) == 0 {
        let mut value = None;
        for y in (0..span).rev() {
            if avail.left & (1 << y) != 0 {
                value = Some(refs.left[y]);
                break;
            }
        }
        if value.is_none() && avail.corner {
            value = Some(refs.corner);
        }
        if value.is_none() {
            for x in 0..span {
                if avail.top & (1 << x) != 0 {
                    value = Some(refs.top[x]);
                    break;
                }
            }
        }
        refs.left[span - 1] = value.unwrap_or(1 << (bit_depth - 1));
    }
    // Step 2: up the left edge, each hole taking the sample below it.
    for y in (0..span - 1).rev() {
        if avail.left & (1 << y) == 0 {
            refs.left[y] = refs.left[y + 1];
        }
    }
    if !avail.corner {
        refs.corner = refs.left[0];
    }
    // Step 3: along the top, each hole taking the sample to its left.
    for x in 0..span {
        if avail.top & (1 << x) == 0 {
            refs.top[x] = if x == 0 { refs.corner } else { refs.top[x - 1] };
        }
    }
}

/// `filterFlag` of 8.4.4.2.3.
fn filter_flag(mode: u8, n: usize, is_luma: bool) -> bool {
    if !is_luma || mode == 1 || n == 4 {
        return false;
    }
    let min_dist = (i32::from(mode) - 26)
        .abs()
        .min((i32::from(mode) - 10).abs());
    let threshold = match n {
        8 => 7,
        16 => 1,
        _ => 0,
    };
    min_dist > threshold
}

/// Neighbouring sample filtering (8.4.4.2.3), including the strong bilinear
/// path for flat 32x32 luma blocks.
pub fn filter_refs(refs: &Refs, mode: u8, n: usize, is_luma: bool, strong_smoothing: bool) -> Refs {
    if !filter_flag(mode, n, is_luma) {
        return *refs;
    }
    let span = 2 * n;
    let mut out = *refs;
    let bi_int = strong_smoothing
        && is_luma
        && n == 32
        && (i32::from(refs.corner) + i32::from(refs.top[span - 1])
            - 2 * i32::from(refs.top[n - 1]))
        .abs()
            < (1 << 3)
        && (i32::from(refs.corner) + i32::from(refs.left[span - 1])
            - 2 * i32::from(refs.left[n - 1]))
        .abs()
            < (1 << 3);
    if bi_int {
        let corner = i32::from(refs.corner);
        let bottom_left = i32::from(refs.left[63]);
        let top_right = i32::from(refs.top[63]);
        for y in 0..63 {
            out.left[y] =
                (((63 - y as i32) * corner + (y as i32 + 1) * bottom_left + 32) >> 6) as u8;
        }
        out.left[63] = refs.left[63];
        for x in 0..63 {
            out.top[x] = (((63 - x as i32) * corner + (x as i32 + 1) * top_right + 32) >> 6) as u8;
        }
        out.top[63] = refs.top[63];
        return out;
    }
    out.corner =
        ((u32::from(refs.left[0]) + 2 * u32::from(refs.corner) + u32::from(refs.top[0]) + 2) >> 2)
            as u8;
    for y in 0..span - 1 {
        let below = u32::from(refs.left[y + 1]);
        let here = u32::from(refs.left[y]);
        let above = if y == 0 {
            u32::from(refs.corner)
        } else {
            u32::from(refs.left[y - 1])
        };
        out.left[y] = ((below + 2 * here + above + 2) >> 2) as u8;
    }
    for x in 0..span - 1 {
        let right = u32::from(refs.top[x + 1]);
        let here = u32::from(refs.top[x]);
        let left = if x == 0 {
            u32::from(refs.corner)
        } else {
            u32::from(refs.top[x - 1])
        };
        out.top[x] = ((right + 2 * here + left + 2) >> 2) as u8;
    }
    out
}

/// Predict an `n x n` block into `out` (row major), for `mode` 0..=34.
///
/// `refs` must already be substituted; filtering is applied here so that a
/// caller trying 35 modes filters once per mode rather than per sample.
pub fn predict(
    refs: &Refs,
    mode: u8,
    n: usize,
    is_luma: bool,
    strong_smoothing: bool,
    out: &mut [u8],
) {
    let filtered = filter_refs(refs, mode, n, is_luma, strong_smoothing);
    match mode {
        0 => predict_planar(&filtered, n, out),
        1 => predict_dc(&filtered, n, is_luma, out),
        _ => predict_angular(&filtered, mode, n, is_luma, out),
    }
}

fn predict_planar(refs: &Refs, n: usize, out: &mut [u8]) {
    let log2n = n.trailing_zeros();
    let top_right = i32::from(refs.top[n]);
    let bottom_left = i32::from(refs.left[n]);
    for y in 0..n {
        let left = i32::from(refs.left[y]);
        for x in 0..n {
            let value = (n as i32 - 1 - x as i32) * left
                + (x as i32 + 1) * top_right
                + (n as i32 - 1 - y as i32) * i32::from(refs.top[x])
                + (y as i32 + 1) * bottom_left
                + n as i32;
            out[y * n + x] = (value >> (log2n + 1)) as u8;
        }
    }
}

fn predict_dc(refs: &Refs, n: usize, is_luma: bool, out: &mut [u8]) {
    let mut sum = n as u32;
    for i in 0..n {
        sum += u32::from(refs.top[i]) + u32::from(refs.left[i]);
    }
    let dc = (sum >> (n.trailing_zeros() + 1)) as i32;
    for value in out[..n * n].iter_mut() {
        *value = dc as u8;
    }
    if is_luma && n < 32 {
        out[0] = ((i32::from(refs.left[0]) + 2 * dc + i32::from(refs.top[0]) + 2) >> 2) as u8;
        for (x, sample) in out[1..n].iter_mut().enumerate() {
            *sample = ((i32::from(refs.top[x + 1]) + 3 * dc + 2) >> 2) as u8;
        }
        for y in 1..n {
            out[y * n] = ((i32::from(refs.left[y]) + 3 * dc + 2) >> 2) as u8;
        }
    }
}

fn predict_angular(refs: &Refs, mode: u8, n: usize, is_luma: bool, out: &mut [u8]) {
    let angle = ANGLE[mode as usize];
    let inv_angle = INV_ANGLE[mode as usize];
    // ref[] is indexed from -nTbS to 2 * nTbS; store it shifted by 32.
    const BIAS: usize = 32;
    let mut reference = [0i32; 32 + 65];
    let (main, side): (&[u8; 64], &[u8; 64]) = if mode >= 18 {
        (&refs.top, &refs.left)
    } else {
        (&refs.left, &refs.top)
    };
    reference[BIAS] = i32::from(refs.corner);
    for x in 1..=n {
        reference[BIAS + x] = i32::from(main[x - 1]);
    }
    if angle < 0 {
        let last = (n as i32 * angle) >> 5;
        if last < -1 {
            for x in last..=-1 {
                let idx = ((x * inv_angle + 128) >> 8) - 1;
                reference[(BIAS as i32 + x) as usize] = if idx < 0 {
                    i32::from(refs.corner)
                } else {
                    i32::from(side[idx as usize])
                };
            }
        }
    } else {
        for x in n + 1..=2 * n {
            reference[BIAS + x] = i32::from(main[x - 1]);
        }
    }
    for y in 0..n {
        // The "row" index along the prediction direction: y for the vertical
        // modes, x for the horizontal ones, which is the whole difference
        // between the two halves of 8.4.4.2.6.
        for x in 0..n {
            let (along, across) = if mode >= 18 { (y, x) } else { (x, y) };
            let idx = ((along as i32 + 1) * angle) >> 5;
            let fact = ((along as i32 + 1) * angle) & 31;
            let base = (BIAS as i32 + across as i32 + idx + 1) as usize;
            let value = if fact != 0 {
                ((32 - fact) * reference[base] + fact * reference[base + 1] + 16) >> 5
            } else {
                reference[base]
            };
            out[y * n + x] = value as u8;
        }
    }
    // The vertical and horizontal modes carry a boundary filter on luma
    // blocks below 32x32 (8-60, 8-68).
    if is_luma && n < 32 {
        if mode == 26 {
            for y in 0..n {
                let value = i32::from(refs.top[0])
                    + ((i32::from(refs.left[y]) - i32::from(refs.corner)) >> 1);
                out[y * n] = value.clamp(0, 255) as u8;
            }
        } else if mode == 10 {
            for (x, sample) in out[..n].iter_mut().enumerate() {
                let value = i32::from(refs.left[0])
                    + ((i32::from(refs.top[x]) - i32::from(refs.corner)) >> 1);
                *sample = value.clamp(0, 255) as u8;
            }
        }
    }
}

/// The three most probable modes (8.4.2 step 3) from the left and above
/// neighbours' modes; `None` means the neighbour is unavailable or not intra.
pub fn mpm_list(left: Option<u8>, above: Option<u8>) -> [u8; 3] {
    let cand_a = left.unwrap_or(1);
    let cand_b = above.unwrap_or(1);
    if cand_a == cand_b {
        if cand_a < 2 {
            [0, 1, 26]
        } else {
            [
                cand_a,
                2 + ((u32::from(cand_a) + 29) % 32) as u8,
                2 + ((u32::from(cand_a) - 2 + 1) % 32) as u8,
            ]
        }
    } else if cand_a != 0 && cand_b != 0 {
        [cand_a, cand_b, 0]
    } else if cand_a != 1 && cand_b != 1 {
        [cand_a, cand_b, 1]
    } else {
        [cand_a, cand_b, 26]
    }
}

/// `scanIdx` for a residual block (7.4.9.11): mode-dependent for the small
/// luma and chroma blocks, diagonal everywhere else.
///
/// 0 = up-right diagonal, 1 = horizontal, 2 = vertical.
pub fn scan_index(mode: u8, log2_size: u32, is_luma: bool) -> usize {
    let mode_dependent = if is_luma {
        log2_size == 2 || log2_size == 3
    } else {
        log2_size == 2
    };
    if !mode_dependent {
        return 0;
    }
    if (6..=14).contains(&mode) {
        2
    } else if (22..=30).contains(&mode) {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_refs(value: u8, n: usize) -> (Refs, Availability) {
        let refs = Refs {
            corner: value,
            top: [value; 64],
            left: [value; 64],
        };
        let mask = if 2 * n >= 64 {
            u64::MAX
        } else {
            (1u64 << (2 * n)) - 1
        };
        (
            refs,
            Availability {
                corner: true,
                top: mask,
                left: mask,
            },
        )
    }

    #[test]
    fn every_mode_predicts_a_flat_block_flat() {
        // With every neighbour equal, all 35 modes must return that value —
        // including the boundary filters, which are differences of equals.
        for &n in &[4usize, 8, 16, 32] {
            let (refs, _) = full_refs(96, n);
            let mut out = vec![0u8; n * n];
            for mode in 0..35u8 {
                predict(&refs, mode, n, true, true, &mut out);
                assert!(
                    out.iter().all(|&v| v == 96),
                    "mode {mode} n {n}: {:?}",
                    &out[..8]
                );
            }
        }
    }

    #[test]
    fn substitution_fills_from_the_nearest_real_sample() {
        let n = 4;
        let mut refs = Refs {
            corner: 0,
            top: [0; 64],
            left: [0; 64],
        };
        refs.left[5] = 70;
        let avail = Availability {
            corner: false,
            top: 0,
            left: 1 << 5,
        };
        substitute(&mut refs, &avail, n, 8);
        // Everything takes 70: below it by the bottom-left rule, above it by
        // the walk up, then the corner and the whole top row.
        assert!(refs.left[..8].iter().all(|&v| v == 70));
        assert!(refs.top[..8].iter().all(|&v| v == 70));
        assert_eq!(refs.corner, 70);

        // Nothing available at all: mid-grey.
        let mut refs = Refs::default();
        substitute(&mut refs, &Availability::default(), n, 8);
        assert_eq!(refs.corner, 128);
        assert!(refs.top[..8].iter().all(|&v| v == 128));
    }

    #[test]
    fn vertical_and_horizontal_modes_copy_their_edge() {
        let n = 8;
        let mut refs = Refs::default();
        for i in 0..64 {
            refs.top[i] = (10 + i) as u8;
            refs.left[i] = (200 - i) as u8;
        }
        refs.corner = 100;
        let mut out = vec![0u8; n * n];
        // Mode 26 on chroma has no boundary filter: every row is the top row.
        predict(&refs, 26, n, false, false, &mut out);
        for y in 0..n {
            for x in 0..n {
                assert_eq!(out[y * n + x], refs.top[x], "mode 26 at {x},{y}");
            }
        }
        // Mode 10 likewise copies the left column across.
        predict(&refs, 10, n, false, false, &mut out);
        for y in 0..n {
            for x in 0..n {
                assert_eq!(out[y * n + x], refs.left[y], "mode 10 at {x},{y}");
            }
        }
    }

    #[test]
    fn diagonal_mode_2_walks_the_left_column() {
        // Mode 2 is the 45-degree "bottom-left" direction: angle 32 means one
        // whole sample of shift per row along the left reference.
        let n = 4;
        let mut refs = Refs::default();
        for i in 0..64 {
            refs.left[i] = i as u8;
        }
        let mut out = vec![0u8; n * n];
        predict(&refs, 2, n, false, false, &mut out);
        for y in 0..n {
            for x in 0..n {
                assert_eq!(out[y * n + x], (y + x + 1) as u8, "at {x},{y}");
            }
        }
    }

    #[test]
    fn mpm_list_follows_the_spec_cases() {
        assert_eq!(mpm_list(None, None), [0, 1, 26]);
        assert_eq!(mpm_list(Some(0), Some(0)), [0, 1, 26]);
        assert_eq!(mpm_list(Some(10), Some(10)), [10, 9, 11]);
        assert_eq!(mpm_list(Some(2), Some(2)), [2, 33, 3]);
        assert_eq!(mpm_list(Some(10), Some(26)), [10, 26, 0]);
        assert_eq!(mpm_list(Some(0), Some(26)), [0, 26, 1]);
        assert_eq!(mpm_list(Some(0), Some(1)), [0, 1, 26]);
    }

    #[test]
    fn scan_index_is_mode_dependent_only_for_small_blocks() {
        assert_eq!(scan_index(10, 2, true), 2);
        assert_eq!(scan_index(26, 3, true), 1);
        assert_eq!(scan_index(26, 4, true), 0);
        assert_eq!(scan_index(10, 2, false), 2);
        assert_eq!(scan_index(10, 3, false), 0);
        assert_eq!(scan_index(0, 2, true), 0);
    }
}

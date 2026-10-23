//! Intra sample prediction, all 35 modes (8.4.2 - 8.4.4.2).
//!
//! Reference samples arrive as `corner` (`p[-1][-1]`), `top` (`p[x][-1]`) and
//! `left` (`p[-1][y]`), each up to `2 * nTbS` long, with a bitmask saying which
//! of them are real. Substitution, smoothing and the mode itself are then
//! exactly the decoder's, because the encoder reconstructs with the same code:
//! a prediction that differs from the decoder's by one sample is a drift that
//! the picture hash would catch but the eye would not.
//!
//! The predictors are generic over the sample type: the encoder runs them on
//! 8-bit samples (`u8`, the default), the decoder on 16-bit storage for both
//! 8-bit and 10-bit pictures. The bit depth is an argument wherever the spec
//! uses `BitDepth`.

/// A sample value: `u8` for 8-bit pictures, `u16` for 10-bit ones.
pub trait Sample: Copy + Default + Into<i32> {
    /// The value `v`, which the caller has already clipped to the sample range.
    fn from_i32(v: i32) -> Self;
}

impl Sample for u8 {
    fn from_i32(v: i32) -> u8 {
        v as u8
    }
}

impl Sample for u16 {
    fn from_i32(v: i32) -> u16 {
        v as u16
    }
}

/// Reference samples for one transform block, before or after filtering.
#[derive(Clone, Copy)]
pub struct Refs<T: Sample = u8> {
    /// `p[-1][-1]`.
    pub corner: T,
    /// `p[x][-1]`, `x = 0..2 * nTbS - 1`.
    pub top: [T; 64],
    /// `p[-1][y]`, `y = 0..2 * nTbS - 1`.
    pub left: [T; 64],
}

impl<T: Sample> Default for Refs<T> {
    fn default() -> Self {
        Refs {
            corner: T::from_i32(128),
            top: [T::from_i32(128); 64],
            left: [T::from_i32(128); 64],
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
pub fn substitute<T: Sample>(refs: &mut Refs<T>, avail: &Availability, n: usize, bit_depth: u32) {
    let span = 2 * n;
    if avail.is_empty(n) {
        let mid = T::from_i32(1 << (bit_depth - 1));
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
        refs.left[span - 1] = value.unwrap_or(T::from_i32(1 << (bit_depth - 1)));
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
/// path for flat 32x32 luma blocks. `bit_depth` sets the flatness threshold
/// `1 << (BitDepthY - 5)`.
pub fn filter_refs<T: Sample>(
    refs: &Refs<T>,
    mode: u8,
    n: usize,
    is_luma: bool,
    strong_smoothing: bool,
    bit_depth: u32,
) -> Refs<T> {
    if !filter_flag(mode, n, is_luma) {
        return *refs;
    }
    let span = 2 * n;
    let mut out = *refs;
    let threshold = 1i32 << (bit_depth - 5);
    let c: i32 = refs.corner.into();
    let bi_int = strong_smoothing
        && is_luma
        && n == 32
        && (c + Into::<i32>::into(refs.top[span - 1]) - 2 * Into::<i32>::into(refs.top[n - 1]))
            .abs()
            < threshold
        && (c + Into::<i32>::into(refs.left[span - 1]) - 2 * Into::<i32>::into(refs.left[n - 1]))
            .abs()
            < threshold;
    if bi_int {
        let corner = c;
        let bottom_left: i32 = refs.left[63].into();
        let top_right: i32 = refs.top[63].into();
        for y in 0..63 {
            out.left[y] =
                T::from_i32(((63 - y as i32) * corner + (y as i32 + 1) * bottom_left + 32) >> 6);
        }
        out.left[63] = refs.left[63];
        for x in 0..63 {
            out.top[x] =
                T::from_i32(((63 - x as i32) * corner + (x as i32 + 1) * top_right + 32) >> 6);
        }
        out.top[63] = refs.top[63];
        return out;
    }
    let v = |s: T| -> i32 { s.into() };
    out.corner = T::from_i32((v(refs.left[0]) + 2 * c + v(refs.top[0]) + 2) >> 2);
    for y in 0..span - 1 {
        let below = v(refs.left[y + 1]);
        let here = v(refs.left[y]);
        let above = if y == 0 { c } else { v(refs.left[y - 1]) };
        out.left[y] = T::from_i32((below + 2 * here + above + 2) >> 2);
    }
    for x in 0..span - 1 {
        let right = v(refs.top[x + 1]);
        let here = v(refs.top[x]);
        let left = if x == 0 { c } else { v(refs.top[x - 1]) };
        out.top[x] = T::from_i32((right + 2 * here + left + 2) >> 2);
    }
    out
}

/// Predict an `n x n` block into `out` (row major), for `mode` 0..=34.
///
/// `refs` must already be substituted; filtering is applied here so that a
/// caller trying 35 modes filters once per mode rather than per sample.
pub fn predict<T: Sample>(
    refs: &Refs<T>,
    mode: u8,
    n: usize,
    is_luma: bool,
    strong_smoothing: bool,
    bit_depth: u32,
    out: &mut [T],
) {
    let flags = PredFlags {
        smooth: is_luma,
        strong: strong_smoothing,
        edge: is_luma,
        no_boundary: false,
    };
    predict_ext(refs, mode, n, flags, bit_depth, out);
}

/// Which of the component-dependent steps of intra prediction apply to a block
/// (8.4.4.2.3 to 8.4.4.2.6 with the range extension).
#[derive(Clone, Copy, Debug)]
pub struct PredFlags {
    /// The neighbouring sample filter may run: luma or 4:4:4 chroma, with
    /// `intra_smoothing_disabled_flag` off.
    pub smooth: bool,
    /// The strong bilinear filter may run (`strong_intra_smoothing_enabled_flag`, luma).
    pub strong: bool,
    /// The DC and horizontal/vertical edge filters may run (luma blocks below 32x32).
    pub edge: bool,
    /// `disableIntraBoundaryFilter`: implicit RDPCM on a transquant-bypass unit.
    pub no_boundary: bool,
}

/// [`predict`] with explicit [`PredFlags`].
pub fn predict_ext<T: Sample>(
    refs: &Refs<T>,
    mode: u8,
    n: usize,
    flags: PredFlags,
    bit_depth: u32,
    out: &mut [T],
) {
    if !filter_flag(mode, n, flags.smooth) {
        predict_filtered(refs, mode, n, flags, bit_depth, out);
        return;
    }
    let filtered = filter_refs(refs, mode, n, flags.smooth, flags.strong, bit_depth);
    predict_filtered(&filtered, mode, n, flags, bit_depth, out);
}

/// [`predict`] for a caller that predicts many modes from the same reference samples:
/// the smoothed copy (the same for every mode that smooths) is built once into `cache`
/// (set it to `None` when `refs` changes). Bit-identical to [`predict`].
#[allow(clippy::too_many_arguments)]
pub fn predict_cached<T: Sample>(
    refs: &Refs<T>,
    cache: &mut Option<Refs<T>>,
    mode: u8,
    n: usize,
    is_luma: bool,
    strong_smoothing: bool,
    bit_depth: u32,
    out: &mut [T],
) {
    let flags = PredFlags {
        smooth: is_luma,
        strong: strong_smoothing,
        edge: is_luma,
        no_boundary: false,
    };
    predict_cached_ext(refs, cache, mode, n, flags, bit_depth, out);
}

/// [`predict_cached`] with explicit [`PredFlags`]. The cache is keyed by the caller on the
/// reference samples and `flags.smooth`/`flags.strong`, which must not change while it is
/// set; the filtered copy does not depend on the mode beyond `filter_flag`.
pub fn predict_cached_ext<T: Sample>(
    refs: &Refs<T>,
    cache: &mut Option<Refs<T>>,
    mode: u8,
    n: usize,
    flags: PredFlags,
    bit_depth: u32,
    out: &mut [T],
) {
    if !filter_flag(mode, n, flags.smooth) {
        predict_filtered(refs, mode, n, flags, bit_depth, out);
        return;
    }
    let filtered = cache
        .get_or_insert_with(|| filter_refs(refs, mode, n, flags.smooth, flags.strong, bit_depth));
    predict_filtered(filtered, mode, n, flags, bit_depth, out);
}

/// Predict from reference samples that already went through the 8.4.4.2.3 filter (or
/// did not need it).
pub fn predict_filtered<T: Sample>(
    refs: &Refs<T>,
    mode: u8,
    n: usize,
    flags: PredFlags,
    bit_depth: u32,
    out: &mut [T],
) {
    predict_dispatch(refs, mode, n, flags, bit_depth, out);
}

/// [`predict_body`] compiled twice by `multiversion`: the AVX2 clone has the vector
/// instructions enabled for the inlined predictors, the baseline clone is the same code.
#[multiversion::multiversion(targets("x86_64+avx2"))]
fn predict_dispatch<T: Sample>(
    refs: &Refs<T>,
    mode: u8,
    n: usize,
    flags: PredFlags,
    bit_depth: u32,
    out: &mut [T],
) {
    predict_body(refs, mode, n, flags, bit_depth, out);
}

#[inline(always)]
fn predict_body<T: Sample>(
    refs: &Refs<T>,
    mode: u8,
    n: usize,
    flags: PredFlags,
    bit_depth: u32,
    out: &mut [T],
) {
    match mode {
        0 => predict_planar(refs, n, out),
        1 => predict_dc(refs, n, flags.edge, out),
        _ => predict_angular(
            refs,
            mode,
            n,
            flags.edge && !flags.no_boundary,
            bit_depth,
            out,
        ),
    }
}

#[inline(always)]
fn predict_planar<T: Sample>(refs: &Refs<T>, n: usize, out: &mut [T]) {
    let log2n = n.trailing_zeros();
    let top_right: i32 = refs.top[n].into();
    let bottom_left: i32 = refs.left[n].into();
    for y in 0..n {
        let left: i32 = refs.left[y].into();
        for x in 0..n {
            let value = (n as i32 - 1 - x as i32) * left
                + (x as i32 + 1) * top_right
                + (n as i32 - 1 - y as i32) * Into::<i32>::into(refs.top[x])
                + (y as i32 + 1) * bottom_left
                + n as i32;
            out[y * n + x] = T::from_i32(value >> (log2n + 1));
        }
    }
}

#[inline(always)]
fn predict_dc<T: Sample>(refs: &Refs<T>, n: usize, is_luma: bool, out: &mut [T]) {
    let mut sum = n as u32;
    for i in 0..n {
        let t: i32 = refs.top[i].into();
        let l: i32 = refs.left[i].into();
        sum += (t + l) as u32;
    }
    let dc = (sum >> (n.trailing_zeros() + 1)) as i32;
    for value in out[..n * n].iter_mut() {
        *value = T::from_i32(dc);
    }
    if is_luma && n < 32 {
        let l0: i32 = refs.left[0].into();
        let t0: i32 = refs.top[0].into();
        out[0] = T::from_i32((l0 + 2 * dc + t0 + 2) >> 2);
        for (x, sample) in out[1..n].iter_mut().enumerate() {
            let t: i32 = refs.top[x + 1].into();
            *sample = T::from_i32((t + 3 * dc + 2) >> 2);
        }
        for y in 1..n {
            let l: i32 = refs.left[y].into();
            out[y * n] = T::from_i32((l + 3 * dc + 2) >> 2);
        }
    }
}

#[inline(always)]
fn predict_angular<T: Sample>(
    refs: &Refs<T>,
    mode: u8,
    n: usize,
    is_luma: bool,
    bit_depth: u32,
    out: &mut [T],
) {
    let angle = ANGLE[mode as usize];
    let inv_angle = INV_ANGLE[mode as usize];
    let max = (1i32 << bit_depth) - 1;
    // ref[] is indexed from -nTbS to 2 * nTbS; store it shifted by 32.
    const BIAS: usize = 32;
    let mut reference = [0i32; 32 + 65];
    let (main, side): (&[T; 64], &[T; 64]) = if mode >= 18 {
        (&refs.top, &refs.left)
    } else {
        (&refs.left, &refs.top)
    };
    reference[BIAS] = refs.corner.into();
    for x in 1..=n {
        reference[BIAS + x] = main[x - 1].into();
    }
    if angle < 0 {
        let last = (n as i32 * angle) >> 5;
        if last < -1 {
            for x in last..=-1 {
                let idx = ((x * inv_angle + 128) >> 8) - 1;
                reference[(BIAS as i32 + x) as usize] = if idx < 0 {
                    refs.corner.into()
                } else {
                    side[idx as usize].into()
                };
            }
        }
    } else {
        for x in n + 1..=2 * n {
            reference[BIAS + x] = main[x - 1].into();
        }
    }
    // `idx`/`fact` depend only on the "row" index along the prediction
    // direction (y for the vertical modes, x for the horizontal ones — the
    // whole difference between the two halves of 8.4.4.2.6), not on the
    // position across it.
    if mode >= 18 {
        for y in 0..n {
            let idx = ((y as i32 + 1) * angle) >> 5;
            let fact = ((y as i32 + 1) * angle) & 31;
            let base = (BIAS as i32 + idx + 1) as usize;
            let row = &mut out[y * n..y * n + n];
            if fact != 0 {
                let (w0, w1) = (32 - fact, fact);
                for x in 0..n {
                    row[x] = T::from_i32(
                        (w0 * reference[base + x] + w1 * reference[base + x + 1] + 16) >> 5,
                    );
                }
            } else {
                for x in 0..n {
                    row[x] = T::from_i32(reference[base + x]);
                }
            }
        }
    } else {
        for x in 0..n {
            let idx = ((x as i32 + 1) * angle) >> 5;
            let fact = ((x as i32 + 1) * angle) & 31;
            let base = (BIAS as i32 + idx + 1) as usize;
            if fact != 0 {
                let (w0, w1) = (32 - fact, fact);
                for y in 0..n {
                    out[y * n + x] = T::from_i32(
                        (w0 * reference[base + y] + w1 * reference[base + y + 1] + 16) >> 5,
                    );
                }
            } else {
                for y in 0..n {
                    out[y * n + x] = T::from_i32(reference[base + y]);
                }
            }
        }
    }
    // The vertical and horizontal modes carry a boundary filter on luma
    // blocks below 32x32 (8-60, 8-68).
    if is_luma && n < 32 {
        let top0: i32 = refs.top[0].into();
        let left0: i32 = refs.left[0].into();
        let corner: i32 = refs.corner.into();
        if mode == 26 {
            for y in 0..n {
                let value = top0 + ((Into::<i32>::into(refs.left[y]) - corner) >> 1);
                out[y * n] = T::from_i32(value.clamp(0, max));
            }
        } else if mode == 10 {
            for (x, sample) in out[..n].iter_mut().enumerate() {
                let value = left0 + ((Into::<i32>::into(refs.top[x]) - corner) >> 1);
                *sample = T::from_i32(value.clamp(0, max));
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
    scan_index_fmt(mode, log2_size, is_luma, false)
}

/// [`scan_index`] for any chroma format: 4:4:4 chroma blocks of 8x8 are mode-dependent too.
pub fn scan_index_fmt(mode: u8, log2_size: u32, is_luma: bool, chroma_444: bool) -> usize {
    let mode_dependent = if is_luma || chroma_444 {
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
        let refs = Refs::<u8> {
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
                predict(&refs, mode, n, true, true, 8, &mut out);
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
        let mut refs = Refs::<u8> {
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
        let mut refs = Refs::<u8>::default();
        substitute(&mut refs, &Availability::default(), n, 8);
        assert_eq!(refs.corner, 128);
        assert!(refs.top[..8].iter().all(|&v| v == 128));
    }

    #[test]
    fn vertical_and_horizontal_modes_copy_their_edge() {
        let n = 8;
        let mut refs = Refs::<u8>::default();
        for i in 0..64 {
            refs.top[i] = (10 + i) as u8;
            refs.left[i] = (200 - i) as u8;
        }
        refs.corner = 100;
        let mut out = vec![0u8; n * n];
        // Mode 26 on chroma has no boundary filter: every row is the top row.
        predict(&refs, 26, n, false, false, 8, &mut out);
        for y in 0..n {
            for x in 0..n {
                assert_eq!(out[y * n + x], refs.top[x], "mode 26 at {x},{y}");
            }
        }
        // Mode 10 likewise copies the left column across.
        predict(&refs, 10, n, false, false, 8, &mut out);
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
        let mut refs = Refs::<u8>::default();
        for i in 0..64 {
            refs.left[i] = i as u8;
        }
        let mut out = vec![0u8; n * n];
        predict(&refs, 2, n, false, false, 8, &mut out);
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

    fn check_all<T: Sample + PartialEq + std::fmt::Debug>(bd: u32) {
        let mut seed = 0x9e37_79b9u32;
        let mut next = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as i32
        };
        let max = (1i32 << bd) - 1;
        for round in 0..6 {
            for n in [4usize, 8, 16, 32] {
                let mut refs = Refs::<T>::default();
                let mut v = |round: i32| match round {
                    0 => 0,
                    1 => max,
                    _ => next().rem_euclid(max + 1),
                };
                refs.corner = T::from_i32(v(round));
                for i in 0..64 {
                    refs.top[i] = T::from_i32(v(round));
                    refs.left[i] = T::from_i32(v(round));
                }
                let mut cache = None;
                for luma in [true, false] {
                    for mode in 0..35u8 {
                        let mut a = vec![T::default(); n * n];
                        let mut b = a.clone();
                        let mut c = a.clone();
                        predict(&refs, mode, n, luma, true, bd, &mut a);
                        predict_cached(&refs, &mut cache, mode, n, luma, true, bd, &mut b);
                        let f = filter_refs(&refs, mode, n, luma, true, bd);
                        let flags = PredFlags {
                            smooth: luma,
                            strong: true,
                            edge: luma,
                            no_boundary: false,
                        };
                        predict_body(&f, mode, n, flags, bd, &mut c);
                        assert_eq!(a, b, "cached n={n} mode={mode}");
                        assert_eq!(a, c, "scalar n={n} mode={mode}");
                    }
                    cache = None;
                }
            }
        }
    }

    #[test]
    fn vector_and_cached_prediction_match_the_plain_one() {
        check_all::<u8>(8);
        check_all::<u16>(8);
        check_all::<u16>(10);
    }
}

//! Intra prediction, the half of spec 7.11.2 a key frame needs.
//!
//! All thirteen key-frame intra modes: the seven that read no further than the
//! row above and the column to the left of the block, and the six that steer
//! along an angle and reach past the block's own width into the samples above
//! its right and below its left. Whether those further samples are decoded is
//! not this module's business -- the caller passes the samples it has, and the
//! edge is extended by repeating the last one, which is what the decoder does
//! with its own `BlockDecoded` bookkeeping (spec 7.11.2.2).

/// The average of the neighbours (spec 7.11.2.5).
pub const DC_PRED: u8 = 0;
/// The row above, repeated down the block.
pub const V_PRED: u8 = 1;
/// The column to the left, repeated across the block.
pub const H_PRED: u8 = 2;
/// The two smooth predictions crossed (spec 7.11.2.6).
pub const SMOOTH_PRED: u8 = 9;
/// Smooth down from the row above to the bottom-left sample.
pub const SMOOTH_V_PRED: u8 = 10;
/// Smooth across from the column left to the top-right sample.
pub const SMOOTH_H_PRED: u8 = 11;
/// Whichever of above, left and the corner is nearest their gradient.
pub const PAETH_PRED: u8 = 12;
/// Down-left along 45 degrees.
pub const D45_PRED: u8 = 3;
/// Down-right along 135 degrees.
pub const D135_PRED: u8 = 4;
/// Down-right, steeper: 113 degrees.
pub const D113_PRED: u8 = 5;
/// Down-right, shallower: 157 degrees.
pub const D157_PRED: u8 = 6;
/// Up-right along 203 degrees.
pub const D203_PRED: u8 = 7;
/// Down-left, steeper: 67 degrees.
pub const D67_PRED: u8 = 8;

/// Every mode a key frame's luma block can be coded with, in the order a
/// search should try them: the cheap ones first, so an early one is likely to
/// be the one that survives.
pub const KEY_FRAME_MODES: [u8; 13] = [
    DC_PRED,
    V_PRED,
    H_PRED,
    SMOOTH_PRED,
    SMOOTH_V_PRED,
    SMOOTH_H_PRED,
    PAETH_PRED,
    D45_PRED,
    D67_PRED,
    D113_PRED,
    D135_PRED,
    D157_PRED,
    D203_PRED,
];

/// The modes that read no further than the row above and the column to the
/// left, in the order a search should try them.
pub const NON_DIRECTIONAL: [u8; 7] = [
    DC_PRED,
    V_PRED,
    H_PRED,
    SMOOTH_PRED,
    SMOOTH_V_PRED,
    SMOOTH_H_PRED,
    PAETH_PRED,
];

/// `Sm_Weights` (spec 9.3), the blocks of the table for sides 4 to 64 laid end
/// to end so that the weights for a side start at that side's own index.
const SM_WEIGHTS: [u16; 128] = [
    0, 0, // unused: a side is at least 2
    255, 128, // 2
    255, 149, 85, 64, // 4
    255, 197, 146, 105, 73, 50, 37, 32, // 8
    255, 225, 196, 170, 145, 123, 102, 84, 68, 54, 43, 33, 26, 20, 17, 16, // 16
    255, 240, 225, 210, 196, 182, 169, 157, 145, 133, 122, 111, 101, 92, 83, 74, 66, 59, 52, 45,
    39, 34, 29, 25, 21, 17, 14, 12, 10, 9, 8, 8, // 32
    255, 248, 240, 233, 225, 218, 210, 203, 196, 189, 182, 176, 169, 163, 156, 150, 144, 138, 133,
    127, 121, 116, 111, 106, 101, 96, 91, 86, 82, 77, 73, 69, 65, 61, 57, 54, 50, 47, 44, 41, 38,
    35, 32, 29, 27, 25, 22, 20, 18, 16, 15, 13, 12, 10, 9, 8, 7, 6, 6, 5, 5, 4, 4, 4, // 64
];

/// `Round2` (spec 4.7): halves round up.
fn round2(value: i32, shift: u32) -> i32 {
    (value + (1 << (shift - 1))) >> shift
}

/// `Dr_Intra_Derivative` (spec 9.3): how far along the edge one row of the
/// block moves, in 64ths of a sample, for the angles a delta can reach.
fn dr_intra_derivative(angle: u16) -> i32 {
    match angle {
        3 => 1023,
        6 => 547,
        9 => 372,
        14 => 273,
        17 => 215,
        20 => 178,
        23 => 151,
        26 => 132,
        29 => 116,
        32 => 102,
        36 => 90,
        39 => 80,
        42 => 71,
        45 => 64,
        48 => 57,
        51 => 51,
        54 => 45,
        58 => 40,
        61 => 35,
        64 => 31,
        67 => 27,
        70 => 23,
        73 => 19,
        76 => 15,
        81 => 11,
        84 => 7,
        87 => 3,
        other => panic!("no derivative is tabulated for {other} degrees"),
    }
}

/// `Mode_To_Angle` (spec 9.3), zero for the modes that have no angle.
const MODE_TO_ANGLE: [u16; 13] = [0, 90, 180, 45, 135, 113, 157, 203, 67, 0, 0, 0, 0];

/// `ANGLE_STEP` (spec 9.3): degrees per unit of `angle_delta`, whose own
/// range is `-MAX_ANGLE_DELTA..=MAX_ANGLE_DELTA` (`MAX_ANGLE_DELTA == 3`).
const ANGLE_STEP: i32 = 3;

/// The edges a block predicts from, as the decoder builds them (spec 7.11.2.2):
/// a side that does not exist is filled from the side that does, a side that
/// runs out is extended by repeating its last sample, and the corner falls back
/// the same way.
///
/// Both arrays hold the corner first, so the spec's index `-1` is index 0 here
/// and they run out to the spec's `w + h - 1`.
struct Edges {
    above: Vec<i32>,
    left: Vec<i32>,
}

impl Edges {
    /// `bw + bh` is the reach both `av1_dr_prediction_z1_c`'s `max_base_x`
    /// and `z3_c`'s `max_base_y` use (libaom `reconintra.c`) -- above and
    /// left both extend that far regardless of which of `bw`/`bh` is larger.
    fn build(
        above: Option<&[u16]>,
        left: Option<&[u16]>,
        corner: Option<u16>,
        bw: usize,
        bh: usize,
    ) -> Self {
        let want = bw + bh;
        let extend = |samples: &[u16]| {
            let mut row: Vec<i32> = samples.iter().map(|&s| i32::from(s)).collect();
            let last = *row.last().expect("an edge that exists has samples");
            row.resize(want, last);
            row
        };
        // Spec 7.11.2.2's no-neighbour fallback is `base = 1 << (BitDepth - 1)`
        // (128 at 8-bit): `base - 1` above, `base + 1` left, `base` corner
        // (libaom `reconintra.c`'s own diagram: the shared top-left corner
        // cell is literally `base`, distinct from either row's own
        // replicated value -- confirmed against `av1_highbd_build_intra_predictors`'s
        // comment block, lane-tiny r1; an earlier attempt at this fix used
        // 127/129 for the corner instead and made a passing 16x16 fixture
        // regress, which is what caught the misreading).
        let base = 1i32 << (crate::decode::bit_depth() - 1);
        let above_row = match (above, left) {
            (Some(a), _) => extend(a),
            (None, Some(l)) => vec![i32::from(l[0]); want],
            (None, None) => vec![base - 1; want],
        };
        let left_col = match (left, above) {
            (Some(l), _) => extend(l),
            (None, Some(a)) => vec![i32::from(a[0]); want],
            (None, None) => vec![base + 1; want],
        };
        let corner = match (corner, above, left) {
            (Some(c), Some(_), Some(_)) => i32::from(c),
            (_, Some(a), _) => i32::from(a[0]),
            (_, None, Some(l)) => i32::from(l[0]),
            (_, None, None) => base,
        };
        let with_corner = |edge: Vec<i32>| {
            let mut v = Vec::with_capacity(want + 1);
            v.push(corner);
            v.extend(edge);
            v
        };
        Self {
            above: with_corner(above_row),
            left: with_corner(left_col),
        }
    }

    /// `AboveRow[i]`, where `i` may be the spec's `-1`.
    fn above(&self, i: i32) -> i32 {
        self.above[usize::try_from(i + 1).expect("an edge is read no further back than the corner")]
    }

    /// `LeftCol[i]`, where `i` may be the spec's `-1`.
    fn left(&self, i: i32) -> i32 {
        self.left[usize::try_from(i + 1).expect("an edge is read no further back than the corner")]
    }
}

/// Predicts one `bw`-wide, `bh`-tall block into `dst`, row-major and
/// `bw * bh` long; `bw == bh` for a square block, the only shape every
/// caller outside this lane's own tests passes.
///
/// `above` is the reconstructed row above the block and `left` its
/// reconstructed left column, each at least `bw`/`bh` samples long
/// respectively, and `corner` the sample diagonally above-left; `None` where
/// the block sits against an edge of the frame. A directional mode reads out
/// to `bw + bh`: pass the samples above-right and below-left where the
/// decoder has them decoded, and the edge is extended by repetition where it
/// does not, which is what the decoder's own clamp to `aboveLimit` and
/// `leftLimit` comes to.
///
/// `angle_delta` (spec `AngleDeltaY`/`AngleDeltaUV`, `-MAX_ANGLE_DELTA` to
/// `MAX_ANGLE_DELTA`) steers every one of `V_PRED`, `H_PRED` and the six
/// diagonal modes off their base angle by `ANGLE_STEP` degrees per unit (spec
/// 7.11.2.1's `pAngle = Mode_To_Angle[mode] + angleDelta * ANGLE_STEP`);
/// ignored (must be `0`) for the seven modes that carry no angle at all.
///
/// `enable_edge_filter` is the sequence header's `enable_intra_edge_filter`
/// (spec 7.11.2.4's own gate on the whole filter/upsample step); `smooth_neighbor`
/// is `get_intra_edge_filter_type` (spec 7.11.2.9, libaom `reconintra.c`) --
/// whether the block above or to the left predicted with one of the three
/// smooth modes, which steers [`intra_edge_filter_strength`]'s threshold
/// table. Ignored outside the directional modes.
///
/// # Panics
/// Panics on a mode this module does not predict, or when `dst` is not
/// `bw * bh` long.
#[allow(clippy::too_many_arguments)]
pub fn predict(
    mode: u8,
    angle_delta: i32,
    above: Option<&[u16]>,
    left: Option<&[u16]>,
    corner: Option<u16>,
    bw: usize,
    bh: usize,
    enable_edge_filter: bool,
    smooth_neighbor: bool,
    dst: &mut [u16],
) {
    assert_eq!(dst.len(), bw * bh, "the destination is the block");
    let edges = Edges::build(above, left, corner, bw, bh);
    let is_directional = matches!(
        mode,
        V_PRED | H_PRED | D45_PRED | D67_PRED | D113_PRED | D135_PRED | D157_PRED | D203_PRED
    );
    if is_directional {
        let angle = i32::from(MODE_TO_ANGLE[usize::from(mode)]) + angle_delta * ANGLE_STEP;
        // `pAngle == 90`/`180` is `V_PRED`/`H_PRED` at zero delta: a plain
        // edge copy, no walk -- and the only two angles `dr_intra_derivative`
        // has no table entry for, since libaom special-cases them the same
        // way (`av1_is_directional_mode`'s callers never call the z1/z2/z3
        // walk there either).
        if angle != 90 && angle != 180 {
            let n_top = above.map_or(0, |a| a.len().min(bw));
            let n_left = left.map_or(0, |a| a.len().min(bh));
            directional(
                angle as u16,
                &edges,
                bw,
                bh,
                enable_edge_filter,
                smooth_neighbor,
                n_top,
                n_left,
                dst,
            );
            return;
        }
    }
    // `smooth_predictor` (libaom `intrapred.c`) indexes its width and height
    // weight rows separately (`smooth_weights + bw - 4` / `+ bh - 4`) -- a
    // square block reads the same row for both, which is why this collapsed
    // to one `weights` slice before.
    let weights_w = &SM_WEIGHTS[bw..bw * 2];
    let weights_h = &SM_WEIGHTS[bh..bh * 2];
    for row in 0..bh {
        for col in 0..bw {
            let (r, c) = (row as i32, col as i32);
            let last_w = bw as i32 - 1;
            let last_h = bh as i32 - 1;
            let value = match mode {
                DC_PRED => dc(above, left, bw, bh),
                V_PRED => edges.above(c),
                H_PRED => edges.left(r),
                SMOOTH_PRED => round2(
                    i32::from(weights_h[row]) * edges.above(c)
                        + (256 - i32::from(weights_h[row])) * edges.left(last_h)
                        + i32::from(weights_w[col]) * edges.left(r)
                        + (256 - i32::from(weights_w[col])) * edges.above(last_w),
                    9,
                ),
                SMOOTH_V_PRED => round2(
                    i32::from(weights_h[row]) * edges.above(c)
                        + (256 - i32::from(weights_h[row])) * edges.left(last_h),
                    8,
                ),
                SMOOTH_H_PRED => round2(
                    i32::from(weights_w[col]) * edges.left(r)
                        + (256 - i32::from(weights_w[col])) * edges.above(last_w),
                    8,
                ),
                PAETH_PRED => paeth(edges.above(c), edges.left(r), edges.above(-1)),
                other => panic!("intra mode {other} is not one this module predicts"),
            };
            dst[row * bw + col] = value.clamp(0, crate::decode::sample_max()) as u16;
        }
    }
}

/// `intra_edge_filter_strength` (spec 7.11.2.9, libaom `reconintra.c`): the
/// filter kernel index (`0`..=`3`) for a gap of `delta` degrees against a
/// `bs0 + bs1`-wide block, split by `smooth_neighbor`'s two threshold tables.
fn intra_edge_filter_strength(bs0: i32, bs1: i32, delta: i32, smooth_neighbor: bool) -> i32 {
    let d = delta.abs();
    let blk_wh = bs0 + bs1;
    let mut strength = 0;
    if !smooth_neighbor {
        if blk_wh <= 8 {
            if d >= 56 {
                strength = 1;
            }
        } else if blk_wh <= 12 {
            // Only reachable by a rect block (e.g. 4x8's blk_wh == 12): no
            // square side sum ever lands in 9..=12, which is why this branch
            // was silently absent before this lane.
            if d >= 40 {
                strength = 1;
            }
        } else if blk_wh <= 16 {
            if d >= 40 {
                strength = 1;
            }
        } else if blk_wh <= 24 {
            if d >= 8 {
                strength = 1;
            }
            if d >= 16 {
                strength = 2;
            }
            if d >= 32 {
                strength = 3;
            }
        } else if blk_wh <= 32 {
            if d >= 1 {
                strength = 1;
            }
            if d >= 4 {
                strength = 2;
            }
            if d >= 32 {
                strength = 3;
            }
        } else if d >= 1 {
            strength = 3;
        }
    } else if blk_wh <= 8 {
        if d >= 40 {
            strength = 1;
        }
        if d >= 64 {
            strength = 2;
        }
    } else if blk_wh <= 16 {
        if d >= 20 {
            strength = 1;
        }
        if d >= 48 {
            strength = 2;
        }
    } else if blk_wh <= 24 {
        if d >= 4 {
            strength = 3;
        }
    } else if d >= 1 {
        strength = 3;
    }
    strength
}

/// `av1_use_intra_edge_upsample` (spec 7.11.2.9): whether the edge doubles in
/// density before the walk reads it.
fn use_intra_edge_upsample(bs0: i32, bs1: i32, delta: i32, smooth_neighbor: bool) -> bool {
    let d = delta.abs();
    let blk_wh = bs0 + bs1;
    if d == 0 || d >= 40 {
        return false;
    }
    if smooth_neighbor {
        blk_wh <= 8
    } else {
        blk_wh <= 16
    }
}

/// `av1_filter_intra_edge_c` (spec 7.11.2.8): a 5-tap smoothing pass over
/// `buf` in place, `buf[0]` (the corner) held fixed as every tap's clamp
/// floor/ceiling.
fn filter_intra_edge(buf: &mut [i32], strength: i32) {
    if strength == 0 {
        return;
    }
    const KERNEL: [[i32; 5]; 3] = [[0, 4, 8, 4, 0], [0, 5, 6, 5, 0], [2, 4, 4, 4, 2]];
    let filt = usize::try_from(strength - 1).expect("a filter strength is never negative");
    let sz = buf.len() as i32;
    let edge = buf.to_vec();
    for i in 1..buf.len() {
        let mut s = 0;
        for (j, &tap) in KERNEL[filt].iter().enumerate() {
            let k = (i as i32 - 2 + j as i32).clamp(0, sz - 1);
            s += edge[k as usize] * tap;
        }
        buf[i] = (s + 8) >> 4;
    }
}

/// `av1_upsample_intra_edge_c` (spec 7.11.2.8): doubles `buf`'s density.
/// `buf[0]` is spec position `-1` and `buf[buf.len() - 1]` is `sz - 1`; the
/// result's `[0]` is spec position `-2`, `[2*i + 1]` the half-sample between
/// `i - 1` and `i`, `[2*i + 2]` the original sample `i` moved to its doubled
/// slot.
fn upsample_intra_edge(buf: &[i32]) -> Vec<i32> {
    let sz = buf.len() - 1;
    let mut inp = vec![0i32; sz + 3];
    inp[0] = buf[0];
    inp[1] = buf[0];
    inp[2..sz + 2].copy_from_slice(&buf[1..]);
    inp[sz + 2] = buf[sz];
    let mut out = vec![0i32; 2 * sz + 1];
    out[0] = inp[0];
    for i in 0..sz {
        let s = -inp[i] + 9 * inp[i + 1] + 9 * inp[i + 2] - inp[i + 3];
        out[2 * i + 1] = ((s + 8) >> 4).clamp(0, crate::decode::sample_max());
        out[2 * i + 2] = inp[i + 2];
    }
    out
}

/// Directional intra prediction (spec 7.11.2.4): the edge filter/upsample
/// steps (spec 7.11.2.7-2.9) run first when `enable_edge_filter` (the
/// sequence header's own gate) says to, then the z1/z2/z3 walk (libaom
/// `av1_dr_prediction_z{1,2,3}_c`) reads whichever of the two -- filtered,
/// upsampled, or neither -- it ends with.
#[allow(clippy::too_many_arguments)]
fn directional(
    angle: u16,
    edges: &Edges,
    bw: usize,
    bh: usize,
    enable_edge_filter: bool,
    smooth_neighbor: bool,
    n_top: usize,
    n_left: usize,
    dst: &mut [u16],
) {
    let (w, h) = (bw as i32, bh as i32);
    let reach = w + h; // `av1_dr_prediction_z{1,3}_c`'s `max_base_{x,y}` reach.
    let need_above = angle < 180;
    let need_left = angle > 90;
    let need_right = angle < 90;
    let need_bottom = angle > 180;

    // Spec position `-1..=w + h - 1`, `above[0]`/`left[0]` the shared corner.
    let mut above: Vec<i32> = (-1..reach).map(|p| edges.above(p)).collect();
    let mut left: Vec<i32> = (-1..reach).map(|p| edges.left(p)).collect();

    if enable_edge_filter && angle != 90 && angle != 180 {
        if need_above && need_left && reach >= 24 {
            let s = round2(left[1] * 5 + above[0] * 6 + above[1] * 5, 4);
            above[0] = s;
            left[0] = s;
        }
        // `intra_edge_filter_strength(txwpx, txhpx, ...)` for above and
        // `(txhpx, txwpx, ...)` for left (`reconintra.c`) -- the strength
        // itself is symmetric in its first two args (only their sum
        // matters), but the pixel counts filtered below are NOT: above's
        // run-past-`n_top` extension is `txhpx` (the CROSS axis), left's is
        // `txwpx`.
        if need_above && n_top > 0 {
            let strength = intra_edge_filter_strength(w, h, i32::from(angle) - 90, smooth_neighbor);
            let n_px = n_top as i32 + 1 + if need_right { h } else { 0 };
            filter_intra_edge(&mut above[..n_px as usize], strength);
        }
        if need_left && n_left > 0 {
            let strength =
                intra_edge_filter_strength(h, w, i32::from(angle) - 180, smooth_neighbor);
            let n_px = n_left as i32 + 1 + if need_bottom { w } else { 0 };
            filter_intra_edge(&mut left[..n_px as usize], strength);
        }
    }

    let upsample_above =
        enable_edge_filter && use_intra_edge_upsample(w, h, i32::from(angle) - 90, smooth_neighbor);
    let upsample_left = enable_edge_filter
        && use_intra_edge_upsample(h, w, i32::from(angle) - 180, smooth_neighbor);

    let above_up;
    let (above_buf, above_off): (&[i32], i32) = if need_above && upsample_above {
        let n_px = (w + if need_right { h } else { 0 }) as usize;
        above_up = upsample_intra_edge(&above[..=n_px]);
        (&above_up, 2)
    } else {
        (&above, 1)
    };
    let left_up;
    let (left_buf, left_off): (&[i32], i32) = if need_left && upsample_left {
        let n_px = (h + if need_bottom { w } else { 0 }) as usize;
        left_up = upsample_intra_edge(&left[..=n_px]);
        (&left_up, 2)
    } else {
        (&left, 1)
    };
    let above_at = |p: i32| above_buf[(p + above_off) as usize];
    let left_at = |p: i32| left_buf[(p + left_off) as usize];
    let up_a = i32::from(upsample_above);
    let up_l = i32::from(upsample_left);
    let blend = |edge: &dyn Fn(i32) -> i32, base: i32, shift: i32| {
        round2(edge(base) * (32 - shift) + edge(base + 1) * shift, 5)
    };

    for row in 0..h {
        for col in 0..w {
            let value = if angle < 90 {
                let dx = dr_intra_derivative(angle);
                let max_base = (reach - 1) << up_a;
                let frac_bits = 6 - up_a;
                let x = dx * (row + 1);
                let base = (x >> frac_bits) + (col << up_a);
                let shift = ((x << up_a) & 0x3F) >> 1;
                if base < max_base {
                    blend(&above_at, base, shift)
                } else {
                    above_at(max_base)
                }
            } else if angle > 180 {
                let dy = dr_intra_derivative(270 - angle);
                let max_base = (reach - 1) << up_l;
                let frac_bits = 6 - up_l;
                let y = dy * (col + 1);
                let base = (y >> frac_bits) + (row << up_l);
                let shift = ((y << up_l) & 0x3F) >> 1;
                if base < max_base {
                    blend(&left_at, base, shift)
                } else {
                    left_at(max_base)
                }
            } else {
                // The two zones meet here: a ray that leaves through the row
                // above is read there, and one that leaves through the column
                // to the left is read there instead.
                let dx = dr_intra_derivative(180 - angle);
                let min_base = -(1 << up_a);
                let frac_bits_x = 6 - up_a;
                let y = row + 1;
                let x = (col << 6) - y * dx;
                let base = x >> frac_bits_x;
                if base >= min_base {
                    blend(&above_at, base, ((x << up_a) & 0x3F) >> 1)
                } else {
                    let dy = dr_intra_derivative(angle - 90);
                    let frac_bits_y = 6 - up_l;
                    let x2 = col + 1;
                    let y2 = (row << 6) - x2 * dy;
                    blend(&left_at, y2 >> frac_bits_y, ((y2 << up_l) & 0x3F) >> 1)
                }
            };
            dst[(row * w + col) as usize] = value.clamp(0, crate::decode::sample_max()) as u16;
        }
    }
}

/// `dc_predict` (spec 7.11.2.5): the average of whichever neighbours exist.
///
/// Only the block's own `side` samples of each edge count: what a directional
/// mode reaches past them is no part of the average. But `side` samples it
/// always is: libaom's `build_intra_predictors` (`av1/common/reconintra.c`)
/// replicates the last real sample out to the full transform width/height
/// before `dc_predictor` averages, so a slice truncated short by the true
/// frame edge must be extended by repetition here too, not averaged over its
/// own shorter length.
fn dc(above: Option<&[u16]>, left: Option<&[u16]>, bw: usize, bh: usize) -> i32 {
    let extend = |samples: &[u16], want: usize| -> Vec<u16> {
        if samples.len() >= want {
            samples[..want].to_vec()
        } else {
            let last = *samples.last().expect("an edge that exists has samples");
            let mut v = samples.to_vec();
            v.resize(want, last);
            v
        }
    };
    let above = above.map(|a| extend(a, bw));
    let left = left.map(|l| extend(l, bh));
    let average = |samples: &[u16]| {
        let sum: u32 = samples.iter().map(|&s| u32::from(s)).sum();
        ((sum + (samples.len() as u32 >> 1)) / samples.len() as u32) as i32
    };
    match (&above, &left) {
        (None, None) => 1i32 << (crate::decode::bit_depth() - 1),
        (Some(a), None) => average(a),
        (None, Some(l)) => average(l),
        (Some(a), Some(l)) if bw == bh => {
            // `dc_predictor` (libaom `intrapred.c`): exact division, the
            // square path this whole lane must leave bit-identical.
            let sum: u32 = a.iter().chain(l).map(|&s| u32::from(s)).sum();
            let count = (a.len() + l.len()) as u32;
            ((sum + (count >> 1)) / count) as i32
        }
        (Some(a), Some(l)) => {
            // `dc_predictor_rect`: AV1 never divides by `bw + bh` exactly for
            // a rect block -- it approximates with a multiply-shift whose
            // constants are derived from `bw + bh`'s odd factor (`d == 3` for
            // every 1:2 ratio, `d == 5` for every 1:4 one; libaom comments
            // this exact derivation in `intrapred.c` rather than tabulating
            // it, so this ports the derivation, not a lookup).
            let sum: u32 = a.iter().chain(l).map(|&s| u32::from(s)).sum();
            let (shift1, multiplier) = dc_rect_multiplier(bw, bh);
            let num = u64::from(sum + ((bw + bh) as u32 >> 1));
            (((num >> shift1) * multiplier) >> 16) as i32
        }
    }
}

/// `dc_predictor_rect`'s per-call `(shift1, multiplier)` (libaom
/// `intrapred.c`, `DC_MULTIPLIER_1X2`/`DC_MULTIPLIER_1X4`): shift `bw + bh`
/// right until it is odd; AV1's only rect ratios (1:2, 1:4) leave that odd
/// remainder at exactly 3 or 5.
fn dc_rect_multiplier(bw: usize, bh: usize) -> (u32, u64) {
    let mut d = (bw + bh) as u32;
    let mut shift1 = 0;
    while d & 1 == 0 {
        d >>= 1;
        shift1 += 1;
    }
    let multiplier = match d {
        3 => 0x5556,
        5 => 0x3334,
        _ => panic!("AV1 rect blocks are only 1:2 or 1:4 (bw={bw} bh={bh})"),
    };
    (shift1, multiplier)
}

/// `Intra_Filter_Taps` (spec 7.11.2.3 / libaom `av1_filter_intra_taps`,
/// `av1/common/reconintra.c`): five recursive-filter modes (DC/V/H/D157/Paeth
/// variants, in that order), each predicting an 8-tap block of eight output
/// samples from seven already-known neighbours.
const FILTER_INTRA_TAPS: [[[i32; 7]; 8]; 5] = [
    [
        [-6, 10, 0, 0, 0, 12, 0],
        [-5, 2, 10, 0, 0, 9, 0],
        [-3, 1, 1, 10, 0, 7, 0],
        [-3, 1, 1, 2, 10, 5, 0],
        [-4, 6, 0, 0, 0, 2, 12],
        [-3, 2, 6, 0, 0, 2, 9],
        [-3, 2, 2, 6, 0, 2, 7],
        [-3, 1, 2, 2, 6, 3, 5],
    ],
    [
        [-10, 16, 0, 0, 0, 10, 0],
        [-6, 0, 16, 0, 0, 6, 0],
        [-4, 0, 0, 16, 0, 4, 0],
        [-2, 0, 0, 0, 16, 2, 0],
        [-10, 16, 0, 0, 0, 0, 10],
        [-6, 0, 16, 0, 0, 0, 6],
        [-4, 0, 0, 16, 0, 0, 4],
        [-2, 0, 0, 0, 16, 0, 2],
    ],
    [
        [-8, 8, 0, 0, 0, 16, 0],
        [-8, 0, 8, 0, 0, 16, 0],
        [-8, 0, 0, 8, 0, 16, 0],
        [-8, 0, 0, 0, 8, 16, 0],
        [-4, 4, 0, 0, 0, 0, 16],
        [-4, 0, 4, 0, 0, 0, 16],
        [-4, 0, 0, 4, 0, 0, 16],
        [-4, 0, 0, 0, 4, 0, 16],
    ],
    [
        [-2, 8, 0, 0, 0, 10, 0],
        [-1, 3, 8, 0, 0, 6, 0],
        [-1, 2, 3, 8, 0, 4, 0],
        [0, 1, 2, 3, 8, 2, 0],
        [-1, 4, 0, 0, 0, 3, 10],
        [-1, 3, 4, 0, 0, 4, 6],
        [-1, 2, 3, 4, 0, 4, 4],
        [-1, 2, 2, 3, 4, 3, 3],
    ],
    [
        [-12, 14, 0, 0, 0, 14, 0],
        [-10, 0, 14, 0, 0, 12, 0],
        [-9, 0, 0, 14, 0, 11, 0],
        [-8, 0, 0, 0, 14, 10, 0],
        [-10, 12, 0, 0, 0, 0, 14],
        [-9, 1, 12, 0, 0, 0, 12],
        [-8, 0, 0, 12, 0, 1, 11],
        [-7, 0, 0, 1, 12, 1, 9],
    ],
];

/// `FILTER_INTRA_SCALE_BITS` (libaom `reconintra.h`): the taps' fixed-point
/// shift.
const FILTER_INTRA_SCALE_BITS: u32 = 4;

/// Recursive filter-intra prediction (spec 7.11.2.3, libaom
/// `av1_filter_intra_predictor_c`): walks 4x2 sub-blocks left-to-right,
/// top-to-bottom, each of the eight output samples a linear combination of
/// seven already-decoded or already-predicted neighbours (never past the
/// block's own top row / left column -- unlike the directional modes, this
/// one never reaches beyond `side` samples of either edge).
///
/// `mode` is `0..=4`, indexing [`FILTER_INTRA_TAPS`]/`FILTER_DC_PRED`
/// through `FILTER_PAETH_PRED`. `above`/`left`/`corner` are exactly
/// [`predict`]'s edge arguments -- a side that does not exist is filled from
/// the side that does and a side that runs out is extended by repeating its
/// last sample, the same [`Edges::build`] rule every other mode uses, even
/// though this mode itself never reaches past its own `side` samples of
/// either edge.
///
/// `bw`x`bh` is the block, not necessarily square (lane-rectsplit r1):
/// `av1_filter_intra_predictor_c` walks the same 4x2 patches over a
/// rectangular block, and `av1_filter_intra_allowed_bsize` genuinely offers
/// this mode on every HORZ/VERT strip whose sides are both <= 32 (16x8,
/// 8x16, 32x16, 16x32, ...), which the square-only assert refused before.
///
/// # Panics
/// Panics when `dst` is not `bw * bh` long, or `bw` is not a multiple of 4
/// or `bh` not a multiple of 2 (spec `av1_filter_intra_allowed_bsize` never
/// offers this mode past 32x32).
pub fn predict_filter_intra(
    mode: usize,
    above: Option<&[u16]>,
    left: Option<&[u16]>,
    corner: Option<u16>,
    bw: usize,
    bh: usize,
    dst: &mut [u16],
) {
    assert_eq!(dst.len(), bw * bh, "the destination is the block");
    assert_eq!(bw % 4, 0, "filter intra patches are 4 wide");
    assert_eq!(bh % 2, 0, "filter intra patches are 2 high");
    let edges = Edges::build(above, left, corner, bw, bh);
    let taps = &FILTER_INTRA_TAPS[mode];
    // A (side+1)-square buffer: row 0 / column 0 hold the corner and the
    // above/left edges, `buffer[r+1][c+1]` the block's own sample at (r, c).
    let mut buffer = vec![0i32; (bh + 1) * (bw + 1)];
    let at = |buffer: &[i32], r: usize, c: usize| buffer[r * (bw + 1) + c];
    buffer[0] = edges.above(-1);
    for c in 0..bw {
        buffer[c + 1] = edges.above(c as i32);
    }
    for r in 0..bh {
        buffer[(r + 1) * (bw + 1)] = edges.left(r as i32);
    }
    let mut r = 1;
    while r < bh + 1 {
        let mut c = 1;
        while c < bw + 1 {
            let p0 = at(&buffer, r - 1, c - 1);
            let p1 = at(&buffer, r - 1, c);
            let p2 = at(&buffer, r - 1, c + 1);
            let p3 = at(&buffer, r - 1, c + 2);
            let p4 = at(&buffer, r - 1, c + 3);
            let p5 = at(&buffer, r, c - 1);
            let p6 = at(&buffer, r + 1, c - 1);
            let p = [p0, p1, p2, p3, p4, p5, p6];
            for k in 0..8 {
                let (r_off, c_off) = (k >> 2, k & 3);
                let pr: i32 = taps[k].iter().zip(&p).map(|(t, s)| t * s).sum();
                let value = round2(pr, FILTER_INTRA_SCALE_BITS).clamp(0, crate::decode::sample_max());
                let idx = (r + r_off) * (bw + 1) + c + c_off;
                buffer[idx] = value;
            }
            c += 4;
        }
        r += 2;
    }
    for row in 0..bh {
        for col in 0..bw {
            dst[row * bw + col] = at(&buffer, row + 1, col + 1) as u16;
        }
    }
}

/// `paeth_predict` (spec 7.11.2.2): of the three neighbours, the one nearest
/// the gradient they describe.
fn paeth(above: i32, left: i32, corner: i32) -> i32 {
    let base = above + left - corner;
    let (d_left, d_above, d_corner) = (
        (base - left).abs(),
        (base - above).abs(),
        (base - corner).abs(),
    );
    if d_left <= d_above && d_left <= d_corner {
        left
    } else if d_above <= d_corner {
        above
    } else {
        corner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Same deterministic synthetic ramp `lanes/intrarect_dump.c`'s `fill`
    /// generates.
    fn fill(n: usize, seed: i32) -> Vec<u16> {
        (0..n)
            .map(|i| ((i as i32 * 7 + seed * 13 + (i as i32 % 5) * 3).rem_euclid(256)) as u16)
            .collect()
    }

    /// `lanes/intrarect_dump.c`'s `checksum`: a position-weighted sum so a
    /// wrong value anywhere in the block, not just at index 0, moves it.
    fn checksum(dst: &[u16]) -> u64 {
        dst.iter()
            .enumerate()
            .map(|(i, &v)| u64::from(v) * (i as u64 + 1))
            .sum()
    }

    /// lane-intrarect r1: checksum-verify DC/SMOOTH/PAETH and a directional
    /// sweep across all three zones, `enable_edge_filter`/`smooth_neighbor`
    /// on and off, for eight rect shapes (both 1:2 and 1:4, both
    /// orientations), against `lanes/intrarect_dump.c`'s independent C
    /// transcription (`lanes/intrarect_dump.expected.txt`, generated by
    /// `gcc -O2 lanes/intrarect_dump.c -o /tmp/intrarect_dump &&
    /// /tmp/intrarect_dump > lanes/intrarect_dump.expected.txt`).
    #[test]
    fn rect_predictors_match_c_dump() {
        let expected_txt = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../lanes/intrarect_dump.expected.txt"
        ))
        .expect(
            "run gcc -O2 lanes/intrarect_dump.c -o /tmp/intrarect_dump && \
             /tmp/intrarect_dump > lanes/intrarect_dump.expected.txt first",
        );
        // Keys: ("DC"|"SMOOTH"|"PAETH", bw, bh, angle, ef, sn) -> value.
        // Non-directional entries use angle=0, ef=2, sn=2 (never matched by
        // the directional lookups below, which always pass 0 or 1).
        let mut expected: HashMap<(&str, usize, usize, i32, i32, i32), u64> = HashMap::new();
        for line in expected_txt.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            let (bw, bh) = parts[0].split_once('x').unwrap();
            let (bw, bh): (usize, usize) = (bw.parse().unwrap(), bh.parse().unwrap());
            match parts[1] {
                "DC" => {
                    let v: u64 = parts[2]["value=".len()..].parse().unwrap();
                    expected.insert(("DC", bw, bh, 0, 2, 2), v);
                }
                "SMOOTH" | "PAETH" => {
                    let cs: u64 = parts[2]["checksum=".len()..].parse().unwrap();
                    expected.insert((parts[1], bw, bh, 0, 2, 2), cs);
                }
                "DR" => {
                    let angle: i32 = parts[2]["angle=".len()..].parse().unwrap();
                    let ef: i32 = parts[3]["ef=".len()..].parse().unwrap();
                    let sn: i32 = parts[4]["sn=".len()..].parse().unwrap();
                    let cs: u64 = parts[5]["checksum=".len()..].parse().unwrap();
                    expected.insert(("DR", bw, bh, angle, ef, sn), cs);
                }
                other => panic!("unrecognised dump line kind {other}"),
            }
        }

        let shapes = [
            (8, 4),
            (4, 8),
            (16, 8),
            (8, 16),
            (4, 16),
            (16, 4),
            (32, 16),
            (16, 32),
        ];
        // (angle, mode, angle_delta) for every angle `intrarect_dump.c`
        // sweeps -- the same reachable `Mode_To_Angle + delta * ANGLE_STEP`
        // combinations the dump had to stay inside of.
        let directional_cases: [(i32, u8, i32); 17] = [
            (45, D45_PRED, 0),
            (48, D45_PRED, 1),
            (64, D67_PRED, -1),
            (67, D67_PRED, 0),
            (84, V_PRED, -2),
            (87, V_PRED, -1),
            (113, D113_PRED, 0),
            (116, D113_PRED, 1),
            (135, D135_PRED, 0),
            (138, D135_PRED, 1),
            (157, D157_PRED, 0),
            (160, D157_PRED, 1),
            (171, H_PRED, -3),
            (183, H_PRED, 1),
            (186, H_PRED, 2),
            (203, D203_PRED, 0),
            (206, D203_PRED, 1),
        ];

        let mut checked = 0;
        for &(bw, bh) in &shapes {
            let reach = bw + bh;
            let above = fill(reach, 1);
            let left = fill(reach, 2);
            let corner = ((bw * 3 + bh * 5) % 256) as u16;

            for (name, mode) in [("DC", DC_PRED), ("SMOOTH", SMOOTH_PRED), ("PAETH", PAETH_PRED)] {
                let mut dst = vec![0u16; bw * bh];
                predict(
                    mode,
                    0,
                    Some(&above),
                    Some(&left),
                    Some(corner),
                    bw,
                    bh,
                    false,
                    false,
                    &mut dst,
                );
                let got = if name == "DC" {
                    u64::from(dst[0])
                } else {
                    checksum(&dst)
                };
                let want = *expected
                    .get(&(name, bw, bh, 0, 2, 2))
                    .unwrap_or_else(|| panic!("missing {name} {bw}x{bh} in the C dump"));
                assert_eq!(got, want, "{name} {bw}x{bh}: rust {got} != C dump {want}");
                checked += 1;
            }

            for &(angle, mode, delta) in &directional_cases {
                for ef in [false, true] {
                    for sn in [false, true] {
                        let mut dst = vec![0u16; bw * bh];
                        predict(
                            mode,
                            delta,
                            Some(&above),
                            Some(&left),
                            Some(corner),
                            bw,
                            bh,
                            ef,
                            sn,
                            &mut dst,
                        );
                        let got = checksum(&dst);
                        let key = ("DR", bw, bh, angle, i32::from(ef), i32::from(sn));
                        let want = *expected.get(&key).unwrap_or_else(|| {
                            panic!("missing DR {bw}x{bh} angle={angle} ef={ef} sn={sn} in the C dump")
                        });
                        assert_eq!(
                            got, want,
                            "DR {bw}x{bh} angle={angle} ef={ef} sn={sn}: rust {got} != C dump {want}"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert_eq!(
            checked,
            shapes.len() * (3 + directional_cases.len() * 4),
            "checked every (shape, mode/angle, ef, sn) combination"
        );
    }
}

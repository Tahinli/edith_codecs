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
    fn build(above: Option<&[u8]>, left: Option<&[u8]>, corner: Option<u8>, side: usize) -> Self {
        // A directional mode reads out to w + h - 1 either way.
        let want = side * 2;
        let extend = |samples: &[u8]| {
            let mut row: Vec<i32> = samples.iter().map(|&s| i32::from(s)).collect();
            let last = *row.last().expect("an edge that exists has samples");
            row.resize(want, last);
            row
        };
        let above_row = match (above, left) {
            (Some(a), _) => extend(a),
            (None, Some(l)) => vec![i32::from(l[0]); want],
            (None, None) => vec![127; want],
        };
        let left_col = match (left, above) {
            (Some(l), _) => extend(l),
            (None, Some(a)) => vec![i32::from(a[0]); want],
            (None, None) => vec![129; want],
        };
        let corner = match (corner, above, left) {
            (Some(c), Some(_), Some(_)) => i32::from(c),
            (_, Some(a), _) => i32::from(a[0]),
            (_, None, Some(l)) => i32::from(l[0]),
            (_, None, None) => 128,
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

/// Predicts one square block into `dst`, row-major and `side * side` long.
///
/// `above` is the reconstructed row above the block and `left` its
/// reconstructed left column, each at least `side` samples long, and `corner`
/// the sample diagonally above-left; `None` where the block sits against an
/// edge of the frame. A directional mode reads out to `2 * side`: pass the
/// samples above-right and below-left where the decoder has them decoded, and
/// the edge is extended by repetition where it does not, which is what the
/// decoder's own clamp to `aboveLimit` and `leftLimit` comes to.
///
/// `angle_delta` (spec `AngleDeltaY`/`AngleDeltaUV`, `-MAX_ANGLE_DELTA` to
/// `MAX_ANGLE_DELTA`) steers every one of `V_PRED`, `H_PRED` and the six
/// diagonal modes off their base angle by `ANGLE_STEP` degrees per unit (spec
/// 7.11.2.1's `pAngle = Mode_To_Angle[mode] + angleDelta * ANGLE_STEP`);
/// ignored (must be `0`) for the seven modes that carry no angle at all.
///
/// # Panics
/// Panics on a mode this module does not predict, or when `dst` is not
/// `side * side` long.
pub fn predict(
    mode: u8,
    angle_delta: i32,
    above: Option<&[u8]>,
    left: Option<&[u8]>,
    corner: Option<u8>,
    side: usize,
    dst: &mut [u8],
) {
    assert_eq!(dst.len(), side * side, "the destination is the block");
    let edges = Edges::build(above, left, corner, side);
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
            directional(angle as u16, &edges, side, dst);
            return;
        }
    }
    let weights = &SM_WEIGHTS[side..side * 2];
    for row in 0..side {
        for col in 0..side {
            let (r, c) = (row as i32, col as i32);
            let last = side as i32 - 1;
            let value = match mode {
                DC_PRED => dc(above, left, side),
                V_PRED => edges.above(c),
                H_PRED => edges.left(r),
                SMOOTH_PRED => round2(
                    i32::from(weights[row]) * edges.above(c)
                        + (256 - i32::from(weights[row])) * edges.left(last)
                        + i32::from(weights[col]) * edges.left(r)
                        + (256 - i32::from(weights[col])) * edges.above(last),
                    9,
                ),
                SMOOTH_V_PRED => round2(
                    i32::from(weights[row]) * edges.above(c)
                        + (256 - i32::from(weights[row])) * edges.left(last),
                    8,
                ),
                SMOOTH_H_PRED => round2(
                    i32::from(weights[col]) * edges.left(r)
                        + (256 - i32::from(weights[col])) * edges.above(last),
                    8,
                ),
                PAETH_PRED => paeth(edges.above(c), edges.left(r), edges.above(-1)),
                other => panic!("intra mode {other} is not one this module predicts"),
            };
            dst[row * side + col] = value.clamp(0, 255) as u8;
        }
    }
}

/// Directional intra prediction (spec 7.11.2.4), with the intra edge filter off
/// -- which is what our sequence header says -- so nothing here upsamples.
fn directional(angle: u16, edges: &Edges, side: usize, dst: &mut [u8]) {
    let n = side as i32;
    // The furthest sample the walk can reach, past which the edge is flat.
    let max_base = n + n - 1;
    let blend = |edge: &dyn Fn(i32) -> i32, base: i32, shift: i32| {
        round2(edge(base) * (32 - shift) + edge(base + 1) * shift, 5)
    };
    let above = |i: i32| edges.above(i);
    let left = |i: i32| edges.left(i);
    for row in 0..n {
        for col in 0..n {
            let value = if angle < 90 {
                let dx = dr_intra_derivative(angle);
                let idx = (row + 1) * dx;
                let base = (idx >> 6) + col;
                let shift = (idx >> 1) & 0x1F;
                if base < max_base {
                    blend(&above, base, shift)
                } else {
                    above(max_base)
                }
            } else if angle > 180 {
                let dy = dr_intra_derivative(270 - angle);
                let idx = (col + 1) * dy;
                let base = (idx >> 6) + row;
                let shift = (idx >> 1) & 0x1F;
                if base < max_base {
                    blend(&left, base, shift)
                } else {
                    left(max_base)
                }
            } else {
                // The two zones meet here: a ray that leaves through the row
                // above is read there, and one that leaves through the column
                // to the left is read there instead.
                let dx = dr_intra_derivative(180 - angle);
                let idx = (col << 6) - (row + 1) * dx;
                let base = idx >> 6;
                if base >= -1 {
                    blend(&above, base, (idx >> 1) & 0x1F)
                } else {
                    let dy = dr_intra_derivative(angle - 90);
                    let idx = (row << 6) - (col + 1) * dy;
                    blend(&left, idx >> 6, (idx >> 1) & 0x1F)
                }
            };
            dst[(row * n + col) as usize] = value.clamp(0, 255) as u8;
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
fn dc(above: Option<&[u8]>, left: Option<&[u8]>, side: usize) -> i32 {
    let extend = |samples: &[u8]| -> Vec<u8> {
        if samples.len() >= side {
            samples[..side].to_vec()
        } else {
            let last = *samples.last().expect("an edge that exists has samples");
            let mut v = samples.to_vec();
            v.resize(side, last);
            v
        }
    };
    let above = above.map(extend);
    let left = left.map(extend);
    let average = |samples: &[u8]| {
        let sum: u32 = samples.iter().map(|&s| u32::from(s)).sum();
        ((sum + (samples.len() as u32 >> 1)) / samples.len() as u32) as i32
    };
    match (&above, &left) {
        (None, None) => 128,
        (Some(a), None) => average(a),
        (None, Some(l)) => average(l),
        (Some(a), Some(l)) => {
            let sum: u32 = a.iter().chain(l).map(|&s| u32::from(s)).sum();
            let count = (a.len() + l.len()) as u32;
            ((sum + (count >> 1)) / count) as i32
        }
    }
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
/// # Panics
/// Panics when `dst` is not `side * side` long, or `side` is not a multiple
/// of 4 (spec `av1_filter_intra_allowed_bsize` never offers this mode past
/// 32x32, and only on square blocks: 4, 8, 16 or 32).
pub fn predict_filter_intra(
    mode: usize,
    above: Option<&[u8]>,
    left: Option<&[u8]>,
    corner: Option<u8>,
    side: usize,
    dst: &mut [u8],
) {
    assert_eq!(dst.len(), side * side, "the destination is the block");
    assert_eq!(side % 4, 0, "filter intra only offers 4/8/16/32");
    let edges = Edges::build(above, left, corner, side);
    let taps = &FILTER_INTRA_TAPS[mode];
    // A (side+1)-square buffer: row 0 / column 0 hold the corner and the
    // above/left edges, `buffer[r+1][c+1]` the block's own sample at (r, c).
    let mut buffer = vec![0i32; (side + 1) * (side + 1)];
    let at = |buffer: &[i32], r: usize, c: usize| buffer[r * (side + 1) + c];
    buffer[0] = edges.above(-1);
    for c in 0..side {
        buffer[c + 1] = edges.above(c as i32);
    }
    for r in 0..side {
        buffer[(r + 1) * (side + 1)] = edges.left(r as i32);
    }
    let mut r = 1;
    while r < side + 1 {
        let mut c = 1;
        while c < side + 1 {
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
                let value = round2(pr, FILTER_INTRA_SCALE_BITS).clamp(0, 255);
                let idx = (r + r_off) * (side + 1) + c + c_off;
                buffer[idx] = value;
            }
            c += 4;
        }
        r += 2;
    }
    for row in 0..side {
        for col in 0..side {
            dst[row * side + col] = at(&buffer, row + 1, col + 1) as u8;
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

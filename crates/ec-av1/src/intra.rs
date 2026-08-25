//! Intra prediction, the half of spec 7.11.2 a key frame needs.
//!
//! The seven modes here are the ones that read no further than the row above
//! and the column to the left of the block: the eight directional modes reach
//! past a block's own width into samples whose availability the decoder tracks
//! in its own bookkeeping, and are not written yet.

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

/// The modes this module predicts, in the order a search should try them.
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

/// The edges a block predicts from, as the decoder builds them (spec 7.11.2.2):
/// a side that does not exist is filled from the side that does, and the corner
/// falls back the same way.
struct Edges {
    above: Vec<i32>,
    left: Vec<i32>,
    corner: i32,
}

impl Edges {
    fn build(above: Option<&[u8]>, left: Option<&[u8]>, corner: Option<u8>, side: usize) -> Self {
        let above_row = match (above, left) {
            (Some(a), _) => a.iter().map(|&s| i32::from(s)).collect(),
            (None, Some(l)) => vec![i32::from(l[0]); side],
            (None, None) => vec![127; side],
        };
        let left_col = match (left, above) {
            (Some(l), _) => l.iter().map(|&s| i32::from(s)).collect(),
            (None, Some(a)) => vec![i32::from(a[0]); side],
            (None, None) => vec![129; side],
        };
        let corner = match (corner, above, left) {
            (Some(c), Some(_), Some(_)) => i32::from(c),
            (_, Some(a), _) => i32::from(a[0]),
            (_, None, Some(l)) => i32::from(l[0]),
            (_, None, None) => 128,
        };
        Self {
            above: above_row,
            left: left_col,
            corner,
        }
    }
}

/// Predicts one square block into `dst`, row-major and `side * side` long.
///
/// `above` is the reconstructed row above the block and `left` its
/// reconstructed left column, each `side` samples long, and `corner` the
/// sample diagonally above-left; `None` where the block sits against an edge
/// of the frame.
///
/// # Panics
/// Panics on a mode this module does not predict, or when `dst` is not
/// `side * side` long.
pub fn predict(
    mode: u8,
    above: Option<&[u8]>,
    left: Option<&[u8]>,
    corner: Option<u8>,
    side: usize,
    dst: &mut [u8],
) {
    assert_eq!(dst.len(), side * side, "the destination is the block");
    let edges = Edges::build(above, left, corner, side);
    let (a, l) = (&edges.above, &edges.left);
    let weights = &SM_WEIGHTS[side..side * 2];
    for row in 0..side {
        for col in 0..side {
            let value = match mode {
                DC_PRED => dc(above, left),
                V_PRED => a[col],
                H_PRED => l[row],
                SMOOTH_PRED => round2(
                    i32::from(weights[row]) * a[col]
                        + (256 - i32::from(weights[row])) * l[side - 1]
                        + i32::from(weights[col]) * l[row]
                        + (256 - i32::from(weights[col])) * a[side - 1],
                    9,
                ),
                SMOOTH_V_PRED => round2(
                    i32::from(weights[row]) * a[col]
                        + (256 - i32::from(weights[row])) * l[side - 1],
                    8,
                ),
                SMOOTH_H_PRED => round2(
                    i32::from(weights[col]) * l[row]
                        + (256 - i32::from(weights[col])) * a[side - 1],
                    8,
                ),
                PAETH_PRED => paeth(a[col], l[row], edges.corner),
                other => panic!("intra mode {other} is not one this module predicts"),
            };
            dst[row * side + col] = value.clamp(0, 255) as u8;
        }
    }
}

/// `dc_predict` (spec 7.11.2.5): the average of whichever neighbours exist.
fn dc(above: Option<&[u8]>, left: Option<&[u8]>) -> i32 {
    let average = |samples: &[u8]| {
        let sum: u32 = samples.iter().map(|&s| u32::from(s)).sum();
        ((sum + (samples.len() as u32 >> 1)) / samples.len() as u32) as i32
    };
    match (above, left) {
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

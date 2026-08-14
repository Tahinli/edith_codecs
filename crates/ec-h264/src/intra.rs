//! Intra prediction (Rec. ITU-T H.264 clause 8.3).
//!
//! Each process is a function named after its clause and takes only the
//! neighbouring samples it is allowed to read, as the availability rules of
//! clause 8.3.1.2 leave them: an unavailable neighbour is `None`, never a
//! zero-filled array, so a mode that reads an unavailable sample is a
//! type error rather than a silently wrong picture.

// Clippy's needless_range_loop asks for iterators where this file
// transcribes the specification's own `for i` / `for j` formulas; the
// index is the point.
#![allow(clippy::needless_range_loop)]

use ec_core::error::{Error, Result};

/// The neighbouring samples of a 4x4 luma block, clause 8.3.1.2.
///
/// `top_right` is separate from `top` because it has its own availability and
/// its own substitution rule: inside a macroblock the block above right is
/// often not decoded yet.
#[derive(Debug, Clone, Copy, Default)]
pub struct Neighbours4x4 {
    /// `p[-1, 0..3]`.
    pub left: Option<[u8; 4]>,
    /// `p[0..3, -1]`.
    pub top: Option<[u8; 4]>,
    /// `p[4..7, -1]`.
    pub top_right: Option<[u8; 4]>,
    /// `p[-1, -1]`.
    pub corner: Option<u8>,
}

impl Neighbours4x4 {
    /// `p[x, -1]` for x in -1..8, after the substitution of clause 8.3.1.2:
    /// when `p[4..7, -1]` are unavailable but `p[3, -1]` is, they take its
    /// value. `x == -1` is the corner sample `p[-1, -1]`, which several of the
    /// diagonal modes reach through this row.
    fn t(&self, x: i32) -> i32 {
        let top = self.top.unwrap_or([0; 4]);
        match x {
            -1 => self.c(),
            0..=3 => top[x as usize] as i32,
            4..=7 => match self.top_right {
                Some(tr) => tr[x as usize - 4] as i32,
                None => top[3] as i32,
            },
            _ => unreachable!("H.264 Intra_4x4: p[{x}, -1] is outside the prediction row"),
        }
    }

    /// `p[-1, y]` for y in -1..4; `y == -1` is again the corner sample.
    fn l(&self, y: i32) -> i32 {
        match y {
            -1 => self.c(),
            0..=3 => self.left.unwrap_or([0; 4])[y as usize] as i32,
            _ => unreachable!("H.264 Intra_4x4: p[-1, {y}] is outside the prediction column"),
        }
    }

    /// `p[-1, -1]`.
    fn c(&self) -> i32 {
        self.corner.unwrap_or(0) as i32
    }
}

/// The nine `Intra_4x4` prediction modes (Table 8-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intra4x4PredMode {
    /// 0: `Intra_4x4_Vertical`.
    Vertical,
    /// 1: `Intra_4x4_Horizontal`.
    Horizontal,
    /// 2: `Intra_4x4_DC`.
    Dc,
    /// 3: `Intra_4x4_Diagonal_Down_Left`.
    DiagonalDownLeft,
    /// 4: `Intra_4x4_Diagonal_Down_Right`.
    DiagonalDownRight,
    /// 5: `Intra_4x4_Vertical_Right`.
    VerticalRight,
    /// 6: `Intra_4x4_Horizontal_Down`.
    HorizontalDown,
    /// 7: `Intra_4x4_Vertical_Left`.
    VerticalLeft,
    /// 8: `Intra_4x4_Horizontal_Up`.
    HorizontalUp,
}

impl Intra4x4PredMode {
    /// Map an `Intra4x4PredMode` value 0..=8 onto the enum.
    pub fn from_value(value: i32) -> Result<Intra4x4PredMode> {
        use Intra4x4PredMode::*;
        Ok(match value {
            0 => Vertical,
            1 => Horizontal,
            2 => Dc,
            3 => DiagonalDownLeft,
            4 => DiagonalDownRight,
            5 => VerticalRight,
            6 => HorizontalDown,
            7 => VerticalLeft,
            8 => HorizontalUp,
            other => {
                return Err(Error::corrupt(format!("H.264: Intra4x4PredMode = {other}")));
            }
        })
    }
}

/// Clauses 8.3.1.2.1 to 8.3.1.2.9: predict one 4x4 luma block, `[y][x]`.
///
/// Every mode except DC needs neighbours the encoder guaranteed are available;
/// asking for one that is not is a corrupt bitstream, not a fallback.
pub fn predict_4x4(mode: Intra4x4PredMode, n: &Neighbours4x4) -> Result<[[u8; 4]; 4]> {
    use Intra4x4PredMode::*;
    let need = |have: bool, what: &str| -> Result<()> {
        if have {
            Ok(())
        } else {
            Err(Error::corrupt(format!(
                "H.264 Intra_4x4 {mode:?}: {what} is not available"
            )))
        }
    };
    let mut pred = [[0u8; 4]; 4];
    match mode {
        // 8.3.1.2.1
        Vertical => {
            need(n.top.is_some(), "p[x, -1]")?;
            for y in 0..4 {
                for x in 0..4 {
                    pred[y][x] = n.t(x as i32) as u8;
                }
            }
        }
        // 8.3.1.2.2
        Horizontal => {
            need(n.left.is_some(), "p[-1, y]")?;
            for y in 0..4 {
                for x in 0..4 {
                    pred[y][x] = n.l(y as i32) as u8;
                }
            }
        }
        // 8.3.1.2.3
        Dc => {
            let value = match (n.top.is_some(), n.left.is_some()) {
                (true, true) => {
                    ((0..4i32).map(|x| n.t(x)).sum::<i32>()
                        + (0..4i32).map(|y| n.l(y)).sum::<i32>()
                        + 4)
                        >> 3
                }
                (false, true) => ((0..4i32).map(|y| n.l(y)).sum::<i32>() + 2) >> 2,
                (true, false) => ((0..4i32).map(|x| n.t(x)).sum::<i32>() + 2) >> 2,
                // Neither neighbour available: mid grey, 1 << (BitDepthY - 1).
                (false, false) => 128,
            };
            pred = [[value as u8; 4]; 4];
        }
        // 8.3.1.2.4
        DiagonalDownLeft => {
            need(n.top.is_some(), "p[x, -1]")?;
            for y in 0..4 {
                for x in 0..4 {
                    let (xi, yi) = (x as i32, y as i32);
                    pred[y][x] = if x == 3 && y == 3 {
                        ((n.t(6) + 3 * n.t(7) + 2) >> 2) as u8
                    } else {
                        ((n.t(xi + yi) + 2 * n.t(xi + yi + 1) + n.t(xi + yi + 2) + 2) >> 2) as u8
                    };
                }
            }
        }
        // 8.3.1.2.5
        DiagonalDownRight => {
            need(
                n.top.is_some() && n.left.is_some() && n.corner.is_some(),
                "p[x, -1], p[-1, y] and p[-1, -1]",
            )?;
            for y in 0..4i32 {
                for x in 0..4i32 {
                    let v = match x.cmp(&y) {
                        std::cmp::Ordering::Greater => {
                            let d = x - y;
                            (n.t(d - 2) + 2 * n.t(d - 1) + n.t(d) + 2) >> 2
                        }
                        std::cmp::Ordering::Less => {
                            let d = y - x;
                            (n.l(d - 2) + 2 * n.l(d - 1) + n.l(d) + 2) >> 2
                        }
                        std::cmp::Ordering::Equal => (n.t(0) + 2 * n.c() + n.l(0) + 2) >> 2,
                    };
                    pred[y as usize][x as usize] = v as u8;
                }
            }
        }
        // 8.3.1.2.6
        VerticalRight => {
            need(
                n.top.is_some() && n.left.is_some() && n.corner.is_some(),
                "p[x, -1], p[-1, y] and p[-1, -1]",
            )?;
            for y in 0..4i32 {
                for x in 0..4i32 {
                    let z = 2 * x - y;
                    let h = x - y / 2; // x - (y >> 1)
                    let v = if z >= 0 && z % 2 == 0 {
                        (n.t(h - 1) + n.t(h) + 1) >> 1
                    } else if z >= 0 {
                        (n.t(h - 2) + 2 * n.t(h - 1) + n.t(h) + 2) >> 2
                    } else if z == -1 {
                        (n.l(0) + 2 * n.c() + n.t(0) + 2) >> 2
                    } else {
                        (n.l(y - 1) + 2 * n.l(y - 2) + n.l(y - 3) + 2) >> 2
                    };
                    pred[y as usize][x as usize] = v as u8;
                }
            }
        }
        // 8.3.1.2.7
        HorizontalDown => {
            need(
                n.top.is_some() && n.left.is_some() && n.corner.is_some(),
                "p[x, -1], p[-1, y] and p[-1, -1]",
            )?;
            for y in 0..4i32 {
                for x in 0..4i32 {
                    let z = 2 * y - x;
                    let v_index = y - x / 2; // y - (x >> 1)
                    let v = if z >= 0 && z % 2 == 0 {
                        (n.l(v_index - 1) + n.l(v_index) + 1) >> 1
                    } else if z >= 0 {
                        (n.l(v_index - 2) + 2 * n.l(v_index - 1) + n.l(v_index) + 2) >> 2
                    } else if z == -1 {
                        (n.l(0) + 2 * n.c() + n.t(0) + 2) >> 2
                    } else {
                        (n.t(x - 1) + 2 * n.t(x - 2) + n.t(x - 3) + 2) >> 2
                    };
                    pred[y as usize][x as usize] = v as u8;
                }
            }
        }
        // 8.3.1.2.8
        VerticalLeft => {
            need(n.top.is_some(), "p[x, -1]")?;
            for y in 0..4 {
                for x in 0..4 {
                    let h = (x + y / 2) as i32; // x + (y >> 1)
                    pred[y][x] = if y % 2 == 0 {
                        ((n.t(h) + n.t(h + 1) + 1) >> 1) as u8
                    } else {
                        ((n.t(h) + 2 * n.t(h + 1) + n.t(h + 2) + 2) >> 2) as u8
                    };
                }
            }
        }
        // 8.3.1.2.9
        HorizontalUp => {
            need(n.left.is_some(), "p[-1, y]")?;
            for y in 0..4 {
                for x in 0..4 {
                    let z = (x + 2 * y) as i32;
                    let v_index = (y + x / 2) as i32; // y + (x >> 1)
                    let v = if z < 5 && z % 2 == 0 {
                        (n.l(v_index) + n.l(v_index + 1) + 1) >> 1
                    } else if z < 5 {
                        (n.l(v_index) + 2 * n.l(v_index + 1) + n.l(v_index + 2) + 2) >> 2
                    } else if z == 5 {
                        (n.l(2) + 3 * n.l(3) + 2) >> 2
                    } else {
                        n.l(3)
                    };
                    pred[y][x] = v as u8;
                }
            }
        }
    }
    Ok(pred)
}

/// The four `Intra_16x16` prediction modes (Table 8-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intra16x16PredMode {
    /// 0: `Intra_16x16_Vertical`.
    Vertical,
    /// 1: `Intra_16x16_Horizontal`.
    Horizontal,
    /// 2: `Intra_16x16_DC`.
    Dc,
    /// 3: `Intra_16x16_Plane`.
    Plane,
}

impl Intra16x16PredMode {
    /// Map an `Intra16x16PredMode` value 0..=3 onto the enum.
    pub fn from_value(value: u32) -> Result<Intra16x16PredMode> {
        use Intra16x16PredMode::*;
        Ok(match value {
            0 => Vertical,
            1 => Horizontal,
            2 => Dc,
            3 => Plane,
            other => {
                return Err(Error::corrupt(format!(
                    "H.264: Intra16x16PredMode = {other}"
                )));
            }
        })
    }
}

/// Clause 8.3.3: predict a 16x16 luma macroblock, `[y][x]`.
pub fn predict_16x16(
    mode: Intra16x16PredMode,
    top: Option<&[u8; 16]>,
    left: Option<&[u8; 16]>,
    corner: Option<u8>,
) -> Result<[[u8; 16]; 16]> {
    use Intra16x16PredMode::*;
    let mut pred = [[0u8; 16]; 16];
    match mode {
        // 8.3.3.1
        Vertical => {
            let top = top.ok_or_else(|| {
                Error::corrupt("H.264 Intra_16x16_Vertical: p[x, -1] is not available")
            })?;
            for row in pred.iter_mut() {
                *row = *top;
            }
        }
        // 8.3.3.2
        Horizontal => {
            let left = left.ok_or_else(|| {
                Error::corrupt("H.264 Intra_16x16_Horizontal: p[-1, y] is not available")
            })?;
            for (y, row) in pred.iter_mut().enumerate() {
                *row = [left[y]; 16];
            }
        }
        // 8.3.3.3
        Dc => {
            let sum = |s: &[u8; 16]| s.iter().map(|&v| v as i32).sum::<i32>();
            let value = match (top, left) {
                (Some(t), Some(l)) => (sum(t) + sum(l) + 16) >> 5,
                (None, Some(l)) => (sum(l) + 8) >> 4,
                (Some(t), None) => (sum(t) + 8) >> 4,
                (None, None) => 128,
            };
            pred = [[value as u8; 16]; 16];
        }
        // 8.3.3.4
        Plane => {
            let (top, left, corner) = match (top, left, corner) {
                (Some(t), Some(l), Some(c)) => (t, l, c as i32),
                _ => {
                    return Err(Error::corrupt(
                        "H.264 Intra_16x16_Plane: p[x, -1], p[-1, y] and p[-1, -1] are required",
                    ));
                }
            };
            // p[-1, -1] is p[6 - x', -1] at x' = 7 and p[-1, 6 - y'] at y' = 7.
            let t = |x: i32| -> i32 {
                if x < 0 {
                    corner
                } else {
                    top[x as usize] as i32
                }
            };
            let l = |y: i32| -> i32 {
                if y < 0 {
                    corner
                } else {
                    left[y as usize] as i32
                }
            };
            let mut h = 0i32;
            let mut v = 0i32;
            for i in 0..8i32 {
                h += (i + 1) * (t(8 + i) - t(6 - i));
                v += (i + 1) * (l(8 + i) - l(6 - i));
            }
            let a = 16 * (l(15) + t(15));
            let b = (5 * h + 32) >> 6;
            let c = (5 * v + 32) >> 6;
            for (y, row) in pred.iter_mut().enumerate() {
                for (x, sample) in row.iter_mut().enumerate() {
                    let value = (a + b * (x as i32 - 7) + c * (y as i32 - 7) + 16) >> 5;
                    *sample = value.clamp(0, 255) as u8;
                }
            }
        }
    }
    Ok(pred)
}

/// The four `intra_chroma_pred_mode` values (Table 8-5). Note the order differs
/// from the luma tables: DC is 0 here, vertical is 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntraChromaPredMode {
    /// 0: `Intra_Chroma_DC`.
    Dc,
    /// 1: `Intra_Chroma_Horizontal`.
    Horizontal,
    /// 2: `Intra_Chroma_Vertical`.
    Vertical,
    /// 3: `Intra_Chroma_Plane`.
    Plane,
}

impl IntraChromaPredMode {
    /// Map an `intra_chroma_pred_mode` value 0..=3 onto the enum.
    pub fn from_value(value: u32) -> Result<IntraChromaPredMode> {
        use IntraChromaPredMode::*;
        Ok(match value {
            0 => Dc,
            1 => Horizontal,
            2 => Vertical,
            3 => Plane,
            other => {
                return Err(Error::corrupt(format!(
                    "H.264: intra_chroma_pred_mode = {other}"
                )));
            }
        })
    }
}

/// Clause 8.3.4: predict one 8x8 chroma component of a macroblock (4:2:0),
/// `[y][x]`.
pub fn predict_chroma_8x8(
    mode: IntraChromaPredMode,
    top: Option<&[u8; 8]>,
    left: Option<&[u8; 8]>,
    corner: Option<u8>,
) -> Result<[[u8; 8]; 8]> {
    use IntraChromaPredMode::*;
    let mut pred = [[0u8; 8]; 8];
    match mode {
        // 8.3.4.1: DC is per 4x4 block, and which neighbour a block prefers
        // depends on where the block sits in the macroblock.
        Dc => {
            for blk in 0..4usize {
                let (x_o, y_o) = ((blk % 2) * 4, (blk / 2) * 4);
                let sum_top = top.map(|t| t[x_o..x_o + 4].iter().map(|&v| v as i32).sum::<i32>());
                let sum_left = left.map(|l| l[y_o..y_o + 4].iter().map(|&v| v as i32).sum::<i32>());
                let value = if (x_o, y_o) == (0, 0) || (x_o > 0 && y_o > 0) {
                    match (sum_top, sum_left) {
                        (Some(t), Some(l)) => (t + l + 4) >> 3,
                        (None, Some(l)) => (l + 2) >> 2,
                        (Some(t), None) => (t + 2) >> 2,
                        (None, None) => 128,
                    }
                } else if x_o > 0 {
                    // Top right block: the row above is preferred.
                    match (sum_top, sum_left) {
                        (Some(t), _) => (t + 2) >> 2,
                        (None, Some(l)) => (l + 2) >> 2,
                        (None, None) => 128,
                    }
                } else {
                    // Bottom left block: the column to the left is preferred.
                    match (sum_left, sum_top) {
                        (Some(l), _) => (l + 2) >> 2,
                        (None, Some(t)) => (t + 2) >> 2,
                        (None, None) => 128,
                    }
                };
                for row in pred.iter_mut().skip(y_o).take(4) {
                    for sample in row.iter_mut().skip(x_o).take(4) {
                        *sample = value as u8;
                    }
                }
            }
        }
        // 8.3.4.2
        Horizontal => {
            let left = left.ok_or_else(|| {
                Error::corrupt("H.264 Intra_Chroma_Horizontal: p[-1, y] is not available")
            })?;
            for (y, row) in pred.iter_mut().enumerate() {
                *row = [left[y]; 8];
            }
        }
        // 8.3.4.3
        Vertical => {
            let top = top.ok_or_else(|| {
                Error::corrupt("H.264 Intra_Chroma_Vertical: p[x, -1] is not available")
            })?;
            for row in pred.iter_mut() {
                *row = *top;
            }
        }
        // 8.3.4.4
        Plane => {
            let (top, left, corner) = match (top, left, corner) {
                (Some(t), Some(l), Some(c)) => (t, l, c as i32),
                _ => {
                    return Err(Error::corrupt(
                        "H.264 Intra_Chroma_Plane: p[x, -1], p[-1, y] and p[-1, -1] are required",
                    ));
                }
            };
            let t = |x: i32| -> i32 {
                if x < 0 {
                    corner
                } else {
                    top[x as usize] as i32
                }
            };
            let l = |y: i32| -> i32 {
                if y < 0 {
                    corner
                } else {
                    left[y as usize] as i32
                }
            };
            // xCF and yCF are both 0 for 4:2:0, so the sums run over 0..3.
            let mut h = 0i32;
            let mut v = 0i32;
            for i in 0..4i32 {
                h += (i + 1) * (t(4 + i) - t(2 - i));
                v += (i + 1) * (l(4 + i) - l(2 - i));
            }
            let a = 16 * (l(7) + t(7));
            let b = (34 * h + 32) >> 6;
            let c = (34 * v + 32) >> 6;
            for (y, row) in pred.iter_mut().enumerate() {
                for (x, sample) in row.iter_mut().enumerate() {
                    let value = (a + b * (x as i32 - 3) + c * (y as i32 - 3) + 16) >> 5;
                    *sample = value.clamp(0, 255) as u8;
                }
            }
        }
    }
    Ok(pred)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn neighbours() -> Neighbours4x4 {
        Neighbours4x4 {
            left: Some([10, 20, 30, 40]),
            top: Some([1, 2, 3, 4]),
            top_right: Some([5, 6, 7, 8]),
            corner: Some(100),
        }
    }

    #[test]
    fn vertical_and_horizontal_copy_their_edge() {
        let n = neighbours();
        let v = predict_4x4(Intra4x4PredMode::Vertical, &n).unwrap();
        assert_eq!(v, [[1, 2, 3, 4]; 4]);
        let h = predict_4x4(Intra4x4PredMode::Horizontal, &n).unwrap();
        assert_eq!(h, [[10; 4], [20; 4], [30; 4], [40; 4]]);
        // A mode whose neighbours are missing is refused, not guessed.
        let bare = Neighbours4x4::default();
        assert!(predict_4x4(Intra4x4PredMode::Vertical, &bare).is_err());
        assert!(predict_4x4(Intra4x4PredMode::DiagonalDownRight, &bare).is_err());
    }

    #[test]
    fn dc_uses_whichever_neighbours_exist() {
        let n = neighbours();
        // (1+2+3+4) + (10+20+30+40) + 4 = 114, >> 3 = 14.
        assert_eq!(predict_4x4(Intra4x4PredMode::Dc, &n).unwrap()[0][0], 14);
        let top_only = Neighbours4x4 { left: None, ..n };
        // (1+2+3+4+2) >> 2 = 3.
        assert_eq!(
            predict_4x4(Intra4x4PredMode::Dc, &top_only).unwrap()[0][0],
            3
        );
        let left_only = Neighbours4x4 {
            top: None,
            top_right: None,
            ..n
        };
        // (10+20+30+40+2) >> 2 = 25.
        assert_eq!(
            predict_4x4(Intra4x4PredMode::Dc, &left_only).unwrap()[0][0],
            25
        );
        assert_eq!(
            predict_4x4(Intra4x4PredMode::Dc, &Neighbours4x4::default()).unwrap()[0][0],
            128,
            "no neighbours at all: mid grey"
        );
    }

    #[test]
    fn top_right_substitution_repeats_the_last_top_sample() {
        // With p[4..7, -1] unavailable, Diagonal_Down_Left must see p[3, -1]
        // four times over, which makes its bottom right sample p[3, -1] exactly.
        let n = Neighbours4x4 {
            left: Some([0; 4]),
            top: Some([10, 20, 30, 40]),
            top_right: None,
            corner: Some(0),
        };
        let p = predict_4x4(Intra4x4PredMode::DiagonalDownLeft, &n).unwrap();
        assert_eq!(p[3][3], 40);
        // At (x, y) = (3, 0) all three taps are the substituted p[3, -1].
        assert_eq!(p[0][3], 40);
        // At (0, 0) the real top samples are still used: (10 + 40 + 30 + 2) >> 2.
        assert_eq!(p[0][0], 20);
    }

    #[test]
    fn every_4x4_mode_is_a_weighted_average_of_its_neighbours() {
        // A flat neighbourhood must predict that same flat value in every mode:
        // all nine filters have unit gain, so this catches a mistyped weight.
        let n = Neighbours4x4 {
            left: Some([77; 4]),
            top: Some([77; 4]),
            top_right: Some([77; 4]),
            corner: Some(77),
        };
        for value in 0..9 {
            let mode = Intra4x4PredMode::from_value(value).unwrap();
            let p = predict_4x4(mode, &n).unwrap();
            assert_eq!(p, [[77; 4]; 4], "mode {mode:?}");
        }
    }

    #[test]
    fn intra_16x16_plane_is_flat_for_a_flat_edge() {
        let top = [64u8; 16];
        let left = [64u8; 16];
        let p =
            predict_16x16(Intra16x16PredMode::Plane, Some(&top), Some(&left), Some(64)).unwrap();
        assert_eq!(p, [[64u8; 16]; 16]);
        // A linear ramp along the top stays linear across the block.
        let ramp: [u8; 16] = std::array::from_fn(|i| (i * 4) as u8);
        let p = predict_16x16(
            Intra16x16PredMode::Plane,
            Some(&ramp),
            Some(&[30; 16]),
            Some(30),
        )
        .unwrap();
        assert!(p[0][0] < p[0][15], "gradient follows the top edge");
        assert_eq!(
            predict_16x16(Intra16x16PredMode::Dc, None, None, None).unwrap()[0][0],
            128
        );
    }

    #[test]
    fn chroma_dc_blocks_prefer_different_neighbours() {
        let top: [u8; 8] = [10, 10, 10, 10, 200, 200, 200, 200];
        let left: [u8; 8] = [20, 20, 20, 20, 100, 100, 100, 100];
        let p =
            predict_chroma_8x8(IntraChromaPredMode::Dc, Some(&top), Some(&left), Some(0)).unwrap();
        // Block 0 averages both: (40 + 80 + 4) >> 3 = 15.
        assert_eq!(p[0][0], 15);
        // Block 1 (top right) uses the row above: (800 + 2) >> 2 = 200.
        assert_eq!(p[0][4], 200);
        // Block 2 (bottom left) uses the column to the left: (400 + 2) >> 2 = 100.
        assert_eq!(p[4][0], 100);
        // Block 3 averages both again: (800 + 400 + 4) >> 3 = 150.
        assert_eq!(p[4][4], 150);
    }
}

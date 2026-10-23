//! Macroblock partitioning and motion vector derivation (spec 7.4.5, 8.4.1).
//!
//! Motion data lives in the picture's per-4x4-block arrays, written as each
//! partition is derived. Neighbour derivation (8.4.1.3.2) then reads those same
//! arrays, which is what makes "the partition inside this macroblock has not
//! been decoded yet" expressible: a bitmask of the blocks written so far stands
//! in for the availability rule of clause 6.4.11.7, exactly rather than
//! approximately.

use crate::dpb::{BLK_DIRECT, BLK_INTRA, BLK_SKIP, Picture};
use crate::sample::Sample;

/// Prediction mode of a macroblock or sub-macroblock partition (Table 7-13,
/// 7-14, 7-17, 7-18).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Pred {
    /// Pred_L0.
    L0,
    /// Pred_L1.
    L1,
    /// BiPred.
    Bi,
    /// Direct (B_Skip, B_Direct_16x16, B_Direct_8x8).
    Direct,
}

impl Pred {
    /// `(predFlagL0, predFlagL1)`; direct mode resolves later.
    #[inline]
    pub(crate) fn uses(self, list: usize) -> bool {
        match self {
            Pred::L0 => list == 0,
            Pred::L1 => list == 1,
            Pred::Bi => true,
            Pred::Direct => false,
        }
    }
}

/// The partitioning of one inter macroblock.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MbShape {
    /// `NumMbPart( mb_type )`.
    pub parts: usize,
    /// `MbPartWidth`/`MbPartHeight` in 4x4 blocks.
    pub w: usize,
    pub h: usize,
    /// `MbPartPredMode` of each partition.
    pub pred: [Pred; 2],
    /// mb_type is P_8x8/P_8x8ref0/B_8x8: sub-macroblock types follow.
    pub sub: bool,
}

const fn shape(parts: usize, w: usize, h: usize, p0: Pred, p1: Pred) -> MbShape {
    MbShape {
        parts,
        w,
        h,
        pred: [p0, p1],
        sub: false,
    }
}

const SUB8X8: MbShape = MbShape {
    parts: 4,
    w: 2,
    h: 2,
    pred: [Pred::L0, Pred::L0],
    sub: true,
};

/// Table 7-13, mb_type 0..4 of a P or SP slice.
pub(crate) const P_SHAPES: [MbShape; 5] = [
    shape(1, 4, 4, Pred::L0, Pred::L0),
    shape(2, 4, 2, Pred::L0, Pred::L0),
    shape(2, 2, 4, Pred::L0, Pred::L0),
    SUB8X8,
    SUB8X8, // P_8x8ref0: same partitioning, ref_idx inferred 0.
];

/// Table 7-14, mb_type 0..22 of a B slice.
pub(crate) const B_SHAPES: [MbShape; 23] = [
    shape(1, 4, 4, Pred::Direct, Pred::Direct),
    shape(1, 4, 4, Pred::L0, Pred::L0),
    shape(1, 4, 4, Pred::L1, Pred::L1),
    shape(1, 4, 4, Pred::Bi, Pred::Bi),
    shape(2, 4, 2, Pred::L0, Pred::L0),
    shape(2, 2, 4, Pred::L0, Pred::L0),
    shape(2, 4, 2, Pred::L1, Pred::L1),
    shape(2, 2, 4, Pred::L1, Pred::L1),
    shape(2, 4, 2, Pred::L0, Pred::L1),
    shape(2, 2, 4, Pred::L0, Pred::L1),
    shape(2, 4, 2, Pred::L1, Pred::L0),
    shape(2, 2, 4, Pred::L1, Pred::L0),
    shape(2, 4, 2, Pred::L0, Pred::Bi),
    shape(2, 2, 4, Pred::L0, Pred::Bi),
    shape(2, 4, 2, Pred::L1, Pred::Bi),
    shape(2, 2, 4, Pred::L1, Pred::Bi),
    shape(2, 4, 2, Pred::Bi, Pred::L0),
    shape(2, 2, 4, Pred::Bi, Pred::L0),
    shape(2, 4, 2, Pred::Bi, Pred::L1),
    shape(2, 2, 4, Pred::Bi, Pred::L1),
    shape(2, 4, 2, Pred::Bi, Pred::Bi),
    shape(2, 2, 4, Pred::Bi, Pred::Bi),
    SUB8X8,
];

/// The partitioning of one 8x8 sub-macroblock.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SubShape {
    /// `NumSubMbPart( sub_mb_type )`.
    pub parts: usize,
    /// `SubMbPartWidth`/`SubMbPartHeight` in 4x4 blocks.
    pub w: usize,
    pub h: usize,
    /// `SubMbPredMode`.
    pub pred: Pred,
}

const fn sub(parts: usize, w: usize, h: usize, pred: Pred) -> SubShape {
    SubShape { parts, w, h, pred }
}

/// Table 7-17, sub_mb_type of a P macroblock.
pub(crate) const P_SUB: [SubShape; 4] = [
    sub(1, 2, 2, Pred::L0),
    sub(2, 2, 1, Pred::L0),
    sub(2, 1, 2, Pred::L0),
    sub(4, 1, 1, Pred::L0),
];

/// Table 7-18, sub_mb_type of a B macroblock.
pub(crate) const B_SUB: [SubShape; 13] = [
    sub(4, 1, 1, Pred::Direct),
    sub(1, 2, 2, Pred::L0),
    sub(1, 2, 2, Pred::L1),
    sub(1, 2, 2, Pred::Bi),
    sub(2, 2, 1, Pred::L0),
    sub(2, 1, 2, Pred::L0),
    sub(2, 2, 1, Pred::L1),
    sub(2, 1, 2, Pred::L1),
    sub(2, 2, 1, Pred::Bi),
    sub(2, 1, 2, Pred::Bi),
    sub(4, 1, 1, Pred::L0),
    sub(4, 1, 1, Pred::L1),
    sub(4, 1, 1, Pred::Bi),
];

/// Motion data of one neighbouring partition (8.4.1.3.2).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Nb {
    /// The partition exists and has been decoded (clause 6.4.11.7).
    pub avail: bool,
    pub mv: [i16; 2],
    /// -1 when the neighbour does not use this list, or is intra.
    pub ref_idx: i8,
}

impl Default for Nb {
    /// An unavailable partition, which 8.4.1.3.2 gives reference index -1 —
    /// *not* 0. A derived `Default` would make it look like a neighbour on
    /// reference picture 0 and hand the median rules a phantom match.
    fn default() -> Nb {
        Nb {
            avail: false,
            mv: [0; 2],
            ref_idx: -1,
        }
    }
}

/// Where the current macroblock's motion data is being written, and which of
/// its 4x4 blocks already carry it.
pub(crate) struct MvCtx {
    pub mb_x: usize,
    pub mb_y: usize,
    pub slice_id: u16,
    /// Bit `y * 4 + x` set once block (x, y) of this macroblock is derived.
    pub written: u16,
    /// The macroblock's place in an MBAFF frame (inert otherwise).
    pub pos: crate::mbaff::MbPos,
}

impl MvCtx {
    /// Resolve the 4x4 block at picture block coordinates `(bx, by)` — the
    /// raster neighbour of a block of the current macroblock — to its index
    /// in the per-block arrays, or `None` when it is unavailable (outside the
    /// picture, in another slice, not yet decoded, or not a neighbour Table
    /// 6-4 allows). In an MBAFF frame the coordinates only say *which*
    /// neighbour (left, above, above-right, above-left) and row; the block
    /// actually read is the one 6.4.12.2 derives. The flag is the neighbouring
    /// macroblock's field mode (the current macroblock's for blocks inside it).
    #[inline]
    fn resolve<S: Sample>(
        &self,
        pic: &Picture<S>,
        bx: i32,
        by: i32,
        diag: bool,
    ) -> Option<(usize, bool)> {
        let w4 = (pic.mb_w * 4) as i32;
        if self.pos.active {
            return self.resolve_mbaff(pic, bx, by, diag);
        }
        let h4 = (pic.mb_h * 4) as i32;
        if bx < 0 || by < 0 || bx >= w4 || by >= h4 {
            return None;
        }
        let (mbx, mby) = (bx as usize / 4, by as usize / 4);
        let addr = mby * pic.mb_w + mbx;
        if mbx == self.mb_x && mby == self.mb_y {
            let within = (by as usize % 4) * 4 + bx as usize % 4;
            if self.written & (1 << within) == 0 {
                return None;
            }
        } else if pic.mb_slice[addr] != self.slice_id {
            return None;
        }
        Some((by as usize * pic.mb_w * 4 + bx as usize, false))
    }

    /// [`MvCtx::resolve`] for an MBAFF frame (Table 6-4).
    #[inline(never)]
    fn resolve_mbaff<S: Sample>(
        &self,
        pic: &Picture<S>,
        bx: i32,
        by: i32,
        diag: bool,
    ) -> Option<(usize, bool)> {
        let w4 = (pic.mb_w * 4) as i32;
        {
            let (rx, ry) = (bx - 4 * self.mb_x as i32, by - 4 * self.mb_y as i32);
            if (0..4).contains(&rx) && (0..4).contains(&ry) {
                if self.written & (1 << (ry * 4 + rx)) == 0 {
                    return None;
                }
                return Some((by as usize * w4 as usize + bx as usize, self.pos.field));
            }
            let xn = if rx < 0 {
                -1
            } else if rx >= 4 {
                16
            } else {
                rx * 4
            };
            // Neighbour D of a block below the top row sits at luma row
            // `y - 1`, the last row of the block above; the others at the
            // block's own first row.
            let yn = if ry < 0 {
                -1
            } else if diag {
                ry * 4 + 3
            } else {
                ry * 4
            };
            let l = crate::mbaff::locate(pic, &self.pos, xn, yn, 16, 16)?;
            let gbx = (l.slot % pic.mb_w) * 4 + (l.xw as usize >> 2);
            let gby = (l.slot / pic.mb_w) * 4 + (l.yw as usize >> 2);
            Some((gby * w4 as usize + gbx, l.field))
        }
    }

    /// [`MvCtx::at`] outside MBAFF frames: raster addressing.
    #[inline]
    fn at_plain<S: Sample>(&self, pic: &Picture<S>, bx: i32, by: i32, list: usize) -> Nb {
        let w4 = (pic.mb_w * 4) as i32;
        let h4 = (pic.mb_h * 4) as i32;
        if bx < 0 || by < 0 || bx >= w4 || by >= h4 {
            return Nb::default();
        }
        let (mbx, mby) = (bx as usize / 4, by as usize / 4);
        let addr = mby * pic.mb_w + mbx;
        if mbx == self.mb_x && mby == self.mb_y {
            let within = (by as usize % 4) * 4 + bx as usize % 4;
            if self.written & (1 << within) == 0 {
                return Nb::default();
            }
        } else if pic.mb_slice[addr] != self.slice_id {
            return Nb::default();
        }
        let idx = by as usize * pic.mb_w * 4 + bx as usize;
        if pic.blk[idx] & BLK_INTRA != 0 {
            // Available as a partition, but contributes no motion (8.4.1.3.2).
            return Nb {
                avail: true,
                mv: [0; 2],
                ref_idx: -1,
            };
        }
        Nb {
            avail: true,
            mv: pic.mv[idx][list],
            ref_idx: pic.ref_idx[idx][list],
        }
    }

    /// Motion data of the 4x4 block at picture block coordinates `(bx, by)`,
    /// for list `list`.
    #[inline]
    pub(crate) fn at<S: Sample>(&self, pic: &Picture<S>, bx: i32, by: i32, list: usize) -> Nb {
        self.at_d(pic, bx, by, list, false)
    }

    /// [`MvCtx::at`] for neighbour D when `diag` (the block diagonally above
    /// and left, `(bx - 1, by - 1)`).
    #[inline]
    fn at_d<S: Sample>(&self, pic: &Picture<S>, bx: i32, by: i32, list: usize, diag: bool) -> Nb {
        let Some((idx, nfield)) = self.resolve(pic, bx, by, diag) else {
            return Nb::default();
        };
        if pic.blk[idx] & BLK_INTRA != 0 {
            // Available as a partition, but contributes no motion (8.4.1.3.2).
            return Nb {
                avail: true,
                mv: [0; 2],
                ref_idx: -1,
            };
        }
        let mut nb = Nb {
            avail: true,
            mv: pic.mv[idx][list],
            ref_idx: pic.ref_idx[idx][list],
        };
        if self.pos.active && nfield != self.pos.field && nb.ref_idx >= 0 {
            nb = self.rescale(nb);
        }
        nb
    }

    #[inline(never)]
    fn rescale(&self, mut nb: Nb) -> Nb {
        {
            // 8.4.1.3.2: motion of a neighbour of the other mode is converted
            // to the current macroblock's units.
            if self.pos.field {
                nb.mv[1] /= 2;
                nb.ref_idx *= 2;
            } else {
                nb.mv[1] = nb.mv[1].wrapping_mul(2);
                nb.ref_idx >>= 1;
            }
        }
        nb
    }

    /// Neighbours A, B and C of a partition whose top-left 4x4 block is at
    /// macroblock-relative `(px, py)` and whose `predPartWidth` is `pw` blocks
    /// (clause 6.4.11.7, with the C-to-D substitution of 8-214).
    #[inline]
    pub(crate) fn neighbours<S: Sample>(
        &self,
        pic: &Picture<S>,
        px: usize,
        py: usize,
        pw: usize,
        list: usize,
    ) -> [Nb; 3] {
        let bx = (self.mb_x * 4 + px) as i32;
        let by = (self.mb_y * 4 + py) as i32;
        if self.pos.active {
            return self.neighbours_mbaff(pic, bx, by, pw, list);
        }
        let a = self.at_plain(pic, bx - 1, by, list);
        let b = self.at_plain(pic, bx, by - 1, list);
        let mut c = self.at_plain(pic, bx + pw as i32, by - 1, list);
        if !c.avail {
            c = self.at_plain(pic, bx - 1, by - 1, list);
        }
        [a, b, c]
    }

    #[inline(never)]
    fn neighbours_mbaff<S: Sample>(
        &self,
        pic: &Picture<S>,
        bx: i32,
        by: i32,
        pw: usize,
        list: usize,
    ) -> [Nb; 3] {
        let a = self.at_d(pic, bx - 1, by, list, false);
        let b = self.at_d(pic, bx, by - 1, list, false);
        let mut c = self.at_d(pic, bx + pw as i32, by - 1, list, false);
        if !c.avail {
            c = self.at_d(pic, bx - 1, by - 1, list, true);
        }
        [a, b, c]
    }
}

#[inline]
fn median(a: i16, b: i16, c: i16) -> i16 {
    a.max(b).min(a.min(b).max(c))
}

/// Motion vector prediction (clause 8.4.1.3), including the directional
/// segmentation shortcuts of Equations 8-203 to 8-206.
///
/// `part_w`/`part_h` are `MbPartWidth`/`MbPartHeight` in 4x4 blocks and only
/// select those shortcuts; `part` is the macroblock partition index.
pub(crate) fn predict_mv(
    n: &[Nb; 3],
    ref_idx: i8,
    part_w: usize,
    part_h: usize,
    part: usize,
) -> [i16; 2] {
    let [a, mut b, mut c] = *n;
    match (part_w, part_h, part) {
        (4, 2, 0) if b.ref_idx == ref_idx => return b.mv,
        (4, 2, 1) if a.ref_idx == ref_idx => return a.mv,
        (2, 4, 0) if a.ref_idx == ref_idx => return a.mv,
        (2, 4, 1) if c.ref_idx == ref_idx => return c.mv,
        _ => {}
    }
    // 8.4.1.3.1 step 1.
    if !b.avail && !c.avail && a.avail {
        b = a;
        c = a;
    }
    let matches = u8::from(a.ref_idx == ref_idx)
        + u8::from(b.ref_idx == ref_idx)
        + u8::from(c.ref_idx == ref_idx);
    if matches == 1 {
        if a.ref_idx == ref_idx {
            return a.mv;
        }
        if b.ref_idx == ref_idx {
            return b.mv;
        }
        return c.mv;
    }
    [
        median(a.mv[0], b.mv[0], c.mv[0]),
        median(a.mv[1], b.mv[1], c.mv[1]),
    ]
}

/// `MinPositive` (Equation 8-187).
#[inline]
pub(crate) fn min_positive(x: i8, y: i8) -> i8 {
    if x >= 0 && y >= 0 { x.min(y) } else { x.max(y) }
}

/// Record one 4x4 block's motion data into the picture arrays.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_block<S: Sample>(
    pic: &mut Picture<S>,
    ctx: &mut MvCtx,
    px: usize,
    py: usize,
    mv: [[i16; 2]; 2],
    ref_idx: [i8; 2],
    ref_id: [i32; 2],
    flags: u8,
) {
    let bx = ctx.mb_x * 4 + px;
    let by = ctx.mb_y * 4 + py;
    let idx = by * pic.mb_w * 4 + bx;
    pic.mv[idx] = mv;
    pic.ref_idx[idx] = ref_idx;
    pic.ref_id[idx] = ref_id;
    pic.blk[idx] = flags;
    ctx.written |= 1 << (py * 4 + px);
}

/// Mark every 4x4 block of the current macroblock as intra, clearing the motion
/// data an inter neighbour would otherwise read (8.4.1.3.2, 9.3.3.1.1.7).
pub(crate) fn write_intra_mb<S: Sample>(pic: &mut Picture<S>, mb_x: usize, mb_y: usize) {
    let w4 = pic.mb_w * 4;
    for py in 0..4 {
        let base = (mb_y * 4 + py) * w4 + mb_x * 4;
        for idx in base..base + 4 {
            pic.mv[idx] = [[0; 2]; 2];
            pic.ref_idx[idx] = [-1; 2];
            pic.ref_id[idx] = [-1; 2];
            pic.mvd_abs[idx] = [0; 4];
            pic.blk[idx] = BLK_INTRA;
        }
    }
}

/// Record `Abs( mvd )` for one 4x4 block, saturated at the largest value the
/// context thresholds of 9.3.3.1.1.7 can distinguish.
#[inline]
pub(crate) fn write_mvd<S: Sample>(
    pic: &mut Picture<S>,
    mb_x: usize,
    mb_y: usize,
    px: usize,
    py: usize,
    list: usize,
    mvd: [i16; 2],
) {
    let idx = (mb_y * 4 + py) * pic.mb_w * 4 + mb_x * 4 + px;
    let e = &mut pic.mvd_abs[idx];
    e[list * 2] = mvd[0].unsigned_abs().min(127) as u8;
    e[list * 2 + 1] = mvd[1].unsigned_abs().min(127) as u8;
}

/// `absMvdCompN` of the neighbouring partition at picture block coordinates
/// `(bx, by)` (9.3.3.1.1.7): zero when the neighbour is unavailable, skipped,
/// intra or direct coded.
pub(crate) fn neighbour_mvd<S: Sample>(
    pic: &Picture<S>,
    ctx: &MvCtx,
    bx: i32,
    by: i32,
    list: usize,
) -> [u32; 2] {
    let Some((idx, nfield)) = ctx.resolve(pic, bx, by, false) else {
        return [0; 2];
    };
    if pic.blk[idx] & (BLK_INTRA | BLK_SKIP | BLK_DIRECT) != 0 || pic.ref_idx[idx][list] < 0 {
        return [0; 2];
    }
    let e = pic.mvd_abs[idx];
    let mut out = [u32::from(e[list * 2]), u32::from(e[list * 2 + 1])];
    if ctx.pos.active && nfield != ctx.pos.field {
        // The vertical component is compared in the current macroblock's
        // units (9.3.3.1.1.7).
        if ctx.pos.field {
            out[1] /= 2;
        } else {
            out[1] *= 2;
        }
    }
    out
}

/// `condTermFlagN` for ref_idx_lX (9.3.3.1.1.6).
pub(crate) fn ref_idx_cond<S: Sample>(
    pic: &Picture<S>,
    ctx: &MvCtx,
    bx: i32,
    by: i32,
    list: usize,
) -> usize {
    let Some((idx, nfield)) = ctx.resolve(pic, bx, by, false) else {
        return 0;
    };
    if pic.blk[idx] & (BLK_INTRA | BLK_SKIP | BLK_DIRECT) != 0 {
        return 0;
    }
    // refIdxZeroFlagN: a frame macroblock reading a field neighbour in an
    // MBAFF frame compares against 1, because field reference indices count
    // fields.
    let limit = if ctx.pos.active && !ctx.pos.field && nfield {
        1
    } else {
        0
    };
    usize::from(pic.ref_idx[idx][list] > limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nb(avail: bool, mv: [i16; 2], ref_idx: i8) -> Nb {
        Nb { avail, mv, ref_idx }
    }

    /// With exactly one neighbour on the same reference picture, that
    /// neighbour's vector is the prediction (Equation 8-211).
    #[test]
    fn single_matching_reference_wins() {
        let n = [
            nb(true, [4, 4], 1),
            nb(true, [8, 8], 0),
            nb(true, [12, 12], 2),
        ];
        assert_eq!(predict_mv(&n, 0, 4, 4, 0), [8, 8]);
    }

    /// Otherwise the prediction is the component-wise median (8-212, 8-213).
    #[test]
    fn median_of_three_when_several_match() {
        let n = [
            nb(true, [4, -8], 0),
            nb(true, [8, 2], 0),
            nb(true, [-2, 6], 1),
        ];
        assert_eq!(predict_mv(&n, 0, 4, 4, 0), [4, 2]);
    }

    /// B and C unavailable with A available copies A into both, which turns the
    /// median into A (8-207 to 8-210).
    #[test]
    fn unavailable_b_and_c_fall_back_to_a() {
        let n = [
            nb(true, [6, -6], 3),
            nb(false, [0, 0], -1),
            nb(false, [0, 0], -1),
        ];
        assert_eq!(predict_mv(&n, 0, 4, 4, 0), [6, -6]);
        // With refIdx 3 the single-match rule fires first, same answer.
        assert_eq!(predict_mv(&n, 3, 4, 4, 0), [6, -6]);
    }

    /// The 16x8 and 8x16 shortcuts bypass the median entirely.
    #[test]
    fn directional_segmentation_shortcuts() {
        let n = [
            nb(true, [1, 1], 0),
            nb(true, [2, 2], 0),
            nb(true, [3, 3], 0),
        ];
        assert_eq!(predict_mv(&n, 0, 4, 2, 0), [2, 2], "16x8 part 0 takes B");
        assert_eq!(predict_mv(&n, 0, 4, 2, 1), [1, 1], "16x8 part 1 takes A");
        assert_eq!(predict_mv(&n, 0, 2, 4, 0), [1, 1], "8x16 part 0 takes A");
        assert_eq!(predict_mv(&n, 0, 2, 4, 1), [3, 3], "8x16 part 1 takes C");
        // A shortcut that does not match the reference index falls through.
        assert_eq!(predict_mv(&n, 1, 4, 2, 0), [2, 2], "median of 1,2,3");
    }

    #[test]
    fn min_positive_prefers_the_non_negative() {
        assert_eq!(min_positive(2, 5), 2);
        assert_eq!(min_positive(-1, 5), 5);
        assert_eq!(min_positive(-1, -1), -1);
        assert_eq!(min_positive(0, -1), 0);
    }

    /// Every B macroblock type maps to the partition shape Table 7-14 states.
    #[test]
    fn b_shapes_match_table_7_14() {
        assert_eq!(B_SHAPES[0].pred[0], Pred::Direct);
        assert_eq!((B_SHAPES[3].parts, B_SHAPES[3].pred[0]), (1, Pred::Bi));
        // The 16x8 / 8x16 pairs alternate, starting at mb_type 4.
        #[allow(clippy::needless_range_loop)]
        for t in 4..22 {
            let s = B_SHAPES[t];
            assert_eq!(s.parts, 2, "mb_type {t}");
            let (w, h) = if t % 2 == 0 { (4, 2) } else { (2, 4) };
            assert_eq!((s.w, s.h), (w, h), "mb_type {t}");
        }
        assert!(B_SHAPES[22].sub);
        assert!(P_SHAPES[3].sub && P_SHAPES[4].sub);
    }
}

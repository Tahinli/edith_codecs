//! Macroblock-adaptive frame/field decoding (MBAFF): macroblock pair
//! addressing and the neighbour derivation of clause 6.4.12.2 (Table 6-4).
//!
//! Layout. A frame picture with `MbaffFrameFlag` 1 codes macroblock *pairs*:
//! macroblock address `2 * pair + b` (b = 0 top, 1 bottom) of pair
//! `(px, pr)`. Every per-macroblock and per-4x4-block array of the
//! [`Picture`] is indexed by *slot*: the pair's top macroblock lives at slot
//! row `2 * pr` and its bottom macroblock at `2 * pr + 1`, whether the pair is
//! a frame pair or a field pair. A macroblock's own blocks are therefore
//! addressed exactly as in a progressive picture (block coordinates
//! `(4 * px + i, 4 * slot_y + j)`), and everything that reads *neighbouring*
//! data goes through [`locate`], the single implementation of Table 6-4.
//!
//! Samples are different: they live at their true picture position. A frame
//! macroblock covers 16 consecutive rows of its pair; a field macroblock
//! covers every second row of the 32-row pair, starting at row 0 (top) or 1
//! (bottom), which a stride of two rows and a first-row offset express.

use crate::dpb::Picture;
use crate::sample::Sample;

/// Where the macroblock being decoded sits in an MBAFF frame.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct MbPos {
    /// The picture is an MBAFF frame; every other field is meaningless when
    /// this is false.
    pub active: bool,
    /// Macroblock column.
    pub px: usize,
    /// Slot row: `2 * pair_row + is_bottom`.
    pub slot_y: usize,
    /// The macroblock belongs to a field macroblock pair.
    pub field: bool,
    /// The slice that is decoding it (neighbours must belong to the same one).
    pub slice_id: u16,
}

impl MbPos {
    /// True for the second macroblock of its pair.
    #[inline]
    pub(crate) fn bottom(&self) -> bool {
        self.slot_y & 1 == 1
    }
}

/// A neighbouring luma (or chroma) location resolved by Table 6-4.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Loc {
    /// Slot (index into the per-macroblock arrays) of `mbAddrN`.
    pub slot: usize,
    /// `xW` and `yW` inside that macroblock.
    pub xw: i32,
    pub yw: i32,
    /// `mbAddrN` is a field macroblock.
    pub field: bool,
}

/// Which macroblock of the left pair a frame macroblock at the bottom of its
/// pair takes as its upper-left neighbour when the left pair is a field pair.
/// Table 6-4 reads `mbAddrA` (the top field macroblock) with
/// `yM = (yN + maxH) >> 1`.
const D_FRAME_BOTTOM_FIELD_LEFT_BOTTOM: bool = true;

/// Clause 6.4.12.2: the macroblock and the position in it that cover the luma
/// (`maxw = maxh = 16`) or chroma (`8`) location `(xn, yn)` relative to the
/// current macroblock, or `None` when it is unavailable.
pub(crate) fn locate<S: Sample>(
    pic: &Picture<S>,
    p: &MbPos,
    xn: i32,
    yn: i32,
    maxw: i32,
    maxh: i32,
) -> Option<Loc> {
    let w = pic.mb_w;
    if xn >= 0 && xn < maxw && yn >= 0 && yn < maxh {
        return Some(Loc {
            slot: p.slot_y * w + p.px,
            xw: xn,
            yw: yn,
            field: p.field,
        });
    }
    if yn >= maxh || (xn >= maxw && yn >= 0) {
        return None;
    }
    let pr = (p.slot_y / 2) as i32;
    let bottom = p.bottom();
    // The pair at column offset `dx`, row offset `dy`: its top slot and
    // whether it is a field pair; None when it is outside the picture or in
    // another slice (clause 6.4.8 availability, per pair in MBAFF frames).
    let pair = |dx: i32, dy: i32| -> Option<(usize, bool)> {
        let x = p.px as i32 + dx;
        let y = pr + dy;
        if x < 0 || y < 0 || x >= w as i32 {
            return None;
        }
        let top = (2 * y as usize) * w + x as usize;
        (pic.mb_slice[top] == p.slice_id).then(|| (top, pic.mb_field[top] != 0))
    };
    let xw = (xn + maxw).rem_euclid(maxw);
    let mk = |top: usize, in_bottom: bool, ym: i32| -> Option<Loc> {
        let slot = if in_bottom { top + w } else { top };
        Some(Loc {
            slot,
            xw,
            yw: ym.rem_euclid(maxh),
            field: pic.mb_field[slot] != 0,
        })
    };
    let cur = p.field;
    if xn < 0 {
        if yn >= 0 {
            // A: the left pair.
            let (t, xf) = pair(-1, 0)?;
            return match (cur, bottom) {
                (false, false) => {
                    if !xf {
                        mk(t, false, yn)
                    } else {
                        mk(t, yn % 2 != 0, yn >> 1)
                    }
                }
                (false, true) => {
                    if !xf {
                        mk(t, true, yn)
                    } else {
                        mk(t, yn % 2 != 0, (yn + maxh) >> 1)
                    }
                }
                (true, false) => {
                    if xf {
                        mk(t, false, yn)
                    } else if yn < maxh / 2 {
                        mk(t, false, yn << 1)
                    } else {
                        mk(t, true, (yn << 1) - maxh)
                    }
                }
                (true, true) => {
                    if xf {
                        mk(t, true, yn)
                    } else if yn < maxh / 2 {
                        mk(t, false, (yn << 1) + 1)
                    } else {
                        mk(t, true, (yn << 1) + 1 - maxh)
                    }
                }
            };
        }
        // D: above-left.
        return match (cur, bottom) {
            (false, false) => {
                let (t, _) = pair(-1, -1)?;
                mk(t, true, yn)
            }
            (false, true) => {
                let (t, xf) = pair(-1, 0)?;
                if !xf {
                    mk(t, false, yn)
                } else {
                    mk(t, D_FRAME_BOTTOM_FIELD_LEFT_BOTTOM, (yn + maxh) >> 1)
                }
            }
            (true, false) => {
                let (t, xf) = pair(-1, -1)?;
                if xf {
                    mk(t, false, yn)
                } else {
                    mk(t, true, 2 * yn)
                }
            }
            (true, true) => {
                let (t, _) = pair(-1, -1)?;
                mk(t, true, yn)
            }
        };
    }
    if xn < maxw {
        // B: above.
        return match (cur, bottom) {
            (false, false) => {
                let (t, _) = pair(0, -1)?;
                mk(t, true, yn)
            }
            (false, true) => mk((p.slot_y - 1) * w + p.px, false, yn),
            (true, false) => {
                let (t, xf) = pair(0, -1)?;
                if xf {
                    mk(t, false, yn)
                } else {
                    mk(t, true, 2 * yn)
                }
            }
            (true, true) => {
                let (t, _) = pair(0, -1)?;
                mk(t, true, yn)
            }
        };
    }
    // C: above-right.
    match (cur, bottom) {
        (false, false) => {
            let (t, _) = pair(1, -1)?;
            mk(t, true, yn)
        }
        (false, true) => None,
        (true, false) => {
            let (t, xf) = pair(1, -1)?;
            if xf {
                mk(t, false, yn)
            } else {
                mk(t, true, 2 * yn)
            }
        }
        (true, true) => {
            let (t, _) = pair(1, -1)?;
            mk(t, true, yn)
        }
    }
}

/// The 4x4 luma block left of (`left`) or above (`!left`) the block at global
/// block coordinates `(bx, by)` of the current macroblock, as global block
/// coordinates plus the neighbouring macroblock's field flag.
pub(crate) fn nb_blk4<S: Sample>(
    pic: &Picture<S>,
    p: &MbPos,
    bx: usize,
    by: usize,
    left: bool,
) -> Option<(usize, usize, bool)> {
    let (lx, ly) = (bx - 4 * p.px, by - 4 * p.slot_y);
    if left {
        if lx > 0 {
            return Some((bx - 1, by, p.field));
        }
        let l = locate(pic, p, -1, ly as i32 * 4, 16, 16)?;
        return Some((
            (l.slot % pic.mb_w) * 4 + (l.xw as usize >> 2),
            (l.slot / pic.mb_w) * 4 + (l.yw as usize >> 2),
            l.field,
        ));
    }
    if ly > 0 {
        return Some((bx, by - 1, p.field));
    }
    let l = locate(pic, p, lx as i32 * 4, -1, 16, 16)?;
    Some((
        (l.slot % pic.mb_w) * 4 + (l.xw as usize >> 2),
        (l.slot / pic.mb_w) * 4 + (l.yw as usize >> 2),
        l.field,
    ))
}

/// The same for a chroma 4x4 block of the `(mb_w * 2) x (mb_h * cbh)` grid,
/// where `cbh` is the number of chroma 4x4 block rows per macroblock (2 for
/// 4:2:0, 4 for 4:2:2; the chroma macroblock is `8` wide and `4 * cbh` high).
pub(crate) fn nb_blk_c<S: Sample>(
    pic: &Picture<S>,
    p: &MbPos,
    cx: usize,
    cy: usize,
    left: bool,
) -> Option<(usize, usize)> {
    let cbh = pic.cmb_h / 4;
    let maxh = (4 * cbh) as i32;
    let (lx, ly) = (cx - 2 * p.px, cy - cbh * p.slot_y);
    if left {
        if lx > 0 {
            return Some((cx - 1, cy));
        }
        let l = locate(pic, p, -1, ly as i32 * 4, 8, maxh)?;
        return Some((
            (l.slot % pic.mb_w) * 2 + (l.xw as usize >> 2),
            (l.slot / pic.mb_w) * cbh + (l.yw as usize >> 2),
        ));
    }
    if ly > 0 {
        return Some((cx, cy - 1));
    }
    let l = locate(pic, p, lx as i32 * 4, -1, 8, maxh)?;
    Some((
        (l.slot % pic.mb_w) * 2 + (l.xw as usize >> 2),
        (l.slot / pic.mb_w) * cbh + (l.yw as usize >> 2),
    ))
}

/// Slot of the macroblock containing the neighbouring location, for the
/// macroblock-level neighbours A (`(-1, 0)`) and B (`(0, -1)`) of 6.4.9.
pub(crate) fn mb_a<S: Sample>(pic: &Picture<S>, p: &MbPos) -> Option<usize> {
    locate(pic, p, -1, 0, 16, 16).map(|l| l.slot)
}

/// See [`mb_a`].
pub(crate) fn mb_b<S: Sample>(pic: &Picture<S>, p: &MbPos) -> Option<usize> {
    locate(pic, p, 0, -1, 16, 16).map(|l| l.slot)
}

/// 7.4.4 inference of `mb_field_decoding_flag` for a pair that does not code
/// it: the left pair of the same slice, else the pair above, else frame.
pub(crate) fn infer_field<S: Sample>(
    pic: &Picture<S>,
    px: usize,
    pair_row: usize,
    slice_id: u16,
) -> bool {
    let w = pic.mb_w;
    if px > 0 {
        let t = (2 * pair_row) * w + px - 1;
        if pic.mb_slice[t] == slice_id {
            return pic.mb_field[t] != 0;
        }
    }
    if pair_row > 0 {
        let t = (2 * (pair_row - 1)) * w + px;
        if pic.mb_slice[t] == slice_id {
            return pic.mb_field[t] != 0;
        }
    }
    false
}

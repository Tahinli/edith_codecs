//! The motion field of a picture, the collocated lookup of temporal motion vector
//! prediction (8.5.3.2.8, 8.5.3.2.9) and the source that plugs the merge and AMVP
//! derivations (8.5.3.2) into the CU parser.
//!
//! [`MotionField`] holds the motion of every 4x4 luma block of the picture being
//! decoded. When the picture is finished, [`MotionField::compress`] keeps only the
//! entry of the top-left 4x4 of each 16x16 block, which is all 8.5.3.2.8 reads, and
//! attaches the slices' reference POCs and long-term flags. The result is a
//! [`ColField`], stored with the picture in the DPB.
//!
//! A block has no entry (`None`) when it is intra coded or not decoded yet. Both
//! make it unavailable as a spatial neighbour (6.4.2) and as a collocated block.
//! Decoding order is z-scan order, so "stored already" is the same test as the
//! z-scan availability of 6.4.1 for every neighbour that can be referenced.

use std::sync::Arc;

use super::cu_syntax::{CuGeometry, Motion, MotionSource, PredictionUnit};
use super::dpb::Dpb;
use super::pic::{PicState, PuMotion};
use super::rps::{RefEntry, RefPicLists};
use super::shared::SharedPic;
use super::{amvp, merge};
use ec_h265_syntax::slice::{SliceHeader, SliceType};
use ec_h265_syntax::{Pps, Sps};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// The motion of one prediction block: `predFlagLX`, `refIdxLX` and `mvLX` for both
/// lists. An unused list has reference index -1 and a zero vector, so two blocks
/// with the same motion compare equal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PbMotion {
    /// `predFlagL0` and `predFlagL1`.
    pub pred: [bool; 2],
    /// `refIdxL0` and `refIdxL1`, -1 for an unused list.
    pub ref_idx: [i8; 2],
    /// `mvL0` and `mvL1` (horizontal, vertical), quarter-sample units.
    pub mv: [[i16; 2]; 2],
}

impl PbMotion {
    /// Motion with the unused-list convention applied.
    pub fn new(pred: [bool; 2], ref_idx: [i8; 2], mv: [[i16; 2]; 2]) -> PbMotion {
        let mut m = PbMotion { pred, ref_idx, mv };
        for (list, &used) in pred.iter().enumerate() {
            if !used {
                m.ref_idx[list] = -1;
                m.mv[list] = [0, 0];
            }
        }
        m
    }

    /// The stored form of a derived [`Motion`]. Vectors are already 16-bit in value.
    pub fn from_motion(m: &Motion) -> PbMotion {
        PbMotion::new(m.pred_flags, m.ref_idx, [to16(m.mv[0]), to16(m.mv[1])])
    }

    /// The [`Motion`] a [`MotionSource`] returns for this block.
    pub fn to_motion(self) -> Motion {
        Motion {
            pred_flags: self.pred,
            ref_idx: self.ref_idx,
            mv: [to32(self.mv[0]), to32(self.mv[1])],
        }
    }
}

/// One stored entry: the motion of a block and the slice it was decoded in (the
/// index into that picture's [`SliceRefs`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldCell {
    /// The motion.
    pub motion: PbMotion,
    /// Index of the slice in the picture's slice list.
    pub slice: u16,
}

/// The reference lists of one slice, as the collocated lookup needs them later: the
/// POC and long-term marking of each entry at the time the slice was decoded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SliceRefs {
    /// `PicOrderCntVal` of each entry of `RefPicList0` and `RefPicList1`.
    pub poc: [Vec<i32>; 2],
    /// Whether each entry was a long-term reference.
    pub long_term: [Vec<bool>; 2],
}

impl SliceRefs {
    /// The POCs and long-term flags of a slice's lists.
    pub fn from_lists(lists: &RefPicLists) -> SliceRefs {
        let split = |list: &Vec<RefEntry>| {
            (
                list.iter().map(|e| e.poc).collect::<Vec<i32>>(),
                list.iter().map(|e| e.long_term).collect::<Vec<bool>>(),
            )
        };
        let (poc0, lt0) = split(&lists.l0);
        let (poc1, lt1) = split(&lists.l1);
        SliceRefs {
            poc: [poc0, poc1],
            long_term: [lt0, lt1],
        }
    }
}

/// The motion of every 4x4 luma block of the picture being decoded.
#[derive(Clone, Debug)]
pub struct MotionField {
    width: usize,
    height: usize,
    /// Cells per row (4x4 units).
    width4: usize,
    cells: Vec<Option<FieldCell>>,
    /// The reference lists of each slice of the picture, indexed by `FieldCell::slice`.
    pub slices: Vec<SliceRefs>,
    checksum: u64,
    /// `TileId` of each CTB, raster order; empty when the picture has one tile.
    tiles: Vec<u16>,
    /// `CtbLog2SizeY` and the CTBs per row, for the tile lookup.
    ctb_log2: u32,
    ctb_cols: usize,
    /// The tile of the prediction unit being derived (set by its [`SliceMotion`]).
    cur_tile: u16,
    /// The 4x4 unit row of `cells[0..width4]`: zero for a whole picture, and the row above
    /// the band for a wavefront row band (see [`MotionField::band`]).
    origin4: usize,
    /// A wavefront band: its stores are kept in `words` for the picture's checksum, which
    /// folds them in decoding order once the band is merged.
    band: bool,
    words: Vec<u64>,
}

/// The collocated view of a finished test field, as a picture of its own.
#[cfg(test)]
pub fn test_shared(field: &MotionField, poc: i32) -> Arc<SharedPic> {
    let shared = SharedPic::new(poc, field.width(), field.height(), (8, 8), false, 1);
    shared.publish_col(field, field.height());
    Arc::new(shared)
}

impl MotionField {
    /// An empty field for a picture of `width` by `height` luma samples.
    pub fn new(width: usize, height: usize) -> MotionField {
        let width4 = width.div_ceil(4);
        let height4 = height.div_ceil(4);
        MotionField {
            width,
            height,
            width4,
            cells: vec![None; width4 * height4],
            slices: Vec::new(),
            checksum: FNV_OFFSET,
            tiles: Vec::new(),
            ctb_log2: 0,
            ctb_cols: 0,
            cur_tile: 0,
            origin4: 0,
            band: false,
            words: Vec::new(),
        }
    }

    /// An empty band of this field for a wavefront row: `unit_rows` 4x4 unit rows starting
    /// at unit row `origin4`, sharing this field's tile map. The band's first row is the row
    /// above the CTB row, which the caller fills (see [`MotionField::cell_row_mut`]).
    pub fn band(&self, origin4: usize, unit_rows: usize) -> MotionField {
        MotionField {
            width: self.width,
            height: self.height,
            width4: self.width4,
            cells: vec![None; unit_rows * self.width4],
            slices: Vec::new(),
            checksum: FNV_OFFSET,
            tiles: self.tiles.clone(),
            ctb_log2: self.ctb_log2,
            ctb_cols: self.ctb_cols,
            cur_tile: 0,
            origin4,
            band: true,
            words: Vec::new(),
        }
    }

    /// The cells of the 4x4 unit row `unit_row` (a row this field stores).
    pub fn cell_row(&self, unit_row: usize) -> &[Option<FieldCell>] {
        let i = (unit_row - self.origin4) * self.width4;
        &self.cells[i..i + self.width4]
    }

    /// The cells split into bands of `unit_rows` 4x4 unit rows each (the last band may be
    /// shorter), for a wavefront to fill one band per CTB row. Only for a whole-picture field.
    pub(crate) fn bands_mut(
        &mut self,
        unit_rows: usize,
    ) -> std::slice::ChunksMut<'_, Option<FieldCell>> {
        self.cells.chunks_mut(unit_rows * self.width4)
    }

    /// Mutable form of [`MotionField::cell_row`].
    pub fn cell_row_mut(&mut self, unit_row: usize) -> &mut [Option<FieldCell>] {
        let i = (unit_row - self.origin4) * self.width4;
        &mut self.cells[i..i + self.width4]
    }

    /// Merge a finished band into this field: its cells from unit row `first_unit_row` on
    /// are copied, and its stores are folded into the checksum in decoding order. Bands
    /// merged in decoding order give the checksum a whole-picture decode would give.
    pub fn merge_band(&mut self, band: &MotionField, first_unit_row: usize) {
        let from = (first_unit_row - band.origin4) * self.width4;
        let to = first_unit_row - self.origin4;
        let n = band.cells.len() - from;
        self.cells[to * self.width4..to * self.width4 + n].copy_from_slice(&band.cells[from..]);
        for words in band.words.as_chunks::<8>().0 {
            for &word in words {
                self.checksum = (self.checksum ^ word).wrapping_mul(FNV_PRIME);
            }
        }
    }

    /// The tile of each CTB (raster order), so that a neighbour in another tile is
    /// unavailable (6.4.1).
    pub fn set_tiles(&mut self, tile_of_rs: Vec<u16>, ctb_log2: u32, ctb_cols: usize) {
        self.tiles = tile_of_rs;
        self.ctb_log2 = ctb_log2;
        self.ctb_cols = ctb_cols;
    }

    /// The tile of the luma position `(x, y)`.
    fn tile_at(&self, x: usize, y: usize) -> u16 {
        if self.tiles.is_empty() {
            return 0;
        }
        let rs = (y >> self.ctb_log2) * self.ctb_cols + (x >> self.ctb_log2);
        self.tiles.get(rs).copied().unwrap_or(0)
    }

    /// Picture width in luma samples.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Picture height in luma samples.
    pub fn height(&self) -> usize {
        self.height
    }

    /// The entry covering luma sample `(x, y)`; `None` outside the picture, for intra
    /// blocks, and for blocks not decoded yet.
    pub fn at(&self, x: isize, y: isize) -> Option<FieldCell> {
        let x = usize::try_from(x).ok()?;
        let y = usize::try_from(y).ok()?;
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = (y / 4).checked_sub(self.origin4)? * self.width4 + x / 4;
        self.cells.get(i).copied().flatten()
    }

    /// Forget the motion of the `w` by `h` block at `(x, y)`, so it reads as it did
    /// before anything was stored there (an intra block has no entry).
    pub fn clear(&mut self, x: usize, y: usize, w: usize, h: usize) {
        let x_end = (x + w).min(self.width);
        let y_end = (y + h).min(self.height);
        for yy in (y..y_end).step_by(4) {
            for xx in (x..x_end).step_by(4) {
                self.cells[(yy / 4 - self.origin4) * self.width4 + xx / 4] = None;
            }
        }
    }

    /// The cells of the `w` by `h` luma area at `(x, y)` in raster order of 4x4
    /// units, for a wavefront row to hand a finished CTB to the row below.
    /// `x`, `y`, `w` and `h` are multiples of 4 inside the picture.
    pub(crate) fn export_cells(
        &self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
    ) -> Vec<Option<FieldCell>> {
        let mut out = Vec::with_capacity((w / 4) * (h / 4));
        for yy in (y..y + h).step_by(4) {
            let row = (yy / 4) * self.width4;
            for xx in (x..x + w).step_by(4) {
                out.push(self.cells[row + xx / 4]);
            }
        }
        out
    }

    /// The inverse of [`MotionField::export_cells`].
    pub(crate) fn import_cells(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        cells: &[Option<FieldCell>],
    ) {
        let mut src = cells.iter();
        for yy in (y..y + h).step_by(4) {
            let row = (yy / 4) * self.width4;
            for xx in (x..x + w).step_by(4) {
                if let Some(cell) = src.next() {
                    self.cells[row + xx / 4] = *cell;
                }
            }
        }
    }

    /// Store the motion of the `w` by `h` block at `(x, y)`, decoded in `slice`.
    pub fn store(&mut self, x: usize, y: usize, w: usize, h: usize, slice: u16, motion: PbMotion) {
        let x_end = (x + w).min(self.width);
        let y_end = (y + h).min(self.height);
        for yy in (y..y_end).step_by(4) {
            for xx in (x..x_end).step_by(4) {
                self.cells[(yy / 4 - self.origin4) * self.width4 + xx / 4] =
                    Some(FieldCell { motion, slice });
            }
        }
        let mut words = [0u64; 8];
        words[0] = x as u64;
        words[1] = y as u64;
        words[2] = w as u64;
        words[3] = h as u64;
        words[4] = u64::from(slice);
        words[5] = u64::from(motion.pred[0]) | (u64::from(motion.pred[1]) << 1);
        words[6] = (motion.ref_idx[0] as u8 as u64) | ((motion.ref_idx[1] as u8 as u64) << 8);
        words[7] = (motion.mv[0][0] as u16 as u64)
            | ((motion.mv[0][1] as u16 as u64) << 16)
            | ((motion.mv[1][0] as u16 as u64) << 32)
            | ((motion.mv[1][1] as u16 as u64) << 48);
        if self.band {
            self.words.extend_from_slice(&words);
            return;
        }
        for word in words {
            self.checksum = (self.checksum ^ word).wrapping_mul(FNV_PRIME);
        }
    }

    /// A hash of every stored block, in decoding order. Equal for equal derivations.
    pub fn checksum(&self) -> u64 {
        self.checksum
    }

    /// The motion of every 4x4 unit in the form the deblocking filter reads (8.7.2.4).
    /// A list's reference picture is named by its POC, which identifies a picture
    /// among those one slice refers to; intra and undecoded units stay at the default.
    pub fn deblock_units(&self) -> Vec<PuMotion> {
        (0..self.cells.len())
            .map(|i| self.deblock_unit(i))
            .collect()
    }

    /// The deblocking motion of the 4x4 unit `i` (see [`MotionField::deblock_units`]).
    fn deblock_unit(&self, i: usize) -> PuMotion {
        let Some(cell) = self.cells[i] else {
            return PuMotion::default();
        };
        let refs = self.slices.get(usize::from(cell.slice));
        let mut ref_pic = [-1i32; 2];
        for (list, slot) in ref_pic.iter_mut().enumerate() {
            if cell.motion.pred[list] {
                let idx = usize::try_from(cell.motion.ref_idx[list]).unwrap_or(0);
                *slot = refs
                    .and_then(|r| r.poc[list].get(idx))
                    .copied()
                    .unwrap_or(-1);
            }
        }
        PuMotion {
            pred: cell.motion.pred,
            ref_pic,
            mv: cell.motion.mv.map(|v| [i32::from(v[0]), i32::from(v[1])]),
        }
    }

    /// The motion of the luma rows `y0..y1` in the form the deblocking filter reads
    /// (see [`MotionField::deblock_units`]), written to `out` (one entry per 4x4
    /// unit of the picture, row-major). `y0` and `y1` are multiples of 4.
    pub fn deblock_rows(&self, out: &mut [PuMotion], y0: usize, y1: usize) {
        let (u0, u1) = ((y0 / 4) * self.width4, (y1 / 4) * self.width4);
        for (i, slot) in out.iter_mut().enumerate().take(u1).skip(u0) {
            *slot = self.deblock_unit(i);
        }
    }
}

/// The collocated picture's motion, kept with the picture in the DPB.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColField {
    /// `PicOrderCntVal` of the picture.
    pub poc: i32,
    width: usize,
    height: usize,
    width16: usize,
    cells: Vec<Option<FieldCell>>,
    slices: Vec<SliceRefs>,
}

impl ColField {
    /// The collocated view of a picture of `width` by `height` luma samples, with no
    /// motion yet: [`ColField::fill_rows`] adds it as the rows are decoded.
    pub fn new(poc: i32, width: usize, height: usize) -> ColField {
        let width16 = width.div_ceil(16);
        ColField {
            poc,
            width,
            height,
            width16,
            cells: vec![None; width16 * height.div_ceil(16)],
            slices: Vec::new(),
        }
    }

    /// Take the top-left 4x4 of each 16x16 block whose top row lies in the luma rows
    /// `y0..y1` from `field` (8.5.3.2.8), and the slices' reference lists. `y0` is a
    /// multiple of 16, and the rows must be final in `field`.
    pub fn fill_rows(&mut self, field: &MotionField, y0: usize, y1: usize) {
        self.slices.clone_from(&field.slices);
        let (b0, b1) = (y0 / 16, y1.div_ceil(16).min(self.height.div_ceil(16)));
        for by in b0..b1 {
            for bx in 0..self.width16 {
                self.cells[by * self.width16 + bx] =
                    field.at((bx * 16) as isize, (by * 16) as isize);
            }
        }
    }

    /// The entry of the 16x16 block containing `(x, y)`, with its slice's reference
    /// lists. `None` outside the picture, and for intra blocks.
    fn at(&self, x: usize, y: usize) -> Option<(FieldCell, &SliceRefs)> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let cell = self.cells[(y >> 4) * self.width16 + (x >> 4)]?;
        let refs = self.slices.get(usize::from(cell.slice))?;
        Some((cell, refs))
    }
}

/// The collocated picture of a slice (8.5.3.2.8): `RefPicList1[collocated_ref_idx]`
/// for a B slice with `collocated_from_l0_flag` 0, else `RefPicList0[...]`. `None`
/// when temporal motion prediction is off, for I slices, and when the picture kept
/// no motion.
pub fn collocated(dpb: &Dpb, header: &SliceHeader, lists: &RefPicLists) -> Option<Arc<SharedPic>> {
    if !header.temporal_mvp_enabled || header.slice_type == SliceType::I {
        return None;
    }
    let list = if header.slice_type == SliceType::B && !header.collocated_from_l0 {
        &lists.l1
    } else {
        &lists.l0
    };
    let entry = list.get(usize::try_from(header.collocated_ref_idx).ok()?)?;
    Some(Arc::clone(&dpb.get(entry.id)?.picture))
}

/// The slice-level parameters the merge and AMVP derivations read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MotionParams {
    /// The slice is a B slice.
    pub is_b: bool,
    /// `MaxNumMergeCand`.
    pub max_merge_cand: usize,
    /// `Log2ParMrgLevel`.
    pub log2_par_mrg_level: u32,
    /// `CtbLog2SizeY`.
    pub ctb_log2: u32,
    /// `collocated_from_l0_flag`.
    pub collocated_from_l0: bool,
    /// `slice_temporal_mvp_enabled_flag`.
    pub temporal_mvp: bool,
    /// `NoBackwardPredFlag`: no reference picture follows the current one in output order.
    pub no_backward_pred: bool,
}

/// The [`MotionSource`] of the decoder: derives each prediction unit's motion with
/// the merge or AMVP process and stores it in the picture's motion field.
pub struct SliceMotion<'a> {
    /// Slice parameters.
    pub params: MotionParams,
    /// The slice's reference picture lists.
    pub lists: &'a RefPicLists,
    /// `PicOrderCntVal` of the current picture.
    pub cur_poc: i32,
    /// The motion field of the current picture.
    pub field: &'a mut MotionField,
    /// The collocated picture, if temporal prediction applies.
    pub col: Option<Arc<SharedPic>>,
    /// The picture this slice belongs to, when its rows are published as they are
    /// decoded (see [`MotionSource::row_done`]).
    pub publish: Option<Arc<SharedPic>>,
    /// Index of this slice in the picture's slice list.
    pub slice: u16,
}

impl<'a> SliceMotion<'a> {
    /// The source for one slice of the current picture.
    #[allow(clippy::too_many_arguments)] // the slice's parameters are all needed here
    pub fn new(
        sps: &Sps,
        pps: &Pps,
        header: &SliceHeader,
        lists: &'a RefPicLists,
        cur_poc: i32,
        field: &'a mut MotionField,
        col: Option<Arc<SharedPic>>,
        slice: u16,
    ) -> SliceMotion<'a> {
        let ctb_log2 = sps.log2_min_cb_size_minus3 + 3 + sps.log2_diff_max_min_cb_size;
        let no_backward_pred = lists
            .l0
            .iter()
            .chain(lists.l1.iter())
            .all(|e| e.poc <= cur_poc);
        SliceMotion {
            params: MotionParams {
                is_b: header.slice_type == SliceType::B,
                max_merge_cand: 5 - header.five_minus_max_num_merge_cand.min(4) as usize,
                log2_par_mrg_level: pps.log2_parallel_merge_level_minus2 + 2,
                ctb_log2,
                collocated_from_l0: header.collocated_from_l0,
                temporal_mvp: header.temporal_mvp_enabled,
                no_backward_pred,
            },
            lists,
            cur_poc,
            field,
            col,
            publish: None,
            slice,
        }
    }

    /// The stored motion of the neighbouring block covering `(x, y)` when it is
    /// available: decoded, inter coded and in this slice (6.4.1, 6.4.2).
    pub fn neighbour(&self, x: isize, y: isize) -> Option<PbMotion> {
        let cell = self.field.at(x, y)?;
        let in_tile = self.field.tile_at(x as usize, y as usize) == self.field.cur_tile;
        (cell.slice == self.slice && in_tile).then_some(cell.motion)
    }

    /// `RefPicListX[idx]` of the current slice.
    pub fn ref_entry(&self, list: usize, idx: i8) -> Option<RefEntry> {
        let entries = if list == 0 {
            &self.lists.l0
        } else {
            &self.lists.l1
        };
        usize::try_from(idx)
            .ok()
            .and_then(|i| entries.get(i))
            .copied()
    }

    /// `PicOrderCntVal` of `RefPicListX[idx]`.
    pub fn ref_poc(&self, list: usize, idx: i8) -> Option<i32> {
        self.ref_entry(list, idx).map(|e| e.poc)
    }

    /// Whether `RefPicListX[idx]` is a long-term reference.
    pub fn ref_long_term(&self, list: usize, idx: i8) -> Option<bool> {
        self.ref_entry(list, idx).map(|e| e.long_term)
    }

    /// The temporal luma vector predictor for the block `(x, y, w, h)` in list `lx`
    /// with reference index `ref_idx` (8.5.3.2.8): the bottom-right collocated block
    /// when it is inside the same CTB row and the picture, else the centre block.
    pub fn temporal_mv(
        &self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        lx: usize,
        ref_idx: i8,
    ) -> Option<[i32; 2]> {
        let shared = self.col.as_deref()?;
        let (xbr, ybr) = (x + w, y + h);
        let ctb = self.params.ctb_log2;
        // Both candidates lie in the CTB row of `y` (the bottom-right one only when it
        // stays in that row), so the collocated rows up to the end of that row suffice.
        let need = (((y >> ctb) + 1) << ctb).min(self.field.height());
        let t = super::trace::now();
        let waited = shared.wait_col_rows(need);
        super::trace::since(super::trace::Phase::WaitCol, t);
        waited.ok()?;
        let col = shared.read_col();
        if (y >> ctb) == (ybr >> ctb) && ybr < self.field.height() && xbr < self.field.width() {
            let found = self.col_mv(&col, (xbr >> 4) << 4, (ybr >> 4) << 4, lx, ref_idx);
            if found.is_some() {
                return found;
            }
        }
        let (xc, yc) = (x + (w >> 1), y + (h >> 1));
        self.col_mv(&col, (xc >> 4) << 4, (yc >> 4) << 4, lx, ref_idx)
    }

    /// The collocated motion vector of the block at `(x, y)` (8.5.3.2.9), scaled to
    /// the current reference (8.5.3.2.8 with the distance scaling).
    fn col_mv(
        &self,
        col: &ColField,
        x: usize,
        y: usize,
        lx: usize,
        ref_idx: i8,
    ) -> Option<[i32; 2]> {
        let (cell, refs) = col.at(x, y)?;
        let m = cell.motion;
        let list_col = match (m.pred[0], m.pred[1]) {
            (false, false) => return None,
            (false, true) => 1,
            (true, false) => 0,
            (true, true) => {
                if self.params.no_backward_pred {
                    lx
                } else {
                    usize::from(self.params.collocated_from_l0)
                }
            }
        };
        let ref_col = usize::try_from(m.ref_idx[list_col]).ok()?;
        let col_ref_poc = *refs.poc[list_col].get(ref_col)?;
        let col_ref_lt = *refs.long_term[list_col].get(ref_col)?;
        let cur = self.ref_entry(lx, ref_idx)?;
        if cur.long_term != col_ref_lt {
            return None;
        }
        let mv = to32(m.mv[list_col]);
        let col_diff = col.poc - col_ref_poc;
        let cur_diff = self.cur_poc - cur.poc;
        if cur.long_term || col_diff == cur_diff {
            Some(mv)
        } else {
            Some(scale_mv(mv, col_diff, cur_diff))
        }
    }
}

impl MotionSource for SliceMotion<'_> {
    fn row_done(&mut self, pic: &mut PicState, row_end: usize) {
        // The CTB row that ended is `row_end - ctb`; its motion is final.
        let ctb = 1usize << self.params.ctb_log2;
        let y0 = row_end.saturating_sub(ctb);
        if pic.has_pixels() {
            let (u0, u1) = (y0, row_end.min(self.field.height()));
            self.field.deblock_rows(&mut pic.motion, u0, u1);
        }
        if let Some(shared) = &self.publish {
            shared.publish_col(self.field, row_end);
        }
    }

    fn band_cells(&mut self) -> Option<&mut MotionField> {
        Some(&mut *self.field)
    }

    fn derive_motion(&mut self, cu: &CuGeometry, pu_index: usize, pu: &PredictionUnit) -> Motion {
        self.field.cur_tile = self.field.tile_at(cu.x, cu.y);
        let derived = if pu.merge_flag {
            merge::derive_merge(self, cu, pu_index, pu)
        } else {
            amvp::derive_amvp(self, pu)
        };
        self.field
            .store(pu.x, pu.y, pu.width, pu.height, self.slice, derived);
        derived.to_motion()
    }
}

/// `mvLX` scaled by the POC distances (8-180 to 8-183): `td` is the distance of the
/// reference the vector points to, `tb` the distance of the target reference, both
/// as `DiffPicOrderCnt` values before clipping. A zero `td` leaves the vector as it is.
pub fn scale_mv(mv: [i32; 2], td: i32, tb: i32) -> [i32; 2] {
    let td = td.clamp(-128, 127);
    let tb = tb.clamp(-128, 127);
    if td == 0 {
        return mv;
    }
    let tx = (16384 + (td.abs() >> 1)) / td;
    let dist_scale = ((tb * tx + 32) >> 6).clamp(-4096, 4095);
    mv.map(|c| {
        let p = dist_scale * c;
        let sign = if p < 0 { -1 } else { 1 };
        (sign * ((p.abs() + 127) >> 8)).clamp(-32768, 32767)
    })
}

/// The chroma vector of 4:2:0 content, in 1/8 chroma sample units (8.5.3.2.10):
/// the luma quarter-sample vector, unchanged in value.
pub fn chroma_mv_420(mv: [i16; 2]) -> [i32; 2] {
    to32(mv)
}

/// A vector widened to `i32`.
pub fn to32(mv: [i16; 2]) -> [i32; 2] {
    [i32::from(mv[0]), i32::from(mv[1])]
}

/// A vector clipped to the 16-bit range of a stored vector.
pub fn to16(mv: [i32; 2]) -> [i16; 2] {
    [
        mv[0].clamp(-32768, 32767) as i16,
        mv[1].clamp(-32768, 32767) as i16,
    ]
}

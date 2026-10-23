//! The per-picture decoding state: the sample planes and the maps the
//! parser, the intra reconstruction and the in-loop filters share.
//!
//! One [`PicState`] lives from the first slice segment of a picture until the
//! picture is finished. The coding-tree parser ([`super::cu_syntax`]) writes
//! the block maps as it parses; the reconstruction ([`super::recon`]) writes
//! the samples in decoding order; the deblocking filter and SAO
//! ([`super::deblock`], [`super::sao`]) read the maps and rewrite the samples
//! once the whole picture is reconstructed.
//!
//! All maps are per 4x4 luma unit (`w4` units per row) except the per-CTB
//! tables. Positions are luma samples.

use super::mc::PlaneMut;
use super::tiles::TileScan;
use crate::cabac::Contexts;
use crate::decode::DecodedPicture;
use ec_h265_syntax::{Pps, Sps};

/// [`BlockInfo::edge_v`] / [`BlockInfo::edge_h`] bit: the left (top) edge of
/// this unit is a transform block edge.
pub const EDGE_TU: u8 = 1;
/// [`BlockInfo::edge_v`] / [`BlockInfo::edge_h`] bit: the left (top) edge of
/// this unit is a prediction block edge.
pub const EDGE_PU: u8 = 2;

/// What is known about one 4x4 luma unit once its coding unit is parsed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockInfo {
    /// `CuPredMode` is MODE_INTRA (false for skipped and inter units, and for
    /// units not yet decoded).
    pub intra: bool,
    /// `cu_skip_flag` of the coding unit.
    pub skip: bool,
    /// The unit belongs to a PCM coding unit (`pcm_flag` 1).
    pub pcm: bool,
    /// `cu_transquant_bypass_flag` of the coding unit.
    pub bypass: bool,
    /// `CtDepth` of the coding unit.
    pub depth: u8,
    /// `IntraPredModeY`; 1 (DC) for inter, PCM and not yet decoded units.
    pub intra_mode: u8,
    /// `QpY` of the coding unit (may be negative at high bit depths).
    pub qp: i8,
    /// The luma transform block covering this unit has `cbf_luma` 1.
    pub nz_luma: bool,
    /// Bit set of [`EDGE_TU`] and [`EDGE_PU`] for the vertical edge at the left
    /// of this unit. Only edges on the 8x8 grid are filtered.
    pub edge_v: u8,
    /// Same for the horizontal edge at the top of this unit.
    pub edge_h: u8,
}

/// Motion of one 4x4 unit, for the deblocking boundary strength (8.7.2.4).
/// Intra and unset units have no prediction flags. Filled at the end of the picture
/// from its [`MotionField`](super::motion_field::MotionField), so one derivation feeds both.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PuMotion {
    /// `predFlagLX` for X = 0, 1.
    pub pred: [bool; 2],
    /// Identity of the reference picture per list (its POC); -1 when the list is
    /// unused. Two entries are the same picture iff their values are equal.
    pub ref_pic: [i32; 2],
    /// Motion vector per list, `[x, y]` in quarter luma samples.
    pub mv: [[i32; 2]; 2],
}

/// `SaoTypeIdx`, `sao_band_position`, `SaoEoClass` and `SaoOffsetVal` of one
/// CTB, for the three colour components (7.4.9.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SaoParams {
    /// 0 not applied, 1 band offset, 2 edge offset, per component.
    pub type_idx: [u8; 3],
    /// `sao_band_position`, per component.
    pub band_position: [u8; 3],
    /// `SaoEoClass`, per component (Cb and Cr share the class of Cb).
    pub eo_class: [u8; 3],
    /// `SaoOffsetVal[1..=4]` per component, sign applied, already scaled by
    /// `1 << (bitDepth - Min(bitDepth, 10))`.
    pub offset: [[i16; 4]; 3],
}

/// Per-CTB state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CtbInfo {
    /// Index into [`PicState::slices`] of the slice segment that contains the CTB.
    pub slice: u16,
    /// `SliceAddrRs` of the slice (the address of its first CTB, which a dependent
    /// slice segment shares with the independent segment before it).
    pub slice_addr: u32,
    /// `TileId` of the CTB.
    pub tile: u16,
    /// The CTB's SAO parameters (all zero when SAO is off or not coded).
    pub sao: SaoParams,
}

/// The slice-header values the in-loop filters read, one entry per slice
/// segment of the picture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SliceFilter {
    /// `slice_deblocking_filter_disabled_flag`.
    pub deblock_disabled: bool,
    /// `slice_beta_offset_div2`.
    pub beta_offset_div2: i32,
    /// `slice_tc_offset_div2`.
    pub tc_offset_div2: i32,
    /// `slice_loop_filter_across_slices_enabled_flag`.
    pub loop_filter_across_slices: bool,
    /// `slice_sao_luma_flag`.
    pub sao_luma: bool,
    /// `slice_sao_chroma_flag`.
    pub sao_chroma: bool,
}

/// State that runs across the slice segments of one picture: the contexts
/// that a later segment restores (9.3.1), and the QP predictor (8.6.1).
#[derive(Default)]
pub struct SliceCarry {
    /// `SliceAddrRs` of the slice the current segment belongs to.
    pub slice_addr: usize,
    /// `TableStateIdxWpp`: the contexts after the second CTB of the last row
    /// that stored them.
    pub wpp: Option<Contexts>,
    /// `TableStateIdxDs`: the contexts at the end of the last slice segment.
    pub ds: Option<Contexts>,
    /// `qPY_PREV` at the end of the last slice segment.
    pub last_qp: i32,
}

/// The decoding state of one picture.
pub struct PicState {
    /// Luma width in samples.
    pub width: usize,
    /// `chroma_format_idc`: 1 (4:2:0), 2 (4:2:2) or 3 (4:4:4).
    pub chroma_format_idc: u32,
    /// Luma height in samples.
    pub height: usize,
    /// `log2_min_luma_transform_block_size`.
    pub min_tb_log2: u32,
    /// `CtbLog2SizeY`.
    pub ctb_log2: u32,
    /// CTBs per row.
    pub ctb_cols: usize,
    /// CTB rows.
    pub ctb_rows: usize,
    /// Units (4x4 luma) per row.
    pub w4: usize,
    /// Units per column.
    pub h4: usize,
    /// `BitDepthY`.
    pub bit_depth_y: u32,
    /// `BitDepthC`.
    pub bit_depth_c: u32,
    /// `constrained_intra_pred_flag`.
    pub constrained_intra_pred: bool,
    /// `strong_intra_smoothing_enabled_flag`.
    pub strong_intra_smoothing: bool,
    /// `intra_smoothing_disabled_flag`.
    pub intra_smoothing_disabled: bool,
    /// `pcm_loop_filter_disabled_flag` (false when PCM is off).
    pub pcm_loop_filter_disabled: bool,
    /// `pps_cb_qp_offset` and `pps_cr_qp_offset` (the chroma QP of the deblocking filter).
    pub pps_cb_qp_offset: i32,
    /// See `pps_cb_qp_offset`.
    pub pps_cr_qp_offset: i32,
    /// The samples. Empty planes when the decoder runs without pixel decode.
    pub planes: DecodedPicture,
    /// Per 4x4 unit, `w4 * h4`.
    pub info: Vec<BlockInfo>,
    /// Per 4x4 unit motion, `w4 * h4`; set from the picture's motion field before deblocking.
    pub motion: Vec<PuMotion>,
    /// Per CTB, raster order.
    pub ctb: Vec<CtbInfo>,
    /// Slice filter parameters; `CtbInfo::slice` indexes this.
    pub slices: Vec<SliceFilter>,
    /// The tile scan: CTB addressing and the tile of each CTB.
    pub tiles: TileScan,
    /// `loop_filter_across_tiles_enabled_flag`.
    pub loop_filter_across_tiles: bool,
    /// The state the slice segments share.
    pub carry: SliceCarry,
    /// Luma row of the first stored sample row. Zero for a whole picture; a wavefront
    /// row band stores the row above it first (see [`super::wpp`]).
    pub origin_y: usize,
    /// Chroma row of the first stored chroma row.
    pub origin_c: usize,
    /// Luma row of the first stored 4x4 unit row (a multiple of 4).
    pub origin_unit: usize,
}

impl PicState {
    /// The state of a `width` x `height` picture. With `pixels` false the
    /// sample planes are left empty and only the maps are kept.
    #[allow(clippy::too_many_arguments)] // one value per picture-level SPS/PPS fact
    pub fn new(
        width: usize,
        height: usize,
        ctb_log2: u32,
        min_tb_log2: u32,
        bit_depth_y: u32,
        bit_depth_c: u32,
        pixels: bool,
    ) -> PicState {
        let ctb = 1usize << ctb_log2;
        let ctb_cols = width.div_ceil(ctb);
        let ctb_rows = height.div_ceil(ctb);
        let w4 = width.div_ceil(4);
        let h4 = height.div_ceil(4);
        let planes = if pixels {
            DecodedPicture::grey_planes(width, height, bit_depth_y, bit_depth_c)
        } else {
            DecodedPicture {
                width,
                height,
                bit_depth: bit_depth_y,
                bit_depth_c,
                chroma_format_idc: 1,
                y: Vec::new(),
                cb: Vec::new(),
                cr: Vec::new(),
            }
        };
        PicState {
            width,
            height,
            chroma_format_idc: 1,
            min_tb_log2,
            ctb_log2,
            ctb_cols,
            ctb_rows,
            w4,
            h4,
            bit_depth_y,
            bit_depth_c,
            constrained_intra_pred: false,
            strong_intra_smoothing: false,
            intra_smoothing_disabled: false,
            pcm_loop_filter_disabled: false,
            pps_cb_qp_offset: 0,
            pps_cr_qp_offset: 0,
            planes,
            info: vec![BlockInfo::default(); w4 * h4],
            motion: vec![PuMotion::default(); w4 * h4],
            ctb: vec![CtbInfo::default(); ctb_cols * ctb_rows],
            slices: Vec::new(),
            tiles: TileScan::single(ctb_cols, ctb_rows),
            loop_filter_across_tiles: true,
            carry: SliceCarry::default(),
            origin_y: 0,
            origin_c: 0,
            origin_unit: 0,
        }
    }

    /// The log2 horizontal chroma subsampling (1 for 4:2:0 and 4:2:2).
    pub fn sub_x(&self) -> u32 {
        crate::decode::chroma_shifts(self.chroma_format_idc).0
    }

    /// The log2 vertical chroma subsampling (1 for 4:2:0 only).
    pub fn sub_y(&self) -> u32 {
        crate::decode::chroma_shifts(self.chroma_format_idc).1
    }

    /// Whether the sample planes are allocated (pixel decode is on).
    pub fn has_pixels(&self) -> bool {
        !self.planes.y.is_empty()
    }

    /// The three writable sample planes, for motion compensation.
    pub fn planes_mut(&mut self) -> [PlaneMut<'_>; 3] {
        let (bd_y, bd_c) = (self.bit_depth_y, self.bit_depth_c);
        let w = self.planes.width;
        let cw = self.width.div_ceil(1 << self.sub_x());
        let (oy, oc) = (self.origin_y, self.origin_c);
        let p = &mut self.planes;
        [
            PlaneMut {
                samples: &mut p.y,
                stride: w,
                bit_depth: bd_y,
                origin: oy,
            },
            PlaneMut {
                samples: &mut p.cb,
                stride: cw,
                bit_depth: bd_c,
                origin: oc,
            },
            PlaneMut {
                samples: &mut p.cr,
                stride: cw,
                bit_depth: bd_c,
                origin: oc,
            },
        ]
    }

    /// The first stored row of component `c`, in that component's rows.
    pub fn plane_origin(&self, c: usize) -> usize {
        if c == 0 { self.origin_y } else { self.origin_c }
    }

    /// The index of the 4x4 unit containing luma position `(x, y)`.
    pub fn unit(&self, x: usize, y: usize) -> usize {
        ((y >> 2) - self.origin_unit / 4) * self.w4 + (x >> 2)
    }

    /// `MinTbAddrZs` at 4x4 granularity (6.5.2): the tile scan address of the
    /// CTB, then the z-order of the unit inside the CTB. Comparing two positions
    /// with this tells which one is decoded first.
    pub fn zscan(&self, x: usize, y: usize) -> usize {
        let rs = (y >> self.ctb_log2) * self.ctb_cols + (x >> self.ctb_log2);
        let ts = self.tiles.rs_to_ts.get(rs).copied().unwrap_or(rs);
        let units_per_ctb_side = 1usize << (self.ctb_log2 - 2);
        let mask = units_per_ctb_side - 1;
        let (ux, uy) = ((x >> 2) & mask, (y >> 2) & mask);
        let mut z = 0usize;
        for bit in 0..self.ctb_log2 as usize - 2 {
            z |= ((ux >> bit) & 1) << (2 * bit);
            z |= ((uy >> bit) & 1) << (2 * bit + 1);
        }
        (ts << (2 * (self.ctb_log2 as usize - 2))) | z
    }

    /// The availability of the neighbouring luma position `(xn, yn)` for the
    /// block at `(xc, yc)` (6.4.1): inside the picture, decoded before the current
    /// block, and in the same slice and the same tile.
    pub fn available(&self, xc: usize, yc: usize, xn: isize, yn: isize) -> bool {
        if xn < 0 || yn < 0 || xn as usize >= self.width || yn as usize >= self.height {
            return false;
        }
        let (xn, yn) = (xn as usize, yn as usize);
        if self.zscan(xn, yn) > self.zscan(xc, yc) {
            return false;
        }
        let rs_n = (yn >> self.ctb_log2) * self.ctb_cols + (xn >> self.ctb_log2);
        let rs_c = (yc >> self.ctb_log2) * self.ctb_cols + (xc >> self.ctb_log2);
        if rs_n == rs_c {
            return true;
        }
        match (self.ctb.get(rs_n), self.ctb.get(rs_c)) {
            (Some(n), Some(c)) => n.slice_addr == c.slice_addr && n.tile == c.tile,
            _ => false,
        }
    }

    /// The raster address of the CTB containing luma position `(x, y)`.
    pub fn ctb_rs(&self, x: usize, y: usize) -> usize {
        (y >> self.ctb_log2) * self.ctb_cols + (x >> self.ctb_log2)
    }

    /// Set `f` on every unit under the `w` x `h` luma area at `(x, y)`.
    pub fn update_info(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        f: impl Fn(&mut BlockInfo),
    ) {
        for by in (y..(y + h).min(self.height)).step_by(4) {
            for bx in (x..(x + w).min(self.width)).step_by(4) {
                let idx = self.unit(bx, by);
                f(&mut self.info[idx]);
            }
        }
    }

    /// Mark the left and top edges of the `w` x `h` block at `(x, y)` with
    /// `kind` (an [`EDGE_TU`] or [`EDGE_PU`] bit).
    pub fn mark_edges(&mut self, x: usize, y: usize, w: usize, h: usize, kind: u8) {
        for by in (y..(y + h).min(self.height)).step_by(4) {
            let idx = self.unit(x, by);
            if x < self.width {
                self.info[idx].edge_v |= kind;
            }
        }
        for bx in (x..(x + w).min(self.width)).step_by(4) {
            let idx = self.unit(bx, y);
            if y < self.height {
                self.info[idx].edge_h |= kind;
            }
        }
    }

    /// The state of a picture of `sps` and `pps`. With `pixels` on, the sample
    /// planes are allocated, grey.
    pub fn for_sps_pps(sps: &Sps, pps: &Pps, pixels: bool) -> PicState {
        let min_cb_log2 = sps.log2_min_cb_size_minus3 + 3;
        let ctb_log2 = min_cb_log2 + sps.log2_diff_max_min_cb_size;
        let min_tb_log2 = sps.log2_min_tb_size_minus2 + 2;
        let mut pic = PicState::new(
            sps.pic_width_in_luma_samples as usize,
            sps.pic_height_in_luma_samples as usize,
            ctb_log2,
            min_tb_log2,
            8 + sps.bit_depth_luma_minus8,
            8 + sps.bit_depth_chroma_minus8,
            pixels,
        );
        if sps.chroma_format_idc != 1 {
            pic.chroma_format_idc = sps.chroma_format_idc;
            if pixels {
                pic.planes = DecodedPicture::grey_planes_fmt(
                    pic.width,
                    pic.height,
                    pic.bit_depth_y,
                    pic.bit_depth_c,
                    sps.chroma_format_idc,
                );
            }
        }
        pic.constrained_intra_pred = pps.constrained_intra_pred;
        pic.strong_intra_smoothing = sps.strong_intra_smoothing;
        pic.intra_smoothing_disabled = sps.range.intra_smoothing_disabled;
        pic.pcm_loop_filter_disabled = sps.pcm_enabled && sps.pcm.is_some_and(|p| p.4);
        pic.pps_cb_qp_offset = pps.cb_qp_offset;
        pic.pps_cr_qp_offset = pps.cr_qp_offset;
        pic.loop_filter_across_tiles = pps.loop_filter_across_tiles_enabled;
        pic
    }

    /// A wavefront band of this picture for the CTB row that starts at luma row `y0`
    /// and holds `own` luma rows. A row below the first one stores the row above it
    /// first, so the band's first sample, unit and CTB rows are that row (copied in by
    /// the wavefront driver). The maps cover only the band; the CTB table is the
    /// whole picture's shape, and the tile scan and slice state are copied.
    pub fn band(&self, y0: usize, own: usize, pixels: bool) -> PicState {
        let bound = usize::from(y0 > 0);
        let own_units = own.div_ceil(4);
        let (sx, sy) = crate::decode::chroma_shifts(self.chroma_format_idc);
        let cw = self.width.div_ceil(1 << sx);
        let own_c = own.div_ceil(1 << sy);
        let planes = DecodedPicture {
            width: self.width,
            height: self.height,
            bit_depth: self.bit_depth_y,
            bit_depth_c: self.bit_depth_c,
            chroma_format_idc: self.chroma_format_idc,
            y: if pixels {
                vec![0; (bound + own) * self.width]
            } else {
                Vec::new()
            },
            cb: if pixels {
                vec![0; (bound + own_c) * cw]
            } else {
                Vec::new()
            },
            cr: if pixels {
                vec![0; (bound + own_c) * cw]
            } else {
                Vec::new()
            },
        };
        PicState {
            width: self.width,
            height: self.height,
            chroma_format_idc: self.chroma_format_idc,
            min_tb_log2: self.min_tb_log2,
            ctb_log2: self.ctb_log2,
            ctb_cols: self.ctb_cols,
            ctb_rows: self.ctb_rows,
            w4: self.w4,
            h4: self.h4,
            bit_depth_y: self.bit_depth_y,
            bit_depth_c: self.bit_depth_c,
            constrained_intra_pred: self.constrained_intra_pred,
            strong_intra_smoothing: self.strong_intra_smoothing,
            intra_smoothing_disabled: self.intra_smoothing_disabled,
            pcm_loop_filter_disabled: self.pcm_loop_filter_disabled,
            pps_cb_qp_offset: self.pps_cb_qp_offset,
            pps_cr_qp_offset: self.pps_cr_qp_offset,
            planes,
            info: vec![BlockInfo::default(); (bound + own_units) * self.w4],
            motion: Vec::new(),
            ctb: vec![CtbInfo::default(); self.ctb.len()],
            slices: Vec::new(),
            tiles: self.tiles.clone(),
            loop_filter_across_tiles: self.loop_filter_across_tiles,
            carry: SliceCarry {
                slice_addr: self.carry.slice_addr,
                ..SliceCarry::default()
            },
            origin_y: y0 - bound,
            origin_c: (y0 >> sy) - bound,
            origin_unit: y0 - 4 * bound,
        }
    }

    /// The bit depth of component `c` (0 luma, 1 and 2 chroma).
    pub fn bit_depth(&self, c: usize) -> u32 {
        if c == 0 {
            self.bit_depth_y
        } else {
            self.bit_depth_c
        }
    }
}

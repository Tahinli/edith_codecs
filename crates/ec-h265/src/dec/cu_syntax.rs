//! CABAC syntax of slice data for P and B slices (and for I slices when the
//! syntax check runs): coding quadtree, coding units, prediction units, the
//! transform tree, SAO parameters and the slice-segment loop (H.265 7.3.8.1 to
//! 7.3.8.12, bin binarisations of 9.3.3 and context selection of 9.3.4.2).
//!
//! The parser reads every bin, keeps the syntax of each coding unit, and turns
//! each transform block's coefficients into residual samples through the same
//! dequantisation and inverse transform the intra path uses. It does not
//! predict. Motion for each prediction unit comes from a [`MotionSource`]; the
//! merge and AMVP lane implements that trait, and [`ZeroMotion`] stands in until
//! then.
//!
//! Slices, slice segments, tiles and wavefront substreams are followed: CTBs
//! are visited in tile scan order, contexts are initialised, synced or restored
//! at each segment, tile and wavefront row start (9.3.1), and the arithmetic
//! decoder restarts at each substream (7.3.8.1).

use super::inter::{self, InterRefs};
use super::mc::{Block, McScratch};
use super::motion_field::MotionField;
use super::pic::{BlockInfo, CtbInfo, EDGE_PU, EDGE_TU, PicState, SaoParams, SliceFilter};
use super::rext;
use super::tiles::TileScan;
use crate::cabac::{CabacDecoder, Contexts, ctx};
use crate::decode::chroma_shifts;
use crate::intra;
use crate::residual::{ResidualCfg, decode_residual_ext};
use crate::transform::{
    chroma_qp_for, dequantize_weighted, inverse_transform_bd, inverse_transform_skip_bd, uses_dst,
};
use ec_core::error::{Error, Result};
use ec_h265_syntax::slice::{SliceHeader, SliceType};
use ec_h265_syntax::{Pps, ScalingFactors, Sps};

/// Wavefront parallel parsing of slice segments (9.3.2.2), in its own module.
pub mod wpp;

/// The partitioning of a coding unit (Table 7-10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartMode {
    /// One prediction unit, the whole coding unit.
    Part2Nx2N,
    /// Two halves, stacked.
    Part2NxN,
    /// Two halves, side by side.
    PartNx2N,
    /// Four quarters (intra: the smallest coding unit; inter: only at the minimum size).
    PartNxN,
    /// Asymmetric: a quarter on top, three quarters below.
    Part2NxnU,
    /// Asymmetric: three quarters on top, a quarter below.
    Part2NxnD,
    /// Asymmetric: a quarter on the left, three quarters right.
    PartnLx2N,
    /// Asymmetric: three quarters on the left, a quarter right.
    PartnRx2N,
}

/// `inter_pred_idc` (Table 7-? value meaning of 7.4.9.6).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InterPredIdc {
    /// `PRED_L0`: list 0 only.
    #[default]
    PredL0,
    /// `PRED_L1`: list 1 only.
    PredL1,
    /// `PRED_BI`: both lists.
    PredBi,
}

/// The syntax of one prediction unit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PredictionUnit {
    /// Luma position of the unit, in samples of the picture.
    pub x: usize,
    /// Luma position of the unit, in samples of the picture.
    pub y: usize,
    /// Luma width.
    pub width: usize,
    /// Luma height.
    pub height: usize,
    /// `merge_flag` (true for every unit of a skipped coding unit).
    pub merge_flag: bool,
    /// `merge_idx`, 0 when not present.
    pub merge_idx: u8,
    /// `inter_pred_idc`, `PredL0` unless a B slice says otherwise.
    pub inter_pred_idc: InterPredIdc,
    /// `ref_idx_l0` and `ref_idx_l1`, 0 when not present.
    pub ref_idx: [u8; 2],
    /// `mvp_l0_flag` and `mvp_l1_flag`.
    pub mvp_flag: [u8; 2],
    /// `MvdLX[0]` and `MvdLX[1]` (horizontal, vertical), per list.
    pub mvd: [[i32; 2]; 2],
}

/// Motion of one prediction unit, as a [`MotionSource`] returns it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Motion {
    /// `predFlagLX` for list 0 and list 1.
    pub pred_flags: [bool; 2],
    /// `refIdxLX`, -1 where the list is not used.
    pub ref_idx: [i8; 2],
    /// `mvLX` (horizontal, vertical), per list.
    pub mv: [[i32; 2]; 2],
}

/// The geometry of a coding unit, as a [`MotionSource`] needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CuGeometry {
    /// Luma position of the coding unit.
    pub x: usize,
    /// Luma position of the coding unit.
    pub y: usize,
    /// Luma size of the coding unit.
    pub size: usize,
    /// The partitioning.
    pub part_mode: PartMode,
    /// Whether the coding unit is skipped.
    pub skip: bool,
}

/// Derives the motion of each prediction unit. Called once per unit, in decoding
/// order, after the unit's syntax has been parsed (so merge candidates from
/// earlier units are available to the implementation). Merge and AMVP derivation
/// (8.5.3) is the job of the implementation; the parser does not look inside.
pub trait MotionSource {
    /// The motion of unit `pu_index` of the coding unit `cu`.
    fn derive_motion(&mut self, cu: &CuGeometry, pu_index: usize, pu: &PredictionUnit) -> Motion;

    /// Called once the CTB row that ends at luma row `row_end` is fully parsed. Only
    /// pictures with one tile get the call (their CTBs come in raster order).
    fn row_done(&mut self, _pic: &mut PicState, _row_end: usize) {}

    /// The motion field this source writes, when it is a wavefront band (see [`wpp`]).
    fn band_cells(&mut self) -> Option<&mut MotionField> {
        None
    }
}

/// The placeholder [`MotionSource`]: zero motion vectors and reference index 0 for
/// every list the unit uses. Replace it with the merge/AMVP derivation.
#[derive(Clone, Copy, Debug, Default)]
pub struct ZeroMotion;

impl MotionSource for ZeroMotion {
    fn derive_motion(&mut self, _cu: &CuGeometry, _pu_index: usize, pu: &PredictionUnit) -> Motion {
        let pred_flags = if pu.merge_flag {
            [true, false]
        } else {
            match pu.inter_pred_idc {
                InterPredIdc::PredL0 => [true, false],
                InterPredIdc::PredL1 => [false, true],
                InterPredIdc::PredBi => [true, true],
            }
        };
        let ref_idx = [
            if pred_flags[0] {
                i8::try_from(pu.ref_idx[0]).unwrap_or(0)
            } else {
                -1
            },
            if pred_flags[1] {
                i8::try_from(pu.ref_idx[1]).unwrap_or(0)
            } else {
                -1
            },
        ];
        Motion {
            pred_flags,
            ref_idx,
            mv: [[0, 0], [0, 0]],
        }
    }
}

/// The residual samples of one transform block, after dequantisation and the
/// inverse transform. Samples are row-major, `size * size`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResidualBlock {
    /// 0 luma, 1 Cb, 2 Cr.
    pub component: usize,
    /// Position in samples of that component.
    pub x: usize,
    /// Position in samples of that component.
    pub y: usize,
    /// Side length in samples of that component.
    pub size: usize,
    /// The residual, `size * size` values.
    pub samples: Vec<i32>,
}

/// One coding unit, as parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodingUnit {
    /// Luma position.
    pub x: usize,
    /// Luma position.
    pub y: usize,
    /// `log2CbSize`.
    pub log2_size: u32,
    /// `cqtDepth` (CtDepth).
    pub depth: u32,
    /// `CuPredMode` is MODE_INTRA.
    pub intra: bool,
    /// `cu_skip_flag`.
    pub skip: bool,
    /// `PartMode`.
    pub part_mode: PartMode,
    /// `IntraPredModeY` of each partition (only the first one for 2Nx2N).
    pub luma_modes: [u8; 4],
    /// `IntraPredModeC` (intra only).
    pub chroma_mode: u8,
    /// The prediction units, in order.
    pub pus: Vec<PredictionUnit>,
    /// The motion of each prediction unit, from the [`MotionSource`].
    pub motion: Vec<Motion>,
    /// `rqt_root_cbf` (inferred 1 for intra).
    pub rqt_root_cbf: bool,
    /// `QpY` of the coding unit.
    pub qp_y: i32,
    /// The residual blocks, in decoding order.
    pub residuals: Vec<ResidualBlock>,
}

/// What a slice's parse produced, for the test tables and for sanity checks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SliceStats {
    /// Coding tree units parsed.
    pub ctus: usize,
    /// Coding units parsed.
    pub cus: usize,
    /// Intra coding units.
    pub intra_cus: usize,
    /// Skipped coding units.
    pub skip_cus: usize,
    /// Prediction units parsed.
    pub pus: usize,
    /// Residual blocks parsed.
    pub residual_blocks: usize,
    /// Coding tree units that carried SAO parameters.
    pub sao_ctus: usize,
    /// Whether the bytes after the end of the slice are the trailing bits
    /// (a stop bit, then zeros) that 7.3.2.9 requires.
    pub trailing_bits_ok: bool,
}

/// Parse one slice segment's data, from the byte after its header, to
/// `end_of_slice_segment_flag`. `sink` sees every coding unit in decoding order.
/// `motion` derives the motion of each prediction unit.
///
/// A desync shows up as an `end_of_slice_segment_flag` at the wrong CTB, or as a
/// trailing-bit pattern that is not the stop bit; both are errors. This form
/// keeps only the syntax: no samples are produced.
pub fn parse_slice(
    sps: &Sps,
    pps: &Pps,
    header: &SliceHeader,
    data: &[u8],
    motion: &mut dyn MotionSource,
    sink: &mut dyn FnMut(&CodingUnit),
) -> Result<SliceStats> {
    let mut pic = PicState::for_sps_pps(sps, pps, false);
    parse_slice_into(
        sps,
        pps,
        header,
        data,
        motion,
        &mut pic,
        None,
        sink,
        &mut |_, _| {},
    )
}

/// Parse one slice segment's data into `pic`. The block maps are always
/// filled. When `pic` has sample planes (pixel decode), intra coding units are
/// reconstructed in decoding order, and inter coding units are predicted from
/// `refs` (8.5.3.3) per prediction unit, then get their residual added.
#[allow(clippy::too_many_arguments)] // the slice, the picture it decodes into, and its references
pub fn parse_slice_into(
    sps: &Sps,
    pps: &Pps,
    header: &SliceHeader,
    data: &[u8],
    motion: &mut dyn MotionSource,
    pic: &mut PicState,
    refs: Option<&InterRefs>,
    sink: &mut dyn FnMut(&CodingUnit),
    rows: &mut dyn FnMut(&mut PicState, usize),
) -> Result<SliceStats> {
    let min_cb_log2 = sps.log2_min_cb_size_minus3 + 3;
    let ctb_log2 = min_cb_log2 + sps.log2_diff_max_min_cb_size;
    let slice_qp = 26 + pps.init_qp_minus26 + header.qp_delta;
    // 7.4.3.3.1: Log2ParMrgLevel = log2_parallel_merge_level_minus2 + 2 is at most CtbLog2SizeY.
    // Merge derivation shifts coordinates by it, so a larger value is a shift overflow.
    if pps.log2_parallel_merge_level_minus2.saturating_add(2) > ctb_log2 {
        return Err(Error::corrupt(
            "HEVC PPS: Log2ParMrgLevel exceeds CtbLog2SizeY",
        ));
    }
    // 7.4.3.3.1: diff_cu_qp_delta_depth is at most log2_diff_max_min_luma_coding_block_size.
    if pps.cu_qp_delta_enabled && pps.diff_cu_qp_delta_depth > sps.log2_diff_max_min_cb_size {
        return Err(Error::corrupt(
            "HEVC PPS: diff_cu_qp_delta_depth exceeds the CTB depth",
        ));
    }
    // 7-56 / 7.4.7.1: SliceQpY lies in -QpBdOffsetY..=51.
    if !(-6 * sps.bit_depth_luma_minus8 as i32..=51).contains(&slice_qp) {
        return Err(Error::corrupt(
            "HEVC slice: SliceQpY is outside -QpBdOffsetY..=51",
        ));
    }
    let init_type = match header.slice_type {
        SliceType::I => 0,
        SliceType::P => 1 + u8::from(header.cabac_init),
        SliceType::B => 2 - u8::from(header.cabac_init),
    };
    let cols = pic.ctb_cols;
    let total = cols * pic.ctb_rows;
    // 6.5.1: the tile scan of this picture (the same for every segment of it).
    pic.tiles = TileScan::from_pps(pps, cols, pic.ctb_rows)?;
    let start_ts = pic
        .tiles
        .rs_to_ts
        .get(header.segment_address as usize)
        .copied()
        .ok_or_else(|| {
            Error::corrupt("HEVC slice: slice_segment_address is outside the picture")
        })?;
    // SliceAddrRs: a dependent segment belongs to the slice of the segment before it.
    if !header.dependent_slice_segment {
        pic.carry.slice_addr = header.segment_address as usize;
    }
    let slice_addr = pic.carry.slice_addr;
    let dependent = header.dependent_slice_segment;
    let slice_index = u16::try_from(pic.slices.len()).map_err(|_| {
        Error::unsupported("HEVC slice syntax: slice count", "more than 65535 slices")
    })?;
    pic.slices.push(SliceFilter {
        deblock_disabled: header.deblocking_filter_disabled,
        beta_offset_div2: header.beta_offset_div2,
        tc_offset_div2: header.tc_offset_div2,
        loop_filter_across_slices: header.loop_filter_across_slices_enabled,
        sao_luma: header.sao_luma,
        sao_chroma: header.sao_chroma,
    });
    // qPY_PREV carries over a dependent segment (the first QG of a slice, not of a segment).
    let last_qp = if dependent {
        pic.carry.last_qp
    } else {
        slice_qp
    };
    let mut p = new_parser(sps, pps, header, pic, refs, motion, last_qp);
    let fresh = Contexts::for_init_type(slice_qp, init_type);
    let mut dec = CabacDecoder::new(data, fresh.clone());
    let wpp = pps.entropy_coding_sync_enabled;
    let raster = !pps.tiles_enabled;
    let ctb_size = 1usize << ctb_log2;
    // A slice segment ends at the first end_of_slice_segment_flag equal to 1,
    // which may come before the picture's last CTB (several slices per picture).
    let mut ended = false;
    let mut ts = start_ts;
    let mut first = true;
    while ts < total {
        let rs = p
            .pic
            .tiles
            .ts_to_rs
            .get(ts)
            .copied()
            .ok_or_else(|| Error::corrupt("HEVC slice: CTB address past the picture"))?;
        let (rx, ry) = (rs % cols, rs / cols);
        let (x0, y0) = (rx << ctb_log2, ry << ctb_log2);
        let tile_start = p.pic.tiles.is_tile_start(ts);
        let row_start = wpp && p.pic.tiles.is_row_start(rs, cols);
        let tile = p.pic.tiles.tile_of_rs.get(rs).copied().unwrap_or(0);
        let ctb = p
            .pic
            .ctb
            .get_mut(rs)
            .ok_or_else(|| Error::corrupt("HEVC slice: CTB address past the picture"))?;
        *ctb = CtbInfo {
            slice: slice_index,
            slice_addr: slice_addr as u32,
            tile,
            sao: ctb.sao,
        };
        p.stats.ctus += 1;
        // 9.3.1: the contexts at the start of a CTB. A tile starts fresh; a wavefront
        // row syncs from the CTB above and to the right (when it is available), and
        // a dependent segment restores the contexts the segment before it left.
        if tile_start {
            dec.contexts = fresh.clone();
        } else if row_start {
            let top_right = p.pic.available(
                x0,
                y0,
                (x0 + ctb_size) as isize,
                y0 as isize - ctb_size as isize,
            );
            dec.contexts = match (top_right, p.pic.carry.wpp.clone()) {
                (true, Some(saved)) => saved,
                _ => fresh.clone(),
            };
        } else if first && dependent {
            dec.contexts = p.pic.carry.ds.clone().unwrap_or_else(|| fresh.clone());
        }
        if tile_start || row_start || (first && !dependent) {
            p.last_qp = slice_qp;
        }
        if header.sao_luma || header.sao_chroma {
            p.sao(&mut dec, rs, rx, ry)?;
        }
        p.coding_quadtree(&mut dec, x0, y0, ctb_log2, 0, sink)?;
        // The row is complete with its last CTB. Single-tile pictures are in raster order.
        if raster && rx + 1 == cols {
            p.motion.row_done(p.pic, y0 + ctb_size);
            rows(p.pic, ry);
        }
        // 9.3.2.2: the wavefront storage after the second CTB of a row in a tile.
        if wpp
            && (rs % cols == 1
                || (rs > 1 && p.pic.tiles.tile_of_rs.get(rs) != p.pic.tiles.tile_of_rs.get(rs - 2)))
        {
            p.pic.carry.wpp = Some(dec.contexts.clone());
        }
        if dec.decode_terminate() != 0 {
            ended = true;
            break;
        }
        ts += 1;
        first = false;
        // 7.3.8.1: a new tile or a new wavefront row starts a substream. The next
        // CTB begins with end_of_subset_one_bit and byte alignment, then a fresh
        // arithmetic decoder (9.3.2.5). Entry point offsets are not needed for this.
        if ts < total {
            let next_rs = p.pic.tiles.ts_to_rs.get(ts).copied().unwrap_or(0);
            let next_row = wpp && p.pic.tiles.is_row_start(next_rs, cols);
            if p.pic.tiles.is_tile_start(ts) || next_row {
                if dec.decode_terminate() != 1 {
                    return Err(Error::corrupt(
                        "HEVC slice syntax: end_of_subset_one_bit was not set at a substream boundary",
                    ));
                }
                dec.restart_at_byte(dec.byte_position_aligned());
            }
        }
    }
    if !ended {
        return Err(Error::corrupt(
            "HEVC slice syntax: end_of_slice_segment_flag 0 after the last CTB of the picture",
        ));
    }
    // The state the next segment may restore (9.3.1 storage at the end of a segment).
    p.pic.carry.ds = Some(dec.contexts.clone());
    p.pic.carry.last_qp = p.last_qp;
    // rbsp_slice_segment_trailing_bits (7.3.2.10): the stop bit, zeros to the
    // byte boundary, then optional cabac_zero_words (zero bytes). Where the stop
    // bit sits relative to the decoder's read position is checked both ways.
    let read = dec.bits_read();
    p.stats.trailing_bits_ok = trailing_bits_ok(data, read);
    if !p.stats.trailing_bits_ok {
        return Err(Error::corrupt(format!(
            "HEVC slice syntax: no stop bit at the end of the slice data (CABAC read {read} bits)"
        )));
    }
    Ok(p.stats)
}

/// Whether the slice data ends with the stop bit of `rbsp_slice_segment_trailing_bits`
/// (7.3.2.10) at the bit the decoder reached (`read` bits in), or one bit before it, and
/// zeros (cabac_zero_words) follow to the end of `data`.
pub(super) fn trailing_bits_ok(data: &[u8], read: usize) -> bool {
    let stop_at = |bit: usize| -> bool {
        let Some(&byte) = data.get(bit / 8) else {
            return false;
        };
        let shift = 7 - (bit % 8);
        byte & (1 << shift) != 0
            && byte & ((1u16 << shift) - 1) as u8 == 0
            && data[bit / 8 + 1..].iter().all(|&b| b == 0)
    };
    read > 0 && (stop_at(read - 1) || stop_at(read))
}

/// The coding-tree walk of one slice segment, starting with `last_qp` as qPY_PREV.
fn new_parser<'a>(
    sps: &'a Sps,
    pps: &'a Pps,
    header: &'a SliceHeader,
    pic: &'a mut PicState,
    refs: Option<&'a InterRefs>,
    motion: &'a mut dyn MotionSource,
    last_qp: i32,
) -> Parser<'a> {
    let min_cb_log2 = sps.log2_min_cb_size_minus3 + 3;
    let ctb_log2 = min_cb_log2 + sps.log2_diff_max_min_cb_size;
    let min_tb_log2 = sps.log2_min_tb_size_minus2 + 2;
    let slice_qp = 26 + pps.init_qp_minus26 + header.qp_delta;
    Parser {
        sps,
        pps,
        header,
        scaling: ScalingFactors::for_picture(sps, pps),
        pic,
        refs,
        mc_scratch: McScratch::new(),
        min_cb_log2,
        ctb_log2,
        min_tb_log2,
        max_tb_log2: min_tb_log2 + sps.log2_diff_max_min_tb_size,
        max_depth_inter: sps.max_transform_hierarchy_depth_inter,
        max_depth_intra: sps.max_transform_hierarchy_depth_intra,
        max_merge: 5 - header.five_minus_max_num_merge_cand.min(4),
        qg_log2: ctb_log2
            - if pps.cu_qp_delta_enabled {
                pps.diff_cu_qp_delta_depth
            } else {
                0
            },
        qp_bd_y: 6 * sps.bit_depth_luma_minus8 as i32,
        qp_bd_c: 6 * sps.bit_depth_chroma_minus8 as i32,
        last_qp,
        qg_pred: slice_qp,
        qp_delta: 0,
        qp_delta_coded: false,
        cu_chroma_qp_offset_enabled: header.cu_chroma_qp_offset_enabled,
        chroma_qg_coded: false,
        chroma_qg_log2: ctb_log2 - pps.range.diff_cu_chroma_qp_offset_depth,
        cu_qp_offset: [0, 0],
        cu_bypass: false,
        chroma_modes: ([1; 4], [false; 4]),
        motion,
        stats: SliceStats::default(),
    }
}

/// Per-transform-tree facts that do not change down the tree.
#[derive(Clone, Copy)]
struct TxInfo {
    intra: bool,
    intra_split: bool,
    inter_split: bool,
    max_depth: u32,
    /// `IntraPredModeC` of each partition (all four the same unless 4:4:4 NxN).
    chroma_modes: [u8; 4],
    /// `intra_chroma_pred_mode == 4` (the derived mode) of each partition.
    chroma_dm: [bool; 4],
    /// The coding unit's position and size, to find the partition of a transform block.
    cu_x: usize,
    cu_y: usize,
    cu_log2: u32,
}

/// One node of the transform tree (7.3.8.8).
#[derive(Clone, Copy)]
struct TxNode {
    x0: usize,
    y0: usize,
    x_base: usize,
    y_base: usize,
    log2: u32,
    depth: u32,
    blk_idx: usize,
}

/// The coded block flags of a transform unit.
/// Table 8-3: the 4:2:2 chroma intra mode for each intra mode (the mode of the 4:2:0 table
/// that the 4:2:2 chroma grid maps it to). Horizontal-ish modes (2..17) double their
/// angle for the half-width chroma grid, so 11 (-2) goes to 12 (-5) and 14 (-13) to 17 (-26).
const MODE_422: [u8; 35] = [
    0, 1, 2, 2, 2, 2, 3, 5, 7, 8, 10, 12, 13, 15, 17, 18, 19, 20, 21, 22, 23, 23, 24, 24, 25, 25,
    26, 27, 27, 28, 28, 29, 29, 30, 31,
];

#[derive(Clone, Copy)]
struct Cbf {
    luma: bool,
    /// `cbf_cb`: one flag, or two for 4:2:2 (the upper and lower square blocks).
    cb: [bool; 2],
    /// `cbf_cr`, as `cb`.
    cr: [bool; 2],
}

/// The coding-tree walk state for one slice segment.
struct Parser<'a> {
    sps: &'a Sps,
    pps: &'a Pps,
    header: &'a SliceHeader,
    /// The picture being decoded: maps, samples, CTB state.
    pic: &'a mut PicState,
    /// The reference samples and weights of the slice (`None` for an I slice).
    refs: Option<&'a InterRefs>,
    /// Scratch buffers of motion compensation, reused across prediction units.
    mc_scratch: McScratch,
    min_cb_log2: u32,
    ctb_log2: u32,
    min_tb_log2: u32,
    max_tb_log2: u32,
    max_depth_inter: u32,
    max_depth_intra: u32,
    max_merge: u32,
    /// `Log2MinCuQpDeltaSize`.
    qg_log2: u32,
    /// The scaling factors of the picture (`None`: flat 16, 7.4.3.3).
    scaling: Option<ScalingFactors>,
    /// `QpBdOffsetY` and `QpBdOffsetC`.
    qp_bd_y: i32,
    qp_bd_c: i32,
    /// `qPY_PREV`: the QpY of the last coding unit parsed.
    last_qp: i32,
    /// `qPY_PRED` of the current quantization group.
    qg_pred: i32,
    /// `CuQpDeltaVal`.
    qp_delta: i32,
    /// `IsCuQpDeltaCoded`.
    qp_delta_coded: bool,
    /// `cu_chroma_qp_offset_enabled_flag` of the slice.
    cu_chroma_qp_offset_enabled: bool,
    /// `IsCuChromaQpOffsetCoded`.
    chroma_qg_coded: bool,
    /// `Log2MinCuChromaQpOffsetSize`.
    chroma_qg_log2: u32,
    /// `CuQpOffsetCb` and `CuQpOffsetCr`.
    cu_qp_offset: [i32; 2],
    /// `cu_transquant_bypass_flag` of the coding unit being parsed.
    cu_bypass: bool,
    /// The chroma modes of the intra coding unit being parsed, with their DM flags.
    chroma_modes: ([u8; 4], [bool; 4]),
    motion: &'a mut dyn MotionSource,
    stats: SliceStats,
}

impl Parser<'_> {
    fn info_at(&self, x: usize, y: usize) -> BlockInfo {
        self.pic.info[self.pic.unit(x, y)]
    }

    /// `QpY` of the current coding unit (8-283): the prediction plus
    /// CuQpDeltaVal, wrapped into the range `-QpBdOffsetY..=51`.
    fn cu_qp(&self) -> i32 {
        let off = self.qp_bd_y;
        // rem_euclid keeps the result in range even for a CuQpDeltaVal outside its
        // legal span (checked in the parser) or an overflowing sum.
        let sum = self
            .qg_pred
            .saturating_add(self.qp_delta)
            .saturating_add(52 + 2 * off);
        sum.rem_euclid(52 + off) - off
    }

    /// `qPY_PRED` for the quantization group at `(x, y)` (8.6.1).
    fn start_quantization_group(&mut self, x: usize, y: usize) {
        self.qp_delta_coded = false;
        self.qp_delta = 0;
        let prev = self.last_qp;
        let ctb_mask = (1usize << self.ctb_log2) - 1;
        let qa = if x & ctb_mask != 0 {
            i32::from(self.info_at(x - 1, y).qp)
        } else {
            prev
        };
        let qb = if y & ctb_mask != 0 {
            i32::from(self.info_at(x, y - 1).qp)
        } else {
            prev
        };
        self.qg_pred = (qa + qb + 1) >> 1;
    }

    fn coding_quadtree(
        &mut self,
        dec: &mut CabacDecoder,
        x0: usize,
        y0: usize,
        log2: u32,
        depth: u32,
        sink: &mut dyn FnMut(&CodingUnit),
    ) -> Result<()> {
        let n = 1usize << log2;
        let fits = x0 + n <= self.pic.width && y0 + n <= self.pic.height;
        let split = if fits && log2 > self.min_cb_log2 {
            let mut inc = 0;
            if self.pic.available(x0, y0, x0 as isize - 1, y0 as isize)
                && u32::from(self.info_at(x0 - 1, y0).depth) > depth
            {
                inc += 1;
            }
            if self.pic.available(x0, y0, x0 as isize, y0 as isize - 1)
                && u32::from(self.info_at(x0, y0 - 1).depth) > depth
            {
                inc += 1;
            }
            dec.decode_bin(ctx::SPLIT_CU + inc) != 0
        } else {
            log2 > self.min_cb_log2
        };
        if log2 >= self.qg_log2 {
            self.start_quantization_group(x0, y0);
        }
        if self.cu_chroma_qp_offset_enabled && log2 >= self.chroma_qg_log2 {
            self.chroma_qg_coded = false;
        }
        if split {
            let half = n / 2;
            for (dx, dy) in [(0, 0), (half, 0), (0, half), (half, half)] {
                if x0 + dx < self.pic.width && y0 + dy < self.pic.height {
                    self.coding_quadtree(dec, x0 + dx, y0 + dy, log2 - 1, depth + 1, sink)?;
                }
            }
            Ok(())
        } else {
            self.coding_unit(dec, x0, y0, log2, depth, sink)
        }
    }

    /// `pcm_flag` may be read for a coding unit of this size (7.3.8.5).
    fn pcm_size_ok(&self, log2: u32) -> bool {
        match self.sps.pcm {
            Some((_, _, min_m3, diff, _)) if self.sps.pcm_enabled => {
                let min = min_m3 + 3;
                (min..=min + diff).contains(&log2)
            }
            _ => false,
        }
    }

    fn coding_unit(
        &mut self,
        dec: &mut CabacDecoder,
        x0: usize,
        y0: usize,
        log2: u32,
        depth: u32,
        sink: &mut dyn FnMut(&CodingUnit),
    ) -> Result<()> {
        let n = 1usize << log2;
        self.stats.cus += 1;
        let is_i = self.header.slice_type == SliceType::I;
        let bypass =
            self.pps.transquant_bypass_enabled && dec.decode_bin(ctx::CU_TRANSQUANT_BYPASS) != 0;
        self.cu_bypass = bypass;
        let skip = !is_i && {
            let mut inc = 0;
            if self.pic.available(x0, y0, x0 as isize - 1, y0 as isize)
                && self.info_at(x0 - 1, y0).skip
            {
                inc += 1;
            }
            if self.pic.available(x0, y0, x0 as isize, y0 as isize - 1)
                && self.info_at(x0, y0 - 1).skip
            {
                inc += 1;
            }
            dec.decode_bin(ctx::CU_SKIP + inc) != 0
        };
        let intra = if skip {
            false
        } else if is_i {
            true
        } else {
            dec.decode_bin(ctx::PRED_MODE) != 0
        };
        let mut cu = CodingUnit {
            x: x0,
            y: y0,
            log2_size: log2,
            depth,
            intra,
            skip,
            part_mode: PartMode::Part2Nx2N,
            luma_modes: [1; 4],
            chroma_mode: 1,
            pus: Vec::new(),
            motion: Vec::new(),
            rqt_root_cbf: false,
            qp_y: 0,
            residuals: Vec::new(),
        };
        self.pic.update_info(x0, y0, n, n, |b| {
            b.depth = depth as u8;
            b.skip = skip;
            b.intra = intra;
            b.bypass = bypass;
            b.pcm = false;
            b.intra_mode = 1;
            b.nz_luma = false;
            b.edge_v = 0;
            b.edge_h = 0;
        });
        // The coding unit's own boundary is a transform (and prediction) edge.
        self.pic.mark_edges(x0, y0, n, n, EDGE_TU | EDGE_PU);

        if skip {
            self.stats.skip_cus += 1;
            let geom = CuGeometry {
                x: x0,
                y: y0,
                size: n,
                part_mode: PartMode::Part2Nx2N,
                skip: true,
            };
            let pu = self.prediction_unit(dec, x0, y0, n, n, depth, true)?;
            let m = self.motion.derive_motion(&geom, 0, &pu);
            self.predict_pu(x0, y0, n, n, &m)?;
            cu.motion.push(m);
            cu.pus.push(pu);
        } else if intra {
            self.stats.intra_cus += 1;
            cu.part_mode = if log2 == self.min_cb_log2 && dec.decode_bin(ctx::PART_MODE) == 0 {
                PartMode::PartNxN
            } else {
                PartMode::Part2Nx2N
            };
            let nparts = if cu.part_mode == PartMode::PartNxN {
                4
            } else {
                1
            };
            let pu_size = if nparts == 4 { n / 2 } else { n };
            if cu.part_mode == PartMode::Part2Nx2N
                && self.pcm_size_ok(log2)
                && dec.decode_terminate() != 0
            {
                self.pcm_cu(dec, x0, y0, log2)?;
            } else {
                cu.luma_modes = self.intra_luma_modes(dec, x0, y0, pu_size, nparts);
                // 4:4:4 codes one chroma mode per partition, other formats one in all.
                let nchroma = match self.sps.chroma_format_idc {
                    0 => 0,
                    3 => nparts,
                    _ => 1,
                };
                let mut chroma_modes = [1u8; 4];
                let mut chroma_dm = [false; 4];
                for i in 0..nchroma {
                    (chroma_modes[i], chroma_dm[i]) = self.chroma_mode(dec, cu.luma_modes[i]);
                }
                if nchroma == 1 {
                    chroma_modes = [chroma_modes[0]; 4];
                    chroma_dm = [chroma_dm[0]; 4];
                }
                cu.chroma_mode = chroma_modes[0];
                cu.rqt_root_cbf = true;
                self.chroma_modes = (chroma_modes, chroma_dm);
                self.transform_tree_root(dec, &mut cu, log2, true)?;
            }
        } else {
            cu.part_mode = if dec.decode_bin(ctx::PART_MODE) == 0 {
                self.inter_part_mode(dec, log2)
            } else {
                PartMode::Part2Nx2N
            };
            let geom = CuGeometry {
                x: x0,
                y: y0,
                size: n,
                part_mode: cu.part_mode,
                skip: false,
            };
            for (i, (px, py, pw, ph)) in pu_rects(cu.part_mode, x0, y0, n).into_iter().enumerate() {
                let pu = self.prediction_unit(dec, px, py, pw, ph, depth, false)?;
                let m = self.motion.derive_motion(&geom, i, &pu);
                self.predict_pu(px, py, pw, ph, &m)?;
                self.pic.mark_edges(px, py, pw, ph, EDGE_PU);
                cu.motion.push(m);
                cu.pus.push(pu);
            }
            cu.rqt_root_cbf = if !(cu.part_mode == PartMode::Part2Nx2N && cu.pus[0].merge_flag) {
                dec.decode_bin(ctx::RQT_ROOT_CBF) != 0
            } else {
                true
            };
            if cu.rqt_root_cbf {
                self.transform_tree_root(dec, &mut cu, log2, false)?;
            }
        }
        if !intra {
            inter::add_residuals(self.pic, &cu.residuals);
        }
        cu.qp_y = self.cu_qp();
        self.last_qp = cu.qp_y;
        let qp = cu.qp_y as i8;
        self.pic.update_info(x0, y0, n, n, |b| b.qp = qp);
        self.stats.pus += cu.pus.len();
        self.stats.residual_blocks += cu.residuals.len();
        sink(&cu);
        Ok(())
    }

    /// Motion-compensated prediction of one prediction unit (8.5.3.3) into the
    /// picture's samples. Nothing happens without pixel decode.
    fn predict_pu(&mut self, x: usize, y: usize, w: usize, h: usize, m: &Motion) -> Result<()> {
        if !self.pic.has_pixels() {
            return Ok(());
        }
        let Some(refs) = self.refs else {
            return Err(Error::corrupt(
                "HEVC decode: an inter prediction unit in a slice with no reference lists",
            ));
        };
        let mut out = self.pic.planes_mut();
        refs.predict(
            m,
            Block {
                x,
                y,
                width: w,
                height: h,
            },
            &mut out,
            &mut self.mc_scratch,
        )
    }

    /// The transform tree of a coding unit, from its root.
    fn transform_tree_root(
        &mut self,
        dec: &mut CabacDecoder,
        cu: &mut CodingUnit,
        log2: u32,
        intra: bool,
    ) -> Result<()> {
        let intra_split = intra && cu.part_mode == PartMode::PartNxN;
        let tx = TxInfo {
            intra,
            intra_split,
            inter_split: !intra && cu.part_mode != PartMode::Part2Nx2N && self.max_depth_inter == 0,
            max_depth: if intra {
                self.max_depth_intra + u32::from(intra_split)
            } else {
                self.max_depth_inter
            },
            chroma_modes: self.chroma_modes.0,
            chroma_dm: self.chroma_modes.1,
            cu_x: cu.x,
            cu_y: cu.y,
            cu_log2: log2,
        };
        let node = TxNode {
            x0: cu.x,
            y0: cu.y,
            x_base: cu.x,
            y_base: cu.y,
            log2,
            depth: 0,
            blk_idx: 0,
        };
        self.transform_tree(dec, &tx, node, [[false; 2]; 2], &mut cu.residuals)
    }

    /// `intra_luma_prev_intra_pred_flag`, `mpm_idx` or `rem_intra_luma_pred_mode`
    /// for each partition, with the modes written to the map as they are derived.
    fn intra_luma_modes(
        &mut self,
        dec: &mut CabacDecoder,
        x0: usize,
        y0: usize,
        pu_size: usize,
        nparts: usize,
    ) -> [u8; 4] {
        let mut prev = [false; 4];
        for flag in prev.iter_mut().take(nparts) {
            *flag = dec.decode_bin(ctx::PREV_INTRA_LUMA_PRED) != 0;
        }
        let mut modes = [1u8; 4];
        for (i, mode_out) in modes.iter_mut().enumerate().take(nparts) {
            let px = x0 + (i & 1) * pu_size;
            let py = y0 + (i >> 1) * pu_size;
            let mpm = self.mpm(px, py);
            let mode = if prev[i] {
                let idx = if dec.decode_bypass() == 0 {
                    0
                } else {
                    1 + dec.decode_bypass() as usize
                };
                mpm[idx]
            } else {
                let rem = dec.decode_bypass_bits(5) as u8;
                let mut sorted = mpm;
                sorted.sort_unstable();
                let mut mode = rem;
                for m in sorted {
                    if mode >= m {
                        mode += 1;
                    }
                }
                mode
            };
            *mode_out = mode;
            self.pic
                .update_info(px, py, pu_size, pu_size, |b| b.intra_mode = mode);
        }
        modes
    }

    /// `candModeList` of 8.4.2 for the partition at `(x, y)`. A neighbour that is
    /// unavailable, not intra, PCM, or above the current CTB row reads as DC.
    fn mpm(&self, x: usize, y: usize) -> [u8; 3] {
        let left = if self.pic.available(x, y, x as isize - 1, y as isize) {
            self.info_at(x - 1, y).intra_mode
        } else {
            1
        };
        let ctb_mask = (1usize << self.ctb_log2) - 1;
        let above = if y & ctb_mask != 0 {
            self.info_at(x, y - 1).intra_mode
        } else {
            1
        };
        intra::mpm_list(Some(left), Some(above))
    }

    /// `intra_chroma_pred_mode` and the mode it names (8.4.3, Table 8-2).
    fn chroma_mode(&self, dec: &mut CabacDecoder, luma: u8) -> (u8, bool) {
        let dm = dec.decode_bin(ctx::INTRA_CHROMA_PRED_MODE) == 0;
        let mode = if dm {
            luma
        } else {
            let candidate = [0u8, 26, 10, 1][dec.decode_bypass_bits(2) as usize];
            if candidate == luma { 34 } else { candidate }
        };
        // 8.4.3, Table 8-3: the 4:2:2 chroma mode is the angle that the 4:2:2 chroma
        // grid gives the mode's angle in 4:2:0 terms.
        let mode = if self.sps.chroma_format_idc == 2 {
            MODE_422[usize::from(mode)]
        } else {
            mode
        };
        (mode, dm)
    }

    /// `part_mode` of an inter coding unit (9.3.3.7, Table 9-43). The first bin
    /// is read by the caller.
    fn inter_part_mode(&mut self, dec: &mut CabacDecoder, log2: u32) -> PartMode {
        if log2 == self.min_cb_log2 {
            if dec.decode_bin(ctx::PART_MODE + 1) != 0 {
                return PartMode::Part2NxN;
            }
            if log2 == 3 {
                return PartMode::PartNx2N;
            }
            return if dec.decode_bin(ctx::PART_MODE + 2) != 0 {
                PartMode::PartNx2N
            } else {
                PartMode::PartNxN
            };
        }
        if !self.sps.amp_enabled {
            return if dec.decode_bin(ctx::PART_MODE + 1) != 0 {
                PartMode::Part2NxN
            } else {
                PartMode::PartNx2N
            };
        }
        let horizontal = dec.decode_bin(ctx::PART_MODE + 1) != 0;
        if dec.decode_bin(ctx::PART_MODE + 3) != 0 {
            return if horizontal {
                PartMode::Part2NxN
            } else {
                PartMode::PartNx2N
            };
        }
        let second = dec.decode_bypass() != 0;
        match (horizontal, second) {
            (true, false) => PartMode::Part2NxnU,
            (true, true) => PartMode::Part2NxnD,
            (false, false) => PartMode::PartnLx2N,
            (false, true) => PartMode::PartnRx2N,
        }
    }

    /// One prediction unit (7.3.8.6). `skip` is the coding unit's `cu_skip_flag`.
    #[allow(clippy::too_many_arguments)] // the unit's position, size and the CU facts it reads
    fn prediction_unit(
        &mut self,
        dec: &mut CabacDecoder,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        depth: u32,
        skip: bool,
    ) -> Result<PredictionUnit> {
        let mut pu = PredictionUnit {
            x,
            y,
            width,
            height,
            ..PredictionUnit::default()
        };
        pu.merge_flag = skip || dec.decode_bin(ctx::MERGE_FLAG) != 0;
        if pu.merge_flag {
            if self.max_merge > 1 {
                pu.merge_idx = truncated_unary(dec, self.max_merge - 1, ctx::MERGE_IDX) as u8;
            }
            return Ok(pu);
        }
        pu.inter_pred_idc = if self.header.slice_type == SliceType::B {
            inter_pred_idc(dec, width, height, depth)
        } else {
            InterPredIdc::PredL0
        };
        let uses_l0 = pu.inter_pred_idc != InterPredIdc::PredL1;
        let uses_l1 = pu.inter_pred_idc != InterPredIdc::PredL0;
        if uses_l0 {
            if self.header.num_ref_idx_l0_active_minus1 > 0 {
                pu.ref_idx[0] = self.ref_idx(dec, self.header.num_ref_idx_l0_active_minus1) as u8;
            }
            pu.mvd[0] = self.mvd_coding(dec)?;
            pu.mvp_flag[0] = dec.decode_bin(ctx::MVP_FLAG) as u8;
        }
        if uses_l1 {
            if self.header.num_ref_idx_l1_active_minus1 > 0 {
                pu.ref_idx[1] = self.ref_idx(dec, self.header.num_ref_idx_l1_active_minus1) as u8;
            }
            pu.mvd[1] = if self.header.mvd_l1_zero && pu.inter_pred_idc == InterPredIdc::PredBi {
                [0, 0]
            } else {
                self.mvd_coding(dec)?
            };
            pu.mvp_flag[1] = dec.decode_bin(ctx::MVP_FLAG) as u8;
        }
        Ok(pu)
    }

    /// `ref_idx_lX`: truncated rice, bins 0 and 1 in contexts, the rest bypass.
    fn ref_idx(&self, dec: &mut CabacDecoder, cmax: u32) -> u32 {
        let mut v = 0;
        while v < cmax {
            let bin = if v < 2 {
                dec.decode_bin(ctx::REF_IDX + v as usize)
            } else {
                dec.decode_bypass()
            };
            if bin == 0 {
                break;
            }
            v += 1;
        }
        v
    }

    /// `mvd_coding` (7.3.8.9): the two greater-than flags of each component,
    /// then for each component `abs_mvd_minus2` (EG1, bypass) and the sign.
    fn mvd_coding(&self, dec: &mut CabacDecoder) -> Result<[i32; 2]> {
        let gt0 = [
            dec.decode_bin(ctx::ABS_MVD_GT0) != 0,
            dec.decode_bin(ctx::ABS_MVD_GT0) != 0,
        ];
        let gt1 = [
            gt0[0] && dec.decode_bin(ctx::ABS_MVD_GT1) != 0,
            gt0[1] && dec.decode_bin(ctx::ABS_MVD_GT1) != 0,
        ];
        let mut mvd = [0i32; 2];
        for (c, mvd_c) in mvd.iter_mut().enumerate() {
            if !gt0[c] {
                continue;
            }
            let abs = if gt1[c] {
                2 + i64::from(decode_exp_golomb(dec, 1)?)
            } else {
                1
            };
            let negative = dec.decode_bypass() != 0;
            let value = if negative { -abs } else { abs };
            *mvd_c = i32::try_from(value)
                .map_err(|_| Error::corrupt("HEVC slice syntax: motion vector difference"))?;
        }
        Ok(mvd)
    }

    /// The transform tree (7.3.8.8).
    fn transform_tree(
        &mut self,
        dec: &mut CabacDecoder,
        tx: &TxInfo,
        node: TxNode,
        parent_cbf: [[bool; 2]; 2],
        out: &mut Vec<ResidualBlock>,
    ) -> Result<()> {
        let TxNode {
            x0,
            y0,
            log2,
            depth,
            ..
        } = node;
        let split = if log2 <= self.max_tb_log2
            && log2 > self.min_tb_log2
            && depth < tx.max_depth
            && !(tx.intra_split && depth == 0)
        {
            dec.decode_bin(ctx::SPLIT_TRANSFORM + (5 - log2) as usize) != 0
        } else {
            log2 > self.max_tb_log2
                || (tx.intra_split && depth == 0)
                || (tx.inter_split && depth == 0)
        };
        // ChromaArrayType 2 (4:2:2) codes a second cbf per chroma plane when the node
        // is not split, or is 8x8 (7.3.8.8).
        let second = self.sps.chroma_format_idc == 2 && (!split || log2 == 3);
        // 4:2:0 chroma. Below an 8x8 luma node the chroma flags are the parent's
        // (7.3.8.10 reads them at trafoDepth - 1); the chroma blocks ride on the
        // last 4x4 luma block.
        let idc = self.sps.chroma_format_idc;
        let (cb, cr) = if idc == 0 {
            ([false; 2], [false; 2])
        } else if log2 > 2 || idc == 3 {
            let mut planes = [[false; 2]; 2];
            for (c, flags) in planes.iter_mut().enumerate() {
                if depth == 0 || parent_cbf[c][0] {
                    flags[0] = dec.decode_bin(ctx::CBF_CHROMA + depth as usize) != 0;
                    if second {
                        flags[1] = dec.decode_bin(ctx::CBF_CHROMA + depth as usize) != 0;
                    }
                }
            }
            (planes[0], planes[1])
        } else {
            (parent_cbf[0], parent_cbf[1])
        };
        if split {
            let half = 1usize << (log2 - 1);
            for (i, (dx, dy)) in [(0, 0), (half, 0), (0, half), (half, half)]
                .into_iter()
                .enumerate()
            {
                let child = TxNode {
                    x0: x0 + dx,
                    y0: y0 + dy,
                    x_base: x0,
                    y_base: y0,
                    log2: log2 - 1,
                    depth: depth + 1,
                    blk_idx: i,
                };
                self.transform_tree(dec, tx, child, [cb, cr], out)?;
            }
            return Ok(());
        }
        let luma = if tx.intra || depth != 0 || cb.iter().chain(cr.iter()).any(|&f| f) {
            dec.decode_bin(ctx::CBF_LUMA + usize::from(depth == 0)) != 0
        } else {
            true
        };
        self.transform_unit(dec, tx, node, Cbf { luma, cb, cr }, out)
    }

    /// The transform unit (7.3.8.10): the QP delta, then each coded block's
    /// residual. With pixel decode on, an intra block is predicted and
    /// reconstructed here, in the order luma, Cb, Cr that its neighbours need.
    fn transform_unit(
        &mut self,
        dec: &mut CabacDecoder,
        tx: &TxInfo,
        node: TxNode,
        cbf: Cbf,
        out: &mut Vec<ResidualBlock>,
    ) -> Result<()> {
        let TxNode {
            x0,
            y0,
            x_base,
            y_base,
            log2,
            blk_idx,
            ..
        } = node;
        let n = 1usize << log2;
        self.pic.mark_edges(x0, y0, n, n, EDGE_TU);
        if (cbf.luma || cbf.cb.iter().chain(cbf.cr.iter()).any(|&f| f))
            && self.pps.cu_qp_delta_enabled
            && !self.qp_delta_coded
        {
            self.cu_qp_delta(dec)?;
        }
        let chroma_coded = cbf.cb.iter().chain(cbf.cr.iter()).any(|&f| f);
        if self.cu_chroma_qp_offset_enabled
            && chroma_coded
            && !self.cu_bypass
            && !self.chroma_qg_coded
        {
            self.cu_chroma_qp_offset(dec);
        }
        let recon = tx.intra && self.pic.has_pixels();
        let luma_mode = tx
            .intra
            .then(|| self.pic.info[self.pic.unit(x0, y0)].intra_mode);
        // disableIntraBoundaryFilter (8.4.4.2.6): implicit RDPCM on a transquant-bypass unit.
        let no_boundary = self.sps.range.implicit_rdpcm_enabled && self.cu_bypass;
        if let (true, Some(mode)) = (recon, luma_mode) {
            self.pic
                .predict_intra_ext(0, x0, y0, log2, mode, no_boundary);
        }
        let cross = self.pps.range.cross_component_prediction_enabled;
        let mut luma_residual: Option<Vec<i32>> = None;
        if cbf.luma {
            self.pic.update_info(x0, y0, n, n, |b| b.nz_luma = true);
            let block = self.residual_block(dec, 0, x0, y0, log2, luma_mode);
            if recon {
                self.pic.add_residual(0, x0, y0, n, &block.samples);
            }
            if cross {
                luma_residual = Some(block.samples.clone());
            }
            out.push(block);
        }
        // 4:2:0 and 4:2:2 chroma: a luma TU of 8x8 or more carries its own chroma blocks;
        // a 4x4 luma TU carries the chroma of its 8x8 parent with the fourth block. 4:4:4
        // chroma blocks are the size of the luma block, 4x4 included.
        let idc = self.sps.chroma_format_idc;
        let (sx, sy) = chroma_shifts(idc);
        let chroma = if idc == 0 {
            None
        } else if idc == 3 {
            Some(((x0, y0), log2))
        } else if log2 > 2 {
            Some(((x0 >> sx, y0 >> sy), log2 - 1))
        } else if blk_idx == 3 {
            Some(((x_base >> sx, y_base >> sy), 2))
        } else {
            None
        };
        if let Some(((cx, cy), clog2)) = chroma {
            let part = if tx.intra_split && idc == 3 {
                let half = 1usize << (tx.cu_log2 - 1);
                usize::from(y0 >= tx.cu_y + half) * 2 + usize::from(x0 >= tx.cu_x + half)
            } else {
                0
            };
            let mode = tx.intra.then_some(tx.chroma_modes[part]);
            let dm = !tx.intra || tx.chroma_dm[part];
            // 4:2:2 splits the chroma block into two square blocks, the upper then the
            // lower, each with its own cbf and its own intra prediction (8.4.4.1).
            let parts = if idc == 2 { 2 } else { 1 };
            for c in 1..=2usize {
                let flags = if c == 1 { cbf.cb } else { cbf.cr };
                let mut res_scale = 0i32;
                if cross && cbf.luma && dm {
                    res_scale = self.cross_comp_pred(dec, c - 1);
                }
                for (k, &coded) in flags.iter().enumerate().take(parts) {
                    let by = cy + (k << clog2);
                    if let (true, Some(m)) = (recon, mode) {
                        self.pic.predict_intra_ext(c, cx, by, clog2, m, no_boundary);
                    }
                    let mut block = if coded {
                        Some(self.residual_block(dec, c, cx, by, clog2, mode))
                    } else if res_scale != 0 {
                        let size = 1usize << clog2;
                        Some(ResidualBlock {
                            component: c,
                            x: cx,
                            y: by,
                            size,
                            samples: vec![0; size * size],
                        })
                    } else {
                        None
                    };
                    if let Some(block) = block.as_mut() {
                        if let (true, Some(luma)) = (res_scale != 0, luma_residual.as_ref()) {
                            rext::cross_component(
                                &mut block.samples,
                                luma,
                                res_scale,
                                self.pic.bit_depth_y,
                                self.pic.bit_depth_c,
                            );
                        }
                        if recon {
                            self.pic.add_residual(c, cx, by, 1 << clog2, &block.samples);
                        }
                    }
                    if let Some(block) = block {
                        out.push(block);
                    }
                }
            }
        }
        Ok(())
    }

    /// `cross_comp_pred(x0, y0, c)` (7.3.8.12): `ResScaleVal` of chroma component `c`
    /// (0 Cb, 1 Cr), 0 when `log2_res_scale_abs_plus1` is 0.
    fn cross_comp_pred(&mut self, dec: &mut CabacDecoder, c: usize) -> i32 {
        // TR, cMax 4, context 4 * c + binIdx.
        let mut abs_plus1 = 0usize;
        while abs_plus1 < 4 && dec.decode_bin(ctx::LOG2_RES_SCALE_ABS + 4 * c + abs_plus1) != 0 {
            abs_plus1 += 1;
        }
        if abs_plus1 == 0 {
            return 0;
        }
        let negative = dec.decode_bin(ctx::RES_SCALE_SIGN + c) != 0;
        let magnitude = 1i32 << (abs_plus1 - 1);
        if negative { -magnitude } else { magnitude }
    }

    /// `cu_chroma_qp_offset_flag` and `cu_chroma_qp_offset_idx` (7.3.8.10, 7.4.9.14):
    /// sets `CuQpOffsetCb` and `CuQpOffsetCr` from the PPS lists. The index is a truncated
    /// unary code with one context (Table 9-41).
    fn cu_chroma_qp_offset(&mut self, dec: &mut CabacDecoder) {
        let flag = dec.decode_bin(ctx::CU_CHROMA_QP_OFFSET_FLAG) != 0;
        let len = self.pps.range.cb_qp_offset_list.len();
        let mut idx = 0usize;
        if flag {
            while idx + 1 < len && dec.decode_bin(ctx::CU_CHROMA_QP_OFFSET_IDX) != 0 {
                idx += 1;
            }
            self.cu_qp_offset = [
                self.pps.range.cb_qp_offset_list[idx],
                self.pps.range.cr_qp_offset_list[idx],
            ];
        } else {
            self.cu_qp_offset = [0, 0];
        }
        self.chroma_qg_coded = true;
    }

    /// `cu_qp_delta_abs` and its sign (9.3.3.10: TU prefix up to 5, EG0 suffix).
    fn cu_qp_delta(&mut self, dec: &mut CabacDecoder) -> Result<()> {
        let mut prefix = 0u32;
        while prefix < 5 {
            let inc = if prefix == 0 { 0 } else { 1 };
            if dec.decode_bin(ctx::CU_QP_DELTA_ABS + inc) == 0 {
                break;
            }
            prefix += 1;
        }
        let mut abs = i64::from(prefix);
        if prefix == 5 {
            abs += i64::from(decode_exp_golomb(dec, 0)?);
        }
        let value = if abs > 0 && dec.decode_bypass() != 0 {
            -abs
        } else {
            abs
        };
        self.qp_delta = i32::try_from(value)
            .map_err(|_| Error::corrupt("HEVC slice syntax: cu_qp_delta_abs"))?;
        self.qp_delta_coded = true;
        Ok(())
    }

    /// One transform block: its coefficients through the residual decoder, then
    /// the residual samples. A transquant-bypass coding unit's residual is its
    /// levels (8.6.2); otherwise dequantisation (8.6.3) and the inverse
    /// transform (8.6.4) at the picture's bit depth. The range extension adds the
    /// rotation of 4x4 transform-skipped or bypassed blocks and RDPCM (8.6.2).
    #[allow(clippy::too_many_arguments)] // a block's position, size and scan source
    fn residual_block(
        &mut self,
        dec: &mut CabacDecoder,
        c_idx: usize,
        x: usize,
        y: usize,
        log2: u32,
        intra_mode: Option<u8>,
    ) -> ResidualBlock {
        let n = 1usize << log2;
        let bypass = self.cu_bypass;
        let scan = intra_mode.map_or(0, |m| {
            intra::scan_index_fmt(m, log2, c_idx == 0, self.sps.chroma_format_idc == 3)
        });
        let range = &self.sps.range;
        let cfg = ResidualCfg {
            log2_size: log2,
            c_idx,
            scan_idx: scan,
            sign_hiding: self.pps.sign_data_hiding_enabled,
            bypass,
            ts_enabled: self.pps.transform_skip_enabled,
            log2_max_ts: 2 + self.pps.range.log2_max_transform_skip_block_size_minus2,
            ts_context: range.transform_skip_context_enabled,
            persistent_rice: range.persistent_rice_adaptation_enabled,
            implicit_rdpcm: range.implicit_rdpcm_enabled,
            explicit_rdpcm: range.explicit_rdpcm_enabled,
            intra_mode,
        };
        let mut levels = vec![0i32; n * n];
        let flags = decode_residual_ext(dec, &mut levels, &cfg);
        let transform_skip = flags.transform_skip;
        let rotate = range.transform_skip_rotation_enabled
            && n == 4
            && intra_mode.is_some()
            && (transform_skip || bypass);
        let samples = if bypass {
            if rotate {
                rext::rotate_180(&mut levels, n);
            }
            if let Some(dir) = flags.rdpcm {
                rext::rdpcm_accumulate(&mut levels, n, dir);
            }
            levels
        } else {
            let bd = self.pic.bit_depth(c_idx);
            let qp_y = self.cu_qp();
            let qp = if c_idx == 0 {
                qp_y + self.qp_bd_y
            } else {
                let offset = if c_idx == 1 {
                    self.pps.cb_qp_offset + self.header.cb_qp_offset + self.cu_qp_offset[0]
                } else {
                    self.pps.cr_qp_offset + self.header.cr_qp_offset + self.cu_qp_offset[1]
                };
                chroma_qp_for(
                    qp_y,
                    offset,
                    self.pic.bit_depth_c,
                    self.sps.chroma_format_idc,
                ) + self.qp_bd_c
            };
            // Scaling matrix (8.6.4.2): flat 16 when the lists are off, or for a
            // transform-skipped block larger than 4x4; otherwise the factor of the
            // block's size and matrixId (Table 7-4: 0..2 intra, 3..5 inter).
            let matrix = if transform_skip && n > 4 {
                None
            } else {
                self.scaling.as_ref().map(|f| {
                    let matrix_id = c_idx + if intra_mode.is_some() { 0 } else { 3 };
                    f.for_block(log2, matrix_id)
                })
            };
            let mut scaled = vec![0i32; n * n];
            dequantize_weighted(&levels, &mut scaled, n, qp, bd, matrix);
            let mut samples = vec![0i32; n * n];
            if transform_skip {
                if rotate {
                    rext::rotate_180(&mut scaled, n);
                }
                inverse_transform_skip_bd(&scaled, &mut samples, n, bd);
                if let Some(dir) = flags.rdpcm {
                    rext::rdpcm_accumulate(&mut samples, n, dir);
                }
            } else {
                let dst = intra_mode.is_some() && uses_dst(n, c_idx == 0);
                inverse_transform_bd(&scaled, &mut samples, n, dst, bd);
            }
            samples
        };
        ResidualBlock {
            component: c_idx,
            x,
            y,
            size: n,
            samples,
        }
    }

    /// `sao(rx, ry)` (7.3.8.3): the CTB's SAO parameters, kept with the CTB. A
    /// merge copies the left or upper CTB's parameters (all components). The
    /// merge flags are read only when the neighbour is in the slice and the tile.
    fn sao(&mut self, dec: &mut CabacDecoder, rs: usize, rx: usize, ry: usize) -> Result<()> {
        self.stats.sao_ctus += 1;
        let cols = self.pic.ctb_cols;
        let slice_addr = self.pic.carry.slice_addr;
        let tile_of = &self.pic.tiles.tile_of_rs;
        let same_tile = |a: usize, b: usize| tile_of.get(a) == tile_of.get(b);
        let left_ok = rx > 0 && rs > slice_addr && same_tile(rs, rs.wrapping_sub(1));
        let up_ok =
            ry > 0 && rs >= cols && rs - cols >= slice_addr && same_tile(rs, rs.wrapping_sub(cols));
        let merge_left = left_ok && dec.decode_bin(ctx::SAO_MERGE) != 0;
        let merge_up = !merge_left && up_ok && dec.decode_bin(ctx::SAO_MERGE) != 0;
        if merge_left {
            self.pic.ctb[rs].sao = self.pic.ctb[rs - 1].sao;
            return Ok(());
        }
        if merge_up {
            self.pic.ctb[rs].sao = self.pic.ctb[rs - cols].sao;
            return Ok(());
        }
        let mut params = SaoParams::default();
        let mut chroma_type = 0u8;
        for c_idx in 0..3usize {
            let enabled = if c_idx == 0 {
                self.header.sao_luma
            } else {
                self.header.sao_chroma
            };
            if !enabled {
                continue;
            }
            let type_idx = if c_idx == 2 {
                chroma_type
            } else {
                // TR, cMax 2: "0" is 0, "10" is 1 (band), "11" is 2 (edge).
                let t = if dec.decode_bin(ctx::SAO_TYPE_IDX) == 0 {
                    0
                } else {
                    1 + dec.decode_bypass() as u8
                };
                if c_idx == 1 {
                    chroma_type = t;
                }
                t
            };
            params.type_idx[c_idx] = type_idx;
            if type_idx == 0 {
                continue;
            }
            let bd = self.pic.bit_depth(c_idx);
            // sao_offset_abs: TR with cMax (1 << (Min(bitDepth, 10) - 5)) - 1, all bypass.
            let cmax = (1u32 << (bd.min(10) - 5)) - 1;
            let mut abs = [0i32; 4];
            for o in abs.iter_mut() {
                let mut v = 0;
                while v < cmax && dec.decode_bypass() != 0 {
                    v += 1;
                }
                *o = v as i32;
            }
            // SaoOffsetVal is scaled by 1 << log2OffsetScale (7-72).
            let shift = if c_idx == 0 {
                self.pps.range.log2_sao_offset_scale_luma
            } else {
                self.pps.range.log2_sao_offset_scale_chroma
            };
            if type_idx == 1 {
                let mut vals = [0i32; 4];
                for (val, &a) in vals.iter_mut().zip(abs.iter()) {
                    *val = if a != 0 && dec.decode_bypass() != 0 {
                        -a
                    } else {
                        a
                    };
                }
                params.band_position[c_idx] = dec.decode_bypass_bits(5) as u8;
                params.offset[c_idx] = vals.map(|v| (v << shift) as i16);
            } else {
                if c_idx != 2 {
                    params.eo_class[c_idx] = dec.decode_bypass_bits(2) as u8;
                } else {
                    params.eo_class[2] = params.eo_class[1];
                }
                params.offset[c_idx] =
                    [abs[0], abs[1], -abs[2], -abs[3]].map(|v| (v << shift) as i16);
            }
        }
        self.pic.ctb[rs].sao = params;
        Ok(())
    }

    /// `pcm_sample()` (7.3.8.7): the raw samples after `pcm_flag`, written
    /// straight into the picture, then the arithmetic decoder restarts after them
    /// (9.3.2.5). The samples are shifted up to the picture's bit depth.
    fn pcm_cu(&mut self, dec: &mut CabacDecoder, x0: usize, y0: usize, log2: u32) -> Result<()> {
        let Some((bd_y_m1, bd_c_m1, _, _, _)) = self.sps.pcm else {
            return Err(Error::corrupt(
                "HEVC slice syntax: pcm_flag without PCM parameters",
            ));
        };
        let pcm_bd = [bd_y_m1 + 1, bd_c_m1 + 1, bd_c_m1 + 1];
        let n = 1usize << log2;
        let data = dec.data();
        let mut pos = dec.byte_position_aligned() * 8;
        let (sx, sy) = chroma_shifts(self.sps.chroma_format_idc);
        let mut blocks: Vec<(usize, Vec<u16>)> = Vec::with_capacity(3);
        let planes = if self.sps.chroma_format_idc == 0 {
            1
        } else {
            3
        };
        for (c, &pcm_depth) in pcm_bd.iter().enumerate().take(planes) {
            let (sw, sh) = if c == 0 { (n, n) } else { (n >> sx, n >> sy) };
            let shift = self.pic.bit_depth(c) - pcm_depth;
            let mut samples = Vec::with_capacity(sw * sh);
            for _ in 0..sw * sh {
                let v = read_raw_bits(data, &mut pos, pcm_bd[c])?;
                samples.push((v << shift) as u16);
            }
            blocks.push((c, samples));
        }
        for (c, samples) in blocks {
            let (sw, sh) = if c == 0 { (n, n) } else { (n >> sx, n >> sy) };
            let (x, y) = if c == 0 {
                (x0, y0)
            } else {
                (x0 >> sx, y0 >> sy)
            };
            if self.pic.has_pixels() {
                let origin = self.pic.plane_origin(c);
                let (stride, plane) = self.pic.plane_view_mut(c);
                for j in 0..sh {
                    let start = (y + j - origin) * stride + x;
                    plane[start..start + sw].copy_from_slice(&samples[j * sw..j * sw + sw]);
                }
            }
        }
        self.pic.update_info(x0, y0, n, n, |b| b.pcm = true);
        dec.restart_at_byte(pos.div_ceil(8));
        Ok(())
    }
}

/// Read `count` bits, most significant first, at bit position `pos` of `data`.
fn read_raw_bits(data: &[u8], pos: &mut usize, count: u32) -> Result<u32> {
    let mut v = 0u32;
    for _ in 0..count {
        let byte = *data
            .get(*pos / 8)
            .ok_or_else(|| Error::corrupt("HEVC slice syntax: PCM samples run past the slice"))?;
        v = (v << 1) | u32::from((byte >> (7 - (*pos % 8))) & 1);
        *pos += 1;
    }
    Ok(v)
}

/// A truncated unary value with cMax `cmax`: the first bin in `first_ctx`, the
/// rest bypass. Used for `merge_idx` (9.3.3.2 with Table 9-41).
fn truncated_unary(dec: &mut CabacDecoder, cmax: u32, first_ctx: usize) -> u32 {
    let mut v = 0;
    while v < cmax {
        let bin = if v == 0 {
            dec.decode_bin(first_ctx)
        } else {
            dec.decode_bypass()
        };
        if bin == 0 {
            break;
        }
        v += 1;
    }
    v
}

/// `inter_pred_idc` (9.3.3.8): one bin in context CtDepth then one bin in
/// context 4 for bi-prediction, or one bin in context 4 for 8x4 and 4x8 units.
fn inter_pred_idc(dec: &mut CabacDecoder, width: usize, height: usize, depth: u32) -> InterPredIdc {
    if width + height != 12 && dec.decode_bin(ctx::INTER_PRED_IDC + depth as usize) != 0 {
        return InterPredIdc::PredBi;
    }
    if dec.decode_bin(ctx::INTER_PRED_IDC + 4) != 0 {
        InterPredIdc::PredL1
    } else {
        InterPredIdc::PredL0
    }
}

/// The k-th order Exp-Golomb bypass suffix (9.3.3.3). The value is capped so a
/// corrupt prefix cannot run past 32 bits.
fn decode_exp_golomb(dec: &mut CabacDecoder, mut k: u32) -> Result<u32> {
    let mut value = 0u32;
    while dec.decode_bypass() != 0 {
        if k >= 31 {
            return Err(Error::corrupt(
                "HEVC slice syntax: Exp-Golomb prefix too long",
            ));
        }
        value += 1 << k;
        k += 1;
    }
    Ok(value + dec.decode_bypass_bits(k))
}

/// The luma prediction partitions of a coding unit, as `(x, y, w, h)`.
fn pu_rects(part: PartMode, x: usize, y: usize, n: usize) -> Vec<(usize, usize, usize, usize)> {
    let h = n / 2;
    let q = n / 4;
    match part {
        PartMode::Part2Nx2N => vec![(x, y, n, n)],
        PartMode::Part2NxN => vec![(x, y, n, h), (x, y + h, n, h)],
        PartMode::PartNx2N => vec![(x, y, h, n), (x + h, y, h, n)],
        PartMode::PartNxN => vec![
            (x, y, h, h),
            (x + h, y, h, h),
            (x, y + h, h, h),
            (x + h, y + h, h, h),
        ],
        PartMode::Part2NxnU => vec![(x, y, n, q), (x, y + q, n, n - q)],
        PartMode::Part2NxnD => vec![(x, y, n, n - q), (x, y + n - q, n, q)],
        PartMode::PartnLx2N => vec![(x, y, q, n), (x + q, y, n - q, n)],
        PartMode::PartnRx2N => vec![(x, y, n - q, n), (x + n - q, y, q, n)],
    }
}

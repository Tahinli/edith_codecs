//! Slice header (Rec. ITU-T H.264 clause 7.3.3) and its sub-structures:
//! reference picture list modification (7.3.3.1), prediction weight table
//! (7.3.3.2) and decoded reference picture marking (7.3.3.3).
//!
//! The P and B fields are parsed even though this release decodes only I
//! slices: a header that is skipped rather than parsed is a header whose
//! correctness nobody can check, and the inter decoding process is built on top
//! of exactly these values.

// The reference list loops below index three parallel arrays by list
// number, which is how clause 7.3.3.2 is written.
#![allow(clippy::needless_range_loop)]

use ec_core::bitio::BitReader;
use ec_core::error::{Error, Result};

use crate::nal::{NalUnitType, RbspReader};
use crate::pps::{PicParameterSet, SliceGroupMap};
use crate::sps::SequenceParameterSet;

/// `slice_type` (Table 7-6).
///
/// Values 5..=9 mean the same five types with the added constraint that all
/// slices of the picture have that type; [`SliceType::from_code`] keeps that
/// distinction in `all_slices_same_type` rather than throwing it away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SliceType {
    /// Predictive.
    P,
    /// Bi-predictive.
    B,
    /// Intra.
    I,
    /// Switching P.
    Sp,
    /// Switching I.
    Si,
}

impl SliceType {
    /// Map `slice_type % 5` onto the enum; `> 9` is corrupt.
    pub fn from_code(slice_type: u32) -> Result<(SliceType, bool)> {
        let all_slices_same_type = slice_type >= 5;
        let ty = match slice_type % 5 {
            0 => SliceType::P,
            1 => SliceType::B,
            2 => SliceType::I,
            3 => SliceType::Sp,
            4 => SliceType::Si,
            _ => {
                return Err(Error::corrupt(format!(
                    "H.264 slice header: slice_type = {slice_type}"
                )));
            }
        };
        if slice_type > 9 {
            return Err(Error::corrupt(format!(
                "H.264 slice header: slice_type = {slice_type} > 9"
            )));
        }
        Ok((ty, all_slices_same_type))
    }

    /// True for I and SI: no reference picture lists, no motion data.
    pub fn is_intra(self) -> bool {
        matches!(self, SliceType::I | SliceType::Si)
    }
}

/// One entry of `ref_pic_list_modification()` (clause 7.3.3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefPicListModification {
    /// `modification_of_pic_nums_idc`: 0/1 subtract/add a short-term picture
    /// number difference, 2 names a long-term picture, 3 ends the list.
    pub modification_of_pic_nums_idc: u32,
    /// `abs_diff_pic_num_minus1` (idc 0 or 1) or `long_term_pic_num` (idc 2).
    pub value: u32,
}

/// `pred_weight_table()` (clause 7.3.3.2).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PredWeightTable {
    /// `luma_log2_weight_denom`.
    pub luma_log2_weight_denom: u32,
    /// `chroma_log2_weight_denom`, absent for monochrome.
    pub chroma_log2_weight_denom: u32,
    /// `(luma_weight_lX[i], luma_offset_lX[i])` per list, defaulted per
    /// clause 7.4.3.2 where the flag is 0.
    pub luma_weights: [Vec<(i32, i32)>; 2],
    /// `(chroma_weight_lX[i][j], chroma_offset_lX[i][j])`, j = Cb, Cr.
    pub chroma_weights: [Vec<[(i32, i32); 2]>; 2],
}

/// `dec_ref_pic_marking()` (clause 7.3.3.3).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DecRefPicMarking {
    /// `no_output_of_prior_pics_flag` (IDR pictures only).
    pub no_output_of_prior_pics_flag: bool,
    /// `long_term_reference_flag` (IDR pictures only).
    pub long_term_reference_flag: bool,
    /// `adaptive_ref_pic_marking_mode_flag` (non-IDR pictures).
    pub adaptive_ref_pic_marking_mode_flag: bool,
    /// The memory management control operations, in bitstream order.
    pub operations: Vec<MemoryManagementControlOperation>,
}

/// One `memory_management_control_operation` and its arguments (Table 7-9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryManagementControlOperation {
    /// The operation itself, 1..=6.
    pub operation: u32,
    /// `difference_of_pic_nums_minus1` (ops 1, 3) or `long_term_pic_num` (op 2)
    /// or `max_long_term_frame_idx_plus1` (op 4).
    pub value: u32,
    /// `long_term_frame_idx` (ops 3, 6).
    pub long_term_frame_idx: u32,
}

/// `slice_header()` (clause 7.3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceHeader {
    /// `first_mb_in_slice`.
    pub first_mb_in_slice: u32,
    /// `slice_type % 5`.
    pub slice_type: SliceType,
    /// True when `slice_type` was coded as 5..=9.
    pub all_slices_same_type: bool,
    /// `pic_parameter_set_id`.
    pub pic_parameter_set_id: u32,
    /// `colour_plane_id`, only for `separate_colour_plane_flag`.
    pub colour_plane_id: u8,
    /// `frame_num`.
    pub frame_num: u32,
    /// `field_pic_flag`.
    pub field_pic_flag: bool,
    /// `bottom_field_flag`.
    pub bottom_field_flag: bool,
    /// `idr_pic_id`, IDR pictures only.
    pub idr_pic_id: u32,
    /// `pic_order_cnt_lsb` (POC type 0).
    pub pic_order_cnt_lsb: u32,
    /// `delta_pic_order_cnt_bottom` (POC type 0).
    pub delta_pic_order_cnt_bottom: i32,
    /// `delta_pic_order_cnt[0..2]` (POC type 1).
    pub delta_pic_order_cnt: [i32; 2],
    /// `redundant_pic_cnt`.
    pub redundant_pic_cnt: u32,
    /// `direct_spatial_mv_pred_flag` (B slices).
    pub direct_spatial_mv_pred_flag: bool,
    /// `num_ref_idx_active_override_flag`.
    pub num_ref_idx_active_override_flag: bool,
    /// `num_ref_idx_l0_active_minus1`, after the PPS default is applied.
    pub num_ref_idx_l0_active_minus1: u32,
    /// `num_ref_idx_l1_active_minus1`, after the PPS default is applied.
    pub num_ref_idx_l1_active_minus1: u32,
    /// `ref_pic_list_modification_flag_l0` / `_l1` and their entries.
    pub ref_pic_list_modification: [Vec<RefPicListModification>; 2],
    /// `pred_weight_table()`, when the PPS asks for one.
    pub pred_weight_table: Option<PredWeightTable>,
    /// `dec_ref_pic_marking()`, present when `nal_ref_idc != 0`.
    pub dec_ref_pic_marking: Option<DecRefPicMarking>,
    /// `cabac_init_idc`, CABAC non-intra slices only.
    pub cabac_init_idc: u32,
    /// `slice_qp_delta`.
    pub slice_qp_delta: i32,
    /// `sp_for_switch_flag` (SP slices).
    pub sp_for_switch_flag: bool,
    /// `slice_qs_delta` (SP and SI slices).
    pub slice_qs_delta: i32,
    /// `disable_deblocking_filter_idc`: 0 filter everything, 1 filter nothing,
    /// 2 filter but never across a slice boundary.
    pub disable_deblocking_filter_idc: u32,
    /// `slice_alpha_c0_offset_div2`.
    pub slice_alpha_c0_offset_div2: i32,
    /// `slice_beta_offset_div2`.
    pub slice_beta_offset_div2: i32,
    /// `slice_group_change_cycle`.
    pub slice_group_change_cycle: u32,
    /// Bit position just past the header: where `slice_data()` starts.
    pub header_bits: u64,
}

impl SliceHeader {
    /// `SliceQPY` (clause 7.4.3): the quantisation parameter the first
    /// macroblock of the slice starts from.
    pub fn slice_qp_y(&self, pps: &PicParameterSet) -> i32 {
        26 + pps.pic_init_qp_minus26 + self.slice_qp_delta
    }

    /// `MbaffFrameFlag` (clause 7.4.3).
    pub fn mbaff_frame_flag(&self, sps: &SequenceParameterSet) -> bool {
        sps.mb_adaptive_frame_field_flag && !self.field_pic_flag
    }

    /// `PicHeightInMbs` (clause 7.4.3): a field picture is half a frame tall.
    pub fn pic_height_in_mbs(&self, sps: &SequenceParameterSet) -> u32 {
        sps.frame_height_in_mbs() / (1 + u32::from(self.field_pic_flag))
    }

    /// Parse `slice_header()` from the RBSP of a VCL NAL unit.
    ///
    /// `nal_unit_type` decides `IdrPicFlag`, and `nal_ref_idc` decides whether a
    /// `dec_ref_pic_marking()` is present, so both travel in from the NAL header.
    pub fn parse(
        rr: &mut RbspReader<'_>,
        nal_unit_type: NalUnitType,
        nal_ref_idc: u8,
        sps: &SequenceParameterSet,
        pps: &PicParameterSet,
    ) -> Result<SliceHeader> {
        let idr_pic_flag = nal_unit_type == NalUnitType::IdrSlice;
        let r = rr.bits();
        let first_mb_in_slice = r.read_ue()?;
        let (slice_type, all_slices_same_type) = SliceType::from_code(r.read_ue()?)?;
        let pic_parameter_set_id = r.read_ue()?;

        let mut h = SliceHeader {
            first_mb_in_slice,
            slice_type,
            all_slices_same_type,
            pic_parameter_set_id,
            colour_plane_id: 0,
            frame_num: 0,
            field_pic_flag: false,
            bottom_field_flag: false,
            idr_pic_id: 0,
            pic_order_cnt_lsb: 0,
            delta_pic_order_cnt_bottom: 0,
            delta_pic_order_cnt: [0; 2],
            redundant_pic_cnt: 0,
            direct_spatial_mv_pred_flag: false,
            num_ref_idx_active_override_flag: false,
            num_ref_idx_l0_active_minus1: pps.num_ref_idx_l0_default_active_minus1,
            num_ref_idx_l1_active_minus1: pps.num_ref_idx_l1_default_active_minus1,
            ref_pic_list_modification: [Vec::new(), Vec::new()],
            pred_weight_table: None,
            dec_ref_pic_marking: None,
            cabac_init_idc: 0,
            slice_qp_delta: 0,
            sp_for_switch_flag: false,
            slice_qs_delta: 0,
            disable_deblocking_filter_idc: 0,
            slice_alpha_c0_offset_div2: 0,
            slice_beta_offset_div2: 0,
            slice_group_change_cycle: 0,
            header_bits: 0,
        };

        if sps.separate_colour_plane_flag {
            h.colour_plane_id = r.read_bits(2)? as u8;
        }
        h.frame_num = r.read_bits(sps.log2_max_frame_num_minus4 + 4)?;
        if !sps.frame_mbs_only_flag {
            h.field_pic_flag = r.read_bit()?;
            if h.field_pic_flag {
                h.bottom_field_flag = r.read_bit()?;
            }
        }
        if idr_pic_flag {
            h.idr_pic_id = r.read_ue()?;
        }
        if sps.pic_order_cnt_type == 0 {
            h.pic_order_cnt_lsb = r.read_bits(sps.log2_max_pic_order_cnt_lsb_minus4 + 4)?;
            if pps.bottom_field_pic_order_in_frame_present_flag && !h.field_pic_flag {
                h.delta_pic_order_cnt_bottom = r.read_se()?;
            }
        } else if sps.pic_order_cnt_type == 1 && !sps.delta_pic_order_always_zero_flag {
            h.delta_pic_order_cnt[0] = r.read_se()?;
            if pps.bottom_field_pic_order_in_frame_present_flag && !h.field_pic_flag {
                h.delta_pic_order_cnt[1] = r.read_se()?;
            }
        }
        if pps.redundant_pic_cnt_present_flag {
            h.redundant_pic_cnt = r.read_ue()?;
        }
        if h.slice_type == SliceType::B {
            h.direct_spatial_mv_pred_flag = r.read_bit()?;
        }
        if matches!(h.slice_type, SliceType::P | SliceType::Sp | SliceType::B) {
            h.num_ref_idx_active_override_flag = r.read_bit()?;
            if h.num_ref_idx_active_override_flag {
                h.num_ref_idx_l0_active_minus1 = r.read_ue()?;
                if h.slice_type == SliceType::B {
                    h.num_ref_idx_l1_active_minus1 = r.read_ue()?;
                }
            }
        }

        // ref_pic_list_modification(), clause 7.3.3.1.
        if !matches!(h.slice_type, SliceType::I | SliceType::Si) {
            h.ref_pic_list_modification[0] = parse_ref_pic_list_modification(r)?;
        }
        if h.slice_type == SliceType::B {
            h.ref_pic_list_modification[1] = parse_ref_pic_list_modification(r)?;
        }

        // pred_weight_table(), clause 7.3.3.2.
        let weighted = (pps.weighted_pred_flag
            && matches!(h.slice_type, SliceType::P | SliceType::Sp))
            || (pps.weighted_bipred_idc == 1 && h.slice_type == SliceType::B);
        if weighted {
            h.pred_weight_table = Some(parse_pred_weight_table(
                r,
                sps.chroma_array_type(),
                h.num_ref_idx_l0_active_minus1,
                h.num_ref_idx_l1_active_minus1,
                h.slice_type == SliceType::B,
            )?);
        }

        if nal_ref_idc != 0 {
            h.dec_ref_pic_marking = Some(parse_dec_ref_pic_marking(r, idr_pic_flag)?);
        }
        if pps.entropy_coding_mode_flag && !h.slice_type.is_intra() {
            h.cabac_init_idc = r.read_ue()?;
        }
        h.slice_qp_delta = r.read_se()?;
        if matches!(h.slice_type, SliceType::Sp | SliceType::Si) {
            if h.slice_type == SliceType::Sp {
                h.sp_for_switch_flag = r.read_bit()?;
            }
            h.slice_qs_delta = r.read_se()?;
        }
        if pps.deblocking_filter_control_present_flag {
            h.disable_deblocking_filter_idc = r.read_ue()?;
            if h.disable_deblocking_filter_idc > 2 {
                return Err(Error::corrupt(format!(
                    "H.264 slice header: disable_deblocking_filter_idc = {}",
                    h.disable_deblocking_filter_idc
                )));
            }
            if h.disable_deblocking_filter_idc != 1 {
                h.slice_alpha_c0_offset_div2 = r.read_se()?;
                h.slice_beta_offset_div2 = r.read_se()?;
            }
        }
        if pps.num_slice_groups_minus1 > 0
            && let Some(SliceGroupMap::Changing {
                change_rate_minus1, ..
            }) = pps.slice_group_map
        {
            // u(v): Ceil(Log2(PicSizeInMapUnits / SliceGroupChangeRate + 1)).
            let slice_group_change_rate = change_rate_minus1 + 1;
            let range = sps.pic_size_in_map_units() / slice_group_change_rate + 1;
            let bits = 32 - (range - 1).leading_zeros();
            h.slice_group_change_cycle = r.read_bits(bits)?;
        }
        h.header_bits = r.bit_position();
        Ok(h)
    }
}

fn parse_ref_pic_list_modification(r: &mut BitReader<'_>) -> Result<Vec<RefPicListModification>> {
    let mut entries = Vec::new();
    if r.read_bit()? {
        loop {
            let modification_of_pic_nums_idc = r.read_ue()?;
            if modification_of_pic_nums_idc == 3 {
                break;
            }
            if modification_of_pic_nums_idc > 3 {
                return Err(Error::corrupt(format!(
                    "H.264 ref_pic_list_modification: idc = {modification_of_pic_nums_idc}"
                )));
            }
            entries.push(RefPicListModification {
                modification_of_pic_nums_idc,
                value: r.read_ue()?,
            });
            // A list longer than the maximum reference count is corrupt; the
            // loop is otherwise unbounded on hostile input.
            if entries.len() > 64 {
                return Err(Error::corrupt(
                    "H.264 ref_pic_list_modification: more than 64 entries",
                ));
            }
        }
    }
    Ok(entries)
}

fn parse_pred_weight_table(
    r: &mut BitReader<'_>,
    chroma_array_type: u32,
    num_ref_idx_l0_active_minus1: u32,
    num_ref_idx_l1_active_minus1: u32,
    bi: bool,
) -> Result<PredWeightTable> {
    let mut t = PredWeightTable {
        luma_log2_weight_denom: r.read_ue()?,
        ..PredWeightTable::default()
    };
    if chroma_array_type != 0 {
        t.chroma_log2_weight_denom = r.read_ue()?;
    }
    let counts = [num_ref_idx_l0_active_minus1, num_ref_idx_l1_active_minus1];
    for list in 0..if bi { 2 } else { 1 } {
        if counts[list] >= 32 {
            return Err(Error::corrupt(format!(
                "H.264 pred_weight_table: num_ref_idx_l{list}_active_minus1 = {}",
                counts[list]
            )));
        }
        for _ in 0..=counts[list] {
            // Absent weights default to (1 << denom, 0), clause 7.4.3.2.
            let mut luma = (1 << t.luma_log2_weight_denom, 0);
            if r.read_bit()? {
                luma = (r.read_se()?, r.read_se()?);
            }
            t.luma_weights[list].push(luma);
            if chroma_array_type != 0 {
                let default = (1 << t.chroma_log2_weight_denom, 0);
                let mut chroma = [default; 2];
                if r.read_bit()? {
                    for slot in &mut chroma {
                        *slot = (r.read_se()?, r.read_se()?);
                    }
                }
                t.chroma_weights[list].push(chroma);
            }
        }
    }
    Ok(t)
}

fn parse_dec_ref_pic_marking(
    r: &mut BitReader<'_>,
    idr_pic_flag: bool,
) -> Result<DecRefPicMarking> {
    let mut m = DecRefPicMarking::default();
    if idr_pic_flag {
        m.no_output_of_prior_pics_flag = r.read_bit()?;
        m.long_term_reference_flag = r.read_bit()?;
        return Ok(m);
    }
    m.adaptive_ref_pic_marking_mode_flag = r.read_bit()?;
    if m.adaptive_ref_pic_marking_mode_flag {
        loop {
            let operation = r.read_ue()?;
            if operation == 0 {
                break;
            }
            if operation > 6 {
                return Err(Error::corrupt(format!(
                    "H.264 dec_ref_pic_marking: memory_management_control_operation = {operation}"
                )));
            }
            let mut op = MemoryManagementControlOperation {
                operation,
                value: 0,
                long_term_frame_idx: 0,
            };
            if matches!(operation, 1 | 3) {
                op.value = r.read_ue()?; // difference_of_pic_nums_minus1
            } else if operation == 2 {
                op.value = r.read_ue()?; // long_term_pic_num
            } else if operation == 4 {
                op.value = r.read_ue()?; // max_long_term_frame_idx_plus1
            }
            if matches!(operation, 3 | 6) {
                op.long_term_frame_idx = r.read_ue()?;
            }
            m.operations.push(op);
            if m.operations.len() > 64 {
                return Err(Error::corrupt(
                    "H.264 dec_ref_pic_marking: more than 64 operations",
                ));
            }
        }
    }
    Ok(m)
}

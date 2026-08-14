//! Picture parameter set (Rec. ITU-T H.264 clause 7.3.2.2).

use ec_core::bitio::BitReader;
use ec_core::error::{Error, Result};

use crate::nal::RbspReader;
use crate::sps::{ScalingList, scaling_list};

/// `slice_group_map_type` payload (clause 7.3.2.2), one variant per map type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SliceGroupMap {
    /// Type 0, interleaved: `run_length_minus1[i]`.
    Interleaved {
        /// `run_length_minus1[i]`, one per slice group.
        run_length_minus1: Vec<u32>,
    },
    /// Type 1, dispersed: no further syntax elements.
    Dispersed,
    /// Type 2, foreground with left-over: `top_left[i]`, `bottom_right[i]`.
    Foreground {
        /// `top_left[i]`.
        top_left: Vec<u32>,
        /// `bottom_right[i]`.
        bottom_right: Vec<u32>,
    },
    /// Types 3, 4 and 5: box-out, raster scan and wipe.
    Changing {
        /// The map type itself, 3, 4 or 5.
        map_type: u32,
        /// `slice_group_change_direction_flag`.
        change_direction_flag: bool,
        /// `slice_group_change_rate_minus1`.
        change_rate_minus1: u32,
    },
    /// Type 6, explicit: `slice_group_id[i]` for every map unit.
    Explicit {
        /// `slice_group_id[i]`.
        slice_group_id: Vec<u32>,
    },
}

/// `pic_parameter_set_rbsp()` (clause 7.3.2.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PicParameterSet {
    /// `pic_parameter_set_id`, 0..=255.
    pub pic_parameter_set_id: u32,
    /// `seq_parameter_set_id` of the SPS this PPS refers to.
    pub seq_parameter_set_id: u32,
    /// `entropy_coding_mode_flag`: 0 = CAVLC, 1 = CABAC.
    pub entropy_coding_mode_flag: bool,
    /// `bottom_field_pic_order_in_frame_present_flag` (was `pic_order_present_flag`).
    pub bottom_field_pic_order_in_frame_present_flag: bool,
    /// `num_slice_groups_minus1`.
    pub num_slice_groups_minus1: u32,
    /// The slice group map, present only when `num_slice_groups_minus1 > 0`.
    pub slice_group_map: Option<SliceGroupMap>,
    /// `num_ref_idx_l0_default_active_minus1`.
    pub num_ref_idx_l0_default_active_minus1: u32,
    /// `num_ref_idx_l1_default_active_minus1`.
    pub num_ref_idx_l1_default_active_minus1: u32,
    /// `weighted_pred_flag`, for P and SP slices.
    pub weighted_pred_flag: bool,
    /// `weighted_bipred_idc`, 0..=2, for B slices.
    pub weighted_bipred_idc: u8,
    /// `pic_init_qp_minus26`.
    pub pic_init_qp_minus26: i32,
    /// `pic_init_qs_minus26`.
    pub pic_init_qs_minus26: i32,
    /// `chroma_qp_index_offset`, applied to Cb (and to Cr unless the second
    /// offset below is present).
    pub chroma_qp_index_offset: i32,
    /// `deblocking_filter_control_present_flag`.
    pub deblocking_filter_control_present_flag: bool,
    /// `constrained_intra_pred_flag`.
    pub constrained_intra_pred_flag: bool,
    /// `redundant_pic_cnt_present_flag`.
    pub redundant_pic_cnt_present_flag: bool,
    /// `transform_8x8_mode_flag`; absent means 0.
    pub transform_8x8_mode_flag: bool,
    /// `pic_scaling_matrix_present_flag`.
    pub pic_scaling_matrix_present_flag: bool,
    /// `ScalingList4x4[0..6]` from this PPS, where present.
    pub scaling_list_4x4: [Option<ScalingList>; 6],
    /// `ScalingList8x8[0..6]` from this PPS, where present.
    pub scaling_list_8x8: [Option<ScalingList>; 6],
    /// `second_chroma_qp_index_offset`; defaults to `chroma_qp_index_offset`
    /// when the trailing extension is absent (clause 7.4.2.2).
    pub second_chroma_qp_index_offset: i32,
}

/// The two identifiers a PPS starts with, read without parsing the rest.
///
/// A PPS says which SPS it belongs to, but the SPS's `chroma_format_idc` is
/// needed to parse the PPS's own scaling list loop — so a decoder peeks the
/// ids, looks the SPS up, then calls [`PicParameterSet::parse`].
pub fn peek_ids(rbsp: &[u8]) -> Result<(u32, u32)> {
    let mut r = BitReader::new(rbsp);
    let pic_parameter_set_id = r.read_ue()?;
    let seq_parameter_set_id = r.read_ue()?;
    Ok((pic_parameter_set_id, seq_parameter_set_id))
}

impl PicParameterSet {
    /// Parse `pic_parameter_set_rbsp()`.
    ///
    /// `chroma_format_idc` comes from the referenced SPS; it selects the length
    /// of the picture scaling list loop only, so 1 (4:2:0) is a safe value when
    /// the SPS has not been seen yet and the stream is known to be 4:2:0.
    pub fn parse(rbsp: &[u8], chroma_format_idc: u32) -> Result<PicParameterSet> {
        let mut rr = RbspReader::new(rbsp);
        let pic_parameter_set_id;
        let seq_parameter_set_id;
        let entropy_coding_mode_flag;
        let bottom_field_pic_order_in_frame_present_flag;
        let num_slice_groups_minus1;
        let mut slice_group_map = None;
        {
            let r = rr.bits();
            pic_parameter_set_id = r.read_ue()?;
            seq_parameter_set_id = r.read_ue()?;
            if pic_parameter_set_id > 255 || seq_parameter_set_id > 31 {
                return Err(Error::corrupt(format!(
                    "H.264 PPS: ids out of range ({pic_parameter_set_id}, {seq_parameter_set_id})"
                )));
            }
            entropy_coding_mode_flag = r.read_bit()?;
            bottom_field_pic_order_in_frame_present_flag = r.read_bit()?;
            num_slice_groups_minus1 = r.read_ue()?;
            if num_slice_groups_minus1 > 7 {
                return Err(Error::corrupt(format!(
                    "H.264 PPS: num_slice_groups_minus1 = {num_slice_groups_minus1} > 7"
                )));
            }
            if num_slice_groups_minus1 > 0 {
                let slice_group_map_type = r.read_ue()?;
                slice_group_map = Some(match slice_group_map_type {
                    0 => {
                        let mut run_length_minus1 = Vec::new();
                        for _ in 0..=num_slice_groups_minus1 {
                            run_length_minus1.push(r.read_ue()?);
                        }
                        SliceGroupMap::Interleaved { run_length_minus1 }
                    }
                    1 => SliceGroupMap::Dispersed,
                    2 => {
                        let mut top_left = Vec::new();
                        let mut bottom_right = Vec::new();
                        for _ in 0..num_slice_groups_minus1 {
                            top_left.push(r.read_ue()?);
                            bottom_right.push(r.read_ue()?);
                        }
                        SliceGroupMap::Foreground {
                            top_left,
                            bottom_right,
                        }
                    }
                    3..=5 => SliceGroupMap::Changing {
                        map_type: slice_group_map_type,
                        change_direction_flag: r.read_bit()?,
                        change_rate_minus1: r.read_ue()?,
                    },
                    6 => {
                        let pic_size_in_map_units_minus1 = r.read_ue()?;
                        if pic_size_in_map_units_minus1 >= 1024 * 1024 {
                            return Err(Error::corrupt(
                                "H.264 PPS: pic_size_in_map_units_minus1 beyond any level",
                            ));
                        }
                        // Ceil(Log2(num_slice_groups_minus1 + 1)) bits each.
                        let bits = 32 - (num_slice_groups_minus1).leading_zeros();
                        let mut slice_group_id = Vec::new();
                        for _ in 0..=pic_size_in_map_units_minus1 {
                            slice_group_id.push(r.read_bits(bits)?);
                        }
                        SliceGroupMap::Explicit { slice_group_id }
                    }
                    other => {
                        return Err(Error::corrupt(format!(
                            "H.264 PPS: slice_group_map_type = {other}"
                        )));
                    }
                });
            }
        }

        let r = rr.bits();
        let mut pps = PicParameterSet {
            pic_parameter_set_id,
            seq_parameter_set_id,
            entropy_coding_mode_flag,
            bottom_field_pic_order_in_frame_present_flag,
            num_slice_groups_minus1,
            slice_group_map,
            num_ref_idx_l0_default_active_minus1: r.read_ue()?,
            num_ref_idx_l1_default_active_minus1: r.read_ue()?,
            weighted_pred_flag: r.read_bit()?,
            weighted_bipred_idc: r.read_bits(2)? as u8,
            pic_init_qp_minus26: r.read_se()?,
            pic_init_qs_minus26: r.read_se()?,
            chroma_qp_index_offset: r.read_se()?,
            deblocking_filter_control_present_flag: r.read_bit()?,
            constrained_intra_pred_flag: r.read_bit()?,
            redundant_pic_cnt_present_flag: r.read_bit()?,
            transform_8x8_mode_flag: false,
            pic_scaling_matrix_present_flag: false,
            scaling_list_4x4: [const { None }; 6],
            scaling_list_8x8: [const { None }; 6],
            second_chroma_qp_index_offset: 0,
        };
        pps.second_chroma_qp_index_offset = pps.chroma_qp_index_offset;

        // The trailing extension (transform 8x8, picture scaling matrices) is
        // optional: its presence is exactly `more_rbsp_data()`.
        if rr.more_rbsp_data() {
            let r = rr.bits();
            pps.transform_8x8_mode_flag = r.read_bit()?;
            pps.pic_scaling_matrix_present_flag = r.read_bit()?;
            if pps.pic_scaling_matrix_present_flag {
                let extra = if chroma_format_idc != 3 { 2 } else { 6 };
                let count = 6 + extra * usize::from(pps.transform_8x8_mode_flag);
                for i in 0..count {
                    if r.read_bit()? {
                        if i < 6 {
                            pps.scaling_list_4x4[i] = Some(scaling_list(r, 16)?);
                        } else {
                            pps.scaling_list_8x8[i - 6] = Some(scaling_list(r, 64)?);
                        }
                    }
                }
            }
            pps.second_chroma_qp_index_offset = r.read_se()?;
        }
        Ok(pps)
    }
}

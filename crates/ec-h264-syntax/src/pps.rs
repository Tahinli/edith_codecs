//! Picture parameter set (spec 7.3.2.2, 7.4.2.2).

use ec_core::BitReader;
use ec_core::error::{Error, Result};

use crate::more_rbsp_data;
use crate::sps::{ScalingLists, Sps, parse_scaling_matrix};

/// Slice-group (FMO) map data, parsed but not interpreted here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SliceGroupMap {
    /// Type 0: `run_length_minus1` per group.
    Interleaved(Vec<u32>),
    /// Type 1: dispersed, no extra data.
    Dispersed,
    /// Type 2: `(top_left, bottom_right)` per group but the last.
    Foreground(Vec<(u32, u32)>),
    /// Types 3..=5: `(change_direction_flag, change_rate_minus1)`.
    Changing {
        /// `slice_group_map_type` (3, 4 or 5).
        map_type: u8,
        /// `slice_group_change_direction_flag`.
        change_direction: bool,
        /// `slice_group_change_rate_minus1`.
        change_rate_minus1: u32,
    },
    /// Type 6: explicit per-map-unit assignment.
    Explicit(Vec<u32>),
}

/// Picture parameter set with decode-ready fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pps {
    /// `pic_parameter_set_id` (0..=255).
    pub id: u32,
    /// `seq_parameter_set_id` of the SPS this PPS refers to.
    pub sps_id: u8,
    /// `entropy_coding_mode_flag`: false = CAVLC, true = CABAC.
    pub entropy_coding_mode: bool,
    /// `bottom_field_pic_order_in_frame_present_flag`.
    pub bottom_field_pic_order_in_frame_present: bool,
    /// `num_slice_groups_minus1` + 1.
    pub num_slice_groups: u32,
    /// FMO map when `num_slice_groups > 1`.
    pub slice_group_map: Option<SliceGroupMap>,
    /// `num_ref_idx_l0_default_active_minus1` + 1.
    pub num_ref_idx_l0_default_active: u32,
    /// `num_ref_idx_l1_default_active_minus1` + 1.
    pub num_ref_idx_l1_default_active: u32,
    /// `weighted_pred_flag`.
    pub weighted_pred: bool,
    /// `weighted_bipred_idc` (0..=2).
    pub weighted_bipred_idc: u8,
    /// `pic_init_qp_minus26` + 26.
    pub pic_init_qp: i32,
    /// `pic_init_qs_minus26` + 26.
    pub pic_init_qs: i32,
    /// `chroma_qp_index_offset` (-12..=12), for Cb.
    pub chroma_qp_index_offset: i32,
    /// `deblocking_filter_control_present_flag`.
    pub deblocking_filter_control_present: bool,
    /// `constrained_intra_pred_flag`.
    pub constrained_intra_pred: bool,
    /// `redundant_pic_cnt_present_flag`.
    pub redundant_pic_cnt_present: bool,
    /// `transform_8x8_mode_flag` (High-profile tail; false when absent).
    pub transform_8x8_mode: bool,
    /// PPS scaling lists when `pic_scaling_matrix_present_flag`, resolved
    /// against the SPS lists per fall-back rule B.
    pub scaling_lists: Option<ScalingLists>,
    /// `second_chroma_qp_index_offset` for Cr (defaults to the Cb offset).
    pub second_chroma_qp_index_offset: i32,
}

impl Pps {
    /// Parse a PPS RBSP (header byte consumed, emulation stripped).
    ///
    /// `sps_lookup` resolves the referenced SPS — needed for the High-profile
    /// tail (scaling-list fall-back and 4:4:4 list count).
    pub fn parse<'s>(rbsp: &[u8], sps_lookup: impl Fn(u8) -> Option<&'s Sps>) -> Result<Pps> {
        let mut reader = BitReader::new(rbsp);
        let r = &mut reader;
        let id = r.read_ue()?;
        if id > 255 {
            return Err(Error::corrupt("pic_parameter_set_id > 255"));
        }
        let sps_id = r.read_ue()?;
        if sps_id > 31 {
            return Err(Error::corrupt("seq_parameter_set_id > 31"));
        }
        let sps_id = sps_id as u8;
        let entropy_coding_mode = r.read_bit()?;
        let bottom_field_pic_order_in_frame_present = r.read_bit()?;
        let num_slice_groups = r.read_ue()?.checked_add(1).ok_or(Error::NeedMore)?;
        if num_slice_groups > 8 {
            return Err(Error::corrupt("num_slice_groups > 8"));
        }
        let slice_group_map = if num_slice_groups > 1 {
            Some(Self::parse_slice_groups(r, num_slice_groups)?)
        } else {
            None
        };
        let num_ref_idx_l0_default_active = r.read_ue()? + 1;
        let num_ref_idx_l1_default_active = r.read_ue()? + 1;
        if num_ref_idx_l0_default_active > 32 || num_ref_idx_l1_default_active > 32 {
            return Err(Error::corrupt("num_ref_idx_default_active > 32"));
        }
        let weighted_pred = r.read_bit()?;
        let weighted_bipred_idc = r.read_bits(2)? as u8;
        let pic_init_qp = 26 + r.read_se()?;
        let pic_init_qs = 26 + r.read_se()?;
        let chroma_qp_index_offset = r.read_se()?;
        if !(-12..=12).contains(&chroma_qp_index_offset) {
            return Err(Error::corrupt("chroma_qp_index_offset outside [-12, 12]"));
        }
        let deblocking_filter_control_present = r.read_bit()?;
        let constrained_intra_pred = r.read_bit()?;
        let redundant_pic_cnt_present = r.read_bit()?;

        let mut transform_8x8_mode = false;
        let mut scaling_lists = None;
        let mut second_chroma_qp_index_offset = chroma_qp_index_offset;
        if more_rbsp_data(r, rbsp) {
            transform_8x8_mode = r.read_bit()?;
            if r.read_bit()? {
                // pic_scaling_matrix_present_flag
                let sps = sps_lookup(sps_id);
                let chroma_444 = sps.map(|s| s.chroma_format_idc == 3).unwrap_or(false);
                let count = 6 + if chroma_444 { 6 } else { 2 } * usize::from(transform_8x8_mode);
                scaling_lists = Some(parse_scaling_matrix(
                    r,
                    count,
                    sps.and_then(|s| s.scaling_lists.as_ref()),
                )?);
            }
            second_chroma_qp_index_offset = r.read_se()?;
            if !(-12..=12).contains(&second_chroma_qp_index_offset) {
                return Err(Error::corrupt(
                    "second_chroma_qp_index_offset outside [-12, 12]",
                ));
            }
        }

        Ok(Pps {
            id,
            sps_id,
            entropy_coding_mode,
            bottom_field_pic_order_in_frame_present,
            num_slice_groups,
            slice_group_map,
            num_ref_idx_l0_default_active,
            num_ref_idx_l1_default_active,
            weighted_pred,
            weighted_bipred_idc,
            pic_init_qp,
            pic_init_qs,
            chroma_qp_index_offset,
            deblocking_filter_control_present,
            constrained_intra_pred,
            redundant_pic_cnt_present,
            transform_8x8_mode,
            scaling_lists,
            second_chroma_qp_index_offset,
        })
    }

    fn parse_slice_groups(r: &mut BitReader<'_>, groups: u32) -> Result<SliceGroupMap> {
        let map_type = r.read_ue()?;
        Ok(match map_type {
            0 => {
                let mut runs = Vec::with_capacity(groups as usize);
                for _ in 0..groups {
                    runs.push(r.read_ue()?);
                }
                SliceGroupMap::Interleaved(runs)
            }
            1 => SliceGroupMap::Dispersed,
            2 => {
                let mut rects = Vec::with_capacity(groups as usize - 1);
                for _ in 0..groups - 1 {
                    rects.push((r.read_ue()?, r.read_ue()?));
                }
                SliceGroupMap::Foreground(rects)
            }
            3..=5 => SliceGroupMap::Changing {
                map_type: map_type as u8,
                change_direction: r.read_bit()?,
                change_rate_minus1: r.read_ue()?,
            },
            6 => {
                let count = r.read_ue()?.checked_add(1).ok_or(Error::NeedMore)?;
                if count > 1 << 20 {
                    return Err(Error::corrupt("slice group map unit count implausible"));
                }
                let bits = 32 - (groups - 1).leading_zeros().min(31);
                let bits = if groups > 1 { bits.max(1) } else { 1 };
                let mut ids = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    ids.push(r.read_bits(bits)?);
                }
                SliceGroupMap::Explicit(ids)
            }
            _ => return Err(Error::corrupt("slice_group_map_type > 6")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_core::BitWriter;

    fn write_baseline_pps(w: &mut BitWriter) {
        w.write_ue(0); // pps id
        w.write_ue(0); // sps id
        w.write_bit(false); // CAVLC
        w.write_bit(false); // bottom_field_pic_order
        w.write_ue(0); // one slice group
        w.write_ue(0); // l0 default
        w.write_ue(0); // l1 default
        w.write_bit(false); // weighted_pred
        w.write_bits(0, 2); // weighted_bipred_idc
        w.write_se(0); // pic_init_qp = 26
        w.write_se(0); // pic_init_qs
        w.write_se(2); // chroma_qp_index_offset
        w.write_bit(true); // deblocking control present
        w.write_bit(false); // constrained intra
        w.write_bit(false); // redundant pic cnt
    }

    #[test]
    fn baseline_pps_without_tail() {
        let mut w = BitWriter::new();
        write_baseline_pps(&mut w);
        w.write_bit(true); // rbsp_stop_one_bit
        w.align_to_byte();
        let pps = Pps::parse(w.as_bytes(), |_| None).unwrap();
        assert_eq!(pps.id, 0);
        assert!(!pps.entropy_coding_mode);
        assert_eq!(pps.pic_init_qp, 26);
        assert_eq!(pps.chroma_qp_index_offset, 2);
        assert_eq!(pps.second_chroma_qp_index_offset, 2); // inherits
        assert!(!pps.transform_8x8_mode);
        assert!(pps.deblocking_filter_control_present);
    }

    #[test]
    fn high_profile_tail_parsed() {
        let mut w = BitWriter::new();
        write_baseline_pps(&mut w);
        w.write_bit(true); // transform_8x8_mode
        w.write_bit(false); // no pic scaling matrix
        w.write_se(-3); // second_chroma_qp_index_offset
        w.write_bit(true); // stop bit
        w.align_to_byte();
        let pps = Pps::parse(w.as_bytes(), |_| None).unwrap();
        assert!(pps.transform_8x8_mode);
        assert_eq!(pps.second_chroma_qp_index_offset, -3);
        assert_eq!(pps.chroma_qp_index_offset, 2);
    }

    #[test]
    fn truncated_pps_is_need_more() {
        assert!(Pps::parse(&[], |_| None).unwrap_err().is_need_more());
    }
}

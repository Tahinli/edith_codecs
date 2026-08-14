//! Slice header (spec 7.3.3, 7.4.3) for all five slice types.

use ec_core::BitReader;
use ec_core::error::{Error, Result};

use crate::nal::NalHeader;
use crate::pps::Pps;
use crate::sps::Sps;

/// `slice_type` % 5 (spec Table 7-6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SliceType {
    /// Predicted slice.
    P,
    /// Bi-predicted slice.
    B,
    /// Intra slice.
    I,
    /// Switching-predicted slice.
    Sp,
    /// Switching-intra slice.
    Si,
}

impl SliceType {
    /// Map `slice_type` (0..=9); values 5..=9 assert the type for the whole
    /// picture but decode identically.
    pub fn from_code(code: u32) -> Result<SliceType> {
        Ok(match code % 5 {
            0 => SliceType::P,
            1 => SliceType::B,
            2 => SliceType::I,
            3 => SliceType::Sp,
            4 => SliceType::Si,
            _ => unreachable!(),
        })
    }

    /// True for I and SI.
    pub fn is_intra(&self) -> bool {
        matches!(self, SliceType::I | SliceType::Si)
    }
}

/// One `ref_pic_list_modification` operation (spec 7.3.3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefPicListMod {
    /// `modification_of_pic_nums_idc` 0/1: `abs_diff_pic_num_minus1`,
    /// subtract (idc 0) or add (idc 1).
    ShortTerm {
        /// `abs_diff_pic_num_minus1`.
        abs_diff_pic_num_minus1: u32,
        /// idc 1 = add, idc 0 = subtract.
        add: bool,
    },
    /// idc 2: `long_term_pic_num`.
    LongTerm(u32),
}

/// One explicit weight entry of the `pred_weight_table` (spec 7.3.3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeightEntry {
    /// `(luma_weight, luma_offset)` when `luma_weight_flag`.
    pub luma: Option<(i32, i32)>,
    /// `[(cb_weight, cb_offset), (cr_weight, cr_offset)]` when signalled.
    pub chroma: Option<[(i32, i32); 2]>,
}

/// `pred_weight_table` (spec 7.3.3.2).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PredWeightTable {
    /// `luma_log2_weight_denom`.
    pub luma_log2_weight_denom: u32,
    /// `chroma_log2_weight_denom` (chroma formats only).
    pub chroma_log2_weight_denom: u32,
    /// Per-reference weights, list 0.
    pub l0: Vec<WeightEntry>,
    /// Per-reference weights, list 1 (B slices).
    pub l1: Vec<WeightEntry>,
}

/// One memory-management control operation (spec 7.3.3.3, Table 7-9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mmco {
    /// `memory_management_control_operation` (1..=6).
    pub op: u32,
    /// First operand (difference_of_pic_nums_minus1 / long_term_pic_num /
    /// long_term_frame_idx / max_long_term_frame_idx_plus1), op-dependent.
    pub arg1: u32,
    /// Second operand (op 3: `long_term_frame_idx`).
    pub arg2: u32,
}

/// `dec_ref_pic_marking` (spec 7.3.3.3).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DecRefPicMarking {
    /// IDR: `no_output_of_prior_pics_flag`.
    pub no_output_of_prior_pics: bool,
    /// IDR: `long_term_reference_flag`.
    pub long_term_reference: bool,
    /// Non-IDR: `adaptive_ref_pic_marking_mode_flag`.
    pub adaptive: bool,
    /// Adaptive-mode operations, in order.
    pub mmcos: Vec<Mmco>,
}

/// Deblocking-filter control from the slice header (spec 7.4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeblockControl {
    /// `disable_deblocking_filter_idc`: 0 = on, 1 = off, 2 = on but not
    /// across slice boundaries.
    pub disable_idc: u8,
    /// `slice_alpha_c0_offset_div2` * 2.
    pub alpha_c0_offset: i32,
    /// `slice_beta_offset_div2` * 2.
    pub beta_offset: i32,
}

/// Parsed slice header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceHeader {
    /// `first_mb_in_slice` (macroblock address; frame coding).
    pub first_mb_in_slice: u32,
    /// Slice type (already reduced mod 5).
    pub slice_type: SliceType,
    /// True when `slice_type` was 5..=9: every slice of the picture has
    /// this type.
    pub all_slices_same_type: bool,
    /// `pic_parameter_set_id`.
    pub pps_id: u32,
    /// `colour_plane_id` (separate colour planes only).
    pub colour_plane_id: u8,
    /// `frame_num`.
    pub frame_num: u32,
    /// `field_pic_flag`.
    pub field_pic: bool,
    /// `bottom_field_flag`.
    pub bottom_field: bool,
    /// `idr_pic_id` for IDR slices.
    pub idr_pic_id: Option<u32>,
    /// `pic_order_cnt_lsb` (POC type 0).
    pub pic_order_cnt_lsb: u32,
    /// `delta_pic_order_cnt_bottom` (POC type 0 + PPS flag).
    pub delta_pic_order_cnt_bottom: i32,
    /// `delta_pic_order_cnt[0..2]` (POC type 1).
    pub delta_pic_order_cnt: [i32; 2],
    /// `redundant_pic_cnt` when present.
    pub redundant_pic_cnt: u32,
    /// B: `direct_spatial_mv_pred_flag`.
    pub direct_spatial_mv_pred: bool,
    /// Active reference count, list 0 (P/SP/B; PPS default applied).
    pub num_ref_idx_l0_active: u32,
    /// Active reference count, list 1 (B; PPS default applied).
    pub num_ref_idx_l1_active: u32,
    /// Reference list modifications, list 0.
    pub ref_pic_list_mod_l0: Vec<RefPicListMod>,
    /// Reference list modifications, list 1.
    pub ref_pic_list_mod_l1: Vec<RefPicListMod>,
    /// Prediction weight table when signalled.
    pub pred_weight_table: Option<PredWeightTable>,
    /// Reference-picture marking when `nal_ref_idc != 0`.
    pub dec_ref_pic_marking: Option<DecRefPicMarking>,
    /// `cabac_init_idc` (CABAC, non-intra slices).
    pub cabac_init_idc: u32,
    /// `SliceQPY` = 26 + pic_init_qp_minus26 + slice_qp_delta.
    pub slice_qp: i32,
    /// SP: `sp_for_switch_flag`.
    pub sp_for_switch: bool,
    /// SP/SI: `SliceQSY`.
    pub slice_qs: i32,
    /// Deblocking control (defaults when the PPS does not signal it).
    pub deblock: DeblockControl,
    /// `slice_group_change_cycle` (FMO map types 3..=5).
    pub slice_group_change_cycle: u32,
    /// Bits consumed by the header — slice data starts here.
    pub header_bits: u64,
}

impl SliceHeader {
    /// Parse a slice header from the RBSP after the NAL header byte.
    pub fn parse(rbsp: &[u8], nal: NalHeader, sps: &Sps, pps: &Pps) -> Result<SliceHeader> {
        let mut reader = BitReader::new(rbsp);
        let r = &mut reader;
        let first_mb_in_slice = r.read_ue()?;
        let slice_type_code = r.read_ue()?;
        if slice_type_code > 9 {
            return Err(Error::corrupt("slice_type > 9"));
        }
        let slice_type = SliceType::from_code(slice_type_code)?;
        let all_slices_same_type = slice_type_code > 4;
        let pps_id = r.read_ue()?;
        if pps_id != pps.id {
            return Err(Error::corrupt("slice parsed against the wrong PPS"));
        }
        let colour_plane_id = if sps.separate_colour_plane {
            r.read_bits(2)? as u8
        } else {
            0
        };
        let frame_num = r.read_bits(sps.log2_max_frame_num as u32)?;
        let mut field_pic = false;
        let mut bottom_field = false;
        if !sps.frame_mbs_only {
            field_pic = r.read_bit()?;
            if field_pic {
                bottom_field = r.read_bit()?;
            }
        }
        let idr_pic_id = if nal.is_idr() {
            let v = r.read_ue()?;
            if v > 65535 {
                return Err(Error::corrupt("idr_pic_id > 65535"));
            }
            Some(v)
        } else {
            None
        };
        let mut pic_order_cnt_lsb = 0;
        let mut delta_pic_order_cnt_bottom = 0;
        let mut delta_pic_order_cnt = [0i32; 2];
        match sps.pic_order_cnt_type {
            0 => {
                pic_order_cnt_lsb = r.read_bits(sps.log2_max_pic_order_cnt_lsb as u32)?;
                if pps.bottom_field_pic_order_in_frame_present && !field_pic {
                    delta_pic_order_cnt_bottom = r.read_se()?;
                }
            }
            1 if !sps.delta_pic_order_always_zero => {
                delta_pic_order_cnt[0] = r.read_se()?;
                if pps.bottom_field_pic_order_in_frame_present && !field_pic {
                    delta_pic_order_cnt[1] = r.read_se()?;
                }
            }
            _ => {}
        }
        let redundant_pic_cnt = if pps.redundant_pic_cnt_present {
            r.read_ue()?
        } else {
            0
        };
        let direct_spatial_mv_pred = if slice_type == SliceType::B {
            r.read_bit()?
        } else {
            false
        };

        let mut num_ref_idx_l0_active = pps.num_ref_idx_l0_default_active;
        let mut num_ref_idx_l1_active = pps.num_ref_idx_l1_default_active;
        if matches!(slice_type, SliceType::P | SliceType::Sp | SliceType::B) {
            if r.read_bit()? {
                // num_ref_idx_active_override_flag
                num_ref_idx_l0_active = r.read_ue()? + 1;
                if slice_type == SliceType::B {
                    num_ref_idx_l1_active = r.read_ue()? + 1;
                }
            }
            if num_ref_idx_l0_active > 32 || num_ref_idx_l1_active > 32 {
                return Err(Error::corrupt("num_ref_idx_active > 32"));
            }
        }

        // ref_pic_list_modification (7.3.3.1) — not for I/SI.
        let mut ref_pic_list_mod_l0 = Vec::new();
        let mut ref_pic_list_mod_l1 = Vec::new();
        if !slice_type.is_intra() {
            Self::parse_ref_list_mod(r, &mut ref_pic_list_mod_l0)?;
        }
        if slice_type == SliceType::B {
            Self::parse_ref_list_mod(r, &mut ref_pic_list_mod_l1)?;
        }

        // pred_weight_table (7.3.3.2).
        let pred_weight_table = if (pps.weighted_pred
            && matches!(slice_type, SliceType::P | SliceType::Sp))
            || (pps.weighted_bipred_idc == 1 && slice_type == SliceType::B)
        {
            Some(Self::parse_pred_weight_table(
                r,
                sps,
                slice_type,
                num_ref_idx_l0_active,
                num_ref_idx_l1_active,
            )?)
        } else {
            None
        };

        // dec_ref_pic_marking (7.3.3.3).
        let dec_ref_pic_marking = if nal.ref_idc != 0 {
            let mut m = DecRefPicMarking::default();
            if nal.is_idr() {
                m.no_output_of_prior_pics = r.read_bit()?;
                m.long_term_reference = r.read_bit()?;
            } else {
                m.adaptive = r.read_bit()?;
                if m.adaptive {
                    loop {
                        let op = r.read_ue()?;
                        if op == 0 {
                            break;
                        }
                        if op > 6 {
                            return Err(Error::corrupt("memory_management_control_operation > 6"));
                        }
                        if m.mmcos.len() >= 64 {
                            return Err(Error::corrupt("more than 64 MMCO operations"));
                        }
                        let arg1 = match op {
                            1..=4 => r.read_ue()?,
                            6 => r.read_ue()?,
                            _ => 0,
                        };
                        let arg2 = if op == 3 { r.read_ue()? } else { 0 };
                        m.mmcos.push(Mmco { op, arg1, arg2 });
                    }
                }
            }
            Some(m)
        } else {
            None
        };

        let cabac_init_idc = if pps.entropy_coding_mode && !slice_type.is_intra() {
            let v = r.read_ue()?;
            if v > 2 {
                return Err(Error::corrupt("cabac_init_idc > 2"));
            }
            v
        } else {
            0
        };

        let slice_qp_delta = r.read_se()?;
        let slice_qp = pps.pic_init_qp + slice_qp_delta;
        if !(-(sps.bit_depth_luma as i32 - 8) * 6..=51).contains(&slice_qp) {
            return Err(Error::corrupt("SliceQPY out of range"));
        }

        let mut sp_for_switch = false;
        let mut slice_qs = pps.pic_init_qs;
        if matches!(slice_type, SliceType::Sp | SliceType::Si) {
            if slice_type == SliceType::Sp {
                sp_for_switch = r.read_bit()?;
            }
            slice_qs = pps.pic_init_qs + r.read_se()?;
            if !(0..=51).contains(&slice_qs) {
                return Err(Error::corrupt("SliceQSY out of range"));
            }
        }

        let mut deblock = DeblockControl::default();
        if pps.deblocking_filter_control_present {
            let idc = r.read_ue()?;
            if idc > 2 {
                return Err(Error::corrupt("disable_deblocking_filter_idc > 2"));
            }
            deblock.disable_idc = idc as u8;
            if idc != 1 {
                let a = r.read_se()?;
                let b = r.read_se()?;
                if !(-6..=6).contains(&a) || !(-6..=6).contains(&b) {
                    return Err(Error::corrupt("deblocking offset div2 outside [-6, 6]"));
                }
                deblock.alpha_c0_offset = a * 2;
                deblock.beta_offset = b * 2;
            }
        }

        let mut slice_group_change_cycle = 0;
        if pps.num_slice_groups > 1
            && let Some(crate::pps::SliceGroupMap::Changing {
                change_rate_minus1, ..
            }) = &pps.slice_group_map
        {
            // u(v) with v = Ceil(Log2(PicSizeInMapUnits / SliceGroupChangeRate + 1)).
            let pic_size_in_map_units =
                sps.mb_width * (sps.mb_height / if sps.frame_mbs_only { 1 } else { 2 });
            let rate = change_rate_minus1 + 1;
            let max = pic_size_in_map_units.div_ceil(rate) + 1;
            let bits = 32 - (max - 1).leading_zeros();
            slice_group_change_cycle = r.read_bits(bits.max(1))?;
        }

        Ok(SliceHeader {
            first_mb_in_slice,
            slice_type,
            all_slices_same_type,
            pps_id,
            colour_plane_id,
            frame_num,
            field_pic,
            bottom_field,
            idr_pic_id,
            pic_order_cnt_lsb,
            delta_pic_order_cnt_bottom,
            delta_pic_order_cnt,
            redundant_pic_cnt,
            direct_spatial_mv_pred,
            num_ref_idx_l0_active,
            num_ref_idx_l1_active,
            ref_pic_list_mod_l0,
            ref_pic_list_mod_l1,
            pred_weight_table,
            dec_ref_pic_marking,
            cabac_init_idc,
            slice_qp,
            sp_for_switch,
            slice_qs,
            deblock,
            slice_group_change_cycle,
            header_bits: reader.bit_position(),
        })
    }

    fn parse_ref_list_mod(r: &mut BitReader<'_>, out: &mut Vec<RefPicListMod>) -> Result<()> {
        if !r.read_bit()? {
            return Ok(()); // ref_pic_list_modification_flag == 0
        }
        loop {
            let idc = r.read_ue()?;
            match idc {
                0 | 1 => out.push(RefPicListMod::ShortTerm {
                    abs_diff_pic_num_minus1: r.read_ue()?,
                    add: idc == 1,
                }),
                2 => out.push(RefPicListMod::LongTerm(r.read_ue()?)),
                3 => return Ok(()),
                _ => return Err(Error::corrupt("modification_of_pic_nums_idc > 3")),
            }
            if out.len() > 64 {
                return Err(Error::corrupt("more than 64 ref list modifications"));
            }
        }
    }

    fn parse_pred_weight_table(
        r: &mut BitReader<'_>,
        sps: &Sps,
        slice_type: SliceType,
        l0_count: u32,
        l1_count: u32,
    ) -> Result<PredWeightTable> {
        let mut t = PredWeightTable {
            luma_log2_weight_denom: r.read_ue()?,
            ..Default::default()
        };
        if t.luma_log2_weight_denom > 7 {
            return Err(Error::corrupt("luma_log2_weight_denom > 7"));
        }
        let has_chroma = sps.chroma_format_idc != 0 && !sps.separate_colour_plane;
        if has_chroma {
            t.chroma_log2_weight_denom = r.read_ue()?;
            if t.chroma_log2_weight_denom > 7 {
                return Err(Error::corrupt("chroma_log2_weight_denom > 7"));
            }
        }
        let parse_list = |r: &mut BitReader<'_>, count: u32| -> Result<Vec<WeightEntry>> {
            let mut list = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let luma = if r.read_bit()? {
                    Some((r.read_se()?, r.read_se()?))
                } else {
                    None
                };
                let chroma = if has_chroma && r.read_bit()? {
                    Some([(r.read_se()?, r.read_se()?), (r.read_se()?, r.read_se()?)])
                } else {
                    None
                };
                list.push(WeightEntry { luma, chroma });
            }
            Ok(list)
        };
        t.l0 = parse_list(r, l0_count)?;
        if slice_type == SliceType::B {
            t.l1 = parse_list(r, l1_count)?;
        }
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nal::NalHeader;
    use crate::pps::Pps;
    use crate::sps::Sps;
    use ec_core::BitWriter;

    fn qcif_sps() -> Sps {
        let mut w = BitWriter::new();
        w.write_bits(66, 8);
        w.write_bits(0xC0, 8);
        w.write_bits(10, 8);
        w.write_ue(0);
        w.write_ue(0); // log2_max_frame_num = 4
        w.write_ue(0); // poc type 0, log2 lsb next
        w.write_ue(0);
        w.write_ue(1);
        w.write_bit(false);
        w.write_ue(10);
        w.write_ue(8);
        w.write_bit(true);
        w.write_bit(true);
        w.write_bit(false);
        w.write_bit(false);
        w.write_bit(true);
        w.align_to_byte();
        Sps::parse(w.as_bytes()).unwrap()
    }

    fn simple_pps() -> Pps {
        let mut w = BitWriter::new();
        w.write_ue(0);
        w.write_ue(0);
        w.write_bit(false);
        w.write_bit(false);
        w.write_ue(0);
        w.write_ue(0);
        w.write_ue(0);
        w.write_bit(false);
        w.write_bits(0, 2);
        w.write_se(0);
        w.write_se(0);
        w.write_se(0);
        w.write_bit(false); // no deblocking control in header
        w.write_bit(false);
        w.write_bit(false);
        w.write_bit(true);
        w.align_to_byte();
        Pps::parse(w.as_bytes(), |_| None).unwrap()
    }

    #[test]
    fn idr_i_slice_header() {
        let sps = qcif_sps();
        let pps = simple_pps();
        let mut w = BitWriter::new();
        w.write_ue(0); // first_mb_in_slice
        w.write_ue(7); // slice_type: I, all slices
        w.write_ue(0); // pps id
        w.write_bits(0, 4); // frame_num
        w.write_ue(3); // idr_pic_id
        w.write_bits(0, 4); // pic_order_cnt_lsb
        w.write_bit(false); // no_output_of_prior_pics (nal_ref_idc != 0)
        w.write_bit(false); // long_term_reference
        w.write_se(4); // slice_qp_delta -> QP 30
        w.write_bit(true); // first slice-data bit, not part of header
        w.align_to_byte();
        let nal = NalHeader::parse(0x65).unwrap();
        let h = SliceHeader::parse(w.as_bytes(), nal, &sps, &pps).unwrap();
        assert_eq!(h.slice_type, SliceType::I);
        assert!(h.all_slices_same_type);
        assert_eq!(h.idr_pic_id, Some(3));
        assert_eq!(h.slice_qp, 30);
        assert_eq!(h.deblock.disable_idc, 0);
        // Header ends right before the trailing data bit we appended.
        assert_eq!(h.header_bits, w.bit_len() - 8 + 7); // wrote 1 bit + 7 pad
    }

    #[test]
    fn p_slice_fields_parse() {
        let sps = qcif_sps();
        let pps = simple_pps();
        let mut w = BitWriter::new();
        w.write_ue(2); // first_mb_in_slice
        w.write_ue(0); // slice_type: P
        w.write_ue(0); // pps id
        w.write_bits(1, 4); // frame_num
        w.write_bits(2, 4); // pic_order_cnt_lsb
        w.write_bit(true); // num_ref_idx_active_override
        w.write_ue(1); // l0 active = 2
        w.write_bit(true); // ref_pic_list_modification_flag_l0
        w.write_ue(0); // idc 0: short term subtract
        w.write_ue(4); // abs_diff_pic_num_minus1
        w.write_ue(3); // idc 3: end
        w.write_bit(false); // adaptive_ref_pic_marking (nal_ref_idc != 0)
        w.write_se(-2); // slice_qp_delta -> 24
        w.write_bit(true);
        w.align_to_byte();
        let nal = NalHeader::parse(0x41).unwrap(); // non-IDR slice, ref
        let h = SliceHeader::parse(w.as_bytes(), nal, &sps, &pps).unwrap();
        assert_eq!(h.slice_type, SliceType::P);
        assert_eq!(h.first_mb_in_slice, 2);
        assert_eq!(h.num_ref_idx_l0_active, 2);
        assert_eq!(
            h.ref_pic_list_mod_l0,
            vec![RefPicListMod::ShortTerm {
                abs_diff_pic_num_minus1: 4,
                add: false
            }]
        );
        assert_eq!(h.slice_qp, 24);
        assert!(h.dec_ref_pic_marking.is_some());
    }

    #[test]
    fn truncated_header_is_need_more() {
        let sps = qcif_sps();
        let pps = simple_pps();
        let nal = NalHeader::parse(0x65).unwrap();
        assert!(
            SliceHeader::parse(&[0x88], nal, &sps, &pps)
                .unwrap_err()
                .is_need_more()
        );
    }
}

//! The syntax layer of Rec. ITU-T H.264 (ISO/IEC 14496-10): NAL units,
//! sequence and picture parameter sets, and slice headers.
//!
//! This crate parses; it does not decode. It exists on its own because the
//! software decoder ([`ec-h264`](https://docs.rs/ec-h264)) and the stateless
//! hardware decoder need the same headers, and because a container muxer needs
//! them without dragging a decoder in.
//!
//! The transcription is deliberately literal. Structure fields carry the
//! specification's own syntax element names — `pic_width_in_mbs_minus1`, not
//! `width` — so that any field can be checked against the syntax table it came
//! from, and derived variables of clause 7.4.2 are methods
//! ([`SequenceParameterSet::pic_width_in_mbs`]) rather than stored state that
//! could drift from the fields it is derived from.
//!
//! ```
//! use ec_h264_syntax::{NalUnit, NalUnitType, SequenceParameterSet, annex_b_units};
//!
//! // A minimal Annex B stream carrying one SPS (Baseline, 176x144).
//! let stream = [
//!     0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x0A, 0xDA, 0x0B, 0x13, 0x90,
//! ];
//! let unit = NalUnit::parse(annex_b_units(&stream)[0]).unwrap();
//! assert_eq!(unit.nal_unit_type, NalUnitType::Sps);
//! let sps = SequenceParameterSet::parse(&unit.rbsp).unwrap();
//! assert_eq!(sps.cropped_size().unwrap(), (176, 144));
//! ```
//!
//! Truncated input is [`ec_core::Error::NeedMore`] and a bitstream that
//! violates its own rules is [`ec_core::Error::Corrupt`]; nothing here panics on
//! input.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod nal;
pub mod pps;
pub mod slice;
pub mod sps;

pub use nal::{NalUnit, NalUnitType, RbspReader, annex_b_units, ebsp_from_rbsp, rbsp_from_ebsp};
pub use pps::{PicParameterSet, SliceGroupMap};
pub use slice::{
    DecRefPicMarking, MemoryManagementControlOperation, PredWeightTable, RefPicListModification,
    SliceHeader, SliceType,
};
pub use sps::{HrdParameters, ScalingList, SequenceParameterSet, VuiParameters, scaling_list};

#[cfg(test)]
mod tests {
    use super::*;
    use ec_core::bitio::BitWriter;

    /// The SPS of the doc example, written out field by field so the parse can
    /// be checked against known values rather than against itself.
    fn baseline_sps_rbsp() -> Vec<u8> {
        let mut w = BitWriter::new();
        w.write_bits(66, 8); // profile_idc: Baseline
        w.write_bits(0, 8); // constraint flags + reserved_zero_2bits
        w.write_bits(10, 8); // level_idc: 1.0
        w.write_ue(0); // seq_parameter_set_id
        w.write_ue(0); // log2_max_frame_num_minus4
        w.write_ue(2); // pic_order_cnt_type
        w.write_ue(1); // max_num_ref_frames
        w.write_bit(false); // gaps_in_frame_num_value_allowed_flag
        w.write_ue(10); // pic_width_in_mbs_minus1: 11 MBs = 176
        w.write_ue(8); // pic_height_in_map_units_minus1: 9 MBs = 144
        w.write_bit(true); // frame_mbs_only_flag
        w.write_bit(true); // direct_8x8_inference_flag
        w.write_bit(false); // frame_cropping_flag
        w.write_bit(false); // vui_parameters_present_flag
        w.write_bit(true); // rbsp_stop_one_bit
        w.align_to_byte();
        w.into_bytes()
    }

    #[test]
    fn annex_b_split_and_nal_header() {
        // Two units, the second preceded by a four-byte start code, and a
        // trailing zero byte that belongs to neither payload.
        let stream = [
            0x00, 0x00, 0x01, 0x67, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x68, 0xBB, 0x00,
        ];
        let units = annex_b_units(&stream);
        assert_eq!(units.len(), 2);
        assert_eq!(units[0], &[0x67, 0xAA]);
        assert_eq!(units[1], &[0x68, 0xBB]);
        let sps = NalUnit::parse(units[0]).unwrap();
        assert_eq!(sps.nal_unit_type, NalUnitType::Sps);
        assert_eq!(sps.nal_ref_idc, 3);
        assert!(sps.nal_unit_type.is_vcl().eq(&false));
        assert!(NalUnit::parse(&[0x85]).is_err(), "forbidden_zero_bit");
    }

    #[test]
    fn emulation_prevention_round_trip() {
        // Every sequence that must be escaped, plus one that must not.
        let rbsp = vec![
            0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x02, 0x00, 0x00, 0x03, 0x00, 0x00, 0x04,
            0x00, 0x01, 0x02,
        ];
        let ebsp = ebsp_from_rbsp(&rbsp);
        assert!(ebsp.len() > rbsp.len(), "escapes were inserted");
        assert_eq!(rbsp_from_ebsp(&ebsp), rbsp);
        // 0x000004 needs no escape; 0x000003 does, and survives the round trip.
        assert_eq!(
            rbsp_from_ebsp(&[0x00, 0x00, 0x03, 0x01]),
            [0x00, 0x00, 0x01]
        );
        assert_eq!(rbsp_from_ebsp(&[0x00, 0x00, 0x04]), [0x00, 0x00, 0x04]);
    }

    #[test]
    fn more_rbsp_data_stops_at_the_stop_bit() {
        // Payload 0b1011 followed by rbsp_stop_one_bit and zero padding.
        let rbsp = [0b1011_1000u8];
        let mut r = RbspReader::new(&rbsp);
        assert!(r.more_rbsp_data());
        r.bits().read_bits(4).unwrap();
        assert!(!r.more_rbsp_data(), "only the stop bit is left");
        assert!(!RbspReader::new(&[0x00]).more_rbsp_data());
    }

    #[test]
    fn sps_fields_and_derived_values() {
        let sps = SequenceParameterSet::parse(&baseline_sps_rbsp()).unwrap();
        assert_eq!(sps.profile_idc, 66);
        assert_eq!(sps.pic_width_in_mbs(), 11);
        assert_eq!(sps.frame_height_in_mbs(), 9);
        assert_eq!(
            sps.chroma_array_type(),
            1,
            "Baseline is 4:2:0 by definition"
        );
        assert_eq!(sps.sub_wh_c(), Some((2, 2)));
        assert_eq!(sps.mb_wh_c(), (8, 8));
        assert_eq!(sps.max_frame_num(), 16);
        assert_eq!(sps.bit_depth_y(), 8);
        assert_eq!(sps.qp_bd_offset_y(), 0);
        assert_eq!(sps.cropped_size().unwrap(), (176, 144));
        // Truncation is NeedMore, never a panic.
        let short = &baseline_sps_rbsp()[..3];
        assert!(
            SequenceParameterSet::parse(short)
                .unwrap_err()
                .is_need_more()
        );
    }

    #[test]
    fn sps_cropping_and_field_coding() {
        let mut w = BitWriter::new();
        w.write_bits(77, 8); // profile_idc: Main
        w.write_bits(0, 8);
        w.write_bits(30, 8);
        w.write_ue(0);
        w.write_ue(0); // log2_max_frame_num_minus4
        w.write_ue(0); // pic_order_cnt_type 0
        w.write_ue(2); // log2_max_pic_order_cnt_lsb_minus4
        w.write_ue(2); // max_num_ref_frames
        w.write_bit(false);
        w.write_ue(119); // 120 MBs = 1920
        w.write_ue(33); // 34 map units; frame_mbs_only 0 => 68 MBs = 1088
        w.write_bit(false); // frame_mbs_only_flag = 0
        w.write_bit(false); // mb_adaptive_frame_field_flag
        w.write_bit(true); // direct_8x8_inference_flag
        w.write_bit(true); // frame_cropping_flag
        w.write_ue(0); // left
        w.write_ue(0); // right
        w.write_ue(0); // top
        w.write_ue(1); // bottom: CropUnitY = 4 here, so 1088 - 4 = 1084
        w.write_bit(false);
        w.write_bit(true);
        w.align_to_byte();
        let sps = SequenceParameterSet::parse(&w.into_bytes()).unwrap();
        assert!(!sps.frame_mbs_only_flag);
        assert_eq!(sps.frame_height_in_mbs(), 68);
        assert_eq!(sps.max_pic_order_cnt_lsb(), 64);
        assert_eq!(sps.crop_units(), (2, 4));
        assert_eq!(sps.cropped_size().unwrap(), (1920, 1084));
    }

    #[test]
    fn scaling_list_default_and_frozen_tail() {
        // delta_scale = 0 on the first coefficient means "use the default
        // matrix"; a nextScale of 0 later freezes the remaining coefficients.
        let mut w = BitWriter::new();
        w.write_se(-8); // nextScale = 0 at j = 0
        w.align_to_byte();
        let bytes = w.into_bytes();
        let mut r = ec_core::bitio::BitReader::new(&bytes);
        let list = scaling_list(&mut r, 16).unwrap();
        assert!(list.use_default);
        assert!(list.values.iter().all(|&v| v == 8));

        let mut w = BitWriter::new();
        w.write_se(2); // 10
        w.write_se(-2); // 8
        w.write_se(-8); // 0 -> freeze at 8 for the rest
        w.align_to_byte();
        let bytes = w.into_bytes();
        let mut r = ec_core::bitio::BitReader::new(&bytes);
        let list = scaling_list(&mut r, 16).unwrap();
        assert!(!list.use_default);
        assert_eq!(&list.values[..4], &[10, 8, 8, 8]);
    }

    #[test]
    fn pps_defaults_second_chroma_qp_offset_to_the_first() {
        let mut w = BitWriter::new();
        w.write_ue(0); // pic_parameter_set_id
        w.write_ue(0); // seq_parameter_set_id
        w.write_bit(false); // entropy_coding_mode_flag: CAVLC
        w.write_bit(false); // bottom_field_pic_order_in_frame_present_flag
        w.write_ue(0); // num_slice_groups_minus1
        w.write_ue(0); // num_ref_idx_l0_default_active_minus1
        w.write_ue(0); // num_ref_idx_l1_default_active_minus1
        w.write_bit(false); // weighted_pred_flag
        w.write_bits(0, 2); // weighted_bipred_idc
        w.write_se(-2); // pic_init_qp_minus26 => 24
        w.write_se(0); // pic_init_qs_minus26
        w.write_se(3); // chroma_qp_index_offset
        w.write_bit(true); // deblocking_filter_control_present_flag
        w.write_bit(false); // constrained_intra_pred_flag
        w.write_bit(false); // redundant_pic_cnt_present_flag
        w.write_bit(true); // rbsp_stop_one_bit
        w.align_to_byte();
        let pps = PicParameterSet::parse(&w.into_bytes(), 1).unwrap();
        assert!(!pps.entropy_coding_mode_flag);
        assert_eq!(pps.pic_init_qp_minus26, -2);
        assert_eq!(pps.chroma_qp_index_offset, 3);
        assert_eq!(
            pps.second_chroma_qp_index_offset, 3,
            "absent extension means the Cr offset equals the Cb offset"
        );
        assert!(!pps.transform_8x8_mode_flag);
        // Two ue(v) values of 0 are two '1' bits: 0b11000000.
        assert_eq!(pps::peek_ids(&[0xC0]).unwrap(), (0, 0));
    }

    #[test]
    fn slice_header_i_slice_round_trip() {
        let sps = SequenceParameterSet::parse(&baseline_sps_rbsp()).unwrap();
        let mut w = BitWriter::new();
        w.write_ue(0); // pic_parameter_set_id
        w.write_ue(0); // seq_parameter_set_id
        w.write_bit(false); // entropy_coding_mode_flag
        w.write_bit(false); // bottom_field_pic_order_in_frame_present_flag
        w.write_ue(0); // num_slice_groups_minus1
        w.write_ue(0); // num_ref_idx_l0_default_active_minus1
        w.write_ue(0); // num_ref_idx_l1_default_active_minus1
        w.write_bit(false); // weighted_pred_flag
        w.write_bits(0, 2); // weighted_bipred_idc
        w.write_se(0); // pic_init_qp_minus26
        w.write_se(0); // pic_init_qs_minus26
        w.write_se(0); // chroma_qp_index_offset
        w.write_bit(true); // deblocking_filter_control_present_flag
        w.write_bit(false); // constrained_intra_pred_flag
        w.write_bit(false); // redundant_pic_cnt_present_flag
        w.write_bit(true);
        w.align_to_byte();
        let pps = PicParameterSet::parse(&w.into_bytes(), 1).unwrap();

        let mut w = BitWriter::new();
        w.write_ue(0); // first_mb_in_slice
        w.write_ue(7); // slice_type 7: I, all slices same type
        w.write_ue(0); // pic_parameter_set_id
        w.write_bits(0, 4); // frame_num
        w.write_ue(0); // idr_pic_id
        // pic_order_cnt_type is 2 in this SPS: no POC syntax elements.
        w.write_bit(false); // no_output_of_prior_pics_flag
        w.write_bit(false); // long_term_reference_flag
        w.write_se(-3); // slice_qp_delta => SliceQPY 23
        w.write_ue(1); // disable_deblocking_filter_idc: no offsets follow
        w.write_bit(true);
        w.align_to_byte();
        let rbsp = w.into_bytes();
        let mut rr = RbspReader::new(&rbsp);
        let h = SliceHeader::parse(&mut rr, NalUnitType::IdrSlice, 3, &sps, &pps).unwrap();
        assert_eq!(h.slice_type, SliceType::I);
        assert!(h.all_slices_same_type);
        assert!(h.slice_type.is_intra());
        assert_eq!(h.slice_qp_y(&pps), 23);
        assert_eq!(h.disable_deblocking_filter_idc, 1);
        assert!(h.dec_ref_pic_marking.is_some());
        assert!(h.pred_weight_table.is_none());
        assert_eq!(h.pic_height_in_mbs(&sps), 9);
        assert!(!h.mbaff_frame_flag(&sps));
    }

    #[test]
    fn slice_header_p_slice_reads_inter_fields() {
        let sps = SequenceParameterSet::parse(&baseline_sps_rbsp()).unwrap();
        let mut w = BitWriter::new();
        w.write_ue(0);
        w.write_ue(0);
        w.write_bit(false);
        w.write_bit(false);
        w.write_ue(0);
        w.write_ue(0);
        w.write_ue(0);
        w.write_bit(true); // weighted_pred_flag
        w.write_bits(0, 2);
        w.write_se(0);
        w.write_se(0);
        w.write_se(0);
        w.write_bit(false); // deblocking_filter_control_present_flag
        w.write_bit(false);
        w.write_bit(false);
        w.write_bit(true);
        w.align_to_byte();
        let pps = PicParameterSet::parse(&w.into_bytes(), 1).unwrap();

        let mut w = BitWriter::new();
        w.write_ue(11); // first_mb_in_slice
        w.write_ue(0); // slice_type 0: P
        w.write_ue(0);
        w.write_bits(3, 4); // frame_num
        w.write_bit(true); // num_ref_idx_active_override_flag
        w.write_ue(1); // num_ref_idx_l0_active_minus1
        w.write_bit(true); // ref_pic_list_modification_flag_l0
        w.write_ue(0); // idc 0
        w.write_ue(4); // abs_diff_pic_num_minus1
        w.write_ue(3); // idc 3: end
        // pred_weight_table(): denominators, then one explicit weight pair.
        w.write_ue(5); // luma_log2_weight_denom
        w.write_ue(6); // chroma_log2_weight_denom
        w.write_bit(true); // luma_weight_l0_flag[0]
        w.write_se(33);
        w.write_se(-7);
        w.write_bit(false); // chroma_weight_l0_flag[0]
        w.write_bit(false); // luma_weight_l0_flag[1]
        w.write_bit(false); // chroma_weight_l0_flag[1]
        w.write_bit(false); // adaptive_ref_pic_marking_mode_flag
        w.write_se(2); // slice_qp_delta
        w.write_bit(true);
        w.align_to_byte();
        let rbsp = w.into_bytes();
        let mut rr = RbspReader::new(&rbsp);
        let h = SliceHeader::parse(&mut rr, NalUnitType::NonIdrSlice, 2, &sps, &pps).unwrap();
        assert_eq!(h.slice_type, SliceType::P);
        assert_eq!(h.first_mb_in_slice, 11);
        assert_eq!(h.frame_num, 3);
        assert_eq!(h.num_ref_idx_l0_active_minus1, 1);
        assert_eq!(h.ref_pic_list_modification[0].len(), 1);
        assert_eq!(h.ref_pic_list_modification[0][0].value, 4);
        let t = h.pred_weight_table.as_ref().unwrap();
        assert_eq!(t.luma_weights[0][0], (33, -7));
        assert_eq!(t.luma_weights[0][1], (1 << 5, 0), "defaulted weight");
        assert_eq!(t.chroma_weights[0][0], [(1 << 6, 0); 2]);
        assert_eq!(h.slice_qp_y(&pps), 28);
        assert_eq!(h.header_bits, rr.bits().bit_position());
    }
}

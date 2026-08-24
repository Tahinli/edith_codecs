//! Parameter sets and slice headers on the writing side (spec 7.3.2, 7.3.3).
//!
//! Only the subset this encoder emits is written: 8-bit 4:2:0 progressive,
//! CAVLC, one slice group, one reference, `pic_order_cnt_type` 2 — which is
//! the type that says decode order *is* output order, and costs no bits in
//! any slice header. The test at the bottom parses everything back through
//! [`ec_h264_syntax`], which is the same parser the decoder uses.

use ec_core::BitWriter;
use ec_h264_syntax::SliceType;

/// Geometry and timing of the sequence, everything the parameter sets need.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SeqParams {
    /// Displayed luma width.
    pub width: u32,
    /// Displayed luma height.
    pub height: u32,
    /// Coded width in macroblocks.
    pub mb_w: u32,
    /// Coded height in macroblocks.
    pub mb_h: u32,
    /// `(num_units_in_tick, time_scale)`; `time_scale` is twice the frame rate.
    pub timing: (u32, u32),
    /// Bits per second the level has to carry.
    pub bitrate: u32,
    /// CABAC, which Baseline does not allow: the sequence is then Main.
    pub cabac: bool,
    /// 8x8 transform (High profile). Forces `profile_idc` 100 with no
    /// constraint flags, and the High-profile SPS tail (7.3.2.1.1 subset:
    /// 4:2:0 8-bit, no scaling matrices, no transform bypass).
    pub transform_8x8: bool,
}

/// `log2_max_frame_num`, fixed: 8 bits of frame_num wrap far beyond the one
/// reference picture this encoder keeps.
pub(crate) const LOG2_MAX_FRAME_NUM: u32 = 8;

/// Annex A levels as `(level_idc, MaxMBPS, MaxFS, MaxBR in kbit/s)`, in
/// ascending order; the first one that fits is what the SPS advertises.
const LEVELS: [(u8, u64, u64, u64); 16] = [
    (10, 1485, 99, 64),
    (11, 3000, 396, 192),
    (12, 6000, 396, 384),
    (13, 11880, 396, 768),
    (20, 11880, 396, 2000),
    (21, 19800, 792, 4000),
    (22, 20250, 1620, 4000),
    (30, 40500, 1620, 10000),
    (31, 108000, 3600, 14000),
    (32, 216000, 5120, 20000),
    (40, 245760, 8192, 20000),
    (42, 522240, 8704, 50000),
    (50, 589824, 22080, 135000),
    (51, 983040, 36864, 240000),
    (52, 2073600, 36864, 240000),
    (62, 16711680, 139264, 800000),
];

fn level_idc(p: &SeqParams) -> u8 {
    let frame_mbs = u64::from(p.mb_w) * u64::from(p.mb_h);
    let fps = f64::from(p.timing.1) / (2.0 * f64::from(p.timing.0).max(1.0));
    let mbps = (frame_mbs as f64 * fps).ceil() as u64;
    let kbps = u64::from(p.bitrate) / 1000;
    for &(idc, max_mbps, max_fs, max_br) in &LEVELS {
        if mbps <= max_mbps && frame_mbs <= max_fs && kbps <= max_br {
            return idc;
        }
    }
    62
}

/// Write the sequence parameter set RBSP (7.3.2.1), Baseline profile with the
/// Main-profile constraint flag set: this encoder emits nothing (no FMO, no
/// ASO, no redundant slices, no interlace) that either profile forbids.
pub(crate) fn write_sps(p: &SeqParams) -> Vec<u8> {
    let mut w = BitWriter::with_capacity(32);
    // Main when CABAC is in force (Baseline has no CABAC), Baseline otherwise;
    // constraint_set1_flag says the stream also obeys the Main constraints.
    // 8x8 transform needs High profile (100), which sets no constraint flags.
    if p.transform_8x8 {
        w.write_bits(100, 8);
        w.write_bits(0, 8);
    } else {
        w.write_bits(if p.cabac { 77 } else { 66 }, 8);
        w.write_bits(if p.cabac { 0b0100_0000 } else { 0b1100_0000 }, 8);
    }
    w.write_bits(u32::from(level_idc(p)), 8);
    w.write_ue(0); // seq_parameter_set_id
    if p.transform_8x8 {
        w.write_ue(1); // chroma_format_idc: 4:2:0
        w.write_ue(0); // bit_depth_luma_minus8
        w.write_ue(0); // bit_depth_chroma_minus8
        w.write_bit(false); // qpprime_y_zero_transform_bypass_flag
        w.write_bit(false); // seq_scaling_matrix_present_flag
    }
    w.write_ue(LOG2_MAX_FRAME_NUM - 4);
    w.write_ue(2); // pic_order_cnt_type 2: output order is decode order
    w.write_ue(1); // max_num_ref_frames
    w.write_bit(false); // gaps_in_frame_num_value_allowed_flag
    w.write_ue(p.mb_w - 1);
    w.write_ue(p.mb_h - 1);
    w.write_bit(true); // frame_mbs_only_flag
    w.write_bit(true); // direct_8x8_inference_flag
    let crop_right = (p.mb_w * 16 - p.width) / 2; // CropUnitX = 2 for 4:2:0
    let crop_bottom = (p.mb_h * 16 - p.height) / 2; // CropUnitY = 2, frame only
    let cropping = crop_right != 0 || crop_bottom != 0;
    w.write_bit(cropping);
    if cropping {
        w.write_ue(0);
        w.write_ue(crop_right);
        w.write_ue(0);
        w.write_ue(crop_bottom);
    }
    // VUI: timing only, so a container-less stream still states its frame rate.
    w.write_bit(true); // vui_parameters_present_flag
    w.write_bit(false); // aspect_ratio_info_present_flag
    w.write_bit(false); // overscan_info_present_flag
    w.write_bit(false); // video_signal_type_present_flag
    w.write_bit(false); // chroma_loc_info_present_flag
    w.write_bit(true); // timing_info_present_flag
    w.write_bits(p.timing.0, 32);
    w.write_bits(p.timing.1, 32);
    w.write_bit(true); // fixed_frame_rate_flag
    w.write_bit(false); // nal_hrd_parameters_present_flag
    w.write_bit(false); // vcl_hrd_parameters_present_flag
    w.write_bit(false); // pic_struct_present_flag
    w.write_bit(false); // bitstream_restriction_flag
    trailing(&mut w);
    w.into_bytes()
}

/// Write the picture parameter set RBSP (7.3.2.2).
pub(crate) fn write_pps(cabac: bool, transform_8x8: bool, cqo: i32) -> Vec<u8> {
    let mut w = BitWriter::with_capacity(8);
    w.write_ue(0); // pic_parameter_set_id
    w.write_ue(0); // seq_parameter_set_id
    w.write_bit(cabac); // entropy_coding_mode_flag
    w.write_bit(false); // bottom_field_pic_order_in_frame_present_flag
    w.write_ue(0); // num_slice_groups_minus1
    w.write_ue(0); // num_ref_idx_l0_default_active_minus1
    w.write_ue(0); // num_ref_idx_l1_default_active_minus1
    w.write_bit(false); // weighted_pred_flag
    w.write_bits(0, 2); // weighted_bipred_idc
    w.write_se(0); // pic_init_qp_minus26
    w.write_se(0); // pic_init_qs_minus26
    w.write_se(cqo); // chroma_qp_index_offset
    w.write_bit(false); // deblocking_filter_control_present_flag
    w.write_bit(false); // constrained_intra_pred_flag
    w.write_bit(false); // redundant_pic_cnt_present_flag
    if transform_8x8 {
        w.write_bit(true); // transform_8x8_mode_flag
        w.write_bit(false); // pic_scaling_matrix_present_flag
        w.write_se(cqo); // second_chroma_qp_index_offset
    }
    trailing(&mut w);
    w.into_bytes()
}

/// What one slice header says about the picture it opens.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SliceParams {
    pub first_mb: u32,
    pub slice_type: SliceType,
    pub frame_num: u32,
    pub idr: bool,
    /// `idr_pic_id`, alternating between consecutive IDR pictures.
    pub idr_pic_id: u32,
    pub qp: i32,
    /// CABAC is in force, so a P slice carries cabac_init_idc.
    pub cabac: bool,
}

/// Write a slice header (7.3.3) into `w`; slice data follows immediately.
pub(crate) fn write_slice_header(w: &mut BitWriter, s: &SliceParams) {
    w.write_ue(s.first_mb);
    // 5..=9 assert that every slice of this picture has the same type, which
    // is true here: the picture is coded in one pass at one type.
    w.write_ue(if s.slice_type == SliceType::I { 7 } else { 5 });
    w.write_ue(0); // pic_parameter_set_id
    w.write_bits(s.frame_num, LOG2_MAX_FRAME_NUM);
    if s.idr {
        w.write_ue(s.idr_pic_id);
    }
    // pic_order_cnt_type 2: no picture order count in the header at all.
    if s.slice_type == SliceType::P {
        w.write_bit(false); // num_ref_idx_active_override_flag
        w.write_bit(false); // ref_pic_list_modification_flag_l0
    }
    // Every picture here is a reference picture (nal_ref_idc != 0).
    if s.idr {
        w.write_bit(false); // no_output_of_prior_pics_flag
        w.write_bit(false); // long_term_reference_flag
    } else {
        w.write_bit(false); // adaptive_ref_pic_marking_mode_flag: sliding window
    }
    if s.cabac && s.slice_type != SliceType::I {
        w.write_ue(0); // cabac_init_idc
    }
    w.write_se(s.qp - 26); // slice_qp_delta, pic_init_qp_minus26 being 0
    // deblocking_filter_control_present_flag is 0: the filter is on, offsets 0.
}

/// `rbsp_trailing_bits` (7.3.2.11).
pub(crate) fn trailing(w: &mut BitWriter) {
    w.write_bit(true);
    w.align_to_byte();
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_h264_syntax::{Pps, Sps};

    fn params(width: u32, height: u32) -> SeqParams {
        SeqParams {
            width,
            height,
            mb_w: width.div_ceil(16),
            mb_h: height.div_ceil(16),
            timing: (1000, 60000),
            bitrate: 8_000_000,
            cabac: true,
            transform_8x8: false,
        }
    }

    /// The parameter sets this encoder writes are the ones the decoder's own
    /// parser reads back, geometry, cropping, profile and entropy coder alike.
    #[test]
    fn parameter_sets_parse_back() {
        for (w, h, cabac, t8x8) in [
            (1920, 1080, true, false),
            (1920, 1080, false, false),
            (640, 480, true, false),
            (854, 482, false, false),
            (16, 16, true, false),
            (640, 480, true, true),
            (640, 480, false, true),
        ] {
            let mut p = params(w, h);
            p.cabac = cabac;
            p.transform_8x8 = t8x8;
            let sps = Sps::parse(&write_sps(&p)).expect("SPS parses");
            assert_eq!((sps.width, sps.height), (w, h), "{w}x{h} crop");
            assert_eq!(sps.mb_width, p.mb_w);
            assert_eq!(sps.mb_height, p.mb_h);
            assert_eq!(sps.pic_order_cnt_type, 2);
            assert_eq!(sps.max_num_ref_frames, 1);
            assert!(sps.frame_mbs_only);
            let vui = sps.vui.as_ref().expect("VUI present");
            assert_eq!(vui.timing_info, Some((1000, 60000, true)));
            assert_eq!(
                sps.profile_idc,
                if t8x8 {
                    100
                } else if cabac {
                    77
                } else {
                    66
                }
            );
            if t8x8 {
                assert_eq!(sps.chroma_format_idc, 1);
                assert_eq!(sps.bit_depth_luma, 8);
            }
            let pps = Pps::parse(&write_pps(cabac, t8x8, -2), |_| Some(&sps)).expect("PPS parses");
            assert_eq!(pps.entropy_coding_mode, cabac);
            assert_eq!(pps.pic_init_qp, 26);
            assert_eq!(pps.transform_8x8_mode, t8x8);
            assert_eq!(pps.chroma_qp_index_offset, -2);
            // Absent, it is inferred equal to chroma_qp_index_offset (7.4.2.2).
            assert_eq!(pps.second_chroma_qp_index_offset, -2);
        }
    }

    /// 1080p60 at 8 Mbit/s needs level 4.0; a small clip needs far less.
    #[test]
    fn level_follows_the_load() {
        assert_eq!(level_idc(&params(1920, 1080)), 40);
        let mut small = params(320, 240);
        small.timing = (1000, 30000);
        small.bitrate = 300_000;
        assert!(level_idc(&small) <= 21);
    }

    /// A slice header reads back at the values that were written.
    #[test]
    fn slice_header_parses_back() {
        let p = params(320, 240);
        let sps = Sps::parse(&write_sps(&p)).unwrap();
        let pps = Pps::parse(&write_pps(true, false, 0), |_| Some(&sps)).unwrap();
        for (idr, slice_type, first_mb, qp) in [
            (true, SliceType::I, 0u32, 30i32),
            (false, SliceType::P, 40, 18),
        ] {
            let mut w = BitWriter::new();
            write_slice_header(
                &mut w,
                &SliceParams {
                    first_mb,
                    slice_type,
                    frame_num: 7,
                    idr,
                    idr_pic_id: 1,
                    qp,
                    cabac: true,
                },
            );
            trailing(&mut w);
            let rbsp = w.into_bytes();
            let header = ec_h264_syntax::SliceHeader::parse(
                &rbsp,
                ec_h264_syntax::nal::NalHeader {
                    ref_idc: 1,
                    unit_type: if idr {
                        ec_h264_syntax::nal::NalUnitType::SliceIdr
                    } else {
                        ec_h264_syntax::nal::NalUnitType::Slice
                    },
                },
                &sps,
                &pps,
            )
            .expect("slice header parses");
            assert_eq!(header.first_mb_in_slice, first_mb);
            assert_eq!(header.slice_type, slice_type);
            assert!(header.all_slices_same_type);
            assert_eq!(header.frame_num, 7);
            assert_eq!(header.slice_qp, qp);
        }
    }
}

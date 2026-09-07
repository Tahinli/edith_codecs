//! Packed headers: the parameter sets and slice headers the *application*
//! writes for the driver.
//!
//! This is not optional plumbing. radeonsi advertises
//! `VA_ENC_PACKED_HEADER_SEQUENCE | PICTURE | SLICE` and then writes none of
//! them itself: without these, the coded buffer comes back holding slice data
//! with no parameter sets in front of it — a stream the oracle answers with
//! "Invalid data found when processing input" (measured, 2026-08-14). Every
//! field below therefore has to agree with the `VAEnc*ParameterBuffer` the
//! driver was given, or the header describes a picture other than the one the
//! hardware coded.

use ec_core::BitWriter;

use super::Colour;
use crate::params::enc::{PACKED_HEADER_PICTURE, PACKED_HEADER_SEQUENCE, PACKED_HEADER_SLICE};

/// A header ready to submit: escaped bytes plus its exact length in bits.
///
/// The bit length matters for slice headers, which do not end on a byte
/// boundary — the driver continues the bitstream from exactly that bit.
pub(crate) struct Packed {
    pub(crate) kind: u32,
    pub(crate) bytes: Vec<u8>,
    pub(crate) bits: u32,
}

/// Wrap a complete NAL: start code, header byte(s), escaped payload.
fn nal(kind: u32, header: &[u8], rbsp: &[u8], padding_bits: u32) -> Packed {
    let mut bytes = vec![0, 0, 0, 1];
    bytes.extend_from_slice(header);
    let mut escaped = Vec::with_capacity(rbsp.len() + 8);
    ec_h264_syntax::escape_rbsp(rbsp, &mut escaped);
    bytes.extend_from_slice(&escaped);
    let bits = bytes.len() as u32 * 8 - padding_bits;
    Packed { kind, bytes, bits }
}

// ---------------------------------------------------------------------------
// H.264
// ---------------------------------------------------------------------------

/// What the H.264 headers need, mirroring the sequence parameter buffer.
pub(super) struct H264Params {
    pub(super) level_idc: u8,
    pub(super) mb_width: u32,
    pub(super) mb_height: u32,
    pub(super) crop_right: u32,
    pub(super) crop_bottom: u32,
    pub(super) num_units_in_tick: u32,
    pub(super) time_scale: u32,
    pub(super) pic_init_qp: u8,
    pub(super) colour: Option<Colour>,
}

/// `log2_max_frame_num`, matching `seq_fields.log2_max_frame_num_minus4 = 0`.
pub(super) const H264_LOG2_MAX_FRAME_NUM: u32 = 4;

/// The sequence parameter set (7.3.2.1), High profile.
pub(super) fn h264_sps(p: &H264Params) -> Packed {
    let mut w = BitWriter::with_capacity(64);
    w.write_bits(100, 8); // profile_idc: High
    w.write_bits(0, 8); // constraint_set flags + reserved_zero_2bits
    w.write_bits(u32::from(p.level_idc), 8);
    w.write_ue(0); // seq_parameter_set_id
    // High profile tail.
    w.write_ue(1); // chroma_format_idc: 4:2:0
    w.write_ue(0); // bit_depth_luma_minus8
    w.write_ue(0); // bit_depth_chroma_minus8
    w.write_bit(false); // qpprime_y_zero_transform_bypass_flag
    w.write_bit(false); // seq_scaling_matrix_present_flag
    w.write_ue(H264_LOG2_MAX_FRAME_NUM - 4);
    w.write_ue(2); // pic_order_cnt_type 2: output order is decode order
    w.write_ue(1); // max_num_ref_frames
    w.write_bit(false); // gaps_in_frame_num_value_allowed_flag
    w.write_ue(p.mb_width - 1);
    w.write_ue(p.mb_height - 1);
    w.write_bit(true); // frame_mbs_only_flag
    w.write_bit(true); // direct_8x8_inference_flag
    let cropping = p.crop_right != 0 || p.crop_bottom != 0;
    w.write_bit(cropping);
    if cropping {
        // CropUnitX = 2 and CropUnitY = 2 for progressive 4:2:0 (7.4.2.1.1).
        w.write_ue(0);
        w.write_ue(p.crop_right);
        w.write_ue(0);
        w.write_ue(p.crop_bottom);
    }
    w.write_bit(true); // vui_parameters_present_flag
    w.write_bit(false); // aspect_ratio_info_present_flag
    w.write_bit(false); // overscan_info_present_flag
    w.write_bit(p.colour.is_some()); // video_signal_type_present_flag
    if let Some(colour) = p.colour {
        w.write_bits(5, 3); // video_format: unspecified
        w.write_bit(colour.full_range);
        w.write_bit(true); // colour_description_present_flag
        w.write_bits(u32::from(colour.primaries), 8);
        w.write_bits(u32::from(colour.transfer), 8);
        w.write_bits(u32::from(colour.matrix), 8);
    }
    w.write_bit(false); // chroma_loc_info_present_flag
    w.write_bit(true); // timing_info_present_flag
    w.write_bits(p.num_units_in_tick, 32);
    w.write_bits(p.time_scale, 32);
    w.write_bit(true); // fixed_frame_rate_flag
    w.write_bit(false); // nal_hrd_parameters_present_flag
    w.write_bit(false); // vcl_hrd_parameters_present_flag
    w.write_bit(false); // pic_struct_present_flag
    w.write_bit(false); // bitstream_restriction_flag
    trailing(&mut w);
    nal(PACKED_HEADER_SEQUENCE, &[0x67], &w.into_bytes(), 0)
}

/// The picture parameter set (7.3.2.2).
pub(super) fn h264_pps(p: &H264Params) -> Packed {
    let mut w = BitWriter::with_capacity(16);
    w.write_ue(0); // pic_parameter_set_id
    w.write_ue(0); // seq_parameter_set_id
    w.write_bit(true); // entropy_coding_mode_flag: CABAC
    w.write_bit(false); // bottom_field_pic_order_in_frame_present_flag
    w.write_ue(0); // num_slice_groups_minus1
    w.write_ue(0); // num_ref_idx_l0_default_active_minus1
    w.write_ue(0); // num_ref_idx_l1_default_active_minus1
    w.write_bit(false); // weighted_pred_flag
    w.write_bits(0, 2); // weighted_bipred_idc
    w.write_se(i32::from(p.pic_init_qp) - 26);
    w.write_se(0); // pic_init_qs_minus26
    w.write_se(0); // chroma_qp_index_offset
    w.write_bit(true); // deblocking_filter_control_present_flag
    w.write_bit(false); // constrained_intra_pred_flag
    w.write_bit(false); // redundant_pic_cnt_present_flag
    trailing(&mut w);
    nal(PACKED_HEADER_PICTURE, &[0x68], &w.into_bytes(), 0)
}

/// The slice header (7.3.3), up to but *not* including the slice data.
///
/// No trailing bits and no alignment: the driver continues the bitstream at the
/// next bit (`cabac_alignment_one_bit` is its business), which is why the
/// returned bit count has to be exact.
pub(super) fn h264_slice_header(idr: bool, frame_num: u32, idr_pic_id: u32) -> Packed {
    let mut w = BitWriter::with_capacity(16);
    w.write_ue(0); // first_mb_in_slice
    // 5..=9 assert that every slice of the picture has this type, which holds:
    // one slice per picture.
    w.write_ue(if idr { 7 } else { 5 });
    w.write_ue(0); // pic_parameter_set_id
    w.write_bits(frame_num, H264_LOG2_MAX_FRAME_NUM);
    if idr {
        w.write_ue(idr_pic_id);
    }
    // pic_order_cnt_type 2: no picture order count in the header.
    if !idr {
        w.write_bit(false); // num_ref_idx_active_override_flag
        w.write_bit(false); // ref_pic_list_modification_flag_l0
    }
    // Every picture here is a reference (nal_ref_idc != 0).
    if idr {
        w.write_bit(false); // no_output_of_prior_pics_flag
        w.write_bit(false); // long_term_reference_flag
    } else {
        w.write_bit(false); // adaptive_ref_pic_marking_mode_flag: sliding window
    }
    if !idr {
        w.write_ue(0); // cabac_init_idc
    }
    w.write_se(0); // slice_qp_delta, against pic_init_qp
    w.write_ue(0); // disable_deblocking_filter_idc
    w.write_se(0); // slice_alpha_c0_offset_div2
    w.write_se(0); // slice_beta_offset_div2
    let bits = w.bit_len() as u32;
    let padding = (8 - bits % 8) % 8;
    w.align_to_byte();
    let header = if idr { 0x65 } else { 0x61 };
    nal(PACKED_HEADER_SLICE, &[header], &w.into_bytes(), padding)
}

/// `rbsp_trailing_bits` (7.3.2.11).
fn trailing(w: &mut BitWriter) {
    w.write_bit(true);
    w.align_to_byte();
}

// ---------------------------------------------------------------------------
// HEVC
// ---------------------------------------------------------------------------

/// Build the HEVC parameter sets, and the slice header for one picture.
///
/// The syntax crate writes all four structures, so the encoder and the software
/// HEVC path in this family share one definition of what a parameter set is.
pub(super) mod hevc {
    use ec_h265_syntax::{
        ConformanceWindow, NalHeader, NalUnitType, Pps, ProfileTierLevel, ShortTermRefPicSet,
        SliceHeader, SliceType, Sps, Vps,
    };

    use super::{Colour, Packed, nal};
    use crate::params::enc::{PACKED_HEADER_PICTURE, PACKED_HEADER_SEQUENCE, PACKED_HEADER_SLICE};

    /// Geometry and timing for the parameter sets.
    pub(in crate::enc) struct Params {
        pub(in crate::enc) width: u32,
        pub(in crate::enc) height: u32,
        pub(in crate::enc) coded_width: u32,
        pub(in crate::enc) coded_height: u32,
        pub(in crate::enc) level_idc: u8,
        pub(in crate::enc) num_units_in_tick: u32,
        pub(in crate::enc) time_scale: u32,
        pub(in crate::enc) init_qp: u8,
        pub(in crate::enc) colour: Option<Colour>,
    }

    /// The VPS, SPS and PPS, in that order.
    pub(in crate::enc) fn parameter_sets(p: &Params) -> (Vps, Sps, Pps) {
        let ptl = ProfileTierLevel::main(p.level_idc);
        let vps = Vps {
            id: 0,
            ptl,
            max_dec_pic_buffering_minus1: 1,
            max_num_reorder_pics: 0,
        };
        let sps = Sps {
            vps_id: 0,
            id: 0,
            chroma_format_idc: 1,
            separate_colour_plane: false,
            pic_width_in_luma_samples: p.coded_width,
            pic_height_in_luma_samples: p.coded_height,
            // The conformance window is how HEVC crops: the coded size is a
            // multiple of the minimum coding block, the displayed size is not.
            conf_win: ConformanceWindow {
                left: 0,
                right: (p.coded_width - p.width) / 2,
                top: 0,
                bottom: (p.coded_height - p.height) / 2,
            },
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            log2_max_poc_lsb_minus4: 8,
            max_dec_pic_buffering_minus1: 1,
            max_num_reorder_pics: 0,
            log2_min_cb_size_minus3: 0,
            log2_diff_max_min_cb_size: 3,
            log2_min_tb_size_minus2: 0,
            log2_diff_max_min_tb_size: 3,
            max_transform_hierarchy_depth_inter: 1,
            max_transform_hierarchy_depth_intra: 1,
            scaling_list_enabled: false,
            amp_enabled: true,
            sao_enabled: true,
            pcm_enabled: false,
            pcm: None,
            // No sets in the sequence: this driver writes the reference picture
            // set out in each slice header (an inline `st_ref_pic_set` naming
            // the previous picture), so declaring one here would leave the two
            // descriptions of the same thing free to disagree.
            num_short_term_ref_pic_sets: 0,
            short_term_ref_pic_sets: Vec::new(),
            long_term_ref_pics_present: false,
            num_long_term_ref_pics_sps: 0,
            long_term_ref_pics_sps: Vec::new(),
            temporal_mvp_enabled: false,
            strong_intra_smoothing: true,
            ptl,
            vui: Some(ec_h265_syntax::vui::VuiParameters {
                timing: Some((p.num_units_in_tick, p.time_scale)),
                video_signal_type: p.colour.map(|colour| {
                    ec_h265_syntax::vui::VideoSignalType {
                        video_format: 5, // unspecified
                        video_full_range_flag: colour.full_range,
                        colour_description: Some(ec_h265_syntax::vui::ColourDescription {
                            colour_primaries: colour.primaries,
                            transfer_characteristics: colour.transfer,
                            matrix_coeffs: colour.matrix,
                        }),
                    }
                }),
                ..ec_h265_syntax::vui::VuiParameters::default()
            }),
        };
        let pps = Pps {
            id: 0,
            sps_id: 0,
            init_qp_minus26: i32::from(p.init_qp) - 26,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            transform_skip_enabled: true,
            loop_filter_across_tiles_enabled: false,
            loop_filter_across_slices_enabled: false,
            ..Pps::default()
        };
        (vps, sps, pps)
    }

    /// `st_ref_pic_set()`: one negative reference at POC - 1, used by this
    /// picture — the single reference this encoder keeps.
    fn previous_picture_set() -> ShortTermRefPicSet {
        let mut set = ShortTermRefPicSet {
            num_negative: 1,
            num_positive: 0,
            num_used_by_curr: 1,
            ..ShortTermRefPicSet::default()
        };
        set.delta_poc_s0[0] = -1;
        set.used_s0[0] = true;
        set
    }

    pub(in crate::enc) fn packed_vps(vps: &Vps) -> Packed {
        nal(
            PACKED_HEADER_SEQUENCE,
            &NalHeader::new(NalUnitType::Vps).to_bytes(),
            &vps.to_rbsp(),
            0,
        )
    }

    pub(in crate::enc) fn packed_sps(sps: &Sps) -> Packed {
        nal(
            PACKED_HEADER_SEQUENCE,
            &NalHeader::new(NalUnitType::Sps).to_bytes(),
            &sps.to_rbsp(),
            0,
        )
    }

    pub(in crate::enc) fn packed_pps(pps: &Pps) -> Packed {
        nal(
            PACKED_HEADER_PICTURE,
            &NalHeader::new(NalUnitType::Pps).to_bytes(),
            &pps.to_rbsp(),
            0,
        )
    }

    /// The slice segment header, ending mid-byte like H.264's.
    pub(in crate::enc) fn packed_slice(sps: &Sps, pps: &Pps, keyframe: bool, poc: i32) -> Packed {
        let nal_type = if keyframe {
            NalUnitType::IdrWRadl
        } else {
            NalUnitType::TrailR
        };
        let mut header = SliceHeader::intra(pps, 0);
        header.slice_type = if keyframe { SliceType::I } else { SliceType::P };
        header.poc_lsb = (poc as u32) & ((1 << (sps.log2_max_poc_lsb_minus4 + 4)) - 1);
        header.sao_luma = sps.sao_enabled;
        header.sao_chroma = sps.sao_enabled;
        header.five_minus_max_num_merge_cand = 0;
        // The set goes in the header rather than the SPS, which is what this
        // driver writes when it composes the header itself.
        header.short_term_ref_pic_set_sps_flag = false;
        header.short_term_ref_pic_set = previous_picture_set();
        header.loop_filter_across_slices_enabled = false;
        let mut w = ec_core::BitWriter::with_capacity(16);
        // With `byte_alignment()` (7.3.6.1): this driver copies the header it is
        // given verbatim and starts the slice data at the next byte, so the
        // alignment bit has to be in what it is given.
        header.write(&mut w, sps, pps, nal_type);
        nal(
            PACKED_HEADER_SLICE,
            &NalHeader::new(nal_type).to_bytes(),
            &w.into_bytes(),
            0,
        )
    }
}

// ---------------------------------------------------------------------------
// AV1
// ---------------------------------------------------------------------------

/// The two OBUs the AV1 encode path hands the driver: a sequence header and,
/// per picture, an uncompressed frame header.
///
/// radeonsi *parses* these (`picture_av1_enc.c:vlVaHandleVAEncPackedHeaderData
/// BufferTypeAV1`) rather than copying them through — the sequence header in
/// full, the frame header as far as `uniform_tile_spacing_flag` — and then
/// writes the header that goes into the stream itself, from those values plus
/// the picture parameter buffer. So these are the *parameters*, in AV1's own
/// syntax: a field written here that disagrees with the parameter buffer is a
/// picture described two ways, and the driver believes this one.
pub(super) mod av1 {
    use ec_core::BitWriter;

    use super::{Colour, Packed};
    use crate::params::enc::{PACKED_HEADER_PICTURE, PACKED_HEADER_SEQUENCE};

    /// `OrderHintBits`, and the width of `order_hint` in a frame header.
    pub(in crate::enc) const ORDER_HINT_BITS: u32 = 8;

    /// Bytes of the `obu_size` field. radeonsi reports
    /// `VAConfigAttribEncAV1Ext2.obu_size_bytes_minus1 = 1` (measured on this
    /// box), i.e. it wants that field a fixed two bytes so it can rewrite it
    /// without moving the payload; leb128 permits the padded encoding.
    const OBU_SIZE_BYTES: usize = 2;

    /// Wrap a payload as one OBU: header byte, fixed-width `obu_size`, payload.
    fn obu(kind: u32, obu_type: u8, payload: Vec<u8>) -> Packed {
        let mut bytes = Vec::with_capacity(payload.len() + 1 + OBU_SIZE_BYTES);
        // forbidden(0) type(4) extension(0) has_size(1) reserved(0)
        bytes.push(obu_type << 3 | 0b010);
        let mut size = payload.len();
        for i in 0..OBU_SIZE_BYTES {
            let last = i + 1 == OBU_SIZE_BYTES;
            bytes.push((size as u8 & 0x7f) | if last { 0 } else { 0x80 });
            size >>= 7;
        }
        bytes.extend_from_slice(&payload);
        let bits = bytes.len() as u32 * 8;
        Packed { kind, bytes, bits }
    }

    /// `trailing_bits()` (5.3.4).
    fn trailing(w: &mut BitWriter) {
        w.write_bit(true);
        w.align_to_byte();
    }

    /// What the sequence header says about the stream.
    pub(in crate::enc) struct SeqParams {
        pub(in crate::enc) width: u32,
        pub(in crate::enc) height: u32,
        pub(in crate::enc) seq_level_idx: u8,
        pub(in crate::enc) seq_tier: u8,
        /// 10-bit (`high_bitdepth`); profile 0 has no `twelve_bit`.
        pub(in crate::enc) high_bitdepth: bool,
        pub(in crate::enc) colour: Option<Colour>,
    }

    /// `sequence_header_obu()` (5.5.1), profile 0, one operating point.
    ///
    /// Every coding tool this driver does not advertise
    /// (`VAConfigAttribEncAV1`: no filter intra, no warped motion, no dual
    /// filter, no superres, no restoration) is signalled off, which is also
    /// what ffmpeg's `av1_vaapi` writes.
    pub(in crate::enc) fn sequence_header(p: &SeqParams) -> Packed {
        let mut w = BitWriter::new();
        w.write_bits(0, 3); // seq_profile
        w.write_bit(false); // still_picture
        w.write_bit(false); // reduced_still_picture_header
        w.write_bit(false); // timing_info_present_flag
        w.write_bit(false); // initial_display_delay_present_flag
        w.write_bits(0, 5); // operating_points_cnt_minus_1
        w.write_bits(0, 12); // operating_point_idc[0]
        w.write_bits(u32::from(p.seq_level_idx), 5);
        if p.seq_level_idx > 7 {
            w.write_bits(u32::from(p.seq_tier), 1);
        }
        let width_bits = 32 - p.width.max(1).leading_zeros(); // floor(log2(w)) + 1
        let height_bits = 32 - p.height.max(1).leading_zeros();
        w.write_bits(width_bits - 1, 4); // frame_width_bits_minus_1
        w.write_bits(height_bits - 1, 4); // frame_height_bits_minus_1
        w.write_bits(p.width - 1, width_bits); // max_frame_width_minus_1
        w.write_bits(p.height - 1, height_bits); // max_frame_height_minus_1
        w.write_bit(false); // frame_id_numbers_present_flag
        w.write_bit(false); // use_128x128_superblock
        w.write_bit(false); // enable_filter_intra
        w.write_bit(false); // enable_intra_edge_filter
        w.write_bit(false); // enable_interintra_compound
        w.write_bit(false); // enable_masked_compound
        w.write_bit(false); // enable_warped_motion
        w.write_bit(false); // enable_dual_filter
        w.write_bit(true); // enable_order_hint
        w.write_bit(false); // enable_jnt_comp
        w.write_bit(false); // enable_ref_frame_mvs
        w.write_bit(false); // seq_choose_screen_content_tools
        w.write_bit(false); // seq_force_screen_content_tools
        w.write_bits(ORDER_HINT_BITS - 1, 3); // order_hint_bits_minus_1
        w.write_bit(false); // enable_superres
        w.write_bit(false); // enable_cdef
        w.write_bit(false); // enable_restoration
        // color_config(): profile 0 is 4:2:0, and never 12-bit.
        w.write_bit(p.high_bitdepth);
        w.write_bit(false); // mono_chrome
        match p.colour {
            Some(c) => {
                w.write_bit(true); // color_description_present_flag
                w.write_bits(u32::from(c.primaries), 8);
                w.write_bits(u32::from(c.transfer), 8);
                w.write_bits(u32::from(c.matrix), 8);
                w.write_bit(c.full_range); // color_range
            }
            None => {
                w.write_bit(false);
                w.write_bit(false); // color_range
            }
        }
        w.write_bits(0, 2); // chroma_sample_position = CSP_UNKNOWN
        w.write_bit(false); // separate_uv_delta_q
        w.write_bit(false); // film_grain_params_present
        trailing(&mut w);
        obu(PACKED_HEADER_SEQUENCE, 1, w.into_bytes())
    }

    /// What one picture's uncompressed header says.
    pub(in crate::enc) struct FrameParams {
        pub(in crate::enc) keyframe: bool,
        pub(in crate::enc) order_hint: u8,
        pub(in crate::enc) primary_ref_frame: u8,
        pub(in crate::enc) refresh_frame_flags: u8,
        pub(in crate::enc) ref_frame_idx: [u8; 7],
        pub(in crate::enc) base_q_idx: u8,
        pub(in crate::enc) loop_filter_level: [u8; 4],
        /// `TX_MODE_SELECT`; this driver advertises no other
        /// (`VAConfigAttribEncAV1Ext2.tx_mode_support = 0x4`).
        pub(in crate::enc) tx_mode_select: bool,
        /// The bit offsets the driver needs to back-annotate its own choices
        /// into this header are only read outside CQP, so they are computed
        /// unconditionally and used by the caller when it is not in CQP.
        pub(in crate::enc) max_tile_cols_log2: u32,
        pub(in crate::enc) max_tile_rows_log2: u32,
    }

    /// Where the driver may rewrite fields of the header we just wrote, in
    /// bits from the first bit of the OBU (`VAEncPictureParameterBufferAV1`).
    pub(in crate::enc) struct FrameHeaderOffsets {
        pub(in crate::enc) qindex: u32,
        pub(in crate::enc) segmentation: u32,
        pub(in crate::enc) loop_filter: u32,
        pub(in crate::enc) cdef: u32,
        pub(in crate::enc) cdef_size: u32,
        pub(in crate::enc) obu_size_byte: u32,
        pub(in crate::enc) total_bits: u32,
    }

    /// `frame_header_obu()` (5.9.1) for a shown key or inter frame with one
    /// tile, no segmentation, no CDEF, no restoration and no global motion.
    pub(in crate::enc) fn frame_header(p: &FrameParams) -> (Packed, FrameHeaderOffsets) {
        // The OBU header and its size field precede every offset below.
        let prefix_bits = 8 * (1 + OBU_SIZE_BYTES) as u32;
        let mut w = BitWriter::new();
        w.write_bit(false); // show_existing_frame
        w.write_bits(u32::from(!p.keyframe), 2); // frame_type: KEY / INTER
        w.write_bit(true); // show_frame
        if !p.keyframe {
            // A shown key frame infers error_resilient_mode = 1.
            w.write_bit(false); // error_resilient_mode
        }
        w.write_bit(false); // disable_cdf_update
        w.write_bit(false); // frame_size_override_flag
        w.write_bits(u32::from(p.order_hint), ORDER_HINT_BITS);
        if !p.keyframe {
            w.write_bits(u32::from(p.primary_ref_frame), 3);
            // A shown key frame refreshes every slot, unsignalled.
            w.write_bits(u32::from(p.refresh_frame_flags), 8);
            w.write_bit(false); // frame_refs_short_signaling
            for idx in p.ref_frame_idx {
                w.write_bits(u32::from(idx), 3);
            }
        }
        // frame_size(): frame_size_override_flag = 0 takes the sequence
        // header's size, and enable_superres = 0 codes no superres bit.
        w.write_bit(false); // render_and_frame_size_different
        if !p.keyframe {
            w.write_bit(false); // allow_high_precision_mv
            w.write_bit(true); // is_filter_switchable
            w.write_bit(false); // is_motion_mode_switchable
            // use_ref_frame_mvs is inferred 0 from enable_ref_frame_mvs = 0.
        }
        w.write_bit(false); // disable_frame_end_update_cdf
        // tile_info(): one tile, uniformly spaced. The increment loop stops
        // with a zero bit for as long as the maximum allows another doubling.
        w.write_bit(true); // uniform_tile_spacing_flag
        if p.max_tile_cols_log2 > 0 {
            w.write_bit(false); // increment_tile_cols_log2
        }
        if p.max_tile_rows_log2 > 0 {
            w.write_bit(false); // increment_tile_rows_log2
        }
        let qindex = prefix_bits + w.bit_len() as u32;
        w.write_bits(u32::from(p.base_q_idx), 8);
        w.write_bit(false); // DeltaQYDc.delta_coded
        w.write_bit(false); // DeltaQUDc.delta_coded
        w.write_bit(false); // DeltaQUAc.delta_coded
        w.write_bit(false); // using_qmatrix
        let segmentation = prefix_bits + w.bit_len() as u32;
        w.write_bit(false); // segmentation_enabled
        w.write_bit(false); // delta_q_present (base_q_idx > 0)
        let loop_filter = prefix_bits + w.bit_len() as u32;
        w.write_bits(u32::from(p.loop_filter_level[0]), 6);
        w.write_bits(u32::from(p.loop_filter_level[1]), 6);
        if p.loop_filter_level[0] != 0 || p.loop_filter_level[1] != 0 {
            w.write_bits(u32::from(p.loop_filter_level[2]), 6);
            w.write_bits(u32::from(p.loop_filter_level[3]), 6);
        }
        w.write_bits(0, 3); // loop_filter_sharpness
        w.write_bit(false); // loop_filter_delta_enabled
        // cdef_params() and lr_params() are empty: enable_cdef =
        // enable_restoration = 0 in the sequence header above.
        let cdef = prefix_bits + w.bit_len() as u32;
        w.write_bit(p.tx_mode_select); // tx_mode_select
        if !p.keyframe {
            w.write_bit(false); // reference_select — single reference, which
            // also makes skipModeAllowed 0, so no skip_mode_present bit
        }
        w.write_bit(false); // reduced_tx_set
        if !p.keyframe {
            for _ in 0..7 {
                w.write_bit(false); // is_global[ref]
            }
        }
        trailing(&mut w);
        let packed = obu(PACKED_HEADER_PICTURE, 3, w.into_bytes());
        let offsets = FrameHeaderOffsets {
            qindex,
            segmentation,
            loop_filter,
            cdef,
            cdef_size: 0,
            obu_size_byte: 1,
            total_bits: packed.bits,
        };
        (packed, offsets)
    }

    /// `tile_log2(1, n)`: the number of doublings of 1 needed to reach `n`,
    /// which is the ceiling of log2 (5.9.15).
    pub(in crate::enc) fn tile_log2(target: u32) -> u32 {
        let mut k = 0;
        while (1u32 << k) < target {
            k += 1;
        }
        k
    }
}

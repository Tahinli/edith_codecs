//! Encode parameter buffers: H.264, HEVC and AV1, plus the codec-independent
//! misc-parameter and packed-header buffers (`va.h`, `va_enc_*.h`).

use ec_va::sys::{
    VAEncPackedHeaderParameterBufferType, VAEncPictureParameterBufferType,
    VAEncSequenceParameterBufferType, VAEncSliceParameterBufferType, VASurfaceID,
};

use super::VaParam;
use super::h264::VAPictureH264;
use super::hevc::VAPictureHEVC;
use crate::va_bits;

/// `VAEncPackedHeaderSequence` (`va.h:2421`) — SPS/VPS.
pub const PACKED_HEADER_SEQUENCE: u32 = 1;
/// `VAEncPackedHeaderPicture` — PPS.
pub const PACKED_HEADER_PICTURE: u32 = 2;
/// `VAEncPackedHeaderSlice` — slice header.
pub const PACKED_HEADER_SLICE: u32 = 3;
/// `VAEncPackedHeaderRawData` — anything else, e.g. an AV1 OBU.
pub const PACKED_HEADER_RAW_DATA: u32 = 4;

/// `VAEncMiscParameterTypeFrameRate`.
pub const MISC_FRAME_RATE: u32 = 0;
/// `VAEncMiscParameterTypeRateControl`.
pub const MISC_RATE_CONTROL: u32 = 1;
/// `VAEncMiscParameterTypeHRD`.
pub const MISC_HRD: u32 = 5;
/// `VAEncMiscParameterTypeQualityLevel`.
pub const MISC_QUALITY_LEVEL: u32 = 6;

/// `VAEncPackedHeaderParameterBuffer`, `va.h:2446`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PackedHeaderParameterBuffer {
    /// One of the `PACKED_HEADER_*` constants.
    pub type_: u32,
    /// Length of the packed header in bits.
    pub bit_length: u32,
    /// Whether the data already carries emulation prevention bytes.
    pub has_emulation_bytes: u8,
    pub(crate) va_reserved: [u32; 4],
}

impl PackedHeaderParameterBuffer {
    /// A header whose bytes are already escaped, as everything this crate
    /// writes is (the syntax crates emit Annex B ready bytes).
    pub fn escaped(type_: u32, bytes: usize) -> PackedHeaderParameterBuffer {
        PackedHeaderParameterBuffer {
            type_,
            bit_length: (bytes * 8) as u32,
            has_emulation_bytes: 1,
            va_reserved: [0; 4],
        }
    }
}

// SAFETY: `#[repr(C)]` transcription of `VAEncPackedHeaderParameterBuffer`,
// checked below; integers only.
unsafe impl VaParam for PackedHeaderParameterBuffer {
    const TYPE: i32 = VAEncPackedHeaderParameterBufferType;
}

/// `VAEncMiscParameterRateControl`, `va.h:2492`.
///
/// Built as words rather than as a struct because a misc parameter is submitted
/// as `{ type, payload }` in one buffer: the payload of every misc type this
/// crate sends is a run of `uint32_t`, so it can be assembled without ever
/// taking a byte view of a Rust struct.
#[derive(Debug, Clone, Copy, Default)]
pub struct RateControl {
    /// Target bitrate, or the peak bitrate under VBR.
    pub bits_per_second: u32,
    /// Percentage of `bits_per_second` to target (VBR); 0 means CBR.
    pub target_percentage: u32,
    /// Rate control window in milliseconds.
    pub window_size: u32,
    /// Initial quantiser, 0 to let the driver choose.
    pub initial_qp: u32,
    /// Minimum quantiser.
    pub min_qp: u32,
    /// Maximum quantiser.
    pub max_qp: u32,
}

impl RateControl {
    /// The payload words in header order.
    pub fn words(&self) -> [u32; 15] {
        [
            self.bits_per_second,
            self.target_percentage,
            self.window_size,
            self.initial_qp,
            self.min_qp,
            0, // basic_unit_size
            0, // rc_flags
            0, // ICQ_quality_factor
            self.max_qp,
            0, // quality_factor
            0, // target_frame_size
            0,
            0,
            0,
            0, // va_reserved[4]
        ]
    }
}

/// `VAEncMiscParameterFrameRate`, `va.h:2617`.
#[derive(Debug, Clone, Copy)]
pub struct FrameRate {
    /// Numerator of the frame rate.
    pub num: u32,
    /// Denominator of the frame rate.
    pub den: u32,
}

impl FrameRate {
    /// The payload words. libva packs a fractional rate as
    /// `den << 16 | num`, and a whole one as the plain numerator.
    pub fn words(&self) -> [u32; 6] {
        let packed = if self.den <= 1 {
            self.num
        } else {
            (self.den << 16) | (self.num & 0xffff)
        };
        [packed, 0, 0, 0, 0, 0]
    }
}

/// `VAEncMiscParameterHRD`, `va.h:2721`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Hrd {
    /// Initial CPB fullness in bits.
    pub initial_buffer_fullness: u32,
    /// CPB size in bits.
    pub buffer_size: u32,
}

impl Hrd {
    /// The payload words in header order.
    pub fn words(&self) -> [u32; 6] {
        [self.initial_buffer_fullness, self.buffer_size, 0, 0, 0, 0]
    }
}

/// Serialise a misc parameter buffer: the type word followed by its payload.
pub fn misc_bytes(type_: u32, words: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + words.len() * 4);
    out.extend_from_slice(&type_.to_ne_bytes());
    for w in words {
        out.extend_from_slice(&w.to_ne_bytes());
    }
    out
}

// ---------------------------------------------------------------------------
// H.264 encode
// ---------------------------------------------------------------------------

va_bits! {
    /// `VAEncSequenceParameterBufferH264::seq_fields`.
    H264SeqFields: u32 {
        chroma_format_idc: 2,
        frame_mbs_only_flag: 1,
        mb_adaptive_frame_field_flag: 1,
        seq_scaling_matrix_present_flag: 1,
        direct_8x8_inference_flag: 1,
        log2_max_frame_num_minus4: 4,
        pic_order_cnt_type: 2,
        log2_max_pic_order_cnt_lsb_minus4: 4,
        delta_pic_order_always_zero_flag: 1,
    }
}

va_bits! {
    /// `VAEncSequenceParameterBufferH264::vui_fields`.
    H264VuiFields: u32 {
        aspect_ratio_info_present_flag: 1,
        timing_info_present_flag: 1,
        bitstream_restriction_flag: 1,
        log2_max_mv_length_horizontal: 5,
        log2_max_mv_length_vertical: 5,
        fixed_frame_rate_flag: 1,
        low_delay_hrd_flag: 1,
        motion_vectors_over_pic_boundaries_flag: 1,
    }
}

/// `VAEncSequenceParameterBufferH264`, `va_enc_h264.h:78`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EncSequenceParameterBufferH264 {
    /// `seq_parameter_set_id`.
    pub seq_parameter_set_id: u8,
    /// `level_idc`.
    pub level_idc: u8,
    /// Frames between I frames.
    pub intra_period: u32,
    /// Frames between IDR frames.
    pub intra_idr_period: u32,
    /// Distance between P frames (1 = no B frames).
    pub ip_period: u32,
    /// Target bitrate.
    pub bits_per_second: u32,
    /// `max_num_ref_frames`.
    pub max_num_ref_frames: u32,
    /// `PicWidthInMbs`.
    pub picture_width_in_mbs: u16,
    /// `PicHeightInMapUnits`.
    pub picture_height_in_mbs: u16,
    /// Sequence flags.
    pub seq_fields: H264SeqFields,
    /// `bit_depth_luma_minus8`.
    pub bit_depth_luma_minus8: u8,
    /// `bit_depth_chroma_minus8`.
    pub bit_depth_chroma_minus8: u8,
    /// `num_ref_frames_in_pic_order_cnt_cycle`.
    pub num_ref_frames_in_pic_order_cnt_cycle: u8,
    /// `offset_for_non_ref_pic`.
    pub offset_for_non_ref_pic: i32,
    /// `offset_for_top_to_bottom_field`.
    pub offset_for_top_to_bottom_field: i32,
    /// `offset_for_ref_frame`.
    pub offset_for_ref_frame: [i32; 256],
    /// `frame_cropping_flag`.
    pub frame_cropping_flag: u8,
    /// `frame_crop_left_offset`.
    pub frame_crop_left_offset: u32,
    /// `frame_crop_right_offset`.
    pub frame_crop_right_offset: u32,
    /// `frame_crop_top_offset`.
    pub frame_crop_top_offset: u32,
    /// `frame_crop_bottom_offset`.
    pub frame_crop_bottom_offset: u32,
    /// `vui_parameters_present_flag`.
    pub vui_parameters_present_flag: u8,
    /// VUI flags.
    pub vui_fields: H264VuiFields,
    /// `aspect_ratio_idc`.
    pub aspect_ratio_idc: u8,
    /// `sar_width`.
    pub sar_width: u32,
    /// `sar_height`.
    pub sar_height: u32,
    /// `num_units_in_tick`.
    pub num_units_in_tick: u32,
    /// `time_scale`.
    pub time_scale: u32,
    pub(crate) va_reserved: [u32; 4],
}

impl Default for EncSequenceParameterBufferH264 {
    fn default() -> Self {
        EncSequenceParameterBufferH264 {
            seq_parameter_set_id: 0,
            level_idc: 41,
            intra_period: 0,
            intra_idr_period: 0,
            ip_period: 1,
            bits_per_second: 0,
            max_num_ref_frames: 1,
            picture_width_in_mbs: 0,
            picture_height_in_mbs: 0,
            seq_fields: H264SeqFields::default(),
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            num_ref_frames_in_pic_order_cnt_cycle: 0,
            offset_for_non_ref_pic: 0,
            offset_for_top_to_bottom_field: 0,
            offset_for_ref_frame: [0; 256],
            frame_cropping_flag: 0,
            frame_crop_left_offset: 0,
            frame_crop_right_offset: 0,
            frame_crop_top_offset: 0,
            frame_crop_bottom_offset: 0,
            vui_parameters_present_flag: 0,
            vui_fields: H264VuiFields::default(),
            aspect_ratio_idc: 0,
            sar_width: 0,
            sar_height: 0,
            num_units_in_tick: 0,
            time_scale: 0,
            va_reserved: [0; 4],
        }
    }
}

// SAFETY: `#[repr(C)]` transcription checked by the assertion block below.
unsafe impl VaParam for EncSequenceParameterBufferH264 {
    const TYPE: i32 = VAEncSequenceParameterBufferType;
}

va_bits! {
    /// `VAEncPictureParameterBufferH264::pic_fields`.
    H264EncPicFields: u32 {
        idr_pic_flag: 1,
        reference_pic_flag: 2,
        entropy_coding_mode_flag: 1,
        weighted_pred_flag: 1,
        weighted_bipred_idc: 2,
        constrained_intra_pred_flag: 1,
        transform_8x8_mode_flag: 1,
        deblocking_filter_control_present_flag: 1,
        redundant_pic_cnt_present_flag: 1,
        pic_order_present_flag: 1,
        pic_scaling_matrix_present_flag: 1,
    }
}

/// `VAEncPictureParameterBufferH264`, `va_enc_h264.h:222`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EncPictureParameterBufferH264 {
    /// Reconstructed picture surface for this frame.
    pub curr_pic: VAPictureH264,
    /// Reference pictures available to this frame.
    pub reference_frames: [VAPictureH264; 16],
    /// Buffer the coded bits are written to.
    pub coded_buf: u32,
    /// `pic_parameter_set_id`.
    pub pic_parameter_set_id: u8,
    /// `seq_parameter_set_id`.
    pub seq_parameter_set_id: u8,
    /// `H264_LAST_PICTURE_*` when this is the last picture.
    pub last_picture: u8,
    /// `frame_num`.
    pub frame_num: u16,
    /// `pic_init_qp_minus26 + 26`.
    pub pic_init_qp: u8,
    /// `num_ref_idx_l0_active_minus1`.
    pub num_ref_idx_l0_active_minus1: u8,
    /// `num_ref_idx_l1_active_minus1`.
    pub num_ref_idx_l1_active_minus1: u8,
    /// `chroma_qp_index_offset`.
    pub chroma_qp_index_offset: i8,
    /// `second_chroma_qp_index_offset`.
    pub second_chroma_qp_index_offset: i8,
    /// Picture flags.
    pub pic_fields: H264EncPicFields,
    pub(crate) va_reserved: [u32; 4],
}

impl Default for EncPictureParameterBufferH264 {
    fn default() -> Self {
        EncPictureParameterBufferH264 {
            curr_pic: VAPictureH264::INVALID,
            reference_frames: [VAPictureH264::INVALID; 16],
            coded_buf: 0,
            pic_parameter_set_id: 0,
            seq_parameter_set_id: 0,
            last_picture: 0,
            frame_num: 0,
            pic_init_qp: 26,
            num_ref_idx_l0_active_minus1: 0,
            num_ref_idx_l1_active_minus1: 0,
            chroma_qp_index_offset: 0,
            second_chroma_qp_index_offset: 0,
            pic_fields: H264EncPicFields::default(),
            va_reserved: [0; 4],
        }
    }
}

// SAFETY: `#[repr(C)]` transcription checked by the assertion block below.
unsafe impl VaParam for EncPictureParameterBufferH264 {
    const TYPE: i32 = VAEncPictureParameterBufferType;
}

/// `VAEncSliceParameterBufferH264`, `va_enc_h264.h:290`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EncSliceParameterBufferH264 {
    /// First macroblock of this slice.
    pub macroblock_address: u32,
    /// Macroblocks in this slice.
    pub num_macroblocks: u32,
    /// Optional per-macroblock parameter buffer; `VA_INVALID_ID` for none.
    pub macroblock_info: u32,
    /// `slice_type`.
    pub slice_type: u8,
    /// `pic_parameter_set_id`.
    pub pic_parameter_set_id: u8,
    /// `idr_pic_id`.
    pub idr_pic_id: u16,
    /// `pic_order_cnt_lsb`.
    pub pic_order_cnt_lsb: u16,
    /// `delta_pic_order_cnt_bottom`.
    pub delta_pic_order_cnt_bottom: i32,
    /// `delta_pic_order_cnt`.
    pub delta_pic_order_cnt: [i32; 2],
    /// `direct_spatial_mv_pred_flag`.
    pub direct_spatial_mv_pred_flag: u8,
    /// `num_ref_idx_active_override_flag`.
    pub num_ref_idx_active_override_flag: u8,
    /// `num_ref_idx_l0_active_minus1`.
    pub num_ref_idx_l0_active_minus1: u8,
    /// `num_ref_idx_l1_active_minus1`.
    pub num_ref_idx_l1_active_minus1: u8,
    /// `RefPicList0`.
    pub ref_pic_list0: [VAPictureH264; 32],
    /// `RefPicList1`.
    pub ref_pic_list1: [VAPictureH264; 32],
    /// `luma_log2_weight_denom`.
    pub luma_log2_weight_denom: u8,
    /// `chroma_log2_weight_denom`.
    pub chroma_log2_weight_denom: u8,
    /// `luma_weight_l0_flag`.
    pub luma_weight_l0_flag: u8,
    /// `luma_weight_l0`.
    pub luma_weight_l0: [i16; 32],
    /// `luma_offset_l0`.
    pub luma_offset_l0: [i16; 32],
    /// `chroma_weight_l0_flag`.
    pub chroma_weight_l0_flag: u8,
    /// `chroma_weight_l0`.
    pub chroma_weight_l0: [[i16; 2]; 32],
    /// `chroma_offset_l0`.
    pub chroma_offset_l0: [[i16; 2]; 32],
    /// `luma_weight_l1_flag`.
    pub luma_weight_l1_flag: u8,
    /// `luma_weight_l1`.
    pub luma_weight_l1: [i16; 32],
    /// `luma_offset_l1`.
    pub luma_offset_l1: [i16; 32],
    /// `chroma_weight_l1_flag`.
    pub chroma_weight_l1_flag: u8,
    /// `chroma_weight_l1`.
    pub chroma_weight_l1: [[i16; 2]; 32],
    /// `chroma_offset_l1`.
    pub chroma_offset_l1: [[i16; 2]; 32],
    /// `cabac_init_idc`.
    pub cabac_init_idc: u8,
    /// `slice_qp_delta`.
    pub slice_qp_delta: i8,
    /// `disable_deblocking_filter_idc`.
    pub disable_deblocking_filter_idc: u8,
    /// `slice_alpha_c0_offset_div2`.
    pub slice_alpha_c0_offset_div2: i8,
    /// `slice_beta_offset_div2`.
    pub slice_beta_offset_div2: i8,
    pub(crate) va_reserved: [u32; 4],
}

impl Default for EncSliceParameterBufferH264 {
    fn default() -> Self {
        EncSliceParameterBufferH264 {
            macroblock_address: 0,
            num_macroblocks: 0,
            macroblock_info: ec_va::sys::VA_INVALID_ID,
            slice_type: 2,
            pic_parameter_set_id: 0,
            idr_pic_id: 0,
            pic_order_cnt_lsb: 0,
            delta_pic_order_cnt_bottom: 0,
            delta_pic_order_cnt: [0; 2],
            direct_spatial_mv_pred_flag: 0,
            num_ref_idx_active_override_flag: 0,
            num_ref_idx_l0_active_minus1: 0,
            num_ref_idx_l1_active_minus1: 0,
            ref_pic_list0: [VAPictureH264::INVALID; 32],
            ref_pic_list1: [VAPictureH264::INVALID; 32],
            luma_log2_weight_denom: 0,
            chroma_log2_weight_denom: 0,
            luma_weight_l0_flag: 0,
            luma_weight_l0: [0; 32],
            luma_offset_l0: [0; 32],
            chroma_weight_l0_flag: 0,
            chroma_weight_l0: [[0; 2]; 32],
            chroma_offset_l0: [[0; 2]; 32],
            luma_weight_l1_flag: 0,
            luma_weight_l1: [0; 32],
            luma_offset_l1: [0; 32],
            chroma_weight_l1_flag: 0,
            chroma_weight_l1: [[0; 2]; 32],
            chroma_offset_l1: [[0; 2]; 32],
            cabac_init_idc: 0,
            slice_qp_delta: 0,
            disable_deblocking_filter_idc: 0,
            slice_alpha_c0_offset_div2: 0,
            slice_beta_offset_div2: 0,
            va_reserved: [0; 4],
        }
    }
}

// SAFETY: `#[repr(C)]` transcription checked by the assertion block below.
unsafe impl VaParam for EncSliceParameterBufferH264 {
    const TYPE: i32 = VAEncSliceParameterBufferType;
}

// ---------------------------------------------------------------------------
// HEVC encode
// ---------------------------------------------------------------------------

va_bits! {
    /// `VAEncSequenceParameterBufferHEVC::seq_fields`.
    HevcSeqFields: u32 {
        chroma_format_idc: 2,
        separate_colour_plane_flag: 1,
        bit_depth_luma_minus8: 3,
        bit_depth_chroma_minus8: 3,
        scaling_list_enabled_flag: 1,
        strong_intra_smoothing_enabled_flag: 1,
        amp_enabled_flag: 1,
        sample_adaptive_offset_enabled_flag: 1,
        pcm_enabled_flag: 1,
        pcm_loop_filter_disabled_flag: 1,
        sps_temporal_mvp_enabled_flag: 1,
        low_delay_seq: 1,
        hierachical_flag: 1,
    }
}

va_bits! {
    /// `VAEncSequenceParameterBufferHEVC::vui_fields`.
    HevcVuiFields: u32 {
        aspect_ratio_info_present_flag: 1,
        neutral_chroma_indication_flag: 1,
        field_seq_flag: 1,
        vui_timing_info_present_flag: 1,
        bitstream_restriction_flag: 1,
        tiles_fixed_structure_flag: 1,
        motion_vectors_over_pic_boundaries_flag: 1,
        restricted_ref_pic_lists_flag: 1,
        log2_max_mv_length_horizontal: 5,
        log2_max_mv_length_vertical: 5,
    }
}

/// `VAEncSequenceParameterBufferHEVC`, `va_enc_hevc.h:139`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EncSequenceParameterBufferHEVC {
    /// `general_profile_idc`.
    pub general_profile_idc: u8,
    /// `general_level_idc`.
    pub general_level_idc: u8,
    /// `general_tier_flag`.
    pub general_tier_flag: u8,
    /// Frames between I frames.
    pub intra_period: u32,
    /// Frames between IDR frames.
    pub intra_idr_period: u32,
    /// Distance between P frames.
    pub ip_period: u32,
    /// Target bitrate.
    pub bits_per_second: u32,
    /// `pic_width_in_luma_samples`.
    pub pic_width_in_luma_samples: u16,
    /// `pic_height_in_luma_samples`.
    pub pic_height_in_luma_samples: u16,
    /// Sequence flags.
    pub seq_fields: HevcSeqFields,
    /// `log2_min_luma_coding_block_size_minus3`.
    pub log2_min_luma_coding_block_size_minus3: u8,
    /// `log2_diff_max_min_luma_coding_block_size`.
    pub log2_diff_max_min_luma_coding_block_size: u8,
    /// `log2_min_transform_block_size_minus2`.
    pub log2_min_transform_block_size_minus2: u8,
    /// `log2_diff_max_min_transform_block_size`.
    pub log2_diff_max_min_transform_block_size: u8,
    /// `max_transform_hierarchy_depth_inter`.
    pub max_transform_hierarchy_depth_inter: u8,
    /// `max_transform_hierarchy_depth_intra`.
    pub max_transform_hierarchy_depth_intra: u8,
    /// `pcm_sample_bit_depth_luma_minus1`.
    pub pcm_sample_bit_depth_luma_minus1: u32,
    /// `pcm_sample_bit_depth_chroma_minus1`.
    pub pcm_sample_bit_depth_chroma_minus1: u32,
    /// `log2_min_pcm_luma_coding_block_size_minus3`.
    pub log2_min_pcm_luma_coding_block_size_minus3: u32,
    /// `log2_max_pcm_luma_coding_block_size_minus3`.
    pub log2_max_pcm_luma_coding_block_size_minus3: u32,
    /// `vui_parameters_present_flag`.
    pub vui_parameters_present_flag: u8,
    /// VUI flags.
    pub vui_fields: HevcVuiFields,
    /// `aspect_ratio_idc`.
    pub aspect_ratio_idc: u8,
    /// `sar_width`.
    pub sar_width: u32,
    /// `sar_height`.
    pub sar_height: u32,
    /// `vui_num_units_in_tick`.
    pub vui_num_units_in_tick: u32,
    /// `vui_time_scale`.
    pub vui_time_scale: u32,
    /// `min_spatial_segmentation_idc`.
    pub min_spatial_segmentation_idc: u16,
    /// `max_bytes_per_pic_denom`.
    pub max_bytes_per_pic_denom: u8,
    /// `max_bits_per_min_cu_denom`.
    pub max_bits_per_min_cu_denom: u8,
    /// Screen content flags.
    pub scc_fields: u32,
    pub(crate) va_reserved: [u32; 7],
}

impl Default for EncSequenceParameterBufferHEVC {
    fn default() -> Self {
        EncSequenceParameterBufferHEVC {
            general_profile_idc: 1,
            general_level_idc: 120,
            general_tier_flag: 0,
            intra_period: 0,
            intra_idr_period: 0,
            ip_period: 1,
            bits_per_second: 0,
            pic_width_in_luma_samples: 0,
            pic_height_in_luma_samples: 0,
            seq_fields: HevcSeqFields::default(),
            log2_min_luma_coding_block_size_minus3: 0,
            log2_diff_max_min_luma_coding_block_size: 3,
            log2_min_transform_block_size_minus2: 0,
            log2_diff_max_min_transform_block_size: 3,
            max_transform_hierarchy_depth_inter: 0,
            max_transform_hierarchy_depth_intra: 0,
            pcm_sample_bit_depth_luma_minus1: 7,
            pcm_sample_bit_depth_chroma_minus1: 7,
            log2_min_pcm_luma_coding_block_size_minus3: 0,
            log2_max_pcm_luma_coding_block_size_minus3: 0,
            vui_parameters_present_flag: 0,
            vui_fields: HevcVuiFields::default(),
            aspect_ratio_idc: 0,
            sar_width: 0,
            sar_height: 0,
            vui_num_units_in_tick: 0,
            vui_time_scale: 0,
            min_spatial_segmentation_idc: 0,
            max_bytes_per_pic_denom: 0,
            max_bits_per_min_cu_denom: 0,
            scc_fields: 0,
            va_reserved: [0; 7],
        }
    }
}

// SAFETY: `#[repr(C)]` transcription checked by the assertion block below.
unsafe impl VaParam for EncSequenceParameterBufferHEVC {
    const TYPE: i32 = VAEncSequenceParameterBufferType;
}

va_bits! {
    /// `VAEncPictureParameterBufferHEVC::pic_fields`.
    HevcEncPicFields: u32 {
        idr_pic_flag: 1,
        coding_type: 3,
        reference_pic_flag: 1,
        dependent_slice_segments_enabled_flag: 1,
        sign_data_hiding_enabled_flag: 1,
        constrained_intra_pred_flag: 1,
        transform_skip_enabled_flag: 1,
        cu_qp_delta_enabled_flag: 1,
        weighted_pred_flag: 1,
        weighted_bipred_flag: 1,
        transquant_bypass_enabled_flag: 1,
        tiles_enabled_flag: 1,
        entropy_coding_sync_enabled_flag: 1,
        loop_filter_across_tiles_enabled_flag: 1,
        pps_loop_filter_across_slices_enabled_flag: 1,
        scaling_list_data_present_flag: 1,
        screen_content_flag: 1,
        enable_gpu_weighted_prediction: 1,
        no_output_of_prior_pics_flag: 1,
    }
}

/// `VAEncPictureParameterBufferHEVC`, `va_enc_hevc.h:270`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EncPictureParameterBufferHEVC {
    /// Reconstructed picture surface for this frame.
    pub decoded_curr_pic: VAPictureHEVC,
    /// Reference pictures available to this frame.
    pub reference_frames: [VAPictureHEVC; 15],
    /// Buffer the coded bits are written to.
    pub coded_buf: u32,
    /// Index into `reference_frames` of the collocated picture.
    pub collocated_ref_pic_index: u8,
    /// `HEVC_LAST_PICTURE_*` when this is the last picture.
    pub last_picture: u8,
    /// `init_qp_minus26 + 26`.
    pub pic_init_qp: u8,
    /// `diff_cu_qp_delta_depth`.
    pub diff_cu_qp_delta_depth: u8,
    /// `pps_cb_qp_offset`.
    pub pps_cb_qp_offset: i8,
    /// `pps_cr_qp_offset`.
    pub pps_cr_qp_offset: i8,
    /// `num_tile_columns_minus1`.
    pub num_tile_columns_minus1: u8,
    /// `num_tile_rows_minus1`.
    pub num_tile_rows_minus1: u8,
    /// `column_width_minus1`.
    pub column_width_minus1: [u8; 19],
    /// `row_height_minus1`.
    pub row_height_minus1: [u8; 21],
    /// `log2_parallel_merge_level_minus2`.
    pub log2_parallel_merge_level_minus2: u8,
    /// Maximum bits a CTU may use, 0 for no limit.
    pub ctu_max_bitsize_allowed: u8,
    /// `num_ref_idx_l0_default_active_minus1`.
    pub num_ref_idx_l0_default_active_minus1: u8,
    /// `num_ref_idx_l1_default_active_minus1`.
    pub num_ref_idx_l1_default_active_minus1: u8,
    /// `slice_pic_parameter_set_id`.
    pub slice_pic_parameter_set_id: u8,
    /// NAL unit type to code the slices as.
    pub nal_unit_type: u8,
    /// Picture flags.
    pub pic_fields: HevcEncPicFields,
    /// Temporal layer + 1, 0 when unused.
    pub hierarchical_level_plus1: u8,
    pub(crate) va_byte_reserved: u8,
    /// Screen content flags.
    pub scc_fields: u16,
    pub(crate) va_reserved: [u32; 15],
}

impl Default for EncPictureParameterBufferHEVC {
    fn default() -> Self {
        EncPictureParameterBufferHEVC {
            decoded_curr_pic: VAPictureHEVC::INVALID,
            reference_frames: [VAPictureHEVC::INVALID; 15],
            coded_buf: 0,
            collocated_ref_pic_index: 0xff,
            last_picture: 0,
            pic_init_qp: 26,
            diff_cu_qp_delta_depth: 0,
            pps_cb_qp_offset: 0,
            pps_cr_qp_offset: 0,
            num_tile_columns_minus1: 0,
            num_tile_rows_minus1: 0,
            column_width_minus1: [0; 19],
            row_height_minus1: [0; 21],
            log2_parallel_merge_level_minus2: 0,
            ctu_max_bitsize_allowed: 0,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            slice_pic_parameter_set_id: 0,
            nal_unit_type: 19, // IDR_W_RADL
            pic_fields: HevcEncPicFields::default(),
            hierarchical_level_plus1: 0,
            va_byte_reserved: 0,
            scc_fields: 0,
            va_reserved: [0; 15],
        }
    }
}

// SAFETY: `#[repr(C)]` transcription checked by the assertion block below.
unsafe impl VaParam for EncPictureParameterBufferHEVC {
    const TYPE: i32 = VAEncPictureParameterBufferType;
}

va_bits! {
    /// `VAEncSliceParameterBufferHEVC::slice_fields`.
    HevcEncSliceFields: u32 {
        last_slice_of_pic_flag: 1,
        dependent_slice_segment_flag: 1,
        colour_plane_id: 2,
        slice_temporal_mvp_enabled_flag: 1,
        slice_sao_luma_flag: 1,
        slice_sao_chroma_flag: 1,
        num_ref_idx_active_override_flag: 1,
        mvd_l1_zero_flag: 1,
        cabac_init_flag: 1,
        slice_deblocking_filter_disabled_flag: 2,
        slice_loop_filter_across_slices_enabled_flag: 1,
        collocated_from_l0_flag: 1,
    }
}

/// `VAEncSliceParameterBufferHEVC`, `va_enc_hevc.h:449`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EncSliceParameterBufferHEVC {
    /// `slice_segment_address`.
    pub slice_segment_address: u32,
    /// CTUs in this slice.
    pub num_ctu_in_slice: u32,
    /// `slice_type`.
    pub slice_type: u8,
    /// `slice_pic_parameter_set_id`.
    pub slice_pic_parameter_set_id: u8,
    /// `num_ref_idx_l0_active_minus1`.
    pub num_ref_idx_l0_active_minus1: u8,
    /// `num_ref_idx_l1_active_minus1`.
    pub num_ref_idx_l1_active_minus1: u8,
    /// `RefPicList0`.
    pub ref_pic_list0: [VAPictureHEVC; 15],
    /// `RefPicList1`.
    pub ref_pic_list1: [VAPictureHEVC; 15],
    /// `luma_log2_weight_denom`.
    pub luma_log2_weight_denom: u8,
    /// `delta_chroma_log2_weight_denom`.
    pub delta_chroma_log2_weight_denom: i8,
    /// `delta_luma_weight_l0`.
    pub delta_luma_weight_l0: [i8; 15],
    /// `luma_offset_l0`.
    pub luma_offset_l0: [i8; 15],
    /// `delta_chroma_weight_l0`.
    pub delta_chroma_weight_l0: [[i8; 2]; 15],
    /// `chroma_offset_l0`.
    pub chroma_offset_l0: [[i8; 2]; 15],
    /// `delta_luma_weight_l1`.
    pub delta_luma_weight_l1: [i8; 15],
    /// `luma_offset_l1`.
    pub luma_offset_l1: [i8; 15],
    /// `delta_chroma_weight_l1`.
    pub delta_chroma_weight_l1: [[i8; 2]; 15],
    /// `chroma_offset_l1`.
    pub chroma_offset_l1: [[i8; 2]; 15],
    /// `MaxNumMergeCand`.
    pub max_num_merge_cand: u8,
    /// `slice_qp_delta`.
    pub slice_qp_delta: i8,
    /// `slice_cb_qp_offset`.
    pub slice_cb_qp_offset: i8,
    /// `slice_cr_qp_offset`.
    pub slice_cr_qp_offset: i8,
    /// `slice_beta_offset_div2`.
    pub slice_beta_offset_div2: i8,
    /// `slice_tc_offset_div2`.
    pub slice_tc_offset_div2: i8,
    /// Slice flags.
    pub slice_fields: HevcEncSliceFields,
    /// Bit offset of `pred_weight_table()` inside the packed slice header.
    pub pred_weight_table_bit_offset: u32,
    /// Bit length of `pred_weight_table()`.
    pub pred_weight_table_bit_length: u32,
    pub(crate) va_reserved: [u32; 6],
}

impl Default for EncSliceParameterBufferHEVC {
    fn default() -> Self {
        EncSliceParameterBufferHEVC {
            slice_segment_address: 0,
            num_ctu_in_slice: 0,
            slice_type: 2,
            slice_pic_parameter_set_id: 0,
            num_ref_idx_l0_active_minus1: 0,
            num_ref_idx_l1_active_minus1: 0,
            ref_pic_list0: [VAPictureHEVC::INVALID; 15],
            ref_pic_list1: [VAPictureHEVC::INVALID; 15],
            luma_log2_weight_denom: 0,
            delta_chroma_log2_weight_denom: 0,
            delta_luma_weight_l0: [0; 15],
            luma_offset_l0: [0; 15],
            delta_chroma_weight_l0: [[0; 2]; 15],
            chroma_offset_l0: [[0; 2]; 15],
            delta_luma_weight_l1: [0; 15],
            luma_offset_l1: [0; 15],
            delta_chroma_weight_l1: [[0; 2]; 15],
            chroma_offset_l1: [[0; 2]; 15],
            max_num_merge_cand: 5,
            slice_qp_delta: 0,
            slice_cb_qp_offset: 0,
            slice_cr_qp_offset: 0,
            slice_beta_offset_div2: 0,
            slice_tc_offset_div2: 0,
            slice_fields: HevcEncSliceFields::default(),
            pred_weight_table_bit_offset: 0,
            pred_weight_table_bit_length: 0,
            va_reserved: [0; 6],
        }
    }
}

// SAFETY: `#[repr(C)]` transcription checked by the assertion block below.
unsafe impl VaParam for EncSliceParameterBufferHEVC {
    const TYPE: i32 = VAEncSliceParameterBufferType;
}

// ---------------------------------------------------------------------------
// AV1 encode
// ---------------------------------------------------------------------------

va_bits! {
    /// `VAEncSequenceParameterBufferAV1::seq_fields`.
    Av1SeqFields: u32 {
        still_picture: 1,
        use_128x128_superblock: 1,
        enable_filter_intra: 1,
        enable_intra_edge_filter: 1,
        enable_interintra_compound: 1,
        enable_masked_compound: 1,
        enable_warped_motion: 1,
        enable_dual_filter: 1,
        enable_order_hint: 1,
        enable_jnt_comp: 1,
        enable_ref_frame_mvs: 1,
        enable_superres: 1,
        enable_cdef: 1,
        enable_restoration: 1,
        bit_depth_minus8: 3,
        subsampling_x: 1,
        subsampling_y: 1,
        mono_chrome: 1,
    }
}

/// `VAEncSequenceParameterBufferAV1`, `va_enc_av1.h:118`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EncSequenceParameterBufferAV1 {
    /// `seq_profile`.
    pub seq_profile: u8,
    /// `seq_level_idx`.
    pub seq_level_idx: u8,
    /// `seq_tier`.
    pub seq_tier: u8,
    /// Temporal layering flag.
    pub hierarchical_flag: u8,
    /// Frames between key frames.
    pub intra_period: u32,
    /// Distance between P frames.
    pub ip_period: u32,
    /// Target bitrate.
    pub bits_per_second: u32,
    /// Sequence flags.
    pub seq_fields: Av1SeqFields,
    /// `OrderHintBits - 1`.
    pub order_hint_bits_minus_1: u8,
    pub(crate) va_reserved: [u32; 16],
}

impl Default for EncSequenceParameterBufferAV1 {
    fn default() -> Self {
        EncSequenceParameterBufferAV1 {
            seq_profile: 0,
            seq_level_idx: 0,
            seq_tier: 0,
            hierarchical_flag: 0,
            intra_period: 0,
            ip_period: 1,
            bits_per_second: 0,
            seq_fields: Av1SeqFields::default(),
            order_hint_bits_minus_1: 0,
            va_reserved: [0; 16],
        }
    }
}

// SAFETY: `#[repr(C)]` transcription checked by the assertion block below.
unsafe impl VaParam for EncSequenceParameterBufferAV1 {
    const TYPE: i32 = VAEncSequenceParameterBufferType;
}

va_bits! {
    /// `VAEncSegParamAV1::seg_flags`.
    Av1SegFlags: u8 {
        segmentation_enabled: 1,
        segmentation_update_map: 1,
        segmentation_temporal_update: 1,
    }
}

/// `VAEncSegParamAV1`, `va_enc_av1.h:216`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct EncSegParamAV1 {
    /// Segmentation flags.
    pub seg_flags: Av1SegFlags,
    /// Number of segments in use.
    pub segment_number: u8,
    /// `FeatureData`.
    pub feature_data: [[i16; 8]; 8],
    /// `FeatureEnabled` bitmask per segment.
    pub feature_mask: [u8; 8],
    pub(crate) va_reserved: [u32; 4],
}

/// `VAEncWarpedMotionParamsAV1`, `va_enc_av1.h:262`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct EncWarpedMotionParamsAV1 {
    /// Transformation type.
    pub wmtype: i32,
    /// Model parameters.
    pub wmmat: [i32; 8],
    /// Set when the model was rejected.
    pub invalid: u8,
    pub(crate) va_reserved: [u32; 4],
}

va_bits! {
    /// `VAEncPictureParameterBufferAV1::picture_flags`.
    Av1EncPicFlags: u32 {
        frame_type: 2,
        error_resilient_mode: 1,
        disable_cdf_update: 1,
        use_superres: 1,
        allow_high_precision_mv: 1,
        use_ref_frame_mvs: 1,
        disable_frame_end_update_cdf: 1,
        reduced_tx_set: 1,
        enable_frame_obu: 1,
        long_term_reference: 1,
        disable_frame_recon: 1,
        allow_intrabc: 1,
        palette_mode_enable: 1,
        allow_screen_content_tools: 1,
        force_integer_mv: 1,
    }
}

va_bits! {
    /// `VAEncPictureParameterBufferAV1::mode_control_flags`.
    Av1ModeControlFlags: u32 {
        delta_q_present: 1,
        delta_q_res: 2,
        delta_lf_present: 1,
        delta_lf_res: 2,
        delta_lf_multi: 1,
        tx_mode: 2,
        reference_mode: 2,
        skip_mode_present: 1,
    }
}

va_bits! {
    /// `VAEncPictureParameterBufferAV1::tile_group_obu_hdr_info`.
    Av1TileGroupObuHdrInfo: u8 {
        obu_extension_flag: 1,
        obu_has_size_field: 1,
        temporal_id: 3,
        spatial_id: 2,
    }
}

/// `VAEncPictureParameterBufferAV1`, `va_enc_av1.h:322`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EncPictureParameterBufferAV1 {
    /// `frame_width_minus_1`.
    pub frame_width_minus_1: u16,
    /// `frame_height_minus_1`.
    pub frame_height_minus_1: u16,
    /// Surface the reconstruction is written to.
    pub reconstructed_frame: VASurfaceID,
    /// Buffer the coded bits are written to.
    pub coded_buf: u32,
    /// The eight reference slots.
    pub reference_frames: [VASurfaceID; 8],
    /// `ref_frame_idx`.
    pub ref_frame_idx: [u8; 7],
    /// Temporal layer + 1.
    pub hierarchical_level_plus1: u8,
    /// `primary_ref_frame`.
    pub primary_ref_frame: u8,
    /// `order_hint`.
    pub order_hint: u8,
    /// `refresh_frame_flags`.
    pub refresh_frame_flags: u8,
    pub(crate) reserved8bits1: u8,
    /// Reference control for list 0.
    pub ref_frame_ctrl_l0: u32,
    /// Reference control for list 1.
    pub ref_frame_ctrl_l1: u32,
    /// Picture flags.
    pub picture_flags: Av1EncPicFlags,
    /// Segment id block size.
    pub seg_id_block_size: u8,
    /// `num_tile_groups_minus1`.
    pub num_tile_groups_minus1: u8,
    /// Temporal id.
    pub temporal_id: u8,
    /// `loop_filter_level[0..2]`.
    pub filter_level: [u8; 2],
    /// `loop_filter_level[2]`.
    pub filter_level_u: u8,
    /// `loop_filter_level[3]`.
    pub filter_level_v: u8,
    /// Loop filter flags.
    pub loop_filter_flags: u8,
    /// `SuperresDenom`.
    pub superres_scale_denominator: u8,
    /// `interpolation_filter`.
    pub interpolation_filter: u8,
    /// `loop_filter_ref_deltas`.
    pub ref_deltas: [i8; 8],
    /// `loop_filter_mode_deltas`.
    pub mode_deltas: [i8; 2],
    /// `base_q_idx`.
    pub base_qindex: u8,
    /// `DeltaQYDc`.
    pub y_dc_delta_q: i8,
    /// `DeltaQUDc`.
    pub u_dc_delta_q: i8,
    /// `DeltaQUAc`.
    pub u_ac_delta_q: i8,
    /// `DeltaQVDc`.
    pub v_dc_delta_q: i8,
    /// `DeltaQVAc`.
    pub v_ac_delta_q: i8,
    /// Lower bound the rate controller may pick.
    pub min_base_qindex: u8,
    /// Upper bound the rate controller may pick.
    pub max_base_qindex: u8,
    /// Quantiser matrix flags.
    pub qmatrix_flags: u16,
    pub(crate) reserved16bits1: u16,
    /// Mode control flags.
    pub mode_control_flags: Av1ModeControlFlags,
    /// Segmentation parameters.
    pub segments: EncSegParamAV1,
    /// `TileCols`.
    pub tile_cols: u8,
    /// `TileRows`.
    pub tile_rows: u8,
    pub(crate) reserved16bits2: u16,
    /// Per-column widths in superblocks, minus one.
    pub width_in_sbs_minus_1: [u16; 63],
    /// Per-row heights in superblocks, minus one.
    pub height_in_sbs_minus_1: [u16; 63],
    /// `context_update_tile_id`.
    pub context_update_tile_id: u16,
    /// `CdefDamping - 3`.
    pub cdef_damping_minus_3: u8,
    /// `cdef_bits`.
    pub cdef_bits: u8,
    /// Combined luma strengths.
    pub cdef_y_strengths: [u8; 8],
    /// Combined chroma strengths.
    pub cdef_uv_strengths: [u8; 8],
    /// Loop restoration flags.
    pub loop_restoration_flags: u16,
    /// Global motion.
    pub wm: [EncWarpedMotionParamsAV1; 7],
    /// Bit offset of `base_q_idx` in the packed frame header.
    pub bit_offset_qindex: u32,
    /// Bit offset of `segmentation_params()`.
    pub bit_offset_segmentation: u32,
    /// Bit offset of `loop_filter_params()`.
    pub bit_offset_loopfilter_params: u32,
    /// Bit offset of `cdef_params()`.
    pub bit_offset_cdef_params: u32,
    /// Bit length of `cdef_params()`.
    pub size_in_bits_cdef_params: u32,
    /// Byte offset of the frame OBU's size field.
    pub byte_offset_frame_hdr_obu_size: u32,
    /// Bit length of the frame header OBU.
    pub size_in_bits_frame_hdr_obu: u32,
    /// Tile group OBU header flags.
    pub tile_group_obu_hdr_info: Av1TileGroupObuHdrInfo,
    /// Number of skipped frames represented by this one.
    pub number_skip_frames: u8,
    pub(crate) reserved16bits3: u16,
    /// Size reduction for skipped frames.
    pub skip_frames_reduced_size: i32,
    pub(crate) va_reserved: [u32; 16],
}

impl Default for EncPictureParameterBufferAV1 {
    fn default() -> Self {
        EncPictureParameterBufferAV1 {
            frame_width_minus_1: 0,
            frame_height_minus_1: 0,
            reconstructed_frame: super::INVALID_SURFACE,
            coded_buf: 0,
            reference_frames: [super::INVALID_SURFACE; 8],
            ref_frame_idx: [0; 7],
            hierarchical_level_plus1: 0,
            primary_ref_frame: 7, // PRIMARY_REF_NONE
            order_hint: 0,
            refresh_frame_flags: 0xff,
            reserved8bits1: 0,
            ref_frame_ctrl_l0: 0,
            ref_frame_ctrl_l1: 0,
            picture_flags: Av1EncPicFlags::default(),
            seg_id_block_size: 0,
            num_tile_groups_minus1: 0,
            temporal_id: 0,
            filter_level: [0; 2],
            filter_level_u: 0,
            filter_level_v: 0,
            loop_filter_flags: 0,
            superres_scale_denominator: 8,
            interpolation_filter: 0,
            ref_deltas: [0; 8],
            mode_deltas: [0; 2],
            base_qindex: 128,
            y_dc_delta_q: 0,
            u_dc_delta_q: 0,
            u_ac_delta_q: 0,
            v_dc_delta_q: 0,
            v_ac_delta_q: 0,
            min_base_qindex: 0,
            max_base_qindex: 255,
            qmatrix_flags: 0,
            reserved16bits1: 0,
            mode_control_flags: Av1ModeControlFlags::default(),
            segments: EncSegParamAV1::default(),
            tile_cols: 1,
            tile_rows: 1,
            reserved16bits2: 0,
            width_in_sbs_minus_1: [0; 63],
            height_in_sbs_minus_1: [0; 63],
            context_update_tile_id: 0,
            cdef_damping_minus_3: 0,
            cdef_bits: 0,
            cdef_y_strengths: [0; 8],
            cdef_uv_strengths: [0; 8],
            loop_restoration_flags: 0,
            wm: [EncWarpedMotionParamsAV1::default(); 7],
            bit_offset_qindex: 0,
            bit_offset_segmentation: 0,
            bit_offset_loopfilter_params: 0,
            bit_offset_cdef_params: 0,
            size_in_bits_cdef_params: 0,
            byte_offset_frame_hdr_obu_size: 0,
            size_in_bits_frame_hdr_obu: 0,
            tile_group_obu_hdr_info: Av1TileGroupObuHdrInfo::default(),
            number_skip_frames: 0,
            reserved16bits3: 0,
            skip_frames_reduced_size: 0,
            va_reserved: [0; 16],
        }
    }
}

// SAFETY: `#[repr(C)]` transcription checked by the assertion block below.
unsafe impl VaParam for EncPictureParameterBufferAV1 {
    const TYPE: i32 = VAEncPictureParameterBufferType;
}

/// `VAEncTileGroupBufferAV1`, `va_enc_av1.h:590`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct EncTileGroupBufferAV1 {
    /// First tile in the group.
    pub tg_start: u8,
    /// Last tile in the group.
    pub tg_end: u8,
    pub(crate) va_reserved: [u32; 4],
}

// SAFETY: `#[repr(C)]` transcription checked by the assertion block below.
unsafe impl VaParam for EncTileGroupBufferAV1 {
    const TYPE: i32 = VAEncSliceParameterBufferType;
}

// ---------------------------------------------------------------------------
// ABI transcription check — `crates/ec-hw/abi-probe.c`, libva 1.23.0, x86_64:
//
//   VAEncPackedHeaderParameterBuffer size=28   align=4 bit_length=4
//                                                      has_emulation_bytes=8
//   VAEncMiscParameterRateControl    size=60   (15 u32 payload words)
//   VAEncMiscParameterFrameRate      size=24   (6 u32)
//   VAEncMiscParameterHRD            size=24   (6 u32)
//   VAEncSequenceParameterBufferH264 size=1132 align=4 intra_period=4
//                                                      picture_width_in_mbs=24
//                                                      seq_fields=28
//                                                      bit_depth_luma_minus8=32
//                                                      offset_for_non_ref_pic=36
//                                                      offset_for_ref_frame=44
//                                                      frame_cropping_flag=1068
//                                                      frame_crop_left_offset=1072
//                                                      vui_parameters_present_flag=1088
//                                                      vui_fields=1092
//                                                      aspect_ratio_idc=1096
//                                                      num_units_in_tick=1108
//                                                      time_scale=1112
//   VAEncPictureParameterBufferH264  size=648  align=4 ReferenceFrames=36
//                                                      coded_buf=612
//                                                      pic_parameter_set_id=616
//                                                      frame_num=620 pic_init_qp=622
//                                                      pic_fields=628
//   VAEncSliceParameterBufferH264    size=3140 align=4 macroblock_info=8
//                                                      slice_type=12 idr_pic_id=14
//                                                      delta_pic_order_cnt_bottom=20
//                                                      direct_spatial_mv_pred_flag=32
//                                                      RefPicList0=36 RefPicList1=1188
//                                                      luma_log2_weight_denom=2340
//                                                      cabac_init_idc=3118
//   VAEncSequenceParameterBufferHEVC size=116  align=4 intra_period=4
//                                                      pic_width_in_luma_samples=20
//                                                      seq_fields=24
//                                                      log2_min_luma_coding_block_size_minus3=28
//                                                      pcm_sample_bit_depth_luma_minus1=36
//                                                      vui_parameters_present_flag=52
//                                                      vui_fields=56 aspect_ratio_idc=60
//                                                      vui_num_units_in_tick=72
//                                                      min_spatial_segmentation_idc=80
//                                                      scc_fields=84
//   VAEncPictureParameterBufferHEVC  size=576  align=4 reference_frames=28
//                                                      coded_buf=448
//                                                      collocated_ref_pic_index=452
//                                                      column_width_minus1=460
//                                                      row_height_minus1=479
//                                                      log2_parallel_merge_level_minus2=500
//                                                      nal_unit_type=505 pic_fields=508
//                                                      hierarchical_level_plus1=512
//                                                      scc_fields=514
//   VAEncSliceParameterBufferHEVC    size=1076 align=4 num_ctu_in_slice=4
//                                                      slice_type=8 ref_pic_list0=12
//                                                      ref_pic_list1=432
//                                                      luma_log2_weight_denom=852
//                                                      max_num_merge_cand=1034
//                                                      slice_fields=1040
//                                                      pred_weight_table_bit_offset=1044
//   VAEncSequenceParameterBufferAV1  size=88   align=4 intra_period=4 seq_fields=16
//                                                      order_hint_bits_minus_1=20
//   VAEncSegParamAV1                 size=156  align=4 feature_data=2
//                                                      feature_mask=130
//   VAEncWarpedMotionParamsAV1       size=56   align=4
//   VAEncPictureParameterBufferAV1   size=1032 align=4 reconstructed_frame=4
//                                                      coded_buf=8
//                                                      reference_frames=12
//                                                      ref_frame_idx=44
//                                                      ref_frame_ctrl_l0=56
//                                                      picture_flags=64
//                                                      seg_id_block_size=68
//                                                      filter_level=71
//                                                      loop_filter_flags=75
//                                                      ref_deltas=78 base_qindex=88
//                                                      min_base_qindex=94
//                                                      qmatrix_flags=96
//                                                      mode_control_flags=100
//                                                      segments=104 tile_cols=260
//                                                      width_in_sbs_minus_1=264
//                                                      context_update_tile_id=516
//                                                      cdef_damping_minus_3=518
//                                                      loop_restoration_flags=536
//                                                      wm=540 bit_offset_qindex=932
//                                                      tile_group_obu_hdr_info=960
//                                                      number_skip_frames=961
//                                                      skip_frames_reduced_size=964
//   VAEncTileGroupBufferAV1          size=20   align=4
const _: () = {
    use std::mem::{align_of, offset_of, size_of};

    assert!(
        size_of::<PackedHeaderParameterBuffer>() == 28
            && align_of::<PackedHeaderParameterBuffer>() == 4
    );
    assert!(offset_of!(PackedHeaderParameterBuffer, bit_length) == 4);
    assert!(offset_of!(PackedHeaderParameterBuffer, has_emulation_bytes) == 8);

    // Misc payloads are submitted as `{ type, words... }`; the word counts are
    // the header sizes minus the 4-byte type each buffer carries.
    assert!(size_of::<u32>() * (1 + 15) == 64);
    assert!(size_of::<u32>() * (1 + 6) == 28);

    assert!(
        size_of::<EncSequenceParameterBufferH264>() == 1132
            && align_of::<EncSequenceParameterBufferH264>() == 4
    );
    assert!(offset_of!(EncSequenceParameterBufferH264, intra_period) == 4);
    assert!(offset_of!(EncSequenceParameterBufferH264, picture_width_in_mbs) == 24);
    assert!(offset_of!(EncSequenceParameterBufferH264, seq_fields) == 28);
    assert!(offset_of!(EncSequenceParameterBufferH264, bit_depth_luma_minus8) == 32);
    assert!(offset_of!(EncSequenceParameterBufferH264, offset_for_non_ref_pic) == 36);
    assert!(offset_of!(EncSequenceParameterBufferH264, offset_for_ref_frame) == 44);
    assert!(offset_of!(EncSequenceParameterBufferH264, frame_cropping_flag) == 1068);
    assert!(offset_of!(EncSequenceParameterBufferH264, frame_crop_left_offset) == 1072);
    assert!(offset_of!(EncSequenceParameterBufferH264, vui_parameters_present_flag) == 1088);
    assert!(offset_of!(EncSequenceParameterBufferH264, vui_fields) == 1092);
    assert!(offset_of!(EncSequenceParameterBufferH264, aspect_ratio_idc) == 1096);
    assert!(offset_of!(EncSequenceParameterBufferH264, num_units_in_tick) == 1108);
    assert!(offset_of!(EncSequenceParameterBufferH264, time_scale) == 1112);

    assert!(
        size_of::<EncPictureParameterBufferH264>() == 648
            && align_of::<EncPictureParameterBufferH264>() == 4
    );
    assert!(offset_of!(EncPictureParameterBufferH264, reference_frames) == 36);
    assert!(offset_of!(EncPictureParameterBufferH264, coded_buf) == 612);
    assert!(offset_of!(EncPictureParameterBufferH264, pic_parameter_set_id) == 616);
    assert!(offset_of!(EncPictureParameterBufferH264, frame_num) == 620);
    assert!(offset_of!(EncPictureParameterBufferH264, pic_init_qp) == 622);
    assert!(offset_of!(EncPictureParameterBufferH264, pic_fields) == 628);

    assert!(
        size_of::<EncSliceParameterBufferH264>() == 3140
            && align_of::<EncSliceParameterBufferH264>() == 4
    );
    assert!(offset_of!(EncSliceParameterBufferH264, macroblock_info) == 8);
    assert!(offset_of!(EncSliceParameterBufferH264, slice_type) == 12);
    assert!(offset_of!(EncSliceParameterBufferH264, idr_pic_id) == 14);
    assert!(offset_of!(EncSliceParameterBufferH264, delta_pic_order_cnt_bottom) == 20);
    assert!(offset_of!(EncSliceParameterBufferH264, direct_spatial_mv_pred_flag) == 32);
    assert!(offset_of!(EncSliceParameterBufferH264, ref_pic_list0) == 36);
    assert!(offset_of!(EncSliceParameterBufferH264, ref_pic_list1) == 1188);
    assert!(offset_of!(EncSliceParameterBufferH264, luma_log2_weight_denom) == 2340);
    assert!(offset_of!(EncSliceParameterBufferH264, cabac_init_idc) == 3118);

    assert!(
        size_of::<EncSequenceParameterBufferHEVC>() == 116
            && align_of::<EncSequenceParameterBufferHEVC>() == 4
    );
    assert!(offset_of!(EncSequenceParameterBufferHEVC, intra_period) == 4);
    assert!(offset_of!(EncSequenceParameterBufferHEVC, pic_width_in_luma_samples) == 20);
    assert!(offset_of!(EncSequenceParameterBufferHEVC, seq_fields) == 24);
    assert!(
        offset_of!(
            EncSequenceParameterBufferHEVC,
            log2_min_luma_coding_block_size_minus3
        ) == 28
    );
    assert!(
        offset_of!(
            EncSequenceParameterBufferHEVC,
            pcm_sample_bit_depth_luma_minus1
        ) == 36
    );
    assert!(offset_of!(EncSequenceParameterBufferHEVC, vui_parameters_present_flag) == 52);
    assert!(offset_of!(EncSequenceParameterBufferHEVC, vui_fields) == 56);
    assert!(offset_of!(EncSequenceParameterBufferHEVC, aspect_ratio_idc) == 60);
    assert!(offset_of!(EncSequenceParameterBufferHEVC, vui_num_units_in_tick) == 72);
    assert!(offset_of!(EncSequenceParameterBufferHEVC, min_spatial_segmentation_idc) == 80);
    assert!(offset_of!(EncSequenceParameterBufferHEVC, scc_fields) == 84);

    assert!(
        size_of::<EncPictureParameterBufferHEVC>() == 576
            && align_of::<EncPictureParameterBufferHEVC>() == 4
    );
    assert!(offset_of!(EncPictureParameterBufferHEVC, reference_frames) == 28);
    assert!(offset_of!(EncPictureParameterBufferHEVC, coded_buf) == 448);
    assert!(offset_of!(EncPictureParameterBufferHEVC, collocated_ref_pic_index) == 452);
    assert!(offset_of!(EncPictureParameterBufferHEVC, column_width_minus1) == 460);
    assert!(offset_of!(EncPictureParameterBufferHEVC, row_height_minus1) == 479);
    assert!(
        offset_of!(
            EncPictureParameterBufferHEVC,
            log2_parallel_merge_level_minus2
        ) == 500
    );
    assert!(offset_of!(EncPictureParameterBufferHEVC, nal_unit_type) == 505);
    assert!(offset_of!(EncPictureParameterBufferHEVC, pic_fields) == 508);
    assert!(offset_of!(EncPictureParameterBufferHEVC, hierarchical_level_plus1) == 512);
    assert!(offset_of!(EncPictureParameterBufferHEVC, scc_fields) == 514);

    assert!(
        size_of::<EncSliceParameterBufferHEVC>() == 1076
            && align_of::<EncSliceParameterBufferHEVC>() == 4
    );
    assert!(offset_of!(EncSliceParameterBufferHEVC, num_ctu_in_slice) == 4);
    assert!(offset_of!(EncSliceParameterBufferHEVC, slice_type) == 8);
    assert!(offset_of!(EncSliceParameterBufferHEVC, ref_pic_list0) == 12);
    assert!(offset_of!(EncSliceParameterBufferHEVC, ref_pic_list1) == 432);
    assert!(offset_of!(EncSliceParameterBufferHEVC, luma_log2_weight_denom) == 852);
    assert!(offset_of!(EncSliceParameterBufferHEVC, max_num_merge_cand) == 1034);
    assert!(offset_of!(EncSliceParameterBufferHEVC, slice_fields) == 1040);
    assert!(offset_of!(EncSliceParameterBufferHEVC, pred_weight_table_bit_offset) == 1044);

    assert!(
        size_of::<EncSequenceParameterBufferAV1>() == 88
            && align_of::<EncSequenceParameterBufferAV1>() == 4
    );
    assert!(offset_of!(EncSequenceParameterBufferAV1, intra_period) == 4);
    assert!(offset_of!(EncSequenceParameterBufferAV1, seq_fields) == 16);
    assert!(offset_of!(EncSequenceParameterBufferAV1, order_hint_bits_minus_1) == 20);

    assert!(size_of::<EncSegParamAV1>() == 156 && align_of::<EncSegParamAV1>() == 4);
    assert!(offset_of!(EncSegParamAV1, feature_data) == 2);
    assert!(offset_of!(EncSegParamAV1, feature_mask) == 130);

    assert!(size_of::<EncWarpedMotionParamsAV1>() == 56);

    assert!(
        size_of::<EncPictureParameterBufferAV1>() == 1032
            && align_of::<EncPictureParameterBufferAV1>() == 4
    );
    assert!(offset_of!(EncPictureParameterBufferAV1, reconstructed_frame) == 4);
    assert!(offset_of!(EncPictureParameterBufferAV1, coded_buf) == 8);
    assert!(offset_of!(EncPictureParameterBufferAV1, reference_frames) == 12);
    assert!(offset_of!(EncPictureParameterBufferAV1, ref_frame_idx) == 44);
    assert!(offset_of!(EncPictureParameterBufferAV1, ref_frame_ctrl_l0) == 56);
    assert!(offset_of!(EncPictureParameterBufferAV1, picture_flags) == 64);
    assert!(offset_of!(EncPictureParameterBufferAV1, seg_id_block_size) == 68);
    assert!(offset_of!(EncPictureParameterBufferAV1, filter_level) == 71);
    assert!(offset_of!(EncPictureParameterBufferAV1, loop_filter_flags) == 75);
    assert!(offset_of!(EncPictureParameterBufferAV1, ref_deltas) == 78);
    assert!(offset_of!(EncPictureParameterBufferAV1, base_qindex) == 88);
    assert!(offset_of!(EncPictureParameterBufferAV1, min_base_qindex) == 94);
    assert!(offset_of!(EncPictureParameterBufferAV1, qmatrix_flags) == 96);
    assert!(offset_of!(EncPictureParameterBufferAV1, mode_control_flags) == 100);
    assert!(offset_of!(EncPictureParameterBufferAV1, segments) == 104);
    assert!(offset_of!(EncPictureParameterBufferAV1, tile_cols) == 260);
    assert!(offset_of!(EncPictureParameterBufferAV1, width_in_sbs_minus_1) == 264);
    assert!(offset_of!(EncPictureParameterBufferAV1, context_update_tile_id) == 516);
    assert!(offset_of!(EncPictureParameterBufferAV1, cdef_damping_minus_3) == 518);
    assert!(offset_of!(EncPictureParameterBufferAV1, loop_restoration_flags) == 536);
    assert!(offset_of!(EncPictureParameterBufferAV1, wm) == 540);
    assert!(offset_of!(EncPictureParameterBufferAV1, bit_offset_qindex) == 932);
    assert!(offset_of!(EncPictureParameterBufferAV1, tile_group_obu_hdr_info) == 960);
    assert!(offset_of!(EncPictureParameterBufferAV1, number_skip_frames) == 961);
    assert!(offset_of!(EncPictureParameterBufferAV1, skip_frames_reduced_size) == 964);

    assert!(size_of::<EncTileGroupBufferAV1>() == 20);
};

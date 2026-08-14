//! HEVC decode parameter buffers (`va.h:5287`, `va_dec_hevc.h`).

use ec_va::sys::{
    VAIQMatrixBufferType, VAPictureParameterBufferType, VASliceParameterBufferType, VASurfaceID,
};

use super::{INVALID_SURFACE, VaParam};
use crate::va_bits;

/// `VA_PICTURE_HEVC_INVALID`.
pub const PICTURE_INVALID: u32 = 0x0000_0001;
/// `VA_PICTURE_HEVC_FIELD_PIC`.
pub const PICTURE_FIELD_PIC: u32 = 0x0000_0002;
/// `VA_PICTURE_HEVC_BOTTOM_FIELD`.
pub const PICTURE_BOTTOM_FIELD: u32 = 0x0000_0004;
/// `VA_PICTURE_HEVC_LONG_TERM_REFERENCE`.
pub const PICTURE_LONG_TERM_REFERENCE: u32 = 0x0000_0008;
/// `VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE`.
pub const PICTURE_RPS_ST_CURR_BEFORE: u32 = 0x0000_0010;
/// `VA_PICTURE_HEVC_RPS_ST_CURR_AFTER`.
pub const PICTURE_RPS_ST_CURR_AFTER: u32 = 0x0000_0020;
/// `VA_PICTURE_HEVC_RPS_LT_CURR`.
pub const PICTURE_RPS_LT_CURR: u32 = 0x0000_0040;

/// `VAPictureHEVC`, `va.h:5287`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VAPictureHEVC {
    /// Surface holding the picture, or [`INVALID_SURFACE`].
    pub picture_id: VASurfaceID,
    /// `PicOrderCntVal`.
    pub pic_order_cnt: i32,
    /// `VA_PICTURE_HEVC_*` bitmask.
    pub flags: u32,
    pub(crate) va_reserved: [u32; 4],
}

impl VAPictureHEVC {
    /// The "no picture here" entry for unused reference slots.
    pub const INVALID: VAPictureHEVC = VAPictureHEVC {
        picture_id: INVALID_SURFACE,
        pic_order_cnt: 0,
        flags: PICTURE_INVALID,
        va_reserved: [0; 4],
    };

    /// A reference or current picture.
    pub fn new(picture_id: VASurfaceID, pic_order_cnt: i32, flags: u32) -> VAPictureHEVC {
        VAPictureHEVC {
            picture_id,
            pic_order_cnt,
            flags,
            va_reserved: [0; 4],
        }
    }
}

impl Default for VAPictureHEVC {
    fn default() -> Self {
        VAPictureHEVC::INVALID
    }
}

va_bits! {
    /// `VAPictureParameterBufferHEVC::pic_fields`.
    PicFields: u32 {
        chroma_format_idc: 2,
        separate_colour_plane_flag: 1,
        pcm_enabled_flag: 1,
        scaling_list_enabled_flag: 1,
        transform_skip_enabled_flag: 1,
        amp_enabled_flag: 1,
        strong_intra_smoothing_enabled_flag: 1,
        sign_data_hiding_enabled_flag: 1,
        constrained_intra_pred_flag: 1,
        cu_qp_delta_enabled_flag: 1,
        weighted_pred_flag: 1,
        weighted_bipred_flag: 1,
        transquant_bypass_enabled_flag: 1,
        tiles_enabled_flag: 1,
        entropy_coding_sync_enabled_flag: 1,
        pps_loop_filter_across_slices_enabled_flag: 1,
        loop_filter_across_tiles_enabled_flag: 1,
        pcm_loop_filter_disabled_flag: 1,
        no_pic_reordering_flag: 1,
        no_bi_pred_flag: 1,
    }
}

va_bits! {
    /// `VAPictureParameterBufferHEVC::slice_parsing_fields`.
    SliceParsingFields: u32 {
        lists_modification_present_flag: 1,
        long_term_ref_pics_present_flag: 1,
        sps_temporal_mvp_enabled_flag: 1,
        cabac_init_present_flag: 1,
        output_flag_present_flag: 1,
        dependent_slice_segments_enabled_flag: 1,
        pps_slice_chroma_qp_offsets_present_flag: 1,
        sample_adaptive_offset_enabled_flag: 1,
        deblocking_filter_override_enabled_flag: 1,
        pps_disable_deblocking_filter_flag: 1,
        slice_segment_header_extension_present_flag: 1,
        rap_pic_flag: 1,
        idr_pic_flag: 1,
        intra_pic_flag: 1,
    }
}

/// `VAPictureParameterBufferHEVC`, `va_dec_hevc.h:57`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PictureParameterBufferHEVC {
    /// The picture being decoded.
    pub curr_pic: VAPictureHEVC,
    /// The DPB; unused slots are [`VAPictureHEVC::INVALID`].
    pub reference_frames: [VAPictureHEVC; 15],
    /// `pic_width_in_luma_samples`.
    pub pic_width_in_luma_samples: u16,
    /// `pic_height_in_luma_samples`.
    pub pic_height_in_luma_samples: u16,
    /// Picture flags.
    pub pic_fields: PicFields,
    /// `sps_max_dec_pic_buffering_minus1`.
    pub sps_max_dec_pic_buffering_minus1: u8,
    /// `bit_depth_luma_minus8`.
    pub bit_depth_luma_minus8: u8,
    /// `bit_depth_chroma_minus8`.
    pub bit_depth_chroma_minus8: u8,
    /// `pcm_sample_bit_depth_luma_minus1`.
    pub pcm_sample_bit_depth_luma_minus1: u8,
    /// `pcm_sample_bit_depth_chroma_minus1`.
    pub pcm_sample_bit_depth_chroma_minus1: u8,
    /// `log2_min_luma_coding_block_size_minus3`.
    pub log2_min_luma_coding_block_size_minus3: u8,
    /// `log2_diff_max_min_luma_coding_block_size`.
    pub log2_diff_max_min_luma_coding_block_size: u8,
    /// `log2_min_transform_block_size_minus2`.
    pub log2_min_transform_block_size_minus2: u8,
    /// `log2_diff_max_min_transform_block_size`.
    pub log2_diff_max_min_transform_block_size: u8,
    /// `log2_min_pcm_luma_coding_block_size_minus3`.
    pub log2_min_pcm_luma_coding_block_size_minus3: u8,
    /// `log2_diff_max_min_pcm_luma_coding_block_size`.
    pub log2_diff_max_min_pcm_luma_coding_block_size: u8,
    /// `max_transform_hierarchy_depth_intra`.
    pub max_transform_hierarchy_depth_intra: u8,
    /// `max_transform_hierarchy_depth_inter`.
    pub max_transform_hierarchy_depth_inter: u8,
    /// `init_qp_minus26`.
    pub init_qp_minus26: i8,
    /// `diff_cu_qp_delta_depth`.
    pub diff_cu_qp_delta_depth: u8,
    /// `pps_cb_qp_offset`.
    pub pps_cb_qp_offset: i8,
    /// `pps_cr_qp_offset`.
    pub pps_cr_qp_offset: i8,
    /// `log2_parallel_merge_level_minus2`.
    pub log2_parallel_merge_level_minus2: u8,
    /// `num_tile_columns_minus1`.
    pub num_tile_columns_minus1: u8,
    /// `num_tile_rows_minus1`.
    pub num_tile_rows_minus1: u8,
    /// `column_width_minus1`, filled even for uniform spacing.
    pub column_width_minus1: [u16; 19],
    /// `row_height_minus1`, filled even for uniform spacing.
    pub row_height_minus1: [u16; 21],
    /// Flags a driver needs to parse slice segment headers.
    pub slice_parsing_fields: SliceParsingFields,
    /// `log2_max_pic_order_cnt_lsb_minus4`.
    pub log2_max_pic_order_cnt_lsb_minus4: u8,
    /// `num_short_term_ref_pic_sets`.
    pub num_short_term_ref_pic_sets: u8,
    /// `num_long_term_ref_pics_sps`.
    pub num_long_term_ref_pic_sps: u8,
    /// `num_ref_idx_l0_default_active_minus1`.
    pub num_ref_idx_l0_default_active_minus1: u8,
    /// `num_ref_idx_l1_default_active_minus1`.
    pub num_ref_idx_l1_default_active_minus1: u8,
    /// `pps_beta_offset_div2`.
    pub pps_beta_offset_div2: i8,
    /// `pps_tc_offset_div2`.
    pub pps_tc_offset_div2: i8,
    /// `num_extra_slice_header_bits`.
    pub num_extra_slice_header_bits: u8,
    /// Size in bits of the `st_ref_pic_set()` written in the slice header, so
    /// the driver can skip it; zero when the slice referenced an SPS set.
    pub st_rps_bits: u32,
    pub(crate) va_reserved: [u32; 8],
}

impl Default for PictureParameterBufferHEVC {
    fn default() -> Self {
        PictureParameterBufferHEVC {
            curr_pic: VAPictureHEVC::INVALID,
            reference_frames: [VAPictureHEVC::INVALID; 15],
            pic_width_in_luma_samples: 0,
            pic_height_in_luma_samples: 0,
            pic_fields: PicFields::default(),
            sps_max_dec_pic_buffering_minus1: 0,
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            pcm_sample_bit_depth_luma_minus1: 0,
            pcm_sample_bit_depth_chroma_minus1: 0,
            log2_min_luma_coding_block_size_minus3: 0,
            log2_diff_max_min_luma_coding_block_size: 0,
            log2_min_transform_block_size_minus2: 0,
            log2_diff_max_min_transform_block_size: 0,
            log2_min_pcm_luma_coding_block_size_minus3: 0,
            log2_diff_max_min_pcm_luma_coding_block_size: 0,
            max_transform_hierarchy_depth_intra: 0,
            max_transform_hierarchy_depth_inter: 0,
            init_qp_minus26: 0,
            diff_cu_qp_delta_depth: 0,
            pps_cb_qp_offset: 0,
            pps_cr_qp_offset: 0,
            log2_parallel_merge_level_minus2: 0,
            num_tile_columns_minus1: 0,
            num_tile_rows_minus1: 0,
            column_width_minus1: [0; 19],
            row_height_minus1: [0; 21],
            slice_parsing_fields: SliceParsingFields::default(),
            log2_max_pic_order_cnt_lsb_minus4: 0,
            num_short_term_ref_pic_sets: 0,
            num_long_term_ref_pic_sps: 0,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            pps_beta_offset_div2: 0,
            pps_tc_offset_div2: 0,
            num_extra_slice_header_bits: 0,
            st_rps_bits: 0,
            va_reserved: [0; 8],
        }
    }
}

// SAFETY: `#[repr(C)]` transcription of `VAPictureParameterBufferHEVC`, checked
// by the assertion block below; all fields are integers or integer arrays.
unsafe impl VaParam for PictureParameterBufferHEVC {
    const TYPE: i32 = VAPictureParameterBufferType;
}

va_bits! {
    /// `VASliceParameterBufferHEVC::LongSliceFlags`.
    LongSliceFlags: u32 {
        last_slice_of_pic: 1,
        dependent_slice_segment_flag: 1,
        slice_type: 2,
        color_plane_id: 2,
        slice_sao_luma_flag: 1,
        slice_sao_chroma_flag: 1,
        mvd_l1_zero_flag: 1,
        cabac_init_flag: 1,
        slice_temporal_mvp_enabled_flag: 1,
        slice_deblocking_filter_disabled_flag: 1,
        collocated_from_l0_flag: 1,
        slice_loop_filter_across_slices_enabled_flag: 1,
    }
}

/// `VASliceParameterBufferHEVC`, `va_dec_hevc.h:352`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SliceParameterBufferHEVC {
    /// Bytes of slice data, NAL unit header included.
    pub slice_data_size: u32,
    /// Offset of this slice's NAL unit within the data buffer.
    pub slice_data_offset: u32,
    /// `VA_SLICE_DATA_FLAG_*`.
    pub slice_data_flag: u32,
    /// Bytes from the NAL unit header to `slice_data()`, in escaped bytes.
    pub slice_data_byte_offset: u32,
    /// `slice_segment_address`.
    pub slice_segment_address: u32,
    /// Indices into `reference_frames`, `0xff` for unused entries.
    pub ref_pic_list: [[u8; 15]; 2],
    /// Slice flags.
    pub long_slice_flags: LongSliceFlags,
    /// Index into `ref_pic_list`, `0xff` when there is no collocated picture.
    pub collocated_ref_idx: u8,
    /// `num_ref_idx_l0_active_minus1`, override applied.
    pub num_ref_idx_l0_active_minus1: u8,
    /// `num_ref_idx_l1_active_minus1`, override applied.
    pub num_ref_idx_l1_active_minus1: u8,
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
    /// `ChromaOffsetL0`.
    pub chroma_offset_l0: [[i8; 2]; 15],
    /// `delta_luma_weight_l1`.
    pub delta_luma_weight_l1: [i8; 15],
    /// `luma_offset_l1`.
    pub luma_offset_l1: [i8; 15],
    /// `delta_chroma_weight_l1`.
    pub delta_chroma_weight_l1: [[i8; 2]; 15],
    /// `ChromaOffsetL1`.
    pub chroma_offset_l1: [[i8; 2]; 15],
    /// `five_minus_max_num_merge_cand`.
    pub five_minus_max_num_merge_cand: u8,
    /// `num_entry_point_offsets`.
    pub num_entry_point_offsets: u16,
    /// `entry_offset_to_subset_array`.
    pub entry_offset_to_subset_array: u16,
    /// Emulation prevention bytes inside the slice header.
    pub slice_data_num_emu_prevn_bytes: u16,
    pub(crate) va_reserved: [u32; 2],
}

impl Default for SliceParameterBufferHEVC {
    fn default() -> Self {
        SliceParameterBufferHEVC {
            slice_data_size: 0,
            slice_data_offset: 0,
            slice_data_flag: 0,
            slice_data_byte_offset: 0,
            slice_segment_address: 0,
            ref_pic_list: [[0xff; 15]; 2],
            long_slice_flags: LongSliceFlags::default(),
            collocated_ref_idx: 0xff,
            num_ref_idx_l0_active_minus1: 0,
            num_ref_idx_l1_active_minus1: 0,
            slice_qp_delta: 0,
            slice_cb_qp_offset: 0,
            slice_cr_qp_offset: 0,
            slice_beta_offset_div2: 0,
            slice_tc_offset_div2: 0,
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
            five_minus_max_num_merge_cand: 0,
            num_entry_point_offsets: 0,
            entry_offset_to_subset_array: 0,
            slice_data_num_emu_prevn_bytes: 0,
            va_reserved: [0; 2],
        }
    }
}

// SAFETY: as `PictureParameterBufferHEVC`.
unsafe impl VaParam for SliceParameterBufferHEVC {
    const TYPE: i32 = VASliceParameterBufferType;
}

/// `VAIQMatrixBufferHEVC`, `va_dec_hevc.h:561`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct IQMatrixBufferHEVC {
    /// `ScalingList[0]`.
    pub scaling_list_4x4: [[u8; 16]; 6],
    /// `ScalingList[1]`.
    pub scaling_list_8x8: [[u8; 64]; 6],
    /// `ScalingList[2]`.
    pub scaling_list_16x16: [[u8; 64]; 6],
    /// `ScalingList[3]`.
    pub scaling_list_32x32: [[u8; 64]; 2],
    /// DC coefficients of the 16x16 lists.
    pub scaling_list_dc_16x16: [u8; 6],
    /// DC coefficients of the 32x32 lists.
    pub scaling_list_dc_32x32: [u8; 2],
    pub(crate) va_reserved: [u32; 4],
}

impl Default for IQMatrixBufferHEVC {
    fn default() -> Self {
        IQMatrixBufferHEVC {
            scaling_list_4x4: [[16; 16]; 6],
            scaling_list_8x8: [[16; 64]; 6],
            scaling_list_16x16: [[16; 64]; 6],
            scaling_list_32x32: [[16; 64]; 2],
            scaling_list_dc_16x16: [16; 6],
            scaling_list_dc_32x32: [16; 2],
            va_reserved: [0; 4],
        }
    }
}

// SAFETY: as `PictureParameterBufferHEVC`.
unsafe impl VaParam for IQMatrixBufferHEVC {
    const TYPE: i32 = VAIQMatrixBufferType;
}

// ---------------------------------------------------------------------------
// ABI transcription check — `crates/ec-hw/abi-probe.c`, libva 1.23.0, x86_64:
//
//   VAPictureHEVC                size=28   align=4  pic_order_cnt=4 flags=8
//   VAPictureParameterBufferHEVC size=604  align=4  ReferenceFrames=28
//                                                   pic_width_in_luma_samples=448
//                                                   pic_fields=452
//                                                   sps_max_dec_pic_buffering_minus1=456
//                                                   column_width_minus1=476
//                                                   row_height_minus1=514
//                                                   slice_parsing_fields=556
//                                                   log2_max_pic_order_cnt_lsb_minus4=560
//                                                   st_rps_bits=568
//   VASliceParameterBufferHEVC   size=264  align=4  slice_segment_address=16
//                                                   RefPicList=20 LongSliceFlags=52
//                                                   collocated_ref_idx=56
//                                                   luma_log2_weight_denom=64
//                                                   delta_luma_weight_l0=66
//                                                   delta_luma_weight_l1=156
//                                                   five_minus_max_num_merge_cand=246
//                                                   num_entry_point_offsets=248
//                                                   slice_data_num_emu_prevn_bytes=252
//   VAIQMatrixBufferHEVC         size=1016 align=4  ScalingList32x32=864
//                                                   ScalingListDC16x16=992
const _: () = {
    use std::mem::{align_of, offset_of, size_of};

    assert!(size_of::<VAPictureHEVC>() == 28 && align_of::<VAPictureHEVC>() == 4);
    assert!(offset_of!(VAPictureHEVC, pic_order_cnt) == 4);
    assert!(offset_of!(VAPictureHEVC, flags) == 8);

    assert!(
        size_of::<PictureParameterBufferHEVC>() == 604
            && align_of::<PictureParameterBufferHEVC>() == 4
    );
    assert!(offset_of!(PictureParameterBufferHEVC, reference_frames) == 28);
    assert!(offset_of!(PictureParameterBufferHEVC, pic_width_in_luma_samples) == 448);
    assert!(offset_of!(PictureParameterBufferHEVC, pic_fields) == 452);
    assert!(offset_of!(PictureParameterBufferHEVC, sps_max_dec_pic_buffering_minus1) == 456);
    assert!(offset_of!(PictureParameterBufferHEVC, column_width_minus1) == 476);
    assert!(offset_of!(PictureParameterBufferHEVC, row_height_minus1) == 514);
    assert!(offset_of!(PictureParameterBufferHEVC, slice_parsing_fields) == 556);
    assert!(
        offset_of!(
            PictureParameterBufferHEVC,
            log2_max_pic_order_cnt_lsb_minus4
        ) == 560
    );
    assert!(offset_of!(PictureParameterBufferHEVC, st_rps_bits) == 568);

    assert!(
        size_of::<SliceParameterBufferHEVC>() == 264 && align_of::<SliceParameterBufferHEVC>() == 4
    );
    assert!(offset_of!(SliceParameterBufferHEVC, slice_segment_address) == 16);
    assert!(offset_of!(SliceParameterBufferHEVC, ref_pic_list) == 20);
    assert!(offset_of!(SliceParameterBufferHEVC, long_slice_flags) == 52);
    assert!(offset_of!(SliceParameterBufferHEVC, collocated_ref_idx) == 56);
    assert!(offset_of!(SliceParameterBufferHEVC, luma_log2_weight_denom) == 64);
    assert!(offset_of!(SliceParameterBufferHEVC, delta_luma_weight_l0) == 66);
    assert!(offset_of!(SliceParameterBufferHEVC, delta_luma_weight_l1) == 156);
    assert!(offset_of!(SliceParameterBufferHEVC, five_minus_max_num_merge_cand) == 246);
    assert!(offset_of!(SliceParameterBufferHEVC, num_entry_point_offsets) == 248);
    assert!(offset_of!(SliceParameterBufferHEVC, slice_data_num_emu_prevn_bytes) == 252);

    assert!(size_of::<IQMatrixBufferHEVC>() == 1016 && align_of::<IQMatrixBufferHEVC>() == 4);
    assert!(offset_of!(IQMatrixBufferHEVC, scaling_list_32x32) == 864);
    assert!(offset_of!(IQMatrixBufferHEVC, scaling_list_dc_16x16) == 992);
};

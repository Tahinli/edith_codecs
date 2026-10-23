//! HEVC decode parameter buffers (`va.h:5287`, `va_dec_hevc.h`), as plain Rust
//! values.
//!
//! Every struct here carries the fields of the libva buffer it becomes, under
//! the same names. [`PictureParameterBufferHEVC::to_buffer`] and friends hand
//! them to cros-libva's typed constructors, which lay them out for the driver.

use ec_va::va::{
    BufferType, HevcLongSliceFlags, HevcPicFields, HevcSliceParsingFields, IQMatrix, PictureHEVC,
    PictureParameter, SliceParameter,
};
use ec_va::va::{
    IQMatrixBufferHEVC as CrosIQMatrix, PictureParameterBufferHEVC as CrosPicture,
    SliceParameterBufferHEVC as CrosSlice,
};

use super::INVALID_SURFACE;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VAPictureHEVC {
    /// Surface holding the picture, or [`INVALID_SURFACE`].
    pub picture_id: u32,
    /// `PicOrderCntVal`.
    pub pic_order_cnt: i32,
    /// `VA_PICTURE_HEVC_*` bitmask.
    pub flags: u32,
}

impl VAPictureHEVC {
    /// The "no picture here" entry for unused reference slots.
    pub const INVALID: VAPictureHEVC = VAPictureHEVC {
        picture_id: INVALID_SURFACE,
        pic_order_cnt: 0,
        flags: PICTURE_INVALID,
    };

    /// A reference or current picture.
    pub fn new(picture_id: u32, pic_order_cnt: i32, flags: u32) -> VAPictureHEVC {
        VAPictureHEVC {
            picture_id,
            pic_order_cnt,
            flags,
        }
    }

    fn to_cros(self) -> PictureHEVC {
        PictureHEVC::new(self.picture_id, self.pic_order_cnt, self.flags)
    }
}

impl Default for VAPictureHEVC {
    fn default() -> Self {
        VAPictureHEVC::INVALID
    }
}

/// `VAPictureParameterBufferHEVC::pic_fields`, one field per flag.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct PicFields {
    pub chroma_format_idc: u32,
    pub separate_colour_plane_flag: u32,
    pub pcm_enabled_flag: u32,
    pub scaling_list_enabled_flag: u32,
    pub transform_skip_enabled_flag: u32,
    pub amp_enabled_flag: u32,
    pub strong_intra_smoothing_enabled_flag: u32,
    pub sign_data_hiding_enabled_flag: u32,
    pub constrained_intra_pred_flag: u32,
    pub cu_qp_delta_enabled_flag: u32,
    pub weighted_pred_flag: u32,
    pub weighted_bipred_flag: u32,
    pub transquant_bypass_enabled_flag: u32,
    pub tiles_enabled_flag: u32,
    pub entropy_coding_sync_enabled_flag: u32,
    pub pps_loop_filter_across_slices_enabled_flag: u32,
    pub loop_filter_across_tiles_enabled_flag: u32,
    pub pcm_loop_filter_disabled_flag: u32,
    pub no_pic_reordering_flag: u32,
    pub no_bi_pred_flag: u32,
}

/// `VAPictureParameterBufferHEVC::slice_parsing_fields`, one field per flag.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct SliceParsingFields {
    pub lists_modification_present_flag: u32,
    pub long_term_ref_pics_present_flag: u32,
    pub sps_temporal_mvp_enabled_flag: u32,
    pub cabac_init_present_flag: u32,
    pub output_flag_present_flag: u32,
    pub dependent_slice_segments_enabled_flag: u32,
    pub pps_slice_chroma_qp_offsets_present_flag: u32,
    pub sample_adaptive_offset_enabled_flag: u32,
    pub deblocking_filter_override_enabled_flag: u32,
    pub pps_disable_deblocking_filter_flag: u32,
    pub slice_segment_header_extension_present_flag: u32,
    pub rap_pic_flag: u32,
    pub idr_pic_flag: u32,
    pub intra_pic_flag: u32,
}

/// `VAPictureParameterBufferHEVC`, `va_dec_hevc.h:57`.
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
    /// `column_width_minus1`.
    pub column_width_minus1: [u16; 19],
    /// `row_height_minus1`.
    pub row_height_minus1: [u16; 21],
    /// Slice parsing flags.
    pub slice_parsing_fields: SliceParsingFields,
    /// `log2_max_pic_order_cnt_lsb_minus4`.
    pub log2_max_pic_order_cnt_lsb_minus4: u8,
    /// `num_short_term_ref_pic_sets`.
    pub num_short_term_ref_pic_sets: u8,
    /// `num_long_term_ref_pic_sps`.
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
    /// Bits used by the short-term RPS in the slice header.
    pub st_rps_bits: u32,
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
        }
    }
}

impl PictureParameterBufferHEVC {
    /// The typed cros-libva buffer for this picture.
    pub fn to_buffer(&self) -> BufferType {
        let f = self.pic_fields;
        let pic = HevcPicFields::new(
            f.chroma_format_idc,
            f.separate_colour_plane_flag,
            f.pcm_enabled_flag,
            f.scaling_list_enabled_flag,
            f.transform_skip_enabled_flag,
            f.amp_enabled_flag,
            f.strong_intra_smoothing_enabled_flag,
            f.sign_data_hiding_enabled_flag,
            f.constrained_intra_pred_flag,
            f.cu_qp_delta_enabled_flag,
            f.weighted_pred_flag,
            f.weighted_bipred_flag,
            f.transquant_bypass_enabled_flag,
            f.tiles_enabled_flag,
            f.entropy_coding_sync_enabled_flag,
            f.pps_loop_filter_across_slices_enabled_flag,
            f.loop_filter_across_tiles_enabled_flag,
            f.pcm_loop_filter_disabled_flag,
            f.no_pic_reordering_flag,
            f.no_bi_pred_flag,
        );
        let s = self.slice_parsing_fields;
        let slice = HevcSliceParsingFields::new(
            s.lists_modification_present_flag,
            s.long_term_ref_pics_present_flag,
            s.sps_temporal_mvp_enabled_flag,
            s.cabac_init_present_flag,
            s.output_flag_present_flag,
            s.dependent_slice_segments_enabled_flag,
            s.pps_slice_chroma_qp_offsets_present_flag,
            s.sample_adaptive_offset_enabled_flag,
            s.deblocking_filter_override_enabled_flag,
            s.pps_disable_deblocking_filter_flag,
            s.slice_segment_header_extension_present_flag,
            s.rap_pic_flag,
            s.idr_pic_flag,
            s.intra_pic_flag,
        );
        let refs: [PictureHEVC; 15] = self.reference_frames.map(VAPictureHEVC::to_cros);
        BufferType::PictureParameter(PictureParameter::HEVC(CrosPicture::new(
            self.curr_pic.to_cros(),
            refs,
            self.pic_width_in_luma_samples,
            self.pic_height_in_luma_samples,
            &pic,
            self.sps_max_dec_pic_buffering_minus1,
            self.bit_depth_luma_minus8,
            self.bit_depth_chroma_minus8,
            self.pcm_sample_bit_depth_luma_minus1,
            self.pcm_sample_bit_depth_chroma_minus1,
            self.log2_min_luma_coding_block_size_minus3,
            self.log2_diff_max_min_luma_coding_block_size,
            self.log2_min_transform_block_size_minus2,
            self.log2_diff_max_min_transform_block_size,
            self.log2_min_pcm_luma_coding_block_size_minus3,
            self.log2_diff_max_min_pcm_luma_coding_block_size,
            self.max_transform_hierarchy_depth_intra,
            self.max_transform_hierarchy_depth_inter,
            self.init_qp_minus26,
            self.diff_cu_qp_delta_depth,
            self.pps_cb_qp_offset,
            self.pps_cr_qp_offset,
            self.log2_parallel_merge_level_minus2,
            self.num_tile_columns_minus1,
            self.num_tile_rows_minus1,
            self.column_width_minus1,
            self.row_height_minus1,
            &slice,
            self.log2_max_pic_order_cnt_lsb_minus4,
            self.num_short_term_ref_pic_sets,
            self.num_long_term_ref_pic_sps,
            self.num_ref_idx_l0_default_active_minus1,
            self.num_ref_idx_l1_default_active_minus1,
            self.pps_beta_offset_div2,
            self.pps_tc_offset_div2,
            self.num_extra_slice_header_bits,
            self.st_rps_bits,
        )))
    }
}

/// `VASliceParameterBufferHEVC::LongSliceFlags`, one field per flag.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct LongSliceFlags {
    pub last_slice_of_pic: u32,
    pub dependent_slice_segment_flag: u32,
    pub slice_type: u32,
    pub color_plane_id: u32,
    pub slice_sao_luma_flag: u32,
    pub slice_sao_chroma_flag: u32,
    pub mvd_l1_zero_flag: u32,
    pub cabac_init_flag: u32,
    pub slice_temporal_mvp_enabled_flag: u32,
    pub slice_deblocking_filter_disabled_flag: u32,
    pub collocated_from_l0_flag: u32,
    pub slice_loop_filter_across_slices_enabled_flag: u32,
}

/// `VASliceParameterBufferHEVC`, `va_dec_hevc.h:101`.
#[derive(Debug, Clone, Copy)]
pub struct SliceParameterBufferHEVC {
    /// Bytes of slice data for this slice.
    pub slice_data_size: u32,
    /// Offset of this slice's NAL unit within the data buffer.
    pub slice_data_offset: u32,
    /// `VA_SLICE_DATA_FLAG_*`.
    pub slice_data_flag: u32,
    /// Bits from the start of the NAL unit to `slice_data()`, in the
    /// *unescaped* RBSP.
    pub slice_data_byte_offset: u32,
    /// `slice_segment_address`.
    pub slice_segment_address: u32,
    /// `RefPicList[2][15]`: indices into `reference_frames`, `0xff` when unused.
    pub ref_pic_list: [[u8; 15]; 2],
    /// Per-slice flags.
    pub long_slice_flags: LongSliceFlags,
    /// `collocated_ref_idx`.
    pub collocated_ref_idx: u8,
    /// `num_ref_idx_l0_active_minus1`.
    pub num_ref_idx_l0_active_minus1: u8,
    /// `num_ref_idx_l1_active_minus1`.
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
    /// `slice_data_num_emu_prevn_bytes`.
    pub slice_data_num_emu_prevn_bytes: u16,
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
        }
    }
}

impl SliceParameterBufferHEVC {
    /// The typed cros-libva buffer for this slice.
    pub fn to_buffer(&self) -> BufferType {
        let f = self.long_slice_flags;
        let flags = HevcLongSliceFlags::new(
            f.last_slice_of_pic,
            f.dependent_slice_segment_flag,
            f.slice_type,
            f.color_plane_id,
            f.slice_sao_luma_flag,
            f.slice_sao_chroma_flag,
            f.mvd_l1_zero_flag,
            f.cabac_init_flag,
            f.slice_temporal_mvp_enabled_flag,
            f.slice_deblocking_filter_disabled_flag,
            f.collocated_from_l0_flag,
            f.slice_loop_filter_across_slices_enabled_flag,
        );
        BufferType::SliceParameter(SliceParameter::HEVC(CrosSlice::new(
            self.slice_data_size,
            self.slice_data_offset,
            self.slice_data_flag,
            self.slice_data_byte_offset,
            self.slice_segment_address,
            self.ref_pic_list,
            &flags,
            self.collocated_ref_idx,
            self.num_ref_idx_l0_active_minus1,
            self.num_ref_idx_l1_active_minus1,
            self.slice_qp_delta,
            self.slice_cb_qp_offset,
            self.slice_cr_qp_offset,
            self.slice_beta_offset_div2,
            self.slice_tc_offset_div2,
            self.luma_log2_weight_denom,
            self.delta_chroma_log2_weight_denom,
            self.delta_luma_weight_l0,
            self.luma_offset_l0,
            self.delta_chroma_weight_l0,
            self.chroma_offset_l0,
            self.delta_luma_weight_l1,
            self.luma_offset_l1,
            self.delta_chroma_weight_l1,
            self.chroma_offset_l1,
            self.five_minus_max_num_merge_cand,
            self.num_entry_point_offsets,
            self.entry_offset_to_subset_array,
            self.slice_data_num_emu_prevn_bytes,
        )))
    }
}

/// `VAIQMatrixBufferHEVC`, `va_dec_hevc.h`. Coefficients in raster order.
#[derive(Debug, Clone, Copy)]
pub struct IQMatrixBufferHEVC {
    /// `ScalingList4x4`.
    pub scaling_list_4x4: [[u8; 16]; 6],
    /// `ScalingList8x8`.
    pub scaling_list_8x8: [[u8; 64]; 6],
    /// `ScalingList16x16`.
    pub scaling_list_16x16: [[u8; 64]; 6],
    /// `ScalingList32x32`.
    pub scaling_list_32x32: [[u8; 64]; 2],
    /// `ScalingListDC16x16`.
    pub scaling_list_dc_16x16: [u8; 6],
    /// `ScalingListDC32x32`.
    pub scaling_list_dc_32x32: [u8; 2],
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
        }
    }
}

impl IQMatrixBufferHEVC {
    /// The typed cros-libva buffer for these scaling lists.
    pub fn to_buffer(&self) -> BufferType {
        BufferType::IQMatrix(IQMatrix::HEVC(CrosIQMatrix::new(
            self.scaling_list_4x4,
            self.scaling_list_8x8,
            self.scaling_list_16x16,
            self.scaling_list_32x32,
            self.scaling_list_dc_16x16,
            self.scaling_list_dc_32x32,
        )))
    }
}

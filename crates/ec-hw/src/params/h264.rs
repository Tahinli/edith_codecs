//! H.264 decode parameter buffers (`va.h:3572-3716`), as plain Rust values.
//!
//! Every struct here carries the fields of the libva buffer it becomes, under
//! the same names. [`PictureParameterBufferH264::to_buffer`] and friends hand
//! them to cros-libva's typed constructors, which lay them out for the driver.

use ec_va::va::{BufferType, IQMatrix, PictureParameter, SliceParameter};
use ec_va::va::{
    H264PicFields, H264SeqFields, IQMatrixBufferH264 as CrosIQMatrix, PictureH264,
    PictureParameterBufferH264 as CrosPicture, SliceParameterBufferH264 as CrosSlice,
};

use super::INVALID_SURFACE;

/// `VA_PICTURE_H264_INVALID` (`va.h:3583`).
pub const PICTURE_INVALID: u32 = 0x0000_0001;
/// `VA_PICTURE_H264_TOP_FIELD`.
pub const PICTURE_TOP_FIELD: u32 = 0x0000_0002;
/// `VA_PICTURE_H264_BOTTOM_FIELD`.
pub const PICTURE_BOTTOM_FIELD: u32 = 0x0000_0004;
/// `VA_PICTURE_H264_SHORT_TERM_REFERENCE`.
pub const PICTURE_SHORT_TERM_REFERENCE: u32 = 0x0000_0008;
/// `VA_PICTURE_H264_LONG_TERM_REFERENCE`.
pub const PICTURE_LONG_TERM_REFERENCE: u32 = 0x0000_0010;

/// `VAPictureH264`, `va.h:3572`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VAPictureH264 {
    /// Surface holding the picture, or [`INVALID_SURFACE`].
    pub picture_id: u32,
    /// `FrameNum` (short term) or `LongTermFrameIdx` (long term).
    pub frame_idx: u32,
    /// `VA_PICTURE_H264_*` bitmask.
    pub flags: u32,
    /// `TopFieldOrderCnt`.
    pub top_field_order_cnt: i32,
    /// `BottomFieldOrderCnt`.
    pub bottom_field_order_cnt: i32,
}

impl VAPictureH264 {
    /// The "no picture here" entry every unused list slot must carry.
    ///
    /// Not a zeroed entry by accident: zero names surface 0, which is a real
    /// surface id, and drivers have been known to read it.
    pub const INVALID: VAPictureH264 = VAPictureH264 {
        picture_id: INVALID_SURFACE,
        frame_idx: 0,
        flags: PICTURE_INVALID,
        top_field_order_cnt: 0,
        bottom_field_order_cnt: 0,
    };

    /// A frame picture with both field order counts set to its POC.
    pub fn frame(picture_id: u32, frame_idx: u32, poc: i32, flags: u32) -> VAPictureH264 {
        VAPictureH264 {
            picture_id,
            frame_idx,
            flags,
            top_field_order_cnt: poc,
            bottom_field_order_cnt: poc,
        }
    }

    fn to_cros(self) -> PictureH264 {
        PictureH264::new(
            self.picture_id,
            self.frame_idx,
            self.flags,
            self.top_field_order_cnt,
            self.bottom_field_order_cnt,
        )
    }
}

impl Default for VAPictureH264 {
    fn default() -> Self {
        VAPictureH264::INVALID
    }
}

/// `VAPictureParameterBufferH264::seq_fields`, one field per flag.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct SeqFields {
    pub chroma_format_idc: u32,
    pub residual_colour_transform_flag: u32,
    pub gaps_in_frame_num_value_allowed_flag: u32,
    pub frame_mbs_only_flag: u32,
    pub mb_adaptive_frame_field_flag: u32,
    pub direct_8x8_inference_flag: u32,
    pub min_luma_bipred_size8x8: u32,
    pub log2_max_frame_num_minus4: u32,
    pub pic_order_cnt_type: u32,
    pub log2_max_pic_order_cnt_lsb_minus4: u32,
    pub delta_pic_order_always_zero_flag: u32,
}

/// `VAPictureParameterBufferH264::pic_fields`, one field per flag.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct PicFields {
    pub entropy_coding_mode_flag: u32,
    pub weighted_pred_flag: u32,
    pub weighted_bipred_idc: u32,
    pub transform_8x8_mode_flag: u32,
    pub field_pic_flag: u32,
    pub constrained_intra_pred_flag: u32,
    pub pic_order_present_flag: u32,
    pub deblocking_filter_control_present_flag: u32,
    pub redundant_pic_cnt_present_flag: u32,
    pub reference_pic_flag: u32,
}

/// `VAPictureParameterBufferH264`, `va.h:3594`.
#[derive(Debug, Clone, Copy)]
pub struct PictureParameterBufferH264 {
    /// The picture being decoded.
    pub curr_pic: VAPictureH264,
    /// Every picture in the DPB; unused slots are [`VAPictureH264::INVALID`].
    pub reference_frames: [VAPictureH264; 16],
    /// `PicWidthInMbs - 1`.
    pub picture_width_in_mbs_minus1: u16,
    /// `PicHeightInMapUnits - 1` for a frame-only stream.
    pub picture_height_in_mbs_minus1: u16,
    /// `bit_depth_luma_minus8`.
    pub bit_depth_luma_minus8: u8,
    /// `bit_depth_chroma_minus8`.
    pub bit_depth_chroma_minus8: u8,
    /// `max_num_ref_frames`.
    pub num_ref_frames: u8,
    /// Sequence flags.
    pub seq_fields: SeqFields,
    /// Deprecated FMO fields; libva ignores them, we write zero.
    pub num_slice_groups_minus1: u8,
    /// Deprecated FMO field, always zero.
    pub slice_group_map_type: u8,
    /// Deprecated FMO field, always zero.
    pub slice_group_change_rate_minus1: u16,
    /// `pic_init_qp_minus26`.
    pub pic_init_qp_minus26: i8,
    /// `pic_init_qs_minus26`.
    pub pic_init_qs_minus26: i8,
    /// `chroma_qp_index_offset`.
    pub chroma_qp_index_offset: i8,
    /// `second_chroma_qp_index_offset`.
    pub second_chroma_qp_index_offset: i8,
    /// Picture flags.
    pub pic_fields: PicFields,
    /// `frame_num` of the current picture.
    pub frame_num: u16,
}

impl Default for PictureParameterBufferH264 {
    fn default() -> Self {
        PictureParameterBufferH264 {
            curr_pic: VAPictureH264::INVALID,
            reference_frames: [VAPictureH264::INVALID; 16],
            picture_width_in_mbs_minus1: 0,
            picture_height_in_mbs_minus1: 0,
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            num_ref_frames: 0,
            seq_fields: SeqFields::default(),
            num_slice_groups_minus1: 0,
            slice_group_map_type: 0,
            slice_group_change_rate_minus1: 0,
            pic_init_qp_minus26: 0,
            pic_init_qs_minus26: 0,
            chroma_qp_index_offset: 0,
            second_chroma_qp_index_offset: 0,
            pic_fields: PicFields::default(),
            frame_num: 0,
        }
    }
}

impl PictureParameterBufferH264 {
    /// The typed cros-libva buffer for this picture.
    pub fn to_buffer(&self) -> BufferType {
        let s = self.seq_fields;
        let seq = H264SeqFields::new(
            s.chroma_format_idc,
            s.residual_colour_transform_flag,
            s.gaps_in_frame_num_value_allowed_flag,
            s.frame_mbs_only_flag,
            s.mb_adaptive_frame_field_flag,
            s.direct_8x8_inference_flag,
            s.min_luma_bipred_size8x8,
            s.log2_max_frame_num_minus4,
            s.pic_order_cnt_type,
            s.log2_max_pic_order_cnt_lsb_minus4,
            s.delta_pic_order_always_zero_flag,
        );
        let p = self.pic_fields;
        let pic = H264PicFields::new(
            p.entropy_coding_mode_flag,
            p.weighted_pred_flag,
            p.weighted_bipred_idc,
            p.transform_8x8_mode_flag,
            p.field_pic_flag,
            p.constrained_intra_pred_flag,
            p.pic_order_present_flag,
            p.deblocking_filter_control_present_flag,
            p.redundant_pic_cnt_present_flag,
            p.reference_pic_flag,
        );
        let refs: [PictureH264; 16] = self.reference_frames.map(VAPictureH264::to_cros);
        BufferType::PictureParameter(PictureParameter::H264(CrosPicture::new(
            self.curr_pic.to_cros(),
            refs,
            self.picture_width_in_mbs_minus1,
            self.picture_height_in_mbs_minus1,
            self.bit_depth_luma_minus8,
            self.bit_depth_chroma_minus8,
            self.num_ref_frames,
            &seq,
            self.num_slice_groups_minus1,
            self.slice_group_map_type,
            self.slice_group_change_rate_minus1,
            self.pic_init_qp_minus26,
            self.pic_init_qs_minus26,
            self.chroma_qp_index_offset,
            self.second_chroma_qp_index_offset,
            &pic,
            self.frame_num,
        )))
    }
}

/// `VAIQMatrixBufferH264`, `va.h:3648`.
#[derive(Debug, Clone, Copy)]
pub struct IQMatrixBufferH264 {
    /// 4x4 scaling lists in raster scan order.
    pub scaling_list_4x4: [[u8; 16]; 6],
    /// 8x8 scaling lists in raster scan order.
    pub scaling_list_8x8: [[u8; 64]; 2],
}

impl Default for IQMatrixBufferH264 {
    fn default() -> Self {
        IQMatrixBufferH264 {
            scaling_list_4x4: [[16; 16]; 6],
            scaling_list_8x8: [[16; 64]; 2],
        }
    }
}

impl IQMatrixBufferH264 {
    /// The typed cros-libva buffer for these scaling lists.
    pub fn to_buffer(&self) -> BufferType {
        BufferType::IQMatrix(IQMatrix::H264(CrosIQMatrix::new(
            self.scaling_list_4x4,
            self.scaling_list_8x8,
        )))
    }
}

/// `VASliceParameterBufferH264`, `va.h:3659`.
///
/// The weight tables are the bulk of it: 3 KB per slice, which is why the
/// decoder builds one per slice and hands it straight to `vaCreateBuffer`.
#[derive(Debug, Clone, Copy)]
pub struct SliceParameterBufferH264 {
    /// Bytes of slice data for this slice.
    pub slice_data_size: u32,
    /// Offset of this slice's NAL unit within the data buffer.
    pub slice_data_offset: u32,
    /// `VA_SLICE_DATA_FLAG_*`.
    pub slice_data_flag: u32,
    /// Bits from the start of the NAL unit byte to `slice_data()`, counted in
    /// the *unescaped* RBSP.
    pub slice_data_bit_offset: u16,
    /// `first_mb_in_slice`.
    pub first_mb_in_slice: u16,
    /// `slice_type` (0..=4, i.e. the modulo-5 value).
    pub slice_type: u8,
    /// `direct_spatial_mv_pred_flag`.
    pub direct_spatial_mv_pred_flag: u8,
    /// `num_ref_idx_l0_active_minus1`, override applied.
    pub num_ref_idx_l0_active_minus1: u8,
    /// `num_ref_idx_l1_active_minus1`, override applied.
    pub num_ref_idx_l1_active_minus1: u8,
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
    /// `RefPicList0` after initialisation and modification (8.2.4).
    pub ref_pic_list0: [VAPictureH264; 32],
    /// `RefPicList1`.
    pub ref_pic_list1: [VAPictureH264; 32],
    /// `luma_log2_weight_denom`.
    pub luma_log2_weight_denom: u8,
    /// `chroma_log2_weight_denom`.
    pub chroma_log2_weight_denom: u8,
    /// Whether any `luma_weight_l0_flag` was set.
    pub luma_weight_l0_flag: u8,
    /// `luma_weight_l0`.
    pub luma_weight_l0: [i16; 32],
    /// `luma_offset_l0`.
    pub luma_offset_l0: [i16; 32],
    /// Whether any `chroma_weight_l0_flag` was set.
    pub chroma_weight_l0_flag: u8,
    /// `chroma_weight_l0`.
    pub chroma_weight_l0: [[i16; 2]; 32],
    /// `chroma_offset_l0`.
    pub chroma_offset_l0: [[i16; 2]; 32],
    /// Whether any `luma_weight_l1_flag` was set.
    pub luma_weight_l1_flag: u8,
    /// `luma_weight_l1`.
    pub luma_weight_l1: [i16; 32],
    /// `luma_offset_l1`.
    pub luma_offset_l1: [i16; 32],
    /// Whether any `chroma_weight_l1_flag` was set.
    pub chroma_weight_l1_flag: u8,
    /// `chroma_weight_l1`.
    pub chroma_weight_l1: [[i16; 2]; 32],
    /// `chroma_offset_l1`.
    pub chroma_offset_l1: [[i16; 2]; 32],
}

impl Default for SliceParameterBufferH264 {
    fn default() -> Self {
        SliceParameterBufferH264 {
            slice_data_size: 0,
            slice_data_offset: 0,
            slice_data_flag: 0,
            slice_data_bit_offset: 0,
            first_mb_in_slice: 0,
            slice_type: 0,
            direct_spatial_mv_pred_flag: 0,
            num_ref_idx_l0_active_minus1: 0,
            num_ref_idx_l1_active_minus1: 0,
            cabac_init_idc: 0,
            slice_qp_delta: 0,
            disable_deblocking_filter_idc: 0,
            slice_alpha_c0_offset_div2: 0,
            slice_beta_offset_div2: 0,
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
        }
    }
}

impl SliceParameterBufferH264 {
    /// The typed cros-libva buffer for this slice.
    pub fn to_buffer(&self) -> BufferType {
        let s = self;
        let l0 = s.ref_pic_list0.map(VAPictureH264::to_cros);
        let l1 = s.ref_pic_list1.map(VAPictureH264::to_cros);
        let slice = CrosSlice::new(
            s.slice_data_size,
            s.slice_data_offset,
            s.slice_data_flag,
            s.slice_data_bit_offset,
            s.first_mb_in_slice,
            s.slice_type,
            s.direct_spatial_mv_pred_flag,
            s.num_ref_idx_l0_active_minus1,
            s.num_ref_idx_l1_active_minus1,
            s.cabac_init_idc,
            s.slice_qp_delta,
            s.disable_deblocking_filter_idc,
            s.slice_alpha_c0_offset_div2,
            s.slice_beta_offset_div2,
            l0,
            l1,
            s.luma_log2_weight_denom,
            s.chroma_log2_weight_denom,
            s.luma_weight_l0_flag,
            s.luma_weight_l0,
            s.luma_offset_l0,
            s.chroma_weight_l0_flag,
            s.chroma_weight_l0,
            s.chroma_offset_l0,
            s.luma_weight_l1_flag,
            s.luma_weight_l1,
            s.luma_offset_l1,
            s.chroma_weight_l1_flag,
            s.chroma_weight_l1,
            s.chroma_offset_l1,
        );
        BufferType::SliceParameter(SliceParameter::H264(slice))
    }
}

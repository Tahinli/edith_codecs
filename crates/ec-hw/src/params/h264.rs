//! H.264 decode parameter buffers (`va.h:3572-3716`).

use ec_va::sys::{
    VAIQMatrixBufferType, VAPictureParameterBufferType, VASliceParameterBufferType, VASurfaceID,
};

use super::{INVALID_SURFACE, VaParam};
use crate::va_bits;

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
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VAPictureH264 {
    /// Surface holding the picture, or [`INVALID_SURFACE`].
    pub picture_id: VASurfaceID,
    /// `FrameNum` (short term) or `LongTermFrameIdx` (long term).
    pub frame_idx: u32,
    /// `VA_PICTURE_H264_*` bitmask.
    pub flags: u32,
    /// `TopFieldOrderCnt`.
    pub top_field_order_cnt: i32,
    /// `BottomFieldOrderCnt`.
    pub bottom_field_order_cnt: i32,
    pub(crate) va_reserved: [u32; 4],
}

impl VAPictureH264 {
    /// The "no picture here" entry every unused list slot must carry.
    ///
    /// Not `Default::default()` by accident: a zeroed entry names surface 0,
    /// which is a real surface id, and drivers have been known to read it.
    pub const INVALID: VAPictureH264 = VAPictureH264 {
        picture_id: INVALID_SURFACE,
        frame_idx: 0,
        flags: PICTURE_INVALID,
        top_field_order_cnt: 0,
        bottom_field_order_cnt: 0,
        va_reserved: [0; 4],
    };

    /// A frame picture with both field order counts set to its POC.
    pub fn frame(picture_id: VASurfaceID, frame_idx: u32, poc: i32, flags: u32) -> VAPictureH264 {
        VAPictureH264 {
            picture_id,
            frame_idx,
            flags,
            top_field_order_cnt: poc,
            bottom_field_order_cnt: poc,
            va_reserved: [0; 4],
        }
    }
}

impl Default for VAPictureH264 {
    fn default() -> Self {
        VAPictureH264::INVALID
    }
}

va_bits! {
    /// `VAPictureParameterBufferH264::seq_fields`.
    SeqFields: u32 {
        chroma_format_idc: 2,
        residual_colour_transform_flag: 1,
        gaps_in_frame_num_value_allowed_flag: 1,
        frame_mbs_only_flag: 1,
        mb_adaptive_frame_field_flag: 1,
        direct_8x8_inference_flag: 1,
        min_luma_bipred_size8x8: 1,
        log2_max_frame_num_minus4: 4,
        pic_order_cnt_type: 2,
        log2_max_pic_order_cnt_lsb_minus4: 4,
        delta_pic_order_always_zero_flag: 1,
    }
}

va_bits! {
    /// `VAPictureParameterBufferH264::pic_fields`.
    PicFields: u32 {
        entropy_coding_mode_flag: 1,
        weighted_pred_flag: 1,
        weighted_bipred_idc: 2,
        transform_8x8_mode_flag: 1,
        field_pic_flag: 1,
        constrained_intra_pred_flag: 1,
        pic_order_present_flag: 1,
        deblocking_filter_control_present_flag: 1,
        redundant_pic_cnt_present_flag: 1,
        reference_pic_flag: 1,
    }
}

/// `VAPictureParameterBufferH264`, `va.h:3594`.
#[repr(C)]
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
    pub(crate) num_slice_groups_minus1: u8,
    pub(crate) slice_group_map_type: u8,
    pub(crate) slice_group_change_rate_minus1: u16,
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
    pub(crate) va_reserved: [u32; 8],
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
            va_reserved: [0; 8],
        }
    }
}

// SAFETY: `#[repr(C)]` transcription of `VAPictureParameterBufferH264`, checked
// field for field by the assertion block below; every field is an integer or an
// array of integers, so any bit pattern is valid.
unsafe impl VaParam for PictureParameterBufferH264 {
    const TYPE: i32 = VAPictureParameterBufferType;
}

/// `VAIQMatrixBufferH264`, `va.h:3648`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct IQMatrixBufferH264 {
    /// 4x4 scaling lists in raster scan order.
    pub scaling_list_4x4: [[u8; 16]; 6],
    /// 8x8 scaling lists in raster scan order.
    pub scaling_list_8x8: [[u8; 64]; 2],
    pub(crate) va_reserved: [u32; 4],
}

impl Default for IQMatrixBufferH264 {
    fn default() -> Self {
        IQMatrixBufferH264 {
            scaling_list_4x4: [[16; 16]; 6],
            scaling_list_8x8: [[16; 64]; 2],
            va_reserved: [0; 4],
        }
    }
}

// SAFETY: as `PictureParameterBufferH264`; plain `u8` arrays plus padding.
unsafe impl VaParam for IQMatrixBufferH264 {
    const TYPE: i32 = VAIQMatrixBufferType;
}

/// `VASliceParameterBufferH264`, `va.h:3659`.
///
/// The weight tables are the bulk of it: 3 KB per slice, which is why the
/// decoder builds one on the stack and hands it straight to `vaCreateBuffer`
/// rather than keeping a queue of them.
#[repr(C)]
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
    pub(crate) va_reserved: [u32; 4],
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
            va_reserved: [0; 4],
        }
    }
}

// SAFETY: as `PictureParameterBufferH264`.
unsafe impl VaParam for SliceParameterBufferH264 {
    const TYPE: i32 = VASliceParameterBufferType;
}

// ---------------------------------------------------------------------------
// ABI transcription check — `crates/ec-hw/abi-probe.c`, libva 1.23.0, x86_64:
//
//   VAPictureH264                size=36   align=4  frame_idx=4 flags=8
//                                                   TopFieldOrderCnt=12
//                                                   BottomFieldOrderCnt=16
//   VAPictureParameterBufferH264 size=672  align=4  ReferenceFrames=36
//                                                   picture_width_in_mbs_minus1=612
//                                                   bit_depth_luma_minus8=616
//                                                   num_ref_frames=618 seq_fields=620
//                                                   pic_init_qp_minus26=628
//                                                   pic_fields=632 frame_num=636
//   VAIQMatrixBufferH264         size=240  align=4  ScalingList8x8=96
//   VASliceParameterBufferH264   size=3128 align=4  slice_data_bit_offset=12
//                                                   slice_type=16 cabac_init_idc=20
//                                                   RefPicList0=28 RefPicList1=1180
//                                                   luma_log2_weight_denom=2332
//                                                   luma_weight_l0=2336
//                                                   chroma_weight_l0=2466
//                                                   luma_weight_l1_flag=2722
//                                                   chroma_offset_l1=2982
const _: () = {
    use std::mem::{align_of, offset_of, size_of};

    assert!(size_of::<VAPictureH264>() == 36 && align_of::<VAPictureH264>() == 4);
    assert!(offset_of!(VAPictureH264, frame_idx) == 4);
    assert!(offset_of!(VAPictureH264, flags) == 8);
    assert!(offset_of!(VAPictureH264, top_field_order_cnt) == 12);
    assert!(offset_of!(VAPictureH264, bottom_field_order_cnt) == 16);

    assert!(
        size_of::<PictureParameterBufferH264>() == 672
            && align_of::<PictureParameterBufferH264>() == 4
    );
    assert!(offset_of!(PictureParameterBufferH264, reference_frames) == 36);
    assert!(offset_of!(PictureParameterBufferH264, picture_width_in_mbs_minus1) == 612);
    assert!(offset_of!(PictureParameterBufferH264, bit_depth_luma_minus8) == 616);
    assert!(offset_of!(PictureParameterBufferH264, num_ref_frames) == 618);
    assert!(offset_of!(PictureParameterBufferH264, seq_fields) == 620);
    assert!(offset_of!(PictureParameterBufferH264, pic_init_qp_minus26) == 628);
    assert!(offset_of!(PictureParameterBufferH264, pic_fields) == 632);
    assert!(offset_of!(PictureParameterBufferH264, frame_num) == 636);

    assert!(size_of::<IQMatrixBufferH264>() == 240 && align_of::<IQMatrixBufferH264>() == 4);
    assert!(offset_of!(IQMatrixBufferH264, scaling_list_8x8) == 96);

    assert!(
        size_of::<SliceParameterBufferH264>() == 3128
            && align_of::<SliceParameterBufferH264>() == 4
    );
    assert!(offset_of!(SliceParameterBufferH264, slice_data_bit_offset) == 12);
    assert!(offset_of!(SliceParameterBufferH264, slice_type) == 16);
    assert!(offset_of!(SliceParameterBufferH264, cabac_init_idc) == 20);
    assert!(offset_of!(SliceParameterBufferH264, ref_pic_list0) == 28);
    assert!(offset_of!(SliceParameterBufferH264, ref_pic_list1) == 1180);
    assert!(offset_of!(SliceParameterBufferH264, luma_log2_weight_denom) == 2332);
    assert!(offset_of!(SliceParameterBufferH264, luma_weight_l0) == 2336);
    assert!(offset_of!(SliceParameterBufferH264, chroma_weight_l0) == 2466);
    assert!(offset_of!(SliceParameterBufferH264, luma_weight_l1_flag) == 2722);
    assert!(offset_of!(SliceParameterBufferH264, chroma_offset_l1) == 2982);
};

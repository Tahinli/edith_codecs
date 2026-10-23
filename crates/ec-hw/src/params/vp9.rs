//! VP9 decode parameter buffers (`va_dec_vp9.h`), as plain Rust values.
//!
//! Every struct here carries the fields of the libva buffer it becomes, under
//! the same names. [`PictureParameterBufferVP9::to_buffer`] and friends hand
//! them to cros-libva's typed constructors, which lay them out for the driver.

use ec_va::va::{
    BufferType, PictureParameter, PictureParameterBufferVP9 as CrosPicture,
    SegmentParameterVP9 as CrosSegment, SliceParameter, SliceParameterBufferVP9 as CrosSlice,
    VP9PicFields, VP9SegmentFlags,
};

use super::INVALID_SURFACE;

/// The picture-level flags of `VADecPictureParameterBufferVP9::pic_fields`,
/// one field per bit as libva names them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PicFields {
    /// `subsampling_x`.
    pub subsampling_x: u32,
    /// `subsampling_y`.
    pub subsampling_y: u32,
    /// `frame_type`.
    pub frame_type: u32,
    /// `show_frame`.
    pub show_frame: u32,
    /// `error_resilient_mode`.
    pub error_resilient_mode: u32,
    /// `intra_only`.
    pub intra_only: u32,
    /// `allow_high_precision_mv`.
    pub allow_high_precision_mv: u32,
    /// `mcomp_filter_type`.
    pub mcomp_filter_type: u32,
    /// `frame_parallel_decoding_mode`.
    pub frame_parallel_decoding_mode: u32,
    /// `reset_frame_context`.
    pub reset_frame_context: u32,
    /// `refresh_frame_context`.
    pub refresh_frame_context: u32,
    /// `frame_context_idx`.
    pub frame_context_idx: u32,
    /// `segmentation_enabled`.
    pub segmentation_enabled: u32,
    /// `segmentation_temporal_update`.
    pub segmentation_temporal_update: u32,
    /// `segmentation_update_map`.
    pub segmentation_update_map: u32,
    /// `last_ref_frame`.
    pub last_ref_frame: u32,
    /// `last_ref_frame_sign_bias`.
    pub last_ref_frame_sign_bias: u32,
    /// `golden_ref_frame`.
    pub golden_ref_frame: u32,
    /// `golden_ref_frame_sign_bias`.
    pub golden_ref_frame_sign_bias: u32,
    /// `alt_ref_frame`.
    pub alt_ref_frame: u32,
    /// `alt_ref_frame_sign_bias`.
    pub alt_ref_frame_sign_bias: u32,
    /// `lossless_flag`.
    pub lossless_flag: u32,
}

impl PicFields {
    fn to_cros(self) -> VP9PicFields {
        VP9PicFields::new(
            self.subsampling_x,
            self.subsampling_y,
            self.frame_type,
            self.show_frame,
            self.error_resilient_mode,
            self.intra_only,
            self.allow_high_precision_mv,
            self.mcomp_filter_type,
            self.frame_parallel_decoding_mode,
            self.reset_frame_context,
            self.refresh_frame_context,
            self.frame_context_idx,
            self.segmentation_enabled,
            self.segmentation_temporal_update,
            self.segmentation_update_map,
            self.last_ref_frame,
            self.last_ref_frame_sign_bias,
            self.golden_ref_frame,
            self.golden_ref_frame_sign_bias,
            self.alt_ref_frame,
            self.alt_ref_frame_sign_bias,
            self.lossless_flag,
        )
    }
}

/// `VADecPictureParameterBufferVP9`, `va_dec_vp9.h:56`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PictureParameterBufferVP9 {
    /// `FrameWidth`.
    pub frame_width: u16,
    /// `FrameHeight`.
    pub frame_height: u16,
    /// The eight reference slots, `VA_INVALID_SURFACE` where empty.
    pub reference_frames: [u32; 8],
    /// Picture flags.
    pub pic_fields: PicFields,
    /// `loop_filter_level`.
    pub filter_level: u8,
    /// `loop_filter_sharpness`.
    pub sharpness_level: u8,
    /// `tile_rows_log2`.
    pub log2_tile_rows: u8,
    /// `tile_cols_log2`.
    pub log2_tile_columns: u8,
    /// Size of the uncompressed header in bytes.
    pub frame_header_length_in_bytes: u8,
    /// `header_size_in_bytes`: the compressed header length.
    pub first_partition_size: u16,
    /// `segmentation_tree_probs`.
    pub mb_segment_tree_probs: [u8; 7],
    /// `segmentation_pred_prob`.
    pub segment_pred_probs: [u8; 3],
    /// `profile`.
    pub profile: u8,
    /// `BitDepth`.
    pub bit_depth: u8,
}

impl Default for PictureParameterBufferVP9 {
    fn default() -> Self {
        PictureParameterBufferVP9 {
            frame_width: 0,
            frame_height: 0,
            reference_frames: [INVALID_SURFACE; 8],
            pic_fields: PicFields::default(),
            filter_level: 0,
            sharpness_level: 0,
            log2_tile_rows: 0,
            log2_tile_columns: 0,
            frame_header_length_in_bytes: 0,
            first_partition_size: 0,
            // 255 is the "no update" probability VP9 infers when segmentation
            // is off; zero would name a real probability.
            mb_segment_tree_probs: [255; 7],
            segment_pred_probs: [255; 3],
            profile: 0,
            bit_depth: 8,
        }
    }
}

impl PictureParameterBufferVP9 {
    /// The typed cros-libva buffer for these picture parameters.
    pub fn to_buffer(&self) -> BufferType {
        BufferType::PictureParameter(PictureParameter::VP9(CrosPicture::new(
            self.frame_width,
            self.frame_height,
            self.reference_frames,
            &self.pic_fields.to_cros(),
            self.filter_level,
            self.sharpness_level,
            self.log2_tile_rows,
            self.log2_tile_columns,
            self.frame_header_length_in_bytes,
            self.first_partition_size,
            self.mb_segment_tree_probs,
            self.segment_pred_probs,
            self.profile,
            self.bit_depth,
        )))
    }
}

/// `VASegmentParameterVP9::segment_flags`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SegmentFlags {
    /// `segment_reference_enabled`.
    pub segment_reference_enabled: u16,
    /// `segment_reference`.
    pub segment_reference: u16,
    /// `segment_reference_skipped`.
    pub segment_reference_skipped: u16,
}

/// `VASegmentParameterVP9`, `va_dec_vp9.h:196`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SegmentParameterVP9 {
    /// Segment feature flags.
    pub segment_flags: SegmentFlags,
    /// `LoopFilterLevel[ref][mode]` for this segment.
    pub filter_level: [[u8; 2]; 4],
    /// Dequantised AC scale, luma.
    pub luma_ac_quant_scale: i16,
    /// Dequantised DC scale, luma.
    pub luma_dc_quant_scale: i16,
    /// Dequantised AC scale, chroma.
    pub chroma_ac_quant_scale: i16,
    /// Dequantised DC scale, chroma.
    pub chroma_dc_quant_scale: i16,
}

impl SegmentParameterVP9 {
    fn to_cros(self) -> CrosSegment {
        let flags = self.segment_flags;
        let segment_flags = VP9SegmentFlags::new(
            flags.segment_reference_enabled,
            flags.segment_reference,
            flags.segment_reference_skipped,
        );
        CrosSegment::new(
            &segment_flags,
            self.filter_level,
            self.luma_ac_quant_scale,
            self.luma_dc_quant_scale,
            self.chroma_ac_quant_scale,
            self.chroma_dc_quant_scale,
        )
    }
}

/// `VASliceParameterBufferVP9`, `va_dec_vp9.h:245`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SliceParameterBufferVP9 {
    /// Bytes of frame data.
    pub slice_data_size: u32,
    /// Offset of the frame within the data buffer.
    pub slice_data_offset: u32,
    /// `VA_SLICE_DATA_FLAG_*`.
    pub slice_data_flag: u32,
    /// Per-segment parameters; all eight are always sent.
    pub seg_param: [SegmentParameterVP9; 8],
}

impl SliceParameterBufferVP9 {
    /// The typed cros-libva buffer for this frame's slice parameters.
    pub fn to_buffer(&self) -> BufferType {
        BufferType::SliceParameter(SliceParameter::VP9(CrosSlice::new(
            self.slice_data_size,
            self.slice_data_offset,
            self.slice_data_flag,
            self.seg_param.map(SegmentParameterVP9::to_cros),
        )))
    }
}

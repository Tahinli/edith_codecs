//! VP9 decode parameter buffers (`va_dec_vp9.h`).

use ec_va::sys::{VAPictureParameterBufferType, VASliceParameterBufferType, VASurfaceID};

use super::{INVALID_SURFACE, VaParam};
use crate::va_bits;

va_bits! {
    /// `VADecPictureParameterBufferVP9::pic_fields`.
    PicFields: u32 {
        subsampling_x: 1,
        subsampling_y: 1,
        frame_type: 1,
        show_frame: 1,
        error_resilient_mode: 1,
        intra_only: 1,
        allow_high_precision_mv: 1,
        mcomp_filter_type: 3,
        frame_parallel_decoding_mode: 1,
        reset_frame_context: 2,
        refresh_frame_context: 1,
        frame_context_idx: 2,
        segmentation_enabled: 1,
        segmentation_temporal_update: 1,
        segmentation_update_map: 1,
        last_ref_frame: 3,
        last_ref_frame_sign_bias: 1,
        golden_ref_frame: 3,
        golden_ref_frame_sign_bias: 1,
        alt_ref_frame: 3,
        alt_ref_frame_sign_bias: 1,
        lossless_flag: 1,
    }
}

/// `VADecPictureParameterBufferVP9`, `va_dec_vp9.h:56`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PictureParameterBufferVP9 {
    /// `FrameWidth`.
    pub frame_width: u16,
    /// `FrameHeight`.
    pub frame_height: u16,
    /// The eight reference slots, `VA_INVALID_SURFACE` where empty.
    pub reference_frames: [VASurfaceID; 8],
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
    pub(crate) va_reserved: [u32; 8],
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
            va_reserved: [0; 8],
        }
    }
}

// SAFETY: `#[repr(C)]` transcription of `VADecPictureParameterBufferVP9`,
// checked by the assertion block below; integers and integer arrays only.
unsafe impl VaParam for PictureParameterBufferVP9 {
    const TYPE: i32 = VAPictureParameterBufferType;
}

va_bits! {
    /// `VASegmentParameterVP9::segment_flags`.
    SegmentFlags: u16 {
        segment_reference_enabled: 1,
        segment_reference: 2,
        segment_reference_skipped: 1,
    }
}

/// `VASegmentParameterVP9`, `va_dec_vp9.h:196`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
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
    pub(crate) va_reserved: [u32; 4],
}

/// `VASliceParameterBufferVP9`, `va_dec_vp9.h:245`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SliceParameterBufferVP9 {
    /// Bytes of frame data.
    pub slice_data_size: u32,
    /// Offset of the frame within the data buffer.
    pub slice_data_offset: u32,
    /// `VA_SLICE_DATA_FLAG_*`.
    pub slice_data_flag: u32,
    /// Per-segment parameters; all eight are always sent.
    pub seg_param: [SegmentParameterVP9; 8],
    pub(crate) va_reserved: [u32; 4],
}

// SAFETY: as `PictureParameterBufferVP9`.
unsafe impl VaParam for SliceParameterBufferVP9 {
    const TYPE: i32 = VASliceParameterBufferType;
}

// ---------------------------------------------------------------------------
// ABI transcription check — `crates/ec-hw/abi-probe.c`, libva 1.23.0, x86_64:
//
//   VADecPictureParameterBufferVP9 size=92  align=4 reference_frames=4
//                                                   pic_fields=36 filter_level=40
//                                                   first_partition_size=46
//                                                   mb_segment_tree_probs=48
//                                                   profile=58
//   VASegmentParameterVP9          size=36  align=4 filter_level=2
//                                                   luma_ac_quant_scale=10
//   VASliceParameterBufferVP9      size=316 align=4 seg_param=12
const _: () = {
    use std::mem::{align_of, offset_of, size_of};

    assert!(
        size_of::<PictureParameterBufferVP9>() == 92
            && align_of::<PictureParameterBufferVP9>() == 4
    );
    assert!(offset_of!(PictureParameterBufferVP9, reference_frames) == 4);
    assert!(offset_of!(PictureParameterBufferVP9, pic_fields) == 36);
    assert!(offset_of!(PictureParameterBufferVP9, filter_level) == 40);
    assert!(offset_of!(PictureParameterBufferVP9, first_partition_size) == 46);
    assert!(offset_of!(PictureParameterBufferVP9, mb_segment_tree_probs) == 48);
    assert!(offset_of!(PictureParameterBufferVP9, profile) == 58);

    assert!(size_of::<SegmentParameterVP9>() == 36 && align_of::<SegmentParameterVP9>() == 4);
    assert!(offset_of!(SegmentParameterVP9, filter_level) == 2);
    assert!(offset_of!(SegmentParameterVP9, luma_ac_quant_scale) == 10);

    assert!(
        size_of::<SliceParameterBufferVP9>() == 316 && align_of::<SliceParameterBufferVP9>() == 4
    );
    assert!(offset_of!(SliceParameterBufferVP9, seg_param) == 12);
};

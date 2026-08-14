//! AV1 decode parameter buffers (`va_dec_av1.h`).

use std::ffi::c_void;

use ec_va::sys::{VAPictureParameterBufferType, VASliceParameterBufferType, VASurfaceID};

use super::{INVALID_SURFACE, VaParam};
use crate::va_bits;

va_bits! {
    /// `VASegmentationStructAV1::segment_info_fields`.
    SegmentInfoFields: u32 {
        enabled: 1,
        update_map: 1,
        temporal_update: 1,
        update_data: 1,
    }
}

/// `VASegmentationStructAV1`, `va_dec_av1.h:65`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SegmentationStructAV1 {
    /// Segmentation flags.
    pub segment_info_fields: SegmentInfoFields,
    /// `FeatureData[segment][feature]`.
    pub feature_data: [[i16; 8]; 8],
    /// `FeatureEnabled[segment]` as a bitmask over features.
    pub feature_mask: [u8; 8],
    pub(crate) va_reserved: [u32; 4],
}

va_bits! {
    /// `VAFilmGrainStructAV1::film_grain_info_fields`.
    FilmGrainInfoFields: u32 {
        apply_grain: 1,
        chroma_scaling_from_luma: 1,
        grain_scaling_minus_8: 2,
        ar_coeff_lag: 2,
        ar_coeff_shift_minus_6: 2,
        grain_scale_shift: 2,
        overlap_flag: 1,
        clip_to_restricted_range: 1,
    }
}

/// `VAFilmGrainStructAV1`, `va_dec_av1.h:100`.
///
/// Film grain synthesis is a display-side effect; the decoder fills this so a
/// driver that applies grain can, and edith's own path leaves `apply_grain`
/// clear (the frame it wants is the ungrained reconstruction).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct FilmGrainStructAV1 {
    /// Film grain flags.
    pub film_grain_info_fields: FilmGrainInfoFields,
    /// `grain_seed`.
    pub grain_seed: u16,
    /// `num_y_points`.
    pub num_y_points: u8,
    /// `point_y_value`.
    pub point_y_value: [u8; 14],
    /// `point_y_scaling`.
    pub point_y_scaling: [u8; 14],
    /// `num_cb_points`.
    pub num_cb_points: u8,
    /// `point_cb_value`.
    pub point_cb_value: [u8; 10],
    /// `point_cb_scaling`.
    pub point_cb_scaling: [u8; 10],
    /// `num_cr_points`.
    pub num_cr_points: u8,
    /// `point_cr_value`.
    pub point_cr_value: [u8; 10],
    /// `point_cr_scaling`.
    pub point_cr_scaling: [u8; 10],
    /// `ar_coeffs_y_plus_128 - 128`.
    pub ar_coeffs_y: [i8; 24],
    /// `ar_coeffs_cb_plus_128 - 128`.
    pub ar_coeffs_cb: [i8; 25],
    /// `ar_coeffs_cr_plus_128 - 128`.
    pub ar_coeffs_cr: [i8; 25],
    /// `cb_mult`.
    pub cb_mult: u8,
    /// `cb_luma_mult`.
    pub cb_luma_mult: u8,
    /// `cb_offset`.
    pub cb_offset: u16,
    /// `cr_mult`.
    pub cr_mult: u8,
    /// `cr_luma_mult`.
    pub cr_luma_mult: u8,
    /// `cr_offset`.
    pub cr_offset: u16,
    pub(crate) va_reserved: [u32; 4],
}

/// `VAWarpedMotionParamsAV1`, `va_dec_av1.h:180`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct WarpedMotionParamsAV1 {
    /// `VAAV1Transformation*`: identity, translation, rotzoom or affine.
    pub wmtype: i32,
    /// `gm_params`, six meaningful entries.
    pub wmmat: [i32; 8],
    /// Set when the model was rejected as non-invertible.
    pub invalid: u8,
    pub(crate) va_reserved: [u32; 4],
}

va_bits! {
    /// `VADecPictureParameterBufferAV1::seq_info_fields`.
    SeqInfoFields: u32 {
        still_picture: 1,
        use_128x128_superblock: 1,
        enable_filter_intra: 1,
        enable_intra_edge_filter: 1,
        enable_interintra_compound: 1,
        enable_masked_compound: 1,
        enable_dual_filter: 1,
        enable_order_hint: 1,
        enable_jnt_comp: 1,
        enable_cdef: 1,
        mono_chrome: 1,
        color_range: 1,
        subsampling_x: 1,
        subsampling_y: 1,
        chroma_sample_position: 1,
        film_grain_params_present: 1,
    }
}

va_bits! {
    /// `VADecPictureParameterBufferAV1::pic_info_fields`.
    PicInfoFields: u32 {
        frame_type: 2,
        show_frame: 1,
        showable_frame: 1,
        error_resilient_mode: 1,
        disable_cdf_update: 1,
        allow_screen_content_tools: 1,
        force_integer_mv: 1,
        allow_intrabc: 1,
        use_superres: 1,
        allow_high_precision_mv: 1,
        is_motion_mode_switchable: 1,
        use_ref_frame_mvs: 1,
        disable_frame_end_update_cdf: 1,
        uniform_tile_spacing_flag: 1,
        allow_warped_motion: 1,
        large_scale_tile: 1,
    }
}

va_bits! {
    /// `VADecPictureParameterBufferAV1::loop_filter_info_fields`.
    LoopFilterInfoFields: u8 {
        sharpness_level: 3,
        mode_ref_delta_enabled: 1,
        mode_ref_delta_update: 1,
    }
}

va_bits! {
    /// `VADecPictureParameterBufferAV1::qmatrix_fields`.
    QMatrixFields: u16 {
        using_qmatrix: 1,
        qm_y: 4,
        qm_u: 4,
        qm_v: 4,
    }
}

va_bits! {
    /// `VADecPictureParameterBufferAV1::mode_control_fields`.
    ModeControlFields: u32 {
        delta_q_present_flag: 1,
        log2_delta_q_res: 2,
        delta_lf_present_flag: 1,
        log2_delta_lf_res: 2,
        delta_lf_multi: 1,
        tx_mode: 2,
        reference_select: 1,
        reduced_tx_set_used: 1,
        skip_mode_present: 1,
    }
}

va_bits! {
    /// `VADecPictureParameterBufferAV1::loop_restoration_fields`.
    LoopRestorationFields: u16 {
        yframe_restoration_type: 2,
        cbframe_restoration_type: 2,
        crframe_restoration_type: 2,
        lr_unit_shift: 2,
        lr_uv_shift: 1,
    }
}

/// `VADecPictureParameterBufferAV1`, `va_dec_av1.h:206`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PictureParameterBufferAV1 {
    /// `seq_profile`.
    pub profile: u8,
    /// `OrderHintBits - 1`.
    pub order_hint_bits_minus_1: u8,
    /// 0 for 8-bit, 1 for 10-bit, 2 for 12-bit.
    pub bit_depth_idx: u8,
    /// `matrix_coefficients` (H.273).
    pub matrix_coefficients: u8,
    /// Sequence header flags.
    pub seq_info_fields: SeqInfoFields,
    /// Surface being decoded into.
    pub current_frame: VASurfaceID,
    /// Surface to display for a `show_existing_frame`, else the current frame.
    pub current_display_picture: VASurfaceID,
    /// Large scale tile only; always zero here.
    pub anchor_frames_num: u8,
    /// Large scale tile only; always NULL here.
    pub anchor_frames_list: *mut c_void,
    /// `FrameWidth - 1`.
    pub frame_width_minus1: u16,
    /// `FrameHeight - 1`.
    pub frame_height_minus1: u16,
    /// Large scale tile only.
    pub output_frame_width_in_tiles_minus_1: u16,
    /// Large scale tile only.
    pub output_frame_height_in_tiles_minus_1: u16,
    /// The eight reference slots.
    pub ref_frame_map: [VASurfaceID; 8],
    /// `ref_frame_idx`, LAST through ALTREF.
    pub ref_frame_idx: [u8; 7],
    /// `primary_ref_frame`.
    pub primary_ref_frame: u8,
    /// `OrderHint`.
    pub order_hint: u8,
    /// Segmentation parameters.
    pub seg_info: SegmentationStructAV1,
    /// Film grain parameters.
    pub film_grain_info: FilmGrainStructAV1,
    /// `TileCols`.
    pub tile_cols: u8,
    /// `TileRows`.
    pub tile_rows: u8,
    /// Per-column widths in superblocks, minus one.
    pub width_in_sbs_minus_1: [u16; 63],
    /// Per-row heights in superblocks, minus one.
    pub height_in_sbs_minus_1: [u16; 63],
    /// `TileCols * TileRows - 1`.
    pub tile_count_minus_1: u16,
    /// `context_update_tile_id`.
    pub context_update_tile_id: u16,
    /// Frame header flags.
    pub pic_info_fields: PicInfoFields,
    /// `SuperresDenom`.
    pub superres_scale_denominator: u8,
    /// `interpolation_filter`.
    pub interp_filter: u8,
    /// `loop_filter_level[0..2]`.
    pub filter_level: [u8; 2],
    /// `loop_filter_level[2]`.
    pub filter_level_u: u8,
    /// `loop_filter_level[3]`.
    pub filter_level_v: u8,
    /// Loop filter flags.
    pub loop_filter_info_fields: LoopFilterInfoFields,
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
    /// Quantiser matrix flags.
    pub qmatrix_fields: QMatrixFields,
    /// Mode control flags.
    pub mode_control_fields: ModeControlFields,
    /// `CdefDamping - 3`.
    pub cdef_damping_minus_3: u8,
    /// `cdef_bits`.
    pub cdef_bits: u8,
    /// Combined luma primary/secondary strengths, as libva packs them.
    pub cdef_y_strengths: [u8; 8],
    /// Combined chroma primary/secondary strengths.
    pub cdef_uv_strengths: [u8; 8],
    /// Loop restoration flags.
    pub loop_restoration_fields: LoopRestorationFields,
    /// Global motion for LAST..ALTREF.
    pub wm: [WarpedMotionParamsAV1; 7],
    pub(crate) va_reserved: [u32; 8],
}

impl Default for PictureParameterBufferAV1 {
    fn default() -> Self {
        PictureParameterBufferAV1 {
            profile: 0,
            order_hint_bits_minus_1: 0,
            bit_depth_idx: 0,
            matrix_coefficients: 2, // MC_UNSPECIFIED
            seq_info_fields: SeqInfoFields::default(),
            current_frame: INVALID_SURFACE,
            current_display_picture: INVALID_SURFACE,
            anchor_frames_num: 0,
            anchor_frames_list: std::ptr::null_mut(),
            frame_width_minus1: 0,
            frame_height_minus1: 0,
            output_frame_width_in_tiles_minus_1: 0,
            output_frame_height_in_tiles_minus_1: 0,
            ref_frame_map: [INVALID_SURFACE; 8],
            ref_frame_idx: [0; 7],
            primary_ref_frame: 0,
            order_hint: 0,
            seg_info: SegmentationStructAV1::default(),
            film_grain_info: FilmGrainStructAV1::default(),
            tile_cols: 1,
            tile_rows: 1,
            width_in_sbs_minus_1: [0; 63],
            height_in_sbs_minus_1: [0; 63],
            tile_count_minus_1: 0,
            context_update_tile_id: 0,
            pic_info_fields: PicInfoFields::default(),
            superres_scale_denominator: 8,
            interp_filter: 0,
            filter_level: [0; 2],
            filter_level_u: 0,
            filter_level_v: 0,
            loop_filter_info_fields: LoopFilterInfoFields::default(),
            ref_deltas: [0; 8],
            mode_deltas: [0; 2],
            base_qindex: 0,
            y_dc_delta_q: 0,
            u_dc_delta_q: 0,
            u_ac_delta_q: 0,
            v_dc_delta_q: 0,
            v_ac_delta_q: 0,
            qmatrix_fields: QMatrixFields::default(),
            mode_control_fields: ModeControlFields::default(),
            cdef_damping_minus_3: 0,
            cdef_bits: 0,
            cdef_y_strengths: [0; 8],
            cdef_uv_strengths: [0; 8],
            loop_restoration_fields: LoopRestorationFields::default(),
            wm: [WarpedMotionParamsAV1::default(); 7],
            va_reserved: [0; 8],
        }
    }
}

// SAFETY: `#[repr(C)]` transcription of `VADecPictureParameterBufferAV1`,
// checked by the assertion block below. The one pointer field is the large
// scale tile anchor list, which this crate always leaves NULL (that mode is out
// of scope), so no dangling pointer can reach the driver.
unsafe impl VaParam for PictureParameterBufferAV1 {
    const TYPE: i32 = VAPictureParameterBufferType;
}

/// `VASliceParameterBufferAV1`, `va_dec_av1.h:660` — one per tile.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SliceParameterBufferAV1 {
    /// Bytes of tile data.
    pub slice_data_size: u32,
    /// Offset of the tile within the data buffer.
    pub slice_data_offset: u32,
    /// `VA_SLICE_DATA_FLAG_*`.
    pub slice_data_flag: u32,
    /// Tile row.
    pub tile_row: u16,
    /// Tile column.
    pub tile_column: u16,
    pub(crate) tg_start: u16,
    pub(crate) tg_end: u16,
    /// Large scale tile only.
    pub anchor_frame_idx: u8,
    /// Large scale tile only.
    pub tile_idx_in_tile_list: u16,
    pub(crate) va_reserved: [u32; 4],
}

// SAFETY: as `PictureParameterBufferAV1`; integers only.
unsafe impl VaParam for SliceParameterBufferAV1 {
    const TYPE: i32 = VASliceParameterBufferType;
}

// ---------------------------------------------------------------------------
// ABI transcription check — `crates/ec-hw/abi-probe.c`, libva 1.23.0, x86_64:
//
//   VASegmentationStructAV1        size=156  align=4 feature_data=4
//                                                    feature_mask=132
//   VAFilmGrainStructAV1           size=176  align=4 grain_seed=4
//                                                    ar_coeffs_y=77 cr_offset=158
//   VAWarpedMotionParamsAV1        size=56   align=4 wmmat=4 invalid=36
//   VADecPictureParameterBufferAV1 size=1160 align=8 seq_info_fields=4
//                                                    current_frame=8
//                                                    anchor_frames_num=16
//                                                    anchor_frames_list=24
//                                                    frame_width_minus1=32
//                                                    ref_frame_map=40
//                                                    ref_frame_idx=72
//                                                    seg_info=84
//                                                    film_grain_info=240
//                                                    tile_cols=416
//                                                    width_in_sbs_minus_1=418
//                                                    height_in_sbs_minus_1=544
//                                                    tile_count_minus_1=670
//                                                    pic_info_fields=676
//                                                    superres_scale_denominator=680
//                                                    loop_filter_info_fields=686
//                                                    ref_deltas=687 base_qindex=697
//                                                    qmatrix_fields=704
//                                                    mode_control_fields=708
//                                                    cdef_damping_minus_3=712
//                                                    cdef_y_strengths=714
//                                                    loop_restoration_fields=730
//                                                    wm=732
//   VASliceParameterBufferAV1      size=40   align=4 tile_row=12
//                                                    anchor_frame_idx=20
//                                                    tile_idx_in_tile_list=22
const _: () = {
    use std::mem::{align_of, offset_of, size_of};

    assert!(size_of::<SegmentationStructAV1>() == 156 && align_of::<SegmentationStructAV1>() == 4);
    assert!(offset_of!(SegmentationStructAV1, feature_data) == 4);
    assert!(offset_of!(SegmentationStructAV1, feature_mask) == 132);

    assert!(size_of::<FilmGrainStructAV1>() == 176 && align_of::<FilmGrainStructAV1>() == 4);
    assert!(offset_of!(FilmGrainStructAV1, grain_seed) == 4);
    assert!(offset_of!(FilmGrainStructAV1, ar_coeffs_y) == 77);
    assert!(offset_of!(FilmGrainStructAV1, cr_offset) == 158);

    assert!(size_of::<WarpedMotionParamsAV1>() == 56 && align_of::<WarpedMotionParamsAV1>() == 4);
    assert!(offset_of!(WarpedMotionParamsAV1, wmmat) == 4);
    assert!(offset_of!(WarpedMotionParamsAV1, invalid) == 36);

    assert!(
        size_of::<PictureParameterBufferAV1>() == 1160
            && align_of::<PictureParameterBufferAV1>() == 8
    );
    assert!(offset_of!(PictureParameterBufferAV1, seq_info_fields) == 4);
    assert!(offset_of!(PictureParameterBufferAV1, current_frame) == 8);
    assert!(offset_of!(PictureParameterBufferAV1, anchor_frames_num) == 16);
    assert!(offset_of!(PictureParameterBufferAV1, anchor_frames_list) == 24);
    assert!(offset_of!(PictureParameterBufferAV1, frame_width_minus1) == 32);
    assert!(offset_of!(PictureParameterBufferAV1, ref_frame_map) == 40);
    assert!(offset_of!(PictureParameterBufferAV1, ref_frame_idx) == 72);
    assert!(offset_of!(PictureParameterBufferAV1, seg_info) == 84);
    assert!(offset_of!(PictureParameterBufferAV1, film_grain_info) == 240);
    assert!(offset_of!(PictureParameterBufferAV1, tile_cols) == 416);
    assert!(offset_of!(PictureParameterBufferAV1, width_in_sbs_minus_1) == 418);
    assert!(offset_of!(PictureParameterBufferAV1, height_in_sbs_minus_1) == 544);
    assert!(offset_of!(PictureParameterBufferAV1, tile_count_minus_1) == 670);
    assert!(offset_of!(PictureParameterBufferAV1, pic_info_fields) == 676);
    assert!(offset_of!(PictureParameterBufferAV1, superres_scale_denominator) == 680);
    assert!(offset_of!(PictureParameterBufferAV1, loop_filter_info_fields) == 686);
    assert!(offset_of!(PictureParameterBufferAV1, ref_deltas) == 687);
    assert!(offset_of!(PictureParameterBufferAV1, base_qindex) == 697);
    assert!(offset_of!(PictureParameterBufferAV1, qmatrix_fields) == 704);
    assert!(offset_of!(PictureParameterBufferAV1, mode_control_fields) == 708);
    assert!(offset_of!(PictureParameterBufferAV1, cdef_damping_minus_3) == 712);
    assert!(offset_of!(PictureParameterBufferAV1, cdef_y_strengths) == 714);
    assert!(offset_of!(PictureParameterBufferAV1, loop_restoration_fields) == 730);
    assert!(offset_of!(PictureParameterBufferAV1, wm) == 732);

    assert!(
        size_of::<SliceParameterBufferAV1>() == 40 && align_of::<SliceParameterBufferAV1>() == 4
    );
    assert!(offset_of!(SliceParameterBufferAV1, tile_row) == 12);
    assert!(offset_of!(SliceParameterBufferAV1, anchor_frame_idx) == 20);
    assert!(offset_of!(SliceParameterBufferAV1, tile_idx_in_tile_list) == 22);
};

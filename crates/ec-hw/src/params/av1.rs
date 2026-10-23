//! AV1 decode parameter buffers (`va_dec_av1.h`), as plain Rust values.
//!
//! Every struct here carries the fields of the libva buffer it becomes, under
//! the same names. The `to_buffer` methods hand them to cros-libva's typed
//! constructors, which lay them out for the driver.

use ec_va::va::{
    AV1FilmGrain, AV1FilmGrainFields, AV1LoopFilterFields, AV1LoopRestorationFields,
    AV1ModeControlFields, AV1PicInfoFields, AV1QMatrixFields, AV1SegmentInfoFields,
    AV1Segmentation, AV1SeqFields, AV1WarpedMotionParams, BufferType, PictureParameter,
    PictureParameterBufferAV1 as CrosPicture, SliceParameter, SliceParameterBufferAV1 as CrosSlice,
};

use super::INVALID_SURFACE;

/// `VASegmentationStructAV1::segment_info_fields`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SegmentInfoFields {
    /// `enabled`.
    pub enabled: u32,
    /// `update_map`.
    pub update_map: u32,
    /// `temporal_update`.
    pub temporal_update: u32,
    /// `update_data`.
    pub update_data: u32,
}

/// `VASegmentationStructAV1`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SegmentationStructAV1 {
    /// Segmentation flags.
    pub segment_info_fields: SegmentInfoFields,
    /// `FeatureData[segment][feature]`.
    pub feature_data: [[i16; 8]; 8],
    /// `FeatureEnabled` as one bit per feature, per segment.
    pub feature_mask: [u8; 8],
}

impl SegmentationStructAV1 {
    fn to_cros(self) -> AV1Segmentation {
        let f = self.segment_info_fields;
        let fields =
            AV1SegmentInfoFields::new(f.enabled, f.update_map, f.temporal_update, f.update_data);
        AV1Segmentation::new(&fields, self.feature_data, self.feature_mask)
    }
}

/// `VAFilmGrainStructAV1::film_grain_info_fields`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FilmGrainInfoFields {
    /// `apply_grain`.
    pub apply_grain: u32,
    /// `chroma_scaling_from_luma`.
    pub chroma_scaling_from_luma: u32,
    /// `grain_scaling_minus_8`.
    pub grain_scaling_minus_8: u32,
    /// `ar_coeff_lag`.
    pub ar_coeff_lag: u32,
    /// `ar_coeff_shift_minus_6`.
    pub ar_coeff_shift_minus_6: u32,
    /// `grain_scale_shift`.
    pub grain_scale_shift: u32,
    /// `overlap_flag`.
    pub overlap_flag: u32,
    /// `clip_to_restricted_range`.
    pub clip_to_restricted_range: u32,
}

/// `VAFilmGrainStructAV1`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FilmGrainStructAV1 {
    /// Grain flags.
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
    /// `ar_coeffs_y_plus_128` as signed coefficients.
    pub ar_coeffs_y: [i8; 24],
    /// `ar_coeffs_cb`.
    pub ar_coeffs_cb: [i8; 25],
    /// `ar_coeffs_cr`.
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
}

impl FilmGrainStructAV1 {
    fn to_cros(self) -> AV1FilmGrain {
        let f = self.film_grain_info_fields;
        let fields = AV1FilmGrainFields::new(
            f.apply_grain,
            f.chroma_scaling_from_luma,
            f.grain_scaling_minus_8,
            f.ar_coeff_lag,
            f.ar_coeff_shift_minus_6,
            f.grain_scale_shift,
            f.overlap_flag,
            f.clip_to_restricted_range,
        );
        AV1FilmGrain::new(
            &fields,
            self.grain_seed,
            self.num_y_points,
            self.point_y_value,
            self.point_y_scaling,
            self.num_cb_points,
            self.point_cb_value,
            self.point_cb_scaling,
            self.num_cr_points,
            self.point_cr_value,
            self.point_cr_scaling,
            self.ar_coeffs_y,
            self.ar_coeffs_cb,
            self.ar_coeffs_cr,
            self.cb_mult,
            self.cb_luma_mult,
            self.cb_offset,
            self.cr_mult,
            self.cr_luma_mult,
            self.cr_offset,
        )
    }
}

/// `VAWarpedMotionParamsAV1`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WarpedMotionParamsAV1 {
    /// `wmtype`: the transformation type (`VAAV1TransformationType`).
    pub wmtype: u32,
    /// `wmmat`.
    pub wmmat: [i32; 8],
    /// `invalid`.
    pub invalid: u8,
}

/// `VADecPictureParameterBufferAV1::seq_info_fields`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeqInfoFields {
    /// `still_picture`.
    pub still_picture: u32,
    /// `use_128x128_superblock`.
    pub use_128x128_superblock: u32,
    /// `enable_filter_intra`.
    pub enable_filter_intra: u32,
    /// `enable_intra_edge_filter`.
    pub enable_intra_edge_filter: u32,
    /// `enable_interintra_compound`.
    pub enable_interintra_compound: u32,
    /// `enable_masked_compound`.
    pub enable_masked_compound: u32,
    /// `enable_dual_filter`.
    pub enable_dual_filter: u32,
    /// `enable_order_hint`.
    pub enable_order_hint: u32,
    /// `enable_jnt_comp`.
    pub enable_jnt_comp: u32,
    /// `enable_cdef`.
    pub enable_cdef: u32,
    /// `mono_chrome`.
    pub mono_chrome: u32,
    /// `color_range`.
    pub color_range: u32,
    /// `subsampling_x`.
    pub subsampling_x: u32,
    /// `subsampling_y`.
    pub subsampling_y: u32,
    /// `chroma_sample_position`.
    pub chroma_sample_position: u32,
    /// `film_grain_params_present`.
    pub film_grain_params_present: u32,
}

/// `VADecPictureParameterBufferAV1::pic_info_fields`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PicInfoFields {
    /// `frame_type`.
    pub frame_type: u32,
    /// `show_frame`.
    pub show_frame: u32,
    /// `showable_frame`.
    pub showable_frame: u32,
    /// `error_resilient_mode`.
    pub error_resilient_mode: u32,
    /// `disable_cdf_update`.
    pub disable_cdf_update: u32,
    /// `allow_screen_content_tools`.
    pub allow_screen_content_tools: u32,
    /// `force_integer_mv`.
    pub force_integer_mv: u32,
    /// `allow_intrabc`.
    pub allow_intrabc: u32,
    /// `use_superres`.
    pub use_superres: u32,
    /// `allow_high_precision_mv`.
    pub allow_high_precision_mv: u32,
    /// `is_motion_mode_switchable`.
    pub is_motion_mode_switchable: u32,
    /// `use_ref_frame_mvs`.
    pub use_ref_frame_mvs: u32,
    /// `disable_frame_end_update_cdf`.
    pub disable_frame_end_update_cdf: u32,
    /// `uniform_tile_spacing_flag`.
    pub uniform_tile_spacing_flag: u32,
    /// `allow_warped_motion`.
    pub allow_warped_motion: u32,
    /// `large_scale_tile`.
    pub large_scale_tile: u32,
}

/// `VADecPictureParameterBufferAV1::loop_filter_info_fields`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoopFilterInfoFields {
    /// `sharpness_level`.
    pub sharpness_level: u8,
    /// `mode_ref_delta_enabled`.
    pub mode_ref_delta_enabled: u8,
    /// `mode_ref_delta_update`.
    pub mode_ref_delta_update: u8,
}

/// `VADecPictureParameterBufferAV1::qmatrix_fields`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QMatrixFields {
    /// `using_qmatrix`.
    pub using_qmatrix: u16,
    /// `qm_y`.
    pub qm_y: u16,
    /// `qm_u`.
    pub qm_u: u16,
    /// `qm_v`.
    pub qm_v: u16,
}

/// `VADecPictureParameterBufferAV1::mode_control_fields`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModeControlFields {
    /// `delta_q_present_flag`.
    pub delta_q_present_flag: u32,
    /// `log2_delta_q_res`.
    pub log2_delta_q_res: u32,
    /// `delta_lf_present_flag`.
    pub delta_lf_present_flag: u32,
    /// `log2_delta_lf_res`.
    pub log2_delta_lf_res: u32,
    /// `delta_lf_multi`.
    pub delta_lf_multi: u32,
    /// `tx_mode`.
    pub tx_mode: u32,
    /// `reference_select`.
    pub reference_select: u32,
    /// `reduced_tx_set_used`.
    pub reduced_tx_set_used: u32,
    /// `skip_mode_present`.
    pub skip_mode_present: u32,
}

/// `VADecPictureParameterBufferAV1::loop_restoration_fields`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoopRestorationFields {
    /// `yframe_restoration_type`.
    pub yframe_restoration_type: u16,
    /// `cbframe_restoration_type`.
    pub cbframe_restoration_type: u16,
    /// `crframe_restoration_type`.
    pub crframe_restoration_type: u16,
    /// `lr_unit_shift`.
    pub lr_unit_shift: u16,
    /// `lr_uv_shift`.
    pub lr_uv_shift: u16,
}

/// `VADecPictureParameterBufferAV1`, `va_dec_av1.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PictureParameterBufferAV1 {
    /// `profile`.
    pub profile: u8,
    /// `order_hint_bits_minus_1`.
    pub order_hint_bits_minus_1: u8,
    /// `bit_depth_idx`: 0 for 8-bit, 1 for 10-bit.
    pub bit_depth_idx: u8,
    /// `matrix_coefficients`.
    pub matrix_coefficients: u8,
    /// Sequence flags.
    pub seq_info_fields: SeqInfoFields,
    /// `current_frame`: the surface being decoded into.
    pub current_frame: u32,
    /// `current_display_picture`: the surface the grained output lands in, or
    /// the current frame when the frame carries no grain.
    pub current_display_picture: u32,
    /// `frame_width_minus1`.
    pub frame_width_minus1: u16,
    /// `frame_height_minus1`.
    pub frame_height_minus1: u16,
    /// `output_frame_width_in_tiles_minus_1` (large scale tile only).
    pub output_frame_width_in_tiles_minus_1: u16,
    /// `output_frame_height_in_tiles_minus_1` (large scale tile only).
    pub output_frame_height_in_tiles_minus_1: u16,
    /// `ref_frame_map`, `VA_INVALID_SURFACE` where empty.
    pub ref_frame_map: [u32; 8],
    /// `ref_frame_idx`.
    pub ref_frame_idx: [u8; 7],
    /// `primary_ref_frame`.
    pub primary_ref_frame: u8,
    /// `order_hint`.
    pub order_hint: u8,
    /// Segmentation state.
    pub seg_info: SegmentationStructAV1,
    /// Film grain state.
    pub film_grain_info: FilmGrainStructAV1,
    /// `tile_cols`.
    pub tile_cols: u8,
    /// `tile_rows`.
    pub tile_rows: u8,
    /// `width_in_sbs_minus_1`.
    pub width_in_sbs_minus_1: [u16; 63],
    /// `height_in_sbs_minus_1`.
    pub height_in_sbs_minus_1: [u16; 63],
    /// `tile_count_minus_1`.
    pub tile_count_minus_1: u16,
    /// `context_update_tile_id`.
    pub context_update_tile_id: u16,
    /// Picture flags.
    pub pic_info_fields: PicInfoFields,
    /// `superres_scale_denominator`.
    pub superres_scale_denominator: u8,
    /// `interp_filter`.
    pub interp_filter: u8,
    /// `filter_level`: luma vertical and horizontal.
    pub filter_level: [u8; 2],
    /// `filter_level_u`.
    pub filter_level_u: u8,
    /// `filter_level_v`.
    pub filter_level_v: u8,
    /// Loop filter flags.
    pub loop_filter_info_fields: LoopFilterInfoFields,
    /// `ref_deltas`.
    pub ref_deltas: [i8; 8],
    /// `mode_deltas`.
    pub mode_deltas: [i8; 2],
    /// `base_qindex`.
    pub base_qindex: u8,
    /// `y_dc_delta_q`.
    pub y_dc_delta_q: i8,
    /// `u_dc_delta_q`.
    pub u_dc_delta_q: i8,
    /// `u_ac_delta_q`.
    pub u_ac_delta_q: i8,
    /// `v_dc_delta_q`.
    pub v_dc_delta_q: i8,
    /// `v_ac_delta_q`.
    pub v_ac_delta_q: i8,
    /// Quantiser matrix flags.
    pub qmatrix_fields: QMatrixFields,
    /// Delta-q/lf and transform mode flags.
    pub mode_control_fields: ModeControlFields,
    /// `cdef_damping_minus_3`.
    pub cdef_damping_minus_3: u8,
    /// `cdef_bits`.
    pub cdef_bits: u8,
    /// `cdef_y_strengths`.
    pub cdef_y_strengths: [u8; 8],
    /// `cdef_uv_strengths`.
    pub cdef_uv_strengths: [u8; 8],
    /// Loop restoration flags.
    pub loop_restoration_fields: LoopRestorationFields,
    /// `wm`: global motion per reference frame.
    pub wm: [WarpedMotionParamsAV1; 7],
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
        }
    }
}

impl PictureParameterBufferAV1 {
    /// The typed cros-libva buffer for these picture parameters. No scale
    /// tile anchor list is sent: this crate does not decode large scale tile
    /// streams, so the anchor count is zero.
    pub fn to_buffer(&self) -> BufferType {
        let s = self.seq_info_fields;
        let seq = AV1SeqFields::new(
            s.still_picture,
            s.use_128x128_superblock,
            s.enable_filter_intra,
            s.enable_intra_edge_filter,
            s.enable_interintra_compound,
            s.enable_masked_compound,
            s.enable_dual_filter,
            s.enable_order_hint,
            s.enable_jnt_comp,
            s.enable_cdef,
            s.mono_chrome,
            s.color_range,
            s.subsampling_x,
            s.subsampling_y,
            s.chroma_sample_position,
            s.film_grain_params_present,
        );
        let p = self.pic_info_fields;
        let pic = AV1PicInfoFields::new(
            p.frame_type,
            p.show_frame,
            p.showable_frame,
            p.error_resilient_mode,
            p.disable_cdf_update,
            p.allow_screen_content_tools,
            p.force_integer_mv,
            p.allow_intrabc,
            p.use_superres,
            p.allow_high_precision_mv,
            p.is_motion_mode_switchable,
            p.use_ref_frame_mvs,
            p.disable_frame_end_update_cdf,
            p.uniform_tile_spacing_flag,
            p.allow_warped_motion,
            p.large_scale_tile,
        );
        let lf = self.loop_filter_info_fields;
        let loop_filter = AV1LoopFilterFields::new(
            lf.sharpness_level,
            lf.mode_ref_delta_enabled,
            lf.mode_ref_delta_update,
        );
        let q = self.qmatrix_fields;
        let qmatrix = AV1QMatrixFields::new(q.using_qmatrix, q.qm_y, q.qm_u, q.qm_v);
        let m = self.mode_control_fields;
        let mode = AV1ModeControlFields::new(
            m.delta_q_present_flag,
            m.log2_delta_q_res,
            m.delta_lf_present_flag,
            m.log2_delta_lf_res,
            m.delta_lf_multi,
            m.tx_mode,
            m.reference_select,
            m.reduced_tx_set_used,
            m.skip_mode_present,
        );
        let r = self.loop_restoration_fields;
        let restoration = AV1LoopRestorationFields::new(
            r.yframe_restoration_type,
            r.cbframe_restoration_type,
            r.crframe_restoration_type,
            r.lr_unit_shift,
            r.lr_uv_shift,
        );
        let wm: [AV1WarpedMotionParams; 7] = self
            .wm
            .map(|w| AV1WarpedMotionParams::new(w.wmtype, w.wmmat, w.invalid));
        BufferType::PictureParameter(PictureParameter::AV1(CrosPicture::new(
            self.profile,
            self.order_hint_bits_minus_1,
            self.bit_depth_idx,
            self.matrix_coefficients,
            &seq,
            self.current_frame,
            self.current_display_picture,
            Vec::new(),
            self.frame_width_minus1,
            self.frame_height_minus1,
            self.output_frame_width_in_tiles_minus_1,
            self.output_frame_height_in_tiles_minus_1,
            self.ref_frame_map,
            self.ref_frame_idx,
            self.primary_ref_frame,
            self.order_hint,
            &self.seg_info.to_cros(),
            &self.film_grain_info.to_cros(),
            self.tile_cols,
            self.tile_rows,
            self.width_in_sbs_minus_1,
            self.height_in_sbs_minus_1,
            self.tile_count_minus_1,
            self.context_update_tile_id,
            &pic,
            self.superres_scale_denominator,
            self.interp_filter,
            self.filter_level,
            self.filter_level_u,
            self.filter_level_v,
            &loop_filter,
            self.ref_deltas,
            self.mode_deltas,
            self.base_qindex,
            self.y_dc_delta_q,
            self.u_dc_delta_q,
            self.u_ac_delta_q,
            self.v_dc_delta_q,
            self.v_ac_delta_q,
            &qmatrix,
            &mode,
            self.cdef_damping_minus_3,
            self.cdef_bits,
            self.cdef_y_strengths,
            self.cdef_uv_strengths,
            &restoration,
            &wm,
        )))
    }
}

/// `VASliceParameterBufferAV1`, one per tile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SliceParameterBufferAV1 {
    /// `slice_data_size`.
    pub slice_data_size: u32,
    /// `slice_data_offset`.
    pub slice_data_offset: u32,
    /// `slice_data_flag`.
    pub slice_data_flag: u32,
    /// `tile_row`.
    pub tile_row: u16,
    /// `tile_column`.
    pub tile_column: u16,
    /// `tg_start`.
    pub tg_start: u16,
    /// `tg_end`.
    pub tg_end: u16,
    /// `anchor_frame_idx`.
    pub anchor_frame_idx: u8,
    /// `tile_idx_in_tile_list`.
    pub tile_idx_in_tile_list: u16,
}

/// The one slice-parameter buffer carrying every tile of a frame. cros-libva
/// sends the tiles as a single array, which is how the driver takes AV1 slices.
pub fn slice_buffer(tiles: &[SliceParameterBufferAV1]) -> BufferType {
    let mut buffer = CrosSlice::new();
    for t in tiles {
        buffer.add_slice_parameter(
            t.slice_data_size,
            t.slice_data_offset,
            t.slice_data_flag,
            t.tile_row,
            t.tile_column,
            t.tg_start,
            t.tg_end,
            t.anchor_frame_idx,
            t.tile_idx_in_tile_list,
        );
    }
    BufferType::SliceParameter(SliceParameter::AV1(buffer))
}

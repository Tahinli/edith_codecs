//! The frame header (spec 5.9) and the tile group header (spec 5.11.1).

use ec_core::{BitReader, Error, Result};

use crate::obu::{read_ns, read_su, tile_log2};
use crate::sequence::{SELECT_INTEGER_MV, SELECT_SCREEN_CONTENT_TOOLS, SequenceHeader};
use crate::{
    MAX_SEGMENTS, MAX_TILE_AREA, MAX_TILE_COLS, MAX_TILE_ROWS, MAX_TILE_WIDTH, NUM_REF_FRAMES,
    PRIMARY_REF_NONE, REFS_PER_FRAME, RESTORATION_TILESIZE_MAX, SEG_LVL_ALT_Q, SEG_LVL_MAX,
    SEG_LVL_REF_FRAME, TOTAL_REFS_PER_FRAME,
};

/// Bits carried by each segment feature (spec 5.9.14, `Segmentation_Feature_Bits`).
const SEG_FEATURE_BITS: [u32; SEG_LVL_MAX] = [8, 6, 6, 6, 6, 3, 0, 0];
/// Whether each segment feature is signed (spec 5.9.14, `Segmentation_Feature_Signed`).
const SEG_FEATURE_SIGNED: [bool; SEG_LVL_MAX] = [true, true, true, true, true, false, false, false];
/// Clamp applied to each segment feature (spec 5.9.14, `Segmentation_Feature_Max`).
const SEG_FEATURE_MAX: [i32; SEG_LVL_MAX] = [255, 63, 63, 63, 63, 7, 0, 0];
/// Loop filter deltas after `setup_past_independence` (spec 7.8): intra, then
/// LAST, LAST2, LAST3, GOLDEN, BWDREF, ALTREF2, ALTREF.
const DEFAULT_REF_DELTAS: [i8; TOTAL_REFS_PER_FRAME] = [1, 0, 0, 0, -1, 0, -1, -1];
/// `Remap_Lr_Type` (spec 5.9.20).
const REMAP_LR_TYPE: [RestorationType; 4] = [
    RestorationType::None,
    RestorationType::Switchable,
    RestorationType::Wiener,
    RestorationType::Sgrproj,
];
/// `WARPEDMODEL_PREC_BITS` (spec 3).
const WARPEDMODEL_PREC_BITS: u32 = 16;

/// Frame type (spec 6.8.2, `frame_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FrameType {
    /// `KEY_FRAME`, 0.
    Key = 0,
    /// `INTER_FRAME`, 1.
    Inter = 1,
    /// `INTRA_ONLY_FRAME`, 2.
    IntraOnly = 2,
    /// `SWITCH_FRAME`, 3.
    Switch = 3,
}

impl FrameType {
    fn from_code(code: u32) -> FrameType {
        match code {
            0 => FrameType::Key,
            1 => FrameType::Inter,
            2 => FrameType::IntraOnly,
            _ => FrameType::Switch,
        }
    }
}

/// Sub-pixel interpolation filter (spec 6.8.9, `interpolation_filter`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum InterpolationFilter {
    /// `EIGHTTAP`, 0.
    Eighttap = 0,
    /// `EIGHTTAP_SMOOTH`, 1.
    EighttapSmooth = 1,
    /// `EIGHTTAP_SHARP`, 2.
    EighttapSharp = 2,
    /// `BILINEAR`, 3.
    Bilinear = 3,
    /// `SWITCHABLE`, 4 — chosen per block.
    Switchable = 4,
}

/// Transform size mode (spec 6.8.21, `TxMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TxMode {
    /// `ONLY_4X4`, 0 — forced by lossless coding.
    Only4x4 = 0,
    /// `TX_MODE_LARGEST`, 1.
    Largest = 1,
    /// `TX_MODE_SELECT`, 2.
    Select = 2,
}

/// Loop restoration filter per plane (spec 6.10.15, `FrameRestorationType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum RestorationType {
    /// `RESTORE_NONE`, 0.
    None = 0,
    /// `RESTORE_WIENER`, 1.
    Wiener = 1,
    /// `RESTORE_SGRPROJ`, 2.
    Sgrproj = 2,
    /// `RESTORE_SWITCHABLE`, 3.
    Switchable = 3,
}

/// Warp model type (spec 6.8.20, `GmType`), matching `VAAV1TransformationType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum WarpModel {
    /// `IDENTITY`, 0.
    Identity = 0,
    /// `TRANSLATION`, 1.
    Translation = 1,
    /// `ROTZOOM`, 2.
    Rotzoom = 2,
    /// `AFFINE`, 3.
    Affine = 3,
}

/// Tile layout (spec 5.9.15, `tile_info`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileInfo {
    /// `uniform_tile_spacing_flag`.
    pub uniform_spacing: bool,
    /// `TileCols`.
    pub cols: u32,
    /// `TileRows`.
    pub rows: u32,
    /// `TileColsLog2`.
    pub cols_log2: u32,
    /// `TileRowsLog2`.
    pub rows_log2: u32,
    /// `MiColStarts`, `TileCols + 1` entries, in 4x4 units.
    pub mi_col_starts: Vec<u32>,
    /// `MiRowStarts`, `TileRows + 1` entries, in 4x4 units.
    pub mi_row_starts: Vec<u32>,
    /// `context_update_tile_id`.
    pub context_update_tile_id: u32,
    /// `TileSizeBytes`, the width of the per-tile size field in a tile group.
    pub tile_size_bytes: u32,
}

impl Default for TileInfo {
    fn default() -> TileInfo {
        TileInfo {
            uniform_spacing: true,
            cols: 1,
            rows: 1,
            cols_log2: 0,
            rows_log2: 0,
            mi_col_starts: Vec::new(),
            mi_row_starts: Vec::new(),
            context_update_tile_id: 0,
            tile_size_bytes: 1,
        }
    }
}

impl TileInfo {
    /// `width_in_sbs_minus_1[i]` as `VADecPictureParameterBufferAV1` wants it:
    /// tile widths in superblocks, derived from the mi column starts.
    pub fn width_in_sbs_minus_1(&self, sb_shift: u32) -> Vec<u16> {
        starts_to_sizes(&self.mi_col_starts, sb_shift)
    }

    /// `height_in_sbs_minus_1[i]`, the row counterpart.
    pub fn height_in_sbs_minus_1(&self, sb_shift: u32) -> Vec<u16> {
        starts_to_sizes(&self.mi_row_starts, sb_shift)
    }
}

fn starts_to_sizes(starts: &[u32], sb_shift: u32) -> Vec<u16> {
    starts
        .windows(2)
        .map(|w| {
            let sbs = (w[1] - w[0]).div_ceil(1 << sb_shift);
            u16::try_from(sbs.saturating_sub(1)).unwrap_or(u16::MAX)
        })
        .collect()
}

/// Quantization parameters (spec 5.9.12, `quantization_params`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QuantizationParams {
    /// `base_q_idx`.
    pub base_q_idx: u8,
    /// `DeltaQYDc`.
    pub delta_q_y_dc: i8,
    /// `DeltaQUDc`.
    pub delta_q_u_dc: i8,
    /// `DeltaQUAc`.
    pub delta_q_u_ac: i8,
    /// `DeltaQVDc`.
    pub delta_q_v_dc: i8,
    /// `DeltaQVAc`.
    pub delta_q_v_ac: i8,
    /// `using_qmatrix`.
    pub using_qmatrix: bool,
    /// `qm_y`.
    pub qm_y: u8,
    /// `qm_u`.
    pub qm_u: u8,
    /// `qm_v`.
    pub qm_v: u8,
}

/// Segmentation parameters (spec 5.9.14, `segmentation_params`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SegmentationParams {
    /// `segmentation_enabled`.
    pub enabled: bool,
    /// `segmentation_update_map`.
    pub update_map: bool,
    /// `segmentation_temporal_update`.
    pub temporal_update: bool,
    /// `segmentation_update_data`.
    pub update_data: bool,
    /// `FeatureEnabled[segment][feature]`, features indexed by the `SEG_LVL_*`
    /// constants. `VASegmentationStructAV1::feature_mask` is this, bit-packed.
    pub feature_enabled: [[bool; SEG_LVL_MAX]; MAX_SEGMENTS],
    /// `FeatureData[segment][feature]`, already clipped to the spec's limits.
    pub feature_data: [[i16; SEG_LVL_MAX]; MAX_SEGMENTS],
    /// `SegIdPreSkip`: a segment id is coded before the skip flag.
    pub seg_id_pre_skip: bool,
    /// `LastActiveSegId`: the highest segment id with any feature enabled.
    pub last_active_seg_id: u8,
}

impl SegmentationParams {
    /// `VASegmentationStructAV1::feature_mask[segment]`: one bit per feature.
    pub fn feature_mask(&self, segment_id: usize) -> u8 {
        let mut mask = 0u8;
        for (feature, &enabled) in self.feature_enabled[segment_id].iter().enumerate() {
            if enabled {
                mask |= 1 << feature;
            }
        }
        mask
    }

    /// `seg_feature_active_idx` (spec 6.8.14).
    pub fn feature_active(&self, segment_id: usize, feature: usize) -> bool {
        self.enabled && self.feature_enabled[segment_id][feature]
    }
}

/// Quantizer and loop filter deltas coded per superblock (spec 5.9.17, 5.9.18).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeltaParams {
    /// `delta_q_present`.
    pub q_present: bool,
    /// `delta_q_res`, the log2 step of the quantizer delta.
    pub q_res: u8,
    /// `delta_lf_present`.
    pub lf_present: bool,
    /// `delta_lf_res`, the log2 step of the loop filter delta.
    pub lf_res: u8,
    /// `delta_lf_multi`: separate deltas per plane and direction.
    pub lf_multi: bool,
}

/// Loop filter parameters (spec 5.9.11, `loop_filter_params`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopFilterParams {
    /// `loop_filter_level[0..4]`: luma vertical, luma horizontal, U, V.
    pub level: [u8; 4],
    /// `loop_filter_sharpness`.
    pub sharpness: u8,
    /// `loop_filter_delta_enabled`.
    pub delta_enabled: bool,
    /// `loop_filter_delta_update`.
    pub delta_update: bool,
    /// `loop_filter_ref_deltas`, one per reference frame including intra.
    pub ref_deltas: [i8; TOTAL_REFS_PER_FRAME],
    /// `loop_filter_mode_deltas`.
    pub mode_deltas: [i8; 2],
}

impl Default for LoopFilterParams {
    fn default() -> LoopFilterParams {
        LoopFilterParams {
            level: [0; 4],
            sharpness: 0,
            delta_enabled: false,
            delta_update: false,
            ref_deltas: DEFAULT_REF_DELTAS,
            mode_deltas: [0; 2],
        }
    }
}

/// CDEF parameters (spec 5.9.19, `cdef_params`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CdefParams {
    /// `CdefDamping`, 3-6.
    pub damping: u8,
    /// `cdef_bits`: there are `1 << cdef_bits` strength pairs.
    pub bits: u8,
    /// `cdef_y_pri_strength`.
    pub y_pri_strength: [u8; 8],
    /// `cdef_y_sec_strength`, after the spec's 3-becomes-4 adjustment.
    pub y_sec_strength: [u8; 8],
    /// `cdef_uv_pri_strength`.
    pub uv_pri_strength: [u8; 8],
    /// `cdef_uv_sec_strength`, after the same adjustment.
    pub uv_sec_strength: [u8; 8],
}

impl Default for CdefParams {
    fn default() -> CdefParams {
        CdefParams {
            damping: 3,
            bits: 0,
            y_pri_strength: [0; 8],
            y_sec_strength: [0; 8],
            uv_pri_strength: [0; 8],
            uv_sec_strength: [0; 8],
        }
    }
}

impl CdefParams {
    /// `VADecPictureParameterBufferAV1::cdef_y_strengths`: primary strength in
    /// the upper four bits, secondary in the lower two, with the secondary
    /// strength back in its coded form (4 encodes as 3).
    pub fn y_strengths(&self) -> [u8; 8] {
        pack_strengths(&self.y_pri_strength, &self.y_sec_strength)
    }

    /// `VADecPictureParameterBufferAV1::cdef_uv_strengths`.
    pub fn uv_strengths(&self) -> [u8; 8] {
        pack_strengths(&self.uv_pri_strength, &self.uv_sec_strength)
    }
}

fn pack_strengths(pri: &[u8; 8], sec: &[u8; 8]) -> [u8; 8] {
    let mut out = [0u8; 8];
    for i in 0..8 {
        // The coded secondary strength is two bits; 4 was coded as 3.
        let coded_sec = if sec[i] == 4 { 3 } else { sec[i] };
        out[i] = (pri[i] << 2) | (coded_sec & 0x3);
    }
    out
}

/// Loop restoration parameters (spec 5.9.20, `lr_params`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopRestorationParams {
    /// `FrameRestorationType[plane]`.
    pub frame_restoration_type: [RestorationType; 3],
    /// `LoopRestorationSize[plane]`, in samples.
    pub loop_restoration_size: [u32; 3],
    /// `lr_unit_shift`, as coded — `VADecPictureParameterBufferAV1` wants it raw.
    pub lr_unit_shift: u8,
    /// `lr_uv_shift`, as coded.
    pub lr_uv_shift: u8,
    /// `UsesLr`: any plane has restoration enabled.
    pub uses_lr: bool,
}

impl Default for LoopRestorationParams {
    fn default() -> LoopRestorationParams {
        LoopRestorationParams {
            frame_restoration_type: [RestorationType::None; 3],
            loop_restoration_size: [RESTORATION_TILESIZE_MAX; 3],
            lr_unit_shift: 0,
            lr_uv_shift: 0,
            uses_lr: false,
        }
    }
}

/// Global motion for one reference frame (spec 5.9.24, `global_motion_params`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WarpParams {
    /// `GmType[ref]`.
    pub model: WarpModel,
    /// `gm_params[ref][0..6]`, in the spec's fixed point. The two extra slots of
    /// `VAWarpedMotionParamsAV1::wmmat` are not coded and stay zero.
    pub params: [i32; 6],
    /// `warpValid` (spec 7.11.3.6): false when the model's shear cannot be
    /// applied, in which case a decoder falls back to translation-only.
    pub invalid: bool,
}

impl Default for WarpParams {
    fn default() -> WarpParams {
        WarpParams {
            model: WarpModel::Identity,
            // The identity warp: unit diagonal in WARPEDMODEL_PREC_BITS fixed point.
            params: [
                0,
                0,
                1 << WARPEDMODEL_PREC_BITS,
                0,
                0,
                1 << WARPEDMODEL_PREC_BITS,
            ],
            invalid: false,
        }
    }
}

/// Film grain synthesis parameters (spec 5.9.30, `film_grain_params`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FilmGrainParams {
    /// `apply_grain`.
    pub apply_grain: bool,
    /// `grain_seed`.
    pub grain_seed: u16,
    /// `update_grain`: false means the parameters come from a reference frame.
    pub update_grain: bool,
    /// `film_grain_params_ref_idx`, when `update_grain` is false.
    pub film_grain_params_ref_idx: u8,
    /// `num_y_points`.
    pub num_y_points: u8,
    /// `point_y_value`.
    pub point_y_value: [u8; 14],
    /// `point_y_scaling`.
    pub point_y_scaling: [u8; 14],
    /// `chroma_scaling_from_luma`.
    pub chroma_scaling_from_luma: bool,
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
    /// `grain_scaling_minus_8`.
    pub grain_scaling_minus_8: u8,
    /// `ar_coeff_lag`.
    pub ar_coeff_lag: u8,
    /// `ar_coeffs_y_plus_128 - 128`.
    pub ar_coeffs_y: [i8; 24],
    /// `ar_coeffs_cb_plus_128 - 128`.
    pub ar_coeffs_cb: [i8; 25],
    /// `ar_coeffs_cr_plus_128 - 128`.
    pub ar_coeffs_cr: [i8; 25],
    /// `ar_coeff_shift_minus_6`.
    pub ar_coeff_shift_minus_6: u8,
    /// `grain_scale_shift`.
    pub grain_scale_shift: u8,
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
    /// `overlap_flag`.
    pub overlap_flag: bool,
    /// `clip_to_restricted_range`.
    pub clip_to_restricted_range: bool,
}

/// One decoded frame header (spec 5.9.2, `uncompressed_header`).
///
/// Field names follow the spec. Every field of `VADecPictureParameterBufferAV1`
/// is here or derived by a method on this type or on
/// [`crate::SequenceHeader`], except the surface ids and the large scale tile
/// fields, which belong to the caller's frame pool and to a mode this crate
/// does not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameHeader {
    /// `show_existing_frame`: output a reference frame and code nothing else.
    pub show_existing_frame: bool,
    /// `frame_to_show_map_idx`, the slot to output.
    pub frame_to_show_map_idx: u8,
    /// `frame_presentation_time`, when the decoder model asks for it.
    pub frame_presentation_time: u32,
    /// `display_frame_id`, when frame ids are present.
    pub display_frame_id: u32,
    /// `frame_type`.
    pub frame_type: FrameType,
    /// `FrameIsIntra`.
    pub frame_is_intra: bool,
    /// `show_frame`.
    pub show_frame: bool,
    /// `showable_frame`: this frame may later be output by a
    /// `show_existing_frame` header.
    pub showable_frame: bool,
    /// `error_resilient_mode`.
    pub error_resilient_mode: bool,
    /// `disable_cdf_update`.
    pub disable_cdf_update: bool,
    /// `allow_screen_content_tools`.
    pub allow_screen_content_tools: bool,
    /// `force_integer_mv`.
    pub force_integer_mv: bool,
    /// `current_frame_id`.
    pub current_frame_id: u32,
    /// `frame_size_override_flag`.
    pub frame_size_override_flag: bool,
    /// `OrderHint`.
    pub order_hint: u32,
    /// `primary_ref_frame`, or [`PRIMARY_REF_NONE`].
    pub primary_ref_frame: u8,
    /// `buffer_removal_time[opNum]`, when coded.
    pub buffer_removal_time: [u32; 32],
    /// `refresh_frame_flags`.
    pub refresh_frame_flags: u8,
    /// `ref_order_hint[i]`, when an error-resilient frame codes them.
    pub ref_order_hint: [u32; NUM_REF_FRAMES],
    /// `FrameWidth`, the coded width — after superres downscaling.
    pub frame_width: u32,
    /// `FrameHeight`.
    pub frame_height: u32,
    /// `UpscaledWidth`, the width after superres upscaling.
    pub upscaled_width: u32,
    /// `RenderWidth`.
    pub render_width: u32,
    /// `RenderHeight`.
    pub render_height: u32,
    /// `use_superres`.
    pub use_superres: bool,
    /// `SuperresDenom`, 8 (no scaling) through 16.
    pub superres_denom: u8,
    /// `MiCols`, the frame width in 4x4 units.
    pub mi_cols: u32,
    /// `MiRows`.
    pub mi_rows: u32,
    /// `allow_intrabc`.
    pub allow_intrabc: bool,
    /// `frame_refs_short_signaling`: only two references were coded and the
    /// rest were derived by `set_frame_refs`.
    pub frame_refs_short_signaling: bool,
    /// `ref_frame_idx[i]`, LAST through ALTREF.
    pub ref_frame_idx: [u8; REFS_PER_FRAME],
    /// `delta_frame_id_minus_1 + 1` per reference, when frame ids are present.
    pub delta_frame_id: [u32; REFS_PER_FRAME],
    /// `allow_high_precision_mv`.
    pub allow_high_precision_mv: bool,
    /// `interpolation_filter`.
    pub interpolation_filter: InterpolationFilter,
    /// `is_motion_mode_switchable`.
    pub is_motion_mode_switchable: bool,
    /// `use_ref_frame_mvs`.
    pub use_ref_frame_mvs: bool,
    /// `OrderHints[refFrame]`, indexed LAST through ALTREF.
    pub order_hints: [u32; REFS_PER_FRAME],
    /// `RefFrameSignBias[refFrame]`, indexed LAST through ALTREF.
    pub ref_frame_sign_bias: [bool; REFS_PER_FRAME],
    /// `disable_frame_end_update_cdf`.
    pub disable_frame_end_update_cdf: bool,
    /// Tile layout.
    pub tile_info: TileInfo,
    /// Quantization parameters.
    pub quantization: QuantizationParams,
    /// Segmentation parameters.
    pub segmentation: SegmentationParams,
    /// Per-superblock delta parameters.
    pub delta: DeltaParams,
    /// `CodedLossless`: every segment is lossless.
    pub coded_lossless: bool,
    /// `AllLossless`: `CodedLossless` and no superres scaling.
    pub all_lossless: bool,
    /// `LosslessArray[segment]`.
    pub lossless: [bool; MAX_SEGMENTS],
    /// Loop filter parameters.
    pub loop_filter: LoopFilterParams,
    /// CDEF parameters.
    pub cdef: CdefParams,
    /// Loop restoration parameters.
    pub loop_restoration: LoopRestorationParams,
    /// `TxMode`.
    pub tx_mode: TxMode,
    /// `reference_select`: compound prediction may be chosen per block.
    pub reference_select: bool,
    /// `skip_mode_present`.
    pub skip_mode_present: bool,
    /// `SkipModeFrame[0..2]`, the two references skip mode blends.
    pub skip_mode_frame: [u8; 2],
    /// `allow_warped_motion`.
    pub allow_warped_motion: bool,
    /// `reduced_tx_set`.
    pub reduced_tx_set: bool,
    /// `gm_params[ref]`, indexed LAST through ALTREF.
    pub global_motion: [WarpParams; REFS_PER_FRAME],
    /// Film grain parameters.
    pub film_grain: FilmGrainParams,
    /// Length of the frame header in bits, from the first bit of
    /// `uncompressed_header`. A tile group inside an `OBU_FRAME` starts at the
    /// next byte boundary after this.
    pub header_bits: u64,
}

impl Default for FrameHeader {
    fn default() -> FrameHeader {
        FrameHeader {
            show_existing_frame: false,
            frame_to_show_map_idx: 0,
            frame_presentation_time: 0,
            display_frame_id: 0,
            frame_type: FrameType::Key,
            frame_is_intra: true,
            show_frame: true,
            showable_frame: false,
            error_resilient_mode: false,
            disable_cdf_update: false,
            allow_screen_content_tools: false,
            force_integer_mv: false,
            current_frame_id: 0,
            frame_size_override_flag: false,
            order_hint: 0,
            primary_ref_frame: PRIMARY_REF_NONE,
            buffer_removal_time: [0; 32],
            refresh_frame_flags: 0,
            ref_order_hint: [0; NUM_REF_FRAMES],
            frame_width: 0,
            frame_height: 0,
            upscaled_width: 0,
            render_width: 0,
            render_height: 0,
            use_superres: false,
            superres_denom: 8,
            mi_cols: 0,
            mi_rows: 0,
            allow_intrabc: false,
            frame_refs_short_signaling: false,
            ref_frame_idx: [0; REFS_PER_FRAME],
            delta_frame_id: [0; REFS_PER_FRAME],
            allow_high_precision_mv: false,
            interpolation_filter: InterpolationFilter::Eighttap,
            is_motion_mode_switchable: false,
            use_ref_frame_mvs: false,
            order_hints: [0; REFS_PER_FRAME],
            ref_frame_sign_bias: [false; REFS_PER_FRAME],
            disable_frame_end_update_cdf: false,
            tile_info: TileInfo::default(),
            quantization: QuantizationParams::default(),
            segmentation: SegmentationParams::default(),
            delta: DeltaParams::default(),
            coded_lossless: false,
            all_lossless: false,
            lossless: [false; MAX_SEGMENTS],
            loop_filter: LoopFilterParams::default(),
            cdef: CdefParams::default(),
            loop_restoration: LoopRestorationParams::default(),
            tx_mode: TxMode::Select,
            reference_select: false,
            skip_mode_present: false,
            skip_mode_frame: [0; 2],
            allow_warped_motion: false,
            reduced_tx_set: false,
            global_motion: [WarpParams::default(); REFS_PER_FRAME],
            film_grain: FilmGrainParams::default(),
            header_bits: 0,
        }
    }
}

impl FrameHeader {
    /// `get_qindex(ignoreDeltaQ = 1, segmentId)` (spec 7.12.2).
    pub fn segment_qindex(&self, segment_id: usize) -> u8 {
        let base = self.quantization.base_q_idx as i32;
        if self.segmentation.feature_active(segment_id, SEG_LVL_ALT_Q) {
            let data = self.segmentation.feature_data[segment_id][SEG_LVL_ALT_Q] as i32;
            (base + data).clamp(0, 255) as u8
        } else {
            self.quantization.base_q_idx
        }
    }

    /// `superres_scale_denominator` as `VADecPictureParameterBufferAV1` names it.
    pub fn superres_scale_denominator(&self) -> u8 {
        self.superres_denom
    }
}

/// What the parser keeps for each reference slot: the reference frame update
/// process of spec 7.20, restricted to what a later header can ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferenceSlot {
    /// `RefValid`.
    pub valid: bool,
    /// `RefFrameType`.
    pub frame_type: FrameType,
    /// `RefFrameId`.
    pub frame_id: u32,
    /// `RefUpscaledWidth`.
    pub upscaled_width: u32,
    /// `RefFrameWidth`.
    pub frame_width: u32,
    /// `RefFrameHeight`.
    pub frame_height: u32,
    /// `RefRenderWidth`.
    pub render_width: u32,
    /// `RefRenderHeight`.
    pub render_height: u32,
    /// `RefMiCols`.
    pub mi_cols: u32,
    /// `RefMiRows`.
    pub mi_rows: u32,
    /// `RefOrderHint`.
    pub order_hint: u32,
    /// `SavedOrderHints[i][refFrame]`.
    pub saved_order_hints: [u32; REFS_PER_FRAME],
    /// `SavedGmParams`.
    pub gm_params: [WarpParams; REFS_PER_FRAME],
    /// `SavedLoopFilterParams`, the part `load_previous` restores.
    pub loop_filter: LoopFilterParams,
    /// `SavedSegmentationParams`.
    pub segmentation: SegmentationParams,
    /// `SavedFilmGrainParams`.
    pub film_grain: FilmGrainParams,
    /// `RefBitDepth`, for a caller sizing its surfaces.
    pub bit_depth: u8,
}

impl Default for ReferenceSlot {
    fn default() -> ReferenceSlot {
        ReferenceSlot {
            valid: false,
            frame_type: FrameType::Key,
            frame_id: 0,
            upscaled_width: 0,
            frame_width: 0,
            frame_height: 0,
            render_width: 0,
            render_height: 0,
            mi_cols: 0,
            mi_rows: 0,
            order_hint: 0,
            saved_order_hints: [0; REFS_PER_FRAME],
            gm_params: [WarpParams::default(); REFS_PER_FRAME],
            loop_filter: LoopFilterParams::default(),
            segmentation: SegmentationParams::default(),
            film_grain: FilmGrainParams::default(),
            bit_depth: 8,
        }
    }
}

/// The mutable decoder state a frame header parse both reads and writes.
#[derive(Debug, Clone)]
pub(crate) struct FrameState {
    pub(crate) refs: [ReferenceSlot; NUM_REF_FRAMES],
    /// `RefOrderHint[i]` for slots that have not been filled yet, kept apart
    /// from the slot so an error-resilient frame can correct it.
    pub(crate) ref_order_hint: [u32; NUM_REF_FRAMES],
    pub(crate) ref_valid: [bool; NUM_REF_FRAMES],
    pub(crate) current_frame_id: u32,
}

impl Default for FrameState {
    fn default() -> FrameState {
        FrameState {
            refs: [ReferenceSlot::default(); NUM_REF_FRAMES],
            ref_order_hint: [0; NUM_REF_FRAMES],
            ref_valid: [false; NUM_REF_FRAMES],
            current_frame_id: 0,
        }
    }
}

/// The frame header parse (spec 5.9.2), driven by [`crate::Av1Parser`].
pub(crate) fn parse_uncompressed_header(
    r: &mut BitReader<'_>,
    seq: &SequenceHeader,
    state: &mut FrameState,
    temporal_id: u8,
    spatial_id: u8,
) -> Result<FrameHeader> {
    let start_bit = r.bit_position();
    let mut h = FrameHeader::default();
    let all_frames = u8::MAX; // (1 << NUM_REF_FRAMES) - 1

    let id_len = if seq.frame_id_numbers_present_flag {
        seq.additional_frame_id_length + seq.delta_frame_id_length
    } else {
        0
    };

    if seq.reduced_still_picture_header {
        h.frame_type = FrameType::Key;
        h.frame_is_intra = true;
        h.show_frame = true;
    } else {
        h.show_existing_frame = r.read_bit()?;
        if h.show_existing_frame {
            h.frame_to_show_map_idx = r.read_bits(3)? as u8;
            if let Some(model) = seq.decoder_model_info
                && !seq.timing_info.is_some_and(|t| t.equal_picture_interval)
            {
                h.frame_presentation_time = r.read_bits(model.frame_presentation_time_length)?;
            }
            h.refresh_frame_flags = 0;
            if seq.frame_id_numbers_present_flag {
                h.display_frame_id = r.read_bits(id_len)?;
            }
            let slot = state.refs[h.frame_to_show_map_idx as usize];
            h.frame_type = slot.frame_type;
            h.frame_is_intra = matches!(h.frame_type, FrameType::Key | FrameType::IntraOnly);
            h.show_frame = true;
            h.frame_width = slot.frame_width;
            h.frame_height = slot.frame_height;
            h.upscaled_width = slot.upscaled_width;
            h.render_width = slot.render_width;
            h.render_height = slot.render_height;
            h.mi_cols = slot.mi_cols;
            h.mi_rows = slot.mi_rows;
            h.order_hint = slot.order_hint;
            h.segmentation = slot.segmentation;
            h.loop_filter = slot.loop_filter;
            if seq.film_grain_params_present {
                h.film_grain = slot.film_grain;
            }
            if h.frame_type == FrameType::Key {
                // Showing a hidden keyframe refreshes every slot with it
                // (spec 7.21, reference frame loading).
                h.refresh_frame_flags = all_frames;
            }
            h.header_bits = r.bit_position() - start_bit;
            return Ok(h);
        }

        h.frame_type = FrameType::from_code(r.read_bits(2)?);
        h.frame_is_intra = matches!(h.frame_type, FrameType::Key | FrameType::IntraOnly);
        h.show_frame = r.read_bit()?;
        if h.show_frame {
            if let Some(model) = seq.decoder_model_info
                && !seq.timing_info.is_some_and(|t| t.equal_picture_interval)
            {
                h.frame_presentation_time = r.read_bits(model.frame_presentation_time_length)?;
            }
            h.showable_frame = h.frame_type != FrameType::Key;
        } else {
            h.showable_frame = r.read_bit()?;
        }
        h.error_resilient_mode = if h.frame_type == FrameType::Switch
            || (h.frame_type == FrameType::Key && h.show_frame)
        {
            true
        } else {
            r.read_bit()?
        };
    }

    if h.frame_type == FrameType::Key && h.show_frame {
        state.ref_valid = [false; NUM_REF_FRAMES];
        state.ref_order_hint = [0; NUM_REF_FRAMES];
        for slot in state.refs.iter_mut() {
            slot.valid = false;
        }
    }

    h.disable_cdf_update = r.read_bit()?;
    h.allow_screen_content_tools =
        if seq.seq_force_screen_content_tools == SELECT_SCREEN_CONTENT_TOOLS {
            r.read_bit()?
        } else {
            seq.seq_force_screen_content_tools != 0
        };
    h.force_integer_mv = if h.allow_screen_content_tools {
        if seq.seq_force_integer_mv == SELECT_INTEGER_MV {
            r.read_bit()?
        } else {
            seq.seq_force_integer_mv != 0
        }
    } else {
        false
    };
    if h.frame_is_intra {
        h.force_integer_mv = true;
    }

    if seq.frame_id_numbers_present_flag {
        h.current_frame_id = r.read_bits(id_len)?;
        state.current_frame_id = h.current_frame_id;
    }

    h.frame_size_override_flag = if h.frame_type == FrameType::Switch {
        true
    } else if seq.reduced_still_picture_header {
        false
    } else {
        r.read_bit()?
    };
    h.order_hint = r.read_bits(seq.order_hint_bits)?;
    h.primary_ref_frame = if h.frame_is_intra || h.error_resilient_mode {
        PRIMARY_REF_NONE
    } else {
        r.read_bits(3)? as u8
    };

    if let Some(model) = seq.decoder_model_info
        && r.read_bit()?
    {
        {
            for (op_num, op) in seq.operating_points.iter().enumerate() {
                if !op.decoder_model_present {
                    continue;
                }
                let in_temporal = (op.idc >> temporal_id) & 1 != 0;
                let in_spatial = (op.idc >> (spatial_id + 8)) & 1 != 0;
                if op.idc == 0 || (in_temporal && in_spatial) {
                    let time = r.read_bits(model.buffer_removal_time_length)?;
                    if let Some(slot) = h.buffer_removal_time.get_mut(op_num) {
                        *slot = time;
                    }
                }
            }
        }
    }

    h.refresh_frame_flags =
        if h.frame_type == FrameType::Switch || (h.frame_type == FrameType::Key && h.show_frame) {
            all_frames
        } else {
            r.read_bits(8)? as u8
        };

    if (!h.frame_is_intra || h.refresh_frame_flags != all_frames)
        && h.error_resilient_mode
        && seq.enable_order_hint
    {
        for i in 0..NUM_REF_FRAMES {
            h.ref_order_hint[i] = r.read_bits(seq.order_hint_bits)?;
            if h.ref_order_hint[i] != state.ref_order_hint[i] {
                state.ref_valid[i] = false;
                state.refs[i].valid = false;
                state.ref_order_hint[i] = h.ref_order_hint[i];
                state.refs[i].order_hint = h.ref_order_hint[i];
            }
        }
    }

    if h.frame_is_intra {
        read_frame_size(r, seq, &mut h)?;
        read_render_size(r, &mut h)?;
        if h.allow_screen_content_tools && h.upscaled_width == h.frame_width {
            h.allow_intrabc = r.read_bit()?;
        }
    } else {
        h.frame_refs_short_signaling = if seq.enable_order_hint {
            r.read_bit()?
        } else {
            false
        };
        if h.frame_refs_short_signaling {
            let last_frame_idx = r.read_bits(3)? as u8;
            let gold_frame_idx = r.read_bits(3)? as u8;
            set_frame_refs(seq, state, &mut h, last_frame_idx, gold_frame_idx);
        }
        for i in 0..REFS_PER_FRAME {
            if !h.frame_refs_short_signaling {
                h.ref_frame_idx[i] = r.read_bits(3)? as u8;
            }
            if seq.frame_id_numbers_present_flag {
                h.delta_frame_id[i] = r.read_bits(seq.delta_frame_id_length)? + 1;
            }
        }
        if h.frame_size_override_flag && !h.error_resilient_mode {
            read_frame_size_with_refs(r, seq, state, &mut h)?;
        } else {
            read_frame_size(r, seq, &mut h)?;
            read_render_size(r, &mut h)?;
        }
        h.allow_high_precision_mv = if h.force_integer_mv {
            false
        } else {
            r.read_bit()?
        };
        h.interpolation_filter = read_interpolation_filter(r)?;
        h.is_motion_mode_switchable = r.read_bit()?;
        h.use_ref_frame_mvs = if h.error_resilient_mode || !seq.enable_ref_frame_mvs {
            false
        } else {
            r.read_bit()?
        };
        for i in 0..REFS_PER_FRAME {
            let hint = state.refs[h.ref_frame_idx[i] as usize].order_hint;
            h.order_hints[i] = hint;
            h.ref_frame_sign_bias[i] =
                seq.enable_order_hint && get_relative_dist(seq, hint, h.order_hint) > 0;
        }
    }

    h.disable_frame_end_update_cdf = if seq.reduced_still_picture_header || h.disable_cdf_update {
        true
    } else {
        r.read_bit()?
    };

    // setup_past_independence / load_previous (spec 6.8.2): the loop filter
    // deltas, segmentation data and global motion this frame starts from either
    // reset to their defaults or come from the primary reference frame.
    // `loop_filter_level`/`loop_filter_sharpness` are NOT part of that forward
    // (spec 7.20 only loads `LoopFilterRefDeltas`/`LoopFilterModeDeltas`) --
    // level is always parsed fresh in `read_loop_filter_params`, defaulting to
    // 0 for the chroma levels when that frame's luma levels are both 0 (its
    // own guard skips reading them). Forwarding the whole struct here left a
    // previous frame's nonzero chroma level in place on a frame that never
    // wrote one, over-filtering U/V at edges this frame's own header says are
    // off (lane-av1golden3: seed 47, frame 1 chroma vs ffmpeg).
    let mut lf = LoopFilterParams {
        level: [0; 4],
        sharpness: 0,
        ..LoopFilterParams::default()
    };
    let mut seg = SegmentationParams::default();
    let mut prev_gm_params = [WarpParams::default(); REFS_PER_FRAME];
    if h.primary_ref_frame != PRIMARY_REF_NONE {
        let prev = state.refs[h.ref_frame_idx[h.primary_ref_frame as usize] as usize];
        lf.ref_deltas = prev.loop_filter.ref_deltas;
        lf.mode_deltas = prev.loop_filter.mode_deltas;
        seg = prev.segmentation;
        prev_gm_params = prev.gm_params;
    }
    h.loop_filter = lf;
    h.segmentation = seg;

    read_tile_info(r, seq, &mut h)?;
    read_quantization_params(r, seq, &mut h)?;
    read_segmentation_params(r, &mut h)?;
    read_delta_q_params(r, &mut h)?;
    read_delta_lf_params(r, &mut h)?;

    for segment_id in 0..MAX_SEGMENTS {
        let qindex = h.segment_qindex(segment_id);
        let q = &h.quantization;
        h.lossless[segment_id] = qindex == 0
            && q.delta_q_y_dc == 0
            && q.delta_q_u_ac == 0
            && q.delta_q_u_dc == 0
            && q.delta_q_v_ac == 0
            && q.delta_q_v_dc == 0;
    }
    h.coded_lossless = h.lossless.iter().all(|&l| l);
    h.all_lossless = h.coded_lossless && h.frame_width == h.upscaled_width;

    read_loop_filter_params(r, seq, &mut h)?;
    read_cdef_params(r, seq, &mut h)?;
    read_lr_params(r, seq, &mut h)?;
    h.tx_mode = if h.coded_lossless {
        TxMode::Only4x4
    } else if r.read_bit()? {
        TxMode::Select
    } else {
        TxMode::Largest
    };
    h.reference_select = if h.frame_is_intra {
        false
    } else {
        r.read_bit()?
    };
    read_skip_mode_params(r, seq, state, &mut h)?;
    h.allow_warped_motion =
        if h.frame_is_intra || h.error_resilient_mode || !seq.enable_warped_motion {
            false
        } else {
            r.read_bit()?
        };
    h.reduced_tx_set = r.read_bit()?;
    read_global_motion_params(r, &mut h, &prev_gm_params)?;
    read_film_grain_params(r, seq, state, &mut h)?;

    h.header_bits = r.bit_position() - start_bit;
    Ok(h)
}

/// `get_relative_dist(a, b)` (spec 5.9.3).
fn get_relative_dist(seq: &SequenceHeader, a: u32, b: u32) -> i32 {
    if !seq.enable_order_hint {
        return 0;
    }
    let bits = seq.order_hint_bits;
    let diff = a as i32 - b as i32;
    let m = 1i32 << (bits - 1);
    (diff & (m - 1)) - (diff & m)
}

/// `frame_size()` (spec 5.9.5) including `superres_params` and `compute_image_size`.
fn read_frame_size(r: &mut BitReader<'_>, seq: &SequenceHeader, h: &mut FrameHeader) -> Result<()> {
    if h.frame_size_override_flag {
        h.frame_width = r.read_bits(seq.frame_width_bits)? + 1;
        h.frame_height = r.read_bits(seq.frame_height_bits)? + 1;
    } else {
        h.frame_width = seq.max_frame_width;
        h.frame_height = seq.max_frame_height;
    }
    read_superres_params(r, seq, h)?;
    compute_image_size(h);
    Ok(())
}

/// `superres_params()` (spec 5.9.8).
fn read_superres_params(
    r: &mut BitReader<'_>,
    seq: &SequenceHeader,
    h: &mut FrameHeader,
) -> Result<()> {
    const SUPERRES_DENOM_BITS: u32 = 3;
    const SUPERRES_DENOM_MIN: u32 = 9;
    const SUPERRES_NUM: u32 = 8;
    h.use_superres = if seq.enable_superres {
        r.read_bit()?
    } else {
        false
    };
    h.superres_denom = if h.use_superres {
        (r.read_bits(SUPERRES_DENOM_BITS)? + SUPERRES_DENOM_MIN) as u8
    } else {
        SUPERRES_NUM as u8
    };
    h.upscaled_width = h.frame_width;
    h.frame_width =
        (h.upscaled_width * SUPERRES_NUM + (h.superres_denom as u32 / 2)) / h.superres_denom as u32;
    Ok(())
}

/// `compute_image_size()` (spec 5.9.9).
fn compute_image_size(h: &mut FrameHeader) {
    h.mi_cols = 2 * ((h.frame_width + 7) >> 3);
    h.mi_rows = 2 * ((h.frame_height + 7) >> 3);
}

/// `render_size()` (spec 5.9.6).
fn read_render_size(r: &mut BitReader<'_>, h: &mut FrameHeader) -> Result<()> {
    if r.read_bit()? {
        h.render_width = r.read_bits(16)? + 1;
        h.render_height = r.read_bits(16)? + 1;
    } else {
        h.render_width = h.upscaled_width;
        h.render_height = h.frame_height;
    }
    Ok(())
}

/// `frame_size_with_refs()` (spec 5.9.7).
fn read_frame_size_with_refs(
    r: &mut BitReader<'_>,
    seq: &SequenceHeader,
    state: &FrameState,
    h: &mut FrameHeader,
) -> Result<()> {
    for i in 0..REFS_PER_FRAME {
        if r.read_bit()? {
            let slot = state.refs[h.ref_frame_idx[i] as usize];
            if !slot.valid {
                return Err(Error::corrupt(format!(
                    "AV1 frame_size_with_refs names empty reference slot {}",
                    h.ref_frame_idx[i]
                )));
            }
            h.upscaled_width = slot.upscaled_width;
            h.frame_width = h.upscaled_width;
            h.frame_height = slot.frame_height;
            h.render_width = slot.render_width;
            h.render_height = slot.render_height;
            read_superres_params(r, seq, h)?;
            compute_image_size(h);
            return Ok(());
        }
    }
    read_frame_size(r, seq, h)?;
    read_render_size(r, h)
}

/// `read_interpolation_filter()` (spec 5.9.10).
fn read_interpolation_filter(r: &mut BitReader<'_>) -> Result<InterpolationFilter> {
    if r.read_bit()? {
        return Ok(InterpolationFilter::Switchable);
    }
    Ok(match r.read_bits(2)? {
        0 => InterpolationFilter::Eighttap,
        1 => InterpolationFilter::EighttapSmooth,
        2 => InterpolationFilter::EighttapSharp,
        _ => InterpolationFilter::Bilinear,
    })
}

/// `set_frame_refs()` (spec 7.8): derive the five references that short
/// signaling does not code, from the order hints of the reference slots.
fn set_frame_refs(
    seq: &SequenceHeader,
    state: &FrameState,
    h: &mut FrameHeader,
    last_frame_idx: u8,
    gold_frame_idx: u8,
) {
    /// `Ref_Frame_List` (spec 7.8): the order the forward references are filled.
    /// Values are indices into `ref_frame_idx`, i.e. refFrame - LAST_FRAME.
    const REF_FRAME_LIST: [usize; 5] = [1, 2, 4, 5, 6]; // LAST2, LAST3, BWDREF, ALTREF2, ALTREF
    let mut assigned = [false; REFS_PER_FRAME];
    let mut idx = [0u8; REFS_PER_FRAME];
    idx[0] = last_frame_idx;
    idx[3] = gold_frame_idx;
    assigned[0] = true;
    assigned[3] = true;

    let mut used = [false; NUM_REF_FRAMES];
    used[last_frame_idx as usize % NUM_REF_FRAMES] = true;
    used[gold_frame_idx as usize % NUM_REF_FRAMES] = true;

    let cur_frame_hint = 1i64 << (seq.order_hint_bits.max(1) - 1);
    let mut shifted = [0i64; NUM_REF_FRAMES];
    for (i, hint) in shifted.iter_mut().enumerate() {
        *hint =
            cur_frame_hint + get_relative_dist(seq, state.refs[i].order_hint, h.order_hint) as i64;
    }

    // The latest backward reference becomes ALTREF, then the two earliest
    // backward references become BWDREF and ALTREF2.
    let find_latest_backward = |used: &[bool; NUM_REF_FRAMES]| -> Option<usize> {
        let mut best: Option<(usize, i64)> = None;
        for i in 0..NUM_REF_FRAMES {
            if !used[i] && shifted[i] >= cur_frame_hint && best.is_none_or(|(_, h)| shifted[i] >= h)
            {
                best = Some((i, shifted[i]));
            }
        }
        best.map(|(i, _)| i)
    };
    let find_earliest_backward = |used: &[bool; NUM_REF_FRAMES]| -> Option<usize> {
        let mut best: Option<(usize, i64)> = None;
        for i in 0..NUM_REF_FRAMES {
            if !used[i] && shifted[i] >= cur_frame_hint && best.is_none_or(|(_, h)| shifted[i] < h)
            {
                best = Some((i, shifted[i]));
            }
        }
        best.map(|(i, _)| i)
    };
    let find_latest_forward = |used: &[bool; NUM_REF_FRAMES]| -> Option<usize> {
        let mut best: Option<(usize, i64)> = None;
        for i in 0..NUM_REF_FRAMES {
            if !used[i] && shifted[i] < cur_frame_hint && best.is_none_or(|(_, h)| shifted[i] >= h)
            {
                best = Some((i, shifted[i]));
            }
        }
        best.map(|(i, _)| i)
    };

    for (slot, finder) in [
        (6usize, 0u8), // ALTREF: latest backward
        (4, 1),        // BWDREF: earliest backward
        (5, 1),        // ALTREF2: next earliest backward
    ] {
        let found = if finder == 0 {
            find_latest_backward(&used)
        } else {
            find_earliest_backward(&used)
        };
        if let Some(i) = found {
            idx[slot] = i as u8;
            assigned[slot] = true;
            used[i] = true;
        }
    }

    for &slot in REF_FRAME_LIST.iter() {
        if !assigned[slot]
            && let Some(i) = find_latest_forward(&used)
        {
            idx[slot] = i as u8;
            assigned[slot] = true;
            used[i] = true;
        }
    }

    // Anything still unset takes the earliest reference of all.
    let mut earliest = 0usize;
    let mut earliest_hint = i64::MAX;
    for (i, &hint) in shifted.iter().enumerate() {
        if hint < earliest_hint {
            earliest_hint = hint;
            earliest = i;
        }
    }
    for i in 0..REFS_PER_FRAME {
        if !assigned[i] {
            idx[i] = earliest as u8;
        }
    }
    h.ref_frame_idx = idx;
}

/// `tile_info()` (spec 5.9.15).
fn read_tile_info(r: &mut BitReader<'_>, seq: &SequenceHeader, h: &mut FrameHeader) -> Result<()> {
    let sb_cols = seq.sb_cols(h.mi_cols);
    let sb_rows = seq.sb_rows(h.mi_rows);
    let sb_shift = if seq.use_128x128_superblock { 5 } else { 4 };
    let sb_size = sb_shift + 2;
    let max_tile_width_sb = MAX_TILE_WIDTH >> sb_size;
    let mut max_tile_area_sb = MAX_TILE_AREA >> (2 * sb_size);
    let min_log2_tile_cols = tile_log2(max_tile_width_sb, sb_cols);
    let max_log2_tile_cols = tile_log2(1, sb_cols.min(MAX_TILE_COLS));
    let max_log2_tile_rows = tile_log2(1, sb_rows.min(MAX_TILE_ROWS));
    let min_log2_tiles = min_log2_tile_cols.max(tile_log2(max_tile_area_sb, sb_rows * sb_cols));

    let mut info = TileInfo {
        uniform_spacing: r.read_bit()?,
        ..TileInfo::default()
    };
    if info.uniform_spacing {
        info.cols_log2 = min_log2_tile_cols;
        while info.cols_log2 < max_log2_tile_cols {
            if r.read_bit()? {
                info.cols_log2 += 1;
            } else {
                break;
            }
        }
        let tile_width_sb = sb_cols.div_ceil(1 << info.cols_log2);
        let mut start_sb = 0;
        while start_sb < sb_cols {
            info.mi_col_starts.push(start_sb << sb_shift);
            start_sb += tile_width_sb;
        }
        info.mi_col_starts.push(h.mi_cols);
        info.cols = info.mi_col_starts.len() as u32 - 1;

        info.rows_log2 = min_log2_tiles.saturating_sub(info.cols_log2);
        while info.rows_log2 < max_log2_tile_rows {
            if r.read_bit()? {
                info.rows_log2 += 1;
            } else {
                break;
            }
        }
        let tile_height_sb = sb_rows.div_ceil(1 << info.rows_log2);
        let mut start_sb = 0;
        while start_sb < sb_rows {
            info.mi_row_starts.push(start_sb << sb_shift);
            start_sb += tile_height_sb;
        }
        info.mi_row_starts.push(h.mi_rows);
        info.rows = info.mi_row_starts.len() as u32 - 1;
    } else {
        let mut widest_tile_sb = 0;
        let mut start_sb = 0;
        while start_sb < sb_cols {
            info.mi_col_starts.push(start_sb << sb_shift);
            let max_width = (sb_cols - start_sb).min(max_tile_width_sb);
            let size_sb = read_ns(r, max_width)? + 1;
            widest_tile_sb = widest_tile_sb.max(size_sb);
            start_sb += size_sb;
            if info.mi_col_starts.len() > MAX_TILE_COLS as usize {
                return Err(Error::corrupt("AV1 tile_info: more than 64 tile columns"));
            }
        }
        info.mi_col_starts.push(h.mi_cols);
        info.cols = info.mi_col_starts.len() as u32 - 1;
        info.cols_log2 = tile_log2(1, info.cols);

        if min_log2_tiles > 0 {
            max_tile_area_sb = (sb_rows * sb_cols) >> (min_log2_tiles + 1);
        } else {
            max_tile_area_sb = sb_rows * sb_cols;
        }
        let max_tile_height_sb = (max_tile_area_sb / widest_tile_sb.max(1)).max(1);
        let mut start_sb = 0;
        while start_sb < sb_rows {
            info.mi_row_starts.push(start_sb << sb_shift);
            let max_height = (sb_rows - start_sb).min(max_tile_height_sb);
            let size_sb = read_ns(r, max_height)? + 1;
            start_sb += size_sb;
            if info.mi_row_starts.len() > MAX_TILE_ROWS as usize {
                return Err(Error::corrupt("AV1 tile_info: more than 64 tile rows"));
            }
        }
        info.mi_row_starts.push(h.mi_rows);
        info.rows = info.mi_row_starts.len() as u32 - 1;
        info.rows_log2 = tile_log2(1, info.rows);
    }

    if info.cols_log2 > 0 || info.rows_log2 > 0 {
        info.context_update_tile_id = r.read_bits(info.rows_log2 + info.cols_log2)?;
        info.tile_size_bytes = r.read_bits(2)? + 1;
    }
    h.tile_info = info;
    Ok(())
}

/// `read_delta_q()` (spec 5.9.13).
fn read_delta_q(r: &mut BitReader<'_>) -> Result<i8> {
    if r.read_bit()? {
        Ok(read_su(r, 7)? as i8)
    } else {
        Ok(0)
    }
}

/// `quantization_params()` (spec 5.9.12).
fn read_quantization_params(
    r: &mut BitReader<'_>,
    seq: &SequenceHeader,
    h: &mut FrameHeader,
) -> Result<()> {
    let color = &seq.color_config;
    let mut q = QuantizationParams {
        base_q_idx: r.read_bits(8)? as u8,
        ..QuantizationParams::default()
    };
    q.delta_q_y_dc = read_delta_q(r)?;
    if color.num_planes > 1 {
        let diff_uv_delta = if color.separate_uv_delta_q {
            r.read_bit()?
        } else {
            false
        };
        q.delta_q_u_dc = read_delta_q(r)?;
        q.delta_q_u_ac = read_delta_q(r)?;
        if diff_uv_delta {
            q.delta_q_v_dc = read_delta_q(r)?;
            q.delta_q_v_ac = read_delta_q(r)?;
        } else {
            q.delta_q_v_dc = q.delta_q_u_dc;
            q.delta_q_v_ac = q.delta_q_u_ac;
        }
    }
    q.using_qmatrix = r.read_bit()?;
    if q.using_qmatrix {
        q.qm_y = r.read_bits(4)? as u8;
        q.qm_u = r.read_bits(4)? as u8;
        q.qm_v = if color.separate_uv_delta_q {
            r.read_bits(4)? as u8
        } else {
            q.qm_u
        };
    }
    h.quantization = q;
    if std::env::var_os("EC_AV1_TRACE").is_some() {
        eprintln!(
            "TRACE quant_deltas base_q_idx={} y_dc={} u_dc={} u_ac={} v_dc={} v_ac={}",
            q.base_q_idx, q.delta_q_y_dc, q.delta_q_u_dc, q.delta_q_u_ac, q.delta_q_v_dc, q.delta_q_v_ac
        );
    }
    Ok(())
}

/// `segmentation_params()` (spec 5.9.14).
fn read_segmentation_params(r: &mut BitReader<'_>, h: &mut FrameHeader) -> Result<()> {
    let mut seg = h.segmentation;
    seg.enabled = r.read_bit()?;
    if seg.enabled {
        if h.primary_ref_frame == PRIMARY_REF_NONE {
            seg.update_map = true;
            seg.temporal_update = false;
            seg.update_data = true;
        } else {
            seg.update_map = r.read_bit()?;
            seg.temporal_update = if seg.update_map { r.read_bit()? } else { false };
            seg.update_data = r.read_bit()?;
        }
        if seg.update_data {
            for segment in 0..MAX_SEGMENTS {
                for feature in 0..SEG_LVL_MAX {
                    let enabled = r.read_bit()?;
                    seg.feature_enabled[segment][feature] = enabled;
                    let mut clipped = 0i32;
                    if enabled {
                        let bits = SEG_FEATURE_BITS[feature];
                        let limit = SEG_FEATURE_MAX[feature];
                        if SEG_FEATURE_SIGNED[feature] {
                            clipped = read_su(r, 1 + bits)?.clamp(-limit, limit);
                        } else {
                            let value = if bits > 0 {
                                r.read_bits(bits)? as i32
                            } else {
                                0
                            };
                            clipped = value.clamp(0, limit);
                        }
                    }
                    seg.feature_data[segment][feature] = clipped as i16;
                }
            }
        }
    } else {
        seg.update_map = false;
        seg.temporal_update = false;
        seg.update_data = false;
        seg.feature_enabled = [[false; SEG_LVL_MAX]; MAX_SEGMENTS];
        seg.feature_data = [[0; SEG_LVL_MAX]; MAX_SEGMENTS];
    }

    seg.seg_id_pre_skip = false;
    seg.last_active_seg_id = 0;
    for segment in 0..MAX_SEGMENTS {
        for feature in 0..SEG_LVL_MAX {
            if seg.feature_enabled[segment][feature] {
                seg.last_active_seg_id = segment as u8;
                if feature >= SEG_LVL_REF_FRAME {
                    seg.seg_id_pre_skip = true;
                }
            }
        }
    }
    h.segmentation = seg;
    Ok(())
}

/// `delta_q_params()` (spec 5.9.17).
fn read_delta_q_params(r: &mut BitReader<'_>, h: &mut FrameHeader) -> Result<()> {
    h.delta.q_res = 0;
    h.delta.q_present = false;
    if h.quantization.base_q_idx > 0 {
        h.delta.q_present = r.read_bit()?;
    }
    if h.delta.q_present {
        h.delta.q_res = r.read_bits(2)? as u8;
    }
    Ok(())
}

/// `delta_lf_params()` (spec 5.9.18).
fn read_delta_lf_params(r: &mut BitReader<'_>, h: &mut FrameHeader) -> Result<()> {
    h.delta.lf_present = false;
    h.delta.lf_res = 0;
    h.delta.lf_multi = false;
    if h.delta.q_present {
        if !h.allow_intrabc {
            h.delta.lf_present = r.read_bit()?;
        }
        if h.delta.lf_present {
            h.delta.lf_res = r.read_bits(2)? as u8;
            h.delta.lf_multi = r.read_bit()?;
        }
    }
    Ok(())
}

/// `loop_filter_params()` (spec 5.9.11).
fn read_loop_filter_params(
    r: &mut BitReader<'_>,
    seq: &SequenceHeader,
    h: &mut FrameHeader,
) -> Result<()> {
    if h.coded_lossless || h.allow_intrabc {
        h.loop_filter.level = [0; 4];
        h.loop_filter.ref_deltas = DEFAULT_REF_DELTAS;
        h.loop_filter.mode_deltas = [0; 2];
        return Ok(());
    }
    let lf = &mut h.loop_filter;
    lf.level[0] = r.read_bits(6)? as u8;
    lf.level[1] = r.read_bits(6)? as u8;
    if seq.color_config.num_planes > 1 && (lf.level[0] > 0 || lf.level[1] > 0) {
        lf.level[2] = r.read_bits(6)? as u8;
        lf.level[3] = r.read_bits(6)? as u8;
    }
    lf.sharpness = r.read_bits(3)? as u8;
    lf.delta_enabled = r.read_bit()?;
    lf.delta_update = false;
    if lf.delta_enabled {
        lf.delta_update = r.read_bit()?;
        if lf.delta_update {
            for i in 0..TOTAL_REFS_PER_FRAME {
                if r.read_bit()? {
                    lf.ref_deltas[i] = read_su(r, 7)? as i8;
                }
            }
            for i in 0..2 {
                if r.read_bit()? {
                    lf.mode_deltas[i] = read_su(r, 7)? as i8;
                }
            }
        }
    }
    Ok(())
}

/// `cdef_params()` (spec 5.9.19).
fn read_cdef_params(
    r: &mut BitReader<'_>,
    seq: &SequenceHeader,
    h: &mut FrameHeader,
) -> Result<()> {
    if h.coded_lossless || h.allow_intrabc || !seq.enable_cdef {
        h.cdef = CdefParams::default();
        return Ok(());
    }
    let mut cdef = CdefParams {
        damping: r.read_bits(2)? as u8 + 3,
        bits: r.read_bits(2)? as u8,
        ..CdefParams::default()
    };
    for i in 0..(1usize << cdef.bits) {
        cdef.y_pri_strength[i] = r.read_bits(4)? as u8;
        cdef.y_sec_strength[i] = r.read_bits(2)? as u8;
        if cdef.y_sec_strength[i] == 3 {
            cdef.y_sec_strength[i] += 1;
        }
        if seq.color_config.num_planes > 1 {
            cdef.uv_pri_strength[i] = r.read_bits(4)? as u8;
            cdef.uv_sec_strength[i] = r.read_bits(2)? as u8;
            if cdef.uv_sec_strength[i] == 3 {
                cdef.uv_sec_strength[i] += 1;
            }
        }
    }
    h.cdef = cdef;
    Ok(())
}

/// `lr_params()` (spec 5.9.20).
fn read_lr_params(r: &mut BitReader<'_>, seq: &SequenceHeader, h: &mut FrameHeader) -> Result<()> {
    if h.all_lossless || h.allow_intrabc || !seq.enable_restoration {
        h.loop_restoration = LoopRestorationParams::default();
        return Ok(());
    }
    let mut lr = LoopRestorationParams::default();
    let mut uses_chroma_lr = false;
    for plane in 0..seq.color_config.num_planes as usize {
        let lr_type = r.read_bits(2)? as usize;
        lr.frame_restoration_type[plane] = REMAP_LR_TYPE[lr_type];
        if lr.frame_restoration_type[plane] != RestorationType::None {
            lr.uses_lr = true;
            if plane > 0 {
                uses_chroma_lr = true;
            }
        }
    }
    if lr.uses_lr {
        let mut lr_unit_shift;
        if seq.use_128x128_superblock {
            lr_unit_shift = r.read_bits(1)? + 1;
        } else {
            lr_unit_shift = r.read_bits(1)?;
            if lr_unit_shift > 0 {
                lr_unit_shift += r.read_bits(1)?;
            }
        }
        lr.lr_unit_shift = lr_unit_shift as u8;
        lr.loop_restoration_size[0] = RESTORATION_TILESIZE_MAX >> (2 - lr_unit_shift);
        let lr_uv_shift = if seq.color_config.subsampling_x == 1
            && seq.color_config.subsampling_y == 1
            && uses_chroma_lr
        {
            r.read_bits(1)?
        } else {
            0
        };
        lr.lr_uv_shift = lr_uv_shift as u8;
        lr.loop_restoration_size[1] = lr.loop_restoration_size[0] >> lr_uv_shift;
        lr.loop_restoration_size[2] = lr.loop_restoration_size[1];
    }
    h.loop_restoration = lr;
    Ok(())
}

/// `skip_mode_params()` (spec 5.9.22).
fn read_skip_mode_params(
    r: &mut BitReader<'_>,
    seq: &SequenceHeader,
    state: &FrameState,
    h: &mut FrameHeader,
) -> Result<()> {
    let mut skip_mode_allowed = false;
    if !h.frame_is_intra && h.reference_select && seq.enable_order_hint {
        let mut forward_idx: i32 = -1;
        let mut backward_idx: i32 = -1;
        let mut forward_hint = 0u32;
        let mut backward_hint = 0u32;
        for i in 0..REFS_PER_FRAME {
            let ref_hint = state.refs[h.ref_frame_idx[i] as usize].order_hint;
            if get_relative_dist(seq, ref_hint, h.order_hint) < 0 {
                if forward_idx < 0 || get_relative_dist(seq, ref_hint, forward_hint) > 0 {
                    forward_idx = i as i32;
                    forward_hint = ref_hint;
                }
            } else if get_relative_dist(seq, ref_hint, h.order_hint) > 0
                && (backward_idx < 0 || get_relative_dist(seq, ref_hint, backward_hint) < 0)
            {
                backward_idx = i as i32;
                backward_hint = ref_hint;
            }
        }
        if forward_idx >= 0 {
            if backward_idx >= 0 {
                skip_mode_allowed = true;
                h.skip_mode_frame = [
                    1 + forward_idx.min(backward_idx) as u8,
                    1 + forward_idx.max(backward_idx) as u8,
                ];
            } else {
                let mut second_forward_idx: i32 = -1;
                let mut second_forward_hint = 0u32;
                for i in 0..REFS_PER_FRAME {
                    let ref_hint = state.refs[h.ref_frame_idx[i] as usize].order_hint;
                    if get_relative_dist(seq, ref_hint, forward_hint) < 0
                        && (second_forward_idx < 0
                            || get_relative_dist(seq, ref_hint, second_forward_hint) > 0)
                    {
                        second_forward_idx = i as i32;
                        second_forward_hint = ref_hint;
                    }
                }
                if second_forward_idx >= 0 {
                    skip_mode_allowed = true;
                    h.skip_mode_frame = [
                        1 + forward_idx.min(second_forward_idx) as u8,
                        1 + forward_idx.max(second_forward_idx) as u8,
                    ];
                }
            }
        }
    }
    h.skip_mode_present = if skip_mode_allowed {
        r.read_bit()?
    } else {
        false
    };
    Ok(())
}

/// `global_motion_params()` (spec 5.9.24).
fn read_global_motion_params(
    r: &mut BitReader<'_>,
    h: &mut FrameHeader,
    prev: &[WarpParams; REFS_PER_FRAME],
) -> Result<()> {
    h.global_motion = [WarpParams::default(); REFS_PER_FRAME];
    if h.frame_is_intra {
        return Ok(());
    }
    for (i, prev) in prev.iter().enumerate().take(REFS_PER_FRAME) {
        let model = if r.read_bit()? {
            if r.read_bit()? {
                WarpModel::Rotzoom
            } else if r.read_bit()? {
                WarpModel::Translation
            } else {
                WarpModel::Affine
            }
        } else {
            WarpModel::Identity
        };
        let mut warp = WarpParams {
            model,
            ..WarpParams::default()
        };
        if matches!(model, WarpModel::Rotzoom | WarpModel::Affine) {
            read_global_param(r, h.allow_high_precision_mv, model, &mut warp, *prev, 2)?;
            read_global_param(r, h.allow_high_precision_mv, model, &mut warp, *prev, 3)?;
            if model == WarpModel::Affine {
                read_global_param(r, h.allow_high_precision_mv, model, &mut warp, *prev, 4)?;
                read_global_param(r, h.allow_high_precision_mv, model, &mut warp, *prev, 5)?;
            } else {
                warp.params[4] = -warp.params[3];
                warp.params[5] = warp.params[2];
            }
        }
        if model != WarpModel::Identity {
            read_global_param(r, h.allow_high_precision_mv, model, &mut warp, *prev, 0)?;
            read_global_param(r, h.allow_high_precision_mv, model, &mut warp, *prev, 1)?;
        }
        warp.invalid = !crate::warp::warp_valid(&warp.params);
        h.global_motion[i] = warp;
    }
    Ok(())
}

/// `read_global_param()` (spec 5.9.25).
fn read_global_param(
    r: &mut BitReader<'_>,
    allow_high_precision_mv: bool,
    model: WarpModel,
    warp: &mut WarpParams,
    prev: WarpParams,
    idx: usize,
) -> Result<()> {
    const GM_ABS_TRANS_BITS: u32 = 12;
    const GM_ABS_TRANS_ONLY_BITS: u32 = 9;
    const GM_ABS_ALPHA_BITS: u32 = 12;
    const GM_ALPHA_PREC_BITS: u32 = 15;
    const GM_TRANS_PREC_BITS: u32 = 6;
    const GM_TRANS_ONLY_PREC_BITS: u32 = 3;

    let mut abs_bits = GM_ABS_ALPHA_BITS;
    let mut prec_bits = GM_ALPHA_PREC_BITS;
    if idx < 2 {
        if model == WarpModel::Translation {
            let high = u32::from(!allow_high_precision_mv);
            abs_bits = GM_ABS_TRANS_ONLY_BITS - high;
            prec_bits = GM_TRANS_ONLY_PREC_BITS - high;
        } else {
            abs_bits = GM_ABS_TRANS_BITS;
            prec_bits = GM_TRANS_PREC_BITS;
        }
    }
    let prec_diff = WARPEDMODEL_PREC_BITS - prec_bits;
    let round = if idx % 3 == 2 {
        1i32 << WARPEDMODEL_PREC_BITS
    } else {
        0
    };
    let sub = if idx % 3 == 2 { 1i32 << prec_bits } else { 0 };
    let mx = 1i32 << abs_bits;
    let reference = (prev.params[idx] >> prec_diff) - sub;
    let value = decode_signed_subexp_with_ref(r, -mx, mx + 1, reference)?;
    warp.params[idx] = (value << prec_diff).wrapping_add(round);
    Ok(())
}

/// `decode_signed_subexp_with_ref()` (spec 5.9.26).
fn decode_signed_subexp_with_ref(
    r: &mut BitReader<'_>,
    low: i32,
    high: i32,
    reference: i32,
) -> Result<i32> {
    let x = decode_unsigned_subexp_with_ref(r, (high - low) as u32, reference - low)?;
    Ok(x as i32 + low)
}

/// `decode_unsigned_subexp_with_ref()` (spec 5.9.27).
fn decode_unsigned_subexp_with_ref(r: &mut BitReader<'_>, mx: u32, reference: i32) -> Result<u32> {
    let v = decode_subexp(r, mx)?;
    let reference = reference.clamp(0, mx as i32) as u32;
    if (reference << 1) <= mx {
        Ok(inverse_recenter(reference, v))
    } else {
        Ok(mx - 1 - inverse_recenter(mx - 1 - reference, v))
    }
}

/// `decode_subexp()` (spec 5.9.28).
fn decode_subexp(r: &mut BitReader<'_>, num_syms: u32) -> Result<u32> {
    let mut i = 0u32;
    let mut mk = 0u32;
    let k = 3u32;
    loop {
        let b2 = if i > 0 { k + i - 1 } else { k };
        if b2 >= 32 {
            return Err(Error::corrupt("AV1 subexp code longer than 32 bits"));
        }
        let a = 1u32 << b2;
        if num_syms <= mk + 3 * a {
            return Ok(read_ns(r, num_syms - mk)? + mk);
        }
        if r.read_bit()? {
            i += 1;
            mk += a;
        } else {
            return Ok(r.read_bits(b2)? + mk);
        }
    }
}

/// `inverse_recenter()` (spec 5.9.29).
fn inverse_recenter(r: u32, v: u32) -> u32 {
    if v > 2 * r {
        v
    } else if v & 1 != 0 {
        r - ((v + 1) >> 1)
    } else {
        r + (v >> 1)
    }
}

/// `film_grain_params()` (spec 5.9.30).
fn read_film_grain_params(
    r: &mut BitReader<'_>,
    seq: &SequenceHeader,
    state: &FrameState,
    h: &mut FrameHeader,
) -> Result<()> {
    if !seq.film_grain_params_present || (!h.show_frame && !h.showable_frame) {
        h.film_grain = FilmGrainParams::default();
        return Ok(());
    }
    let mut fg = FilmGrainParams {
        apply_grain: r.read_bit()?,
        ..FilmGrainParams::default()
    };
    if !fg.apply_grain {
        h.film_grain = FilmGrainParams::default();
        return Ok(());
    }
    fg.grain_seed = r.read_bits(16)? as u16;
    fg.update_grain = if h.frame_type == FrameType::Inter {
        r.read_bit()?
    } else {
        true
    };
    if !fg.update_grain {
        fg.film_grain_params_ref_idx = r.read_bits(3)? as u8;
        let seed = fg.grain_seed;
        let idx = fg.film_grain_params_ref_idx;
        // load_grain_params: everything but the seed comes from the reference.
        fg = state.refs[idx as usize].film_grain;
        fg.grain_seed = seed;
        fg.update_grain = false;
        fg.film_grain_params_ref_idx = idx;
        h.film_grain = fg;
        return Ok(());
    }

    let color = &seq.color_config;
    fg.num_y_points = r.read_bits(4)? as u8;
    if fg.num_y_points as usize > fg.point_y_value.len() {
        return Err(Error::corrupt(format!(
            "AV1 film grain: num_y_points {} exceeds 14",
            fg.num_y_points
        )));
    }
    for i in 0..fg.num_y_points as usize {
        fg.point_y_value[i] = r.read_bits(8)? as u8;
        fg.point_y_scaling[i] = r.read_bits(8)? as u8;
    }
    fg.chroma_scaling_from_luma = if color.mono_chrome {
        false
    } else {
        r.read_bit()?
    };
    if color.mono_chrome
        || fg.chroma_scaling_from_luma
        || (color.subsampling_x == 1 && color.subsampling_y == 1 && fg.num_y_points == 0)
    {
        fg.num_cb_points = 0;
        fg.num_cr_points = 0;
    } else {
        fg.num_cb_points = r.read_bits(4)? as u8;
        if fg.num_cb_points as usize > fg.point_cb_value.len() {
            return Err(Error::corrupt("AV1 film grain: num_cb_points exceeds 10"));
        }
        for i in 0..fg.num_cb_points as usize {
            fg.point_cb_value[i] = r.read_bits(8)? as u8;
            fg.point_cb_scaling[i] = r.read_bits(8)? as u8;
        }
        fg.num_cr_points = r.read_bits(4)? as u8;
        if fg.num_cr_points as usize > fg.point_cr_value.len() {
            return Err(Error::corrupt("AV1 film grain: num_cr_points exceeds 10"));
        }
        for i in 0..fg.num_cr_points as usize {
            fg.point_cr_value[i] = r.read_bits(8)? as u8;
            fg.point_cr_scaling[i] = r.read_bits(8)? as u8;
        }
    }
    fg.grain_scaling_minus_8 = r.read_bits(2)? as u8;
    fg.ar_coeff_lag = r.read_bits(2)? as u8;
    let num_pos_luma = 2 * fg.ar_coeff_lag as usize * (fg.ar_coeff_lag as usize + 1);
    let num_pos_chroma = if fg.num_y_points > 0 {
        for i in 0..num_pos_luma {
            fg.ar_coeffs_y[i] = (r.read_bits(8)? as i32 - 128) as i8;
        }
        num_pos_luma + 1
    } else {
        num_pos_luma
    };
    if fg.chroma_scaling_from_luma || fg.num_cb_points > 0 {
        for i in 0..num_pos_chroma {
            fg.ar_coeffs_cb[i] = (r.read_bits(8)? as i32 - 128) as i8;
        }
    }
    if fg.chroma_scaling_from_luma || fg.num_cr_points > 0 {
        for i in 0..num_pos_chroma {
            fg.ar_coeffs_cr[i] = (r.read_bits(8)? as i32 - 128) as i8;
        }
    }
    fg.ar_coeff_shift_minus_6 = r.read_bits(2)? as u8;
    fg.grain_scale_shift = r.read_bits(2)? as u8;
    if fg.num_cb_points > 0 {
        fg.cb_mult = r.read_bits(8)? as u8;
        fg.cb_luma_mult = r.read_bits(8)? as u8;
        fg.cb_offset = r.read_bits(9)? as u16;
    }
    if fg.num_cr_points > 0 {
        fg.cr_mult = r.read_bits(8)? as u8;
        fg.cr_luma_mult = r.read_bits(8)? as u8;
        fg.cr_offset = r.read_bits(9)? as u16;
    }
    fg.overlap_flag = r.read_bit()?;
    fg.clip_to_restricted_range = r.read_bit()?;
    h.film_grain = fg;
    Ok(())
}

/// One tile of a tile group (spec 5.11.1), located in the bitstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tile {
    /// `TileNum` within the frame, row major.
    pub tile_num: u32,
    /// Tile row, `VASliceParameterBufferAV1::tile_row`.
    pub row: u32,
    /// Tile column, `VASliceParameterBufferAV1::tile_column`.
    pub column: u32,
    /// Byte offset of the tile data, from the start of the buffer handed to the
    /// parser. `VASliceParameterBufferAV1::slice_data_offset`.
    pub offset: usize,
    /// Tile data length in bytes. `VASliceParameterBufferAV1::slice_data_size`.
    pub size: usize,
}

/// `tile_group_obu()` (spec 5.11.1): locate every tile in the payload.
///
/// `data` is the OBU payload; `base_offset` is where that payload starts in
/// whatever buffer the caller will hand to the hardware, so the returned
/// offsets are usable as they stand.
pub(crate) fn parse_tile_group(
    data: &[u8],
    base_offset: usize,
    tile_info: &TileInfo,
) -> Result<Vec<Tile>> {
    let num_tiles = tile_info.cols * tile_info.rows;
    let mut r = BitReader::new(data);
    let tile_start_and_end_present_flag = if num_tiles > 1 { r.read_bit()? } else { false };
    let (tg_start, tg_end) = if num_tiles == 1 || !tile_start_and_end_present_flag {
        (0, num_tiles.saturating_sub(1))
    } else {
        let tile_bits = tile_info.cols_log2 + tile_info.rows_log2;
        let start = r.read_bits(tile_bits)?;
        let end = r.read_bits(tile_bits)?;
        if end < start || end >= num_tiles {
            return Err(Error::corrupt(format!(
                "AV1 tile group {start}..{end} outside the frame's {num_tiles} tiles"
            )));
        }
        (start, end)
    };
    r.align_to_byte();
    let mut pos = (r.bit_position() / 8) as usize;

    let mut tiles = Vec::with_capacity((tg_end - tg_start + 1) as usize);
    for tile_num in tg_start..=tg_end {
        let last_tile = tile_num == tg_end;
        let size = if last_tile {
            data.len().checked_sub(pos).ok_or(Error::NeedMore)?
        } else {
            let bytes = tile_info.tile_size_bytes as usize;
            let field = data.get(pos..pos + bytes).ok_or(Error::NeedMore)?;
            let mut size = 0usize;
            for (i, &b) in field.iter().enumerate() {
                size |= (b as usize) << (8 * i);
            }
            pos += bytes;
            size + 1
        };
        if pos + size > data.len() {
            return Err(Error::corrupt(format!(
                "AV1 tile {tile_num} claims {size} bytes but only {} remain",
                data.len() - pos
            )));
        }
        tiles.push(Tile {
            tile_num,
            row: tile_num / tile_info.cols.max(1),
            column: tile_num % tile_info.cols.max(1),
            offset: base_offset + pos,
            size,
        });
        pos += size;
    }
    Ok(tiles)
}

/// Reference update (spec 7.20): store this frame into every slot its
/// `refresh_frame_flags` names.
pub(crate) fn apply_refresh(state: &mut FrameState, h: &FrameHeader, bit_depth: u8) {
    let slot = ReferenceSlot {
        valid: true,
        frame_type: h.frame_type,
        frame_id: h.current_frame_id,
        upscaled_width: h.upscaled_width,
        frame_width: h.frame_width,
        frame_height: h.frame_height,
        render_width: h.render_width,
        render_height: h.render_height,
        mi_cols: h.mi_cols,
        mi_rows: h.mi_rows,
        order_hint: h.order_hint,
        saved_order_hints: h.order_hints,
        gm_params: h.global_motion,
        loop_filter: h.loop_filter,
        segmentation: h.segmentation,
        film_grain: h.film_grain,
        bit_depth,
    };
    for i in 0..NUM_REF_FRAMES {
        if h.refresh_frame_flags & (1 << i) != 0 {
            state.refs[i] = slot;
            state.ref_valid[i] = true;
            state.ref_order_hint[i] = h.order_hint;
        }
    }
}

/// Reference frame loading (spec 7.21): a `show_existing_frame` keyframe
/// republishes the slot it shows into every slot.
pub(crate) fn apply_show_existing_refresh(state: &mut FrameState, h: &FrameHeader) {
    if h.refresh_frame_flags == 0 {
        return;
    }
    let slot = state.refs[h.frame_to_show_map_idx as usize];
    for i in 0..NUM_REF_FRAMES {
        if h.refresh_frame_flags & (1 << i) != 0 {
            state.refs[i] = slot;
            state.ref_valid[i] = slot.valid;
            state.ref_order_hint[i] = slot.order_hint;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdef_strengths_pack_the_way_va_wants() {
        let cdef = CdefParams {
            bits: 1,
            y_pri_strength: [9, 2, 0, 0, 0, 0, 0, 0],
            // 4 is the decoded form of the coded value 3.
            y_sec_strength: [4, 1, 0, 0, 0, 0, 0, 0],
            ..CdefParams::default()
        };
        let packed = cdef.y_strengths();
        assert_eq!(packed[0], (9 << 2) | 3);
        assert_eq!(packed[1], (2 << 2) | 1);
    }

    #[test]
    fn feature_mask_is_one_bit_per_feature() {
        let mut seg = SegmentationParams::default();
        seg.feature_enabled[3][0] = true;
        seg.feature_enabled[3][5] = true;
        assert_eq!(seg.feature_mask(3), 0b0010_0001);
        assert_eq!(seg.feature_mask(0), 0);
    }

    #[test]
    fn inverse_recenter_is_its_own_shape() {
        // v > 2r passes through; even and odd v alternate above and below r.
        assert_eq!(inverse_recenter(3, 9), 9);
        assert_eq!(inverse_recenter(3, 2), 4);
        assert_eq!(inverse_recenter(3, 1), 2);
        assert_eq!(inverse_recenter(0, 0), 0);
    }

    #[test]
    fn identity_warp_is_the_unit_matrix() {
        let w = WarpParams::default();
        assert_eq!(w.model, WarpModel::Identity);
        assert_eq!(w.params[2], 1 << 16);
        assert_eq!(w.params[5], 1 << 16);
        assert_eq!(w.params[0], 0);
    }

    #[test]
    fn tile_sizes_come_back_out_of_the_starts() {
        // Two 64-superblock columns of a 1280-wide frame: mi starts 0, 16, 320.
        let info = TileInfo {
            cols: 2,
            mi_col_starts: vec![0, 16, 320],
            ..TileInfo::default()
        };
        assert_eq!(info.width_in_sbs_minus_1(4), vec![0, 18]);
    }

    #[test]
    fn superres_shrinks_the_coded_width() {
        let mut h = FrameHeader {
            frame_width: 1920,
            frame_height: 1080,
            ..FrameHeader::default()
        };
        // denom 16 halves the coded width; UpscaledWidth keeps the display size.
        h.superres_denom = 16;
        h.upscaled_width = h.frame_width;
        h.frame_width = (h.upscaled_width * 8 + 8) / 16;
        assert_eq!((h.frame_width, h.upscaled_width), (960, 1920));
        compute_image_size(&mut h);
        assert_eq!((h.mi_cols, h.mi_rows), (240, 270));
    }
}

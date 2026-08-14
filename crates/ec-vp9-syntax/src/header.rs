//! The uncompressed frame header (spec 6.2 `uncompressed_header`).

use ec_core::{BitReader, Error, Result};

use crate::{
    MAX_SEGMENTS, NUM_REF_FRAMES, REFS_PER_FRAME, SEG_LVL_ALT_L, SEG_LVL_ALT_Q, SEG_LVL_MAX,
};

/// The three-byte frame sync code that opens a keyframe or intra-only frame.
const SYNC_CODE: [u8; 3] = [0x49, 0x83, 0x42];
/// Bits carried by each segment feature (spec 6.2, `segmentation_feature_bits`).
const SEG_FEATURE_BITS: [u32; SEG_LVL_MAX] = [8, 6, 2, 0];
/// Which segment features carry a sign bit (spec 6.2, `segmentation_feature_signed`).
const SEG_FEATURE_SIGNED: [bool; SEG_LVL_MAX] = [true, true, false, false];
/// Loop filter deltas after `setup_past_independence` (spec 7.2, "intra, last, golden, altref").
const DEFAULT_REF_DELTAS: [i8; 4] = [1, 0, -1, -1];
/// The largest loop filter level, and the clamp applied to every derived level.
const MAX_LOOP_FILTER: i32 = 63;

/// Frame type (spec 6.2, `frame_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameType {
    /// `KEY_FRAME`: refreshes every reference slot and resets all persistent state.
    Key,
    /// `NON_KEY_FRAME`: inter, or intra-only when `intra_only` is set.
    Inter,
}

/// Colour space code point (spec 6.2, `color_space`).
///
/// These are VP9's own three-bit codes, not H.273; convert with
/// [`ColorSpace::h273`] when handing them to the family colour model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorSpace {
    /// `CS_UNKNOWN`, 0.
    Unknown,
    /// `CS_BT_601`, 1.
    Bt601,
    /// `CS_BT_709`, 2.
    Bt709,
    /// `CS_SMPTE_170`, 3.
    Smpte170,
    /// `CS_SMPTE_240`, 4.
    Smpte240,
    /// `CS_BT_2020`, 5.
    Bt2020,
    /// `CS_RESERVED`, 6.
    Reserved,
    /// `CS_RGB`, 7 — 4:4:4 with full range, profiles 1 and 3 only.
    Rgb,
}

impl ColorSpace {
    fn from_code(code: u32) -> ColorSpace {
        match code {
            1 => ColorSpace::Bt601,
            2 => ColorSpace::Bt709,
            3 => ColorSpace::Smpte170,
            4 => ColorSpace::Smpte240,
            5 => ColorSpace::Bt2020,
            6 => ColorSpace::Reserved,
            7 => ColorSpace::Rgb,
            _ => ColorSpace::Unknown,
        }
    }

    /// The H.273 `(colour_primaries, transfer_characteristics, matrix_coefficients)`
    /// triplet VP9 means by this code point, as tabulated in the WebM container
    /// guidelines; `None` for `CS_UNKNOWN` and `CS_RESERVED`, which say nothing.
    pub fn h273(self) -> Option<(u8, u8, u8)> {
        match self {
            ColorSpace::Bt601 => Some((5, 6, 6)),
            ColorSpace::Bt709 => Some((1, 1, 1)),
            ColorSpace::Smpte170 => Some((6, 6, 6)),
            ColorSpace::Smpte240 => Some((7, 7, 7)),
            ColorSpace::Bt2020 => Some((9, 14, 9)),
            ColorSpace::Rgb => Some((1, 13, 0)),
            ColorSpace::Unknown | ColorSpace::Reserved => None,
        }
    }
}

/// Sub-pixel interpolation filter (spec 6.2, `read_interpolation_filter`).
///
/// The discriminants are the VP9 `interp_filter` values, which is also what
/// `VADecPictureParameterBufferVP9::mcomp_filter_type` wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum InterpolationFilter {
    /// `EIGHTTAP_SMOOTH`, 0.
    EighttapSmooth = 0,
    /// `EIGHTTAP`, 1.
    Eighttap = 1,
    /// `EIGHTTAP_SHARP`, 2.
    EighttapSharp = 2,
    /// `BILINEAR`, 3.
    Bilinear = 3,
    /// `SWITCHABLE`, 4 — chosen per block in the compressed header.
    Switchable = 4,
}

/// Loop filter parameters (spec 6.2, `loop_filter_params`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopFilterParams {
    /// `loop_filter_level`, 0-63; 0 disables the filter for the frame.
    pub level: u8,
    /// `loop_filter_sharpness`, 0-7.
    pub sharpness: u8,
    /// `loop_filter_delta_enabled`: per-reference and per-mode adjustments apply.
    pub delta_enabled: bool,
    /// `loop_filter_delta_update`: this frame coded new delta values.
    pub delta_update: bool,
    /// `loop_filter_ref_deltas`, indexed intra, last, golden, altref. Persists
    /// across frames until reset by `setup_past_independence`.
    pub ref_deltas: [i8; 4],
    /// `loop_filter_mode_deltas`, indexed by inter mode class. Persists likewise.
    pub mode_deltas: [i8; 2],
}

impl Default for LoopFilterParams {
    fn default() -> LoopFilterParams {
        LoopFilterParams {
            level: 0,
            sharpness: 0,
            delta_enabled: false,
            delta_update: false,
            ref_deltas: DEFAULT_REF_DELTAS,
            mode_deltas: [0; 2],
        }
    }
}

/// Quantization parameters (spec 6.2, `quantization_params`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QuantizationParams {
    /// `base_q_idx`, the frame-level quantizer index, 0-255.
    pub base_q_idx: u8,
    /// `delta_q_y_dc`, luma DC offset from `base_q_idx`.
    pub delta_q_y_dc: i8,
    /// `delta_q_uv_dc`, chroma DC offset from `base_q_idx`.
    pub delta_q_uv_dc: i8,
    /// `delta_q_uv_ac`, chroma AC offset from `base_q_idx`.
    pub delta_q_uv_ac: i8,
}

impl QuantizationParams {
    /// `LosslessFlag` (spec 6.2): quantizer index 0 with no plane offsets.
    ///
    /// This is `VADecPictureParameterBufferVP9::lossless_flag` verbatim.
    pub fn lossless(&self) -> bool {
        self.base_q_idx == 0
            && self.delta_q_y_dc == 0
            && self.delta_q_uv_dc == 0
            && self.delta_q_uv_ac == 0
    }
}

/// Segmentation parameters (spec 6.2, `segmentation_params`).
///
/// Everything but `enabled`, `update_map` and `update_data` persists from frame
/// to frame: a frame may enable segmentation and code no data, meaning "keep
/// using the previous frame's". [`Vp9Parser`] carries that state, so the copy in
/// a parsed header is always the state in force *for that frame*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SegmentationParams {
    /// `segmentation_enabled`.
    pub enabled: bool,
    /// `segmentation_update_map`: the tree probabilities below are new.
    pub update_map: bool,
    /// `segmentation_temporal_update`: the map is coded predictively, using
    /// [`SegmentationParams::pred_probs`].
    pub temporal_update: bool,
    /// `segmentation_update_data`: the feature table below is new.
    pub update_data: bool,
    /// `segmentation_abs_or_delta_update`: feature data is absolute, not a delta
    /// against the frame-level value.
    pub abs_or_delta_update: bool,
    /// `segmentation_tree_probs`, the 7 probabilities of the segment id tree;
    /// 255 where not coded. `VADecPictureParameterBufferVP9::mb_segment_tree_probs`.
    pub tree_probs: [u8; 7],
    /// `segmentation_pred_prob`, the 3 temporal prediction probabilities; 255
    /// where not coded. `VADecPictureParameterBufferVP9::segment_pred_probs`.
    pub pred_probs: [u8; 3],
    /// `FeatureEnabled[segment][feature]`, features indexed by the `SEG_LVL_*`
    /// constants.
    pub feature_enabled: [[bool; SEG_LVL_MAX]; MAX_SEGMENTS],
    /// `FeatureData[segment][feature]`, sign already applied.
    pub feature_data: [[i16; SEG_LVL_MAX]; MAX_SEGMENTS],
}

/// Tile layout (spec 6.2, `tile_info`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TileInfo {
    /// `tile_cols_log2`: the frame has `1 << tile_cols_log2` tile columns.
    pub cols_log2: u8,
    /// `tile_rows_log2`: the frame has `1 << tile_rows_log2` tile rows.
    pub rows_log2: u8,
}

/// One decoded uncompressed frame header.
///
/// Field names follow the spec, so a reader can hold the two side by side. Every
/// field of `VADecPictureParameterBufferVP9` is either here verbatim or derived
/// by a method on this type; the two exceptions are the surface ids, which are a
/// property of the caller's frame pool rather than the bitstream, and the
/// per-segment dequantization scales, which need the spec 8.6.1 lookup tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameHeader {
    /// `profile`, 0-3. Profiles 2 and 3 carry more than 8 bits per sample;
    /// profiles 1 and 3 code their chroma subsampling.
    pub profile: u8,
    /// `show_existing_frame`: this frame is a bodiless instruction to output an
    /// existing reference. No other field below was coded; they hold defaults.
    pub show_existing_frame: bool,
    /// `frame_to_show_map_idx`, the reference slot to output. Only meaningful
    /// when [`FrameHeader::show_existing_frame`] is set.
    pub frame_to_show_map_idx: u8,
    /// `frame_type`.
    pub frame_type: FrameType,
    /// `show_frame`: this frame is output as well as decoded. Hidden frames
    /// (ALTREFs) clear it and are shown later by a `show_existing_frame` header.
    pub show_frame: bool,
    /// `error_resilient_mode`: probability state is reset, so a decoder may
    /// start here without the previous frame.
    pub error_resilient_mode: bool,
    /// `intra_only`: a non-key frame coded without inter prediction.
    pub intra_only: bool,
    /// `FrameIsIntra` (spec 6.2): a keyframe or an intra-only frame.
    pub frame_is_intra: bool,
    /// `BitDepth`: 8, 10 or 12.
    pub bit_depth: u8,
    /// `color_space`.
    pub color_space: ColorSpace,
    /// `color_range`: true for full range (0-255), false for studio swing.
    pub color_range: bool,
    /// `subsampling_x`: 1 for 4:2:0 and 4:2:2, 0 for 4:4:4.
    pub subsampling_x: u8,
    /// `subsampling_y`: 1 for 4:2:0, 0 for 4:2:2 and 4:4:4.
    pub subsampling_y: u8,
    /// `FrameWidth`, the coded width in samples.
    pub width: u32,
    /// `FrameHeight`, the coded height in samples.
    pub height: u32,
    /// `RenderWidth`, the width the frame should be displayed at.
    pub render_width: u32,
    /// `RenderHeight`, the height the frame should be displayed at.
    pub render_height: u32,
    /// `refresh_frame_flags`: bit *i* means reference slot *i* is replaced by
    /// this frame. 0xff on a keyframe.
    pub refresh_frame_flags: u8,
    /// `ref_frame_idx[i]`, the reference slot used as LAST, GOLDEN and ALTREF.
    pub ref_frame_idx: [u8; REFS_PER_FRAME],
    /// `ref_frame_sign_bias[LAST_FRAME + i]`, in the same order.
    pub ref_frame_sign_bias: [bool; REFS_PER_FRAME],
    /// `allow_high_precision_mv`: motion vectors have 1/8-pel precision.
    pub allow_high_precision_mv: bool,
    /// `interpolation_filter`.
    pub interpolation_filter: InterpolationFilter,
    /// `reset_frame_context`, 0-3.
    pub reset_frame_context: u8,
    /// `refresh_frame_context`: the compressed header's probability updates are
    /// saved back into the frame context.
    pub refresh_frame_context: bool,
    /// `frame_parallel_decoding_mode`: no backward probability adaptation.
    pub frame_parallel_decoding_mode: bool,
    /// `frame_context_idx`, 0-3, after the reset rules in spec 6.2 are applied.
    pub frame_context_idx: u8,
    /// Loop filter parameters in force for this frame.
    pub loop_filter: LoopFilterParams,
    /// Quantization parameters for this frame.
    pub quantization: QuantizationParams,
    /// Segmentation parameters in force for this frame.
    pub segmentation: SegmentationParams,
    /// Tile layout for this frame.
    pub tile_info: TileInfo,
    /// `header_size_in_bytes`, the length of the compressed header that follows.
    /// This is `VASliceParameterBufferVP9`'s partition 0 size, better known as
    /// `first_partition_size`.
    pub header_size_in_bytes: u16,
    /// Length of the uncompressed header itself, in bytes, counting from the
    /// first byte of the frame. `VADecPictureParameterBufferVP9::frame_header_length_in_bytes`.
    pub uncompressed_header_size: u8,
}

impl FrameHeader {
    /// `get_qindex` (spec 8.6.1): the quantizer index in force for `segment_id`,
    /// after the segment's `SEG_LVL_ALT_Q` feature is applied.
    ///
    /// Feed the result, plus [`QuantizationParams::delta_q_y_dc`] and friends,
    /// into the spec 8.6.1 `dc_q`/`ac_q` tables to fill
    /// `VASegmentParameterVP9::{luma,chroma}_{dc,ac}_quant_scale`.
    pub fn segment_qindex(&self, segment_id: usize) -> u8 {
        let base = self.quantization.base_q_idx as i32;
        let seg = &self.segmentation;
        if !seg.enabled
            || segment_id >= MAX_SEGMENTS
            || !seg.feature_enabled[segment_id][SEG_LVL_ALT_Q]
        {
            return self.quantization.base_q_idx;
        }
        let data = seg.feature_data[segment_id][SEG_LVL_ALT_Q] as i32;
        let q = if seg.abs_or_delta_update {
            data
        } else {
            base + data
        };
        q.clamp(0, 255) as u8
    }

    /// The four dequantization scales for `segment_id` (spec 8.6.1), which are
    /// `VASegmentParameterVP9`'s `luma_dc_quant_scale` and its three siblings.
    pub fn segment_dequant(&self, segment_id: usize) -> SegmentDequant {
        let q = self.segment_qindex(segment_id) as i32;
        let depth = self.bit_depth;
        SegmentDequant {
            luma_dc: crate::quant::dc_q(depth, q + self.quantization.delta_q_y_dc as i32),
            luma_ac: crate::quant::ac_q(depth, q),
            chroma_dc: crate::quant::dc_q(depth, q + self.quantization.delta_q_uv_dc as i32),
            chroma_ac: crate::quant::ac_q(depth, q + self.quantization.delta_q_uv_ac as i32),
        }
    }

    /// The loop filter level table for `segment_id` (spec 8.8.1, "loop filter
    /// frame init process"), indexed `[reference][mode]` exactly like
    /// `VASegmentParameterVP9::filter_level`.
    ///
    /// Reference 0 is intra, which has no mode delta; the spec writes only
    /// `[0][0]` and this follows it, leaving `[0][1]` equal to it.
    pub fn loop_filter_levels(&self, segment_id: usize) -> [[u8; 2]; 4] {
        let lf = &self.loop_filter;
        let seg = &self.segmentation;
        let mut lvl_seg = lf.level as i32;
        if seg.enabled
            && segment_id < MAX_SEGMENTS
            && seg.feature_enabled[segment_id][SEG_LVL_ALT_L]
        {
            let data = seg.feature_data[segment_id][SEG_LVL_ALT_L] as i32;
            let v = if seg.abs_or_delta_update {
                data
            } else {
                lvl_seg + data
            };
            lvl_seg = v.clamp(0, MAX_LOOP_FILTER);
        }

        if !lf.delta_enabled {
            return [[lvl_seg as u8; 2]; 4];
        }
        let shift = lvl_seg >> 5;
        let mut out = [[0u8; 2]; 4];
        let intra =
            (lvl_seg + ((lf.ref_deltas[0] as i32) << shift)).clamp(0, MAX_LOOP_FILTER) as u8;
        out[0] = [intra, intra];
        for (reference, levels) in out.iter_mut().enumerate().skip(1) {
            for (mode, slot) in levels.iter_mut().enumerate() {
                let level = lvl_seg
                    + ((lf.ref_deltas[reference] as i32) << shift)
                    + ((lf.mode_deltas[mode] as i32) << shift);
                *slot = level.clamp(0, MAX_LOOP_FILTER) as u8;
            }
        }
        out
    }

    /// `MiCols` (spec 6.2, `compute_image_size`): the frame width in 8x8 blocks.
    pub fn mi_cols(&self) -> u32 {
        self.width.div_ceil(8)
    }

    /// `MiRows`: the frame height in 8x8 blocks.
    pub fn mi_rows(&self) -> u32 {
        self.height.div_ceil(8)
    }
}

impl Default for FrameHeader {
    fn default() -> FrameHeader {
        FrameHeader {
            profile: 0,
            show_existing_frame: false,
            frame_to_show_map_idx: 0,
            frame_type: FrameType::Key,
            show_frame: true,
            error_resilient_mode: false,
            intra_only: false,
            frame_is_intra: true,
            bit_depth: 8,
            color_space: ColorSpace::Unknown,
            color_range: false,
            subsampling_x: 1,
            subsampling_y: 1,
            width: 0,
            height: 0,
            render_width: 0,
            render_height: 0,
            refresh_frame_flags: 0,
            ref_frame_idx: [0; REFS_PER_FRAME],
            ref_frame_sign_bias: [false; REFS_PER_FRAME],
            allow_high_precision_mv: false,
            interpolation_filter: InterpolationFilter::Eighttap,
            reset_frame_context: 0,
            refresh_frame_context: false,
            frame_parallel_decoding_mode: false,
            frame_context_idx: 0,
            loop_filter: LoopFilterParams::default(),
            quantization: QuantizationParams::default(),
            segmentation: SegmentationParams::default(),
            tile_info: TileInfo::default(),
            header_size_in_bytes: 0,
            uncompressed_header_size: 0,
        }
    }
}

/// The per-segment dequantization scales of spec 8.6.1, in the shape
/// `VASegmentParameterVP9` asks for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentDequant {
    /// `luma_dc_quant_scale`: `dc_q(qindex + delta_q_y_dc)`.
    pub luma_dc: i16,
    /// `luma_ac_quant_scale`: `ac_q(qindex)`.
    pub luma_ac: i16,
    /// `chroma_dc_quant_scale`: `dc_q(qindex + delta_q_uv_dc)`.
    pub chroma_dc: i16,
    /// `chroma_ac_quant_scale`: `ac_q(qindex + delta_q_uv_ac)`.
    pub chroma_ac: i16,
}

/// Sample format, as last coded by a keyframe or intra-only frame.
///
/// Inter frames do not code `color_config`; they inherit it. A picture parameter
/// buffer needs `bit_depth` and the subsampling for *every* frame, so the parser
/// carries the last one seen and stamps it onto the frames that omit it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ColorConfig {
    bit_depth: u8,
    color_space: ColorSpace,
    color_range: bool,
    subsampling_x: u8,
    subsampling_y: u8,
}

impl ColorConfig {
    /// Stamp the sample format onto a header being parsed.
    fn apply(&self, h: &mut FrameHeader) {
        h.bit_depth = self.bit_depth;
        h.color_space = self.color_space;
        h.color_range = self.color_range;
        h.subsampling_x = self.subsampling_x;
        h.subsampling_y = self.subsampling_y;
    }
}

impl Default for ColorConfig {
    fn default() -> ColorConfig {
        ColorConfig {
            bit_depth: 8,
            color_space: ColorSpace::Unknown,
            color_range: false,
            subsampling_x: 1,
            subsampling_y: 1,
        }
    }
}

/// What the parser remembers about a reference slot: enough to answer
/// `frame_size_with_refs`, and enough for a caller to size its surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReferenceSlot {
    /// Whether any frame has been stored in this slot yet.
    pub valid: bool,
    /// `RefFrameWidth`.
    pub width: u32,
    /// `RefFrameHeight`.
    pub height: u32,
    /// `RefBitDepth`.
    pub bit_depth: u8,
    /// `RefSubsamplingX`.
    pub subsampling_x: u8,
    /// `RefSubsamplingY`.
    pub subsampling_y: u8,
}

/// A VP9 header parser holding the state the format carries between frames.
///
/// Feed it every frame of a stream in order — [`crate::superframe::split`] first,
/// since a chunk may hold several — and it applies the reference refresh after
/// each one.
#[derive(Debug, Clone, Default)]
pub struct Vp9Parser {
    refs: [ReferenceSlot; NUM_REF_FRAMES],
    segmentation: SegmentationParams,
    loop_filter: LoopFilterParams,
    color: ColorConfig,
}

impl Vp9Parser {
    /// A parser with no reference state, as at the start of a stream.
    pub fn new() -> Vp9Parser {
        Vp9Parser {
            refs: [ReferenceSlot::default(); NUM_REF_FRAMES],
            segmentation: SegmentationParams::default(),
            loop_filter: LoopFilterParams::default(),
            color: ColorConfig::default(),
        }
    }

    /// The reference slots as they stand, after every frame parsed so far.
    pub fn reference_slots(&self) -> &[ReferenceSlot; NUM_REF_FRAMES] {
        &self.refs
    }

    /// Parse one frame's uncompressed header and apply its reference refresh.
    ///
    /// `frame` starts at the frame marker, i.e. it is one element of
    /// [`crate::superframe::split`]'s output.
    pub fn parse_frame(&mut self, frame: &[u8]) -> Result<FrameHeader> {
        let mut r = BitReader::new(frame);
        let mut h = FrameHeader::default();

        let marker = r.read_bits(2)?;
        if marker != 2 {
            return Err(Error::corrupt(format!(
                "VP9 frame marker is {marker}, expected 2"
            )));
        }
        let profile_low = r.read_bits(1)?;
        let profile_high = r.read_bits(1)?;
        h.profile = ((profile_high << 1) | profile_low) as u8;
        if h.profile == 3 && r.read_bit()? {
            return Err(Error::corrupt("VP9 profile 3 reserved_zero bit is set"));
        }

        h.show_existing_frame = r.read_bit()?;
        if h.show_existing_frame {
            h.frame_to_show_map_idx = r.read_bits(3)? as u8;
            let slot = &self.refs[h.frame_to_show_map_idx as usize];
            h.width = slot.width;
            h.height = slot.height;
            h.render_width = slot.width;
            h.render_height = slot.height;
            self.color.apply(&mut h);
            h.show_frame = true;
            h.uncompressed_header_size = header_bytes(&r);
            return Ok(h);
        }

        h.frame_type = if r.read_bit()? {
            FrameType::Inter
        } else {
            FrameType::Key
        };
        h.show_frame = r.read_bit()?;
        h.error_resilient_mode = r.read_bit()?;

        if h.frame_type == FrameType::Key {
            read_sync_code(&mut r)?;
            read_color_config(&mut r, h.profile, &mut self.color)?;
            self.color.apply(&mut h);
            read_frame_size(&mut r, &mut h)?;
            read_render_size(&mut r, &mut h)?;
            h.refresh_frame_flags = 0xff;
            h.frame_is_intra = true;
        } else {
            h.intra_only = if h.show_frame { false } else { r.read_bit()? };
            h.frame_is_intra = h.intra_only;
            if !h.error_resilient_mode {
                h.reset_frame_context = r.read_bits(2)? as u8;
            }
            if h.intra_only {
                read_sync_code(&mut r)?;
                if h.profile > 0 {
                    read_color_config(&mut r, h.profile, &mut self.color)?;
                } else {
                    // Profile 0 intra-only is 8-bit 4:2:0 BT.601 by definition.
                    self.color = ColorConfig {
                        bit_depth: 8,
                        color_space: ColorSpace::Bt601,
                        color_range: false,
                        subsampling_x: 1,
                        subsampling_y: 1,
                    };
                }
                self.color.apply(&mut h);
                h.refresh_frame_flags = r.read_bits(8)? as u8;
                read_frame_size(&mut r, &mut h)?;
                read_render_size(&mut r, &mut h)?;
            } else {
                // An inter frame codes no color_config; it inherits the one the
                // last keyframe or intra-only frame set.
                self.color.apply(&mut h);
                h.refresh_frame_flags = r.read_bits(8)? as u8;
                for i in 0..REFS_PER_FRAME {
                    h.ref_frame_idx[i] = r.read_bits(3)? as u8;
                    h.ref_frame_sign_bias[i] = r.read_bit()?;
                }
                self.read_frame_size_with_refs(&mut r, &mut h)?;
                h.allow_high_precision_mv = r.read_bit()?;
                h.interpolation_filter = read_interpolation_filter(&mut r)?;
            }
        }

        if !h.error_resilient_mode {
            h.refresh_frame_context = r.read_bit()?;
            h.frame_parallel_decoding_mode = r.read_bit()?;
        } else {
            h.refresh_frame_context = false;
            h.frame_parallel_decoding_mode = true;
        }
        h.frame_context_idx = r.read_bits(2)? as u8;

        if h.frame_is_intra || h.error_resilient_mode {
            // setup_past_independence (spec 7.2): segmentation data and loop
            // filter deltas go back to their defaults before this header codes
            // over them, and the frame context index is forced to 0.
            self.segmentation = SegmentationParams::default();
            self.loop_filter = LoopFilterParams::default();
            h.frame_context_idx = 0;
        }

        self.read_loop_filter_params(&mut r, &mut h)?;
        read_quantization_params(&mut r, &mut h)?;
        self.read_segmentation_params(&mut r, &mut h)?;
        read_tile_info(&mut r, &mut h)?;
        h.header_size_in_bytes = r.read_bits(16)? as u16;
        if h.header_size_in_bytes == 0 {
            return Err(Error::corrupt("VP9 header_size_in_bytes is 0"));
        }
        h.uncompressed_header_size = header_bytes(&r);

        self.apply_refresh(&h);
        Ok(h)
    }

    /// Reference update (spec 8.10): store this frame into every slot its
    /// `refresh_frame_flags` names.
    fn apply_refresh(&mut self, h: &FrameHeader) {
        let slot = ReferenceSlot {
            valid: true,
            width: h.width,
            height: h.height,
            bit_depth: h.bit_depth,
            subsampling_x: h.subsampling_x,
            subsampling_y: h.subsampling_y,
        };
        for i in 0..NUM_REF_FRAMES {
            if h.refresh_frame_flags & (1 << i) != 0 {
                self.refs[i] = slot;
            }
        }
    }

    /// `frame_size_with_refs` (spec 6.2): take the size from the first reference
    /// that claims it, else code it outright.
    fn read_frame_size_with_refs(&self, r: &mut BitReader<'_>, h: &mut FrameHeader) -> Result<()> {
        let mut found = false;
        for i in 0..REFS_PER_FRAME {
            if r.read_bit()? {
                let slot = &self.refs[h.ref_frame_idx[i] as usize];
                if !slot.valid {
                    return Err(Error::corrupt(format!(
                        "VP9 frame_size_with_refs names empty reference slot {}",
                        h.ref_frame_idx[i]
                    )));
                }
                h.width = slot.width;
                h.height = slot.height;
                found = true;
                break;
            }
        }
        if !found {
            read_frame_size(r, h)?;
        }
        read_render_size(r, h)
    }

    fn read_loop_filter_params(
        &mut self,
        r: &mut BitReader<'_>,
        h: &mut FrameHeader,
    ) -> Result<()> {
        let lf = &mut self.loop_filter;
        lf.level = r.read_bits(6)? as u8;
        lf.sharpness = r.read_bits(3)? as u8;
        lf.delta_enabled = r.read_bit()?;
        lf.delta_update = false;
        if lf.delta_enabled {
            lf.delta_update = r.read_bit()?;
            if lf.delta_update {
                for delta in lf.ref_deltas.iter_mut() {
                    if r.read_bit()? {
                        *delta = read_signed_magnitude(r, 6)? as i8;
                    }
                }
                for delta in lf.mode_deltas.iter_mut() {
                    if r.read_bit()? {
                        *delta = read_signed_magnitude(r, 6)? as i8;
                    }
                }
            }
        }
        h.loop_filter = *lf;
        Ok(())
    }

    fn read_segmentation_params(
        &mut self,
        r: &mut BitReader<'_>,
        h: &mut FrameHeader,
    ) -> Result<()> {
        let seg = &mut self.segmentation;
        seg.enabled = r.read_bit()?;
        seg.update_map = false;
        seg.temporal_update = false;
        seg.update_data = false;
        if seg.enabled {
            seg.update_map = r.read_bit()?;
            if seg.update_map {
                for prob in seg.tree_probs.iter_mut() {
                    *prob = if r.read_bit()? {
                        r.read_bits(8)? as u8
                    } else {
                        255
                    };
                }
                seg.temporal_update = r.read_bit()?;
                for prob in seg.pred_probs.iter_mut() {
                    *prob = if seg.temporal_update && r.read_bit()? {
                        r.read_bits(8)? as u8
                    } else {
                        255
                    };
                }
            }
            seg.update_data = r.read_bit()?;
            if seg.update_data {
                seg.abs_or_delta_update = r.read_bit()?;
                for segment in 0..MAX_SEGMENTS {
                    for feature in 0..SEG_LVL_MAX {
                        let enabled = r.read_bit()?;
                        seg.feature_enabled[segment][feature] = enabled;
                        let mut value = 0i32;
                        if enabled {
                            let bits = SEG_FEATURE_BITS[feature];
                            if bits > 0 {
                                value = r.read_bits(bits)? as i32;
                            }
                            if SEG_FEATURE_SIGNED[feature] && r.read_bit()? {
                                value = -value;
                            }
                        }
                        seg.feature_data[segment][feature] = value as i16;
                    }
                }
            }
        }
        h.segmentation = *seg;
        Ok(())
    }
}

/// Bytes consumed so far, rounded up — the uncompressed header is byte-aligned
/// at its end by construction, but rounding keeps a corrupt stream honest.
fn header_bytes(r: &BitReader<'_>) -> u8 {
    u8::try_from(r.bit_position().div_ceil(8)).unwrap_or(u8::MAX)
}

fn read_sync_code(r: &mut BitReader<'_>) -> Result<()> {
    let code = [
        r.read_bits(8)? as u8,
        r.read_bits(8)? as u8,
        r.read_bits(8)? as u8,
    ];
    if code != SYNC_CODE {
        return Err(Error::corrupt(format!(
            "VP9 frame sync code is {code:02x?}, expected {SYNC_CODE:02x?}"
        )));
    }
    Ok(())
}

/// `color_config` (spec 6.2).
fn read_color_config(r: &mut BitReader<'_>, profile: u8, c: &mut ColorConfig) -> Result<()> {
    c.bit_depth = if profile >= 2 {
        if r.read_bit()? { 12 } else { 10 }
    } else {
        8
    };
    c.color_space = ColorSpace::from_code(r.read_bits(3)?);
    if c.color_space != ColorSpace::Rgb {
        c.color_range = r.read_bit()?;
        if profile == 1 || profile == 3 {
            c.subsampling_x = r.read_bits(1)? as u8;
            c.subsampling_y = r.read_bits(1)? as u8;
            if r.read_bit()? {
                return Err(Error::corrupt("VP9 color_config reserved_zero bit is set"));
            }
        } else {
            c.subsampling_x = 1;
            c.subsampling_y = 1;
        }
    } else {
        // CS_RGB is 4:4:4 full range, and only profiles 1 and 3 may code it.
        c.color_range = true;
        if profile == 1 || profile == 3 {
            c.subsampling_x = 0;
            c.subsampling_y = 0;
            if r.read_bit()? {
                return Err(Error::corrupt("VP9 color_config reserved_zero bit is set"));
            }
        } else {
            return Err(Error::corrupt(format!(
                "VP9 CS_RGB is not allowed in profile {profile}"
            )));
        }
    }
    Ok(())
}

/// `frame_size` (spec 6.2).
fn read_frame_size(r: &mut BitReader<'_>, h: &mut FrameHeader) -> Result<()> {
    h.width = r.read_bits(16)? + 1;
    h.height = r.read_bits(16)? + 1;
    Ok(())
}

/// `render_size` (spec 6.2).
fn read_render_size(r: &mut BitReader<'_>, h: &mut FrameHeader) -> Result<()> {
    if r.read_bit()? {
        h.render_width = r.read_bits(16)? + 1;
        h.render_height = r.read_bits(16)? + 1;
    } else {
        h.render_width = h.width;
        h.render_height = h.height;
    }
    Ok(())
}

/// `read_interpolation_filter` (spec 6.2), including the literal-to-type map.
fn read_interpolation_filter(r: &mut BitReader<'_>) -> Result<InterpolationFilter> {
    if r.read_bit()? {
        return Ok(InterpolationFilter::Switchable);
    }
    Ok(match r.read_bits(2)? {
        0 => InterpolationFilter::EighttapSmooth,
        1 => InterpolationFilter::Eighttap,
        2 => InterpolationFilter::EighttapSharp,
        _ => InterpolationFilter::Bilinear,
    })
}

/// `quantization_params` (spec 6.2).
fn read_quantization_params(r: &mut BitReader<'_>, h: &mut FrameHeader) -> Result<()> {
    h.quantization.base_q_idx = r.read_bits(8)? as u8;
    h.quantization.delta_q_y_dc = read_delta_q(r)?;
    h.quantization.delta_q_uv_dc = read_delta_q(r)?;
    h.quantization.delta_q_uv_ac = read_delta_q(r)?;
    Ok(())
}

/// `read_delta_q` (spec 6.2): a flag, then a 4-bit magnitude and a sign.
fn read_delta_q(r: &mut BitReader<'_>) -> Result<i8> {
    if r.read_bit()? {
        Ok(read_signed_magnitude(r, 4)? as i8)
    } else {
        Ok(0)
    }
}

/// `s(n)` (spec 4.10): an `n`-bit magnitude followed by a sign bit.
fn read_signed_magnitude(r: &mut BitReader<'_>, n: u32) -> Result<i32> {
    let value = r.read_bits(n)? as i32;
    Ok(if r.read_bit()? { -value } else { value })
}

/// `tile_info` (spec 6.2).
fn read_tile_info(r: &mut BitReader<'_>, h: &mut FrameHeader) -> Result<()> {
    let sb64_cols = h.mi_cols().div_ceil(8);
    let (min_log2, max_log2) = tile_cols_log2_bounds(sb64_cols);
    let mut cols_log2 = min_log2;
    while cols_log2 < max_log2 {
        if r.read_bit()? {
            cols_log2 += 1;
        } else {
            break;
        }
    }
    let mut rows_log2 = u8::from(r.read_bit()?);
    if rows_log2 > 0 && r.read_bit()? {
        rows_log2 += 1;
    }
    h.tile_info = TileInfo {
        cols_log2,
        rows_log2,
    };
    Ok(())
}

/// `calc_min_log2_tile_cols` / `calc_max_log2_tile_cols` (spec 6.2).
///
/// A tile column is at most 64 superblocks wide and at least 4, which is what
/// pins the range the increment bits then walk.
fn tile_cols_log2_bounds(sb64_cols: u32) -> (u8, u8) {
    const MAX_TILE_WIDTH_B64: u32 = 64;
    const MIN_TILE_WIDTH_B64: u32 = 4;
    let mut min_log2 = 0u8;
    while (MAX_TILE_WIDTH_B64 << min_log2) < sb64_cols {
        min_log2 += 1;
    }
    let mut max_log2 = 1u8;
    while (sb64_cols >> max_log2) >= MIN_TILE_WIDTH_B64 {
        max_log2 += 1;
    }
    (min_log2, max_log2 - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec 6.2 worked bounds: 1920 wide is 30 superblocks, so tile columns may
    /// go up to 2^2 (a column must stay at least 4 superblocks wide) and none
    /// are forced (a column may be up to 64 superblocks wide).
    #[test]
    fn tile_bounds_match_the_spec_formulas() {
        assert_eq!(
            tile_cols_log2_bounds(1920u32.div_ceil(8).div_ceil(8)),
            (0, 2)
        );
        assert_eq!(
            tile_cols_log2_bounds(3840u32.div_ceil(8).div_ceil(8)),
            (0, 3)
        );
        // 4096 superblocks wide forces at least 2^6 tile columns.
        assert_eq!(tile_cols_log2_bounds(4096).0, 6);
        assert_eq!(tile_cols_log2_bounds(1), (0, 0));
    }

    #[test]
    fn lossless_needs_every_delta_zero() {
        let mut q = QuantizationParams::default();
        assert!(q.lossless());
        q.delta_q_uv_ac = -1;
        assert!(!q.lossless());
        q.delta_q_uv_ac = 0;
        q.base_q_idx = 1;
        assert!(!q.lossless());
    }

    #[test]
    fn filter_levels_apply_deltas_and_clamp() {
        let mut h = FrameHeader {
            loop_filter: LoopFilterParams {
                level: 32,
                delta_enabled: true,
                ref_deltas: [1, 0, -1, -1],
                mode_deltas: [0, 2],
                ..LoopFilterParams::default()
            },
            ..FrameHeader::default()
        };
        // lvlSeg 32 -> shift 1: intra 32 + (1<<1) = 34, altref 32 - 2 = 30,
        // altref with mode delta 2: 30 + 4 = 34.
        let levels = h.loop_filter_levels(0);
        assert_eq!(levels[0], [34, 34]);
        assert_eq!(levels[3], [30, 34]);

        // A segment ALT_L override replaces the frame level before the deltas —
        // which still apply, so a clamped-to-zero segment is not filter-free.
        h.segmentation.enabled = true;
        h.segmentation.feature_enabled[1][SEG_LVL_ALT_L] = true;
        h.segmentation.feature_data[1][SEG_LVL_ALT_L] = -64;
        assert_eq!(h.loop_filter_levels(1), [[1, 1], [0, 2], [0, 1], [0, 1]]);

        // Deltas off means one level everywhere.
        h.loop_filter.delta_enabled = false;
        assert_eq!(h.loop_filter_levels(0), [[32; 2]; 4]);
    }

    #[test]
    fn segment_qindex_is_absolute_or_relative() {
        let mut h = FrameHeader {
            quantization: QuantizationParams {
                base_q_idx: 100,
                ..QuantizationParams::default()
            },
            ..FrameHeader::default()
        };
        h.segmentation.enabled = true;
        h.segmentation.feature_enabled[2][SEG_LVL_ALT_Q] = true;
        h.segmentation.feature_data[2][SEG_LVL_ALT_Q] = -20;
        assert_eq!(h.segment_qindex(2), 80);
        assert_eq!(h.segment_qindex(0), 100);
        h.segmentation.abs_or_delta_update = true;
        assert_eq!(h.segment_qindex(2), 0); // clamped up from -20
        h.segmentation.feature_data[2][SEG_LVL_ALT_Q] = 200;
        assert_eq!(h.segment_qindex(2), 200);
    }

    #[test]
    fn truncated_header_is_need_more_not_panic() {
        let mut p = Vp9Parser::new();
        for len in 0..12 {
            let frame = vec![0x82u8; len];
            assert!(p.parse_frame(&frame).is_err());
        }
    }

    #[test]
    fn bad_marker_and_sync_code_are_corrupt() {
        let mut p = Vp9Parser::new();
        assert!(matches!(
            p.parse_frame(&[0x00, 0, 0, 0, 0, 0, 0, 0]),
            Err(Error::Corrupt { .. })
        ));
        // marker 2, profile 0, no show_existing, key frame, then a wrong sync code.
        assert!(matches!(
            p.parse_frame(&[0x80, 0x00, 0x00, 0x00, 0, 0, 0, 0]),
            Err(Error::Corrupt { .. })
        ));
    }
}

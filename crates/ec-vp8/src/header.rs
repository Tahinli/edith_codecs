//! Uncompressed chunk and first-partition header parsing
//! (RFC 6386 §9-§10, following the reference decoder's field order).
//!
//! Three pieces of state persist across frames until the next key frame
//! resets them: the entropy context (coefficient, MV and intra-mode
//! probabilities), the segmentation feature data and the per-macroblock
//! loop-filter deltas. They live in [`PersistedState`]; every frame's
//! [`FrameHeader::parse`] folds that frame's updates into it.

use crate::bool::BoolDecoder;
use crate::frame::{FrameTag, FrameType, KEYFRAME_HEADER_SZ, parse_keyframe_dims};
use crate::tables::{DEFAULT_COEFF_PROBS, DEFAULT_MV_PROBS, MV_UPDATE_PROBS};
use ec_core::{Error, Result};

/// Probability-array length per MV component (dixie `MV_PROB_CNT`):
/// is_short, sign, 7 short-tree, 10 long-bits probabilities.
pub const MV_PROB_CNT: usize = 19;

/// Everything that survives from frame to frame until a key frame.
#[derive(Debug, Clone)]
pub struct PersistedState {
    /// Token-tree probabilities, `[block_type][band][ctx][node]`
    /// (RFC 6386 §13.3).
    /// Token-tree probabilities `[block_type][band][ctx][node]` (§13.3).
    pub coeff_probs: [[[[u8; 11]; 3]; 8]; 4],
    /// Per-MV-component probabilities `[component][prob]` (RFC §17.2).
    /// Per-MV-component probabilities `[component][prob]` (§17.2).
    pub mv_probs: [[u8; MV_PROB_CNT]; 2],
    /// Interframe Y-mode tree probabilities (RFC §16.1).
    /// Interframe Y-mode tree probabilities (§16.1).
    pub ymode_probs: [u8; 4],
    /// Interframe chroma-mode tree probabilities (RFC §16.1).
    /// Interframe chroma-mode tree probabilities (§16.1).
    pub uv_mode_probs: [u8; 3],
    /// Segment feature state (RFC §10), zeroed at each key frame.
    /// Segment feature state (§10), zeroed at each key frame.
    pub segmentation: SegmentationState,
    /// Per-MB loop-filter deltas (RFC §9.4), zeroed at each key frame.
    /// Reference-frame-based loop-filter deltas (§9.4; intra, last,
    /// golden, altref order).
    pub ref_lf_delta: [i32; 4],
    /// Mode-based loop-filter deltas (§9.4; B_PRED, ZEROMV, MV,
    /// SPLITMV order).
    pub mode_lf_delta: [i32; 4],
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            coeff_probs: DEFAULT_COEFF_PROBS,
            mv_probs: DEFAULT_MV_PROBS,
            ymode_probs: [145, 156, 163, 128],
            uv_mode_probs: [142, 114, 183],
            segmentation: SegmentationState::default(),
            ref_lf_delta: [0; 4],
            mode_lf_delta: [0; 4],
        }
    }
}

/// Segment feature state (RFC 6386 §10). `quant_idx`/`lf_level` hold the
/// raw transmitted values; they are absolute in absolute mode and deltas
/// in delta mode.
#[derive(Debug, Clone, Default)]
pub struct SegmentationState {
    /// Segment features active for the current frame.
    pub enabled: bool,
    /// The per-MB segment map is re-coded this frame.
    pub update_map: bool,
    /// Segment feature values are re-coded this frame.
    pub update_data: bool,
    /// 0 = absolute values, 1 = deltas against the frame baseline.
    /// 0 = absolute values, 1 = deltas against the frame baseline.
    pub abs_delta: bool,
    /// Per-segment quantizer value or delta (§10).
    pub quant_idx: [i32; 4],
    /// Per-segment loop-filter level or delta (§10).
    pub lf_level: [i32; 4],
    /// Segment-map decoding tree probabilities (default 255).
    /// Segment-map tree probabilities (default 255; §9.3 item 5).
    pub tree_probs: [u8; 3],
}

/// Frame-level fields parsed from the uncompressed chunk and the first
/// data partition. Everything the per-macroblock decode needs.
#[derive(Debug, Clone)]
pub struct FrameHeader {
    /// Parsed 3-byte frame tag.
    pub tag: FrameTag,
    /// Key-frame dimensions (key frames only).
    pub dims: Option<crate::frame::KeyFrameDims>,
    /// Key-frame-only colour-space bit (0 = YUV; 1 is reserved).
    pub color_space: u8,
    /// Key-frame-only clamping bit (1 = reconstructed pixels guaranteed
    /// 0..255, clamping optional).
    pub clamping_type: u8,
    /// Segment feature snapshot this frame decodes under.
    pub segmentation: SegmentationState,
    /// Loop filter type/level/sharpness (§9.4).
    /// 0 = normal loop filter, 1 = simple (§9.4).
    pub filter_type: u8,
    /// Baseline loop filter strength 0-63 (0 disables filtering).
    pub filter_level: u8,
    /// Loop filter sharpness 0-7 (§9.4, §15.2).
    pub sharpness_level: u8,
    /// Per-MB loop-filter delta adjustment enabled for this frame.
    pub lf_delta_enabled: bool,
    /// This frame updated the (persisted) delta values.
    pub lf_delta_update: bool,
    /// log2 of the token partition count (1, 2, 4 or 8 partitions).
    pub log2_partitions: u8,
    /// Quantization indices (§9.6): base Y-AC index plus per-plane deltas.
    pub quant: QuantIndices,
    /// Probability an interframe MB is intra-coded (§9.10 `prob_intra`).
    pub prob_intra: u8,
    /// Probability an inter MB uses the last-frame reference.
    pub prob_last: u8,
    /// Probability an inter MB uses the golden rather than altref ref.
    pub prob_gf: u8,
    /// Whether this frame updated the interframe Y-mode probabilities.
    pub ymode_probs_updated: bool,
    /// Whether this frame updated the chroma-mode probabilities.
    pub uv_mode_probs_updated: bool,
    /// Reference-frame refresh/copies/sign-biases (§9.7-§9.8).
    pub refresh: RefreshHeader,
    /// mb_no_skip_coeff (§9.10/§9.11): per-MB "no non-zero coefficients"
    /// flag is coded at all.
    pub coeff_skip_enabled: bool,
    /// Probability for the per-MB skip flag (meaningful when enabled).
    pub prob_skip_false: u8,
    /// Offset of the first token partition from the start of `data`
    /// (i.e. uncompressed chunk + first partition).
    pub token_data_offset: usize,
    /// Token partition sizes in bytes; last entry is the remainder.
    pub partition_sizes: Vec<u32>,
}

/// Six dequantization indices (§9.6): the always-coded Y-AC index plus
/// signed deltas for the other five factors.
#[derive(Debug, Clone, Copy, Default)]
pub struct QuantIndices {
    /// Base quantizer index for Y AC (7 bits).
    pub yac_qi: u8,
    /// Signed delta applied to the Y-DC dequant index (§9.6).
    pub ydc_delta: i32,
    /// Signed delta applied to the Y2-DC dequant index (§9.6).
    pub y2dc_delta: i32,
    /// Signed delta applied to the Y2-AC dequant index (§9.6).
    pub y2ac_delta: i32,
    /// Signed delta applied to the chroma-DC dequant index (§9.6).
    pub uvdc_delta: i32,
    /// Signed delta applied to the chroma-AC dequant index (§9.6).
    pub uvac_delta: i32,
}

/// Reference-frame refresh state for one frame (§9.7-§9.8). Key frames
/// refresh every reference and read only `refresh_entropy`.
#[derive(Debug, Clone, Copy)]
pub struct RefreshHeader {
    /// The golden buffer is refreshed with the current frame.
    pub refresh_gf: bool,
    /// The altref buffer is refreshed with the current frame.
    pub refresh_arf: bool,
    /// 0 = none, 1 = copy last, 2 = copy altref (to golden).
    pub copy_gf: u8,
    /// 0 = none, 1 = copy last, 2 = copy golden (to altref).
    pub copy_arf: u8,
    /// Sign bias applied to MVs referencing the golden frame (§9.7).
    pub sign_bias_golden: bool,
    /// Sign bias applied to MVs referencing the altref frame (§9.7).
    pub sign_bias_altref: bool,
    /// Whether this frame's entropy updates persist to the next frame.
    pub refresh_entropy: bool,
    /// The last-frame buffer is refreshed with the current frame.
    pub refresh_last: bool,
}

impl FrameHeader {
    /// The frame's type (key or inter), from the frame tag.
    pub fn frame_type(&self) -> FrameType {
        self.tag.frame_type
    }

    /// Parse one frame's uncompressed chunk + first partition from
    /// `data`, folding entropy/segmentation/filter-delta updates into
    /// `state`. Returns the header and the partition-1 slice.
    ///
    /// `token_data_offset` + every `partition_sizes[i]` together describe
    /// the bytes after partition 1.
    pub fn parse<'a>(
        data: &'a [u8],
        state: &mut PersistedState,
    ) -> Result<(FrameHeader, &'a [u8])> {
        let tag = FrameTag::parse(data)?;
        let key = tag.frame_type == FrameType::Key;
        let mut off = crate::frame::FRAME_TAG_SZ;

        let mut dims = None;
        if key {
            let kf = parse_keyframe_dims(data)?;
            dims = Some(kf);
            off += KEYFRAME_HEADER_SZ;
            // A key frame resets every piece of persisted entropy and
            // segment/filter state (dixie `decode_frame`).
            *state = PersistedState::default();
        }

        let part0_end = off + tag.first_part_size as usize;
        if data.len() < part0_end {
            return Err(Error::corrupt(format!(
                "VP8 first partition extends past end of frame \
                 (need {part0_end} bytes, have {})",
                data.len()
            )));
        }
        let mut d = BoolDecoder::new(&data[off..part0_end])?;

        // Colour space + clamping (key frames only): two literal bits;
        // the colour-space bit is reserved and must be 0 (dixie reads
        // both together and refuses any non-zero pair; we keep the
        // (legal) clamping bit decodable and refuse only the reserved
        // colour-space one).
        let (color_space, clamping_type) = if key {
            let cs = d.read_literal(1) as u8;
            let clamp = d.read_literal(1) as u8;
            if cs != 0 {
                return Err(Error::unsupported(
                    "VP8 reserved colour-space bit",
                    "bit 0 of the key-frame colour-space pair was set",
                ));
            }
            (cs, clamp)
        } else {
            (0, 0)
        };

        // --- segmentation (§9.3, dixie decode_segmentation_header) ---
        let mut segmentation = if key {
            SegmentationState::default()
        } else {
            state.segmentation.clone()
        };
        segmentation.enabled = d.read_bool(128);
        if segmentation.enabled {
            segmentation.update_map = d.read_bool(128);
            segmentation.update_data = d.read_bool(128);
            if segmentation.update_data {
                segmentation.abs_delta = d.read_bool(128);
                for i in 0..4 {
                    segmentation.quant_idx[i] = d.read_maybe_signed(7);
                }
                for i in 0..4 {
                    segmentation.lf_level[i] = d.read_maybe_signed(6);
                }
            }
            if segmentation.update_map {
                for i in 0..3 {
                    segmentation.tree_probs[i] = if d.read_bool(128) {
                        d.read_literal(8) as u8
                    } else {
                        255
                    };
                }
            }
        } else {
            segmentation.update_map = false;
            segmentation.update_data = false;
        }
        state.segmentation = segmentation.clone();

        // --- loop filter (§9.4, dixie decode_loopfilter_header) ---
        let mut ref_lf_delta = state.ref_lf_delta;
        let mut mode_lf_delta = state.mode_lf_delta;
        if key {
            ref_lf_delta = [0; 4];
            mode_lf_delta = [0; 4];
        }
        let filter_type = d.read_literal(1) as u8;
        let filter_level = d.read_literal(6) as u8;
        let sharpness_level = d.read_literal(3) as u8;
        let lf_delta_enabled = d.read_bool(128);
        let mut lf_delta_update = false;
        if lf_delta_enabled && d.read_bool(128) {
            lf_delta_update = true;
            for i in 0..4 {
                ref_lf_delta[i] = d.read_maybe_signed(6);
            }
            for i in 0..4 {
                mode_lf_delta[i] = d.read_maybe_signed(6);
            }
        }
        state.ref_lf_delta = ref_lf_delta;
        state.mode_lf_delta = mode_lf_delta;

        // --- token partitions (§9.5) ---
        let log2_partitions = d.read_literal(2) as u8;
        let partitions = 1usize << log2_partitions;
        let sizes_area = 3 * (partitions - 1);
        if data.len() < part0_end + sizes_area {
            return Err(Error::corrupt(
                "VP8 token partition size table extends past end of frame",
            ));
        }
        let mut partition_sizes = Vec::with_capacity(partitions);
        let mut size_off = part0_end;
        let mut remaining = (data.len() - part0_end - sizes_area) as u32;
        for i in 0..partitions {
            if i < partitions - 1 {
                let sz = u32::from(data[size_off])
                    | (u32::from(data[size_off + 1]) << 8)
                    | (u32::from(data[size_off + 2]) << 16);
                size_off += 3;
                partition_sizes.push(sz);
                remaining = remaining
                    .checked_sub(sz)
                    .ok_or_else(|| Error::corrupt("VP8 token partitions exceed frame size"))?;
            } else {
                partition_sizes.push(remaining);
            }
        }

        // --- quantization indices (§9.6) ---
        let mut quant = QuantIndices {
            yac_qi: d.read_literal(7) as u8,
            ..QuantIndices::default()
        };
        quant.ydc_delta = d.read_maybe_signed(4);
        quant.y2dc_delta = d.read_maybe_signed(4);
        quant.y2ac_delta = d.read_maybe_signed(4);
        quant.uvdc_delta = d.read_maybe_signed(4);
        quant.uvac_delta = d.read_maybe_signed(4);

        // --- reference refresh (§9.7-§9.8) ---
        let refresh = if key {
            RefreshHeader {
                refresh_gf: true,
                refresh_arf: true,
                copy_gf: 0,
                copy_arf: 0,
                sign_bias_golden: false,
                sign_bias_altref: false,
                refresh_entropy: d.read_bool(128),
                refresh_last: true,
            }
        } else {
            let refresh_gf = d.read_bool(128);
            let refresh_arf = d.read_bool(128);
            let copy_gf = if refresh_gf {
                0
            } else {
                d.read_literal(2) as u8
            };
            let copy_arf = if refresh_arf {
                0
            } else {
                d.read_literal(2) as u8
            };
            RefreshHeader {
                refresh_gf,
                refresh_arf,
                copy_gf,
                copy_arf,
                sign_bias_golden: d.read_bool(128),
                sign_bias_altref: d.read_bool(128),
                refresh_entropy: d.read_bool(128),
                refresh_last: d.read_bool(128),
            }
        };

        // --- entropy updates (§9.9; dixie decode_entropy_header) ---
        // Coefficient probability updates are read for every frame; on
        // key frames the tables were reset to defaults above.
        for i in 0..4 {
            for j in 0..8 {
                for k in 0..3 {
                    for l in 0..11 {
                        if d.read_bool(crate::tables::COEFF_UPDATE_PROBS[i][j][k][l]) {
                            state.coeff_probs[i][j][k][l] = d.read_literal(8) as u8;
                        }
                    }
                }
            }
        }

        // mb_no_skip_coeff (§9.11 / inter §9.10).
        let coeff_skip_enabled = d.read_bool(128);
        let prob_skip_false = if coeff_skip_enabled {
            d.read_literal(8) as u8
        } else {
            128
        };
        // Interframe-only probability updates.
        let mut ymode_probs_updated = false;
        let mut uv_mode_probs_updated = false;
        let (prob_intra, prob_last, prob_gf) = if !key {
            let probs = (
                d.read_literal(8) as u8,
                d.read_literal(8) as u8,
                d.read_literal(8) as u8,
            );
            ymode_probs_updated = d.read_bool(128);
            if ymode_probs_updated {
                for i in 0..4 {
                    state.ymode_probs[i] = d.read_literal(8) as u8;
                }
            }
            uv_mode_probs_updated = d.read_bool(128);
            if uv_mode_probs_updated {
                for i in 0..3 {
                    state.uv_mode_probs[i] = d.read_literal(8) as u8;
                }
            }
            probs
        } else {
            (145, 156, 163)
        };
        for c in 0..2 {
            for j in 0..MV_PROB_CNT {
                if d.read_bool(MV_UPDATE_PROBS[c][j]) {
                    state.mv_probs[c][j] = d.read_prob7();
                }
            }
        }

        if d.overreads() > 0 {
            return Err(Error::corrupt(format!(
                "VP8 first partition over-read by {} bytes (header desync)",
                d.overreads()
            )));
        }

        let header = FrameHeader {
            tag,
            dims,
            color_space,
            clamping_type,
            segmentation,
            filter_type,
            filter_level,
            sharpness_level,
            lf_delta_enabled,
            lf_delta_update,
            log2_partitions,
            quant,
            refresh,
            prob_intra,
            prob_last,
            prob_gf,
            ymode_probs_updated,
            uv_mode_probs_updated,
            coeff_skip_enabled,
            prob_skip_false,
            token_data_offset: part0_end,
            partition_sizes,
        };
        Ok((header, &data[off..part0_end]))
    }
}

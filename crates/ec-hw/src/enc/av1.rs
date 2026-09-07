//! AV1 encode parameters.
//!
//! One tile, one reference, no segmentation, no superres, no film grain: the
//! subset this driver advertises (`VAConfigAttribEncAV1` on radeonsi reports
//! no filter intra, no warped motion, no dual filter, no superres and no
//! restoration) and the same subset ffmpeg's `av1_vaapi` encoder writes.
//!
//! The driver does not compose the headers: it parses the sequence header and
//! frame header OBUs this module packs (see [`super::headers::av1`]) and
//! writes the stream's own headers from them together with the picture
//! parameter buffer below. Both therefore describe one picture, or the coded
//! frame is not the frame that was submitted.

use std::sync::Arc;

use ec_va::Buffer;

use super::headers::av1 as av1_headers;
use super::{Encoder, RateControlMode};
use crate::error::Result;
use crate::params::enc::{
    EncPictureParameterBufferAV1, EncSequenceParameterBufferAV1, EncTileGroupBufferAV1,
};
use crate::params::param_buffer;
use crate::pool::PooledSurface;
use crate::params::INVALID_SURFACE;

/// `KEY_FRAME` and `INTER_FRAME` (spec 6.8.2).
const KEY_FRAME: u32 = 0;
const INTER_FRAME: u32 = 1;
/// `PRIMARY_REF_NONE`.
const PRIMARY_REF_NONE: u8 = 7;
/// `TX_MODE_SELECT`, the only mode this driver advertises.
const TX_MODE_SELECT: u32 = 2;
/// `SWITCHABLE` interpolation filter.
const INTERP_SWITCHABLE: u8 = 4;
/// The deblocking level written into the header; the driver picks its own for
/// the stream it writes, so this is only the value it starts from.
const LOOP_FILTER_LEVEL: u8 = 1;

pub(super) fn parameters(
    encoder: &Encoder,
    recon: &Arc<PooledSurface>,
    coded_buf: u32,
    keyframe: bool,
    out: &mut Vec<Buffer>,
) -> Result<()> {
    let config = *encoder.config();
    let context = encoder.context();
    let (coded_w, coded_h) = encoder.coded_size();
    let (width, height) = (config.width.max(1), config.height.max(1));
    let cqp = matches!(config.rate_control, RateControlMode::ConstantQp { .. });
    let level_idx = seq_level_idx(coded_w, coded_h, config.framerate);

    let mut seq = EncSequenceParameterBufferAV1 {
        seq_profile: 0,
        seq_level_idx: level_idx,
        seq_tier: 0,
        intra_period: config.gop_size.max(1),
        ip_period: 1,
        bits_per_second: match config.rate_control {
            RateControlMode::ConstantBitrate => config.bitrate,
            RateControlMode::ConstantQp { .. } => 0,
        },
        order_hint_bits_minus_1: (av1_headers::ORDER_HINT_BITS - 1) as u8,
        ..EncSequenceParameterBufferAV1::default()
    };
    seq.seq_fields = seq
        .seq_fields
        .enable_order_hint(1)
        .bit_depth_minus8(0)
        .subsampling_x(1)
        .subsampling_y(1);
    out.push(param_buffer(context, &seq)?);

    // Superblocks are 64x64: use_128x128_superblock is off in the sequence
    // header, and this driver advertises no support for the larger one.
    let sb_cols = coded_w.div_ceil(64).max(1);
    let sb_rows = coded_h.div_ceil(64).max(1);
    let order_hint = if keyframe {
        0
    } else {
        (encoder.gop_position() % 256) as u8
    };
    let base_qindex = match config.rate_control {
        RateControlMode::ConstantQp { qp } => qp.clamp(1, 255) as u8,
        RateControlMode::ConstantBitrate => 128,
    };
    // One reference, refreshed every picture: slot 0 holds the previous
    // reconstruction and every ref_frame_idx points at it.
    let refresh_frame_flags = if keyframe { 0xff } else { 0x01 };

    let mut pic = EncPictureParameterBufferAV1 {
        frame_width_minus_1: (width - 1) as u16,
        frame_height_minus_1: (height - 1) as u16,
        reconstructed_frame: recon.id(),
        coded_buf,
        primary_ref_frame: if keyframe { PRIMARY_REF_NONE } else { 0 },
        order_hint,
        refresh_frame_flags,
        base_qindex,
        min_base_qindex: if cqp { 0 } else { 1 },
        max_base_qindex: if cqp { 0 } else { 255 },
        filter_level: [LOOP_FILTER_LEVEL; 2],
        filter_level_u: LOOP_FILTER_LEVEL,
        filter_level_v: LOOP_FILTER_LEVEL,
        interpolation_filter: INTERP_SWITCHABLE,
        tile_cols: 1,
        tile_rows: 1,
        superres_scale_denominator: 8,
        ..EncPictureParameterBufferAV1::default()
    };
    pic.width_in_sbs_minus_1[0] = (sb_cols - 1) as u16;
    pic.height_in_sbs_minus_1[0] = (sb_rows - 1) as u16;
    if let Some(reference) = encoder.reference().filter(|_| !keyframe) {
        pic.reference_frames[0] = reference.id();
        // ref_frame_ctrl_l0 = LAST_FRAME in search_idx0 (bits 0..2).
        pic.ref_frame_ctrl_l0 = 1;
    }
    pic.picture_flags = pic
        .picture_flags
        .frame_type(if keyframe { KEY_FRAME } else { INTER_FRAME })
        // A shown key frame is error resilient by inference (5.9.2).
        .error_resilient_mode(u32::from(keyframe))
        // The driver writes the frame header and the tile group as separate
        // OBUs, which is what the header packed below describes.
        .enable_frame_obu(0);
    pic.mode_control_flags = pic
        .mode_control_flags
        .tx_mode(TX_MODE_SELECT)
        // Single reference prediction: compound needs a second reference.
        .reference_mode(0);
    pic.tile_group_obu_hdr_info = pic.tile_group_obu_hdr_info.obu_has_size_field(1);
    for slot in pic.reference_frames.iter_mut().skip(1) {
        *slot = INVALID_SURFACE;
    }

    if keyframe {
        encoder.push_packed(
            &av1_headers::sequence_header(&av1_headers::SeqParams {
                width,
                height,
                seq_level_idx: level_idx,
                seq_tier: 0,
                high_bitdepth: false,
                colour: config.colour,
            }),
            out,
        )?;
    }
    let (packed, offsets) = av1_headers::frame_header(&av1_headers::FrameParams {
        keyframe,
        order_hint,
        primary_ref_frame: 0,
        refresh_frame_flags,
        ref_frame_idx: [0; 7],
        base_q_idx: base_qindex,
        loop_filter_level: [LOOP_FILTER_LEVEL; 4],
        tx_mode_select: true,
        max_tile_cols_log2: av1_headers::tile_log2(sb_cols.min(64)),
        max_tile_rows_log2: av1_headers::tile_log2(sb_rows.min(64)),
    });
    if !cqp {
        // Outside CQP the driver picks the quantiser and the deblocking
        // levels itself and back-annotates them into the header it writes,
        // which is what these offsets are for (ffmpeg sets them on the same
        // condition). In CQP it has nothing to write back.
        pic.bit_offset_qindex = offsets.qindex;
        pic.bit_offset_segmentation = offsets.segmentation;
        pic.bit_offset_loopfilter_params = offsets.loop_filter;
        pic.bit_offset_cdef_params = offsets.cdef;
        pic.size_in_bits_cdef_params = offsets.cdef_size;
        pic.size_in_bits_frame_hdr_obu = offsets.total_bits;
        pic.byte_offset_frame_hdr_obu_size = offsets.obu_size_byte;
    }
    out.push(param_buffer(context, &pic)?);
    encoder.push_packed(&packed, out)?;

    let tile_group = EncTileGroupBufferAV1 {
        tg_start: 0,
        tg_end: 0,
        ..EncTileGroupBufferAV1::default()
    };
    out.push(param_buffer(context, &tile_group)?);
    Ok(())
}

/// The lowest AV1 level whose picture size and rate fit (Annex A), as a
/// `seq_level_idx`. Levels below 2.0 do not exist, and anything past 6.3 is
/// signalled as the maximum-parameters level.
fn seq_level_idx(width: u32, height: u32, framerate: (u32, u32)) -> u8 {
    // (seq_level_idx, MaxPicSize, MaxHSize, MaxVSize, MaxDisplayRate)
    const LEVELS: [(u8, u64, u32, u32, u64); 12] = [
        (0, 147_456, 2_048, 1_152, 4_423_680),          // 2.0
        (1, 278_784, 2_816, 1_584, 8_363_520),          // 2.1
        (4, 665_856, 4_352, 2_448, 19_975_680),         // 3.0
        (5, 1_065_024, 5_504, 3_096, 31_950_720),       // 3.1
        (8, 2_359_296, 6_144, 3_456, 70_778_880),       // 4.0
        (9, 2_359_296, 6_144, 3_456, 141_557_760),      // 4.1
        (12, 8_912_896, 8_192, 4_352, 267_386_880),     // 5.0
        (13, 8_912_896, 8_192, 4_352, 534_773_760),     // 5.1
        (14, 8_912_896, 8_192, 4_352, 1_069_547_520),   // 5.2
        (16, 35_651_584, 16_384, 8_704, 1_069_547_520), // 6.0
        (17, 35_651_584, 16_384, 8_704, 2_139_095_040), // 6.1
        (18, 35_651_584, 16_384, 8_704, 4_278_190_080), // 6.2
    ];
    let samples = u64::from(width) * u64::from(height);
    let fps = f64::from(framerate.0.max(1)) / f64::from(framerate.1.max(1));
    let rate = (samples as f64 * fps).ceil() as u64;
    for &(idx, max_size, max_h, max_v, max_rate) in &LEVELS {
        if samples <= max_size && width <= max_h && height <= max_v && rate <= max_rate {
            return idx;
        }
    }
    31 // maximum parameters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_follow_size_and_rate() {
        // 1080p30 is 4.0 (idx 8), 2160p30 5.0 (12), 2160p60 5.1 (13).
        assert_eq!(seq_level_idx(1920, 1088, (30, 1)), 8);
        assert_eq!(seq_level_idx(3840, 2176, (30, 1)), 12);
        assert_eq!(seq_level_idx(3840, 2176, (60, 1)), 13);
    }
}

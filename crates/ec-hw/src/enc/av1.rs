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

use std::rc::Rc;

use ec_va::va::{
    AV1EncLoopFilterFlags, AV1EncLoopRestorationFlags, AV1EncModeControlFlags,
    AV1EncPictureFlags, AV1EncQMatrixFlags, AV1EncSeqFields, AV1EncTileGroupObuHdrInfo,
    BufferType, EncPictureParameter, EncPictureParameterBufferAV1, EncSegParamAV1,
    EncSegParamFlagsAV1, EncSequenceParameter, EncSequenceParameterBufferAV1, EncSliceParameter,
    EncTileGroupBufferAV1, EncWarpedMotionParamsAV1, RefFrameCtrlAV1, VABufferID,
};

use super::headers::av1 as av1_headers;
use super::{Encoder, RateControlMode};
use crate::error::Result;
use crate::params::INVALID_SURFACE;
use crate::pool::PooledSurface;

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
    recon: &Rc<PooledSurface>,
    coded_buf: VABufferID,
    keyframe: bool,
    out: &mut Vec<BufferType>,
) -> Result<()> {
    let config = *encoder.config();
    let (coded_w, coded_h) = encoder.coded_size();
    let (width, height) = (config.width.max(1), config.height.max(1));
    let cqp = config.rate_control.is_cqp();
    let ten_bit = config.bit_depth == 10;
    let level_idx = seq_level_idx(coded_w, coded_h, config.framerate);

    // enable_order_hint, the bit depth and 4:2:0 subsampling; everything else off.
    let seq_fields = AV1EncSeqFields::new(
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        true,
        false,
        false,
        false,
        false,
        false,
        if ten_bit { 2 } else { 0 },
        true,
        true,
        false,
    );
    let seq = EncSequenceParameterBufferAV1::new(
        0,
        level_idx,
        0,
        0,
        config.gop_size.max(1),
        1,
        match config.rate_control {
            RateControlMode::ConstantQp { .. } => 0,
            _ => config.bitrate,
        },
        &seq_fields,
        (av1_headers::ORDER_HINT_BITS - 1) as u8,
    );
    out.push(BufferType::EncSequenceParameter(EncSequenceParameter::AV1(seq)));

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
        _ => 128,
    };
    // One reference, refreshed every picture: slot 0 holds the previous
    // reconstruction and every ref_frame_idx points at it.
    let refresh_frame_flags: u8 = if keyframe { 0xff } else { 0x01 };

    let mut reference_frames = [INVALID_SURFACE; 8];
    let mut ref_ctrl_l0 = RefFrameCtrlAV1::default();
    if let Some(reference) = encoder.reference().filter(|_| !keyframe) {
        reference_frames[0] = reference.id();
        // ref_frame_ctrl_l0 = LAST_FRAME in search_idx0 (bits 0..2).
        ref_ctrl_l0 = RefFrameCtrlAV1::new(1, 0, 0, 0, 0, 0, 0);
    }

    // A shown key frame is error resilient by inference (5.9.2). The driver
    // writes the frame header and the tile group as separate OBUs, which is
    // what the header packed below describes.
    let picture_flags = AV1EncPictureFlags::new(
        if keyframe { KEY_FRAME } else { INTER_FRAME },
        keyframe,
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        false,
    );
    let loop_filter_flags = AV1EncLoopFilterFlags::new(0, false, false);
    // Single reference prediction: compound needs a second reference.
    let mode_control = AV1EncModeControlFlags::new(
        false,
        0,
        false,
        0,
        false,
        TX_MODE_SELECT,
        0,
        false,
    );
    let tile_group_hdr = AV1EncTileGroupObuHdrInfo::new(false, true, 0, 0);
    let segments = EncSegParamAV1::new(
        &EncSegParamFlagsAV1::new(false, false, false),
        0,
        [[0; 8]; 8],
        [0; 8],
    );
    let loop_restoration = AV1EncLoopRestorationFlags::new(0, 0, 0, 0, false);
    let wm = [EncWarpedMotionParamsAV1::default(); 7];
    let mut width_in_sbs_minus_1 = [0u16; 63];
    let mut height_in_sbs_minus_1 = [0u16; 63];
    width_in_sbs_minus_1[0] = (sb_cols - 1) as u16;
    height_in_sbs_minus_1[0] = (sb_rows - 1) as u16;

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
    // Outside CQP the driver picks the quantiser and the deblocking levels
    // itself and back-annotates them into the header it writes, which is what
    // these offsets are for (ffmpeg sets them on the same condition). In CQP
    // it has nothing to write back, so they stay zero.
    let back_annotate = !cqp;
    let (bit_offset_qindex, bit_offset_segmentation, bit_offset_loopfilter_params) = if back_annotate {
        (offsets.qindex, offsets.segmentation, offsets.loop_filter)
    } else {
        (0, 0, 0)
    };
    let (bit_offset_cdef_params, size_in_bits_cdef_params) = if back_annotate {
        (offsets.cdef, offsets.cdef_size)
    } else {
        (0, 0)
    };
    let (size_in_bits_frame_hdr_obu, byte_offset_frame_hdr_obu_size) = if back_annotate {
        (offsets.total_bits, offsets.obu_size_byte)
    } else {
        (0, 0)
    };

    let pic = EncPictureParameterBufferAV1::new(
        (width - 1) as u16,
        (height - 1) as u16,
        recon.id(),
        coded_buf,
        reference_frames,
        [0; 7],
        0,
        if keyframe { PRIMARY_REF_NONE } else { 0 },
        order_hint,
        refresh_frame_flags,
        &ref_ctrl_l0,
        &RefFrameCtrlAV1::default(),
        &picture_flags,
        0,
        0,
        0,
        [LOOP_FILTER_LEVEL; 2],
        LOOP_FILTER_LEVEL,
        LOOP_FILTER_LEVEL,
        &loop_filter_flags,
        8,
        INTERP_SWITCHABLE,
        [0; 8],
        [0; 2],
        base_qindex,
        0,
        0,
        0,
        0,
        0,
        if cqp { 0 } else { 1 },
        if cqp { 0 } else { 255 },
        &AV1EncQMatrixFlags::new(false, 0, 0, 0),
        &mode_control,
        &segments,
        1,
        1,
        width_in_sbs_minus_1,
        height_in_sbs_minus_1,
        0,
        0,
        0,
        [0; 8],
        [0; 8],
        &loop_restoration,
        wm,
        bit_offset_qindex,
        bit_offset_segmentation,
        bit_offset_loopfilter_params,
        bit_offset_cdef_params,
        size_in_bits_cdef_params,
        byte_offset_frame_hdr_obu_size,
        size_in_bits_frame_hdr_obu,
        &tile_group_hdr,
        0,
        0,
    );
    if keyframe {
        encoder.push_packed(
            &av1_headers::sequence_header(&av1_headers::SeqParams {
                width,
                height,
                seq_level_idx: level_idx,
                seq_tier: 0,
                high_bitdepth: ten_bit,
                colour: config.colour,
            }),
            out,
        )?;
    }
    out.push(BufferType::EncPictureParameter(EncPictureParameter::AV1(pic)));
    encoder.push_packed(&packed, out)?;

    out.push(BufferType::EncSliceParameter(EncSliceParameter::AV1(
        EncTileGroupBufferAV1::new(0, 0),
    )));
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

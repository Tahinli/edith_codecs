//! H.264 encode parameters.
//!
//! `pic_order_cnt_type` is 2 — decode order *is* output order — because this
//! encoder codes no B frames, and type 2 then costs no picture order count in
//! any slice header. Everything else follows from one reference and one slice
//! per picture.

use std::sync::Arc;

use ec_va::Buffer;

use super::{Encoder, RateControlMode, headers};
use crate::error::Result;
use crate::params::enc::{
    EncPictureParameterBufferH264, EncSequenceParameterBufferH264, EncSliceParameterBufferH264,
};
use crate::params::h264::{PICTURE_SHORT_TERM_REFERENCE, VAPictureH264};
use crate::params::param_buffer;
use crate::pool::PooledSurface;

/// `log2_max_frame_num_minus4` = 0, i.e. `frame_num` wraps at 16 — far beyond
/// the single reference this encoder keeps.
const LOG2_MAX_FRAME_NUM: u64 = 4;

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
    let frame_num = if keyframe {
        0
    } else {
        (encoder.coded_count() % (1 << LOG2_MAX_FRAME_NUM)) as u16
    };

    let mut seq = EncSequenceParameterBufferH264 {
        level_idc: level_idc(coded_w, coded_h, config.framerate),
        intra_period: config.gop_size.max(1),
        intra_idr_period: config.gop_size.max(1),
        ip_period: 1,
        bits_per_second: match config.rate_control {
            RateControlMode::ConstantBitrate => config.bitrate,
            RateControlMode::ConstantQp { .. } => 0,
        },
        max_num_ref_frames: 1,
        picture_width_in_mbs: (coded_w / 16) as u16,
        picture_height_in_mbs: (coded_h / 16) as u16,
        // 4:2:0 in a crop rectangle of two luma samples per unit (7.4.2.1.1).
        frame_cropping_flag: u8::from(coded_w != config.width || coded_h != config.height),
        frame_crop_right_offset: (coded_w - config.width) / 2,
        frame_crop_bottom_offset: (coded_h - config.height) / 2,
        vui_parameters_present_flag: 1,
        num_units_in_tick: config.framerate.1.max(1),
        // time_scale is twice the frame rate: a "tick" is a field period.
        time_scale: config.framerate.0.max(1) * 2,
        ..EncSequenceParameterBufferH264::default()
    };
    seq.seq_fields = seq
        .seq_fields
        .chroma_format_idc(1)
        .frame_mbs_only_flag(1)
        .direct_8x8_inference_flag(1)
        .log2_max_frame_num_minus4(0)
        .pic_order_cnt_type(2);
    seq.vui_fields = seq
        .vui_fields
        .timing_info_present_flag(1)
        .fixed_frame_rate_flag(1);
    out.push(param_buffer(context, &seq)?);

    let mut pic = EncPictureParameterBufferH264 {
        curr_pic: VAPictureH264::frame(
            recon.id(),
            u32::from(frame_num),
            2 * encoder.coded_count() as i32,
            PICTURE_SHORT_TERM_REFERENCE,
        ),
        coded_buf,
        frame_num,
        pic_init_qp: match config.rate_control {
            RateControlMode::ConstantQp { qp } => qp.clamp(1, 51) as u8,
            RateControlMode::ConstantBitrate => 26,
        },
        ..EncPictureParameterBufferH264::default()
    };
    if let Some(reference) = encoder.reference().filter(|_| !keyframe) {
        pic.reference_frames[0] = VAPictureH264::frame(
            reference.id(),
            u32::from(frame_num.wrapping_sub(1)),
            2 * (encoder.coded_count() as i32 - 1),
            PICTURE_SHORT_TERM_REFERENCE,
        );
    }
    pic.pic_fields = pic
        .pic_fields
        .idr_pic_flag(u32::from(keyframe))
        .reference_pic_flag(1)
        .entropy_coding_mode_flag(1) // CABAC: High profile, and 10% smaller
        .deblocking_filter_control_present_flag(1);
    out.push(param_buffer(context, &pic)?);

    let mut slice = EncSliceParameterBufferH264 {
        macroblock_address: 0,
        num_macroblocks: (coded_w / 16) * (coded_h / 16),
        slice_type: if keyframe { 2 } else { 0 },
        // idr_pic_id alternates between consecutive IDR pictures (7.4.3).
        idr_pic_id: ((encoder.gop_position() / config.gop_size.max(1)) % 2) as u16,
        num_ref_idx_active_override_flag: 0,
        ..EncSliceParameterBufferH264::default()
    };
    if !keyframe && let Some(reference) = encoder.reference() {
        slice.ref_pic_list0[0] = VAPictureH264::frame(
            reference.id(),
            u32::from(frame_num.wrapping_sub(1)),
            2 * (encoder.coded_count() as i32 - 1),
            PICTURE_SHORT_TERM_REFERENCE,
        );
    }

    // Packed headers, in bitstream order: the parameter sets open every key
    // frame (so a stream is seekable and a decoder can start anywhere), and the
    // slice header opens every picture.
    let header_params = headers::H264Params {
        level_idc: seq.level_idc,
        mb_width: u32::from(seq.picture_width_in_mbs),
        mb_height: u32::from(seq.picture_height_in_mbs),
        crop_right: seq.frame_crop_right_offset,
        crop_bottom: seq.frame_crop_bottom_offset,
        num_units_in_tick: seq.num_units_in_tick,
        time_scale: seq.time_scale,
        pic_init_qp: pic.pic_init_qp,
        colour: config.colour,
    };
    if keyframe {
        encoder.push_packed(&headers::h264_sps(&header_params), out)?;
        encoder.push_packed(&headers::h264_pps(&header_params), out)?;
    }
    encoder.push_packed(
        &headers::h264_slice_header(keyframe, u32::from(frame_num), u32::from(slice.idr_pic_id)),
        out,
    )?;
    out.push(param_buffer(context, &slice)?);
    Ok(())
}

/// The lowest Annex A level that fits the picture size and rate.
///
/// Advertising level 6.2 for a 640x480 clip is legal and lazy; a player that
/// checks levels against its decoder budget deserves the real one.
fn level_idc(width: u32, height: u32, framerate: (u32, u32)) -> u8 {
    const LEVELS: [(u8, u64, u64); 12] = [
        (10, 1485, 99),
        (11, 3000, 396),
        (20, 11880, 396),
        (21, 19800, 792),
        (22, 20250, 1620),
        (30, 40500, 1620),
        (31, 108000, 3600),
        (32, 216000, 5120),
        (40, 245760, 8192),
        (50, 589824, 22080),
        (51, 983040, 36864),
        (52, 2073600, 36864),
    ];
    let frame_mbs = u64::from(width / 16) * u64::from(height / 16);
    let fps = f64::from(framerate.0.max(1)) / f64::from(framerate.1.max(1));
    let mbps = (frame_mbs as f64 * fps).ceil() as u64;
    for &(idc, max_mbps, max_fs) in &LEVELS {
        if mbps <= max_mbps && frame_mbs <= max_fs {
            return idc;
        }
    }
    62
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_follow_size_and_rate() {
        assert_eq!(level_idc(1920, 1088, (30, 1)), 40);
        assert_eq!(level_idc(1920, 1088, (60, 1)), 50);
        assert_eq!(level_idc(3840, 2160, (30, 1)), 51);
        assert_eq!(level_idc(640, 480, (30, 1)), 30);
    }
}

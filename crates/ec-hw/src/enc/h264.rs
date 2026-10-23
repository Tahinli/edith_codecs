//! H.264 encode parameters.
//!
//! `pic_order_cnt_type` is 2 — decode order *is* output order — because this
//! encoder codes no B frames, and type 2 then costs no picture order count in
//! any slice header. Everything else follows from one reference and one slice
//! per picture.

use std::rc::Rc;

use ec_va::va::{
    BufferType, EncPictureParameter, EncPictureParameterBufferH264, EncSequenceParameter,
    EncSequenceParameterBufferH264, EncSliceParameter, EncSliceParameterBufferH264,
    H264EncFrameCropOffsets, H264EncPicFields, H264EncSeqFields, H264VuiFields, PictureH264,
    VABufferID, VA_INVALID_ID,
};

use super::{Encoder, RateControlMode, headers};
use crate::error::Result;
use crate::params::h264::{PICTURE_INVALID, PICTURE_SHORT_TERM_REFERENCE};
use crate::params::INVALID_SURFACE;
use crate::pool::PooledSurface;

/// `log2_max_frame_num_minus4` = 0, i.e. `frame_num` wraps at 16 — far beyond
/// the single reference this encoder keeps.
const LOG2_MAX_FRAME_NUM: u64 = 4;
const MAX_FRAME_NUM: u16 = 1 << LOG2_MAX_FRAME_NUM;

/// `frame_num` of the picture `gop_position` pictures after the key frame
/// (7.4.3): the key frame is 0, every reference picture after it adds one, and
/// the count wraps at `MaxFrameNum`. It restarts at every IDR, so a decoder sees
/// the same numbering whichever IDR it started from.
fn frame_num_at(gop_position: u32) -> u16 {
    (gop_position % u32::from(MAX_FRAME_NUM)) as u16
}

/// `frame_num` of the reference picture, one step before `frame_num` (wrapping
/// at `MaxFrameNum`, not at 2^16).
fn previous_frame_num(frame_num: u16) -> u16 {
    (frame_num + MAX_FRAME_NUM - 1) % MAX_FRAME_NUM
}

/// An unused entry of a reference list: no surface, flagged invalid.
fn invalid_picture() -> PictureH264 {
    PictureH264::new(INVALID_SURFACE, 0, PICTURE_INVALID, 0, 0)
}

/// A short-term reference frame whose top and bottom field order counts are `poc`.
fn frame_picture(surface: u32, frame_idx: u32, poc: i32) -> PictureH264 {
    PictureH264::new(
        surface,
        frame_idx,
        PICTURE_SHORT_TERM_REFERENCE,
        poc,
        poc,
    )
}

pub(super) fn parameters(
    encoder: &Encoder,
    recon: &Rc<PooledSurface>,
    coded_buf: VABufferID,
    keyframe: bool,
    out: &mut Vec<BufferType>,
) -> Result<()> {
    let config = *encoder.config();
    let (coded_w, coded_h) = encoder.coded_size();
    let mb_width = coded_w / 16;
    let mb_height = coded_h / 16;
    // Picture order count type 2 (see `pic_order_cnt_type` in the SPS below):
    // TopFieldOrderCnt = 2 * (FrameNumOffset + frame_num). Both restart at the
    // key frame, so the count follows the GOP and not the stream.
    let (frame_num, poc) = if keyframe {
        (0, 0)
    } else {
        let position = encoder.gop_position();
        (frame_num_at(position), 2 * position as i32)
    };
    let level_idc = level_idc(coded_w, coded_h, config.framerate);
    let intra_period = config.gop_size.max(1);
    let bits_per_second = match config.rate_control {
        RateControlMode::ConstantQp { .. } => 0,
        _ => config.bitrate,
    };
    // time_scale is twice the frame rate: a "tick" is a field period.
    let num_units_in_tick = config.framerate.1.max(1);
    let time_scale = config.framerate.0.max(1) * 2;

    // 4:2:0 in a crop rectangle of two luma samples per unit (7.4.2.1.1).
    let crop_right = (coded_w - config.width) / 2;
    let crop_bottom = (coded_h - config.height) / 2;
    let crop = (coded_w != config.width || coded_h != config.height)
        .then(|| H264EncFrameCropOffsets::new(0, crop_right, 0, crop_bottom));

    // chroma_format_idc 1, frame_mbs_only, direct_8x8_inference,
    // log2_max_frame_num_minus4 0, pic_order_cnt_type 2.
    let seq_fields = H264EncSeqFields::new(1, 1, 0, 0, 1, 0, 2, 0, 0);
    // timing_info_present_flag, fixed_frame_rate_flag.
    let vui = H264VuiFields::new(0, 1, 0, 0, 0, 1, 0, 0);
    let seq = EncSequenceParameterBufferH264::new(
        0,
        level_idc,
        intra_period,
        intra_period,
        1,
        bits_per_second,
        1,
        mb_width as u16,
        mb_height as u16,
        &seq_fields,
        0,
        0,
        0,
        0,
        0,
        [0; 256],
        crop,
        Some(vui),
        0,
        0,
        0,
        num_units_in_tick,
        time_scale,
    );
    out.push(BufferType::EncSequenceParameter(EncSequenceParameter::H264(seq)));

    let pic_init_qp = match config.rate_control {
        RateControlMode::ConstantQp { qp } => qp.clamp(1, 51) as u8,
        _ => 26,
    };
    let mut reference_frames: [PictureH264; 16] = std::array::from_fn(|_| invalid_picture());
    if let Some(reference) = encoder.reference().filter(|_| !keyframe) {
        reference_frames[0] = frame_picture(
            reference.id(),
            u32::from(previous_frame_num(frame_num)),
            poc - 2,
        );
    }
    let cabac = config.h264_profile.cabac();
    // reference_pic_flag = 1; CABAC in Main and High (10% smaller), CAVLC in
    // Constrained Baseline; deblocking_filter_control_present_flag = 1.
    let pic_fields = H264EncPicFields::new(
        u32::from(keyframe),
        1,
        u32::from(cabac),
        0,
        0,
        0,
        0,
        1,
        0,
        0,
        0,
    );
    let pic = EncPictureParameterBufferH264::new(
        frame_picture(recon.id(), u32::from(frame_num), poc),
        reference_frames,
        coded_buf,
        0,
        0,
        0,
        frame_num,
        pic_init_qp,
        0,
        0,
        0,
        0,
        &pic_fields,
    );
    out.push(BufferType::EncPictureParameter(EncPictureParameter::H264(pic)));

    // idr_pic_id alternates between consecutive IDR pictures (7.4.3).
    let idr_pic_id = (encoder.idr_count() % 2) as u16;
    let mut ref_pic_list0: [PictureH264; 32] = std::array::from_fn(|_| invalid_picture());
    let ref_pic_list1: [PictureH264; 32] = std::array::from_fn(|_| invalid_picture());
    if !keyframe && let Some(reference) = encoder.reference() {
        ref_pic_list0[0] = frame_picture(
            reference.id(),
            u32::from(previous_frame_num(frame_num)),
            poc - 2,
        );
    }
    let slice = EncSliceParameterBufferH264::new(
        0,
        mb_width * mb_height,
        VA_INVALID_ID,
        if keyframe { 2 } else { 0 },
        0,
        idr_pic_id,
        0,
        0,
        [0, 0],
        0,
        0,
        0,
        0,
        ref_pic_list0,
        ref_pic_list1,
        0,
        0,
        0,
        [0; 32],
        [0; 32],
        0,
        [[0; 2]; 32],
        [[0; 2]; 32],
        0,
        [0; 32],
        [0; 32],
        0,
        [[0; 2]; 32],
        [[0; 2]; 32],
        0,
        0,
        0,
        0,
        0,
    );

    // Packed headers, in bitstream order: the parameter sets open every key
    // frame (so a stream is seekable and a decoder can start anywhere), and the
    // slice header opens every picture.
    let header_params = headers::H264Params {
        level_idc,
        mb_width,
        mb_height,
        crop_right,
        crop_bottom,
        num_units_in_tick,
        time_scale,
        pic_init_qp,
        colour: config.colour,
        profile: config.h264_profile,
    };
    if keyframe {
        encoder.push_packed(&headers::h264_sps(&header_params), out)?;
        encoder.push_packed(&headers::h264_pps(&header_params), out)?;
    }
    encoder.push_packed(
        &headers::h264_slice_header(keyframe, u32::from(frame_num), u32::from(idr_pic_id)),
        out,
    )?;
    out.push(BufferType::EncSliceParameter(EncSliceParameter::H264(slice)));
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

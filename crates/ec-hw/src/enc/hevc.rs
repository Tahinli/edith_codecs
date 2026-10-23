//! HEVC encode parameters.
//!
//! HEVC encoding in-tree is one of the reasons this crate exists: the incumbent
//! stack carried it on a branch for months. It is the same IDR-then-P structure
//! as the H.264 path, in HEVC's units — CTBs instead of macroblocks, a picture
//! order count instead of `frame_num`, and a conformance window instead of a
//! frame crop, which is what makes 1916x1080 (or any width that is not a
//! multiple of 32) come out the size it went in.

use std::rc::Rc;

use ec_va::va::{
    BufferType, EncPictureParameter, EncPictureParameterBufferHEVC, EncSequenceParameter,
    EncSequenceParameterBufferHEVC, EncSliceParameter, EncSliceParameterBufferHEVC,
    HEVCEncPicFields, HEVCEncSeqFields, HevcEncPicSccFields, HevcEncSeqSccFields,
    HevcEncSliceFields, HevcEncVuiFields, PictureHEVC, VABufferID,
};

use super::headers::hevc as hevc_headers;
use super::{Encoder, RateControlMode};
use crate::error::Result;
use crate::params::INVALID_SURFACE;
use crate::pool::PooledSurface;

/// `NAL_IDR_W_RADL`.
const NAL_IDR_W_RADL: u8 = 19;
/// `NAL_TRAIL_R`.
const NAL_TRAIL_R: u8 = 1;
/// `VA_PICTURE_HEVC_INVALID`: an unused entry of a reference list.
const PICTURE_INVALID: u32 = 0x0000_0001;

/// An unused entry of a reference list.
fn invalid_picture() -> PictureHEVC {
    PictureHEVC::new(INVALID_SURFACE, 0, PICTURE_INVALID)
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
    let poc = if keyframe {
        0
    } else {
        encoder.gop_position() as i32
    };
    let general_level_idc = level_idc(coded_w, coded_h, config.framerate);
    let intra_period = config.gop_size.max(1);
    let bits_per_second = match config.rate_control {
        RateControlMode::ConstantQp { .. } => 0,
        _ => config.bitrate,
    };
    let vui_num_units_in_tick = config.framerate.1.max(1);
    let vui_time_scale = config.framerate.0.max(1);

    // chroma_format_idc 1, bit depths 8, strong intra smoothing, AMP, SAO.
    let seq_fields = HEVCEncSeqFields::new(1, 0, 0, 0, 0, 1, 1, 1, 0, 0, 0, 0, 0);
    // vui_timing_info_present_flag.
    let vui = HevcEncVuiFields::new(0, 0, 0, 1, 0, 0, 0, 0, 0, 0);
    let scc = HevcEncSeqSccFields::new(0);
    // 64x64 CTBs from 8x8 minimum CBs: log2_min = 0 (8), diff = 3 (64).
    // Not a preference — this driver codes 64x64 CTBs whatever the sequence
    // parameters say, and `num_ctu_in_slice` below has to count the same
    // blocks the hardware does.
    let seq = EncSequenceParameterBufferHEVC::new(
        1,
        general_level_idc,
        0,
        intra_period,
        intra_period,
        1,
        bits_per_second,
        coded_w as u16,
        coded_h as u16,
        &seq_fields,
        0,
        3,
        0,
        3,
        1,
        1,
        0,
        0,
        0,
        0,
        Some(vui),
        0,
        0,
        0,
        vui_num_units_in_tick,
        vui_time_scale,
        0,
        0,
        0,
        &scc,
    );
    out.push(BufferType::EncSequenceParameter(EncSequenceParameter::HEVC(seq)));

    let pic_init_qp = match config.rate_control {
        RateControlMode::ConstantQp { qp } => qp.clamp(1, 51) as u8,
        _ => 26,
    };
    let nal_unit_type = if keyframe {
        NAL_IDR_W_RADL
    } else {
        NAL_TRAIL_R
    };
    // Off, and it has to be off: this driver omits
    // `slice_loop_filter_across_slices_enabled_flag` from the slice header
    // it writes, so a PPS that says the flag is present describes a header
    // that is one bit longer than the one in the stream — every P picture
    // then decodes as `alignment_bit_equal_to_one = 0` (measured).
    let pic_fields = HEVCEncPicFields::new(
        u32::from(keyframe),
        // 1 = I, 2 = P in this driver's coding_type numbering.
        if keyframe { 1 } else { 2 },
        1,
        0,
        0,
        0,
        1,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    );
    let pic_scc = HevcEncPicSccFields::new(0);
    let mut reference_frames: [PictureHEVC; 15] = std::array::from_fn(|_| invalid_picture());
    if let Some(reference) = encoder.reference().filter(|_| !keyframe) {
        reference_frames[0] = PictureHEVC::new(reference.id(), poc - 1, 0);
    }
    let pic = EncPictureParameterBufferHEVC::new(
        PictureHEVC::new(recon.id(), poc, 0),
        reference_frames,
        coded_buf,
        0xff,
        0,
        pic_init_qp,
        0,
        0,
        0,
        0,
        0,
        [0; 19],
        [0; 21],
        0,
        0,
        0,
        0,
        0,
        nal_unit_type,
        &pic_fields,
        0,
        0,
        &pic_scc,
    );
    out.push(BufferType::EncPictureParameter(EncPictureParameter::HEVC(pic)));

    // CTBs are 64x64 by the sequence parameters above.
    let ctbs = coded_w.div_ceil(64) * coded_h.div_ceil(64);
    let mut ref_pic_list0: [PictureHEVC; 15] = std::array::from_fn(|_| invalid_picture());
    let ref_pic_list1: [PictureHEVC; 15] = std::array::from_fn(|_| invalid_picture());
    if !keyframe && let Some(reference) = encoder.reference() {
        ref_pic_list0[0] = PictureHEVC::new(reference.id(), poc - 1, 0);
    }
    // last_slice_of_pic_flag, slice_sao_luma/chroma, and the reference override
    // on P pictures. The loop filter across slices stays off, matching the PPS.
    let slice_fields = HevcEncSliceFields::new(
        1,
        0,
        0,
        0,
        1,
        1,
        u32::from(!keyframe),
        0,
        0,
        0,
        0,
        0,
    );
    let slice = EncSliceParameterBufferHEVC::new(
        0,
        ctbs,
        if keyframe { 2 } else { 1 }, // 2 = I, 1 = P
        0,
        0,
        0,
        ref_pic_list0,
        ref_pic_list1,
        0,
        0,
        [0; 15],
        [0; 15],
        [[0; 2]; 15],
        [[0; 2]; 15],
        [0; 15],
        [0; 15],
        [[0; 2]; 15],
        [[0; 2]; 15],
        5,
        0,
        0,
        0,
        0,
        0,
        &slice_fields,
        0,
        0,
    );

    let header_params = hevc_headers::Params {
        width: config.width,
        height: config.height,
        coded_width: coded_w,
        coded_height: coded_h,
        level_idc: general_level_idc,
        num_units_in_tick: vui_num_units_in_tick,
        time_scale: vui_time_scale,
        init_qp: pic_init_qp,
        colour: config.colour,
    };
    let (vps, sps, pps) = hevc_headers::parameter_sets(&header_params);
    if keyframe {
        encoder.push_packed(&hevc_headers::packed_vps(&vps), out)?;
        encoder.push_packed(&hevc_headers::packed_sps(&sps), out)?;
        encoder.push_packed(&hevc_headers::packed_pps(&pps), out)?;
    }
    // The slice header goes in too. This driver composes its own for HEVC and
    // ignores the content of this one — but it emits the parameter sets above
    // only when a slice header accompanies them, so dropping it costs the
    // stream its VPS, SPS and PPS (measured 2026-08-14).
    encoder.push_packed(&hevc_headers::packed_slice(&sps, &pps, keyframe, poc), out)?;
    out.push(BufferType::EncSliceParameter(EncSliceParameter::HEVC(slice)));
    Ok(())
}

/// The lowest level whose luma sample rate and picture size fit (Annex A).
fn level_idc(width: u32, height: u32, framerate: (u32, u32)) -> u8 {
    // (general_level_idc = level * 30, MaxLumaPs, MaxLumaSr)
    const LEVELS: [(u8, u64, u64); 9] = [
        (30, 36_864, 552_960),
        (60, 122_880, 3_686_400),
        (63, 245_760, 7_372_800),
        (90, 552_960, 16_588_800),
        (93, 983_040, 33_177_600),
        (120, 2_228_224, 66_846_720),
        (123, 2_228_224, 133_693_440),
        (150, 8_912_896, 267_386_880),
        (153, 8_912_896, 534_773_760),
    ];
    let samples = u64::from(width) * u64::from(height);
    let fps = f64::from(framerate.0.max(1)) / f64::from(framerate.1.max(1));
    let rate = (samples as f64 * fps).ceil() as u64;
    for &(idc, max_ps, max_sr) in &LEVELS {
        if samples <= max_ps && rate <= max_sr {
            return idc;
        }
    }
    186
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_follow_size_and_rate() {
        // 1080p30 is level 4.0 (120), 1080p60 level 4.1 (123), 2160p30 5.0 (150).
        assert_eq!(level_idc(1920, 1088, (30, 1)), 120);
        assert_eq!(level_idc(1920, 1088, (60, 1)), 123);
        assert_eq!(level_idc(3840, 2160, (30, 1)), 150);
    }
}

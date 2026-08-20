//! HEVC encode parameters.
//!
//! HEVC encoding in-tree is one of the reasons this crate exists: the incumbent
//! stack carried it on a branch for months. It is the same IDR-then-P structure
//! as the H.264 path, in HEVC's units — CTBs instead of macroblocks, a picture
//! order count instead of `frame_num`, and a conformance window instead of a
//! frame crop, which is what makes 1916x1080 (or any width that is not a
//! multiple of 32) come out the size it went in.

use std::sync::Arc;

use ec_va::Buffer;

use super::headers::hevc as hevc_headers;
use super::{Encoder, RateControlMode};
use crate::error::Result;
use crate::params::enc::{
    EncPictureParameterBufferHEVC, EncSequenceParameterBufferHEVC, EncSliceParameterBufferHEVC,
};
use crate::params::hevc::VAPictureHEVC;
use crate::params::param_buffer;
use crate::pool::PooledSurface;

/// `NAL_IDR_W_RADL`.
const NAL_IDR_W_RADL: u8 = 19;
/// `NAL_TRAIL_R`.
const NAL_TRAIL_R: u8 = 1;

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
    let poc = if keyframe {
        0
    } else {
        encoder.gop_position() as i32
    };

    let mut seq = EncSequenceParameterBufferHEVC {
        general_profile_idc: 1, // Main
        general_level_idc: level_idc(coded_w, coded_h, config.framerate),
        general_tier_flag: 0,
        intra_period: config.gop_size.max(1),
        intra_idr_period: config.gop_size.max(1),
        ip_period: 1,
        bits_per_second: match config.rate_control {
            RateControlMode::ConstantBitrate => config.bitrate,
            RateControlMode::ConstantQp { .. } => 0,
        },
        pic_width_in_luma_samples: coded_w as u16,
        pic_height_in_luma_samples: coded_h as u16,
        // 64x64 CTBs from 8x8 minimum CBs: log2_min = 0 (8), diff = 3 (64).
        // Not a preference — this driver codes 64x64 CTBs whatever the sequence
        // parameters say, and `num_ctu_in_slice` below has to count the same
        // blocks the hardware does.
        log2_min_luma_coding_block_size_minus3: 0,
        log2_diff_max_min_luma_coding_block_size: 3,
        log2_min_transform_block_size_minus2: 0,
        log2_diff_max_min_transform_block_size: 3,
        max_transform_hierarchy_depth_inter: 1,
        max_transform_hierarchy_depth_intra: 1,
        vui_parameters_present_flag: 1,
        vui_num_units_in_tick: config.framerate.1.max(1),
        vui_time_scale: config.framerate.0.max(1),
        ..EncSequenceParameterBufferHEVC::default()
    };
    seq.seq_fields = seq
        .seq_fields
        .chroma_format_idc(1)
        .bit_depth_luma_minus8(0)
        .bit_depth_chroma_minus8(0)
        .strong_intra_smoothing_enabled_flag(1)
        .amp_enabled_flag(1)
        .sample_adaptive_offset_enabled_flag(1);
    seq.vui_fields = seq.vui_fields.vui_timing_info_present_flag(1);
    out.push(param_buffer(context, &seq)?);

    let mut pic = EncPictureParameterBufferHEVC {
        decoded_curr_pic: VAPictureHEVC::new(recon.id(), poc, 0),
        coded_buf,
        collocated_ref_pic_index: 0xff,
        pic_init_qp: match config.rate_control {
            RateControlMode::ConstantQp { qp } => qp.clamp(1, 51) as u8,
            RateControlMode::ConstantBitrate => 26,
        },
        nal_unit_type: if keyframe {
            NAL_IDR_W_RADL
        } else {
            NAL_TRAIL_R
        },
        ..EncPictureParameterBufferHEVC::default()
    };
    if let Some(reference) = encoder.reference().filter(|_| !keyframe) {
        pic.reference_frames[0] = VAPictureHEVC::new(reference.id(), poc - 1, 0);
    }
    pic.pic_fields = pic
        .pic_fields
        .idr_pic_flag(u32::from(keyframe))
        // 1 = I, 2 = P in this driver's coding_type numbering.
        .coding_type(if keyframe { 1 } else { 2 })
        .reference_pic_flag(1)
        .transform_skip_enabled_flag(1)
        // Off, and it has to be off: this driver omits
        // `slice_loop_filter_across_slices_enabled_flag` from the slice header
        // it writes, so a PPS that says the flag is present describes a header
        // that is one bit longer than the one in the stream — every P picture
        // then decodes as `alignment_bit_equal_to_one = 0` (measured).
        .pps_loop_filter_across_slices_enabled_flag(0);
    out.push(param_buffer(context, &pic)?);

    // CTBs are 64x64 by the sequence parameters above.
    let ctbs = coded_w.div_ceil(64) * coded_h.div_ceil(64);
    let mut slice = EncSliceParameterBufferHEVC {
        slice_segment_address: 0,
        num_ctu_in_slice: ctbs,
        slice_type: if keyframe { 2 } else { 1 }, // 2 = I, 1 = P
        num_ref_idx_l0_active_minus1: 0,
        num_ref_idx_l1_active_minus1: 0,
        max_num_merge_cand: 5,
        ..EncSliceParameterBufferHEVC::default()
    };
    if !keyframe && let Some(reference) = encoder.reference() {
        slice.ref_pic_list0[0] = VAPictureHEVC::new(reference.id(), poc - 1, 0);
    }
    slice.slice_fields = slice
        .slice_fields
        .last_slice_of_pic_flag(1)
        .slice_sao_luma_flag(1)
        .slice_sao_chroma_flag(1)
        .cabac_init_flag(0)
        .num_ref_idx_active_override_flag(u32::from(!keyframe))
        .slice_loop_filter_across_slices_enabled_flag(0);

    let header_params = hevc_headers::Params {
        width: config.width,
        height: config.height,
        coded_width: coded_w,
        coded_height: coded_h,
        level_idc: seq.general_level_idc,
        num_units_in_tick: seq.vui_num_units_in_tick,
        time_scale: seq.vui_time_scale,
        init_qp: pic.pic_init_qp,
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
    out.push(param_buffer(context, &slice)?);
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

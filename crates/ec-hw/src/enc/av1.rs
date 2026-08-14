//! AV1 encode parameters — opt-in only.
//!
//! Kept deliberately minimal: one tile, one reference, no segmentation, no
//! superres, no film grain. The point of this path today is that it exists,
//! compiles, and either produces a frame or a typed error — never a panic and
//! never a submission this crate cannot describe. See the module docs of
//! [`super`] for why it is not on by default.

use std::sync::Arc;

use ec_va::Buffer;

use super::{Encoder, RateControlMode};
use crate::error::Result;
use crate::params::enc::{
    EncPictureParameterBufferAV1, EncSequenceParameterBufferAV1, EncTileGroupBufferAV1,
};
use crate::params::param_buffer;
use crate::pool::PooledSurface;

/// `KEY_FRAME` and `INTER_FRAME` (spec 6.8.2).
const KEY_FRAME: u32 = 0;
const INTER_FRAME: u32 = 1;

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

    let mut seq = EncSequenceParameterBufferAV1 {
        seq_profile: 0,
        // 0 lets the driver pick the level from the picture size and rate.
        seq_level_idx: 0,
        intra_period: config.gop_size.max(1),
        ip_period: 1,
        bits_per_second: match config.rate_control {
            RateControlMode::ConstantBitrate => config.bitrate,
            RateControlMode::ConstantQp { .. } => 0,
        },
        order_hint_bits_minus_1: 6,
        ..EncSequenceParameterBufferAV1::default()
    };
    seq.seq_fields = seq
        .seq_fields
        .enable_order_hint(1)
        .bit_depth_minus8(0)
        .subsampling_x(1)
        .subsampling_y(1);
    out.push(param_buffer(context, &seq)?);

    let order_hint = (encoder.gop_position() % 128) as u8;
    let mut pic = EncPictureParameterBufferAV1 {
        frame_width_minus_1: (config.width.max(1) - 1) as u16,
        frame_height_minus_1: (config.height.max(1) - 1) as u16,
        reconstructed_frame: recon.id(),
        coded_buf,
        primary_ref_frame: 7, // PRIMARY_REF_NONE
        order_hint,
        refresh_frame_flags: 0xff,
        base_qindex: match config.rate_control {
            RateControlMode::ConstantQp { qp } => qp.clamp(1, 255) as u8,
            RateControlMode::ConstantBitrate => 128,
        },
        min_base_qindex: 1,
        max_base_qindex: 255,
        tile_cols: 1,
        tile_rows: 1,
        superres_scale_denominator: 8,
        ..EncPictureParameterBufferAV1::default()
    };
    // One tile covering the whole frame, in 64x64 superblocks.
    pic.width_in_sbs_minus_1[0] = (coded_w.div_ceil(64).max(1) - 1) as u16;
    pic.height_in_sbs_minus_1[0] = (coded_h.div_ceil(64).max(1) - 1) as u16;
    if let Some(reference) = encoder.reference().filter(|_| !keyframe) {
        for slot in pic.reference_frames.iter_mut() {
            *slot = reference.id();
        }
        pic.primary_ref_frame = 0;
    }
    pic.picture_flags = pic
        .picture_flags
        .frame_type(if keyframe { KEY_FRAME } else { INTER_FRAME })
        .disable_cdf_update(1)
        .disable_frame_end_update_cdf(1)
        .enable_frame_obu(1);
    out.push(param_buffer(context, &pic)?);

    let tile_group = EncTileGroupBufferAV1 {
        tg_start: 0,
        tg_end: 0,
        ..EncTileGroupBufferAV1::default()
    };
    out.push(param_buffer(context, &tile_group)?);
    Ok(())
}

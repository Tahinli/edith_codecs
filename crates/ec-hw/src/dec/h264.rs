//! Stateless H.264 decoding.

use std::sync::Arc;

use ec_h264_syntax::{
    AnnexBIter, DecRefPicMarking, NalHeader, NalUnitType, Pps, PredWeightTable, SliceHeader,
    SliceType, Sps, unescape_rbsp,
};
use ec_va::caps::Profile;
use ec_va::{Buffer, Display, sys};

use super::dpb264::{Dpb, Mark, PicInfo, Picture, Poc, RefList};
use super::{ReadyFrames, Session, StreamInfo};
use crate::error::{Error, Result};
use crate::frame::Frame;
use crate::params::h264::{
    IQMatrixBufferH264, PICTURE_LONG_TERM_REFERENCE, PICTURE_SHORT_TERM_REFERENCE,
    PictureParameterBufferH264, SliceParameterBufferH264, VAPictureH264,
};
use crate::params::param_buffer;
use crate::pool::PooledSurface;

/// Extra surfaces beyond the DPB: one being decoded, one or two in the caller's
/// hands, plus slack so a `frame_num` gap fill never starves the decoder.
const EXTRA_SURFACES: usize = 6;

/// The picture currently being assembled from its slices.
struct Current {
    surface: Arc<PooledSurface>,
    id: i32,
    sps_id: u8,
    info: PicInfo,
    poc: Poc,
    timestamp: i64,
    /// `dec_ref_pic_marking()` from the picture's first slice header, which is
    /// what 8.2.5 applies once the whole picture is decoded.
    marking: Option<DecRefPicMarking>,
    /// Parameter and data buffers submitted so far.
    buffers: Vec<Buffer>,
    slices: usize,
}

/// A stateless H.264 decoder.
pub struct H264Decoder {
    display: Arc<Display>,
    sps_map: [Option<Sps>; 32],
    pps_map: Vec<Option<Pps>>,
    session: Option<Session>,
    dpb: Dpb,
    ready: ReadyFrames,
    current: Option<Current>,
    /// Reusable unescape buffer; NAL payloads are rewritten into it in turn.
    rbsp: Vec<u8>,
    gap_frames: u64,
}

impl H264Decoder {
    /// A decoder with no stream state. The GPU context is created when the
    /// first SPS arrives, because only then is the size and bit depth known.
    pub fn new(display: &Arc<Display>) -> H264Decoder {
        H264Decoder {
            display: Arc::clone(display),
            sps_map: [const { None }; 32],
            pps_map: (0..256).map(|_| None).collect(),
            session: None,
            dpb: Dpb::default(),
            ready: ReadyFrames::default(),
            current: None,
            rbsp: Vec::new(),
            gap_frames: 0,
        }
    }

    /// Decode one Annex B access unit.
    pub fn decode(&mut self, data: &[u8], timestamp: i64) -> Result<()> {
        for nal in AnnexBIter::new(data) {
            let Some((&first, payload)) = nal.split_first() else {
                continue;
            };
            let header = NalHeader::parse(first)?;
            match header.unit_type {
                NalUnitType::Sps => {
                    let mut rbsp = std::mem::take(&mut self.rbsp);
                    unescape_rbsp(payload, &mut rbsp);
                    let sps = Sps::parse(&rbsp);
                    self.rbsp = rbsp;
                    let sps = sps?;
                    self.dpb.configure(&sps);
                    let id = usize::from(sps.id);
                    self.sps_map[id] = Some(sps);
                }
                NalUnitType::Pps => {
                    let mut rbsp = std::mem::take(&mut self.rbsp);
                    unescape_rbsp(payload, &mut rbsp);
                    let pps = Pps::parse(&rbsp, |id| self.sps_map[usize::from(id)].as_ref());
                    self.rbsp = rbsp;
                    let pps = pps?;
                    let id = pps.id as usize;
                    if id < self.pps_map.len() {
                        self.pps_map[id] = Some(pps);
                    }
                }
                NalUnitType::Slice | NalUnitType::SliceIdr => {
                    self.slice(header, nal, payload, timestamp)?;
                }
                // SEI, AUD, filler and everything else carries nothing a
                // stateless decode submission needs.
                _ => {}
            }
        }
        // One access unit is one picture: close it here rather than waiting for
        // the next first_mb_in_slice == 0, so a caller gets its frame in the
        // call that fed the data.
        self.finish_picture()?;
        Ok(())
    }

    /// The next frame in display order.
    pub fn next_frame(&mut self) -> Option<Frame> {
        self.ready.pop()
    }

    /// Push every buffered picture out (end of stream).
    pub fn flush(&mut self) {
        let _ = self.finish_picture();
        self.bump(true);
    }

    /// Drop all picture state; parameter sets and the GPU session survive.
    pub fn reset(&mut self) {
        self.current = None;
        self.dpb.clear();
        self.ready.clear();
        self.gap_frames = 0;
    }

    /// What the stream turned out to be, once its first SPS was seen.
    pub fn stream_info(&self) -> Option<StreamInfo> {
        self.session.as_ref().map(Session::info)
    }

    /// Pictures inferred for `frame_num` gaps (8.2.5.2) since the last reset.
    ///
    /// Exposed because it is the difference between a stream that stays on the
    /// hardware and one that falls back to software: a gap is normal, and a
    /// caller watching this counter can tell "the encoder skipped pictures"
    /// from "the decoder is guessing".
    pub fn gap_frames_synthesized(&self) -> u64 {
        self.gap_frames
    }

    fn slice(
        &mut self,
        header: NalHeader,
        nal: &[u8],
        payload: &[u8],
        timestamp: i64,
    ) -> Result<()> {
        let mut rbsp = std::mem::take(&mut self.rbsp);
        unescape_rbsp(payload, &mut rbsp);
        let result = self.slice_rbsp(header, nal, &rbsp, timestamp);
        self.rbsp = rbsp;
        result
    }

    fn slice_rbsp(
        &mut self,
        header: NalHeader,
        nal: &[u8],
        rbsp: &[u8],
        timestamp: i64,
    ) -> Result<()> {
        // Two passes over the parameter sets: the slice header needs the PPS to
        // parse, and the PPS names its SPS.
        let pps_id = peek_pps_id(rbsp)?;
        let Some(pps) = self.pps_map.get(pps_id as usize).and_then(|p| p.clone()) else {
            // A slice referring to a parameter set we never received is what a
            // seek into the middle of a stream looks like; skip it rather than
            // failing the whole access unit.
            return Ok(());
        };
        let Some(sps) = self.sps_map[usize::from(pps.sps_id)].clone() else {
            return Ok(());
        };
        let sh = SliceHeader::parse(rbsp, header, &sps, &pps)?;

        if sh.first_mb_in_slice == 0 {
            self.finish_picture()?;
            self.start_picture(&sps, &sh, header, timestamp)?;
        }
        if self.current.is_none() {
            // Mid-picture entry after a seek: no first slice, nothing to add to.
            return Ok(());
        }
        self.render_slice(&sps, &pps, &sh, nal, header)
    }

    /// Create the GPU session if this stream has not had one yet.
    fn ensure_session(&mut self, sps: &Sps) -> Result<()> {
        let profile = profile_for(sps)?;
        let coded = (sps.coded_width, sps.coded_height);
        let bit_depth = sps.bit_depth_luma;
        if let Some(session) = &self.session
            && session.coded_size == coded
            && session.bit_depth == bit_depth
            && session.profile == profile
        {
            return Ok(());
        }
        // A resolution change starts a new session; the old one drops with its
        // surfaces once the frames still out there are released.
        self.current = None;
        self.dpb.clear();
        let surfaces = self.dpb.capacity() + EXTRA_SURFACES;
        self.session = Some(Session::new(
            &self.display,
            profile,
            coded,
            (sps.width, sps.height),
            bit_depth,
            surfaces,
        )?);
        Ok(())
    }

    fn start_picture(
        &mut self,
        sps: &Sps,
        sh: &SliceHeader,
        header: NalHeader,
        timestamp: i64,
    ) -> Result<()> {
        self.ensure_session(sps)?;
        let info = PicInfo {
            is_idr: header.is_idr(),
            is_reference: header.ref_idc != 0,
            frame_num: sh.frame_num,
            pic_order_cnt_lsb: sh.pic_order_cnt_lsb,
            delta_pic_order_cnt_bottom: sh.delta_pic_order_cnt_bottom,
            delta_pic_order_cnt: sh.delta_pic_order_cnt,
        };

        // 8.2.5.2: missing frame_num values are normal (a seek, a dropped
        // packet, an encoder that skipped a picture). Synthesising the frames
        // keeps the reference numbering aligned, which is the difference
        // between decoding the rest of the GOP and falling back to software.
        if !info.is_idr && self.dpb.frame_num_gap(sps, sh.frame_num) {
            self.gap_frames += self.dpb.fill_frame_num_gap(sps, sh.frame_num)? as u64;
        }

        let poc = self.dpb.picture_order_count(sps, &info)?;
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| Error::config("H.264 picture before any SPS"))?;
        let surface = session
            .pool
            .acquire()
            .ok_or_else(|| Error::config("H.264 decode: every surface is still in use"))?;
        let id = self.dpb.next_id();
        self.current = Some(Current {
            surface,
            id,
            sps_id: sps.id,
            info,
            poc,
            timestamp,
            marking: sh.dec_ref_pic_marking.clone(),
            buffers: Vec::new(),
            slices: 0,
        });
        Ok(())
    }

    fn render_slice(
        &mut self,
        sps: &Sps,
        pps: &Pps,
        sh: &SliceHeader,
        nal: &[u8],
        header: NalHeader,
    ) -> Result<()> {
        let (Some(current), Some(session)) = (self.current.as_mut(), self.session.as_ref()) else {
            return Ok(());
        };
        if current.sps_id != sps.id {
            return Err(Error::Stream(ec_core::Error::corrupt(
                "H.264 SPS changed mid-picture",
            )));
        }

        // The picture parameter buffer goes in once, with the first slice: it
        // describes the picture, and the DPB it names must not change under the
        // driver between slices.
        if current.slices == 0 {
            let pic_param = picture_parameters(sps, pps, sh, header, current, &self.dpb);
            current
                .buffers
                .push(param_buffer(&session.context, &pic_param)?);
            // The matrices are sent for every picture, flat or not: a driver
            // that never receives them dequantises with a zero matrix and
            // hands back a uniformly grey surface (radeonsi does exactly this).
            // A PPS scaling matrix overrides the SPS one (7.4.2.2).
            let iq = match pps.scaling_lists.as_ref().or(sps.scaling_lists.as_ref()) {
                Some(lists) => IQMatrixBufferH264 {
                    scaling_list_4x4: lists.list_4x4,
                    scaling_list_8x8: [lists.list_8x8[0], lists.list_8x8[1]],
                    ..IQMatrixBufferH264::default()
                },
                None => IQMatrixBufferH264::default(),
            };
            current.buffers.push(param_buffer(&session.context, &iq)?);
        }

        let max_pic_num = 1i32 << sps.log2_max_frame_num;
        let lists = if sh.slice_type.is_intra() {
            [RefList::default(), RefList::default()]
        } else {
            self.dpb.number_short_term(sh.frame_num, sps);
            self.dpb.build_ref_lists(
                sh.slice_type,
                current.poc.value,
                sh.frame_num as i32,
                max_pic_num,
                sh.num_ref_idx_l0_active as usize,
                sh.num_ref_idx_l1_active as usize,
                (&sh.ref_pic_list_mod_l0, &sh.ref_pic_list_mod_l1),
            )?
        };

        let slice_param = slice_parameters(sh, pps, nal, &lists, &self.dpb);
        current
            .buffers
            .push(param_buffer(&session.context, &slice_param)?);
        current.buffers.push(Buffer::from_bytes(
            &session.context,
            sys::VASliceDataBufferType,
            nal,
        )?);
        current.slices += 1;
        Ok(())
    }

    fn finish_picture(&mut self) -> Result<()> {
        let Some(current) = self.current.take() else {
            return Ok(());
        };
        let Some(session) = self.session.as_ref() else {
            return Ok(());
        };
        if current.slices == 0 {
            // No slice data reached the driver, so there is nothing to submit;
            // the surface returns to the pool with `current`.
            return Ok(());
        }
        let Some(sps) = self.sps_map[usize::from(current.sps_id)].clone() else {
            return Ok(());
        };

        session.submit(&current.surface, current.buffers)?;

        let picture = Picture {
            id: current.id,
            surface: Arc::clone(&current.surface),
            timestamp: current.timestamp,
            frame_num: current.info.frame_num,
            frame_num_wrap: current.info.frame_num as i32,
            pic_num: current.info.frame_num as i32,
            long_term_frame_idx: 0,
            poc: current.poc.value,
            poc_msb: current.poc.msb,
            poc_lsb: current.poc.lsb,
            frame_num_offset: current.poc.frame_num_offset,
            mark: Mark::Unused,
            non_existing: false,
            output: true,
        };
        self.dpb
            .store(picture, &sps, &current.info, current.marking.as_ref())?;
        self.bump(false);
        Ok(())
    }

    /// Move whatever the bumping process releases into the ready queue.
    fn bump(&mut self, flush: bool) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        while let Some(idx) = self.dpb.next_output(flush) {
            let pic = &self.dpb.frames[idx];
            let frame = Frame::new(
                Arc::clone(&pic.surface),
                Arc::clone(&session.images),
                pic.timestamp,
                session.display_size,
                session.coded_size,
                session.bit_depth,
            );
            self.ready.push(frame);
            self.dpb.released(idx);
        }
    }
}

/// The `pic_parameter_set_id` of a slice header, which is the second syntax
/// element and needs no parameter set to read.
fn peek_pps_id(rbsp: &[u8]) -> Result<u32> {
    let mut r = ec_core::BitReader::new(rbsp);
    r.read_ue()?; // first_mb_in_slice
    r.read_ue()?; // slice_type
    Ok(r.read_ue()?)
}

/// The VA profile for a stream, from its SPS.
fn profile_for(sps: &Sps) -> Result<Profile> {
    Ok(match sps.profile_idc {
        66 => {
            // Constrained baseline is a subset of main and every driver that
            // decodes main decodes it; plain baseline (FMO/ASO) is not, and
            // this crate does not implement slice groups either way.
            Profile::H264ConstrainedBaseline
        }
        77 | 88 => Profile::H264Main,
        100 | 110 | 122 | 244 if sps.bit_depth_luma > 8 => Profile::H264High10,
        100 => Profile::H264High,
        other => {
            return Err(Error::unsupported(
                format!("H.264 profile_idc {other}"),
                "no VA profile covers it",
            ));
        }
    })
}

/// `slice_type` as the driver wants it: the modulo-5 value of Table 7-6.
fn slice_type_code(slice_type: SliceType) -> u8 {
    match slice_type {
        SliceType::P => 0,
        SliceType::B => 1,
        SliceType::I => 2,
        SliceType::Sp => 3,
        SliceType::Si => 4,
    }
}

/// The VA reference entry for a stored picture.
fn reference_entry(pic: &Picture) -> VAPictureH264 {
    let flags = match pic.mark {
        Mark::Long => PICTURE_LONG_TERM_REFERENCE,
        _ => PICTURE_SHORT_TERM_REFERENCE,
    };
    let frame_idx = if pic.mark == Mark::Long {
        pic.long_term_frame_idx
    } else {
        pic.frame_num
    };
    VAPictureH264::frame(pic.surface.id(), frame_idx, pic.poc, flags)
}

fn picture_parameters(
    sps: &Sps,
    pps: &Pps,
    sh: &SliceHeader,
    header: NalHeader,
    current: &Current,
    dpb: &Dpb,
) -> PictureParameterBufferH264 {
    let mut p = PictureParameterBufferH264 {
        curr_pic: VAPictureH264::frame(
            current.surface.id(),
            current.info.frame_num,
            current.poc.value,
            if current.info.is_reference {
                PICTURE_SHORT_TERM_REFERENCE
            } else {
                0
            },
        ),
        picture_width_in_mbs_minus1: (sps.mb_width.max(1) - 1) as u16,
        picture_height_in_mbs_minus1: (sps.mb_height.max(1) - 1) as u16,
        bit_depth_luma_minus8: sps.bit_depth_luma.saturating_sub(8),
        bit_depth_chroma_minus8: sps.bit_depth_chroma.saturating_sub(8),
        num_ref_frames: sps.max_num_ref_frames.min(255) as u8,
        pic_init_qp_minus26: (pps.pic_init_qp - 26) as i8,
        pic_init_qs_minus26: (pps.pic_init_qs - 26) as i8,
        chroma_qp_index_offset: pps.chroma_qp_index_offset as i8,
        second_chroma_qp_index_offset: pps.second_chroma_qp_index_offset as i8,
        frame_num: sh.frame_num as u16,
        ..PictureParameterBufferH264::default()
    };

    for (n, pic) in dpb
        .frames
        .iter()
        .filter(|p| p.mark != Mark::Unused)
        .take(p.reference_frames.len())
        .enumerate()
    {
        p.reference_frames[n] = reference_entry(pic);
    }

    p.seq_fields = p
        .seq_fields
        .chroma_format_idc(u32::from(sps.chroma_format_idc))
        .residual_colour_transform_flag(u32::from(sps.separate_colour_plane))
        .gaps_in_frame_num_value_allowed_flag(u32::from(sps.gaps_in_frame_num_allowed))
        .frame_mbs_only_flag(u32::from(sps.frame_mbs_only))
        .mb_adaptive_frame_field_flag(u32::from(sps.mb_adaptive_frame_field))
        .direct_8x8_inference_flag(u32::from(sps.direct_8x8_inference))
        // A.3.3.2: 8x8 bi-prediction is restricted from level 3.1 up.
        .min_luma_bipred_size8x8(u32::from(sps.level_idc >= 31))
        .log2_max_frame_num_minus4(u32::from(sps.log2_max_frame_num.saturating_sub(4)))
        .pic_order_cnt_type(u32::from(sps.pic_order_cnt_type))
        .log2_max_pic_order_cnt_lsb_minus4(u32::from(
            sps.log2_max_pic_order_cnt_lsb.saturating_sub(4),
        ))
        .delta_pic_order_always_zero_flag(u32::from(sps.delta_pic_order_always_zero));

    p.pic_fields = p
        .pic_fields
        .entropy_coding_mode_flag(u32::from(pps.entropy_coding_mode))
        .weighted_pred_flag(u32::from(pps.weighted_pred))
        .weighted_bipred_idc(u32::from(pps.weighted_bipred_idc))
        .transform_8x8_mode_flag(u32::from(pps.transform_8x8_mode))
        .field_pic_flag(u32::from(sh.field_pic))
        .constrained_intra_pred_flag(u32::from(pps.constrained_intra_pred))
        .pic_order_present_flag(u32::from(pps.bottom_field_pic_order_in_frame_present))
        .deblocking_filter_control_present_flag(u32::from(pps.deblocking_filter_control_present))
        .redundant_pic_cnt_present_flag(u32::from(pps.redundant_pic_cnt_present))
        .reference_pic_flag(u32::from(header.ref_idc != 0));
    p
}

fn slice_parameters(
    sh: &SliceHeader,
    pps: &Pps,
    nal: &[u8],
    lists: &[RefList; 2],
    dpb: &Dpb,
) -> SliceParameterBufferH264 {
    let mut s = SliceParameterBufferH264 {
        slice_data_size: nal.len() as u32,
        slice_data_offset: 0,
        slice_data_flag: 0, // VA_SLICE_DATA_FLAG_ALL
        // "relative to and includes the NAL unit byte", counted in the
        // unescaped RBSP: the syntax crate's header_bits starts *after* that
        // byte, hence the 8.
        slice_data_bit_offset: (sh.header_bits + 8) as u16,
        first_mb_in_slice: sh.first_mb_in_slice as u16,
        slice_type: slice_type_code(sh.slice_type),
        direct_spatial_mv_pred_flag: u8::from(sh.direct_spatial_mv_pred),
        num_ref_idx_l0_active_minus1: sh.num_ref_idx_l0_active.saturating_sub(1) as u8,
        num_ref_idx_l1_active_minus1: sh.num_ref_idx_l1_active.saturating_sub(1) as u8,
        cabac_init_idc: sh.cabac_init_idc as u8,
        // The syntax crate resolves SliceQPY; the driver wants the delta back.
        slice_qp_delta: (sh.slice_qp - pps.pic_init_qp) as i8,
        disable_deblocking_filter_idc: sh.deblock.disable_idc,
        slice_alpha_c0_offset_div2: (sh.deblock.alpha_c0_offset / 2) as i8,
        slice_beta_offset_div2: (sh.deblock.beta_offset / 2) as i8,
        ..SliceParameterBufferH264::default()
    };

    for (x, list) in lists.iter().enumerate() {
        let out = if x == 0 {
            &mut s.ref_pic_list0
        } else {
            &mut s.ref_pic_list1
        };
        for i in 0..list.len().min(out.len()) {
            if let Some(idx) = list.get(i)
                && let Some(pic) = dpb.frames.get(idx)
            {
                out[i] = reference_entry(pic);
            }
        }
    }

    if let Some(w) = sh.pred_weight_table.as_ref() {
        write_weights(&mut s, w);
    }
    s
}

/// Copy a parsed `pred_weight_table()` into the slice parameter buffer.
///
/// The spec's default weights are `(1 << denom, 0)`, and a driver reads every
/// entry the active count names — so an absent entry has to be *written* as the
/// default rather than left at zero, which would black out the prediction.
fn write_weights(s: &mut SliceParameterBufferH264, w: &PredWeightTable) {
    s.luma_log2_weight_denom = w.luma_log2_weight_denom as u8;
    s.chroma_log2_weight_denom = w.chroma_log2_weight_denom as u8;
    let luma_default = 1i16 << w.luma_log2_weight_denom;
    let chroma_default = 1i16 << w.chroma_log2_weight_denom;

    for (list, entries) in [(0usize, &w.l0), (1usize, &w.l1)] {
        let (weights, offsets, cweights, coffsets, luma_flag, chroma_flag) = if list == 0 {
            (
                &mut s.luma_weight_l0,
                &mut s.luma_offset_l0,
                &mut s.chroma_weight_l0,
                &mut s.chroma_offset_l0,
                &mut s.luma_weight_l0_flag,
                &mut s.chroma_weight_l0_flag,
            )
        } else {
            (
                &mut s.luma_weight_l1,
                &mut s.luma_offset_l1,
                &mut s.chroma_weight_l1,
                &mut s.chroma_offset_l1,
                &mut s.luma_weight_l1_flag,
                &mut s.chroma_weight_l1_flag,
            )
        };
        for (i, entry) in entries.iter().take(32).enumerate() {
            match entry.luma {
                Some((weight, offset)) => {
                    *luma_flag = 1;
                    weights[i] = weight as i16;
                    offsets[i] = offset as i16;
                }
                None => {
                    weights[i] = luma_default;
                    offsets[i] = 0;
                }
            }
            match entry.chroma {
                Some(pair) => {
                    *chroma_flag = 1;
                    for c in 0..2 {
                        cweights[i][c] = pair[c].0 as i16;
                        coffsets[i][c] = pair[c].1 as i16;
                    }
                }
                None => {
                    for c in 0..2 {
                        cweights[i][c] = chroma_default;
                        coffsets[i][c] = 0;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_sps() -> Sps {
        Sps {
            profile_idc: 100,
            constraint_flags: 0,
            level_idc: 40,
            id: 0,
            chroma_format_idc: 1,
            separate_colour_plane: false,
            bit_depth_luma: 8,
            bit_depth_chroma: 8,
            transform_bypass: false,
            scaling_lists: None,
            log2_max_frame_num: 4,
            pic_order_cnt_type: 0,
            log2_max_pic_order_cnt_lsb: 4,
            delta_pic_order_always_zero: false,
            offset_for_non_ref_pic: 0,
            offset_for_top_to_bottom_field: 0,
            offsets_for_ref_frame: Vec::new(),
            max_num_ref_frames: 4,
            gaps_in_frame_num_allowed: true,
            frame_mbs_only: true,
            mb_adaptive_frame_field: false,
            direct_8x8_inference: true,
            crop: (0, 0, 0, 0),
            vui: None,
            mb_width: 120,
            mb_height: 68,
            coded_width: 1920,
            coded_height: 1088,
            width: 1920,
            height: 1080,
        }
    }

    #[test]
    fn slice_type_codes_match_table_7_6() {
        assert_eq!(slice_type_code(SliceType::P), 0);
        assert_eq!(slice_type_code(SliceType::B), 1);
        assert_eq!(slice_type_code(SliceType::I), 2);
        assert_eq!(slice_type_code(SliceType::Sp), 3);
        assert_eq!(slice_type_code(SliceType::Si), 4);
    }

    #[test]
    fn profiles_follow_the_stream_bit_depth() {
        let mut sps = test_sps();
        sps.profile_idc = 100;
        assert_eq!(profile_for(&sps).unwrap(), Profile::H264High);
        sps.bit_depth_luma = 10;
        assert_eq!(profile_for(&sps).unwrap(), Profile::H264High10);
        sps.profile_idc = 66;
        sps.bit_depth_luma = 8;
        assert_eq!(profile_for(&sps).unwrap(), Profile::H264ConstrainedBaseline);
        sps.profile_idc = 244;
        assert!(profile_for(&sps).is_err());
    }
}

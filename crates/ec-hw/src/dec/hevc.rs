//! Stateless HEVC decoding.
//!
//! HEVC keeps no `frame_num` and no sliding window: every picture states its
//! whole reference picture set, and anything the set does not name stops being
//! a reference there and then (8.3.2). That makes the DPB smaller than H.264's
//! but the *derivation* larger, and all of it has to happen here — a stateless
//! driver is handed `RefPicSetStCurrBefore`, `StCurrAfter` and `LtCurr` as
//! finished picture lists.

use std::sync::Arc;

use ec_core::color::{ColorDescription, ContentLight, Tags};
use ec_h265_syntax::{
    NalUnitType, ParsePositions, Pps, SliceHeader, SliceType, Sps, Vps, split_annex_b,
    unescape_rbsp,
};
use ec_va::caps::Profile;
use ec_va::{Buffer, Display, sys};

use super::{ReadyFrames, Session, StreamInfo};
use crate::error::{Error, Result};
use crate::frame::{Colour, Frame};
use crate::params::hevc::{
    IQMatrixBufferHEVC, PICTURE_INVALID, PICTURE_LONG_TERM_REFERENCE, PICTURE_RPS_LT_CURR,
    PICTURE_RPS_ST_CURR_AFTER, PICTURE_RPS_ST_CURR_BEFORE, PictureParameterBufferHEVC,
    SliceParameterBufferHEVC, VAPictureHEVC,
};
use crate::params::param_buffer;
use crate::pool::PooledSurface;

/// Beyond `sps_max_dec_pic_buffering`: the picture being decoded plus the ones
/// a caller is still holding.
const EXTRA_SURFACES: usize = 6;

/// Reference marking of a stored picture (8.3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    Unused,
    ShortTerm,
    LongTerm,
}

struct StoredPicture {
    surface: Arc<PooledSurface>,
    poc: i32,
    timestamp: i64,
    mark: Mark,
    /// Still to be handed to the caller.
    output: bool,
}

impl StoredPicture {
    fn stored(&self) -> bool {
        self.output || self.mark != Mark::Unused
    }
}

/// The reference picture set of the picture being decoded, as DPB indices.
#[derive(Debug, Default, Clone)]
struct Rps {
    st_curr_before: Vec<usize>,
    st_curr_after: Vec<usize>,
    lt_curr: Vec<usize>,
}

/// The picture being assembled from its slices.
struct Current {
    surface: Arc<PooledSurface>,
    sps_id: u32,
    poc: i32,
    timestamp: i64,
    is_reference: bool,
    /// True when this picture may be `prevTid0Pic` for the next one (8.3.1).
    is_tid0_anchor: bool,
    output: bool,
    rps: Rps,
    buffers: Vec<Buffer>,
    slices: usize,
    /// Index into `PictureParameterBufferHEVC::reference_frames` per DPB index.
    va_index: Vec<u8>,
}

/// A stateless HEVC decoder.
pub struct HevcDecoder {
    display: Arc<Display>,
    vps_map: Vec<Option<Vps>>,
    sps_map: Vec<Option<Sps>>,
    pps_map: Vec<Option<Pps>>,
    session: Option<Session>,
    dpb: Vec<StoredPicture>,
    ready: ReadyFrames,
    current: Option<Current>,
    /// `prevTid0Pic`'s picture order count, for the MSB derivation of 8.3.1.
    prev_tid0_poc: i32,
    /// Set once a picture has been decoded since the last reset.
    started: bool,
    /// `NoRaslOutputFlag` (8.1.3) of the most recent IRAP: RASL pictures
    /// associated with it must not be decoded or output when set, because
    /// they predict from pictures before a random-access point this decoder
    /// never saw.
    no_rasl_output: bool,
    max_reorder: usize,
    max_dec_pic_buffering: usize,
    /// The most recent SPS's VUI colour tags, sticky across pictures.
    colour_tags: Tags,
    /// The prefix SEI HDR peak seen so far, merged so a later access unit that
    /// says nothing does not erase what an earlier IRAP declared.
    colour_light: ContentLight,
}

impl HevcDecoder {
    /// A decoder with no stream state.
    pub fn new(display: &Arc<Display>) -> HevcDecoder {
        HevcDecoder {
            display: Arc::clone(display),
            vps_map: (0..16).map(|_| None).collect(),
            sps_map: (0..16).map(|_| None).collect(),
            pps_map: (0..64).map(|_| None).collect(),
            session: None,
            dpb: Vec::new(),
            ready: ReadyFrames::default(),
            current: None,
            prev_tid0_poc: 0,
            started: false,
            no_rasl_output: false,
            max_reorder: 1,
            max_dec_pic_buffering: 6,
            colour_tags: Tags::default(),
            colour_light: ContentLight::default(),
        }
    }

    /// Decode one Annex B access unit.
    pub fn decode(&mut self, data: &[u8], timestamp: i64) -> Result<()> {
        for nal in split_annex_b(data) {
            match nal.header.nal_type {
                NalUnitType::Vps => {
                    let vps = Vps::parse(&nal.rbsp())?;
                    let id = usize::from(vps.id);
                    if id < self.vps_map.len() {
                        self.vps_map[id] = Some(vps);
                    }
                }
                NalUnitType::Sps => {
                    let sps = Sps::parse(&nal.rbsp())?;
                    self.max_reorder = sps.max_num_reorder_pics as usize;
                    self.max_dec_pic_buffering = sps.max_dec_pic_buffering_minus1 as usize + 1;
                    self.colour_tags = vui_colour_tags(&sps);
                    let id = sps.id as usize;
                    if id < self.sps_map.len() {
                        self.sps_map[id] = Some(sps);
                    }
                }
                NalUnitType::Pps => {
                    let pps = Pps::parse(&nal.rbsp())?;
                    let id = pps.id as usize;
                    if id < self.pps_map.len() {
                        self.pps_map[id] = Some(pps);
                    }
                }
                t if t.is_vcl() => {
                    self.slice(t, nal.header.temporal_id, nal.payload, timestamp)?;
                }
                _ => {}
            }
        }
        // The HDR SEI messages (mastering display, content light level) live
        // outside the slice loop above and are read on the whole access unit:
        // an encoder writes them once, ahead of the IRAP they describe, and
        // they otherwise say nothing (`ContentLight::default()`), so a later
        // access unit that carries neither never overwrites what an earlier
        // one declared.
        let light = ec_core::color::hevc_sei_light(data);
        if light != ContentLight::default() {
            self.colour_light = light.over(self.colour_light);
        }
        self.finish_picture()?;
        Ok(())
    }

    /// This stream's colour metadata, once its first SPS has been seen.
    pub fn colour(&self) -> Option<Colour> {
        let session = self.session.as_ref()?;
        Some(Colour {
            description: ColorDescription::resolve(
                Tags::default(),
                self.colour_tags,
                session.display_size.1,
            ),
            light: self.colour_light,
        })
    }

    /// The next frame in output order.
    pub fn next_frame(&mut self) -> Option<Frame> {
        self.ready.pop()
    }

    /// Push every buffered picture out (end of stream).
    pub fn flush(&mut self) {
        let _ = self.finish_picture();
        self.bump(true);
    }

    /// Drop all picture state after a seek; parameter sets survive.
    pub fn reset(&mut self) {
        self.current = None;
        self.dpb.clear();
        self.ready.clear();
        self.prev_tid0_poc = 0;
        self.started = false;
        self.no_rasl_output = false;
    }

    /// What the stream turned out to be.
    pub fn stream_info(&self) -> Option<StreamInfo> {
        self.session.as_ref().map(Session::info)
    }

    fn slice(
        &mut self,
        nal_type: NalUnitType,
        temporal_id: u8,
        payload: &[u8],
        timestamp: i64,
    ) -> Result<()> {
        let rbsp = unescape_rbsp(payload);
        // The header cannot be parsed without its PPS, and the PPS names the SPS.
        let pps_id = peek_pps_id(&rbsp, nal_type)?;
        let Some(pps) = self.pps_map.get(pps_id as usize).and_then(|p| p.clone()) else {
            return Ok(());
        };
        let Some(sps) = self
            .sps_map
            .get(pps.sps_id as usize)
            .and_then(|s| s.clone())
        else {
            return Ok(());
        };
        let (sh, pos) = SliceHeader::parse(&rbsp, &sps, &pps, nal_type)?;

        if sh.first_slice_segment_in_pic {
            self.finish_picture()?;
            // 8.1.3 / C.5.2.2: a RASL picture associated with an IRAP whose
            // NoRaslOutputFlag is 1 predicts from pictures before that IRAP,
            // which a mid-stream start never decoded. It is not output and
            // is not guaranteed decodable, so it is dropped whole rather than
            // handed to the driver.
            if !(is_rasl_picture(nal_type) && self.no_rasl_output) {
                self.start_picture(&sps, &sh, nal_type, temporal_id, timestamp)?;
            }
        }
        if self.current.is_none() {
            return Ok(());
        }
        self.render_slice(&sps, &pps, &sh, &pos, payload, nal_type)
    }

    fn ensure_session(&mut self, sps: &Sps) -> Result<()> {
        let bit_depth = (sps.bit_depth_luma_minus8 + 8) as u8;
        let profile = match (sps.chroma_format_idc, bit_depth) {
            (1, 8) => Profile::HEVCMain,
            (1, 10) => Profile::HEVCMain10,
            (chroma, depth) => {
                return Err(Error::unsupported(
                    format!("HEVC chroma_format_idc {chroma} at {depth} bits"),
                    "only Main and Main10 (4:2:0, 8 or 10 bit) are decoded in hardware",
                ));
            }
        };
        let coded = (
            sps.pic_width_in_luma_samples,
            sps.pic_height_in_luma_samples,
        );
        if let Some(session) = &self.session
            && session.coded_size == coded
            && session.bit_depth == bit_depth
            && session.profile == profile
        {
            return Ok(());
        }
        self.current = None;
        self.dpb.clear();
        self.session = Some(Session::new(
            &self.display,
            profile,
            coded,
            sps.display_size(),
            bit_depth,
            self.max_dec_pic_buffering + EXTRA_SURFACES,
        )?);
        Ok(())
    }

    fn start_picture(
        &mut self,
        sps: &Sps,
        sh: &SliceHeader,
        nal_type: NalUnitType,
        temporal_id: u8,
        timestamp: i64,
    ) -> Result<()> {
        self.ensure_session(sps)?;

        if nal_type.is_irap() {
            // 8.1.3: 1 for an IDR, a BLA, or the first picture of a coded
            // video sequence — which a mid-stream start's first IRAP always
            // is, `self.started` still being false at that point.
            self.no_rasl_output = nal_type.is_idr() || is_bla_picture(nal_type) || !self.started;
        }

        let poc = self.picture_order_count(sps, sh, nal_type);
        // 8.3.2 runs before the picture is decoded and after its POC is known:
        // every stored picture the set does not name stops being a reference.
        let rps = self.apply_rps(sps, sh, nal_type, poc);

        if nal_type.is_irap() && (nal_type.is_idr() || sh.no_output_of_prior_pics) {
            // An IRAP that restarts output order: everything ahead of it in the
            // buffer belongs ahead of it on screen too.
            self.bump(true);
        }

        if std::env::var_os("EC_HW_DEBUG").is_some() {
            eprintln!(
                "pic poc {poc} nal {:?} tid {temporal_id} lsb {} dpb {} ready {} (st {}/{} lt {}) free {}",
                nal_type,
                sh.poc_lsb,
                self.dpb.len(),
                self.ready.len(),
                rps.st_curr_before.len(),
                rps.st_curr_after.len(),
                rps.lt_curr.len(),
                self.session
                    .as_ref()
                    .map(|s| s.pool.available())
                    .unwrap_or(0),
            );
        }
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| Error::config("HEVC picture before any SPS"))?;
        let surface = session
            .pool
            .acquire()
            .ok_or_else(|| Error::config("HEVC decode: every surface is still in use"))?;

        // The driver indexes RefPicList into `reference_frames`, so the mapping
        // from DPB slot to that array is fixed once, here.
        let mut va_index = vec![0xffu8; self.dpb.len()];
        let mut n = 0u8;
        for (i, pic) in self.dpb.iter().enumerate() {
            if pic.mark != Mark::Unused && n < 15 {
                va_index[i] = n;
                n += 1;
            }
        }

        self.current = Some(Current {
            surface,
            sps_id: sps.id,
            poc,
            timestamp,
            // A "_N" picture (an even NAL type below 16) is a sub-layer
            // non-reference: nothing in its own sub-layer may predict from it,
            // and a decoder that stores it as a reference lets the next
            // picture's reference picture set match the wrong picture by POC.
            is_reference: is_reference_picture(nal_type),
            // 8.3.1: `prevTid0Pic` is the previous picture with TemporalId 0
            // that is neither RASL, RADL nor sub-layer non-reference. Taking
            // the previous picture instead — which a stream with temporal
            // sub-layers immediately punishes — puts the picture order count
            // MSB one wrap out and hands the driver the wrong references.
            is_tid0_anchor: temporal_id == 0
                && is_reference_picture(nal_type)
                && !is_leading_picture(nal_type),
            output: sh.pic_output_flag,
            rps,
            buffers: Vec::new(),
            slices: 0,
            va_index,
        });
        Ok(())
    }

    /// `PicOrderCntVal` (8.3.1).
    fn picture_order_count(&self, sps: &Sps, sh: &SliceHeader, nal_type: NalUnitType) -> i32 {
        if nal_type.is_idr() {
            return 0;
        }
        let max_lsb = 1i32 << (sps.log2_max_poc_lsb_minus4 + 4);
        let lsb = sh.poc_lsb as i32;
        let (prev_msb, prev_lsb) = if !self.started {
            (0, 0)
        } else {
            (
                self.prev_tid0_poc - (self.prev_tid0_poc.rem_euclid(max_lsb)),
                self.prev_tid0_poc.rem_euclid(max_lsb),
            )
        };
        // An IRAP that starts a new coded video sequence has no predecessor.
        if nal_type.is_irap() && !self.started {
            return lsb;
        }
        let msb = if lsb < prev_lsb && prev_lsb - lsb >= max_lsb / 2 {
            prev_msb + max_lsb
        } else if lsb > prev_lsb && lsb - prev_lsb > max_lsb / 2 {
            prev_msb - max_lsb
        } else {
            prev_msb
        };
        msb + lsb
    }

    /// Reference picture set derivation and marking (8.3.2).
    fn apply_rps(&mut self, sps: &Sps, sh: &SliceHeader, nal_type: NalUnitType, poc: i32) -> Rps {
        if nal_type.is_irap() && (nal_type.is_idr() || !self.started) {
            for pic in &mut self.dpb {
                pic.mark = Mark::Unused;
            }
            self.dpb.retain(|p| p.stored());
            return Rps::default();
        }

        let set = &sh.short_term_ref_pic_set;
        let max_lsb = 1i32 << (sps.log2_max_poc_lsb_minus4 + 4);
        let mut wanted: Vec<(i32, bool, bool)> = Vec::new(); // (poc, used, long term)
        for i in 0..set.num_negative as usize {
            wanted.push((poc + set.delta_poc_s0[i], set.used_s0[i], false));
        }
        for i in 0..set.num_positive as usize {
            wanted.push((poc + set.delta_poc_s1[i], set.used_s1[i], false));
        }
        for lt in &sh.long_term {
            // 8.3.2: a long-term entry without an MSB cycle is matched on its
            // POC low bits alone, which is how a stream survives an MSB wrap.
            let target = if lt.delta_poc_msb_present {
                poc - (lt.delta_poc_msb_cycle as i32) * max_lsb - poc.rem_euclid(max_lsb)
                    + lt.poc_lsb_lt as i32
            } else {
                lt.poc_lsb_lt as i32
            };
            wanted.push((target, lt.used_by_curr, !lt.delta_poc_msb_present));
        }

        let mut rps = Rps::default();
        let mut keep = vec![false; self.dpb.len()];
        for (target, used, lsb_only) in wanted {
            let found = self.dpb.iter().position(|p| {
                if lsb_only {
                    p.poc.rem_euclid(max_lsb) == target.rem_euclid(max_lsb)
                } else {
                    p.poc == target
                }
            });
            let Some(idx) = found else { continue };
            keep[idx] = true;
            if !used {
                continue;
            }
            // The three "current" lists are what the reference lists are built
            // from, in POC order relative to the current picture.
            if self.dpb[idx].mark == Mark::LongTerm || lsb_only {
                rps.lt_curr.push(idx);
            } else if self.dpb[idx].poc < poc {
                rps.st_curr_before.push(idx);
            } else {
                rps.st_curr_after.push(idx);
            }
        }
        rps.st_curr_before
            .sort_by_key(|&i| std::cmp::Reverse(self.dpb[i].poc));
        rps.st_curr_after.sort_by_key(|&i| self.dpb[i].poc);

        for (i, pic) in self.dpb.iter_mut().enumerate() {
            if !keep[i] {
                pic.mark = Mark::Unused;
            }
        }
        // Dropping unreferenced, already-output pictures here is what returns
        // their surfaces to the pool; the indices in `rps` are recomputed after.
        let mut remap = vec![usize::MAX; self.dpb.len()];
        let mut next = 0usize;
        for (i, pic) in self.dpb.iter().enumerate() {
            if pic.stored() {
                remap[i] = next;
                next += 1;
            }
        }
        self.dpb.retain(|p| p.stored());
        for list in [
            &mut rps.st_curr_before,
            &mut rps.st_curr_after,
            &mut rps.lt_curr,
        ] {
            list.retain(|&i| remap[i] != usize::MAX);
            for entry in list.iter_mut() {
                *entry = remap[*entry];
            }
        }
        rps
    }

    fn render_slice(
        &mut self,
        sps: &Sps,
        pps: &Pps,
        sh: &SliceHeader,
        pos: &ParsePositions,
        payload: &[u8],
        nal_type: NalUnitType,
    ) -> Result<()> {
        let (Some(current), Some(session)) = (self.current.as_mut(), self.session.as_ref()) else {
            return Ok(());
        };
        if current.sps_id != sps.id {
            return Err(Error::Stream(ec_core::Error::corrupt(
                "HEVC SPS changed mid-picture",
            )));
        }

        if current.slices == 0 {
            let pic_param = picture_parameters(sps, pps, sh, pos, nal_type, current, &self.dpb);
            current
                .buffers
                .push(param_buffer(&session.context, &pic_param)?);
            if sps.scaling_list_enabled {
                // The syntax crate refuses a stream that codes its own lists, so
                // "enabled" here always means the flat default matrices.
                current.buffers.push(param_buffer(
                    &session.context,
                    &IQMatrixBufferHEVC::default(),
                )?);
            }
        }

        // The NAL unit header is part of the slice data the driver is handed,
        // and every offset it is given is measured from there.
        let nal_bytes = payload.len() + 2;
        let mut slice_param = SliceParameterBufferHEVC {
            slice_data_size: nal_bytes as u32,
            slice_data_offset: 0,
            slice_data_flag: 0, // VA_SLICE_DATA_FLAG_ALL
            slice_data_byte_offset: pos.slice_data_byte_offset(payload) as u32,
            slice_segment_address: sh.segment_address,
            collocated_ref_idx: if sh.temporal_mvp_enabled {
                sh.collocated_ref_idx as u8
            } else {
                0xff
            },
            num_ref_idx_l0_active_minus1: sh.num_ref_idx_l0_active_minus1 as u8,
            num_ref_idx_l1_active_minus1: sh.num_ref_idx_l1_active_minus1 as u8,
            slice_qp_delta: sh.qp_delta as i8,
            slice_cb_qp_offset: sh.cb_qp_offset as i8,
            slice_cr_qp_offset: sh.cr_qp_offset as i8,
            slice_beta_offset_div2: sh.beta_offset_div2 as i8,
            slice_tc_offset_div2: sh.tc_offset_div2 as i8,
            five_minus_max_num_merge_cand: sh.five_minus_max_num_merge_cand as u8,
            num_entry_point_offsets: sh.entry_point_offsets.len().min(u16::MAX as usize) as u16,
            slice_data_num_emu_prevn_bytes: ec_h265_syntax::count_emulation_prevention_bytes(
                payload,
            )
            .min(u16::MAX as usize) as u16,
            ..SliceParameterBufferHEVC::default()
        };
        slice_param.long_slice_flags = slice_param
            .long_slice_flags
            .last_slice_of_pic(0)
            .dependent_slice_segment_flag(u32::from(sh.dependent_slice_segment))
            .slice_type(sh.slice_type.code())
            .color_plane_id(u32::from(sh.colour_plane_id))
            .slice_sao_luma_flag(u32::from(sh.sao_luma))
            .slice_sao_chroma_flag(u32::from(sh.sao_chroma))
            .mvd_l1_zero_flag(u32::from(sh.mvd_l1_zero))
            .cabac_init_flag(u32::from(sh.cabac_init))
            .slice_temporal_mvp_enabled_flag(u32::from(sh.temporal_mvp_enabled))
            .slice_deblocking_filter_disabled_flag(u32::from(sh.deblocking_filter_disabled))
            .collocated_from_l0_flag(u32::from(sh.collocated_from_l0))
            .slice_loop_filter_across_slices_enabled_flag(u32::from(
                sh.loop_filter_across_slices_enabled,
            ));

        if sh.slice_type != SliceType::I {
            let lists = reference_lists(sh, &current.rps);
            for (x, list) in lists.iter().enumerate() {
                for (i, &dpb_idx) in list.iter().take(15).enumerate() {
                    slice_param.ref_pic_list[x][i] =
                        current.va_index.get(dpb_idx).copied().unwrap_or(0xff);
                }
            }
        }
        write_weights(&mut slice_param, sh);

        current
            .buffers
            .push(param_buffer(&session.context, &slice_param)?);
        // Rebuild the NAL unit: `split_annex_b` hands over the payload without
        // its two header bytes, and the driver counts from them.
        let mut nal = Vec::with_capacity(nal_bytes);
        nal.extend_from_slice(&ec_h265_syntax::NalHeader::new(nal_type).to_bytes());
        nal.extend_from_slice(payload);
        current.buffers.push(Buffer::from_bytes(
            &session.context,
            sys::VASliceDataBufferType,
            &nal,
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
            return Ok(());
        }
        session.submit(&current.surface, current.buffers)?;

        self.dpb.push(StoredPicture {
            surface: Arc::clone(&current.surface),
            poc: current.poc,
            timestamp: current.timestamp,
            mark: if current.is_reference {
                Mark::ShortTerm
            } else {
                Mark::Unused
            },
            output: current.output,
        });
        if current.is_tid0_anchor {
            self.prev_tid0_poc = current.poc;
        }
        self.started = true;
        self.bump(false);
        Ok(())
    }

    /// The bumping process (C.5.2.4): output while the buffer holds more than
    /// its reordering or storage limit allows.
    fn bump(&mut self, flush: bool) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        loop {
            let pending = self.dpb.iter().filter(|p| p.output).count();
            let stored = self.dpb.iter().filter(|p| p.stored()).count();
            if !flush && pending <= self.max_reorder && stored <= self.max_dec_pic_buffering {
                return;
            }
            let Some(idx) = self
                .dpb
                .iter()
                .enumerate()
                .filter(|(_, p)| p.output)
                .min_by_key(|(_, p)| p.poc)
                .map(|(i, _)| i)
            else {
                return;
            };
            let pic = &self.dpb[idx];
            self.ready.push(Frame::new(
                Arc::clone(&pic.surface),
                Arc::clone(&session.images),
                pic.timestamp,
                session.display_size,
                session.coded_size,
                session.bit_depth,
                self.colour(),
            ));
            self.dpb[idx].output = false;
            self.dpb.retain(|p| p.stored());
        }
    }
}

/// True unless the NAL type says sub-layer non-reference (`_N`, an even type
/// below 16).
fn is_reference_picture(nal_type: NalUnitType) -> bool {
    let code = nal_type.code();
    code >= 16 || code % 2 == 1
}

/// RADL (6, 7) and RASL (8, 9): the leading pictures of an IRAP, which 8.3.1
/// excludes from the picture order count prediction.
fn is_leading_picture(nal_type: NalUnitType) -> bool {
    (6..=9).contains(&nal_type.code())
}

/// An SPS's VUI `colour_description` and range flag, as [`Tags`] for
/// [`ColorDescription::resolve`]. [`Tags::default()`] when the VUI has no
/// `video_signal_type` at all.
fn vui_colour_tags(sps: &Sps) -> Tags {
    let Some(vst) = sps.vui.as_ref().and_then(|vui| vui.video_signal_type) else {
        return Tags::default();
    };
    let (_primaries, transfer, matrix) = vst
        .colour_description
        .map(|cd| {
            (
                cd.colour_primaries,
                cd.transfer_characteristics,
                cd.matrix_coeffs,
            )
        })
        .unwrap_or((0, 0, 0));
    Tags::from_codes(
        u64::from(matrix),
        u64::from(transfer),
        if vst.video_full_range_flag { 2 } else { 1 },
    )
}

/// RASL_N and RASL_R (8, 9): leading pictures dropped whole when their IRAP's
/// `NoRaslOutputFlag` is 1 (8.1.3).
fn is_rasl_picture(nal_type: NalUnitType) -> bool {
    matches!(nal_type.code(), 8 | 9)
}

/// BLA_W_LP, BLA_W_RADL and BLA_N_LP (16..=18): always `NoRaslOutputFlag` 1
/// (8.1.3), a broken-link splice point that never has a decodable RASL.
fn is_bla_picture(nal_type: NalUnitType) -> bool {
    (16..=18).contains(&nal_type.code())
}

/// `slice_pic_parameter_set_id`, readable without any parameter set.
fn peek_pps_id(rbsp: &[u8], nal_type: NalUnitType) -> Result<u32> {
    let mut r = ec_core::BitReader::new(rbsp);
    r.read_bit()?; // first_slice_segment_in_pic_flag
    if nal_type.is_irap() {
        r.read_bit()?; // no_output_of_prior_pics_flag
    }
    Ok(r.read_ue()?)
}

/// Reference list construction (8.3.4), as DPB indices.
fn reference_lists(sh: &SliceHeader, rps: &Rps) -> [Vec<usize>; 2] {
    let mut out = [Vec::new(), Vec::new()];
    let counts = [
        sh.num_ref_idx_l0_active_minus1 as usize + 1,
        sh.num_ref_idx_l1_active_minus1 as usize + 1,
    ];
    for x in 0..2 {
        if x == 1 && sh.slice_type != SliceType::B {
            continue;
        }
        // RefPicListTemp0 is StCurrBefore, StCurrAfter, LtCurr; list 1 swaps the
        // first two, which is the whole difference between the two lists.
        let mut temp: Vec<usize> = Vec::new();
        while temp.len() < counts[x] {
            let before = if x == 0 {
                &rps.st_curr_before
            } else {
                &rps.st_curr_after
            };
            let after = if x == 0 {
                &rps.st_curr_after
            } else {
                &rps.st_curr_before
            };
            let round = before.len() + after.len() + rps.lt_curr.len();
            if round == 0 {
                break;
            }
            temp.extend(before.iter().copied());
            temp.extend(after.iter().copied());
            temp.extend(rps.lt_curr.iter().copied());
        }
        if temp.is_empty() {
            continue;
        }
        for i in 0..counts[x] {
            let idx = match sh.list_entry[x].get(i) {
                Some(&entry) => *temp.get(entry as usize).unwrap_or(&temp[0]),
                None => temp[i.min(temp.len() - 1)],
            };
            out[x].push(idx);
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn picture_parameters(
    sps: &Sps,
    pps: &Pps,
    sh: &SliceHeader,
    pos: &ParsePositions,
    nal_type: NalUnitType,
    current: &Current,
    dpb: &[StoredPicture],
) -> PictureParameterBufferHEVC {
    let mut p = PictureParameterBufferHEVC {
        curr_pic: VAPictureHEVC::new(current.surface.id(), current.poc, 0),
        pic_width_in_luma_samples: sps.pic_width_in_luma_samples as u16,
        pic_height_in_luma_samples: sps.pic_height_in_luma_samples as u16,
        sps_max_dec_pic_buffering_minus1: sps.max_dec_pic_buffering_minus1 as u8,
        bit_depth_luma_minus8: sps.bit_depth_luma_minus8 as u8,
        bit_depth_chroma_minus8: sps.bit_depth_chroma_minus8 as u8,
        pcm_sample_bit_depth_luma_minus1: sps.pcm.map(|p| p.0 as u8).unwrap_or(0),
        pcm_sample_bit_depth_chroma_minus1: sps.pcm.map(|p| p.1 as u8).unwrap_or(0),
        log2_min_luma_coding_block_size_minus3: sps.log2_min_cb_size_minus3 as u8,
        log2_diff_max_min_luma_coding_block_size: sps.log2_diff_max_min_cb_size as u8,
        log2_min_transform_block_size_minus2: sps.log2_min_tb_size_minus2 as u8,
        log2_diff_max_min_transform_block_size: sps.log2_diff_max_min_tb_size as u8,
        log2_min_pcm_luma_coding_block_size_minus3: sps.pcm.map(|p| p.2 as u8).unwrap_or(0),
        log2_diff_max_min_pcm_luma_coding_block_size: sps.pcm.map(|p| p.3 as u8).unwrap_or(0),
        max_transform_hierarchy_depth_intra: sps.max_transform_hierarchy_depth_intra as u8,
        max_transform_hierarchy_depth_inter: sps.max_transform_hierarchy_depth_inter as u8,
        init_qp_minus26: pps.init_qp_minus26 as i8,
        diff_cu_qp_delta_depth: pps.diff_cu_qp_delta_depth as u8,
        pps_cb_qp_offset: pps.cb_qp_offset as i8,
        pps_cr_qp_offset: pps.cr_qp_offset as i8,
        log2_parallel_merge_level_minus2: pps.log2_parallel_merge_level_minus2 as u8,
        num_tile_columns_minus1: pps.num_tile_columns_minus1 as u8,
        num_tile_rows_minus1: pps.num_tile_rows_minus1 as u8,
        log2_max_pic_order_cnt_lsb_minus4: sps.log2_max_poc_lsb_minus4 as u8,
        num_short_term_ref_pic_sets: sps.num_short_term_ref_pic_sets as u8,
        num_long_term_ref_pic_sps: sps.num_long_term_ref_pics_sps as u8,
        num_ref_idx_l0_default_active_minus1: pps.num_ref_idx_l0_default_active_minus1 as u8,
        num_ref_idx_l1_default_active_minus1: pps.num_ref_idx_l1_default_active_minus1 as u8,
        pps_beta_offset_div2: pps.beta_offset_div2 as i8,
        pps_tc_offset_div2: pps.tc_offset_div2 as i8,
        num_extra_slice_header_bits: pps.num_extra_slice_header_bits as u8,
        // The size of an `st_ref_pic_set()` the slice header carried itself,
        // so the driver can skip it; zero when the slice named an SPS set,
        // which is what the parser reports.
        st_rps_bits: pos.st_rps_bits,
        ..PictureParameterBufferHEVC::default()
    };

    // Tile geometry: uniform spacing is expanded here because libva wants the
    // widths either way (`va_dec_hevc.h:125`).
    let (cols, rows) = (
        pps.num_tile_columns_minus1 as usize + 1,
        pps.num_tile_rows_minus1 as usize + 1,
    );
    if pps.tiles_enabled {
        let ctb_cols = sps.pic_width_in_ctbs();
        let ctb_rows = sps.pic_height_in_ctbs();
        for i in 0..cols.min(p.column_width_minus1.len()) {
            let width = if pps.uniform_spacing {
                ((i + 1) as u32 * ctb_cols) / cols as u32 - (i as u32 * ctb_cols) / cols as u32
            } else {
                pps.column_width_minus1.get(i).copied().unwrap_or(0) + 1
            };
            p.column_width_minus1[i] = width.saturating_sub(1) as u16;
        }
        for i in 0..rows.min(p.row_height_minus1.len()) {
            let height = if pps.uniform_spacing {
                ((i + 1) as u32 * ctb_rows) / rows as u32 - (i as u32 * ctb_rows) / rows as u32
            } else {
                pps.row_height_minus1.get(i).copied().unwrap_or(0) + 1
            };
            p.row_height_minus1[i] = height.saturating_sub(1) as u16;
        }
    }

    let mut n = 0usize;
    for (i, pic) in dpb.iter().enumerate() {
        if pic.mark == Mark::Unused || n >= p.reference_frames.len() {
            continue;
        }
        let mut flags = 0;
        if pic.mark == Mark::LongTerm {
            flags |= PICTURE_LONG_TERM_REFERENCE;
        }
        if current.rps.st_curr_before.contains(&i) {
            flags |= PICTURE_RPS_ST_CURR_BEFORE;
        } else if current.rps.st_curr_after.contains(&i) {
            flags |= PICTURE_RPS_ST_CURR_AFTER;
        } else if current.rps.lt_curr.contains(&i) {
            flags |= PICTURE_RPS_LT_CURR;
        }
        p.reference_frames[n] = VAPictureHEVC::new(pic.surface.id(), pic.poc, flags);
        n += 1;
    }
    for entry in p.reference_frames.iter_mut().skip(n) {
        *entry = VAPictureHEVC::new(ec_va::sys::VA_INVALID_SURFACE, 0, PICTURE_INVALID);
    }

    p.pic_fields = p
        .pic_fields
        .chroma_format_idc(sps.chroma_format_idc)
        .separate_colour_plane_flag(u32::from(sps.separate_colour_plane))
        .pcm_enabled_flag(u32::from(sps.pcm_enabled))
        .scaling_list_enabled_flag(u32::from(sps.scaling_list_enabled))
        .transform_skip_enabled_flag(u32::from(pps.transform_skip_enabled))
        .amp_enabled_flag(u32::from(sps.amp_enabled))
        .strong_intra_smoothing_enabled_flag(u32::from(sps.strong_intra_smoothing))
        .sign_data_hiding_enabled_flag(u32::from(pps.sign_data_hiding_enabled))
        .constrained_intra_pred_flag(u32::from(pps.constrained_intra_pred))
        .cu_qp_delta_enabled_flag(u32::from(pps.cu_qp_delta_enabled))
        .weighted_pred_flag(u32::from(pps.weighted_pred))
        .weighted_bipred_flag(u32::from(pps.weighted_bipred))
        .transquant_bypass_enabled_flag(u32::from(pps.transquant_bypass_enabled))
        .tiles_enabled_flag(u32::from(pps.tiles_enabled))
        .entropy_coding_sync_enabled_flag(u32::from(pps.entropy_coding_sync_enabled))
        .pps_loop_filter_across_slices_enabled_flag(u32::from(
            pps.loop_filter_across_slices_enabled,
        ))
        .loop_filter_across_tiles_enabled_flag(u32::from(pps.loop_filter_across_tiles_enabled))
        .pcm_loop_filter_disabled_flag(u32::from(sps.pcm.map(|p| p.4).unwrap_or(false)))
        .no_pic_reordering_flag(u32::from(sps.max_num_reorder_pics == 0))
        .no_bi_pred_flag(u32::from(sh.slice_type != SliceType::B));

    p.slice_parsing_fields = p
        .slice_parsing_fields
        .lists_modification_present_flag(u32::from(pps.lists_modification_present))
        .long_term_ref_pics_present_flag(u32::from(sps.long_term_ref_pics_present))
        .sps_temporal_mvp_enabled_flag(u32::from(sps.temporal_mvp_enabled))
        .cabac_init_present_flag(u32::from(pps.cabac_init_present))
        .output_flag_present_flag(u32::from(pps.output_flag_present))
        .dependent_slice_segments_enabled_flag(u32::from(pps.dependent_slice_segments_enabled))
        .pps_slice_chroma_qp_offsets_present_flag(u32::from(pps.slice_chroma_qp_offsets_present))
        .sample_adaptive_offset_enabled_flag(u32::from(sps.sao_enabled))
        .deblocking_filter_override_enabled_flag(u32::from(pps.deblocking_filter_override_enabled))
        .pps_disable_deblocking_filter_flag(u32::from(pps.deblocking_filter_disabled))
        .slice_segment_header_extension_present_flag(u32::from(
            pps.slice_segment_header_extension_present,
        ))
        .rap_pic_flag(u32::from(nal_type.is_irap()))
        .idr_pic_flag(u32::from(nal_type.is_idr()))
        .intra_pic_flag(u32::from(sh.slice_type == SliceType::I));
    p
}

/// Copy the parsed `pred_weight_table()` into the slice parameter buffer.
fn write_weights(s: &mut SliceParameterBufferHEVC, sh: &SliceHeader) {
    let Some(w) = sh.pred_weight_table.as_ref() else {
        return;
    };
    s.luma_log2_weight_denom = w.luma_log2_weight_denom as u8;
    s.delta_chroma_log2_weight_denom = w.delta_chroma_log2_weight_denom as i8;
    for (list, entries) in [(0usize, &w.l0), (1usize, &w.l1)] {
        let (dlw, lo, dcw, co) = if list == 0 {
            (
                &mut s.delta_luma_weight_l0,
                &mut s.luma_offset_l0,
                &mut s.delta_chroma_weight_l0,
                &mut s.chroma_offset_l0,
            )
        } else {
            (
                &mut s.delta_luma_weight_l1,
                &mut s.luma_offset_l1,
                &mut s.delta_chroma_weight_l1,
                &mut s.chroma_offset_l1,
            )
        };
        for (i, entry) in entries.iter().take(15).enumerate() {
            if let Some((weight, offset)) = entry.luma {
                dlw[i] = weight as i8;
                lo[i] = offset as i8;
            }
            if let Some(pair) = entry.chroma {
                for c in 0..2 {
                    dcw[i][c] = pair[c].0 as i8;
                    co[i][c] = pair[c].1 as i8;
                }
            }
        }
    }
}

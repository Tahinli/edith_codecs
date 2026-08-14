//! Decoder core: parameter-set store, decoded picture buffer, macroblock
//! decode loop, reconstruction and the in-loop filter driver.
//!
//! Layout choices (perf-first):
//! - Planes are padded (`PAD_Y`/`PAD_C` samples each side) and their borders
//!   replicated once per picture, so neighbour gathers and inter-prediction
//!   overreach never bounds-check per sample and edge availability is data,
//!   not arithmetic.
//! - All per-macroblock and per-4x4-block state lives in flat struct-of-arrays
//!   keyed by macroblock address or block coordinate: coefficient counts, intra
//!   modes, QP, flags, owning slice, motion vectors, reference identities. No
//!   per-macroblock heap objects anywhere.
//! - Pictures are pooled by the [`Dpb`], so a steady-state decode loop reuses
//!   every buffer and performs zero allocations.
//!
//! Output is in display order: pictures leave the decoded picture buffer by the
//! bumping process, smallest picture order count first, which is the order a
//! player presents them in. [`OutputOrder::Decode`] switches that off for a
//! caller that wants pictures as they are decoded.

use std::collections::VecDeque;

use ec_core::BitReader;
use ec_core::error::{Error, Result};
use ec_core::frame::{PixelFormat, Plane, VideoFrame};
use ec_core::timebase::Timestamp;
use ec_h264_syntax::{
    AnnexBIter, DecRefPicMarking, NalHeader, NalUnitType, Pps, PredWeightTable, SliceHeader,
    SliceType, Sps, WeightEntry, unescape_rbsp,
};

use crate::deblock::{
    chroma_qp, edge_params, filter_chroma_line, filter_luma_h_edge16, filter_luma_line,
};
use crate::dpb::{
    BLK_DIRECT, BLK_INTRA, BLK_SKIP, Dpb, Mark, NO_SLICE, PicInfo, Picture, Plane8, RefList,
    SliceParams,
};
use crate::entropy::{
    BlockCat, Entropy, FLAG_CHROMA_PRED, FLAG_DECODED, FLAG_DIRECT, FLAG_I16, FLAG_INTER, FLAG_PCM,
    FLAG_SKIP, FLAG_TRANS8X8, MbCtx, MbInfo,
};
use crate::inter::{RefPlane, Weights, combine, mc_chroma, mc_luma};
use crate::mv::{
    B_SHAPES, B_SUB, MbShape, MvCtx, P_SHAPES, P_SUB, Pred, SubShape, min_positive, neighbour_mvd,
    predict_mv, ref_idx_cond, write_block, write_intra_mb, write_mvd,
};
use crate::pred::{
    Nbr4, Nbr8, PlaneWindow, add_residual_4x4, add_residual_8x8, filter_nbr8, pred_4x4, pred_8x8,
    pred_16x16, pred_chroma_8x8,
};
use crate::tables::{BLK4_POS, CHROMA_QP};
use crate::transform::{
    LevelScale4x4, LevelScale8x8, chroma_dc_transform_420, dequant_4x4, dequant_8x8,
    inverse_transform_4x4, inverse_transform_8x8, luma_dc_transform, unzigzag, unzigzag_8x8,
    unzigzag_ac15,
};

/// Where a 4x4 block's top-right neighbour samples come from, per
/// luma4x4BlkIdx (derivation of 6.4.12 + the 8.3.1.2 blkIdx 3/11 rule).
#[derive(Clone, Copy, PartialEq)]
enum TrKind {
    /// Bottom row of an earlier block in this macroblock: always available.
    InMb,
    /// The macroblock above.
    B,
    /// The macroblock above-right.
    C,
    /// Never available (right column, or blkIdx 3/11).
    None,
}

const TR_KIND: [TrKind; 16] = [
    TrKind::B,
    TrKind::B,
    TrKind::InMb,
    TrKind::None,
    TrKind::B,
    TrKind::C,
    TrKind::InMb,
    TrKind::None,
    TrKind::InMb,
    TrKind::InMb,
    TrKind::InMb,
    TrKind::None,
    TrKind::InMb,
    TrKind::None,
    TrKind::InMb,
    TrKind::None,
];

/// The order [`Decoder`] hands decoded pictures back in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputOrder {
    /// Display order: pictures leave the decoded picture buffer by the bumping
    /// process of clause C.4.5.3. This is what a player needs, and the only
    /// order that is correct for a stream with B pictures.
    #[default]
    Display,
    /// Decode order: each picture is emitted the moment it is complete. Lower
    /// latency, wrong presentation order whenever the stream reorders.
    Decode,
}

/// Streaming H.264 decoder (frame-coded 4:2:0 8-bit; unsupported features come
/// back as named [`Error::Unsupported`] values, never wrong pixels).
pub struct Decoder {
    sps_map: Vec<Option<Sps>>,
    pps_map: Vec<Option<Pps>>,
    rbsp: Vec<u8>,
    /// The picture being decoded, held out of the buffer pool.
    cur: Picture,
    dpb: Dpb,
    has_picture: bool,
    /// Scaling factors of the active PPS/SPS, `[intra, inter][Y, Cb, Cr]`.
    ls: [[LevelScale4x4; 3]; 2],
    /// The same for the 8x8 luma transform, `[intra, inter]` (4:2:0 has no 8x8
    /// chroma transform).
    ls8: [LevelScale8x8; 2],
    active_pps: Option<u32>,
    /// `seq_parameter_set_id` of the most recently stored SPS, so a container
    /// entry path can publish the picture size before the first slice.
    last_sps: Option<u8>,
    /// Clause 8.2 view of the open picture, from its first slice header.
    cur_info: PicInfo,
    /// `dec_ref_pic_marking( )` of the open picture, kept in place so a
    /// steady-state loop does not reallocate its MMCO list.
    cur_marking: DecRefPicMarking,
    has_marking: bool,
    /// Identity of the picture `end_picture` completed most recently.
    last_decoded: i32,
    output_order: OutputOrder,
    out: VecDeque<VideoFrame>,
    /// Presentation timestamp to attach to the next picture started.
    pending_pts: Option<Timestamp>,
}

/// What [`Decoder::push_nal`] did with a NAL unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NalOutcome {
    /// SPS or PPS stored.
    ParameterSet,
    /// Non-VCL NAL skipped (SEI, AUD, filler, ...).
    Skipped,
    /// A slice was decoded into the open picture.
    SliceDecoded,
    /// The NAL starts a new picture while one is open; it was not consumed.
    /// Call [`Decoder::end_picture`], then push it again.
    PictureBoundary,
}

impl Default for Decoder {
    fn default() -> Self {
        Decoder::new()
    }
}

fn flat_scaling() -> [[LevelScale4x4; 3]; 2] {
    [
        [
            LevelScale4x4::new(&[16; 16]),
            LevelScale4x4::new(&[16; 16]),
            LevelScale4x4::new(&[16; 16]),
        ],
        [
            LevelScale4x4::new(&[16; 16]),
            LevelScale4x4::new(&[16; 16]),
            LevelScale4x4::new(&[16; 16]),
        ],
    ]
}

impl Decoder {
    /// A fresh decoder with no parameter sets.
    pub fn new() -> Decoder {
        Decoder {
            sps_map: vec![None; 32],
            pps_map: vec![None; 256],
            rbsp: Vec::new(),
            cur: Picture::default(),
            dpb: Dpb::default(),
            has_picture: false,
            ls: flat_scaling(),
            ls8: [LevelScale8x8::new(&[16; 64]), LevelScale8x8::new(&[16; 64])],
            active_pps: None,
            last_sps: None,
            cur_info: PicInfo {
                is_idr: false,
                is_reference: false,
                frame_num: 0,
                pic_order_cnt_lsb: 0,
                delta_pic_order_cnt_bottom: 0,
                delta_pic_order_cnt: [0; 2],
            },
            cur_marking: DecRefPicMarking::default(),
            has_marking: false,
            last_decoded: -1,
            output_order: OutputOrder::default(),
            out: VecDeque::new(),
            pending_pts: None,
        }
    }

    /// Choose between display order (the default) and decode order.
    pub fn set_output_order(&mut self, order: OutputOrder) {
        self.output_order = order;
    }

    /// The order pictures are handed back in.
    pub fn output_order(&self) -> OutputOrder {
        self.output_order
    }

    /// Attach `pts` to the next picture started, so a presentation timestamp
    /// follows its picture through reordering instead of staying with the
    /// packet that carried it.
    pub fn set_next_pts(&mut self, pts: Option<Timestamp>) {
        self.pending_pts = pts;
    }

    /// Cropped picture size of the most recently stored SPS, `None` until one
    /// arrives. Lets a container entry path fill in stream parameters from an
    /// `avcC` record without decoding a picture first.
    pub fn picture_size(&self) -> Option<(u32, u32)> {
        let sps = self.sps_map[self.last_sps? as usize].as_ref()?;
        Some((sps.width, sps.height))
    }

    /// Decode the first IDR picture of an Annex B stream and return it.
    pub fn decode_first_idr(&mut self, annexb: &[u8]) -> Result<VideoFrame> {
        let mut got_slice = false;
        for nal in AnnexBIter::new(annexb) {
            match self.push_nal(nal)? {
                NalOutcome::PictureBoundary => break,
                NalOutcome::SliceDecoded => got_slice = true,
                _ => {}
            }
        }
        if !got_slice {
            return Err(Error::NeedMore);
        }
        self.end_picture()?;
        self.frame()
    }

    /// Feed one NAL unit (header byte first, no start code).
    pub fn push_nal(&mut self, nal: &[u8]) -> Result<NalOutcome> {
        let Some((&header_byte, payload)) = nal.split_first() else {
            return Err(Error::NeedMore);
        };
        let header = NalHeader::parse(header_byte)?;
        match header.unit_type {
            NalUnitType::Sps => {
                let mut rbsp = core::mem::take(&mut self.rbsp);
                unescape_rbsp(payload, &mut rbsp);
                let sps = Sps::parse(&rbsp);
                self.rbsp = rbsp;
                let sps = sps?;
                let id = sps.id;
                self.dpb.configure(&sps);
                self.sps_map[id as usize] = Some(sps);
                self.last_sps = Some(id);
                Ok(NalOutcome::ParameterSet)
            }
            NalUnitType::Pps => {
                let mut rbsp = core::mem::take(&mut self.rbsp);
                unescape_rbsp(payload, &mut rbsp);
                let pps = Pps::parse(&rbsp, |id| self.sps_map[id as usize].as_ref());
                self.rbsp = rbsp;
                let pps = pps?;
                let id = pps.id as usize;
                self.pps_map[id] = Some(pps);
                Ok(NalOutcome::ParameterSet)
            }
            NalUnitType::Slice | NalUnitType::SliceIdr => {
                let mut rbsp = core::mem::take(&mut self.rbsp);
                unescape_rbsp(payload, &mut rbsp);
                let r = self.decode_slice_rbsp(header, &rbsp);
                self.rbsp = rbsp;
                r
            }
            NalUnitType::SliceDataA | NalUnitType::SliceDataB | NalUnitType::SliceDataC => {
                Err(Error::unsupported(
                    "slice data partitioning",
                    "nal_unit_type 2 to 4 carry a slice in three NAL units that \
                     clause 7.4.1 reassembles by slice_id; not implemented",
                ))
            }
            _ => Ok(NalOutcome::Skipped),
        }
    }

    /// Run the deblocking filter over the open picture, store it in the decoded
    /// picture buffer and queue whatever pictures that makes ready for output.
    pub fn end_picture(&mut self) -> Result<()> {
        if !self.has_picture || self.cur.complete {
            return Err(Error::corrupt("end_picture without an open picture"));
        }
        deblock_picture(&mut self.cur);
        self.cur.extend_borders();
        self.cur.complete = true;
        self.last_decoded = self.cur.id;
        let sps_id = self.cur.sps_id as usize;
        let sps = self.sps_map[sps_id]
            .take()
            .ok_or_else(|| Error::corrupt("active SPS vanished"))?;
        let pic = core::mem::take(&mut self.cur);
        let marking = self.has_marking.then_some(&self.cur_marking);
        let stored = self.dpb.store(pic, &sps, &self.cur_info, marking);
        self.sps_map[sps_id] = Some(sps);
        stored?;
        self.has_picture = false;
        self.pump_output(false)
    }

    /// Emit every remaining picture (end of stream).
    pub fn flush(&mut self) -> Result<()> {
        if self.has_picture && !self.cur.complete {
            self.end_picture()?;
        }
        self.pump_output(true)
    }

    /// The next frame ready for output, in the configured order.
    pub fn next_frame(&mut self) -> Option<VideoFrame> {
        self.out.pop_front()
    }

    /// The picture `end_picture` completed most recently, in decode order.
    ///
    /// Kept for a caller that drives one picture at a time; a player wants
    /// [`Decoder::next_frame`], which respects the reordering the stream asks
    /// for.
    pub fn frame(&self) -> Result<VideoFrame> {
        let id = self.last_decoded;
        let pic = self
            .dpb
            .frames
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| Error::corrupt("frame() before end_picture()"))?;
        self.picture_to_frame(pic)
    }

    /// True while a picture is open (started, not completed).
    pub fn picture_open(&self) -> bool {
        self.has_picture && !self.cur.complete
    }

    /// Drop picture state after a seek. Parameter sets survive, because the
    /// container does not resend them at every seek point.
    pub fn reset_pictures(&mut self) {
        self.has_picture = false;
        self.cur.complete = false;
        self.dpb.clear();
        self.out.clear();
        self.last_decoded = -1;
        self.pending_pts = None;
    }

    /// Move pictures out of the decoded picture buffer into the output queue.
    fn pump_output(&mut self, flush: bool) -> Result<()> {
        if self.output_order == OutputOrder::Decode && !flush {
            let id = self.last_decoded;
            if let Some(i) = self.dpb.frames.iter().position(|p| p.id == id && p.output) {
                let frame = self.picture_to_frame(&self.dpb.frames[i])?;
                self.out.push_back(frame);
                self.dpb.released(i);
            }
            return Ok(());
        }
        while let Some(i) = self.dpb.next_output(flush) {
            let frame = self.picture_to_frame(&self.dpb.frames[i])?;
            self.out.push_back(frame);
            self.dpb.released(i);
        }
        Ok(())
    }

    /// Copy a stored picture out as a cropped I420 frame.
    fn picture_to_frame(&self, pic: &Picture) -> Result<VideoFrame> {
        let (w, h) = (pic.out_size.0 as usize, pic.out_size.1 as usize);
        let (cx, cy) = (pic.crop.0 as usize, pic.crop.1 as usize);
        let copy = |p: &Plane8, x0: usize, y0: usize, w: usize, h: usize| -> Plane {
            let mut out = vec![0u8; w * h];
            for row in 0..h {
                let src = p.at(x0, y0 + row);
                out[row * w..(row + 1) * w].copy_from_slice(&p.data[src..src + w]);
            }
            Plane::new(out, w)
        };
        let planes = vec![
            copy(&pic.y, cx, cy, w, h),
            copy(&pic.cb, cx / 2, cy / 2, w.div_ceil(2), h.div_ceil(2)),
            copy(&pic.cr, cx / 2, cy / 2, w.div_ceil(2), h.div_ceil(2)),
        ];
        let mut frame = VideoFrame::try_new(PixelFormat::I420, w as u32, h as u32, planes)?;
        frame.color = pic.color;
        frame.pts = pic.pts;
        Ok(frame)
    }

    fn decode_slice_rbsp(&mut self, header: NalHeader, rbsp: &[u8]) -> Result<NalOutcome> {
        // Peek first_mb / slice_type / pps_id to select parameter sets.
        let mut peek = BitReader::new(rbsp);
        let first_mb = peek.read_ue()?;
        let slice_type_code = peek.read_ue()?;
        let pps_id = peek.read_ue()?;
        if pps_id > 255 {
            return Err(Error::corrupt("pic_parameter_set_id > 255"));
        }
        if self.has_picture && !self.cur.complete && first_mb == 0 && self.cur.decoded_mbs > 0 {
            return Ok(NalOutcome::PictureBoundary);
        }
        let pps = self.pps_map[pps_id as usize]
            .as_ref()
            .ok_or_else(|| Error::corrupt("slice refers to an unknown PPS"))?;
        let sps = self.sps_map[pps.sps_id as usize]
            .as_ref()
            .ok_or_else(|| Error::corrupt("PPS refers to an unknown SPS"))?;

        check_supported(sps, pps, slice_type_code)?;
        let sh = SliceHeader::parse(rbsp, header, sps, pps)?;
        // Borrow the parameter sets out of their slots for the duration of the
        // slice: cloning them per slice would allocate, and a decode loop that
        // allocates per slice is the thing the whole layout is built to avoid.
        let sps_slot = pps.sps_id as usize;
        let sps = self.sps_map[sps_slot].take().expect("checked above");
        let pps = self.pps_map[pps_id as usize].take().expect("checked above");
        let outcome = self.decode_slice_data(&sps, &pps, &sh, header, rbsp, first_mb);
        self.sps_map[sps_slot] = Some(sps);
        self.pps_map[pps_id as usize] = Some(pps);
        outcome
    }

    #[allow(clippy::too_many_arguments)]
    fn decode_slice_data(
        &mut self,
        sps: &Sps,
        pps: &Pps,
        sh: &SliceHeader,
        header: NalHeader,
        rbsp: &[u8],
        first_mb: u32,
    ) -> Result<NalOutcome> {
        if !self.has_picture || self.cur.complete {
            self.start_picture(sps, pps, sh, header)?;
        }
        if self.cur.sps_id != sps.id {
            return Err(Error::corrupt("SPS changed mid-picture"));
        }
        if self.cur.slices.len() >= usize::from(u16::MAX) {
            return Err(Error::corrupt("more than 65534 slices in a picture"));
        }
        let slice_id = self.cur.slices.len() as u16;
        self.cur.slices.push(SliceParams {
            disable_deblock_idc: sh.deblock.disable_idc,
            alpha_offset: sh.deblock.alpha_c0_offset,
            beta_offset: sh.deblock.beta_offset,
            cb_qp_offset: pps.chroma_qp_index_offset,
            cr_qp_offset: pps.second_chroma_qp_index_offset,
        });

        // Reference picture lists (clause 8.2.4), rebuilt per slice because
        // num_ref_idx_active and the modification list are slice header state.
        let max_pic_num = 1i32 << sps.log2_max_frame_num;
        let lists = if sh.slice_type.is_intra() {
            [RefList::default(), RefList::default()]
        } else {
            self.dpb.number_short_term(sh.frame_num, sps);
            self.dpb.build_ref_lists(
                sh.slice_type,
                self.cur.poc,
                sh.frame_num as i32,
                max_pic_num,
                sh.num_ref_idx_l0_active as usize,
                sh.num_ref_idx_l1_active as usize,
                (&sh.ref_pic_list_mod_l0, &sh.ref_pic_list_mod_l1),
            )?
        };

        let mut r = if pps.entropy_coding_mode {
            let column = if sh.slice_type.is_intra() {
                0
            } else {
                sh.cabac_init_idc as usize + 1
            };
            Entropy::cabac(rbsp, sh.header_bits, sh.slice_qp, column)?
        } else {
            Entropy::cavlc(rbsp, sh.header_bits)
        };

        let mut ctx = SliceCtx {
            slice_id,
            slice_type: sh.slice_type,
            qp: sh.slice_qp,
            cb_qp_offset: pps.chroma_qp_index_offset,
            cr_qp_offset: pps.second_chroma_qp_index_offset,
            transform_8x8_mode: pps.transform_8x8_mode,
            constrained_intra_pred: pps.constrained_intra_pred,
            qp_delta_inc: 0,
            lists,
            num_ref_idx: [
                sh.num_ref_idx_l0_active as usize,
                sh.num_ref_idx_l1_active as usize,
            ],
            direct_spatial: sh.direct_spatial_mv_pred,
            direct_8x8_inference: sps.direct_8x8_inference,
            weighted_pred: pps.weighted_pred,
            weighted_bipred_idc: pps.weighted_bipred_idc,
            weights: sh.pred_weight_table.clone(),
            poc: self.cur.poc,
        };
        let intra_slice = sh.slice_type.is_intra();
        let b_slice = sh.slice_type == SliceType::B;
        if !intra_slice && ctx.lists[0].len() == 0 {
            return Err(Error::corrupt("inter slice with no reference pictures"));
        }

        let mbs = self.cur.mb_w * self.cur.mb_h;
        let mut mb_addr = first_mb as usize;
        let mut more = true;
        while more {
            if mb_addr >= mbs {
                return Err(Error::corrupt("macroblock address beyond the picture"));
            }
            let mut decode_mb = true;
            if !intra_slice {
                if r.is_cabac() {
                    self.begin_mb(&mut r, &ctx, mb_addr);
                    let inc = self.skip_inc(&ctx, mb_addr);
                    if r.mb_skip_flag(b_slice, inc)? {
                        decode_macroblock(
                            &mut self.cur,
                            &self.dpb.frames,
                            &self.ls,
                            &self.ls8,
                            &mut r,
                            &mut ctx,
                            mb_addr,
                            true,
                        )?;
                        decode_mb = false;
                    }
                } else {
                    let run = r.mb_skip_run()?;
                    for _ in 0..run {
                        if mb_addr >= mbs {
                            return Err(Error::corrupt("mb_skip_run past the picture"));
                        }
                        decode_macroblock(
                            &mut self.cur,
                            &self.dpb.frames,
                            &self.ls,
                            &self.ls8,
                            &mut r,
                            &mut ctx,
                            mb_addr,
                            true,
                        )?;
                        mb_addr += 1;
                    }
                    if run > 0 {
                        more = r.more_macroblocks()?;
                        if !more {
                            break;
                        }
                        if mb_addr >= mbs {
                            return Err(Error::corrupt("macroblock address beyond the picture"));
                        }
                    }
                }
            }
            if decode_mb {
                decode_macroblock(
                    &mut self.cur,
                    &self.dpb.frames,
                    &self.ls,
                    &self.ls8,
                    &mut r,
                    &mut ctx,
                    mb_addr,
                    false,
                )?;
            }
            more = r.more_macroblocks()?;
            mb_addr += 1;
        }
        Ok(NalOutcome::SliceDecoded)
    }

    /// Publish the neighbourhood of `mb_addr` to a CABAC reader ahead of
    /// mb_skip_flag, which is decoded before the macroblock layer.
    fn begin_mb(&self, r: &mut Entropy<'_>, ctx: &SliceCtx, mb_addr: usize) {
        let pic = &self.cur;
        let mb_x = mb_addr % pic.mb_w;
        let mb_y = mb_addr / pic.mb_w;
        let nbr = mb_neighbors(pic, mb_x, mb_y, ctx.slice_id);
        let info = |addr: usize| MbInfo {
            flags: pic.mb_flags[addr],
            cbp: pic.mb_cbp[addr],
            dc_cbf: pic.mb_dc_cbf[addr],
        };
        r.begin_mb(&MbCtx {
            a: nbr.a.then(|| info(mb_addr - 1)),
            b: nbr.b.then(|| info(mb_addr - pic.mb_w)),
            qp_delta_inc: ctx.qp_delta_inc,
        });
    }

    /// ctxIdxInc for mb_skip_flag (9.3.3.1.1.1): a neighbour counts unless it
    /// is unavailable or was itself skipped.
    fn skip_inc(&self, ctx: &SliceCtx, mb_addr: usize) -> usize {
        let pic = &self.cur;
        let mb_x = mb_addr % pic.mb_w;
        let mb_y = mb_addr / pic.mb_w;
        let nbr = mb_neighbors(pic, mb_x, mb_y, ctx.slice_id);
        let cond =
            |avail: bool, addr: usize| usize::from(avail && pic.mb_flags[addr] & FLAG_SKIP == 0);
        cond(nbr.a, mb_addr.wrapping_sub(1)) + cond(nbr.b, mb_addr.wrapping_sub(pic.mb_w))
    }

    /// Open a new picture: clause 8.2 bookkeeping, then the geometry reset.
    fn start_picture(
        &mut self,
        sps: &Sps,
        pps: &Pps,
        sh: &SliceHeader,
        header: NalHeader,
    ) -> Result<()> {
        let info = PicInfo {
            is_idr: header.is_idr(),
            is_reference: header.ref_idc != 0,
            frame_num: sh.frame_num,
            pic_order_cnt_lsb: sh.pic_order_cnt_lsb,
            delta_pic_order_cnt_bottom: sh.delta_pic_order_cnt_bottom,
            delta_pic_order_cnt: sh.delta_pic_order_cnt,
        };
        // A frame_num gap is what a seek or a dropped packet looks like: the
        // pictures it names never arrive, so clause 8.2.5.2 infers them and
        // decoding continues rather than stalling until the next IDR.
        if !info.is_idr && self.dpb.frame_num_gap(sps, sh.frame_num) {
            if !sps.gaps_in_frame_num_allowed {
                // Not a conformant gap: pictures were lost. Same repair.
                self.dpb.fill_frame_num_gap(sps, sh.frame_num)?;
            } else {
                self.dpb.fill_frame_num_gap(sps, sh.frame_num)?;
            }
            self.pump_output(false)?;
        }
        let poc = self.dpb.picture_order_count(sps, &info)?;
        let mut pic = self.dpb.take_picture();
        pic.start(sps);
        pic.poc = poc.value;
        pic.poc_msb = poc.msb;
        pic.poc_lsb = poc.lsb;
        pic.frame_num_offset = poc.frame_num_offset;
        pic.frame_num = sh.frame_num;
        pic.pts = self.pending_pts.take();
        self.cur = pic;
        self.cur_info = info;
        self.has_marking = sh.dec_ref_pic_marking.is_some();
        if let Some(m) = &sh.dec_ref_pic_marking {
            self.cur_marking.no_output_of_prior_pics = m.no_output_of_prior_pics;
            self.cur_marking.long_term_reference = m.long_term_reference;
            self.cur_marking.adaptive = m.adaptive;
            self.cur_marking.mmcos.clear();
            self.cur_marking.mmcos.extend_from_slice(&m.mmcos);
        }
        self.has_picture = true;
        if self.active_pps != Some(pps.id) {
            let lists = pps.scaling_lists.as_ref().or(sps.scaling_lists.as_ref());
            let weights = match lists {
                Some(l) => [
                    [&l.list_4x4[0], &l.list_4x4[1], &l.list_4x4[2]],
                    [&l.list_4x4[3], &l.list_4x4[4], &l.list_4x4[5]],
                ],
                None => [[&[16u8; 16]; 3]; 2],
            };
            self.ls = [
                [
                    LevelScale4x4::new(weights[0][0]),
                    LevelScale4x4::new(weights[0][1]),
                    LevelScale4x4::new(weights[0][2]),
                ],
                [
                    LevelScale4x4::new(weights[1][0]),
                    LevelScale4x4::new(weights[1][1]),
                    LevelScale4x4::new(weights[1][2]),
                ],
            ];
            let w8 = match lists {
                Some(l) => [&l.list_8x8[0], &l.list_8x8[1]],
                None => [&[16u8; 64]; 2],
            };
            self.ls8 = [LevelScale8x8::new(w8[0]), LevelScale8x8::new(w8[1])];
            self.active_pps = Some(pps.id);
        }
        Ok(())
    }
}

/// Reject unsupported streams with named reasons before touching pixels.
///
/// Every `why` here names the syntax element that fired and the machinery it
/// would need: a refusal is a capability statement about this binary, and the
/// conformance suite proves each one against a stream that really uses the
/// feature.
fn check_supported(sps: &Sps, pps: &Pps, slice_type_code: u32) -> Result<()> {
    if pps.num_slice_groups > 1 {
        return Err(Error::unsupported(
            "FMO slice groups",
            "num_slice_groups_minus1 > 0 needs the macroblock-to-slice-group \
             map of clause 8.2.2; only one slice group per picture is decoded",
        ));
    }
    if sps.chroma_format_idc != 1 {
        return Err(Error::unsupported(
            "chroma format",
            format!(
                "chroma_format_idc {} needs its own chroma prediction, scan and \
                 DC transform; only 4:2:0 (ChromaArrayType 1) is decoded",
                sps.chroma_format_idc
            ),
        ));
    }
    if sps.bit_depth_luma != 8 || sps.bit_depth_chroma != 8 {
        return Err(Error::unsupported(
            "bit depth",
            format!(
                "{}-bit luma / {}-bit chroma needs 16-bit sample planes and the \
                 widened transform clip of clause 8.5; only 8-bit is decoded",
                sps.bit_depth_luma, sps.bit_depth_chroma
            ),
        ));
    }
    if !sps.frame_mbs_only {
        return Err(Error::unsupported(
            "interlaced coding",
            "frame_mbs_only_flag 0 admits field pictures and MBAFF, whose \
             neighbour derivation (6.4.9) and field deblocking are not implemented",
        ));
    }
    if sps.separate_colour_plane {
        return Err(Error::unsupported(
            "separate colour planes",
            "separate_colour_plane_flag 1 codes 4:4:4 as three monochrome \
             planes with their own slice headers; not implemented",
        ));
    }
    if sps.transform_bypass {
        return Err(Error::unsupported(
            "transform bypass",
            "qpprime_y_zero_transform_bypass_flag 1 makes QP'Y 0 lossless, \
             bypassing the transform and scaling of 8.5 entirely; not implemented",
        ));
    }
    if slice_type_code > 9 {
        return Err(Error::corrupt("slice_type > 9"));
    }
    if matches!(slice_type_code % 5, 3 | 4) {
        return Err(Error::unsupported(
            "SP and SI slices",
            "switching slices need the transform-domain reconstruction of \
             clause 8.5.14 and their own deblocking strengths; not implemented",
        ));
    }
    Ok(())
}

/// Mutable per-slice decode state.
struct SliceCtx {
    slice_id: u16,
    slice_type: SliceType,
    qp: i32,
    cb_qp_offset: i32,
    cr_qp_offset: i32,
    transform_8x8_mode: bool,
    constrained_intra_pred: bool,
    /// ctxIdxInc for the next macroblock's first mb_qp_delta bin: set when the
    /// macroblock just decoded coded a non-zero mb_qp_delta (9.3.3.1.1.5).
    qp_delta_inc: u8,
    /// `RefPicList0` and `RefPicList1` as decoded picture buffer indices.
    lists: [RefList; 2],
    num_ref_idx: [usize; 2],
    direct_spatial: bool,
    direct_8x8_inference: bool,
    weighted_pred: bool,
    weighted_bipred_idc: u8,
    weights: Option<PredWeightTable>,
    /// `PicOrderCnt( CurrPic )`.
    poc: i32,
}

impl SliceCtx {
    fn is_b(&self) -> bool {
        self.slice_type == SliceType::B
    }

    /// The reference picture `RefPicListX[idx]` points at.
    fn reference<'a>(&self, refs: &'a [Picture], list: usize, idx: i8) -> Option<&'a Picture> {
        if idx < 0 {
            return None;
        }
        refs.get(self.lists[list].get(idx as usize)?)
    }

    /// Prediction weights of one component (clause 8.4.3).
    fn weights_for(
        &self,
        refs: &[Picture],
        comp: usize,
        r0: i8,
        r1: i8,
        use0: bool,
        use1: bool,
    ) -> Weights {
        let implicit = self.weighted_bipred_idc == 2 && self.is_b() && use0 && use1;
        let explicit =
            (self.weighted_bipred_idc == 1 && self.is_b()) || (self.weighted_pred && !self.is_b());
        if implicit {
            let (Some(p0), Some(p1)) = (self.reference(refs, 0, r0), self.reference(refs, 1, r1))
            else {
                return Weights::DEFAULT;
            };
            let td = (p1.poc - p0.poc).clamp(-128, 127);
            let long_term = p0.mark == Mark::Long || p1.mark == Mark::Long;
            if td == 0 || long_term {
                return Weights {
                    log_wd: 5,
                    w: [32, 32],
                    o: [0, 0],
                };
            }
            let tb = (self.poc - p0.poc).clamp(-128, 127);
            let tx = (16384 + (td / 2).abs()) / td;
            let dist = ((tb * tx + 32) >> 6).clamp(-1024, 1023);
            let w1 = dist >> 2;
            if !(-64..=128).contains(&w1) {
                return Weights {
                    log_wd: 5,
                    w: [32, 32],
                    o: [0, 0],
                };
            }
            return Weights {
                log_wd: 5,
                w: [64 - w1, w1],
                o: [0, 0],
            };
        }
        if !explicit {
            return Weights::DEFAULT;
        }
        let Some(t) = &self.weights else {
            return Weights::DEFAULT;
        };
        let denom = if comp == 0 {
            t.luma_log2_weight_denom
        } else {
            t.chroma_log2_weight_denom
        } as i32;
        let pick = |list: &[WeightEntry], idx: i8| -> (i32, i32) {
            let default = (1 << denom, 0);
            if idx < 0 {
                return default;
            }
            match list.get(idx as usize) {
                None => default,
                Some(e) if comp == 0 => e.luma.unwrap_or(default),
                Some(e) => e.chroma.map(|c| c[comp - 1]).unwrap_or(default),
            }
        };
        let (w0, o0) = pick(&t.l0, r0);
        let (w1, o1) = pick(&t.l1, r1);
        Weights {
            log_wd: denom,
            w: [w0, w1],
            o: [o0, o1],
        }
    }
}

/// Neighbour-availability snapshot for one macroblock (left, top, top-right,
/// top-left). The top-left neighbour is only read by the Intra_8x8 reference
/// filter of 8.3.2.2.1, which changes its end taps when the corner sample is
/// missing.
#[derive(Clone, Copy)]
pub(crate) struct MbNeighbors {
    pub a: bool,
    pub b: bool,
    pub c: bool,
    pub d: bool,
}

pub(crate) fn mb_neighbors(pic: &Picture, mb_x: usize, mb_y: usize, slice_id: u16) -> MbNeighbors {
    let w = pic.mb_w;
    let addr = mb_y * w + mb_x;
    let same = |a: usize| pic.mb_slice[a] == slice_id;
    MbNeighbors {
        a: mb_x > 0 && same(addr - 1),
        b: mb_y > 0 && same(addr - w),
        c: mb_y > 0 && mb_x + 1 < w && same(addr - w + 1),
        d: mb_y > 0 && mb_x > 0 && same(addr - w - 1),
    }
}

/// The same, with the constrained intra prediction rule of 7.4.2.2 applied:
/// an inter-coded neighbour is not available as an intra prediction source.
fn intra_neighbors(pic: &Picture, mb_x: usize, mb_y: usize, n: MbNeighbors) -> MbNeighbors {
    let w = pic.mb_w;
    let addr = mb_y * w + mb_x;
    let intra = |a: usize| pic.mb_flags[a] & FLAG_INTER == 0;
    MbNeighbors {
        a: n.a && intra(addr - 1),
        b: n.b && intra(addr - w),
        c: n.c && intra(addr - w + 1),
        d: n.d && intra(addr - w - 1),
    }
}

/// Non-zero counts of the left and above neighbours of the luma 4x4 block at
/// global block coords `(bx, by)`, `None` when that neighbour is unavailable
/// (clause 6.4.11.4 plus the availability rules of 6.4.8).
#[inline]
pub(crate) fn luma_nz_pair(
    pic: &Picture,
    n: &MbNeighbors,
    bx: usize,
    by: usize,
) -> (Option<u8>, Option<u8>) {
    let w4 = pic.mb_w * 4;
    let left_avail = if bx.is_multiple_of(4) { n.a } else { true };
    let top_avail = if by.is_multiple_of(4) { n.b } else { true };
    (
        (left_avail && bx > 0).then(|| pic.nz_y[by * w4 + bx - 1]),
        (top_avail && by > 0).then(|| pic.nz_y[(by - 1) * w4 + bx]),
    )
}

/// The same for a chroma AC 4x4 block at global chroma block coords.
#[inline]
pub(crate) fn chroma_nz_pair(
    pic: &Picture,
    n: &MbNeighbors,
    comp: usize,
    cx: usize,
    cy: usize,
) -> (Option<u8>, Option<u8>) {
    let w2 = pic.mb_w * 2;
    let grid = &pic.nz_c[comp];
    let left_avail = if cx.is_multiple_of(2) { n.a } else { true };
    let top_avail = if cy.is_multiple_of(2) { n.b } else { true };
    (
        (left_avail && cx > 0).then(|| grid[cy * w2 + cx - 1]),
        (top_avail && cy > 0).then(|| grid[(cy - 1) * w2 + cx]),
    )
}

/// Motion data parsed for one macroblock, before prediction derivation.
#[derive(Clone, Copy)]
struct MbMotion {
    /// Sub-macroblock types of a P_8x8 / B_8x8 macroblock.
    sub: [SubShape; 4],
    /// `ref_idx_lX[ mbPartIdx ]`, -1 when the partition does not use the list.
    ref_idx: [[i8; 4]; 2],
    /// `mvd_lX[ mbPartIdx ][ subMbPartIdx ]`, indexed by 4x4 raster position.
    mvd: [[[i16; 2]; 16]; 2],
}

/// Decode one macroblock (spec 7.3.5, 7.4.5) and reconstruct it. `ctx.qp`
/// carries the QPY prediction chain.
// Block loops are indexed: `blk` is simultaneously a bitstream position, a
// geometry key (BLK4_POS) and a grid coordinate; iterators obscure that.
#[allow(clippy::needless_range_loop, clippy::too_many_arguments)]
fn decode_macroblock(
    pic: &mut Picture,
    refs: &[Picture],
    ls: &[[LevelScale4x4; 3]; 2],
    ls8: &[LevelScale8x8; 2],
    r: &mut Entropy<'_>,
    ctx: &mut SliceCtx,
    mb_addr: usize,
    skipped: bool,
) -> Result<()> {
    let mb_x = mb_addr % pic.mb_w;
    let mb_y = mb_addr / pic.mb_w;
    if pic.mb_slice[mb_addr] != NO_SLICE {
        return Err(Error::corrupt("macroblock decoded twice"));
    }
    let nbr = mb_neighbors(pic, mb_x, mb_y, ctx.slice_id);
    if !skipped || !r.is_cabac() {
        let info = |addr: usize| MbInfo {
            flags: pic.mb_flags[addr],
            cbp: pic.mb_cbp[addr],
            dc_cbf: pic.mb_dc_cbf[addr],
        };
        r.begin_mb(&MbCtx {
            a: nbr.a.then(|| info(mb_addr - 1)),
            b: nbr.b.then(|| info(mb_addr - pic.mb_w)),
            qp_delta_inc: ctx.qp_delta_inc,
        });
    }
    let w4 = pic.mb_w * 4;
    let w2 = pic.mb_w * 2;
    let (bx0, by0) = (mb_x * 4, mb_y * 4);
    let (cx0, cy0) = (mb_x * 2, mb_y * 2);

    // Every 4x4 block of this macroblock starts from a known state: the arrays
    // survive from the previous picture, and both the CABAC contexts and the
    // neighbour derivations read them for blocks of the macroblock in progress.
    for py in 0..4 {
        let base = (by0 + py) * w4 + bx0;
        for idx in base..base + 4 {
            pic.mv[idx] = [[0; 2]; 2];
            pic.ref_idx[idx] = [-1; 2];
            pic.ref_id[idx] = [-1; 2];
            pic.mvd_abs[idx] = [0; 4];
            pic.blk[idx] = 0;
        }
    }

    let mb_type = if skipped {
        u32::MAX
    } else if ctx.slice_type == SliceType::P {
        r.mb_type_p()?
    } else if ctx.is_b() {
        r.mb_type_b()?
    } else {
        r.mb_type_i()?
    };

    // Split the P/B numbering into an inter shape or an intra mb_type.
    let (shape, intra_type) = if skipped {
        let s = if ctx.is_b() { B_SHAPES[0] } else { P_SHAPES[0] };
        (Some(s), None)
    } else if ctx.slice_type == SliceType::P {
        if mb_type < 5 {
            (Some(P_SHAPES[mb_type as usize]), None)
        } else {
            (None, Some(mb_type - 5))
        }
    } else if ctx.is_b() {
        if mb_type < 23 {
            (Some(B_SHAPES[mb_type as usize]), None)
        } else {
            (None, Some(mb_type - 23))
        }
    } else {
        (None, Some(mb_type))
    };
    r.set_intra(intra_type.is_some());
    if let Some(shape) = shape {
        return decode_inter_mb(
            pic, refs, ls, ls8, r, ctx, mb_addr, shape, skipped, mb_type, nbr,
        );
    }
    let mb_type = intra_type.expect("an intra macroblock type");
    write_intra_mb(pic, mb_x, mb_y);
    let intra_nbr = if ctx.constrained_intra_pred {
        intra_neighbors(pic, mb_x, mb_y, nbr)
    } else {
        nbr
    };

    if mb_type == 25 {
        // I_PCM (7.3.5, 8.3.5): raw samples, byte aligned.
        let pcm = r.pcm_block()?;
        for y in 0..16 {
            let dst = pic.y.at(mb_x * 16, mb_y * 16 + y);
            pic.y.data[dst..dst + 16].copy_from_slice(&pcm[y * 16..y * 16 + 16]);
        }
        for (c, plane) in [&mut pic.cb, &mut pic.cr].into_iter().enumerate() {
            let base = 256 + c * 64;
            for y in 0..8 {
                let dst = plane.at(mb_x * 8, mb_y * 8 + y);
                plane.data[dst..dst + 8].copy_from_slice(&pcm[base + y * 8..base + y * 8 + 8]);
            }
        }
        for dy in 0..4 {
            let base = (by0 + dy) * w4 + bx0;
            pic.nz_y[base..base + 4].fill(16);
            pic.i4_modes[base..base + 4].fill(2);
        }
        for comp in 0..2 {
            for dy in 0..2 {
                let base = (cy0 + dy) * w2 + cx0;
                pic.nz_c[comp][base..base + 2].fill(16);
            }
        }
        // QPY prediction chain continues unchanged (mb_qp_delta inferred 0);
        // the deblocker substitutes 0 via FLAG_PCM (spec 8.7.2).
        pic.mb_qp[mb_addr] = ctx.qp as u8;
        pic.mb_flags[mb_addr] = FLAG_DECODED | FLAG_PCM;
        pic.mb_cbp[mb_addr] = 0x3F;
        pic.mb_dc_cbf[mb_addr] = 0b111;
        ctx.qp_delta_inc = 0;
        pic.mb_slice[mb_addr] = ctx.slice_id;
        pic.decoded_mbs += 1;
        return Ok(());
    }

    let is_i16 = mb_type >= 1;
    let (cbp_luma, cbp_chroma, i16_mode) = if is_i16 {
        let m = mb_type - 1;
        (
            if m >= 12 { 15u8 } else { 0 },
            ((m / 4) % 3) as u8,
            (m % 4) as u8,
        )
    } else {
        (0, 0, 0) // read below via me(v)
    };

    // I_NxN transform size flag (7.3.5, High profile only): it selects
    // Intra_8x8 prediction and the 8x8 transform for this macroblock.
    let trans8x8 = !is_i16 && ctx.transform_8x8_mode && r.transform_size_8x8_flag()?;

    // mb_pred (7.3.5.1): intra 4x4 or 8x8 modes, then chroma mode.
    let mut modes = [2u8; 16];
    let mut modes8 = [2u8; 4];
    if trans8x8 {
        // Intra_8x8: four modes, predicted by 8.3.2.1 from the neighbouring
        // block modes. Every 4x4 slot of an 8x8 block carries its mode, which
        // is exactly what 8.3.1.1 and 8.3.2.1 read back from a neighbour
        // whichever size that neighbour used.
        for blk8 in 0..4 {
            let (bx, by) = (bx0 + (blk8 % 2) * 2, by0 + (blk8 / 2) * 2);
            let left_avail = if blk8.is_multiple_of(2) {
                intra_nbr.a
            } else {
                true
            };
            let top_avail = if blk8 < 2 { intra_nbr.b } else { true };
            let pred = if left_avail && top_avail {
                let ma = pic.i4_modes[by * w4 + bx - 1];
                let mb = pic.i4_modes[(by - 1) * w4 + bx];
                ma.min(mb)
            } else {
                2
            };
            let mode = match r.intra4x4_pred_mode()? {
                None => pred,
                Some(rem) if rem < pred => rem,
                Some(rem) => rem + 1,
            };
            modes8[blk8] = mode;
            for dy in 0..2 {
                let base = (by + dy) * w4 + bx;
                pic.i4_modes[base..base + 2].fill(mode);
            }
        }
    } else if !is_i16 {
        for blk in 0..16 {
            let (dx, dy) = BLK4_POS[blk];
            let (bx, by) = (bx0 + dx as usize, by0 + dy as usize);
            // Predicted mode = min of neighbours, DC when either unavailable
            // (8.3.1.1; non-I4x4 neighbours were written as 2).
            let left_avail = if bx % 4 == 0 { intra_nbr.a } else { true };
            let top_avail = if by % 4 == 0 { intra_nbr.b } else { true };
            let pred = if left_avail && top_avail {
                let ma = pic.i4_modes[by * w4 + bx - 1];
                let mb = pic.i4_modes[(by - 1) * w4 + bx];
                ma.min(mb)
            } else {
                2
            };
            let mode = match r.intra4x4_pred_mode()? {
                None => pred,
                Some(rem) if rem < pred => rem,
                Some(rem) => rem + 1,
            };
            pic.i4_modes[by * w4 + bx] = mode;
            modes[blk] = mode;
        }
    } else {
        for dy in 0..4 {
            let base = (by0 + dy) * w4 + bx0;
            pic.i4_modes[base..base + 4].fill(2);
        }
    }
    let chroma_mode = r.intra_chroma_pred_mode()?;

    // coded_block_pattern for I_NxN; Intra_16x16 derives it from mb_type.
    let (cbp_luma, cbp_chroma) = if is_i16 {
        (cbp_luma, cbp_chroma)
    } else {
        r.coded_block_pattern_intra()?
    };

    finish_macroblock(
        pic,
        &ls[0],
        &ls8[0],
        r,
        ctx,
        mb_addr,
        &nbr,
        MbFinish {
            pred_nbr: intra_nbr,
            is_i16,
            i16_mode,
            cbp_luma,
            cbp_chroma,
            modes,
            modes8,
            trans8x8,
            chroma_mode,
            intra: true,
            flags: if is_i16 { FLAG_I16 } else { 0 } | if trans8x8 { FLAG_TRANS8X8 } else { 0 },
        },
    )
}

/// Everything the shared residual path needs after prediction is decided.
struct MbFinish {
    /// Neighbour availability for *prediction*, which honours
    /// constrained_intra_pred_flag. The residual contexts use the plain
    /// availability instead — clause 9.2.1 and 9.3.3.1.1.9 only exclude an
    /// inter neighbour there when slice data partitioning is in use, and this
    /// decoder refuses partitioned slices outright.
    pred_nbr: MbNeighbors,
    is_i16: bool,
    i16_mode: u8,
    cbp_luma: u8,
    cbp_chroma: u8,
    modes: [u8; 16],
    /// Intra8x8PredMode per 8x8 block, when `trans8x8` and `intra`.
    modes8: [u8; 4],
    /// transform_size_8x8_flag: the luma residual is four 8x8 transform blocks.
    trans8x8: bool,
    chroma_mode: u8,
    intra: bool,
    flags: u8,
}

/// The residual half of a macroblock (7.3.5.3 + 8.5), shared by the intra and
/// inter paths.
///
/// Bitstream order (luma blocks in Z-order, then chroma DC, then chroma AC)
/// equals reconstruction dependency order, so each block reconstructs the
/// moment its levels are parsed: no per-macroblock coefficient arrays, and
/// empty blocks cost one coeff_token read plus a prediction write. For an inter
/// macroblock the prediction is already in the plane, so the intra branches
/// below are simply skipped.
#[allow(clippy::needless_range_loop, clippy::too_many_arguments)]
fn finish_macroblock(
    pic: &mut Picture,
    ls: &[LevelScale4x4; 3],
    ls8: &LevelScale8x8,
    r: &mut Entropy<'_>,
    ctx: &mut SliceCtx,
    mb_addr: usize,
    nbr: &MbNeighbors,
    m: MbFinish,
) -> Result<()> {
    let mb_x = mb_addr % pic.mb_w;
    let mb_y = mb_addr / pic.mb_w;
    let w4 = pic.mb_w * 4;
    let w2 = pic.mb_w * 2;
    let (bx0, by0) = (mb_x * 4, mb_y * 4);
    let (cx0, cy0) = (mb_x * 2, mb_y * 2);
    let (cbp_luma, cbp_chroma) = (m.cbp_luma, m.cbp_chroma);

    // mb_qp_delta (7.4.5).
    let mut qp_delta = 0;
    if cbp_luma != 0 || cbp_chroma != 0 || m.is_i16 {
        qp_delta = r.mb_qp_delta()?;
        ctx.qp = (ctx.qp + qp_delta + 52) % 52;
    }
    ctx.qp_delta_inc = u8::from(qp_delta != 0);
    pic.mb_qp[mb_addr] = ctx.qp as u8;
    pic.mb_flags[mb_addr] = FLAG_DECODED
        | m.flags
        | if !m.intra { FLAG_INTER } else { 0 }
        | if m.intra && m.chroma_mode != 0 {
            FLAG_CHROMA_PRED
        } else {
            0
        };
    pic.mb_cbp[mb_addr] = cbp_luma | (cbp_chroma << 4);
    pic.mb_slice[mb_addr] = ctx.slice_id;
    pic.decoded_mbs += 1;
    let qp_y = ctx.qp;
    let qp_cb = i32::from(CHROMA_QP[(qp_y + ctx.cb_qp_offset).clamp(0, 51) as usize]);
    let qp_cr = i32::from(CHROMA_QP[(qp_y + ctx.cr_qp_offset).clamp(0, 51) as usize]);

    let mut dc_cbf = 0u8;
    let mut scan = [0i32; 16];
    let mut raster = [0i32; 16];
    let mut resid = [0i32; 16];
    if m.is_i16 {
        let mut w = PlaneWindow {
            data: &mut pic.y.data,
            stride: pic.y.stride,
            origin: pic.y.origin + (mb_y * 16) * pic.y.stride + mb_x * 16,
        };
        pred_16x16(m.i16_mode, &mut w, m.pred_nbr.b, m.pred_nbr.a);
        // Luma DC: un-zigzag over the 4x4 DC array, Hadamard + scale.
        let (na, nb) = luma_nz_pair(pic, nbr, bx0, by0);
        let dc_tc = r.residual_block(&mut scan, BlockCat::LumaDc, na, nb)?;
        dc_cbf |= u8::from(dc_tc != 0);
        unzigzag(&scan, &mut raster);
        luma_dc_transform(&mut raster, &ls[0], qp_y);
        let dc = raster;
        for blk in 0..16 {
            let (dx, dy) = BLK4_POS[blk];
            let (bx, by) = (bx0 + dx as usize, by0 + dy as usize);
            let tc = if cbp_luma != 0 {
                let (na, nb) = luma_nz_pair(pic, nbr, bx, by);
                r.residual_block(&mut scan, BlockCat::LumaAc, na, nb)?
            } else {
                0
            };
            pic.nz_y[by * w4 + bx] = tc;
            let dc_blk = dc[dy as usize * 4 + dx as usize];
            if tc == 0 && dc_blk == 0 {
                continue; // prediction already in place
            }
            if tc > 0 {
                unzigzag_ac15(&scan, &mut raster);
                dequant_4x4(&mut raster, &ls[0], qp_y, true);
            } else {
                raster = [0; 16]; // DC-only block
            }
            raster[0] = dc_blk;
            inverse_transform_4x4(&raster, &mut resid);
            let origin = pic
                .y
                .at(mb_x * 16 + dx as usize * 4, mb_y * 16 + dy as usize * 4);
            add_residual_4x4(&mut pic.y.data, pic.y.stride, origin, &resid);
        }
    } else if m.trans8x8 {
        // 8x8 transform (8.5.13): predict, parse and reconstruct one 8x8 block
        // at a time, because an Intra_8x8 block predicts from the reconstructed
        // samples of the blocks before it.
        let mut scan8 = [0i32; 64];
        let mut raster8 = [0i32; 64];
        let mut resid8 = [0i32; 64];
        for blk8 in 0..4 {
            let (x, y) = (mb_x * 16 + (blk8 % 2) * 8, mb_y * 16 + (blk8 / 2) * 8);
            let (bx, by) = (bx0 + (blk8 % 2) * 2, by0 + (blk8 / 2) * 2);
            let origin = pic.y.at(x, y);
            let stride = pic.y.stride;
            if m.intra {
                let n = filter_nbr8(&gather_nbr8(pic, &m.pred_nbr, blk8, x, y));
                let mut p = [0u8; 64];
                pred_8x8(m.modes8[blk8], &n, &mut p);
                for ry in 0..8 {
                    let row = origin + ry * stride;
                    pic.y.data[row..row + 8].copy_from_slice(&p[ry * 8..ry * 8 + 8]);
                }
            }
            if cbp_luma & (1 << blk8) == 0 {
                for dy in 0..2 {
                    let base = (by + dy) * w4 + bx;
                    pic.nz_y[base..base + 2].fill(0);
                }
                continue;
            }
            let tc = if r.is_cabac() {
                // ctxBlockCat 5: one 64-coefficient block, and every 4x4 slot
                // of it reports the block's own count to the neighbours
                // (9.3.3.1.1.9 resolves them to the 8x8 transform block).
                let tc = r.residual_block_8x8(&mut scan8)?;
                for dy in 0..2 {
                    let base = (by + dy) * w4 + bx;
                    pic.nz_y[base..base + 2].fill(tc.min(16));
                }
                tc
            } else {
                // CAVLC codes the same 64 coefficients as four interleaved 4x4
                // blocks (7.3.5.3.1), each with its own nC neighbourhood.
                let mut tc = 0u8;
                for i4 in 0..4 {
                    let (dx, dy) = BLK4_POS[blk8 * 4 + i4];
                    let (sx, sy) = (bx0 + dx as usize, by0 + dy as usize);
                    let (na, nb) = luma_nz_pair(pic, nbr, sx, sy);
                    let n = r.residual_block(&mut scan, BlockCat::Luma4x4, na, nb)?;
                    pic.nz_y[sy * w4 + sx] = n;
                    tc += n;
                    for i in 0..16 {
                        scan8[4 * i + i4] = scan[i];
                    }
                }
                tc
            };
            if tc == 0 {
                continue;
            }
            unzigzag_8x8(&scan8, &mut raster8);
            dequant_8x8(&mut raster8, ls8, qp_y);
            inverse_transform_8x8(&raster8, &mut resid8);
            add_residual_8x8(&mut pic.y.data, stride, origin, &resid8);
        }
    } else {
        for blk in 0..16 {
            let (dx, dy) = BLK4_POS[blk];
            let (bx, by) = (bx0 + dx as usize, by0 + dy as usize);
            let tc = if cbp_luma & (1 << (blk >> 2)) != 0 {
                let (na, nb) = luma_nz_pair(pic, nbr, bx, by);
                r.residual_block(&mut scan, BlockCat::Luma4x4, na, nb)?
            } else {
                0
            };
            pic.nz_y[by * w4 + bx] = tc;
            let (x, y) = (mb_x * 16 + dx as usize * 4, mb_y * 16 + dy as usize * 4);
            let origin = pic.y.at(x, y);
            let stride = pic.y.stride;
            if m.intra {
                let n = gather_nbr4(pic, &m.pred_nbr, blk, x, y);
                let mut p = [0u8; 16];
                pred_4x4(m.modes[blk], &n, &mut p);
                if tc == 0 {
                    // Pure prediction: copy the 16 samples in.
                    for ry in 0..4 {
                        let row = origin + ry * stride;
                        pic.y.data[row..row + 4].copy_from_slice(&p[ry * 4..ry * 4 + 4]);
                    }
                    continue;
                }
                unzigzag(&scan, &mut raster);
                dequant_4x4(&mut raster, &ls[0], qp_y, false);
                inverse_transform_4x4(&raster, &mut resid);
                for ry in 0..4 {
                    let row = origin + ry * stride;
                    for rx in 0..4 {
                        let v = i32::from(p[ry * 4 + rx]) + resid[ry * 4 + rx];
                        pic.y.data[row + rx] = v.clamp(0, 255) as u8;
                    }
                }
            } else {
                if tc == 0 {
                    continue; // motion-compensated prediction already in place
                }
                unzigzag(&scan, &mut raster);
                dequant_4x4(&mut raster, &ls[0], qp_y, false);
                inverse_transform_4x4(&raster, &mut resid);
                add_residual_4x4(&mut pic.y.data, stride, origin, &resid);
            }
        }
    }

    // Chroma: predict both components (intra only), then DC (Cb, Cr), then AC
    // in bitstream order, reconstructing as levels arrive.
    if m.intra {
        for comp in 0..2 {
            let plane = if comp == 0 { &mut pic.cb } else { &mut pic.cr };
            let mut w = PlaneWindow {
                data: &mut plane.data,
                stride: plane.stride,
                origin: plane.origin + (mb_y * 8) * plane.stride + mb_x * 8,
            };
            pred_chroma_8x8(m.chroma_mode, &mut w, m.pred_nbr.b, m.pred_nbr.a);
        }
    }
    let mut chroma_dc = [[0i32; 4]; 2];
    if cbp_chroma != 0 {
        for (comp, c) in chroma_dc.iter_mut().enumerate() {
            let tc = r.residual_block(&mut scan, BlockCat::ChromaDc(comp as u8), None, None)?;
            dc_cbf |= u8::from(tc != 0) << (1 + comp);
            c.copy_from_slice(&scan[..4]);
            let (ls_c, qp_c) = if comp == 0 {
                (&ls[1], qp_cb)
            } else {
                (&ls[2], qp_cr)
            };
            chroma_dc_transform_420(c, ls_c, qp_c);
        }
    }
    for comp in 0..2 {
        for blk in 0..4 {
            let (cx, cy) = (cx0 + (blk & 1), cy0 + (blk >> 1));
            let tc = if cbp_chroma == 2 {
                let (na, nb) = chroma_nz_pair(pic, nbr, comp, cx, cy);
                r.residual_block(&mut scan, BlockCat::ChromaAc, na, nb)?
            } else {
                0
            };
            pic.nz_c[comp][cy * w2 + cx] = tc;
            if cbp_chroma == 0 {
                continue;
            }
            let dc_blk = chroma_dc[comp][blk];
            if tc == 0 && dc_blk == 0 {
                continue;
            }
            let (plane, ls_c, qp_c) = if comp == 0 {
                (&mut pic.cb, &ls[1], qp_cb)
            } else {
                (&mut pic.cr, &ls[2], qp_cr)
            };
            if tc > 0 {
                unzigzag_ac15(&scan, &mut raster);
                dequant_4x4(&mut raster, ls_c, qp_c, true);
            } else {
                raster = [0; 16]; // DC-only block
            }
            raster[0] = dc_blk;
            inverse_transform_4x4(&raster, &mut resid);
            let origin = plane.at(mb_x * 8 + (blk & 1) * 4, mb_y * 8 + (blk >> 1) * 4);
            add_residual_4x4(&mut plane.data, plane.stride, origin, &resid);
        }
    }
    pic.mb_dc_cbf[mb_addr] = dc_cbf;
    Ok(())
}

/// Decode an inter macroblock: sub-macroblock types, reference indices and
/// motion vector differences (7.3.5.1, 7.3.5.2), then motion compensation
/// (8.4.2) and the shared residual path.
// `comp` indexes a motion vector component and selects its context offset at
// the same time, which an iterator would obscure.
#[allow(clippy::too_many_arguments, clippy::needless_range_loop)]
fn decode_inter_mb(
    pic: &mut Picture,
    refs: &[Picture],
    ls: &[[LevelScale4x4; 3]; 2],
    ls8: &[LevelScale8x8; 2],
    r: &mut Entropy<'_>,
    ctx: &mut SliceCtx,
    mb_addr: usize,
    shape: MbShape,
    skipped: bool,
    mb_type: u32,
    nbr: MbNeighbors,
) -> Result<()> {
    let mb_x = mb_addr % pic.mb_w;
    let mb_y = mb_addr / pic.mb_w;
    let b = ctx.is_b();
    let mut mvc = MvCtx {
        mb_x,
        mb_y,
        slice_id: ctx.slice_id,
        written: 0,
    };
    let mut motion = MbMotion {
        sub: [P_SUB[0]; 4],
        ref_idx: [[-1; 4]; 2],
        mvd: [[[0; 2]; 16]; 2],
    };
    // A direct or skipped block contributes nothing to the ref_idx and mvd
    // contexts of its neighbours (9.3.3.1.1.6, 9.3.3.1.1.7), so the flag has to
    // be in place before any of this macroblock's syntax is parsed.
    let direct_mb = skipped || (b && mb_type == 0);
    if direct_mb {
        let f = BLK_DIRECT | if skipped { BLK_SKIP } else { 0 };
        for py in 0..4 {
            let base = (mb_y * 4 + py) * pic.mb_w * 4 + mb_x * 4;
            for idx in base..base + 4 {
                pic.blk[idx] = f;
            }
        }
    }

    if shape.sub && !skipped {
        for part in 0..4 {
            let t = r.sub_mb_type(b)? as usize;
            motion.sub[part] = if b { B_SUB[t] } else { P_SUB[t] };
            if motion.sub[part].pred == Pred::Direct {
                for k in 0..4 {
                    let (px, py) = (part % 2 * 2 + k % 2, part / 2 * 2 + k / 2);
                    let idx = (mb_y * 4 + py) * pic.mb_w * 4 + mb_x * 4 + px;
                    pic.blk[idx] = BLK_DIRECT;
                }
            }
        }
    }

    // ---- ref_idx_lX (7.3.5.1, 7.3.5.2) ----
    // P_8x8ref0 infers reference index 0 for every partition.
    let infer_ref0 = ctx.slice_type == SliceType::P && mb_type == 4;
    if !skipped {
        for list in 0..2 {
            for part in 0..shape.parts {
                let uses = if shape.sub {
                    motion.sub[part].pred.uses(list)
                } else {
                    shape.pred[part.min(1)].uses(list)
                };
                if !uses {
                    continue;
                }
                let idx = if infer_ref0 || ctx.num_ref_idx[list] <= 1 {
                    0
                } else {
                    let (px, py) = part_origin(&shape, part, (0, 0));
                    let bx = (mb_x * 4 + px) as i32;
                    let by = (mb_y * 4 + py) as i32;
                    let inc = ref_idx_cond(pic, &mvc, bx - 1, by, list)
                        + 2 * ref_idx_cond(pic, &mvc, bx, by - 1, list);
                    r.ref_idx(ctx.num_ref_idx[list] as u32 - 1, inc)? as i8
                };
                motion.ref_idx[list][part] = idx;
                // Publish it over the whole macroblock partition — one
                // reference index covers an entire 8x8, however it is further
                // split — so the next partition's context (9.3.3.1.1.6) and
                // the mvd contexts of 9.3.3.1.1.7 see it everywhere.
                let (px, py) = part_origin(&shape, part, (0, 0));
                let (pw, ph) = mb_part_size(&shape);
                for dy in 0..ph {
                    for dx in 0..pw {
                        let bi = (mb_y * 4 + py + dy) * pic.mb_w * 4 + mb_x * 4 + px + dx;
                        pic.ref_idx[bi][list] = idx;
                        mvc.written |= 1 << ((py + dy) * 4 + px + dx);
                    }
                }
            }
        }
        if infer_ref0 {
            for part in 0..4 {
                motion.ref_idx[0][part] = 0;
            }
        }

        // ---- mvd_lX ----
        mvc.written = 0;
        for list in 0..2 {
            for part in 0..shape.parts {
                let uses = if shape.sub {
                    motion.sub[part].pred.uses(list)
                } else {
                    shape.pred[part.min(1)].uses(list)
                };
                if !uses {
                    continue;
                }
                let subparts = if shape.sub { motion.sub[part].parts } else { 1 };
                for sp in 0..subparts {
                    let (px, py) = part_origin(&shape, part, sp_offset(&shape, &motion, part, sp));
                    let (pw, ph) = part_size(&shape, &motion, part);
                    let bx = (mb_x * 4 + px) as i32;
                    let by = (mb_y * 4 + py) as i32;
                    let mut mvd = [0i16; 2];
                    for comp in 0..2 {
                        let a = neighbour_mvd(pic, &mvc, bx - 1, by, list)[comp];
                        let bb = neighbour_mvd(pic, &mvc, bx, by - 1, list)[comp];
                        let sum = a + bb;
                        let inc = if sum > 32 {
                            2
                        } else if sum > 2 {
                            1
                        } else {
                            0
                        };
                        let v = r.mvd(comp, inc)?;
                        mvd[comp] = i16::try_from(v)
                            .map_err(|_| Error::corrupt("mvd outside the level limits"))?;
                    }
                    for dy in 0..ph {
                        for dx in 0..pw {
                            motion.mvd[list][(py + dy) * 4 + px + dx] = mvd;
                            write_mvd(pic, mb_x, mb_y, px + dx, py + dy, list, mvd);
                            mvc.written |= 1 << ((py + dy) * 4 + px + dx);
                        }
                    }
                }
            }
        }
    }

    // ---- motion vector derivation and compensation (8.4.1, 8.4.2) ----
    mvc.written = 0;
    if direct_mb && !b {
        predict_p_skip(pic, refs, ctx, &mut mvc)?;
    } else if direct_mb {
        predict_direct(pic, refs, ctx, &mut mvc, true)?;
    } else {
        for part in 0..shape.parts {
            if shape.sub && motion.sub[part].pred == Pred::Direct {
                predict_direct_8x8(pic, refs, ctx, &mut mvc, part)?;
                continue;
            }
            let subparts = if shape.sub { motion.sub[part].parts } else { 1 };
            let pred = if shape.sub {
                motion.sub[part].pred
            } else {
                shape.pred[part.min(1)]
            };
            for sp in 0..subparts {
                let (px, py) = part_origin(&shape, part, sp_offset(&shape, &motion, part, sp));
                let (pw, ph) = part_size(&shape, &motion, part);
                let mut mv = [[0i16; 2]; 2];
                let mut ref_idx = [-1i8; 2];
                let mut ref_id = [-1i32; 2];
                for list in 0..2 {
                    if !pred.uses(list) {
                        continue;
                    }
                    let idx = motion.ref_idx[list][part];
                    // predPartWidth of 6.4.11.7: the macroblock partition width
                    // for a whole-macroblock partition, the sub-macroblock
                    // partition width inside an 8x8.
                    let n = mvc.neighbours(pic, px, py, pw, list);
                    let mvp = predict_mv(&n, idx, shape.w, shape.h, part);
                    let d = motion.mvd[list][py * 4 + px];
                    mv[list] = [mvp[0].wrapping_add(d[0]), mvp[1].wrapping_add(d[1])];
                    ref_idx[list] = idx;
                    ref_id[list] = ctx.reference(refs, list, idx).map(|p| p.id).unwrap_or(-1);
                }
                for dy in 0..ph {
                    for dx in 0..pw {
                        write_block(pic, &mut mvc, px + dx, py + dy, mv, ref_idx, ref_id, 0);
                    }
                }
                compensate(pic, refs, ctx, mb_x, mb_y, px, py, pw, ph, mv, ref_idx)?;
            }
        }
    }

    if skipped {
        // 7.4.5: a skipped macroblock has no residual and does not change QPY.
        let w4 = pic.mb_w * 4;
        let w2 = pic.mb_w * 2;
        for dy in 0..4 {
            let base = (mb_y * 4 + dy) * w4 + mb_x * 4;
            pic.nz_y[base..base + 4].fill(0);
            pic.i4_modes[base..base + 4].fill(2);
        }
        for comp in 0..2 {
            for dy in 0..2 {
                let base = (mb_y * 2 + dy) * w2 + mb_x * 2;
                pic.nz_c[comp][base..base + 2].fill(0);
            }
        }
        pic.mb_qp[mb_addr] = ctx.qp as u8;
        pic.mb_flags[mb_addr] =
            FLAG_DECODED | FLAG_INTER | FLAG_SKIP | if b { FLAG_DIRECT } else { 0 };
        pic.mb_cbp[mb_addr] = 0;
        pic.mb_dc_cbf[mb_addr] = 0;
        pic.mb_slice[mb_addr] = ctx.slice_id;
        pic.decoded_mbs += 1;
        ctx.qp_delta_inc = 0;
        return Ok(());
    }

    let w4 = pic.mb_w * 4;
    for dy in 0..4 {
        let base = (mb_y * 4 + dy) * w4 + mb_x * 4;
        pic.i4_modes[base..base + 4].fill(2);
    }
    let (cbp_luma, cbp_chroma) = r.coded_block_pattern_inter()?;
    // transform_size_8x8_flag (7.3.5): only when every 8x8 of this macroblock
    // is predicted as a whole, because an 8x8 transform block may not straddle
    // two motion partitions.
    let no_sub_lt_8x8 = !shape.sub
        || (0..4).all(|p| {
            let s = motion.sub[p];
            if s.pred == Pred::Direct {
                ctx.direct_8x8_inference
            } else {
                s.parts == 1
            }
        });
    let trans8x8 = cbp_luma != 0
        && ctx.transform_8x8_mode
        && no_sub_lt_8x8
        && (!direct_mb || ctx.direct_8x8_inference)
        && r.transform_size_8x8_flag()?;
    // The inter scaling lists (Table 7-2 lists 3..5) govern an inter residual.
    finish_macroblock(
        pic,
        &ls[1],
        &ls8[1],
        r,
        ctx,
        mb_addr,
        &nbr,
        MbFinish {
            pred_nbr: nbr,
            is_i16: false,
            i16_mode: 0,
            cbp_luma,
            cbp_chroma,
            modes: [2; 16],
            modes8: [2; 4],
            trans8x8,
            chroma_mode: 0,
            intra: false,
            flags: if b && mb_type == 0 { FLAG_DIRECT } else { 0 }
                | if trans8x8 { FLAG_TRANS8X8 } else { 0 },
        },
    )
}

/// Top-left 4x4 block of macroblock partition `part`, offset by `sp` blocks in
/// raster order within the 8x8.
fn part_origin(shape: &MbShape, part: usize, sp: (usize, usize)) -> (usize, usize) {
    let (px, py) = if shape.sub {
        (part % 2 * 2, part / 2 * 2)
    } else if shape.parts == 1 {
        (0, 0)
    } else if shape.w == 4 {
        (0, part * 2) // 16x8
    } else {
        (part * 2, 0) // 8x16
    };
    (px + sp.0, py + sp.1)
}

/// Block offset of sub-macroblock partition `sp` inside its 8x8.
fn sp_offset(shape: &MbShape, m: &MbMotion, part: usize, sp: usize) -> (usize, usize) {
    if !shape.sub {
        return (0, 0);
    }
    let s = m.sub[part];
    match (s.w, s.h) {
        (2, 2) => (0, 0),
        (2, 1) => (0, sp),
        (1, 2) => (sp, 0),
        _ => (sp % 2, sp / 2),
    }
}

/// Size of one *macroblock* partition in 4x4 blocks: always 8x8 for a
/// macroblock that carries sub-macroblock types, however those split it.
fn mb_part_size(shape: &MbShape) -> (usize, usize) {
    if shape.sub {
        (2, 2)
    } else {
        (shape.w, shape.h)
    }
}

/// Size of one (sub-)macroblock partition in 4x4 blocks.
fn part_size(shape: &MbShape, m: &MbMotion, part: usize) -> (usize, usize) {
    if shape.sub {
        let s = m.sub[part];
        (s.w, s.h)
    } else {
        (shape.w, shape.h)
    }
}

/// P_Skip (clause 8.4.1.1).
fn predict_p_skip(
    pic: &mut Picture,
    refs: &[Picture],
    ctx: &SliceCtx,
    mvc: &mut MvCtx,
) -> Result<()> {
    let n = mvc.neighbours(pic, 0, 0, 4, 0);
    let zero = !n[0].avail
        || !n[1].avail
        || (n[0].ref_idx == 0 && n[0].mv == [0, 0])
        || (n[1].ref_idx == 0 && n[1].mv == [0, 0]);
    let mv = if zero {
        [0i16; 2]
    } else {
        predict_mv(&n, 0, 4, 4, 0)
    };
    let ref_id = ctx.reference(refs, 0, 0).map(|p| p.id).unwrap_or(-1);
    for py in 0..4 {
        for px in 0..4 {
            write_block(
                pic,
                mvc,
                px,
                py,
                [mv, [0; 2]],
                [0, -1],
                [ref_id, -1],
                BLK_SKIP | BLK_DIRECT,
            );
        }
    }
    compensate(
        pic,
        refs,
        ctx,
        mvc.mb_x,
        mvc.mb_y,
        0,
        0,
        4,
        4,
        [mv, [0; 2]],
        [0, -1],
    )
}

/// B_Skip / B_Direct_16x16 (clause 8.4.1.2) over the whole macroblock.
fn predict_direct(
    pic: &mut Picture,
    refs: &[Picture],
    ctx: &SliceCtx,
    mvc: &mut MvCtx,
    skip: bool,
) -> Result<()> {
    for quad in 0..4 {
        direct_quadrant(pic, refs, ctx, mvc, quad, skip)?;
    }
    Ok(())
}

/// B_Direct_8x8 for one 8x8 sub-macroblock.
fn predict_direct_8x8(
    pic: &mut Picture,
    refs: &[Picture],
    ctx: &SliceCtx,
    mvc: &mut MvCtx,
    part: usize,
) -> Result<()> {
    direct_quadrant(pic, refs, ctx, mvc, part, false)
}

/// One 8x8 quadrant of a direct-mode macroblock. With
/// `direct_8x8_inference_flag` the whole quadrant shares the motion of its
/// outer corner 4x4 block; without it each 4x4 block is derived on its own.
fn direct_quadrant(
    pic: &mut Picture,
    refs: &[Picture],
    ctx: &SliceCtx,
    mvc: &mut MvCtx,
    quad: usize,
    skip: bool,
) -> Result<()> {
    let (mb_x, mb_y) = (mvc.mb_x, mvc.mb_y);
    let (qx, qy) = (quad % 2 * 2, quad / 2 * 2);
    let flags = BLK_DIRECT | if skip { BLK_SKIP } else { 0 };
    if ctx.direct_8x8_inference {
        // 8.4.1.2.1: luma4x4BlkIdx = 5 * mbPartIdx, i.e. the quadrant's own
        // outer corner of the macroblock.
        let corner = (quad % 2 * 3, quad / 2 * 3);
        let (mv, ref_idx, ref_id) = direct_block(pic, refs, ctx, mvc, corner)?;
        for dy in 0..2 {
            for dx in 0..2 {
                write_block(pic, mvc, qx + dx, qy + dy, mv, ref_idx, ref_id, flags);
            }
        }
        compensate(pic, refs, ctx, mb_x, mb_y, qx, qy, 2, 2, mv, ref_idx)
    } else {
        for dy in 0..2 {
            for dx in 0..2 {
                let (px, py) = (qx + dx, qy + dy);
                let (mv, ref_idx, ref_id) = direct_block(pic, refs, ctx, mvc, (px, py))?;
                write_block(pic, mvc, px, py, mv, ref_idx, ref_id, flags);
                compensate(pic, refs, ctx, mb_x, mb_y, px, py, 1, 1, mv, ref_idx)?;
            }
        }
        Ok(())
    }
}

/// Motion of one direct-mode 4x4 block: `(mvLX, refIdxLX, reference picture
/// identity)`, spatial (8.4.1.2.2) or temporal (8.4.1.2.3).
#[allow(clippy::type_complexity)]
fn direct_block(
    pic: &Picture,
    refs: &[Picture],
    ctx: &SliceCtx,
    mvc: &MvCtx,
    at: (usize, usize),
) -> Result<([[i16; 2]; 2], [i8; 2], [i32; 2])> {
    // The co-located block, in both modes (8.4.1.2.1): its list 0 motion, or
    // its list 1 motion when list 0 is unused. `col_ref_idx` is refIdxCol —
    // an index into the co-located picture's own reference list, not ours.
    let col = ctx.lists[1].get(0).and_then(|i| refs.get(i));
    let (col_ref_id, col_mv, col_ref_idx) = match col {
        Some(c) => {
            let bx = mvc.mb_x * 4 + at.0;
            let by = mvc.mb_y * 4 + at.1;
            let idx = by * c.mb_w.max(1) * 4 + bx;
            if idx < c.ref_id.len() && c.blk[idx] & BLK_INTRA == 0 {
                let list = usize::from(c.ref_id[idx][0] < 0);
                (c.ref_id[idx][list], c.mv[idx][list], c.ref_idx[idx][list])
            } else {
                (-1, [0i16; 2], -1)
            }
        }
        None => (-1, [0i16; 2], -1),
    };

    if ctx.direct_spatial {
        // Reference indices from the macroblock's own neighbours, once.
        let mut ref_idx = [-1i8; 2];
        let mut mvp = [[0i16; 2]; 2];
        for list in 0..2 {
            let n = mvc.neighbours(pic, 0, 0, 4, list);
            ref_idx[list] = min_positive(n[0].ref_idx, min_positive(n[1].ref_idx, n[2].ref_idx));
            mvp[list] = predict_mv(&n, ref_idx[list], 4, 4, 0);
        }
        let zero_prediction = ref_idx[0] < 0 && ref_idx[1] < 0;
        if zero_prediction {
            ref_idx = [0, 0];
        }
        // colZeroFlag (8.4.1.2.2): the co-located block sits still on a
        // short-term picture, referencing that picture's own list entry 0. The
        // index is the one belonging to the list 8.4.1.2.1 picked — reading
        // "either list is 0" instead lets a bi-predicted co-located block whose
        // list 1 index happens to be 0 zero a motion vector it must not, which
        // only shows on a B picture used as the co-located one.
        let col_short = col.is_some_and(|c| c.mark == Mark::Short);
        let col_zero = col_short
            && col_ref_id >= 0
            && col_ref_idx == 0
            && (-1..=1).contains(&col_mv[0])
            && (-1..=1).contains(&col_mv[1]);
        let mut mv = [[0i16; 2]; 2];
        let mut ref_id = [-1i32; 2];
        for list in 0..2 {
            if ref_idx[list] < 0 {
                continue;
            }
            ref_id[list] = ctx
                .reference(refs, list, ref_idx[list])
                .map(|p| p.id)
                .unwrap_or(-1);
            if zero_prediction || (ref_idx[list] == 0 && col_zero) {
                mv[list] = [0; 2];
            } else {
                mv[list] = mvp[list];
            }
        }
        return Ok((mv, ref_idx, ref_id));
    }

    // Temporal direct (8.4.1.2.3).
    let ref_idx0 = if col_ref_id < 0 {
        0
    } else {
        map_col_to_list0(ctx, refs, col_ref_id)
    };
    let pic0 = ctx.reference(refs, 0, ref_idx0);
    let pic1 = ctx.reference(refs, 1, 0);
    let ref_id = [
        pic0.map(|p| p.id).unwrap_or(-1),
        pic1.map(|p| p.id).unwrap_or(-1),
    ];
    let (Some(p0), Some(p1)) = (pic0, pic1) else {
        return Ok(([[0; 2]; 2], [ref_idx0, 0], ref_id));
    };
    let td = (p1.poc - p0.poc).clamp(-128, 127);
    if p0.mark == Mark::Long || td == 0 {
        return Ok(([col_mv, [0; 2]], [ref_idx0, 0], ref_id));
    }
    let tb = (ctx.poc - p0.poc).clamp(-128, 127);
    let tx = (16384 + (td / 2).abs()) / td;
    let dist = ((tb * tx + 32) >> 6).clamp(-1024, 1023);
    let mv0 = [
        ((dist * i32::from(col_mv[0]) + 128) >> 8) as i16,
        ((dist * i32::from(col_mv[1]) + 128) >> 8) as i16,
    ];
    let mv1 = [mv0[0] - col_mv[0], mv0[1] - col_mv[1]];
    Ok(([mv0, mv1], [ref_idx0, 0], ref_id))
}

/// `MapColToList0` (8.4.1.2.3): the lowest RefPicList0 index that names the
/// picture the co-located block referenced.
fn map_col_to_list0(ctx: &SliceCtx, refs: &[Picture], col_ref_id: i32) -> i8 {
    for i in 0..ctx.lists[0].len() {
        if ctx.lists[0]
            .get(i)
            .and_then(|k| refs.get(k))
            .is_some_and(|p| p.id == col_ref_id)
        {
            return i as i8;
        }
    }
    0
}

/// Motion compensation of one partition (clause 8.4.2): interpolate each list's
/// prediction, then combine them by the weighting of 8.4.2.3 and write the
/// result straight into the picture, where the residual is added on top.
///
/// `(px, py)` and `(pw, ph)` are in 4x4 blocks within the macroblock.
#[allow(clippy::too_many_arguments)]
fn compensate(
    pic: &mut Picture,
    refs: &[Picture],
    ctx: &SliceCtx,
    mb_x: usize,
    mb_y: usize,
    px: usize,
    py: usize,
    pw: usize,
    ph: usize,
    mv: [[i16; 2]; 2],
    ref_idx: [i8; 2],
) -> Result<()> {
    let use0 = ref_idx[0] >= 0;
    let use1 = ref_idx[1] >= 0;
    if !use0 && !use1 {
        return Ok(());
    }
    let x0 = (mb_x * 16 + px * 4) as i32;
    let y0 = (mb_y * 16 + py * 4) as i32;
    let (w, h) = (pw * 4, ph * 4);

    let wt = ctx.weights_for(refs, 0, ref_idx[0], ref_idx[1], use0, use1);
    // 8.4.2.3.1 over one list is the identity, so the interpolation of that one
    // list IS the prediction: it is written straight into the picture at the
    // picture's own pitch, rather than into a temporary that is combined into a
    // second temporary and copied a third time.
    let direct = wt.is_default() && (use0 != use1);
    let mut part = [[0u8; 256]; 2];
    for list in 0..2 {
        if ref_idx[list] < 0 {
            continue;
        }
        let rp = ctx.reference(refs, list, ref_idx[list]).ok_or_else(|| {
            Error::corrupt("reference index names a picture the buffer does not hold")
        })?;
        let plane = RefPlane {
            data: &rp.y.data,
            stride: rp.y.stride,
            origin: rp.y.origin,
            width: rp.y.width,
            height: rp.y.height,
            pad: rp.y.pad,
        };
        if direct {
            let stride = pic.y.stride;
            let origin = pic.y.at(x0 as usize, y0 as usize);
            mc_luma(
                &plane,
                x0,
                y0,
                mv[list],
                w,
                h,
                stride,
                &mut pic.y.data[origin..],
            );
        } else {
            mc_luma(&plane, x0, y0, mv[list], w, h, w, &mut part[list]);
        }
    }
    if !direct {
        let mut out = [0u8; 256];
        let (p0, p1) = part.split_at(1);
        combine(&mut out, &p0[0], &p1[0], use0, use1, &wt, w * h);
        let stride = pic.y.stride;
        let origin = pic.y.at(x0 as usize, y0 as usize);
        for row in 0..h {
            let dst = origin + row * stride;
            pic.y.data[dst..dst + w].copy_from_slice(&out[row * w..row * w + w]);
        }
    }

    // Chroma (8.4.1.4): for 4:2:0 frame coding the chroma vector is the luma
    // vector, read as eighth-sample units over the half-resolution plane.
    let (cw, ch) = (w / 2, h / 2);
    let (cx0, cy0) = (x0 / 2, y0 / 2);
    for comp in 0..2 {
        let wt = ctx.weights_for(refs, comp + 1, ref_idx[0], ref_idx[1], use0, use1);
        let direct = wt.is_default() && (use0 != use1);
        let mut cpart = [[0u8; 64]; 2];
        for list in 0..2 {
            if ref_idx[list] < 0 {
                continue;
            }
            let rp = ctx
                .reference(refs, list, ref_idx[list])
                .expect("checked above");
            let src = if comp == 0 { &rp.cb } else { &rp.cr };
            let plane = RefPlane {
                data: &src.data,
                stride: src.stride,
                origin: src.origin,
                width: src.width,
                height: src.height,
                pad: src.pad,
            };
            if direct {
                let dst = if comp == 0 { &mut pic.cb } else { &mut pic.cr };
                let stride = dst.stride;
                let origin = dst.at(cx0 as usize, cy0 as usize);
                mc_chroma(
                    &plane,
                    cx0,
                    cy0,
                    mv[list],
                    cw,
                    ch,
                    stride,
                    &mut dst.data[origin..],
                );
            } else {
                mc_chroma(&plane, cx0, cy0, mv[list], cw, ch, cw, &mut cpart[list]);
            }
        }
        if !direct {
            let mut cout = [0u8; 64];
            let (c0, c1) = cpart.split_at(1);
            combine(&mut cout, &c0[0], &c1[0], use0, use1, &wt, cw * ch);
            let plane = if comp == 0 { &mut pic.cb } else { &mut pic.cr };
            let stride = plane.stride;
            let origin = plane.at(cx0 as usize, cy0 as usize);
            for row in 0..ch {
                let dst = origin + row * stride;
                plane.data[dst..dst + cw].copy_from_slice(&cout[row * cw..row * cw + cw]);
            }
        }
    }
    Ok(())
}

/// Gather the 13 neighbour samples of a luma 4x4 block (spec 8.3.1.2),
/// including the top-right substitution rule.
pub(crate) fn gather_nbr4(
    pic: &Picture,
    nbr: &MbNeighbors,
    blk: usize,
    x: usize,
    y: usize,
) -> Nbr4 {
    let (dx, dy) = BLK4_POS[blk];
    let stride = pic.y.stride;
    let o = pic.y.at(x, y);
    let have_left = if dx == 0 { nbr.a } else { true };
    let have_top = if dy == 0 { nbr.b } else { true };
    let have_tr = match TR_KIND[blk] {
        TrKind::InMb => true,
        TrKind::B => nbr.b,
        TrKind::C => nbr.c,
        TrKind::None => false,
    };
    let mut top = [0u8; 8];
    let data = &pic.y.data;
    top[..4].copy_from_slice(&data[o - stride..o - stride + 4]);
    if have_tr {
        top[4..].copy_from_slice(&data[o - stride + 4..o - stride + 8]);
    } else if have_top {
        let t3 = top[3];
        top[4..].fill(t3);
    }
    let mut left = [0u8; 4];
    for (i, l) in left.iter_mut().enumerate() {
        *l = data[o + i * stride - 1];
    }
    Nbr4 {
        top,
        left,
        top_left: data[o - stride - 1],
        have_top,
        have_left,
    }
}

/// Gather the 25 neighbour samples of an Intra_8x8 luma block (spec 8.3.2.2),
/// including the top-right substitution rule, unfiltered.
///
/// `blk8` is luma8x8BlkIdx: block 1 reaches the above-right macroblock for its
/// top-right run, block 2 finds it inside this macroblock, and block 3 has none
/// (the samples right of it are not decoded yet).
fn gather_nbr8(pic: &Picture, nbr: &MbNeighbors, blk8: usize, x: usize, y: usize) -> Nbr8 {
    let stride = pic.y.stride;
    let o = pic.y.at(x, y);
    let data = &pic.y.data;
    let have_top = if blk8 < 2 { nbr.b } else { true };
    let have_left = if blk8.is_multiple_of(2) { nbr.a } else { true };
    let have_tr = match blk8 {
        0 => nbr.b,
        1 => nbr.c,
        2 => true,
        _ => false,
    };
    let have_tl = match blk8 {
        0 => nbr.d,
        1 => nbr.b,
        2 => nbr.a,
        _ => true,
    };
    let mut top = [0u8; 16];
    top[..8].copy_from_slice(&data[o - stride..o - stride + 8]);
    if have_tr {
        top[8..].copy_from_slice(&data[o - stride + 8..o - stride + 16]);
    } else {
        let t7 = top[7];
        top[8..].fill(t7);
    }
    let mut left = [0u8; 8];
    for (i, l) in left.iter_mut().enumerate() {
        *l = data[o + i * stride - 1];
    }
    Nbr8 {
        top,
        left,
        top_left: data[o - stride - 1],
        have_top,
        have_left,
        have_tl,
    }
}

/// Whole-picture deblocking (spec 8.7): macroblocks in raster order, all
/// vertical edges then all horizontal edges per macroblock, then chroma.
// `e` and `j` are edge and segment positions inside the macroblock, used to
// index the plane as well as the strength arrays; iterating the arrays would
// hide which geometry each one names.
#[allow(clippy::needless_range_loop)]
pub(crate) fn deblock_picture(pic: &mut Picture) {
    let mb_w = pic.mb_w;
    for mb_y in 0..pic.mb_h {
        for mb_x in 0..mb_w {
            let addr = mb_y * mb_w + mb_x;
            if pic.mb_flags[addr] & FLAG_DECODED == 0 {
                continue;
            }
            let sid = pic.mb_slice[addr];
            let sp = pic.slices[sid as usize];
            if sp.disable_deblock_idc == 1 {
                continue;
            }
            let decoded = |a: usize| pic.mb_flags[a] & FLAG_DECODED != 0;
            let cross_ok = |a: usize| sp.disable_deblock_idc != 2 || pic.mb_slice[a] == sid;
            let filter_left = mb_x > 0 && decoded(addr - 1) && cross_ok(addr - 1);
            let filter_top = mb_y > 0 && decoded(addr - mb_w) && cross_ok(addr - mb_w);
            // Deblocking QP: 0 for I_PCM macroblocks (8.7.2).
            let qp_of = |a: usize| -> i32 {
                if pic.mb_flags[a] & FLAG_PCM != 0 {
                    0
                } else {
                    i32::from(pic.mb_qp[a])
                }
            };
            let qp_q = qp_of(addr);
            // 8.7: an 8x8-transform macroblock has no transform edge at luma
            // 4 or 12, so those internal edges are not filtered. Chroma is
            // unaffected for 4:2:0 (its internal edge sits on luma edge 8).
            let internal =
                |e: usize| e.is_multiple_of(2) || pic.mb_flags[addr] & FLAG_TRANS8X8 == 0;

            // Boundary strengths, per 4-sample segment of each edge (8.7.2.1).
            //
            // Both sides of an internal edge live in this macroblock, so the
            // whole 8.7.2.1 ladder is decided once for all 24 of them: an intra
            // macroblock gives 3 everywhere, and an inter macroblock with no
            // luma coefficients and one motion vector over the whole 16x16
            // gives 0 everywhere. Only the two macroblock edges then need the
            // per-segment derivation.
            let mut bs_v = [[0u8; 4]; 4];
            let mut bs_h = [[0u8; 4]; 4];
            let cur_intra = pic.mb_flags[addr] & FLAG_INTER == 0;
            let internal_uniform = if cur_intra {
                Some(3)
            } else if pic.mb_cbp[addr] == 0 && uniform_motion(pic, mb_x, mb_y) {
                Some(0)
            } else {
                None
            };
            for k in 0..4 {
                if filter_left {
                    bs_v[0][k] = boundary_strength(pic, mb_x, mb_y, 0, k, true);
                }
                if filter_top {
                    bs_h[0][k] = boundary_strength(pic, mb_x, mb_y, 0, k, false);
                }
            }
            for e in 1..4 {
                match internal_uniform {
                    Some(bs) => {
                        bs_v[e] = [bs; 4];
                        bs_h[e] = [bs; 4];
                    }
                    None => {
                        for k in 0..4 {
                            bs_v[e][k] = boundary_strength(pic, mb_x, mb_y, e, k, true);
                            bs_h[e][k] = boundary_strength(pic, mb_x, mb_y, e, k, false);
                        }
                    }
                }
            }

            // ---- luma ----
            {
                let stride = pic.y.stride;
                let base = pic.y.at(mb_x * 16, mb_y * 16);
                // Vertical edges (filter across columns), left to right.
                for e in 0..4 {
                    if e == 0 && !filter_left {
                        continue;
                    }
                    if !internal(e) {
                        continue;
                    }
                    let qp_p = if e == 0 { qp_of(addr - 1) } else { qp_q };
                    let qp_avg = (qp_p + qp_q + 1) >> 1;
                    // Vertical edges stay scalar: a gather/scatter SIMD
                    // variant measured slower than this loop (strided
                    // per-lane loads dominate).
                    for j in 0..4 {
                        let bs = bs_v[e][j];
                        if bs == 0 {
                            continue;
                        }
                        let params = edge_params(qp_avg, sp.alpha_offset, sp.beta_offset, bs);
                        if params.alpha == 0 || params.beta == 0 {
                            continue;
                        }
                        for k in 0..4 {
                            let row = j * 4 + k;
                            filter_luma_line(
                                &mut pic.y.data,
                                base + row * stride + e * 4,
                                1,
                                &params,
                            );
                        }
                    }
                }
                // Horizontal edges, top to bottom.
                for e in 0..4 {
                    if e == 0 && !filter_top {
                        continue;
                    }
                    if !internal(e) {
                        continue;
                    }
                    let qp_p = if e == 0 { qp_of(addr - mb_w) } else { qp_q };
                    let qp_avg = (qp_p + qp_q + 1) >> 1;
                    let bs = bs_h[e];
                    let uniform = bs[0] == bs[1] && bs[1] == bs[2] && bs[2] == bs[3];
                    if uniform {
                        if bs[0] == 0 {
                            continue;
                        }
                        let params = edge_params(qp_avg, sp.alpha_offset, sp.beta_offset, bs[0]);
                        if params.alpha == 0 || params.beta == 0 {
                            continue;
                        }
                        // Eight rows around the edge are contiguous, so the
                        // whole 16-sample edge filters in one pass.
                        filter_luma_h_edge16(
                            &mut pic.y.data,
                            base + e * 4 * stride,
                            stride,
                            &params,
                        );
                        continue;
                    }
                    for j in 0..4 {
                        if bs[j] == 0 {
                            continue;
                        }
                        let params = edge_params(qp_avg, sp.alpha_offset, sp.beta_offset, bs[j]);
                        if params.alpha == 0 || params.beta == 0 {
                            continue;
                        }
                        for k in 0..4 {
                            filter_luma_line(
                                &mut pic.y.data,
                                base + e * 4 * stride + j * 4 + k,
                                stride,
                                &params,
                            );
                        }
                    }
                }
            }

            // ---- chroma (Cb, Cr) ----
            for comp in 0..2 {
                let (plane, off_sel): (&mut Plane8, fn(&SliceParams) -> i32) = if comp == 0 {
                    (&mut pic.cb, |s| s.cb_qp_offset)
                } else {
                    (&mut pic.cr, |s| s.cr_qp_offset)
                };
                let stride = plane.stride;
                let base = plane.origin + (mb_y * 8) * stride + mb_x * 8;
                // Chroma QP per side: each macroblock's luma QP mapped with
                // the offset of ITS OWN slice's PPS (8.7.2).
                let cqp = |a: usize| -> i32 {
                    let s = pic.slices[pic.mb_slice[a] as usize];
                    if pic.mb_flags[a] & FLAG_PCM != 0 {
                        chroma_qp(0, off_sel(&s))
                    } else {
                        chroma_qp(i32::from(pic.mb_qp[a]), off_sel(&s))
                    }
                };
                let cq_q = cqp(addr);
                // A chroma edge inherits the boundary strength of the luma edge
                // it sits on: chroma sample k maps to luma sample 2k, so a
                // chroma 4-sample run spans two luma 4x4 block rows.
                for e in 0..2 {
                    if e == 0 && !filter_left {
                        continue;
                    }
                    let cq_p = if e == 0 { cqp(addr - 1) } else { cq_q };
                    let qp_avg = (cq_p + cq_q + 1) >> 1;
                    for k in 0..8 {
                        let bs = bs_v[e * 2][k / 2];
                        if bs == 0 {
                            continue;
                        }
                        let params = edge_params(qp_avg, sp.alpha_offset, sp.beta_offset, bs);
                        if params.alpha == 0 || params.beta == 0 {
                            continue;
                        }
                        filter_chroma_line(&mut plane.data, base + k * stride + e * 4, 1, &params);
                    }
                }
                for e in 0..2 {
                    if e == 0 && !filter_top {
                        continue;
                    }
                    let cq_p = if e == 0 { cqp(addr - mb_w) } else { cq_q };
                    let qp_avg = (cq_p + cq_q + 1) >> 1;
                    for k in 0..8 {
                        let bs = bs_h[e * 2][k / 2];
                        if bs == 0 {
                            continue;
                        }
                        let params = edge_params(qp_avg, sp.alpha_offset, sp.beta_offset, bs);
                        if params.alpha == 0 || params.beta == 0 {
                            continue;
                        }
                        filter_chroma_line(
                            &mut plane.data,
                            base + e * 4 * stride + k,
                            stride,
                            &params,
                        );
                    }
                }
            }
        }
    }
}

/// True when every 4x4 block of this macroblock predicts from the same
/// pictures with the same motion, so no internal edge can reach the motion
/// clauses of 8.7.2.1.
fn uniform_motion(pic: &Picture, mb_x: usize, mb_y: usize) -> bool {
    let w4 = pic.mb_w * 4;
    let first = mb_y * 4 * w4 + mb_x * 4;
    let (mv, id) = (pic.mv[first], pic.ref_id[first]);
    (0..4).all(|by| {
        let row = first + by * w4;
        (0..4).all(|bx| pic.mv[row + bx] == mv && pic.ref_id[row + bx] == id)
    })
}

/// Boundary strength of one 4-sample luma edge segment (clause 8.7.2.1).
///
/// `edge` 0..4 selects the vertical (`vertical`) or horizontal edge inside the
/// macroblock, `seg` the 4-sample run along it. Edge 0 is the macroblock edge.
fn boundary_strength(
    pic: &Picture,
    mb_x: usize,
    mb_y: usize,
    edge: usize,
    seg: usize,
    vertical: bool,
) -> u8 {
    let w4 = pic.mb_w * 4;
    let (qbx, qby) = if vertical {
        (mb_x * 4 + edge, mb_y * 4 + seg)
    } else {
        (mb_x * 4 + seg, mb_y * 4 + edge)
    };
    let (pbx, pby) = if vertical {
        (qbx - 1, qby)
    } else {
        (qbx, qby - 1)
    };
    let q = qby * w4 + qbx;
    let p = pby * w4 + pbx;
    let mb_edge = edge == 0;
    let q_mb = (qby / 4) * pic.mb_w + qbx / 4;
    let p_mb = (pby / 4) * pic.mb_w + pbx / 4;
    let intra = pic.mb_flags[q_mb] & FLAG_INTER == 0 || pic.mb_flags[p_mb] & FLAG_INTER == 0;
    if intra {
        return if mb_edge { 4 } else { 3 };
    }
    // "the transform block containing the sample has non-zero levels": for an
    // 8x8-transform macroblock that is the whole 8x8, whose four 4x4 slots
    // carry per-slot counts under CAVLC (7.3.5.3.1 splits it into four blocks).
    let coded = |mb: usize, bx: usize, by: usize| -> bool {
        if pic.mb_flags[mb] & FLAG_TRANS8X8 == 0 {
            return pic.nz_y[by * w4 + bx] != 0;
        }
        let (ox, oy) = (bx & !1, by & !1);
        pic.nz_y[oy * w4 + ox] != 0
            || pic.nz_y[oy * w4 + ox + 1] != 0
            || pic.nz_y[(oy + 1) * w4 + ox] != 0
            || pic.nz_y[(oy + 1) * w4 + ox + 1] != 0
    };
    if coded(p_mb, pbx, pby) || coded(q_mb, qbx, qby) {
        return 2;
    }
    let (pr, qr) = (pic.ref_id[p], pic.ref_id[q]);
    let pn = u8::from(pr[0] >= 0) + u8::from(pr[1] >= 0);
    let qn = u8::from(qr[0] >= 0) + u8::from(qr[1] >= 0);
    if pn != qn {
        return 1;
    }
    let (pm, qm) = (pic.mv[p], pic.mv[q]);
    let differs = |a: [i16; 2], b: [i16; 2]| {
        (i32::from(a[0]) - i32::from(b[0])).abs() >= 4
            || (i32::from(a[1]) - i32::from(b[1])).abs() >= 4
    };
    if pn == 1 {
        let pl = usize::from(pr[0] < 0);
        let ql = usize::from(qr[0] < 0);
        if pr[pl] != qr[ql] {
            return 1;
        }
        return u8::from(differs(pm[pl], qm[ql]));
    }
    if pn == 0 {
        return 0;
    }
    // Two motion vectors each: compare the two partitions by which pictures
    // they reference, not by list position (NOTE 1 of 8.7.2.1).
    let same_set = (pr[0] == qr[0] && pr[1] == qr[1]) || (pr[0] == qr[1] && pr[1] == qr[0]);
    if !same_set {
        return 1;
    }
    if pr[0] != pr[1] {
        return if pr[0] == qr[0] {
            u8::from(differs(pm[0], qm[0]) || differs(pm[1], qm[1]))
        } else {
            u8::from(differs(pm[0], qm[1]) || differs(pm[1], qm[0]))
        };
    }
    // Both lists name the same picture: either pairing agreeing is enough.
    let straight = !differs(pm[0], qm[0]) && !differs(pm[1], qm[1]);
    let crossed = !differs(pm[0], qm[1]) && !differs(pm[1], qm[0]);
    u8::from(!(straight || crossed))
}

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
use ec_core::frame::{Interlace, PixelFormat, Plane, VideoFrame};
use ec_core::timebase::Timestamp;
use ec_h264_syntax::{
    AnnexBIter, DecRefPicMarking, NalHeader, NalUnitType, Pps, PredWeightTable, SliceHeader,
    SliceType, Sps, WeightEntry, unescape_rbsp,
};

use crate::cabac::BitAccount;
use crate::deblock::{chroma_qp, edge_params, filter_chroma_line, filter_luma_line};
use crate::dpb::{
    BLK_DIRECT, BLK_INTRA, BLK_SKIP, BOTTOM, CODED_AFRM, CODED_FLD, Dpb, FRAME, Mark, NO_SLICE,
    PicInfo, PicPlane, Picture, RefList, SliceParams, TOP,
};
use crate::entropy::{
    BlockCat, Entropy, FLAG_CHROMA_PRED, FLAG_DECODED, FLAG_DIRECT, FLAG_I16, FLAG_INTER, FLAG_PCM,
    FLAG_SKIP, FLAG_TRANS8X8, MbCtx, MbInfo, MbaffA,
};
use crate::inter::{RefPlane, Weights, combine, mc_chroma, mc_luma};
use crate::mbaff::{self, MbPos};
use crate::mv::{
    B_SHAPES, B_SUB, MbShape, MvCtx, P_SHAPES, P_SUB, Pred, SubShape, min_positive, neighbour_mvd,
    predict_mv, ref_idx_cond, write_block, write_intra_mb, write_mvd,
};
use crate::pred::{
    Nbr4, Nbr8, PlaneWindow, add_residual_4x4, add_residual_8x8, add_residual_dpcm, filter_nbr8,
    pred_4x4, pred_8x8, pred_16x16, pred_chroma_8x8_halves, pred_chroma_8x16,
};
use crate::sample::Sample;
use crate::tables::BLK4_POS;
use crate::transform::{
    CHROMA_DC_422_RASTER, LevelScale4x4, LevelScale8x8, chroma_dc_transform_420,
    chroma_dc_transform_422, dequant_4x4, dequant_8x8, inverse_transform_4x4,
    inverse_transform_8x8, luma_dc_transform, unfield, unfield_8x8, unfield_ac15, unzigzag,
    unzigzag_8x8, unzigzag_ac15,
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

/// The decode core, generic over the sample type of the pictures it holds.
pub(crate) struct Core<S: Sample> {
    sps_map: Vec<Option<Sps>>,
    pps_map: Vec<Option<Pps>>,
    rbsp: Vec<u8>,
    /// The picture being decoded, held out of the buffer pool.
    cur: Picture<S>,
    dpb: Dpb<S>,
    has_picture: bool,
    /// Identity of the open picture, from its first slice's header.
    cur_key: Option<PicKey>,
    /// `first_mb_in_slice` of the most recent slice of the open picture.
    /// Scaling factors of the active PPS/SPS, `[intra, inter][Y, Cb, Cr]`.
    ls: [[LevelScale4x4; 3]; 2],
    /// The same for the 8x8 transform, `[intra, inter][Y, Cb, Cr]` (only 4:4:4
    /// has an 8x8 chroma transform).
    ls8: [[LevelScale8x8; 3]; 2],
    /// Which colour plane this core decodes when the stream codes the three
    /// planes separately (`separate_colour_plane_flag`): 0 Y, 1 Cb, 2 Cr. The
    /// scaling lists of that plane stand in for the luma ones (8.5.9).
    plane_id: u8,
    /// Accept sample depths up to 14 bits ([`Decoder::set_wide_samples`]).
    wide: bool,
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
    /// Bits parsed so far, by syntax class (CABAC slices only).
    account: BitAccount,
    shapes: MbShapes,
    /// Presentation timestamp to attach to the next picture started.
    pending_pts: Option<Timestamp>,
    /// `PicOrderCnt` of the random-access point most recently established by
    /// [`Decoder::reset_pictures`] — the first picture decoded after it,
    /// i.e. the I picture a seek landed on. `None` before that picture
    /// completes. Absent a recovery_point SEI (not parsed here), D.2.8's
    /// recovery point defaults to this picture itself: anything with a
    /// smaller POC that had to guess a reference (see `cur_ref_padded`)
    /// predicts from something never decoded since the seek and must not
    /// reach the caller.
    recovery_poc: Option<i32>,
    /// Set while decoding the open picture if `build_ref_lists` had to pad a
    /// reference list, i.e. this picture asked for a reference the DPB does
    /// not have. Reset per picture in `start_picture`.
    cur_ref_padded: bool,
}

/// Streaming H.264 decoder (monochrome, 4:2:0, 4:2:2 and 4:4:4, 8 to 10
/// bits, with lossless transform bypass; unsupported features come back as
/// named [`Error::Unsupported`] values, never wrong pixels).
///
/// 8-bit streams decode into 8-bit planes and come out as `I420` (`I422` for
/// 4:2:2, `I444` for 4:4:4); 9- and 10-bit streams decode into 16-bit planes
/// and come out as `I010` (`I210`, `I410`): 16-bit little-endian samples, the
/// formats the HEVC Main 10 decoder produces. Monochrome leaves as 4:2:0 with
/// neutral chroma, and a stream with `separate_colour_plane_flag` as 4:4:4,
/// its three colour planes decoded as independent monochrome sequences. The
/// choice of sample width is made per slice from the bit depth of the active
/// SPS.
pub struct Decoder {
    c8: Core<u8>,
    c16: Core<u16>,
    /// Cores for colour planes 1 and 2 of a `separate_colour_plane_flag`
    /// stream: each colour plane is an independent monochrome picture
    /// sequence, merged into one 4:4:4 frame on output.
    x8: [Core<u8>; 2],
    x16: [Core<u16>; 2],
    /// The core the open picture belongs to.
    hbd: bool,
}

impl Default for Decoder {
    fn default() -> Self {
        Decoder::new()
    }
}

impl Decoder {
    /// A fresh decoder with no parameter sets.
    pub fn new() -> Decoder {
        let mut d = Decoder {
            c8: Core::new(),
            c16: Core::new(),
            x8: [Core::new(), Core::new()],
            x16: [Core::new(), Core::new()],
            hbd: false,
        };
        for i in 0..2 {
            d.x8[i].plane_id = i as u8 + 1;
            d.x16[i].plane_id = i as u8 + 1;
        }
        d
    }

    /// Accept High 4:4:4 Predictive streams of up to 14 bits per sample (the
    /// JVT conformance streams use 14). Frames then carry the samples at their
    /// native depth in 16-bit little-endian words inside `I410`/`I010`
    /// containers, which only describe 10 bits, so this is for conformance
    /// tooling and off by default: without it deeper streams are refused.
    pub fn set_wide_samples(&mut self, wide: bool) {
        self.c8.wide = wide;
        self.c16.wide = wide;
        for i in 0..2 {
            self.x8[i].wide = wide;
            self.x16[i].wide = wide;
        }
    }

    /// Choose between display order (the default) and decode order.
    pub fn set_output_order(&mut self, order: OutputOrder) {
        self.c8.set_output_order(order);
        self.c16.set_output_order(order);
        for i in 0..2 {
            self.x8[i].set_output_order(order);
            self.x16[i].set_output_order(order);
        }
    }

    /// The order pictures are handed back in.
    pub fn output_order(&self) -> OutputOrder {
        self.c8.output_order()
    }

    /// Attach `pts` to the next picture started, so a presentation timestamp
    /// follows its picture through reordering instead of staying with the
    /// packet that carried it.
    pub fn set_next_pts(&mut self, pts: Option<Timestamp>) {
        self.c8.set_next_pts(pts);
        self.c16.set_next_pts(pts);
        for i in 0..2 {
            self.x8[i].set_next_pts(pts);
            self.x16[i].set_next_pts(pts);
        }
    }

    /// Cropped picture size of the most recently stored SPS, `None` until one
    /// arrives. Lets a container entry path fill in stream parameters from an
    /// `avcC` record without decoding a picture first.
    pub fn picture_size(&self) -> Option<(u32, u32)> {
        self.c8.picture_size()
    }

    /// Pixel format of the frames the most recently stored SPS produces: `I420`
    /// at 8 bits, `I010` above.
    pub fn picture_format(&self) -> Option<PixelFormat> {
        let sps = self.c8.sps_map[self.c8.last_sps? as usize].as_ref()?;
        let high = sps.bit_depth_luma > 8 || sps.bit_depth_chroma > 8;
        // Monochrome leaves as 4:2:0 with neutral chroma; separate colour
        // planes are merged into 4:4:4.
        let wide = if sps.separate_colour_plane {
            3
        } else {
            sps.chroma_format_idc
        };
        Some(match (wide, high) {
            (3, false) => PixelFormat::I444,
            (3, true) => PixelFormat::I410,
            (2, false) => PixelFormat::I422,
            (2, true) => PixelFormat::I210,
            (_, false) => PixelFormat::I420,
            (_, true) => PixelFormat::I010,
        })
    }

    /// Where the bits of every CABAC slice parsed so far went, by syntax
    /// class, in 1/256ths of a bit.
    pub fn bit_account(&self) -> BitAccount {
        if self.hbd {
            self.c16.bit_account()
        } else {
            self.c8.bit_account()
        }
    }

    /// How many macroblocks of each shape this decoder has read.
    pub fn mb_shapes(&self) -> MbShapes {
        if self.hbd {
            self.c16.mb_shapes()
        } else {
            self.c8.mb_shapes()
        }
    }

    /// Decode the first IDR picture of an Annex B stream and return it.
    pub fn decode_first_idr(&mut self, annexb: &[u8]) -> Result<VideoFrame> {
        let mut got_slice = false;
        // An IDR picture that is a field is only half a frame: the second
        // field of the pair is decoded too.
        let mut fields_done = false;
        for nal in AnnexBIter::new(annexb) {
            match self.push_nal(nal)? {
                NalOutcome::PictureBoundary => {
                    if fields_done || !got_slice {
                        break;
                    }
                    self.end_picture()?;
                    if !self.awaiting_second_field() {
                        // The IDR picture was a whole frame.
                        return self.frame();
                    }
                    fields_done = true;
                    got_slice = false;
                    if let NalOutcome::SliceDecoded = self.push_nal(nal)? {
                        got_slice = true;
                    }
                }
                NalOutcome::SliceDecoded => got_slice = true,
                _ => {}
            }
        }
        if !got_slice {
            if fields_done {
                // The stream ended after the first field.
                return self.frame();
            }
            return Err(Error::NeedMore);
        }
        self.end_picture()?;
        self.frame()
    }

    /// Feed one NAL unit (header byte first, no start code).
    pub fn push_nal(&mut self, nal: &[u8]) -> Result<NalOutcome> {
        let is_slice = nal.first().is_some_and(|b| matches!(b & 0x1f, 1 | 5));
        if !is_slice {
            // Parameter sets are kept by both cores: which one a slice runs
            // on depends on the SPS it names, known only at the slice.
            let mut r = self.c8.push_nal(nal).and(self.c16.push_nal(nal));
            for i in 0..2 {
                r = r
                    .and(self.x8[i].push_nal(nal))
                    .and(self.x16[i].push_nal(nal));
            }
            return r;
        }
        let mut plane = 0usize;
        if let Some((depth, cp)) = self.c8.slice_route(nal) {
            plane = usize::from(cp);
            let hbd = depth > 8;
            if hbd != self.hbd {
                // The bit depth changed: finish whatever the old core had open
                // so its pictures are queued, then switch.
                self.flush()?;
                self.hbd = hbd;
            }
        }
        match (self.hbd, plane) {
            (true, 0) => self.c16.push_nal(nal),
            (true, p) => self.x16[p - 1].push_nal(nal),
            (false, 0) => self.c8.push_nal(nal),
            (false, p) => self.x8[p - 1].push_nal(nal),
        }
    }

    /// Run the deblocking filter over the open picture, store it in the decoded
    /// picture buffer and queue whatever pictures that makes ready for output.
    pub fn end_picture(&mut self) -> Result<()> {
        // Every colour plane's picture ends together; planes that never
        // opened one (the usual case) are skipped.
        if self.hbd {
            if self.x16.iter().all(|c| !c.picture_open()) {
                return self.c16.end_picture();
            }
            let mut r = Ok(());
            if self.c16.picture_open() {
                r = self.c16.end_picture();
            }
            for c in self.x16.iter_mut().filter(|c| c.picture_open()) {
                r = r.and(c.end_picture());
            }
            r
        } else {
            if self.x8.iter().all(|c| !c.picture_open()) {
                return self.c8.end_picture();
            }
            let mut r = Ok(());
            if self.c8.picture_open() {
                r = self.c8.end_picture();
            }
            for c in self.x8.iter_mut().filter(|c| c.picture_open()) {
                r = r.and(c.end_picture());
            }
            r
        }
    }

    /// Emit every remaining picture (end of stream).
    pub fn flush(&mut self) -> Result<()> {
        self.c8.flush()?;
        self.c16.flush()?;
        for i in 0..2 {
            self.x8[i].flush()?;
            self.x16[i].flush()?;
        }
        Ok(())
    }

    /// The next frame ready for output, in the configured order.
    pub fn next_frame(&mut self) -> Option<VideoFrame> {
        // The core that is not running holds the older pictures.
        if self.hbd {
            if let Some(f) = self.c8.next_frame() {
                return Some(f);
            }
            let f = self.c16.next_frame()?;
            Some(merge_planes(
                f,
                self.x16[0].next_frame(),
                self.x16[1].next_frame(),
            ))
        } else {
            if let Some(f) = self.c16.next_frame() {
                return Some(f);
            }
            let f = self.c8.next_frame()?;
            Some(merge_planes(
                f,
                self.x8[0].next_frame(),
                self.x8[1].next_frame(),
            ))
        }
    }

    /// The picture `end_picture` completed most recently, in decode order.
    pub fn frame(&self) -> Result<VideoFrame> {
        if self.hbd {
            let f = self.c16.frame()?;
            Ok(merge_planes(
                f,
                self.x16[0].frame().ok(),
                self.x16[1].frame().ok(),
            ))
        } else {
            let f = self.c8.frame()?;
            Ok(merge_planes(
                f,
                self.x8[0].frame().ok(),
                self.x8[1].frame().ok(),
            ))
        }
    }

    /// True when the picture just completed is a first field whose second
    /// field has not arrived.
    fn awaiting_second_field(&self) -> bool {
        if self.hbd {
            self.c16.awaiting_second_field()
        } else {
            self.c8.awaiting_second_field()
        }
    }

    /// True while a picture is open (started, not completed).
    pub fn picture_open(&self) -> bool {
        if self.hbd {
            self.c16.picture_open() || self.x16.iter().any(|c| c.picture_open())
        } else {
            self.c8.picture_open() || self.x8.iter().any(|c| c.picture_open())
        }
    }

    /// Drop picture state after a seek. Parameter sets survive.
    pub fn reset_pictures(&mut self) {
        self.c8.reset_pictures();
        self.c16.reset_pictures();
        for i in 0..2 {
            self.x8[i].reset_pictures();
            self.x16[i].reset_pictures();
        }
    }
}

/// Join the monochrome frames of colour planes 1 and 2 to the one of plane 0
/// as a 4:4:4 frame. A frame that is not part of a separate-plane stream (the
/// other planes returned nothing) leaves as it is.
fn merge_planes(f0: VideoFrame, f1: Option<VideoFrame>, f2: Option<VideoFrame>) -> VideoFrame {
    let (Some(f1), Some(f2)) = (f1, f2) else {
        return f0;
    };
    let format = match f0.format {
        PixelFormat::I420 => PixelFormat::I444,
        PixelFormat::I010 => PixelFormat::I410,
        _ => return f0,
    };
    let planes = vec![
        f0.planes[0].clone(),
        f1.planes[0].clone(),
        f2.planes[0].clone(),
    ];
    match VideoFrame::try_new(format, f0.width, f0.height, planes) {
        Ok(mut f) => {
            f.color = f0.color;
            f.pts = f0.pts;
            f.interlace = f0.interlace;
            f
        }
        Err(_) => f0,
    }
}

impl<S: Sample> Core<S> {
    /// Bit depth named by the SPS of the slice in `nal`, `None` when the slice
    /// or its parameter sets cannot be read (the core then reports the error).
    pub(crate) fn slice_route(&mut self, nal: &[u8]) -> Option<(u8, u8)> {
        let (_, payload) = nal.split_first()?;
        let n = payload.len().min(16);
        // The scratch buffer the slice path uses anyway, so a steady-state
        // loop allocates nothing here.
        let mut rbsp = core::mem::take(&mut self.rbsp);
        unescape_rbsp(&payload[..n], &mut rbsp);
        let mut peek = BitReader::new(&rbsp);
        let ids = (|| {
            Some((
                peek.read_ue().ok()?,
                peek.read_ue().ok()?,
                peek.read_ue().ok()?,
            ))
        })();
        let (_, _, pps_id) = ids?;
        let pps = self.pps_map.get(pps_id as usize)?.as_ref()?;
        let sps = self.sps_map.get(pps.sps_id as usize)?.as_ref()?;
        let plane = if sps.separate_colour_plane {
            // colour_plane_id u(2) follows pic_parameter_set_id.
            peek.read_bits(2).ok().map_or(0, |v| (v as u8).min(2))
        } else {
            0
        };
        let depth = if sps.chroma_array_type() == 0 {
            sps.bit_depth_luma
        } else {
            sps.bit_depth_luma.max(sps.bit_depth_chroma)
        };
        self.rbsp = rbsp;
        Some((depth, plane))
    }
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

impl<S: Sample> Core<S> {
    /// A fresh decoder with no parameter sets.
    pub(crate) fn new() -> Core<S> {
        Core {
            sps_map: vec![None; 32],
            pps_map: vec![None; 256],
            rbsp: Vec::new(),
            cur: Picture::default(),
            dpb: Dpb::default(),
            has_picture: false,
            cur_key: None,
            ls: flat_scaling(),
            ls8: std::array::from_fn(|_| std::array::from_fn(|_| LevelScale8x8::new(&[16; 64]))),
            plane_id: 0,
            wide: false,
            active_pps: None,
            last_sps: None,
            cur_info: PicInfo {
                structure: FRAME,
                second_of: None,
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
            account: BitAccount::default(),
            shapes: MbShapes::default(),
            pending_pts: None,
            recovery_poc: None,
            cur_ref_padded: false,
        }
    }

    /// Choose between display order (the default) and decode order.
    pub(crate) fn set_output_order(&mut self, order: OutputOrder) {
        self.output_order = order;
    }

    /// The order pictures are handed back in.
    pub(crate) fn output_order(&self) -> OutputOrder {
        self.output_order
    }

    /// Attach `pts` to the next picture started, so a presentation timestamp
    /// follows its picture through reordering instead of staying with the
    /// packet that carried it.
    pub(crate) fn set_next_pts(&mut self, pts: Option<Timestamp>) {
        self.pending_pts = pts;
    }

    /// Cropped picture size of the most recently stored SPS, `None` until one
    /// arrives. Lets a container entry path fill in stream parameters from an
    /// `avcC` record without decoding a picture first.
    pub(crate) fn picture_size(&self) -> Option<(u32, u32)> {
        let sps = self.sps_map[self.last_sps? as usize].as_ref()?;
        Some((sps.width, sps.height))
    }

    /// Where the bits of every CABAC slice parsed so far went, by syntax
    /// class, in 1/256ths of a bit. Any conforming stream can be measured
    /// this way, so an encoder's spending can be compared with another's
    /// through one account.
    pub(crate) fn bit_account(&self) -> BitAccount {
        self.account
    }

    /// How many macroblocks of each shape this decoder has read, which is what
    /// a stream's partitioning looks like from the outside.
    pub(crate) fn mb_shapes(&self) -> MbShapes {
        self.shapes
    }

    /// Feed one NAL unit (header byte first, no start code).
    pub(crate) fn push_nal(&mut self, nal: &[u8]) -> Result<NalOutcome> {
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
                self.active_pps = None;
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
                // A re-sent PPS may carry other scaling lists: rebuild the level scales.
                self.active_pps = None;
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
    pub(crate) fn end_picture(&mut self) -> Result<()> {
        if !self.has_picture || self.cur.complete {
            return Err(Error::corrupt("end_picture without an open picture"));
        }
        if self.cur.mbaff {
            crate::deblock_mbaff::deblock_mbaff(&mut self.cur);
        } else {
            deblock_picture(&mut self.cur);
        }
        self.cur.extend_borders();
        self.cur.complete = true;
        self.last_decoded = self.cur.id;
        // The first picture completed after a reset is the random-access
        // point the seek landed on; its own POC becomes the recovery
        // threshold (see `recovery_poc`'s doc comment).
        let recovery_poc = *self.recovery_poc.get_or_insert(self.cur.poc);
        let output_ok = !(self.cur_ref_padded && self.cur.poc < recovery_poc);
        let sps_id = self.cur.sps_id as usize;
        let sps = self.sps_map[sps_id]
            .take()
            .ok_or_else(|| Error::corrupt("active SPS vanished"))?;
        let pic = core::mem::take(&mut self.cur);
        let marking = self.has_marking.then_some(&self.cur_marking);
        let stored = if self.dpb.interlaced {
            self.dpb
                .store_i(pic, &sps, &self.cur_info, marking, output_ok)
        } else {
            self.dpb
                .store(pic, &sps, &self.cur_info, marking, output_ok)
        };
        self.sps_map[sps_id] = Some(sps);
        stored?;
        if self.dpb.interlaced {
            // The picture a caller asks for is the frame store, which a field
            // joined or created.
            self.last_decoded = self.dpb.last_stored_id();
        }
        self.has_picture = false;
        self.pump_output(false)
    }

    /// Emit every remaining picture (end of stream).
    pub(crate) fn flush(&mut self) -> Result<()> {
        if self.has_picture && !self.cur.complete {
            self.end_picture()?;
        }
        self.pump_output(true)
    }

    /// The next frame ready for output, in the configured order.
    pub(crate) fn next_frame(&mut self) -> Option<VideoFrame> {
        self.out.pop_front()
    }

    /// The picture `end_picture` completed most recently, in decode order.
    ///
    /// Kept for a caller that drives one picture at a time; a player wants
    /// [`Decoder::next_frame`], which respects the reordering the stream asks
    /// for.
    pub(crate) fn frame(&self) -> Result<VideoFrame> {
        let id = self.last_decoded;
        let pic = self
            .dpb
            .frames
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| Error::corrupt("frame() before end_picture()"))?;
        self.picture_to_frame(pic)
    }

    /// True when the most recently completed picture is a first field.
    pub(crate) fn awaiting_second_field(&self) -> bool {
        self.dpb.awaiting_second_field()
    }

    /// True while a picture is open (started, not completed).
    pub(crate) fn picture_open(&self) -> bool {
        self.has_picture && !self.cur.complete
    }

    /// Drop picture state after a seek. Parameter sets survive, because the
    /// container does not resend them at every seek point.
    pub(crate) fn reset_pictures(&mut self) {
        self.has_picture = false;
        self.cur.complete = false;
        self.dpb.clear();
        self.out.clear();
        self.last_decoded = -1;
        self.pending_pts = None;
        self.recovery_poc = None;
    }

    /// Move pictures out of the decoded picture buffer into the output queue.
    fn pump_output(&mut self, flush: bool) -> Result<()> {
        if self.output_order == OutputOrder::Decode && !flush {
            let id = self.last_decoded;
            if let Some(i) = self
                .dpb
                .frames
                .iter()
                .position(|p| p.id == id && p.output && !p.awaiting_second)
            {
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
    fn picture_to_frame(&self, pic: &Picture<S>) -> Result<VideoFrame> {
        let (w, h) = (pic.out_size.0 as usize, pic.out_size.1 as usize);
        let (cx, cy) = (pic.crop.0 as usize, pic.crop.1 as usize);
        // Above 8 bits the samples leave as 16-bit little-endian words in I010,
        // left-aligned to 10 bits when the stream is 9-bit.
        let shift = 10u32.saturating_sub(pic.bd);
        let copy = |p: &PicPlane<S>, x0: usize, y0: usize, w: usize, h: usize| -> Plane {
            // Grown row by row rather than allocated zeroed: every byte is
            // about to be overwritten, and at 1080p the fill it avoids is
            // three megabytes per frame.
            let mut out = Vec::with_capacity(w * h * S::BYTES);
            for row in 0..h {
                let src = p.at(x0, y0 + row);
                S::extend_bytes(&mut out, &p.data[src..src + w], shift);
            }
            Plane::new(out, w * S::BYTES)
        };
        let c422 = pic.cat == 2;
        let c444 = pic.cat == 3;
        // 4:2:2 chroma is full height: no vertical halving of the crop origin
        // or of the plane height; 4:4:4 chroma is full size.
        let (cy_c, ch_c) = if c422 || c444 {
            (cy, h)
        } else {
            (cy / 2, h.div_ceil(2))
        };
        let (cx_c, cw_c) = if c444 {
            (cx, w)
        } else {
            (cx / 2, w.div_ceil(2))
        };
        let planes = vec![
            copy(&pic.y, cx, cy, w, h),
            copy(&pic.cb, cx_c, cy_c, cw_c, ch_c),
            copy(&pic.cr, cx_c, cy_c, cw_c, ch_c),
        ];
        let mut frame = VideoFrame::try_new(
            match (pic.cat, S::IS_8) {
                (3, true) => PixelFormat::I444,
                (3, false) => PixelFormat::I410,
                (2, true) => PixelFormat::I422,
                (2, false) => PixelFormat::I210,
                (_, true) => PixelFormat::I420,
                (_, false) => PixelFormat::I010,
            },
            w as u32,
            h as u32,
            planes,
        )?;
        frame.color = pic.color;
        frame.pts = pic.pts;
        frame.interlace = if pic.fields.is_empty() {
            Interlace::Progressive
        } else {
            // The field that displays first is the one with the lower order
            // count (a lone field leaves the other one's count meaningless).
            let first_is_bottom = match pic.have {
                [true, false] => false,
                [false, true] => true,
                _ => pic.fpoc[1] < pic.fpoc[0],
            };
            if first_is_bottom {
                Interlace::BottomFieldFirst
            } else {
                Interlace::TopFieldFirst
            }
        };
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
        let pps = self.pps_map[pps_id as usize]
            .as_ref()
            .ok_or_else(|| Error::corrupt("slice refers to an unknown PPS"))?;
        let sps = self.sps_map[pps.sps_id as usize]
            .as_ref()
            .ok_or_else(|| Error::corrupt("PPS refers to an unknown SPS"))?;

        check_supported::<S>(sps, pps, slice_type_code, self.wide)?;
        let sh = SliceHeader::parse(rbsp, header, sps, pps)?;
        if self.has_picture && !self.cur.complete {
            let same_picture = self.cur_key == Some(PicKey::of(&sh, header));
            // A picture starts again at macroblock 0 once macroblock 0 is
            // already decoded; with ASO (clause 7.4.3) the slice holding
            // macroblock 0 may also arrive after the others.
            if self.cur.decoded_mbs > 0
                && (!same_picture || (first_mb == 0 && self.cur.mb_slice[0] != NO_SLICE))
            {
                return Ok(NalOutcome::PictureBoundary);
            }
        }
        if sh.redundant_pic_cnt > 0 {
            // Redundant coded slices (7.4.3) repeat primary data: ignored.
            return Ok(NalOutcome::Skipped);
        }
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
            self.cur_key = Some(PicKey::of(sh, header));
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
        let lists = if sh.slice_type.is_intra() {
            [RefList::default(), RefList::default()]
        } else {
            let (lists, padded) = if self.dpb.interlaced {
                // 8.2.4.1: a field counts picture numbers per field.
                let (curr_pic_num, max_pic_num) = if sh.field_pic {
                    (2 * sh.frame_num as i32 + 1, 2i32 << sps.log2_max_frame_num)
                } else {
                    (sh.frame_num as i32, 1i32 << sps.log2_max_frame_num)
                };
                self.dpb.number_short_term_i(sh.frame_num, sps);
                self.dpb.build_ref_lists_i(
                    sh.slice_type,
                    self.cur.structure,
                    self.cur.poc,
                    curr_pic_num,
                    max_pic_num,
                    sh.num_ref_idx_l0_active as usize,
                    sh.num_ref_idx_l1_active as usize,
                    (&sh.ref_pic_list_mod_l0, &sh.ref_pic_list_mod_l1),
                )?
            } else {
                let max_pic_num = 1i32 << sps.log2_max_frame_num;
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
            // A ref list shorter than requested was padded by repeating an
            // available entry (8.2.4.2's unspecified tail) rather than the
            // reference this slice actually asked for. Right after a seek's
            // reset, that means the picture predicts from something never
            // decoded since the random-access start; recorded here so
            // `end_picture` can hold its output back (see `recovery_poc`).
            self.cur_ref_padded |= padded;
            lists
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
            shapes: MbShapes::default(),
            slice_id,
            slice_type: sh.slice_type,
            qp: sh.slice_qp,
            cb_qp_offset: pps.chroma_qp_index_offset,
            cr_qp_offset: pps.second_chroma_qp_index_offset,
            cmb_h: self.cur.cmb_h,
            cat: self.cur.cat,
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
            fpoc: self.cur.fpoc,
            structure: self.cur.structure,
            mb_parity: self.cur.structure,
            pos: MbPos::default(),
        };
        let intra_slice = sh.slice_type.is_intra();
        let b_slice = sh.slice_type == SliceType::B;
        if !intra_slice && ctx.lists[0].len() == 0 {
            return Err(Error::corrupt("inter slice with no reference pictures"));
        }

        let mbs = self.cur.mb_w * self.cur.mb_h;
        if self.cur.mbaff {
            self.decode_mbaff_slice(
                &mut r,
                &mut ctx,
                first_mb as usize * 2,
                pps.entropy_coding_mode,
            )?;
            self.shapes.add(&ctx.shapes);
            if let Entropy::Cabac(c) = &r {
                self.account.add(&c.account());
            }
            return Ok(NalOutcome::SliceDecoded);
        }
        let groups = if pps.num_slice_groups > 1 {
            crate::fmo::mb_to_slice_group(sps, pps, sh.slice_group_change_cycle)
        } else {
            None
        };
        let step = |a: usize| match &groups {
            Some(g) => crate::fmo::next_in_group(g, a),
            None => a + 1,
        };
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
                        mb_addr = step(mb_addr);
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
            mb_addr = step(mb_addr);
        }
        self.shapes.add(&ctx.shapes);
        if let Entropy::Cabac(c) = &r {
            self.account.add(&c.account());
        }
        Ok(NalOutcome::SliceDecoded)
    }

    /// Point `ctx` at macroblock `mb_addr` (syntax order, `2 * pair + b`) of an
    /// MBAFF frame in the given frame/field mode; returns its slot.
    fn mbaff_enter(&self, ctx: &mut SliceCtx, mb_addr: usize, field: bool) -> usize {
        let w = self.cur.mb_w;
        let pair = mb_addr >> 1;
        let (px, pr) = (pair % w, pair / w);
        let b = mb_addr & 1;
        ctx.pos = MbPos {
            active: true,
            px,
            slot_y: 2 * pr + b,
            field,
            slice_id: ctx.slice_id,
        };
        ctx.mb_parity = if !field {
            FRAME
        } else if b == 1 {
            BOTTOM
        } else {
            TOP
        };
        (2 * pr + b) * w + px
    }

    /// ctxIdxInc of mb_field_decoding_flag (9.3.3.1.1.2): the left and above
    /// macroblock pairs of the same slice that are field pairs.
    fn field_flag_inc(&self, ctx: &SliceCtx, mb_addr: usize) -> usize {
        let pic = &self.cur;
        let w = pic.mb_w;
        let pair = mb_addr >> 1;
        let (px, pr) = (pair % w, pair / w);
        let is_field = |x: usize, y: usize| {
            let t = (2 * y) * w + x;
            usize::from(pic.mb_slice[t] == ctx.slice_id && pic.mb_field[t] != 0)
        };
        (if px > 0 { is_field(px - 1, pr) } else { 0 })
            + (if pr > 0 { is_field(px, pr - 1) } else { 0 })
    }

    /// The slice data loop of an MBAFF frame (7.3.4): macroblock pairs, the
    /// per-pair `mb_field_decoding_flag`, and the skipped-top-macroblock
    /// cases in which the flag arrives with the bottom macroblock.
    fn decode_mbaff_slice(
        &mut self,
        r: &mut Entropy<'_>,
        ctx: &mut SliceCtx,
        first_addr: usize,
        cabac: bool,
    ) -> Result<()> {
        let intra_slice = ctx.slice_type.is_intra();
        let b_slice = ctx.is_b();
        let mbs = self.cur.mb_w * self.cur.mb_h;
        let w = self.cur.mb_w;
        let mut mb_addr = first_addr;
        let mut more = true;
        macro_rules! decode {
            ($slot:expr, $skipped:expr) => {
                decode_macroblock(
                    &mut self.cur,
                    &self.dpb.frames,
                    &self.ls,
                    &self.ls8,
                    r,
                    ctx,
                    $slot,
                    $skipped,
                )?
            };
        }
        // The frame/field mode of the pair being decoded, once known.
        let mut pair_field = false;
        if cabac {
            // CABAC codes pairs: at most two macroblocks between end_of_slice
            // flags, with the top-skipped case reordered.
            if !mb_addr.is_multiple_of(2) {
                return Err(Error::corrupt("MBAFF slice starts at a bottom macroblock"));
            }
            while more {
                if mb_addr + 1 >= mbs {
                    return Err(Error::corrupt("macroblock address beyond the picture"));
                }
                let pair = mb_addr >> 1;
                let (px, pr) = (pair % w, pair / w);
                let inferred = mbaff::infer_field(&self.cur, px, pr, ctx.slice_id);
                let top_slot = self.mbaff_enter(ctx, mb_addr, inferred);
                let bot_slot = top_slot + w;
                let mut top_skipped = false;
                if !intra_slice {
                    self.begin_mb(r, ctx, top_slot);
                    let inc = self.skip_inc(ctx, top_slot);
                    top_skipped = r.mb_skip_flag(b_slice, inc)?;
                }
                if !top_skipped {
                    let inc = self.field_flag_inc(ctx, mb_addr);
                    pair_field = r.mb_field_flag(inc)?;
                    self.mbaff_enter(ctx, mb_addr, pair_field);
                    decode!(top_slot, false);
                    self.mbaff_enter(ctx, mb_addr + 1, pair_field);
                    let mut bot_skipped = false;
                    if !intra_slice {
                        self.begin_mb(r, ctx, bot_slot);
                        let inc = self.skip_inc(ctx, bot_slot);
                        bot_skipped = r.mb_skip_flag(b_slice, inc)?;
                    }
                    decode!(bot_slot, bot_skipped);
                } else {
                    // The top macroblock is skipped; whether the pair is a
                    // field pair is only known after the bottom macroblock's
                    // skip flag (and, when that is clear, its field flag).
                    // Until then the top stands in as a decoded skipped
                    // macroblock for the bottom's context derivation.
                    self.cur.mb_slice[top_slot] = ctx.slice_id;
                    self.cur.mb_flags[top_slot] = FLAG_DECODED
                        | FLAG_INTER
                        | FLAG_SKIP
                        | if b_slice { FLAG_DIRECT } else { 0 };
                    self.cur.mb_field[top_slot] = u8::from(inferred);
                    self.mbaff_enter(ctx, mb_addr + 1, inferred);
                    self.begin_mb(r, ctx, bot_slot);
                    let inc = self.skip_inc(ctx, bot_slot);
                    let bot_skipped = r.mb_skip_flag(b_slice, inc)?;
                    pair_field = if bot_skipped {
                        inferred
                    } else {
                        let inc = self.field_flag_inc(ctx, mb_addr);
                        r.mb_field_flag(inc)?
                    };
                    self.cur.mb_slice[top_slot] = NO_SLICE;
                    self.cur.mb_flags[top_slot] = 0;
                    self.mbaff_enter(ctx, mb_addr, pair_field);
                    decode!(top_slot, true);
                    self.mbaff_enter(ctx, mb_addr + 1, pair_field);
                    decode!(bot_slot, bot_skipped);
                }
                more = r.more_macroblocks()?;
                mb_addr += 2;
            }
            return Ok(());
        }
        // CAVLC.
        while more {
            let mut run = 0u32;
            if !intra_slice {
                run = r.mb_skip_run()?;
                for left in (1..=run).rev() {
                    if mb_addr >= mbs {
                        return Err(Error::corrupt("mb_skip_run past the picture"));
                    }
                    let pair = mb_addr >> 1;
                    let (px, pr) = (pair % w, pair / w);
                    let top = mb_addr.is_multiple_of(2);
                    // A skipped top macroblock whose bottom macroblock is
                    // coded takes the bottom's field flag, which is the next
                    // bit of the stream; two skipped macroblocks infer it.
                    if top {
                        pair_field = if left > 1 {
                            mbaff::infer_field(&self.cur, px, pr, ctx.slice_id)
                        } else {
                            r.peek_bit()
                        };
                    }
                    let slot = self.mbaff_enter(ctx, mb_addr, pair_field);
                    decode!(slot, true);
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
            let top = mb_addr.is_multiple_of(2);
            if top || run > 0 {
                pair_field = r.mb_field_flag(0)?;
            }
            let slot = self.mbaff_enter(ctx, mb_addr, pair_field);
            decode!(slot, false);
            more = r.more_macroblocks()?;
            mb_addr += 1;
        }
        Ok(())
    }

    /// Publish the neighbourhood of `mb_addr` to a CABAC reader ahead of
    /// mb_skip_flag, which is decoded before the macroblock layer.
    fn begin_mb(&self, r: &mut Entropy<'_>, ctx: &SliceCtx, mb_addr: usize) {
        let pic = &self.cur;
        let nbr = ctx_neighbors(pic, ctx, mb_addr % pic.mb_w, mb_addr / pic.mb_w);
        r.begin_mb(&build_mb_ctx(pic, ctx, &nbr, mb_addr));
    }

    /// ctxIdxInc for mb_skip_flag (9.3.3.1.1.1): a neighbour counts unless it
    /// is unavailable or was itself skipped.
    fn skip_inc(&self, ctx: &SliceCtx, mb_addr: usize) -> usize {
        let pic = &self.cur;
        let mb_x = mb_addr % pic.mb_w;
        let mb_y = mb_addr / pic.mb_w;
        if ctx.pos.active {
            let cond =
                |s: Option<usize>| usize::from(s.is_some_and(|s| pic.mb_flags[s] & FLAG_SKIP == 0));
            return cond(mbaff::mb_a(pic, &ctx.pos)) + cond(mbaff::mb_b(pic, &ctx.pos));
        }
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
        self.cur_ref_padded = false;
        let structure = if !sh.field_pic {
            FRAME
        } else if sh.bottom_field {
            BOTTOM
        } else {
            TOP
        };
        let mut info = PicInfo {
            structure,
            second_of: None,
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
        let interlaced = self.dpb.interlaced;
        if interlaced
            && self
                .dpb
                .pending_first_field(sh.frame_num, structure, info.is_idr)
                .is_none()
        {
            // A picture that does not complete the waiting first field leaves
            // that field a non-paired one.
            self.dpb.finalize_unpaired();
            self.pump_output(false)?;
        }
        let second = if interlaced {
            self.dpb
                .pending_first_field(sh.frame_num, structure, info.is_idr)
        } else {
            None
        };
        if second.is_none() && !info.is_idr && self.dpb.frame_num_gap(sps, sh.frame_num) {
            // Pictures were lost (or a seek landed here): infer them either way.
            if interlaced {
                self.dpb.fill_frame_num_gap_i(sps, sh.frame_num)?;
            } else {
                self.dpb.fill_frame_num_gap(sps, sh.frame_num)?;
            }
            self.pump_output(false)?;
        }
        info.second_of = second;
        let poc = self.dpb.picture_order_count(sps, &info)?;
        let mut pic = if structure == FRAME {
            self.dpb.take_picture()
        } else {
            self.dpb.take_field_picture()
        };
        pic.start_as(sps, structure);
        if sps.mb_adaptive_frame_field && structure == FRAME {
            pic.coding = CODED_AFRM;
            pic.mbaff = true;
        }
        pic.poc = poc.value;
        pic.fpoc = [poc.top, poc.bottom];
        pic.poc_msb = poc.msb;
        pic.poc_lsb = poc.lsb;
        pic.frame_num_offset = poc.frame_num_offset;
        pic.frame_num = sh.frame_num;
        if second.is_none() {
            pic.pts = self.pending_pts.take();
        }
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
            // Separately coded colour planes are monochrome pictures that
            // take the lists of their own colour (iYCbCr = colour_plane_id).
            let weights = if sps.separate_colour_plane {
                let k = usize::from(self.plane_id);
                [[weights[0][k]; 3], [weights[1][k]; 3]]
            } else {
                weights
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
            // list_8x8 is [Intra Y, Inter Y, Intra Cb, Inter Cb, Intra Cr, Inter Cr].
            let w8 = |intra: usize, comp: usize| -> &[u8; 64] {
                let k = if sps.separate_colour_plane {
                    usize::from(self.plane_id)
                } else {
                    comp
                };
                match lists {
                    Some(l) => &l.list_8x8[2 * k + intra],
                    None => &[16u8; 64],
                }
            };
            self.ls8 = std::array::from_fn(|intra| {
                std::array::from_fn(|comp| LevelScale8x8::new(w8(intra, comp)))
            });
            self.active_pps = Some(pps.id);
        }
        Ok(())
    }
}

/// What tells two slices of one picture apart from the first slice of the
/// next one: the first-slice fields of clause 7.4.1.2.4.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct PicKey {
    field_pic: bool,
    bottom_field: bool,
    idr: bool,
    reference: bool,
    idr_pic_id: Option<u32>,
    frame_num: u32,
    pps_id: u32,
    poc_lsb: u32,
    delta_poc_bottom: i32,
    delta_poc: [i32; 2],
}

impl PicKey {
    fn of(sh: &SliceHeader, header: NalHeader) -> PicKey {
        PicKey {
            field_pic: sh.field_pic,
            bottom_field: sh.bottom_field,
            idr: header.is_idr(),
            reference: header.ref_idc != 0,
            idr_pic_id: sh.idr_pic_id,
            frame_num: sh.frame_num,
            pps_id: sh.pps_id,
            poc_lsb: sh.pic_order_cnt_lsb,
            delta_poc_bottom: sh.delta_pic_order_cnt_bottom,
            delta_poc: sh.delta_pic_order_cnt,
        }
    }
}

/// Reject unsupported streams with named reasons before touching pixels.
///
/// Every `why` here names the syntax element that fired and the machinery it
/// would need: a refusal is a capability statement about this binary, and the
/// conformance suite proves each one against a stream that really uses the
/// feature.
fn check_supported<S: Sample>(
    sps: &Sps,
    pps: &Pps,
    slice_type_code: u32,
    wide: bool,
) -> Result<()> {
    if pps.num_slice_groups > 1 && !sps.frame_mbs_only {
        return Err(Error::unsupported(
            "FMO slice groups with interlaced coding",
            "slice groups over field or MBAFF map units (clause 8.2.2.8) are \
             not implemented; FMO is decoded for frame_mbs_only streams",
        ));
    }
    // With ChromaArrayType 0 (monochrome, or three separately coded planes)
    // the chroma depth is not used by the decoding process (7.4.2.1.1).
    if sps.chroma_array_type() != 0 && sps.bit_depth_luma != sps.bit_depth_chroma {
        return Err(Error::unsupported(
            "different luma and chroma bit depths",
            format!(
                "{}-bit luma with {}-bit chroma; planes and output frames carry \
                 one sample depth",
                sps.bit_depth_luma, sps.bit_depth_chroma
            ),
        ));
    }
    if sps.bit_depth_luma > if wide { 14 } else { 10 } {
        return Err(Error::unsupported(
            "bit depth above 10",
            format!(
                "{}-bit samples are beyond what this decoder carries; it handles \
                 8, 9 and 10 bits",
                sps.bit_depth_luma
            ),
        ));
    }
    if (sps.bit_depth_luma == 8) != S::IS_8 {
        // slice_bit_depth routes slices to the matching core; this is the
        // guard for a caller that bypasses it.
        return Err(Error::unsupported(
            "bit depth change inside a picture buffer",
            "the sample type of the decoder does not match the SPS bit depth",
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
/// How many macroblocks of each shape a stream codes: the partitioning the
/// encoder chose, counted from the outside.
///
/// P macroblocks only; a B macroblock's own shapes are not split out.
#[derive(Debug, Default, Clone, Copy)]
pub struct MbShapes {
    pub skip: u64,
    pub p16x16: u64,
    pub p16x8: u64,
    pub p8x16: u64,
    /// P_8x8 and P_8x8ref0, whatever the sub_mb_types inside are.
    pub p8x8: u64,
    pub bidir: u64,
    pub intra: u64,
}

impl MbShapes {
    /// Add another count into this one.
    pub fn add_public(&mut self, o: &MbShapes) {
        self.add(o);
    }

    pub(crate) fn add(&mut self, o: &MbShapes) {
        self.skip += o.skip;
        self.p16x16 += o.p16x16;
        self.p16x8 += o.p16x8;
        self.p8x16 += o.p8x16;
        self.p8x8 += o.p8x8;
        self.bidir += o.bidir;
        self.intra += o.intra;
    }
}

/// See [`SliceCtx::geo`]. Chroma rows are half of each luma row value.
#[derive(Clone, Copy)]
struct Geo {
    src_y: usize,
    dst_y: usize,
    dst_cy: usize,
    mul: usize,
}

impl Geo {
    /// Picture row of luma row `r` of the macroblock.
    #[inline]
    fn y(&self, r: usize) -> usize {
        self.dst_y + r * self.mul
    }

    /// Picture row of chroma row `r` of the macroblock.
    #[inline]
    fn cy(&self, r: usize) -> usize {
        self.dst_cy + r * self.mul
    }
}

pub(crate) struct SliceCtx {
    slice_id: u16,
    slice_type: SliceType,
    qp: i32,
    cb_qp_offset: i32,
    cr_qp_offset: i32,
    /// Chroma rows per macroblock: 8 (4:2:0) or 16 (4:2:2).
    cmb_h: usize,
    /// ChromaArrayType of the picture being decoded.
    cat: u8,
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
    /// `TopFieldOrderCnt` and `BottomFieldOrderCnt` of the current picture.
    fpoc: [i32; 2],
    /// Structure of the picture being decoded: [`FRAME`], [`TOP`] or [`BOTTOM`].
    structure: u8,
    /// Parity of the macroblock being decoded when it is a field macroblock
    /// ([`TOP`] or [`BOTTOM`]), [`FRAME`] for a frame macroblock. In a field
    /// picture this is the picture's own parity.
    mb_parity: u8,
    /// Where the macroblock being decoded sits when the picture is an MBAFF
    /// frame (`pos.active`); inert otherwise.
    pos: MbPos,
    /// How many macroblocks of each shape this slice decoded.
    shapes: MbShapes,
}

impl SliceCtx {
    /// `PicOrderCnt` of the current picture, or of the field of the current
    /// macroblock's parity for a field macroblock of an MBAFF frame (8.4.1.2.3, 8.4.2.3.1).
    #[inline]
    fn cur_poc(&self) -> i32 {
        if self.pos.active && self.pos.field {
            self.fpoc[usize::from(self.mb_parity == BOTTOM)]
        } else {
            self.poc
        }
    }

    fn is_b(&self) -> bool {
        self.slice_type == SliceType::B
    }

    /// The reference picture `RefPicListX[idx]` points at: a frame, or one
    /// field of a stored frame.
    #[inline(always)]
    fn reference<'a, S: Sample>(
        &self,
        refs: &'a [Picture<S>],
        list: usize,
        idx: i8,
    ) -> Option<&'a Picture<S>> {
        if idx < 0 {
            return None;
        }
        if self.pos.active && self.pos.field {
            return self.field_mb_reference(refs, list, idx);
        }
        let (k, s) = self.lists[list].get_ref(idx as usize)?;
        let p = refs.get(k)?;
        if s == FRAME {
            Some(p)
        } else {
            p.fields
                .get(usize::from(s) - 1)
                .filter(|f| !f.y.data.is_empty())
        }
    }

    /// [`SliceCtx::reference`] for a field macroblock of an MBAFF frame.
    #[inline(never)]
    fn field_mb_reference<'a, S: Sample>(
        &self,
        refs: &'a [Picture<S>],
        list: usize,
        idx: i8,
    ) -> Option<&'a Picture<S>> {
        // 8.4.2.1: a field macroblock of an MBAFF frame indexes the fields of
        // the frame list: entry `idx >> 1`, the field of the macroblock's own
        // parity for an even index, the other for odd.
        let (k, _) = self.lists[list].get_ref((idx >> 1) as usize)?;
        let par = usize::from(self.mb_parity == BOTTOM) ^ (idx as usize & 1);
        refs.get(k)?
            .fields
            .get(par)
            .filter(|f| !f.y.data.is_empty())
    }

    /// True while decoding a field picture or a field macroblock.
    #[inline]
    fn is_field(&self) -> bool {
        self.mb_parity != FRAME
    }

    /// Sample geometry of the macroblock in slot row `mb_y`: the reference
    /// row of its first luma row (motion compensation reads fields at field
    /// row positions), the picture row it is written at, and the row step.
    #[inline]
    fn geo(&self, mb_y: usize) -> Geo {
        if self.pos.active && self.pos.field {
            Geo {
                src_y: (mb_y / 2) * 16,
                dst_y: (mb_y / 2) * 32 + (mb_y & 1),
                dst_cy: (mb_y / 2) * 2 * self.cmb_h + (mb_y & 1),
                mul: 2,
            }
        } else {
            Geo {
                src_y: mb_y * 16,
                dst_y: mb_y * 16,
                dst_cy: mb_y * self.cmb_h,
                mul: 1,
            }
        }
    }

    /// The vertical chroma vector offset of 8.4.1.4 (Tables 8-9 and 8-10) for
    /// a prediction from `rp`: fields of opposite parity sit half a field line
    /// apart, which is a quarter of a chroma sample.
    #[inline]
    fn chroma_dy<S: Sample>(&self, rp: &Picture<S>) -> i16 {
        if self.cat != 1 {
            // Tables 8-9 and 8-10 apply to ChromaArrayType 1 only.
            return 0;
        }
        match (self.mb_parity, rp.structure) {
            (TOP, BOTTOM) => -2,
            (BOTTOM, TOP) => 2,
            _ => 0,
        }
    }

    /// True when no slice-level weighting can apply at all, so every
    /// component of every partition resolves to [`Weights::DEFAULT`] without
    /// the per-component lookup: neither weighted prediction (P) nor
    /// weighted/bi-prediction (B) is signalled.
    fn no_weights(&self) -> bool {
        self.weighted_bipred_idc == 0 && !self.weighted_pred
    }

    /// Prediction weights of one component (clause 8.4.3).
    fn weights_for<S: Sample>(
        &self,
        refs: &[Picture<S>],
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
            let tb = (self.cur_poc() - p0.poc).clamp(-128, 127);
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
        let field_mb = self.pos.active && self.pos.field;
        let pick = |list: &[WeightEntry], idx: i8| -> (i32, i32) {
            let default = (1 << denom, 0);
            if idx < 0 {
                return default;
            }
            // refIdxL0WP of 8.4.2.3: field macroblocks of an MBAFF frame use
            // the frame entry of the weight table.
            let idx = if field_mb { idx >> 1 } else { idx };
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
    /// Availability of the left samples of each group of four rows. All equal
    /// to `a` except under constrained intra prediction in an MBAFF frame,
    /// where the rows of a macroblock can come from different neighbours.
    pub a4: [bool; 4],
    pub b: bool,
    pub c: bool,
    pub d: bool,
    /// The macroblock's place in an MBAFF frame; inert (`active` false) in
    /// every other picture, where the readers below take the raster path.
    pub pos: MbPos,
}

pub(crate) fn mb_neighbors<S: Sample>(
    pic: &Picture<S>,
    mb_x: usize,
    mb_y: usize,
    slice_id: u16,
) -> MbNeighbors {
    let w = pic.mb_w;
    let addr = mb_y * w + mb_x;
    let same = |a: usize| pic.mb_slice[a] == slice_id;
    let a = mb_x > 0 && same(addr - 1);
    MbNeighbors {
        a,
        a4: [a; 4],
        b: mb_y > 0 && same(addr - w),
        c: mb_y > 0 && mb_x + 1 < w && same(addr - w + 1),
        d: mb_y > 0 && mb_x > 0 && same(addr - w - 1),
        pos: MbPos::default(),
    }
}

/// [`mb_neighbors`] for the macroblock the slice context currently points at,
/// MBAFF-aware: neighbours A..D are the macroblocks Table 6-4 gives for the
/// locations (-1, 0), (0, -1), (16, -1) and (-1, -1).
fn ctx_neighbors<S: Sample>(
    pic: &Picture<S>,
    ctx: &SliceCtx,
    mb_x: usize,
    mb_y: usize,
) -> MbNeighbors {
    if !ctx.pos.active {
        return mb_neighbors(pic, mb_x, mb_y, ctx.slice_id);
    }
    let pos = ctx.pos;
    let at = |x, y| mbaff::locate(pic, &pos, x, y, 16, 16).is_some();
    let a = at(-1, 0);
    MbNeighbors {
        a,
        a4: [a; 4],
        b: at(0, -1),
        c: at(16, -1),
        d: at(-1, -1),
        pos,
    }
}

/// The same, with the constrained intra prediction rule of 7.4.2.2 applied:
/// an inter-coded neighbour is not available as an intra prediction source.
fn intra_neighbors<S: Sample>(
    pic: &Picture<S>,
    mb_x: usize,
    mb_y: usize,
    n: MbNeighbors,
) -> MbNeighbors {
    let w = pic.mb_w;
    let addr = mb_y * w + mb_x;
    if n.pos.active {
        let intra_at = |x, y| {
            mbaff::locate(pic, &n.pos, x, y, 16, 16)
                .is_some_and(|l| pic.mb_flags[l.slot] & FLAG_INTER == 0)
        };
        let a4: [bool; 4] =
            std::array::from_fn(|k| n.a && (4 * k..4 * k + 4).all(|y| intra_at(-1, y as i32)));
        return MbNeighbors {
            a: a4.iter().all(|&v| v),
            a4,
            b: n.b && intra_at(0, -1),
            c: n.c && intra_at(16, -1),
            d: n.d && intra_at(-1, -1),
            pos: n.pos,
        };
    }
    let intra = |a: usize| pic.mb_flags[a] & FLAG_INTER == 0;
    let a = n.a && intra(addr - 1);
    MbNeighbors {
        a,
        a4: [a; 4],
        b: n.b && intra(addr - w),
        c: n.c && intra(addr - w + 1),
        d: n.d && intra(addr - w - 1),
        pos: n.pos,
    }
}

/// Non-zero counts of the left and above neighbours of the luma 4x4 block at
/// global block coords `(bx, by)`, `None` when that neighbour is unavailable
/// (clause 6.4.11.4 plus the availability rules of 6.4.8).
#[inline]
pub(crate) fn luma_nz_pair<S: Sample>(
    pic: &Picture<S>,
    n: &MbNeighbors,
    bx: usize,
    by: usize,
) -> (Option<u8>, Option<u8>) {
    nz_pair_in(pic, &pic.nz_y, n, bx, by)
}

/// The count grid of colour component `comp` laid out like the luma one: `nz_y`,
/// or for 4:4:4 the Cb / Cr grids, which are coded like luma.
#[inline]
fn nz_grid<S: Sample>(pic: &Picture<S>, comp: usize) -> &[u8] {
    if comp == 0 {
        &pic.nz_y
    } else {
        &pic.nz_c[comp - 1]
    }
}

#[inline]
fn nz_grid_mut<S: Sample>(pic: &mut Picture<S>, comp: usize) -> &mut [u8] {
    if comp == 0 {
        &mut pic.nz_y
    } else {
        &mut pic.nz_c[comp - 1]
    }
}

/// [`luma_nz_pair`] over an explicit count grid (`mb_w * 4` wide).
#[inline]
fn nz_pair_in<S: Sample>(
    pic: &Picture<S>,
    grid: &[u8],
    n: &MbNeighbors,
    bx: usize,
    by: usize,
) -> (Option<u8>, Option<u8>) {
    let w4 = pic.mb_w * 4;
    if n.pos.active {
        let get =
            |left| mbaff::nb_blk4(pic, &n.pos, bx, by, left).map(|(x, y, _)| grid[y * w4 + x]);
        return (get(true), get(false));
    }
    let left_avail = if bx.is_multiple_of(4) { n.a } else { true };
    let top_avail = if by.is_multiple_of(4) { n.b } else { true };
    (
        (left_avail && bx > 0).then(|| grid[by * w4 + bx - 1]),
        (top_avail && by > 0).then(|| grid[(by - 1) * w4 + bx]),
    )
}

/// The neighbours of an 8x8 block for the coded_block_flag of ChromaArrayType 3
/// (9.3.3.1.1.9, ctxBlockCat 5, 9, 13): a neighbouring macroblock that did not
/// use the 8x8 transform has no 8x8 block there, which counts as "not coded"
/// (I_PCM counts as coded).
fn nz_pair_8x8<S: Sample>(
    pic: &Picture<S>,
    comp: usize,
    n: &MbNeighbors,
    bx: usize,
    by: usize,
) -> (Option<u8>, Option<u8>) {
    let grid = nz_grid(pic, comp);
    let w4 = pic.mb_w * 4;
    let masked = |loc: Option<(usize, usize)>| {
        loc.map(|(x, y)| {
            let slot = (y / 4) * pic.mb_w + x / 4;
            if pic.mb_flags[slot] & (FLAG_TRANS8X8 | FLAG_PCM) != 0 {
                grid[y * w4 + x]
            } else {
                0
            }
        })
    };
    if n.pos.active {
        let get = |left| masked(mbaff::nb_blk4(pic, &n.pos, bx, by, left).map(|(x, y, _)| (x, y)));
        return (get(true), get(false));
    }
    let left_avail = if bx.is_multiple_of(4) { n.a } else { true };
    let top_avail = if by.is_multiple_of(4) { n.b } else { true };
    (
        masked((left_avail && bx > 0).then(|| (bx - 1, by))),
        masked((top_avail && by > 0).then(|| (bx, by - 1))),
    )
}

/// The same for a chroma AC 4x4 block at global chroma block coords.
#[inline]
pub(crate) fn chroma_nz_pair<S: Sample>(
    pic: &Picture<S>,
    n: &MbNeighbors,
    comp: usize,
    cx: usize,
    cy: usize,
) -> (Option<u8>, Option<u8>) {
    let w2 = pic.mb_w * 2;
    let grid = &pic.nz_c[comp];
    if n.pos.active {
        let get = |left| mbaff::nb_blk_c(pic, &n.pos, cx, cy, left).map(|(x, y)| grid[y * w2 + x]);
        return (get(true), get(false));
    }
    let cbh = pic.cmb_h / 4;
    let left_avail = if cx.is_multiple_of(2) { n.a } else { true };
    let top_avail = if cy.is_multiple_of(cbh) { n.b } else { true };
    (
        (left_avail && cx > 0).then(|| grid[cy * w2 + cx - 1]),
        (top_avail && cy > 0).then(|| grid[(cy - 1) * w2 + cx]),
    )
}

/// Predicted Intra4x4PredMode / Intra8x8PredMode (8.3.1.1, 8.3.2.1) of the
/// block at global block coordinates `(bx, by)` of an MBAFF macroblock: the
/// smaller of the modes of the blocks Table 6-4 gives on the left and above,
/// DC (2) when either is unavailable or, under constrained intra prediction,
/// inter coded.
fn pred_mode_mbaff<S: Sample>(
    pic: &Picture<S>,
    nbr: &MbNeighbors,
    constrained: bool,
    bx: usize,
    by: usize,
) -> u8 {
    let w4 = pic.mb_w * 4;
    let get = |left: bool| -> Option<u8> {
        let (gx, gy, _) = mbaff::nb_blk4(pic, &nbr.pos, bx, by, left)?;
        let slot = (gy / 4) * pic.mb_w + gx / 4;
        if constrained && pic.mb_flags[slot] & FLAG_INTER != 0 {
            return None;
        }
        Some(pic.i4_modes[gy * w4 + gx])
    };
    match (get(true), get(false)) {
        (Some(a), Some(b)) => a.min(b),
        _ => 2,
    }
}

/// The CABAC neighbourhood of the macroblock at slot `mb_addr`.
fn build_mb_ctx<S: Sample>(
    pic: &Picture<S>,
    ctx: &SliceCtx,
    nbr: &MbNeighbors,
    mb_addr: usize,
) -> MbCtx {
    let info = |addr: usize| MbInfo {
        flags: pic.mb_flags[addr],
        cbp: pic.mb_cbp[addr],
        dc_cbf: pic.mb_dc_cbf[addr],
    };
    if nbr.pos.active {
        let la = mbaff::locate(pic, &nbr.pos, -1, 0, 16, 16);
        let lo = mbaff::locate(pic, &nbr.pos, -1, 8, 16, 16);
        let blk = |l: Option<mbaff::Loc>| l.map_or(1, |l| ((l.yw >> 3) * 2 + 1) as u8);
        return MbCtx {
            a: la.map(|l| info(l.slot)),
            b: mbaff::mb_b(pic, &nbr.pos).map(info),
            qp_delta_inc: ctx.qp_delta_inc,
            field: ctx.is_field(),
            mbaff_a: Some(MbaffA {
                lower: lo.map(|l| info(l.slot)),
                blk: [blk(la), blk(lo)],
            }),
        };
    }
    MbCtx {
        a: nbr.a.then(|| info(mb_addr - 1)),
        b: nbr.b.then(|| info(mb_addr - pic.mb_w)),
        qp_delta_inc: ctx.qp_delta_inc,
        field: ctx.is_field(),
        mbaff_a: None,
    }
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
fn decode_macroblock<S: Sample>(
    pic: &mut Picture<S>,
    refs: &[Picture<S>],
    ls: &[[LevelScale4x4; 3]; 2],
    ls8: &[[LevelScale8x8; 3]; 2],
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
    let nbr = ctx_neighbors(pic, ctx, mb_x, mb_y);
    if ctx.pos.active {
        pic.mb_field[mb_addr] = u8::from(ctx.pos.field);
    }
    if !skipped || !r.is_cabac() {
        r.begin_mb(&build_mb_ctx(pic, ctx, &nbr, mb_addr));
    }
    let w4 = pic.mb_w * 4;
    let (bx0, by0) = (mb_x * 4, mb_y * 4);

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

    if skipped {
        ctx.shapes.skip += 1;
    } else if ctx.slice_type == SliceType::P {
        match mb_type {
            0 => ctx.shapes.p16x16 += 1,
            1 => ctx.shapes.p16x8 += 1,
            2 => ctx.shapes.p8x16 += 1,
            3 | 4 => ctx.shapes.p8x8 += 1,
            _ => ctx.shapes.intra += 1,
        }
    } else if ctx.is_b() {
        ctx.shapes.bidir += 1;
    } else {
        ctx.shapes.intra += 1;
    }

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
        let bd = pic.bd;
        let samples = match pic.cat {
            0 => 256,
            1 => 384,
            2 => 512,
            _ => 768,
        };
        let pcm = r.pcm_block(bd, samples)?;
        // pcm_sample_luma / chroma are `bd`-bit words, most significant bit
        // first; at 8 bits they are the bytes themselves.
        let sample = |i: usize| -> S {
            if bd == 8 {
                S::from_i32(i32::from(pcm[i]))
            } else {
                let mut v = 0i32;
                for b in 0..bd as usize {
                    let bit = i * bd as usize + b;
                    v = (v << 1) | i32::from((pcm[bit >> 3] >> (7 - (bit & 7))) & 1);
                }
                S::from_i32(v)
            }
        };
        let geo = ctx.geo(mb_y);
        for y in 0..16 {
            let dst = pic.y.at(mb_x * 16, geo.y(y));
            for x in 0..16 {
                pic.y.data[dst + x] = sample(y * 16 + x);
            }
        }
        let (cmb_h, cw) = (pic.cmb_h, pic.cw);
        if pic.cat != 0 {
            for (c, plane) in [&mut pic.cb, &mut pic.cr].into_iter().enumerate() {
                let base = 256 + c * cw * cmb_h;
                for y in 0..cmb_h {
                    let dst = plane.at(mb_x * cw, geo.cy(y));
                    for x in 0..cw {
                        plane.data[dst + x] = sample(base + y * cw + x);
                    }
                }
            }
        }
        for dy in 0..4 {
            let base = (by0 + dy) * w4 + bx0;
            pic.nz_y[base..base + 4].fill(16);
            pic.i4_modes[base..base + 4].fill(2);
        }
        fill_chroma_nz(pic, mb_x, mb_y, 16);
        // QPY prediction chain continues unchanged (mb_qp_delta inferred 0);
        // the deblocker substitutes 0 via FLAG_PCM (spec 8.7.2).
        pic.mb_qp[mb_addr] = ctx.qp as i8;
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
    // ChromaArrayType 1 and 2 have the chroma prediction mode and the chroma
    // part of the pattern; 0 and 3 code none of it (7.3.5, 7.4.5).
    let has_chroma = matches!(pic.cat, 1 | 2);
    if is_i16 && !has_chroma && cbp_chroma != 0 {
        return Err(Error::corrupt(
            "Intra_16x16 mb_type with a chroma pattern in a stream without subsampled chroma",
        ));
    }

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
            let pred = if nbr.pos.active {
                pred_mode_mbaff(pic, &nbr, ctx.constrained_intra_pred, bx, by)
            } else if left_avail && top_avail {
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
            let pred = if nbr.pos.active {
                pred_mode_mbaff(pic, &nbr, ctx.constrained_intra_pred, bx, by)
            } else if left_avail && top_avail {
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
    let chroma_mode = if has_chroma {
        r.intra_chroma_pred_mode()?
    } else {
        0
    };

    // coded_block_pattern for I_NxN; Intra_16x16 derives it from mb_type.
    let (cbp_luma, cbp_chroma) = if is_i16 {
        (cbp_luma, cbp_chroma)
    } else {
        r.coded_block_pattern_intra(has_chroma)?
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

/// Set the chroma coefficient counts of a macroblock without a residual to
/// `value` (16 for I_PCM, 0 for a skipped macroblock). Monochrome has none.
fn fill_chroma_nz<S: Sample>(pic: &mut Picture<S>, mb_x: usize, mb_y: usize, value: u8) {
    match pic.cat {
        0 => {}
        3 => {
            let w4 = pic.mb_w * 4;
            for comp in 0..2 {
                for dy in 0..4 {
                    let base = (mb_y * 4 + dy) * w4 + mb_x * 4;
                    pic.nz_c[comp][base..base + 4].fill(value);
                }
            }
        }
        _ => {
            let w2 = pic.mb_w * 2;
            let cbh = pic.cmb_h / 4;
            for comp in 0..2 {
                for dy in 0..cbh {
                    let base = (mb_y * cbh + dy) * w2 + mb_x * 2;
                    pic.nz_c[comp][base..base + 2].fill(value);
                }
            }
        }
    }
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

/// The plane of colour component `comp` (0 Y, 1 Cb, 2 Cr).
#[inline]
fn plane_mut<S: Sample>(pic: &mut Picture<S>, comp: usize) -> &mut PicPlane<S> {
    match comp {
        0 => &mut pic.y,
        1 => &mut pic.cb,
        _ => &mut pic.cr,
    }
}

#[inline]
fn plane_ref<S: Sample>(pic: &Picture<S>, comp: usize) -> &PicPlane<S> {
    match comp {
        0 => &pic.y,
        1 => &pic.cb,
        _ => &pic.cr,
    }
}

/// `residual_luma( )` (7.3.5.3.1) of one colour component plus its
/// reconstruction (8.5): the luma residual, and for ChromaArrayType 3 the Cb
/// and Cr residual too, which is coded and reconstructed exactly like luma
/// (against the plane of `comp`, with `qp` its QP', and `ls` / `ls8` its
/// scaling lists).
///
/// Bitstream order equals reconstruction dependency order, so each block
/// reconstructs the moment its levels are parsed. `bypass` is
/// TransformBypassModeFlag: the levels are the residual (8.5.15).
#[allow(clippy::needless_range_loop, clippy::too_many_arguments)]
fn residual_luma_style<S: Sample>(
    pic: &mut Picture<S>,
    r: &mut Entropy<'_>,
    nbr: &MbNeighbors,
    m: &MbFinish,
    mb_x: usize,
    mb_y: usize,
    comp: usize,
    ls: &LevelScale4x4,
    ls8: &LevelScale8x8,
    qp: i32,
    bypass: bool,
    fld: bool,
    geo: Geo,
    dc_cbf: &mut u8,
) -> Result<()> {
    let bd = pic.bd;
    let max = (1i32 << bd) - 1;
    let w4 = pic.mb_w * 4;
    let (bx0, by0) = (mb_x * 4, mb_y * 4);
    let mul = geo.mul;
    let cbp_luma = m.cbp_luma;
    let c = comp as u8;
    let cat3 = pic.cat == 3;
    let mut scan = [0i32; 16];
    let mut raster = [0i32; 16];
    let mut resid = [0i32; 16];
    if m.is_i16 {
        {
            let pl = plane_mut(pic, comp);
            let mut w = PlaneWindow {
                origin: pl.at(mb_x * 16, geo.y(0)),
                stride: pl.stride * mul,
                data: &mut pl.data,
            };
            pred_16x16(m.i16_mode, bd, &mut w, m.pred_nbr.b, m.pred_nbr.a);
        }
        // Luma DC: un-zigzag over the 4x4 DC array, Hadamard + scale.
        let (na, nb) = nz_pair_in(pic, nz_grid(pic, comp), nbr, bx0, by0);
        let dc_tc = r.residual_block_c(&mut scan, BlockCat::LumaDc, c, na, nb)?;
        *dc_cbf |= u8::from(dc_tc != 0) << comp;
        if fld {
            unfield(&scan, &mut raster);
        } else {
            unzigzag(&scan, &mut raster);
        }
        if !bypass {
            luma_dc_transform(&mut raster, ls, qp);
        }
        let dc = raster;
        // Lossless horizontal / vertical prediction accumulates the residual
        // over the whole macroblock, so every block takes part.
        let dpcm = bypass && m.i16_mode < 2;
        let horizontal = m.i16_mode == 1;
        for blk in 0..16 {
            let (dx, dy) = BLK4_POS[blk];
            let (bx, by) = (bx0 + dx as usize, by0 + dy as usize);
            let tc = if cbp_luma != 0 {
                let (na, nb) = nz_pair_in(pic, nz_grid(pic, comp), nbr, bx, by);
                r.residual_block_c(&mut scan, BlockCat::LumaAc, c, na, nb)?
            } else {
                0
            };
            nz_grid_mut(pic, comp)[by * w4 + bx] = tc;
            let dc_blk = dc[dy as usize * 4 + dx as usize];
            if tc == 0 && dc_blk == 0 && !dpcm {
                continue; // prediction already in place
            }
            if tc > 0 {
                if fld {
                    unfield_ac15(&scan, &mut raster);
                } else {
                    unzigzag_ac15(&scan, &mut raster);
                }
                if !bypass {
                    dequant_4x4(&mut raster, ls, qp, true);
                }
            } else {
                raster = [0; 16]; // DC-only block
            }
            raster[0] = dc_blk;
            let pl = plane_mut(pic, comp);
            let origin = pl.at(mb_x * 16 + dx as usize * 4, geo.y(dy as usize * 4));
            let stride = pl.stride * mul;
            if bypass {
                if dpcm {
                    let first = if horizontal { dx == 0 } else { dy == 0 };
                    add_residual_dpcm(
                        &mut pl.data,
                        bd,
                        stride,
                        origin,
                        4,
                        4,
                        &raster,
                        horizontal,
                        first,
                    );
                } else {
                    add_residual_4x4(&mut pl.data, bd, stride, origin, &raster);
                }
            } else {
                inverse_transform_4x4(&raster, &mut resid);
                add_residual_4x4(&mut pl.data, bd, stride, origin, &resid);
            }
        }
    } else if m.trans8x8 {
        // 8x8 transform (8.5.13): predict, parse and reconstruct one 8x8 block
        // at a time, because an Intra_8x8 block predicts from the reconstructed
        // samples of the blocks before it.
        let mut scan8 = [0i32; 64];
        let mut raster8 = [0i32; 64];
        let mut resid8 = [0i32; 64];
        for blk8 in 0..4 {
            let (x, y) = (mb_x * 16 + (blk8 % 2) * 8, geo.y((blk8 / 2) * 8));
            let (bx, by) = (bx0 + (blk8 % 2) * 2, by0 + (blk8 / 2) * 2);
            let origin = plane_ref(pic, comp).at(x, y);
            let stride = plane_ref(pic, comp).stride * mul;
            if m.intra {
                let n = filter_nbr8(&gather_nbr8_in(
                    &plane_ref(pic, comp).data,
                    &m.pred_nbr,
                    blk8,
                    origin,
                    stride,
                ));
                let mut p = [S::default(); 64];
                pred_8x8(m.modes8[blk8], bd, &n, &mut p);
                let pl = plane_mut(pic, comp);
                for ry in 0..8 {
                    let row = origin + ry * stride;
                    pl.data[row..row + 8].copy_from_slice(&p[ry * 8..ry * 8 + 8]);
                }
            }
            if cbp_luma & (1 << blk8) == 0 {
                let grid = nz_grid_mut(pic, comp);
                for dy in 0..2 {
                    let base = (by + dy) * w4 + bx;
                    grid[base..base + 2].fill(0);
                }
                continue;
            }
            let tc = if r.is_cabac() {
                // ctxBlockCat 5: one 64-coefficient block, and every 4x4 slot
                // of it reports the block's own count to the neighbours
                // (9.3.3.1.1.9 resolves them to the 8x8 transform block). Only
                // ChromaArrayType 3 sends a coded_block_flag for it.
                let cbf = cat3.then(|| nz_pair_8x8(pic, comp, nbr, bx, by));
                let tc = r.residual_block_8x8(&mut scan8, c, cbf)?;
                let grid = nz_grid_mut(pic, comp);
                for dy in 0..2 {
                    let base = (by + dy) * w4 + bx;
                    grid[base..base + 2].fill(tc.min(16));
                }
                tc
            } else {
                // CAVLC codes the same 64 coefficients as four interleaved 4x4
                // blocks (7.3.5.3.1), each with its own nC neighbourhood.
                let mut tc = 0u8;
                for i4 in 0..4 {
                    let (dx, dy) = BLK4_POS[blk8 * 4 + i4];
                    let (sx, sy) = (bx0 + dx as usize, by0 + dy as usize);
                    let (na, nb) = nz_pair_in(pic, nz_grid(pic, comp), nbr, sx, sy);
                    let n = r.residual_block_c(&mut scan, BlockCat::Luma4x4, c, na, nb)?;
                    nz_grid_mut(pic, comp)[sy * w4 + sx] = n;
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
            if fld {
                unfield_8x8(&scan8, &mut raster8);
            } else {
                unzigzag_8x8(&scan8, &mut raster8);
            }
            let pl = plane_mut(pic, comp);
            if bypass {
                if m.intra && m.modes8[blk8] < 2 {
                    add_residual_dpcm(
                        &mut pl.data,
                        bd,
                        stride,
                        origin,
                        8,
                        8,
                        &raster8,
                        m.modes8[blk8] == 1,
                        true,
                    );
                } else {
                    add_residual_8x8(&mut pl.data, bd, stride, origin, &raster8);
                }
            } else {
                dequant_8x8(&mut raster8, ls8, qp);
                inverse_transform_8x8(&raster8, &mut resid8);
                add_residual_8x8(&mut pl.data, bd, stride, origin, &resid8);
            }
        }
    } else {
        for blk in 0..16 {
            let (dx, dy) = BLK4_POS[blk];
            let (bx, by) = (bx0 + dx as usize, by0 + dy as usize);
            let tc = if cbp_luma & (1 << (blk >> 2)) != 0 {
                let (na, nb) = nz_pair_in(pic, nz_grid(pic, comp), nbr, bx, by);
                r.residual_block_c(&mut scan, BlockCat::Luma4x4, c, na, nb)?
            } else {
                0
            };
            nz_grid_mut(pic, comp)[by * w4 + bx] = tc;
            let (x, y) = (mb_x * 16 + dx as usize * 4, geo.y(dy as usize * 4));
            let origin = plane_ref(pic, comp).at(x, y);
            let stride = plane_ref(pic, comp).stride * mul;
            if m.intra {
                let n =
                    gather_nbr4_in(&plane_ref(pic, comp).data, &m.pred_nbr, blk, origin, stride);
                let mut p = [S::default(); 16];
                pred_4x4(m.modes[blk], bd, &n, &mut p);
                let pl = plane_mut(pic, comp);
                if tc == 0 {
                    // Pure prediction: copy the 16 samples in.
                    for ry in 0..4 {
                        let row = origin + ry * stride;
                        pl.data[row..row + 4].copy_from_slice(&p[ry * 4..ry * 4 + 4]);
                    }
                    continue;
                }
                if fld {
                    unfield(&scan, &mut raster);
                } else {
                    unzigzag(&scan, &mut raster);
                }
                if bypass {
                    for ry in 0..4 {
                        let row = origin + ry * stride;
                        pl.data[row..row + 4].copy_from_slice(&p[ry * 4..ry * 4 + 4]);
                    }
                    if m.modes[blk] < 2 {
                        add_residual_dpcm(
                            &mut pl.data,
                            bd,
                            stride,
                            origin,
                            4,
                            4,
                            &raster,
                            m.modes[blk] == 1,
                            true,
                        );
                    } else {
                        add_residual_4x4(&mut pl.data, bd, stride, origin, &raster);
                    }
                    continue;
                }
                dequant_4x4(&mut raster, ls, qp, false);
                inverse_transform_4x4(&raster, &mut resid);
                for ry in 0..4 {
                    let row = origin + ry * stride;
                    for rx in 0..4 {
                        let v = p[ry * 4 + rx].to_i32() + resid[ry * 4 + rx];
                        pl.data[row + rx] = S::clip(v, max);
                    }
                }
            } else {
                if tc == 0 {
                    continue; // motion-compensated prediction already in place
                }
                if fld {
                    unfield(&scan, &mut raster);
                } else {
                    unzigzag(&scan, &mut raster);
                }
                let pl = plane_mut(pic, comp);
                if bypass {
                    add_residual_4x4(&mut pl.data, bd, stride, origin, &raster);
                } else {
                    dequant_4x4(&mut raster, ls, qp, false);
                    inverse_transform_4x4(&raster, &mut resid);
                    add_residual_4x4(&mut pl.data, bd, stride, origin, &resid);
                }
            }
        }
    }
    Ok(())
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
fn finish_macroblock<S: Sample>(
    pic: &mut Picture<S>,
    ls: &[LevelScale4x4; 3],
    ls8: &[LevelScale8x8; 3],
    r: &mut Entropy<'_>,
    ctx: &mut SliceCtx,
    mb_addr: usize,
    nbr: &MbNeighbors,
    m: MbFinish,
) -> Result<()> {
    let mb_x = mb_addr % pic.mb_w;
    let mb_y = mb_addr / pic.mb_w;
    let bd = pic.bd;
    let qp_bd = 6 * (bd as i32 - 8);
    let w2 = pic.mb_w * 2;
    let c422 = pic.cat == 2;
    let c_sub = matches!(pic.cat, 1 | 2);
    let cbh = pic.cmb_h / 4;
    let (cx0, cy0) = (mb_x * 2, mb_y * cbh);
    let (cbp_luma, cbp_chroma) = (m.cbp_luma, m.cbp_chroma);
    let fld = ctx.is_field();
    let geo = ctx.geo(mb_y);
    let mul = geo.mul;

    // mb_qp_delta (7.4.5).
    let mut qp_delta = 0;
    if cbp_luma != 0 || cbp_chroma != 0 || m.is_i16 {
        qp_delta = r.mb_qp_delta()?;
        // 7.4.5: the delta spans the extended QP range, and QPY wraps modulo
        // 52 + QpBdOffsetY back into [-QpBdOffsetY, 51].
        if qp_delta < -(26 + qp_bd / 2) || qp_delta > 25 + qp_bd / 2 {
            return Err(Error::corrupt("mb_qp_delta outside [-26, 25]"));
        }
        ctx.qp = (ctx.qp + qp_delta + 52 + 2 * qp_bd) % (52 + qp_bd) - qp_bd;
    }
    ctx.qp_delta_inc = u8::from(qp_delta != 0);
    pic.mb_qp[mb_addr] = ctx.qp as i8;
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
    // The scaling process takes QP'Y = QPY + QpBdOffsetY and QP'C = QPC +
    // QpBdOffsetC (8.5.8); QPC maps qPI = Clip3(-QpBdOffsetC, 51, QPY + off).
    let qp_y = ctx.qp + qp_bd;
    let qp_cb = chroma_qp(ctx.qp, ctx.cb_qp_offset, qp_bd) + qp_bd;
    let qp_cr = chroma_qp(ctx.qp, ctx.cr_qp_offset, qp_bd) + qp_bd;

    let mut dc_cbf = 0u8;
    let bypass = pic.bypass_enabled && qp_y == 0;
    let ncomp = if pic.cat == 3 { 3 } else { 1 };
    let qps = [qp_y, qp_cb, qp_cr];
    for comp in 0..ncomp {
        residual_luma_style(
            pic,
            r,
            nbr,
            &m,
            mb_x,
            mb_y,
            comp,
            &ls[comp],
            &ls8[comp],
            qps[comp],
            bypass,
            fld,
            geo,
            &mut dc_cbf,
        )?;
    }
    let mut scan = [0i32; 16];
    let mut raster = [0i32; 16];
    let mut resid = [0i32; 16];

    // Chroma of 4:2:0 and 4:2:2: predict both components (intra only), then DC
    // (Cb, Cr), then AC in bitstream order, reconstructing as levels arrive.
    // (ChromaArrayType 3 coded its chroma like luma above; 0 has none.)
    if c_sub {
        if m.intra {
            for comp in 0..2 {
                let plane = if comp == 0 { &mut pic.cb } else { &mut pic.cr };
                let mut w = PlaneWindow {
                    origin: plane.at(mb_x * 8, geo.cy(0)),
                    stride: plane.stride * mul,
                    data: &mut plane.data,
                };
                if c422 {
                    pred_chroma_8x16(m.chroma_mode, bd, &mut w, m.pred_nbr.b, m.pred_nbr.a4);
                } else {
                    pred_chroma_8x8_halves(
                        m.chroma_mode,
                        bd,
                        &mut w,
                        m.pred_nbr.b,
                        [
                            m.pred_nbr.a4[0] && m.pred_nbr.a4[1],
                            m.pred_nbr.a4[2] && m.pred_nbr.a4[3],
                        ],
                    );
                }
            }
        }
        // Lossless horizontal / vertical chroma prediction accumulates the
        // residual over the whole chroma block (8.5.15), so every block of it
        // takes part.
        let dpcm = bypass && m.intra && matches!(m.chroma_mode, 1 | 2);
        let horizontal = m.chroma_mode == 1;
        let mut chroma_dc = [[0i32; 8]; 2];
        if cbp_chroma != 0 {
            for (comp, c) in chroma_dc.iter_mut().enumerate() {
                let (ls_c, qp_c) = if comp == 0 {
                    (&ls[1], qp_cb)
                } else {
                    (&ls[2], qp_cr)
                };
                if c422 {
                    let tc =
                        r.residual_block(&mut scan, BlockCat::ChromaDc422(comp as u8), None, None)?;
                    dc_cbf |= u8::from(tc != 0) << (1 + comp);
                    c.copy_from_slice(&scan[..8]);
                    if bypass {
                        // The levels are the DC values, in raster order.
                        for (rr, &sc) in CHROMA_DC_422_RASTER.iter().enumerate() {
                            c[rr] = scan[sc];
                        }
                    } else {
                        chroma_dc_transform_422(c, ls_c, qp_c);
                    }
                } else {
                    let tc =
                        r.residual_block(&mut scan, BlockCat::ChromaDc(comp as u8), None, None)?;
                    dc_cbf |= u8::from(tc != 0) << (1 + comp);
                    let mut c4 = [0i32; 4];
                    c4.copy_from_slice(&scan[..4]);
                    if !bypass {
                        chroma_dc_transform_420(&mut c4, ls_c, qp_c);
                    }
                    c[..4].copy_from_slice(&c4);
                }
            }
        }
        for comp in 0..2 {
            for blk in 0..cbh * 2 {
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
                if tc == 0 && dc_blk == 0 && !dpcm {
                    continue;
                }
                let (plane, ls_c, qp_c) = if comp == 0 {
                    (&mut pic.cb, &ls[1], qp_cb)
                } else {
                    (&mut pic.cr, &ls[2], qp_cr)
                };
                if tc > 0 {
                    if fld {
                        unfield_ac15(&scan, &mut raster);
                    } else {
                        unzigzag_ac15(&scan, &mut raster);
                    }
                    if !bypass {
                        dequant_4x4(&mut raster, ls_c, qp_c, true);
                    }
                } else {
                    raster = [0; 16]; // DC-only block
                }
                raster[0] = dc_blk;
                let origin = plane.at(mb_x * 8 + (blk & 1) * 4, geo.cy((blk >> 1) * 4));
                let stride = plane.stride * mul;
                if bypass {
                    if dpcm {
                        let first = if horizontal {
                            blk & 1 == 0
                        } else {
                            blk >> 1 == 0
                        };
                        add_residual_dpcm(
                            &mut plane.data,
                            bd,
                            stride,
                            origin,
                            4,
                            4,
                            &raster,
                            horizontal,
                            first,
                        );
                    } else {
                        add_residual_4x4(&mut plane.data, bd, stride, origin, &raster);
                    }
                } else {
                    inverse_transform_4x4(&raster, &mut resid);
                    add_residual_4x4(&mut plane.data, bd, stride, origin, &resid);
                }
            }
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
fn decode_inter_mb<S: Sample>(
    pic: &mut Picture<S>,
    refs: &[Picture<S>],
    ls: &[[LevelScale4x4; 3]; 2],
    ls8: &[[LevelScale8x8; 3]; 2],
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
        pos: ctx.pos,
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
                let nref = if ctx.pos.active && ctx.pos.field {
                    2 * ctx.num_ref_idx[list]
                } else {
                    ctx.num_ref_idx[list]
                };
                let idx = if infer_ref0 || nref <= 1 {
                    0
                } else {
                    let (px, py) = part_origin(&shape, part, (0, 0));
                    let bx = (mb_x * 4 + px) as i32;
                    let by = (mb_y * 4 + py) as i32;
                    let inc = ref_idx_cond(pic, &mvc, bx - 1, by, list)
                        + 2 * ref_idx_cond(pic, &mvc, bx, by - 1, list);
                    r.ref_idx(nref as u32 - 1, inc)? as i8
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
                S::compensate(pic, refs, ctx, mb_x, mb_y, px, py, pw, ph, mv, ref_idx)?;
            }
        }
    }

    if skipped {
        // 7.4.5: a skipped macroblock has no residual and does not change QPY.
        let w4 = pic.mb_w * 4;
        for dy in 0..4 {
            let base = (mb_y * 4 + dy) * w4 + mb_x * 4;
            pic.nz_y[base..base + 4].fill(0);
            pic.i4_modes[base..base + 4].fill(2);
        }
        fill_chroma_nz(pic, mb_x, mb_y, 0);
        pic.mb_qp[mb_addr] = ctx.qp as i8;
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
    let (cbp_luma, cbp_chroma) = r.coded_block_pattern_inter(matches!(pic.cat, 1 | 2))?;
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
fn predict_p_skip<S: Sample>(
    pic: &mut Picture<S>,
    refs: &[Picture<S>],
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
    S::compensate(
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
fn predict_direct<S: Sample>(
    pic: &mut Picture<S>,
    refs: &[Picture<S>],
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
fn predict_direct_8x8<S: Sample>(
    pic: &mut Picture<S>,
    refs: &[Picture<S>],
    ctx: &SliceCtx,
    mvc: &mut MvCtx,
    part: usize,
) -> Result<()> {
    direct_quadrant(pic, refs, ctx, mvc, part, false)
}

/// One 8x8 quadrant of a direct-mode macroblock. With
/// `direct_8x8_inference_flag` the whole quadrant shares the motion of its
/// outer corner 4x4 block; without it each 4x4 block is derived on its own.
fn direct_quadrant<S: Sample>(
    pic: &mut Picture<S>,
    refs: &[Picture<S>],
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
        S::compensate(pic, refs, ctx, mb_x, mb_y, qx, qy, 2, 2, mv, ref_idx)
    } else {
        // Each 4x4 block derives its own motion, but they usually come out
        // equal: interpolation is position-invariant, so one 8x8 compensation
        // of the shared vector produces the same samples as four 4x4 ones and
        // pays the per-partition setup once instead of four times.
        let mut motion = [([[0i16; 2]; 2], [0i8; 2]); 4];
        for dy in 0..2 {
            for dx in 0..2 {
                let (px, py) = (qx + dx, qy + dy);
                let (mv, ref_idx, ref_id) = direct_block(pic, refs, ctx, mvc, (px, py))?;
                write_block(pic, mvc, px, py, mv, ref_idx, ref_id, flags);
                motion[dy * 2 + dx] = (mv, ref_idx);
            }
        }
        if motion.iter().all(|m| *m == motion[0]) {
            let (mv, ref_idx) = motion[0];
            return S::compensate(pic, refs, ctx, mb_x, mb_y, qx, qy, 2, 2, mv, ref_idx);
        }
        for dy in 0..2 {
            for dx in 0..2 {
                let (mv, ref_idx) = motion[dy * 2 + dx];
                S::compensate(
                    pic,
                    refs,
                    ctx,
                    mb_x,
                    mb_y,
                    qx + dx,
                    qy + dy,
                    1,
                    1,
                    mv,
                    ref_idx,
                )?;
            }
        }
        Ok(())
    }
}

/// Motion of one direct-mode 4x4 block: `(mvLX, refIdxLX, reference picture
/// identity)`, spatial (8.4.1.2.2) or temporal (8.4.1.2.3).
#[allow(clippy::type_complexity)]
fn direct_block<S: Sample>(
    pic: &Picture<S>,
    refs: &[Picture<S>],
    ctx: &SliceCtx,
    mvc: &MvCtx,
    at: (usize, usize),
) -> Result<([[i16; 2]; 2], [i8; 2], [i32; 2])> {
    // The co-located block, in both modes (8.4.1.2.1): its list 0 motion, or
    // its list 1 motion when list 0 is unused. `col_ref_idx` is refIdxCol —
    // an index into the co-located picture's own reference list, not ours.
    let (col_ref_id, col_mv, col_ref_idx, vscale) = colocated(ctx, refs, mvc, at);
    let col = ctx.reference(refs, 1, 0);

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
        map_col_to_list0(ctx, refs, col_ref_id, vscale)
    };
    let col_mv = match vscale {
        // 8-195 and 8-196: "/" truncates toward zero.
        COL_FRM_TO_FLD => [col_mv[0], col_mv[1] / 2],
        COL_FLD_TO_FRM => [col_mv[0], col_mv[1].wrapping_mul(2)],
        _ => col_mv,
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
    let tb = (ctx.cur_poc() - p0.poc).clamp(-128, 127);
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
fn map_col_to_list0<S: Sample>(
    ctx: &SliceCtx,
    refs: &[Picture<S>],
    col_ref_id: i32,
    vscale: u8,
) -> i8 {
    // Picture ids step by 4: a field id is its frame's id + 1 + parity.
    let field_mb = ctx.pos.active && ctx.pos.field;
    let want = match vscale {
        // The field of the referenced frame with the current picture's (or
        // macroblock's) parity.
        COL_FRM_TO_FLD => col_ref_id + i32::from(ctx.mb_parity),
        // A field macroblock of an MBAFF frame names the field of the frame
        // the co-located field macroblock referenced that has its own parity.
        _ if field_mb && MBAFF_ONE_TO_ONE_SAME_PARITY => {
            (col_ref_id & !3) + i32::from(ctx.mb_parity)
        }
        _ => col_ref_id,
    };
    let n = if field_mb {
        2 * ctx.lists[0].len()
    } else {
        ctx.lists[0].len()
    };
    for i in 0..n {
        let hit = ctx.reference(refs, 0, i as i8).is_some_and(|p| {
            if vscale == COL_FLD_TO_FRM {
                p.id == col_ref_id & !3
            } else {
                p.id == want
            }
        });
        if hit {
            return i as i8;
        }
    }
    0
}

/// Whether a field macroblock of an MBAFF frame whose co-located block is a
/// field macroblock maps the referenced field to the field of the same parity
/// as itself (true) or to that very field (false).
const MBAFF_ONE_TO_ONE_SAME_PARITY: bool = false;

/// `vertMvScale` of 8.4.1.2.1.
const COL_ONE_TO_ONE: u8 = 0;
const COL_FRM_TO_FLD: u8 = 1;
const COL_FLD_TO_FRM: u8 = 2;

/// [`colocated`] for a frame macroblock or field macroblock of an MBAFF frame.
#[inline(never)]
fn colocated_mbaff<S: Sample>(
    ctx: &SliceCtx,
    refs: &[Picture<S>],
    mvc: &MvCtx,
    at: (usize, usize),
) -> (i32, [i16; 2], i8, u8) {
    const NONE: (i32, [i16; 2], i8, u8) = (-1, [0; 2], -1, COL_ONE_TO_ONE);
    let Some((k, _)) = ctx.lists[1].get_ref(0) else {
        return NONE;
    };
    let Some(store) = refs.get(k) else {
        return NONE;
    };
    let (mb_x, mb_y) = (mvc.mb_x, mvc.mb_y);
    let w = store.mb_w.max(1);
    let (c, bx, by, vs) = {
        let field_mb = ctx.pos.field;
        let bot = mb_y & 1;
        let top = mb_y & !1;
        // The field of a complementary pair, or of an MBAFF frame's field
        // macroblock pair, a frame macroblock takes its co-located block from.
        let by_poc =
            usize::from((store.fpoc[0] - ctx.poc).abs() >= (store.fpoc[1] - ctx.poc).abs());
        if store.coding == CODED_FLD {
            let p = if field_mb { bot } else { by_poc };
            let Some(f) = store.fields.get(p) else {
                return NONE;
            };
            if field_mb {
                (f, mb_x * 4 + at.0, (mb_y / 2) * 4 + at.1, COL_ONE_TO_ONE)
            } else {
                // mbAddrCol5 and yM = 8 * (CurrMbAddr % 2) + 4 * (yCol / 8).
                let by = (mb_y / 2) * 4 + 2 * bot + at.1 / 2;
                (f, mb_x * 4 + at.0, by, COL_FLD_TO_FRM)
            }
        } else {
            let col_field = store.mb_field.get(top * w + mb_x).copied().unwrap_or(0) != 0;
            match (field_mb, col_field) {
                (false, false) => (store, mb_x * 4 + at.0, mb_y * 4 + at.1, COL_ONE_TO_ONE),
                (false, true) => {
                    let slot = top + by_poc;
                    let by = slot * 4 + 2 * bot + at.1 / 2;
                    (store, mb_x * 4 + at.0, by, COL_FLD_TO_FRM)
                }
                (true, false) => {
                    let slot = top + at.1 / 2;
                    let by = slot * 4 + (2 * at.1) % 4;
                    (store, mb_x * 4 + at.0, by, COL_FRM_TO_FLD)
                }
                (true, true) => (store, mb_x * 4 + at.0, mb_y * 4 + at.1, COL_ONE_TO_ONE),
            }
        }
    };
    let idx = by * c.mb_w.max(1) * 4 + bx;
    if idx < c.ref_id.len() && c.blk[idx] & BLK_INTRA == 0 {
        let list = usize::from(c.ref_id[idx][0] < 0);
        (
            c.ref_id[idx][list],
            c.mv[idx][list],
            c.ref_idx[idx][list],
            vs,
        )
    } else {
        (-1, [0; 2], -1, vs)
    }
}

/// The co-located block of 8.4.1.2.1 for the 4x4 block `at` of the current
/// macroblock: `(refPicCol identity, mvCol, refIdxCol, vertMvScale)`; the
/// identity is -1 for an intra block.
fn colocated<S: Sample>(
    ctx: &SliceCtx,
    refs: &[Picture<S>],
    mvc: &MvCtx,
    at: (usize, usize),
) -> (i32, [i16; 2], i8, u8) {
    const NONE: (i32, [i16; 2], i8, u8) = (-1, [0; 2], -1, COL_ONE_TO_ONE);
    let Some((k, _)) = ctx.lists[1].get_ref(0) else {
        return NONE;
    };
    let Some(store) = refs.get(k) else {
        return NONE;
    };
    if ctx.pos.active {
        return colocated_mbaff(ctx, refs, mvc, at);
    }
    let Some(cur) = ctx.reference(refs, 1, 0) else {
        return NONE;
    };
    let (mb_x, mb_y) = (mvc.mb_x, mvc.mb_y);
    let w = store.mb_w.max(1);
    // Table 8-6 and Table 8-8. `mb_y` is a slot row for an MBAFF frame (the
    // pair is `mb_y / 2`, the macroblock of the pair `mb_y % 2`).
    let (c, bx, by, vs) = if ctx.structure != FRAME {
        if store.coding == CODED_FLD {
            (cur, mb_x * 4 + at.0, mb_y * 4 + at.1, COL_ONE_TO_ONE)
        } else if store.coding == CODED_AFRM {
            let top = 2 * mb_y;
            if store.mb_field.get(top * w + mb_x).copied().unwrap_or(0) == 0 {
                // mbAddrCol6 = 2 * CurrMbAddr + (yCol / 8), yM = (2 * yCol) % 16.
                let by = (top + at.1 / 2) * 4 + (2 * at.1) % 4;
                (store, mb_x * 4 + at.0, by, COL_FRM_TO_FLD)
            } else {
                // mbAddrCol7 = 2 * CurrMbAddr + bottom_field_flag.
                let slot = top + usize::from(ctx.structure == BOTTOM);
                (store, mb_x * 4 + at.0, slot * 4 + at.1, COL_ONE_TO_ONE)
            }
        } else {
            // mbAddrCol1 and yM = (2 * yCol) % 16.
            let by = (2 * mb_y + at.1 / 2) * 4 + (2 * at.1) % 4;
            (store, mb_x * 4 + at.0, by, COL_FRM_TO_FLD)
        }
    } else if store.coding == CODED_FLD {
        let top = (store.fpoc[0] - ctx.poc).abs();
        let bottom = (store.fpoc[1] - ctx.poc).abs();
        let p = usize::from(top >= bottom);
        let Some(f) = store.fields.get(p) else {
            return NONE;
        };
        // mbAddrCol4 and yM = 8 * ((CurrMbAddr / PicWidthInMbs) % 2) + 4 * (yCol / 8).
        let by = (mb_y / 2) * 4 + 2 * (mb_y % 2) + at.1 / 2;
        (f, mb_x * 4 + at.0, by, COL_FLD_TO_FRM)
    } else {
        (store, mb_x * 4 + at.0, mb_y * 4 + at.1, COL_ONE_TO_ONE)
    };
    let idx = by * c.mb_w.max(1) * 4 + bx;
    if idx < c.ref_id.len() && c.blk[idx] & BLK_INTRA == 0 {
        let list = usize::from(c.ref_id[idx][0] < 0);
        (
            c.ref_id[idx][list],
            c.mv[idx][list],
            c.ref_idx[idx][list],
            vs,
        )
    } else {
        (-1, [0; 2], -1, vs)
    }
}

/// The motion vectors chroma prediction uses: the luma vectors, with the
/// vertical offset of 8.4.1.4 added when a field predicts from a field of the
/// opposite parity.
#[inline]
fn chroma_mvs<S: Sample>(
    ctx: &SliceCtx,
    refs: &[Picture<S>],
    ref_idx: [i8; 2],
    mv: [[i16; 2]; 2],
) -> [[i16; 2]; 2] {
    let mut out = mv;
    if ctx.cat == 2 {
        // 4:2:2 chroma is full height: the vertical vector stays in quarter
        // samples of the chroma plane, which the eighth-sample interpolator
        // reads as twice the value.
        for l in out.iter_mut() {
            l[1] = l[1].wrapping_mul(2);
        }
    } else if ctx.is_field() {
        for l in 0..2 {
            if let Some(rp) = ctx.reference(refs, l, ref_idx[l]) {
                out[l][1] = out[l][1].wrapping_add(ctx.chroma_dy(rp));
            }
        }
    }
    out
}

/// The reference plane (`0` luma, `1` Cb, `2` Cr) list `list` predicts from.
#[inline(always)]
fn ref_plane<'a>(
    ctx: &SliceCtx,
    refs: &'a [Picture<u8>],
    ref_idx: [i8; 2],
    list: usize,
    comp: usize,
) -> Result<RefPlane<'a>> {
    let rp = ctx.reference(refs, list, ref_idx[list]).ok_or_else(|| {
        Error::corrupt("reference index names a picture the buffer does not hold")
    })?;
    let src = match comp {
        0 => &rp.y,
        1 => &rp.cb,
        _ => &rp.cr,
    };
    Ok(RefPlane {
        data: &src.data,
        stride: src.stride,
        origin: src.origin,
        width: src.width,
        height: src.height,
        pad: src.pad,
    })
}

/// Motion compensation of one partition (clause 8.4.2): interpolate each list's
/// prediction, then combine them by the weighting of 8.4.2.3 and write the
/// result straight into the picture, where the residual is added on top.
///
/// `(px, py)` and `(pw, ph)` are in 4x4 blocks within the macroblock.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compensate8(
    pic: &mut Picture<u8>,
    refs: &[Picture<u8>],
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
    let geo = ctx.geo(mb_y);
    let x0 = (mb_x * 16 + px * 4) as i32;
    let y0 = (geo.src_y + py * 4) as i32;
    let (w, h) = (pw * 4, ph * 4);
    // A stream carrying no weighted prediction at all resolves every
    // component to the identity weighting without the per-component lookup.
    let plain = ctx.no_weights();
    // ChromaArrayType 3 predicts Cb and Cr with the luma interpolation, on
    // their own planes and weights.
    let ncomp = if ctx.cat == 3 { 3 } else { 1 };
    for comp in 0..ncomp {
        let wt = if plain {
            Weights::DEFAULT
        } else {
            ctx.weights_for(refs, comp, ref_idx[0], ref_idx[1], use0, use1)
        };
        // 8.4.2.3.1 over one list is the identity, so the interpolation of that
        // one list IS the prediction: it is written straight into the picture at
        // the picture's own pitch. Over both lists the combined prediction is the
        // rounded byte average of the two clipped ones, so list 0 interpolates
        // into the picture and list 1 averages on top of it (Put::Avg) — no
        // temporaries, no combining pass, no copy back.
        let direct = wt.is_default();
        let stride = plane_ref(pic, comp).stride * geo.mul;
        let origin = plane_ref(pic, comp).at(x0 as usize, geo.y(py * 4));
        // Weighted bi-prediction fuses the same way: the combining formula is
        // per-sample, so list 0 lands in the picture and list 1 combines on top
        // of it (Put::AvgW) with 8.4.2.3.2's exact integers.
        let weighted_put = if !direct && use0 && use1 {
            Some(crate::inter::Put::AvgW {
                w0: wt.w[0],
                w1: wt.w[1],
                shift: (wt.log_wd + 1) as u32,
                round: 1 << wt.log_wd,
                off: (wt.o[0] + wt.o[1] + 1) >> 1,
            })
        } else {
            None
        };
        if direct && use0 && use1 {
            let plane = ref_plane(ctx, refs, ref_idx, 0, comp)?;
            mc_luma(
                &plane,
                x0,
                y0,
                mv[0],
                w,
                h,
                stride,
                &mut plane_mut(pic, comp).data[origin..],
                crate::inter::Put::Set,
            );
            let plane = ref_plane(ctx, refs, ref_idx, 1, comp)?;
            mc_luma(
                &plane,
                x0,
                y0,
                mv[1],
                w,
                h,
                stride,
                &mut plane_mut(pic, comp).data[origin..],
                crate::inter::Put::Avg,
            );
        } else if direct {
            // No scratch buffer is declared on this path at all: the scratch is
            // zeroed on entry, and most partitions of a P slice come through here.
            let list = usize::from(!use0);
            let plane = ref_plane(ctx, refs, ref_idx, list, comp)?;
            mc_luma(
                &plane,
                x0,
                y0,
                mv[list],
                w,
                h,
                stride,
                &mut plane_mut(pic, comp).data[origin..],
                crate::inter::Put::Set,
            );
        } else if let Some(put) = weighted_put {
            let plane = ref_plane(ctx, refs, ref_idx, 0, comp)?;
            mc_luma(
                &plane,
                x0,
                y0,
                mv[0],
                w,
                h,
                stride,
                &mut plane_mut(pic, comp).data[origin..],
                crate::inter::Put::Set,
            );
            let plane = ref_plane(ctx, refs, ref_idx, 1, comp)?;
            mc_luma(
                &plane,
                x0,
                y0,
                mv[1],
                w,
                h,
                stride,
                &mut plane_mut(pic, comp).data[origin..],
                put,
            );
        } else {
            let mut part = [[0u8; 256]; 2];
            for list in 0..2 {
                if ref_idx[list] < 0 {
                    continue;
                }
                let plane = ref_plane(ctx, refs, ref_idx, list, comp)?;
                mc_luma(
                    &plane,
                    x0,
                    y0,
                    mv[list],
                    w,
                    h,
                    w,
                    &mut part[list],
                    crate::inter::Put::Set,
                );
            }
            let mut out = [0u8; 256];
            let (p0, p1) = part.split_at(1);
            combine(&mut out, &p0[0], &p1[0], use0, use1, &wt, w * h);
            for row in 0..h {
                let dst = origin + row * stride;
                plane_mut(pic, comp).data[dst..dst + w].copy_from_slice(&out[row * w..row * w + w]);
            }
        }
    }

    // Chroma (8.4.1.4): for 4:2:0 frame coding the chroma vector is the luma
    // vector, read as eighth-sample units over the half-resolution plane.
    if ctx.cat == 0 || ctx.cat == 3 {
        return Ok(());
    }
    let cmv = chroma_mvs(ctx, refs, ref_idx, mv);
    let c422 = ctx.cat == 2;
    let (cw, ch) = (w / 2, if c422 { h } else { h / 2 });
    let (cx0, cy0) = (x0 / 2, if c422 { y0 } else { y0 / 2 });
    let crow = if c422 { py * 4 } else { py * 2 };
    for comp in 0..2 {
        let wt = if plain {
            Weights::DEFAULT
        } else {
            ctx.weights_for(refs, comp + 1, ref_idx[0], ref_idx[1], use0, use1)
        };
        let direct = wt.is_default();
        // The combining weights are per-component syntax (luma and chroma
        // carry their own denoms, weights and offsets), so this component's
        // fused mode is built from ITS OWN lookup — never the luma one.
        let weighted_put = if !direct && use0 && use1 {
            Some(crate::inter::Put::AvgW {
                w0: wt.w[0],
                w1: wt.w[1],
                shift: (wt.log_wd + 1) as u32,
                round: 1 << wt.log_wd,
                off: (wt.o[0] + wt.o[1] + 1) >> 1,
            })
        } else {
            None
        };
        let plane_out = if comp == 0 { &mut pic.cb } else { &mut pic.cr };
        let stride = plane_out.stride * geo.mul;
        let origin = plane_out.at(cx0 as usize, geo.cy(crow));
        if direct && use0 && use1 {
            let plane = ref_plane(ctx, refs, ref_idx, 0, 1 + comp)?;
            mc_chroma(
                &plane,
                cx0,
                cy0,
                cmv[0],
                cw,
                ch,
                stride,
                &mut plane_out.data[origin..],
                crate::inter::Put::Set,
            );
            let plane = ref_plane(ctx, refs, ref_idx, 1, 1 + comp)?;
            mc_chroma(
                &plane,
                cx0,
                cy0,
                cmv[1],
                cw,
                ch,
                stride,
                &mut plane_out.data[origin..],
                crate::inter::Put::Avg,
            );
        } else if let Some(put) = weighted_put {
            let plane = ref_plane(ctx, refs, ref_idx, 0, 1 + comp)?;
            mc_chroma(
                &plane,
                cx0,
                cy0,
                cmv[0],
                cw,
                ch,
                stride,
                &mut plane_out.data[origin..],
                crate::inter::Put::Set,
            );
            let plane = ref_plane(ctx, refs, ref_idx, 1, 1 + comp)?;
            mc_chroma(
                &plane,
                cx0,
                cy0,
                cmv[1],
                cw,
                ch,
                stride,
                &mut plane_out.data[origin..],
                put,
            );
        } else if direct {
            let list = usize::from(!use0);
            let plane = ref_plane(ctx, refs, ref_idx, list, 1 + comp)?;
            mc_chroma(
                &plane,
                cx0,
                cy0,
                cmv[list],
                cw,
                ch,
                stride,
                &mut plane_out.data[origin..],
                crate::inter::Put::Set,
            );
        } else {
            let mut cpart = [[0u8; 128]; 2];
            for list in 0..2 {
                if ref_idx[list] < 0 {
                    continue;
                }
                let plane = ref_plane(ctx, refs, ref_idx, list, 1 + comp)?;
                mc_chroma(
                    &plane,
                    cx0,
                    cy0,
                    cmv[list],
                    cw,
                    ch,
                    cw,
                    &mut cpart[list],
                    crate::inter::Put::Set,
                );
            }
            let mut cout = [0u8; 128];
            let (c0, c1) = cpart.split_at(1);
            combine(&mut cout, &c0[0], &c1[0], use0, use1, &wt, cw * ch);
            for row in 0..ch {
                let dst = origin + row * stride;
                plane_out.data[dst..dst + cw].copy_from_slice(&cout[row * cw..row * cw + cw]);
            }
        }
    }
    Ok(())
}

/// Motion compensation of one partition over `u16` planes: the same clause
/// 8.4.2 process as [`compensate8`], computed per sample from the equations.
/// Weighted prediction offsets scale by `1 << (BitDepth - 8)` (8-298, 8-299).
#[allow(clippy::too_many_arguments)]
pub(crate) fn compensate16(
    pic: &mut Picture<u16>,
    refs: &[Picture<u16>],
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
    let bd = pic.bd;
    let max = (1i32 << bd) - 1;
    let geo = ctx.geo(mb_y);
    let x0 = (mb_x * 16 + px * 4) as i32;
    let y0 = (geo.src_y + py * 4) as i32;
    let plain = ctx.no_weights();
    let cmv = chroma_mvs(ctx, refs, ref_idx, mv);
    let cat = pic.cat;
    let ncomp = if cat == 0 { 1 } else { 3 };
    for comp in 0..ncomp {
        let c422 = cat == 2;
        let (w, h, cx, cy) = if comp == 0 || cat == 3 {
            (pw * 4, ph * 4, x0, y0)
        } else if c422 {
            (pw * 2, ph * 4, x0 / 2, y0)
        } else {
            (pw * 2, ph * 2, x0 / 2, y0 / 2)
        };
        let mut wt = if plain {
            Weights::DEFAULT
        } else {
            ctx.weights_for(refs, comp, ref_idx[0], ref_idx[1], use0, use1)
        };
        wt.o[0] <<= bd - 8;
        wt.o[1] <<= bd - 8;
        let mut pred = [[0i32; 256]; 2];
        for list in 0..2 {
            if ref_idx[list] < 0 {
                continue;
            }
            let rp = ctx.reference(refs, list, ref_idx[list]).ok_or_else(|| {
                Error::corrupt("reference index names a picture the buffer does not hold")
            })?;
            match comp {
                0 => crate::inter16::luma_pred(&rp.y, cx, cy, mv[list], w, h, max, &mut pred[list]),
                1 if cat == 3 => {
                    crate::inter16::luma_pred(&rp.cb, cx, cy, mv[list], w, h, max, &mut pred[list])
                }
                _ if cat == 3 => {
                    crate::inter16::luma_pred(&rp.cr, cx, cy, mv[list], w, h, max, &mut pred[list])
                }
                1 => crate::inter16::chroma_pred(&rp.cb, cx, cy, cmv[list], w, h, &mut pred[list]),
                _ => crate::inter16::chroma_pred(&rp.cr, cx, cy, cmv[list], w, h, &mut pred[list]),
            }
        }
        let plane = match comp {
            0 => &mut pic.y,
            1 => &mut pic.cb,
            _ => &mut pic.cr,
        };
        let stride = plane.stride * geo.mul;
        let origin = if comp == 0 || cat == 3 {
            plane.at(cx as usize, geo.y(py * 4))
        } else {
            plane.at(cx as usize, geo.cy(if c422 { py * 4 } else { py * 2 }))
        };
        let round1 = if wt.log_wd >= 1 {
            1 << (wt.log_wd - 1)
        } else {
            0
        };
        for row in 0..h {
            for col in 0..w {
                let i = row * w + col;
                let v = if use0 && use1 {
                    // 8-301 / 8-300 (default weights reduce to the rounded mean).
                    ((pred[0][i] * wt.w[0] + pred[1][i] * wt.w[1] + (1 << wt.log_wd))
                        >> (wt.log_wd + 1))
                        + ((wt.o[0] + wt.o[1] + 1) >> 1)
                } else {
                    let k = usize::from(!use0);
                    if wt.log_wd >= 1 {
                        ((pred[k][i] * wt.w[k] + round1) >> wt.log_wd) + wt.o[k]
                    } else {
                        pred[k][i] * wt.w[k] + wt.o[k]
                    }
                };
                plane.data[origin + row * stride + col] = v.clamp(0, max) as u16;
            }
        }
    }
    Ok(())
}

/// Gather the 13 neighbour samples of a luma 4x4 block (spec 8.3.1.2),
/// including the top-right substitution rule.
pub(crate) fn gather_nbr4<S: Sample>(
    pic: &Picture<S>,
    nbr: &MbNeighbors,
    blk: usize,
    x: usize,
    y: usize,
) -> Nbr4<S> {
    gather_nbr4_at(pic, nbr, blk, pic.y.at(x, y), pic.y.stride)
}

/// [`gather_nbr4`] for a block at plane index `o` in a plane read at row
/// pitch `stride` (twice the plane's own for a field macroblock of an MBAFF
/// frame, whose neighbouring rows are the same-parity ones).
pub(crate) fn gather_nbr4_at<S: Sample>(
    pic: &Picture<S>,
    nbr: &MbNeighbors,
    blk: usize,
    o: usize,
    stride: usize,
) -> Nbr4<S> {
    gather_nbr4_in(&pic.y.data, nbr, blk, o, stride)
}

/// [`gather_nbr4_at`] over any plane's samples (the Cb and Cr planes of 4:4:4
/// are predicted like luma).
pub(crate) fn gather_nbr4_in<S: Sample>(
    data: &[S],
    nbr: &MbNeighbors,
    blk: usize,
    o: usize,
    stride: usize,
) -> Nbr4<S> {
    let (dx, dy) = BLK4_POS[blk];
    let have_left = if dx == 0 { nbr.a4[dy as usize] } else { true };
    let have_top = if dy == 0 { nbr.b } else { true };
    let have_tr = match TR_KIND[blk] {
        TrKind::InMb => true,
        TrKind::B => nbr.b,
        TrKind::C => nbr.c,
        TrKind::None => false,
    };
    let mut top = [S::default(); 8];
    top[..4].copy_from_slice(&data[o - stride..o - stride + 4]);
    if have_tr {
        top[4..].copy_from_slice(&data[o - stride + 4..o - stride + 8]);
    } else if have_top {
        let t3 = top[3];
        top[4..].fill(t3);
    }
    let mut left = [S::default(); 4];
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
pub(crate) fn gather_nbr8<S: Sample>(
    pic: &Picture<S>,
    nbr: &MbNeighbors,
    blk8: usize,
    x: usize,
    y: usize,
) -> Nbr8<S> {
    gather_nbr8_at(pic, nbr, blk8, pic.y.at(x, y), pic.y.stride)
}

/// [`gather_nbr8`] at plane index `o` with row pitch `stride`.
pub(crate) fn gather_nbr8_at<S: Sample>(
    pic: &Picture<S>,
    nbr: &MbNeighbors,
    blk8: usize,
    o: usize,
    stride: usize,
) -> Nbr8<S> {
    gather_nbr8_in(&pic.y.data, nbr, blk8, o, stride)
}

/// [`gather_nbr8_at`] over any plane's samples.
pub(crate) fn gather_nbr8_in<S: Sample>(
    data: &[S],
    nbr: &MbNeighbors,
    blk8: usize,
    o: usize,
    stride: usize,
) -> Nbr8<S> {
    let have_top = if blk8 < 2 { nbr.b } else { true };
    let have_left = if blk8.is_multiple_of(2) {
        nbr.a4[blk8] && nbr.a4[blk8 + 1]
    } else {
        true
    };
    let have_tr = match blk8 {
        0 => nbr.b,
        1 => nbr.c,
        2 => true,
        _ => false,
    };
    let have_tl = match blk8 {
        0 => nbr.d,
        1 => nbr.b,
        2 => nbr.a4[1],
        _ => true,
    };
    let mut top = [S::default(); 16];
    top[..8].copy_from_slice(&data[o - stride..o - stride + 8]);
    if have_tr {
        top[8..].copy_from_slice(&data[o - stride + 8..o - stride + 16]);
    } else {
        let t7 = top[7];
        top[8..].fill(t7);
    }
    let mut left = [S::default(); 8];
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
pub(crate) fn deblock_picture<S: Sample>(pic: &mut Picture<S>) {
    let mb_w = pic.mb_w;
    let bd = pic.bd;
    let max = (1i32 << bd) - 1;
    let qp_bd = 6 * (bd as i32 - 8);
    let cmb_h = pic.cmb_h;
    let cat = pic.cat;
    let lcomps = if cat == 3 { 3 } else { 1 };
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

            // ---- luma (and Cb/Cr, coded like luma, for ChromaArrayType 3) ----
            for comp in 0..lcomps {
                let pl: &mut PicPlane<S> = match comp {
                    0 => &mut pic.y,
                    1 => &mut pic.cb,
                    _ => &mut pic.cr,
                };
                let stride = pl.stride;
                let base = pl.at(mb_x * 16, mb_y * 16);
                let qpf = |a: usize| -> i32 {
                    if comp == 0 {
                        qp_of(a)
                    } else {
                        let s = pic.slices[pic.mb_slice[a] as usize];
                        let off = if comp == 1 {
                            s.cb_qp_offset
                        } else {
                            s.cr_qp_offset
                        };
                        let q = if pic.mb_flags[a] & FLAG_PCM != 0 {
                            0
                        } else {
                            i32::from(pic.mb_qp[a])
                        };
                        chroma_qp(q, off, qp_bd)
                    }
                };
                let qp_q = qpf(addr);
                // Lossless macroblocks keep their samples (8.7.2.2).
                let lossless = |a: usize| {
                    pic.bypass_enabled
                        && pic.mb_flags[a] & FLAG_PCM == 0
                        && i32::from(pic.mb_qp[a]) + qp_bd == 0
                };
                let (p_ll_v, q_ll) = (filter_left && lossless(addr - 1), lossless(addr));
                let snap_v = (p_ll_v || (filter_left && q_ll))
                    .then(|| snap_edge(&pl.data, base, stride, true));
                // Vertical edges (filter across columns), left to right.
                for e in 0..4 {
                    if e == 0 && !filter_left {
                        continue;
                    }
                    if !internal(e) {
                        continue;
                    }
                    let qp_p = if e == 0 { qpf(addr - 1) } else { qp_q };
                    let qp_avg = (qp_p + qp_q + 1) >> 1;
                    // Vertical edges stay scalar: a gather/scatter SIMD
                    // variant measured slower than this loop (strided
                    // per-lane loads dominate).
                    for j in 0..4 {
                        let bs = bs_v[e][j];
                        if bs == 0 {
                            continue;
                        }
                        let params = edge_params(qp_avg, sp.alpha_offset, sp.beta_offset, bs, bd);
                        if params.alpha == 0 || params.beta == 0 {
                            continue;
                        }
                        for k in 0..4 {
                            let row = j * 4 + k;
                            filter_luma_line(
                                &mut pl.data,
                                max,
                                base + row * stride + e * 4,
                                1,
                                &params,
                            );
                        }
                    }
                }
                if let Some(sv) = &snap_v {
                    restore_edge(&mut pl.data, sv, base, stride, true, p_ll_v, q_ll);
                }
                let p_ll_h = filter_top && lossless(addr - mb_w);
                let snap_h = (p_ll_h || (filter_top && q_ll))
                    .then(|| snap_edge(&pl.data, base, stride, false));
                // Horizontal edges, top to bottom.
                for e in 0..4 {
                    if e == 0 && !filter_top {
                        continue;
                    }
                    if !internal(e) {
                        continue;
                    }
                    let qp_p = if e == 0 { qpf(addr - mb_w) } else { qp_q };
                    let qp_avg = (qp_p + qp_q + 1) >> 1;
                    let bs = bs_h[e];
                    let uniform = bs[0] == bs[1] && bs[1] == bs[2] && bs[2] == bs[3];
                    if uniform {
                        if bs[0] == 0 {
                            continue;
                        }
                        let params =
                            edge_params(qp_avg, sp.alpha_offset, sp.beta_offset, bs[0], bd);
                        if params.alpha == 0 || params.beta == 0 {
                            continue;
                        }
                        // Eight rows around the edge are contiguous, so the
                        // whole 16-sample edge filters in one pass.
                        S::filter_luma_h_edge16(
                            &mut pl.data,
                            max,
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
                        let params =
                            edge_params(qp_avg, sp.alpha_offset, sp.beta_offset, bs[j], bd);
                        if params.alpha == 0 || params.beta == 0 {
                            continue;
                        }
                        for k in 0..4 {
                            filter_luma_line(
                                &mut pl.data,
                                max,
                                base + e * 4 * stride + j * 4 + k,
                                stride,
                                &params,
                            );
                        }
                    }
                }
                if let Some(sh) = &snap_h {
                    restore_edge(&mut pl.data, sh, base, stride, false, p_ll_h, q_ll);
                }
            }

            // ---- chroma (Cb, Cr) ----
            for comp in 0..if matches!(cat, 1 | 2) { 2 } else { 0 } {
                let (plane, off_sel): (&mut PicPlane<S>, fn(&SliceParams) -> i32) = if comp == 0 {
                    (&mut pic.cb, |s| s.cb_qp_offset)
                } else {
                    (&mut pic.cr, |s| s.cr_qp_offset)
                };
                let stride = plane.stride;
                let c422 = cat == 2;
                let base = plane.origin + (mb_y * cmb_h) * stride + mb_x * 8;
                // Chroma QP per side: each macroblock's luma QP mapped with
                // the offset of ITS OWN slice's PPS (8.7.2).
                let cqp = |a: usize| -> i32 {
                    let s = pic.slices[pic.mb_slice[a] as usize];
                    if pic.mb_flags[a] & FLAG_PCM != 0 {
                        chroma_qp(0, off_sel(&s), qp_bd)
                    } else {
                        chroma_qp(i32::from(pic.mb_qp[a]), off_sel(&s), qp_bd)
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
                    for k in 0..cmb_h {
                        let bs = bs_v[e * 2][k >> if c422 { 2 } else { 1 }];
                        if bs == 0 {
                            continue;
                        }
                        let params = edge_params(qp_avg, sp.alpha_offset, sp.beta_offset, bs, bd);
                        if params.alpha == 0 || params.beta == 0 {
                            continue;
                        }
                        filter_chroma_line(
                            &mut plane.data,
                            max,
                            base + k * stride + e * 4,
                            1,
                            &params,
                        );
                    }
                }
                // 4:2:2 chroma is as tall as luma: its horizontal 4x4 transform
                // edges sit at rows 0, 4, 8 and 12, each on the boundary
                // strength of the luma edge on the same row.
                for e in 0..if c422 { 4 } else { 2 } {
                    if e == 0 && !filter_top {
                        continue;
                    }
                    let cq_p = if e == 0 { cqp(addr - mb_w) } else { cq_q };
                    let qp_avg = (cq_p + cq_q + 1) >> 1;
                    let bs = bs_h[if c422 { e } else { e * 2 }];
                    if bs[0] == bs[1] && bs[1] == bs[2] && bs[2] == bs[3] {
                        if bs[0] == 0 {
                            continue;
                        }
                        let params =
                            edge_params(qp_avg, sp.alpha_offset, sp.beta_offset, bs[0], bd);
                        if params.alpha == 0 || params.beta == 0 {
                            continue;
                        }
                        // The four rows around the edge are contiguous, so the
                        // whole eight-sample edge filters in one pass.
                        S::filter_chroma_h_edge8(
                            &mut plane.data,
                            max,
                            base + e * 4 * stride,
                            stride,
                            &params,
                        );
                        continue;
                    }
                    for k in 0..8 {
                        let bs = bs[k / 2];
                        if bs == 0 {
                            continue;
                        }
                        let params = edge_params(qp_avg, sp.alpha_offset, sp.beta_offset, bs, bd);
                        if params.alpha == 0 || params.beta == 0 {
                            continue;
                        }
                        filter_chroma_line(
                            &mut plane.data,
                            max,
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

/// Copy the 8 samples across a macroblock edge (4 each side) along its 16
/// samples, so a lossless side can be put back after the filter ran.
fn snap_edge<S: Sample>(data: &[S], base: usize, stride: usize, vertical: bool) -> Vec<S> {
    let mut v = Vec::with_capacity(128);
    for i in 0..16 {
        for d in 0..8 {
            v.push(if vertical {
                data[base + i * stride + d - 4]
            } else {
                data[base + d * stride + i - 4 * stride]
            });
        }
    }
    v
}

fn restore_edge<S: Sample>(
    data: &mut [S],
    snap: &[S],
    base: usize,
    stride: usize,
    vertical: bool,
    p: bool,
    q: bool,
) {
    for i in 0..16 {
        for d in 0..8 {
            if (d < 4 && !p) || (d >= 4 && !q) {
                continue;
            }
            let idx = if vertical {
                base + i * stride + d - 4
            } else {
                base + d * stride + i - 4 * stride
            };
            data[idx] = snap[i * 8 + d];
        }
    }
}

/// True when every 4x4 block of this macroblock predicts from the same
/// pictures with the same motion, so no internal edge can reach the motion
/// clauses of 8.7.2.1.
fn uniform_motion<S: Sample>(pic: &Picture<S>, mb_x: usize, mb_y: usize) -> bool {
    let w4 = pic.mb_w * 4;
    let first = mb_y * 4 * w4 + mb_x * 4;
    let (mv, id) = (pic.mv[first], pic.ref_id[first]);
    (0..4).all(|by| {
        let row = first + by * w4;
        (0..4).all(|bx| pic.mv[row + bx] == mv && pic.ref_id[row + bx] == id)
    })
}

/// What clause 8.7.2.1 needs from the 4x4 block on one side of a luma edge.
#[derive(Clone, Copy)]
pub(crate) struct BsSide {
    /// The containing macroblock is intra: strength 4 on macroblock edges and
    /// 3 on internal ones, regardless of motion or levels.
    pub(crate) intra: bool,
    /// The transform block containing this sample has non-zero levels.
    pub(crate) coded: bool,
    /// MvLX of the block, `[list][component]`.
    pub(crate) mv: [[i16; 2]; 2],
    /// RefIdxLX of the block as list identities (`Picture::id`), -1 unused.
    pub(crate) refs: [i32; 2],
}

/// Boundary strength between the 4x4 blocks on the two sides of a luma edge
/// (clause 8.7.2.1). `mb_edge` selects the macroblock-edge strengths 4/3.
pub(crate) fn bs_between(p: &BsSide, q: &BsSide, mb_edge: bool, field: bool, vertical: bool) -> u8 {
    if p.intra || q.intra {
        // A horizontal edge of a field picture is never strength 4 (8.7.2.1).
        return if mb_edge && (vertical || !field) {
            4
        } else {
            3
        };
    }
    if p.coded || q.coded {
        return 2;
    }
    let (pr, qr) = (p.refs, q.refs);
    let pn = u8::from(pr[0] >= 0) + u8::from(pr[1] >= 0);
    let qn = u8::from(qr[0] >= 0) + u8::from(qr[1] >= 0);
    if pn != qn {
        return 1;
    }
    let (pm, qm) = (p.mv, q.mv);
    // The vertical limit is 4 quarter frame samples: 2 in field units.
    let ylim = if field { 2 } else { 4 };
    let differs = |a: [i16; 2], b: [i16; 2]| {
        (i32::from(a[0]) - i32::from(b[0])).abs() >= 4
            || (i32::from(a[1]) - i32::from(b[1])).abs() >= ylim
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

/// One side of a luma edge from picture state: the decoder path, both sides
/// final macroblocks. The encoder's inter trial uses this for the
/// already-coded neighbour and builds its candidate side itself.
pub(crate) fn bs_side<S: Sample>(pic: &Picture<S>, bx: usize, by: usize) -> BsSide {
    let w4 = pic.mb_w * 4;
    let mb = (by / 4) * pic.mb_w + bx / 4;
    BsSide {
        intra: pic.mb_flags[mb] & FLAG_INTER == 0,
        coded: bs_coded(pic, mb, bx, by),
        mv: pic.mv[by * w4 + bx],
        refs: pic.ref_id[by * w4 + bx],
    }
}

/// "The transform block containing the sample has non-zero levels": for an
/// 8x8-transform macroblock that is the whole 8x8, whose four 4x4 slots
/// carry per-slot counts under CAVLC (7.3.5.3.1 splits it into four blocks).
fn bs_coded<S: Sample>(pic: &Picture<S>, mb: usize, bx: usize, by: usize) -> bool {
    let w4 = pic.mb_w * 4;
    if pic.mb_flags[mb] & FLAG_TRANS8X8 == 0 {
        return pic.nz_y[by * w4 + bx] != 0;
    }
    let (ox, oy) = (bx & !1, by & !1);
    pic.nz_y[oy * w4 + ox] != 0
        || pic.nz_y[oy * w4 + ox + 1] != 0
        || pic.nz_y[(oy + 1) * w4 + ox] != 0
        || pic.nz_y[(oy + 1) * w4 + ox + 1] != 0
}

/// Boundary strength of one 4-sample luma edge segment (clause 8.7.2.1).
///
/// `edge` 0..4 selects the vertical (`vertical`) or horizontal edge inside the
/// macroblock, `seg` the 4-sample run along it. Edge 0 is the macroblock edge.
fn boundary_strength<S: Sample>(
    pic: &Picture<S>,
    mb_x: usize,
    mb_y: usize,
    edge: usize,
    seg: usize,
    vertical: bool,
) -> u8 {
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
    bs_between(
        &bs_side(pic, pbx, pby),
        &bs_side(pic, qbx, qby),
        edge == 0,
        pic.structure != FRAME,
        vertical,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dpb::RefList;
    use ec_h264_syntax::{PredWeightTable, WeightEntry};

    /// A picture whose visible samples are a fixed ramp, borders replicated.
    fn ramp_picture(value: impl Fn(usize, usize) -> u8) -> Picture {
        let mut pic = Picture {
            mb_w: 1,
            mb_h: 1,
            ..Default::default()
        };
        for (plane, size) in [(0usize, 16usize), (1, 8), (2, 8)] {
            let p = match plane {
                0 => &mut pic.y,
                1 => &mut pic.cb,
                _ => &mut pic.cr,
            };
            let pad = 8;
            let stride = size + 2 * pad;
            let mut data = vec![0u8; stride * (size + 2 * pad)];
            for y in 0..size {
                for x in 0..size {
                    data[(pad + y) * stride + pad + x] = value(x, y);
                }
            }
            *p = PicPlane {
                data,
                stride,
                origin: pad * stride + pad,
                width: size,
                height: size,
                pad,
            };
            p.extend_borders();
        }
        pic
    }

    fn plane_byte(p: &PicPlane, x: usize, y: usize) -> u8 {
        p.data[p.origin + y * p.stride + x]
    }

    /// Explicit-weight bipred combines each component with ITS OWN weights:
    /// luma and chroma signal independent denoms/weights/offsets, so chroma
    /// must never ride the luma lookup (regression guard for the fused
    /// weighted path in [`compensate`]).
    #[test]
    fn weighted_bipred_combines_per_component() {
        let refs = vec![
            ramp_picture(|x, y| (40 + x * 3 + y) as u8),
            ramp_picture(|x, y| (220 - x - y * 2) as u8),
        ];
        let mut l0 = RefList::default();
        l0.push(0);
        let mut l1 = RefList::default();
        l1.push(1);
        let ctx = SliceCtx {
            structure: FRAME,
            mb_parity: FRAME,
            pos: MbPos::default(),
            slice_id: 0,
            slice_type: SliceType::B,
            qp: 30,
            cb_qp_offset: 0,
            cr_qp_offset: 0,
            cmb_h: 8,
            cat: 1,
            transform_8x8_mode: false,
            constrained_intra_pred: false,
            qp_delta_inc: 0,
            lists: [l0, l1],
            num_ref_idx: [1, 1],
            direct_spatial: true,
            direct_8x8_inference: true,
            weighted_pred: false,
            weighted_bipred_idc: 1,
            weights: Some(PredWeightTable {
                luma_log2_weight_denom: 1,
                chroma_log2_weight_denom: 2,
                l0: vec![WeightEntry {
                    luma: Some((2, 1)),
                    chroma: Some([(5, 2), (7, 3)]),
                }],
                l1: vec![WeightEntry {
                    luma: Some((2, 1)),
                    chroma: Some([(5, 2), (7, 3)]),
                }],
            }),
            poc: 4,
            fpoc: [4, 4],
            shapes: MbShapes::default(),
        };
        let mut pic = ramp_picture(|_, _| 0);

        compensate8(&mut pic, &refs, &ctx, 0, 0, 0, 0, 4, 4, [[0, 0]; 2], [0, 0])
            .expect("compensate");

        let expected = |p0: u8, p1: u8, w: i32, log_wd: u32, o: i32| -> u8 {
            let v = (((i32::from(p0) * w + i32::from(p1) * w + (1 << log_wd)) >> (log_wd + 1)) + o)
                .clamp(0, 255);
            v as u8
        };
        // Luma: weights (2, 1) — whatever luma does is the luma reference
        // behaviour; the point is chroma below.
        for y in 0..16 {
            for x in 0..16 {
                let (p0, p1) = (plane_byte(&refs[0].y, x, y), plane_byte(&refs[1].y, x, y));
                assert_eq!(
                    plane_byte(&pic.y, x, y),
                    expected(p0, p1, 2, 1, 1),
                    "luma ({x},{y})"
                );
            }
        }
        // Chroma Cb: weights (5, 2) with denom 2 — NOT the luma weights.
        for y in 0..8 {
            for x in 0..8 {
                let (p0, p1) = (plane_byte(&refs[0].cb, x, y), plane_byte(&refs[1].cb, x, y));
                assert_eq!(
                    plane_byte(&pic.cb, x, y),
                    expected(p0, p1, 5, 2, 2),
                    "cb ({x},{y})"
                );
                let (p0, p1) = (plane_byte(&refs[0].cr, x, y), plane_byte(&refs[1].cr, x, y));
                assert_eq!(
                    plane_byte(&pic.cr, x, y),
                    expected(p0, p1, 7, 2, 3),
                    "cr ({x},{y})"
                );
            }
        }
    }
}

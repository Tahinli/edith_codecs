//! Decoder core: parameter-set store, flat per-picture context, macroblock
//! decode loop, reconstruction and the in-loop filter driver.
//!
//! Layout choices (perf-first):
//! - Planes are padded (`PAD_Y`/`PAD_C` samples each side), so neighbour
//!   gathers and future inter-prediction overreach never bounds-check per
//!   sample and edge availability is data, not arithmetic.
//! - All per-macroblock state lives in flat struct-of-arrays keyed by
//!   macroblock address or 4x4-block coordinate: non-zero coefficient counts
//!   (`nz_y`, `nz_c`), intra 4x4 modes, QP, flags, owning slice. No per-MB
//!   heap objects anywhere.
//! - Buffers are reused across pictures; a picture start is O(macroblocks)
//!   fills, and the slice decode loop performs zero allocations.

use ec_core::BitReader;
use ec_core::error::{Error, Result};
use ec_core::frame::{PixelFormat, Plane, VideoFrame};
use ec_h264_syntax::{
    AnnexBIter, NalHeader, NalUnitType, Pps, SliceHeader, SliceType, Sps, unescape_rbsp,
};

use crate::deblock::{
    chroma_qp, edge_params, filter_chroma_line, filter_luma_h_edge16, filter_luma_line,
};
use crate::entropy::{
    BlockCat, Entropy, FLAG_CHROMA_PRED, FLAG_DECODED, FLAG_I16, FLAG_PCM, MbCtx, MbInfo,
};
use crate::pred::{Nbr4, PlaneWindow, add_residual_4x4, pred_4x4, pred_16x16, pred_chroma_8x8};
use crate::tables::{BLK4_POS, CHROMA_QP};
use crate::transform::{
    LevelScale4x4, chroma_dc_transform_420, dequant_4x4, inverse_transform_4x4, luma_dc_transform,
    unzigzag, unzigzag_ac15,
};

/// Luma plane padding on every side, sized for future inter-prediction
/// filter overreach (16 MB + margin).
const PAD_Y: usize = 32;
/// Chroma plane padding on every side.
const PAD_C: usize = 16;
/// `mb_slice` value for a not-yet-decoded macroblock.
const NO_SLICE: u16 = u16::MAX;

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

/// A padded 8-bit plane.
#[derive(Debug, Default)]
struct Plane8 {
    data: Vec<u8>,
    stride: usize,
    /// Index of sample (0, 0) of the picture.
    origin: usize,
}

impl Plane8 {
    fn resize(&mut self, width: usize, height: usize, pad: usize) {
        self.stride = width + 2 * pad;
        self.origin = pad * self.stride + pad;
        self.data.clear();
        self.data.resize(self.stride * (height + 2 * pad), 0);
    }

    #[inline]
    fn at(&self, x: usize, y: usize) -> usize {
        self.origin + y * self.stride + x
    }
}

/// Per-slice values the deblocking filter needs after slice decode ends.
#[derive(Debug, Clone, Copy)]
struct SliceParams {
    disable_deblock_idc: u8,
    alpha_offset: i32,
    beta_offset: i32,
    cb_qp_offset: i32,
    cr_qp_offset: i32,
}

/// Flat per-picture decode context (see module docs for the layout story).
#[derive(Debug, Default)]
struct Picture {
    sps_id: u8,
    mb_w: usize,
    mb_h: usize,
    y: Plane8,
    cb: Plane8,
    cr: Plane8,
    /// TotalCoeff per luma 4x4 block, `(mb_w * 4) x (mb_h * 4)`.
    nz_y: Vec<u8>,
    /// TotalCoeff per chroma 4x4 block, `(mb_w * 2) x (mb_h * 2)`, Cb and Cr.
    nz_c: [Vec<u8>; 2],
    /// Intra4x4PredMode per luma 4x4 block (2 = DC for non-I4x4 MBs).
    i4_modes: Vec<u8>,
    /// QPY per macroblock (the prediction-chain value; PCM keeps the chain).
    mb_qp: Vec<u8>,
    /// `FLAG_*` bits per macroblock.
    mb_flags: Vec<u8>,
    /// `CodedBlockPatternLuma | CodedBlockPatternChroma << 4` per macroblock,
    /// read by CABAC context selection (9.3.3.1.1.4).
    mb_cbp: Vec<u8>,
    /// coded_block_flag of the DC blocks per macroblock: bit 0 luma, 1 Cb,
    /// 2 Cr (9.3.3.1.1.9 with ctxBlockCat 0 and 3).
    mb_dc_cbf: Vec<u8>,
    /// Owning slice index per macroblock, `NO_SLICE` when undecoded.
    mb_slice: Vec<u16>,
    slices: Vec<SliceParams>,
    decoded_mbs: usize,
    complete: bool,
}

impl Picture {
    fn start(&mut self, sps: &Sps) {
        let mb_w = sps.mb_width as usize;
        let mb_h = sps.mb_height as usize;
        if self.mb_w != mb_w || self.mb_h != mb_h || self.sps_id != sps.id {
            self.mb_w = mb_w;
            self.mb_h = mb_h;
            self.sps_id = sps.id;
            self.y.resize(mb_w * 16, mb_h * 16, PAD_Y);
            self.cb.resize(mb_w * 8, mb_h * 8, PAD_C);
            self.cr.resize(mb_w * 8, mb_h * 8, PAD_C);
            self.nz_y.resize(mb_w * mb_h * 16, 0);
            for c in &mut self.nz_c {
                c.resize(mb_w * mb_h * 4, 0);
            }
            self.i4_modes.resize(mb_w * mb_h * 16, 2);
            self.mb_qp.resize(mb_w * mb_h, 0);
            self.mb_flags.resize(mb_w * mb_h, 0);
            self.mb_cbp.resize(mb_w * mb_h, 0);
            self.mb_dc_cbf.resize(mb_w * mb_h, 0);
            self.mb_slice.resize(mb_w * mb_h, NO_SLICE);
        }
        // Per-MB metadata is rewritten by each decoded macroblock; only the
        // "who is decoded" state must be wiped between pictures.
        self.mb_slice.fill(NO_SLICE);
        self.mb_flags.fill(0);
        self.slices.clear();
        self.decoded_mbs = 0;
        self.complete = false;
    }
}

/// Streaming H.264 decoder (CAVLC intra scope; unsupported features come back
/// as named [`Error::Unsupported`] values, never wrong pixels).
pub struct Decoder {
    sps_map: Vec<Option<Sps>>,
    pps_map: Vec<Option<Pps>>,
    rbsp: Vec<u8>,
    pic: Picture,
    has_picture: bool,
    /// Scaling factors for intra Y/Cb/Cr 4x4 lists of the active PPS/SPS.
    ls: [LevelScale4x4; 3],
    active_pps: Option<u32>,
    /// `seq_parameter_set_id` of the most recently stored SPS, so a container
    /// entry path can publish the picture size before the first slice.
    last_sps: Option<u8>,
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
    /// Call [`Decoder::end_picture`] + [`Decoder::frame`], then push it again.
    PictureBoundary,
}

impl Default for Decoder {
    fn default() -> Self {
        Decoder::new()
    }
}

impl Decoder {
    /// A fresh decoder with no parameter sets.
    pub fn new() -> Decoder {
        Decoder {
            sps_map: vec![None; 32],
            pps_map: vec![None; 256],
            rbsp: Vec::new(),
            pic: Picture::default(),
            has_picture: false,
            ls: [
                LevelScale4x4::new(&[16; 16]),
                LevelScale4x4::new(&[16; 16]),
                LevelScale4x4::new(&[16; 16]),
            ],
            active_pps: None,
            last_sps: None,
        }
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

    /// Run the deblocking filter over the open picture and mark it complete.
    pub fn end_picture(&mut self) -> Result<()> {
        if !self.has_picture || self.pic.complete {
            return Err(Error::corrupt("end_picture without an open picture"));
        }
        deblock_picture(&mut self.pic);
        self.pic.complete = true;
        Ok(())
    }

    /// Copy the completed picture out as a cropped I420 frame.
    pub fn frame(&self) -> Result<VideoFrame> {
        if !self.pic.complete {
            return Err(Error::corrupt("frame() before end_picture()"));
        }
        let sps = self.sps_map[self.pic.sps_id as usize]
            .as_ref()
            .ok_or_else(|| Error::corrupt("active SPS vanished"))?;
        let (w, h) = (sps.width as usize, sps.height as usize);
        let (cx, cy) = (sps.crop.0 as usize, sps.crop.2 as usize);
        let copy = |p: &Plane8, x0: usize, y0: usize, w: usize, h: usize| -> Plane {
            let mut out = vec![0u8; w * h];
            for row in 0..h {
                let src = p.at(x0, y0 + row);
                out[row * w..(row + 1) * w].copy_from_slice(&p.data[src..src + w]);
            }
            Plane::new(out, w)
        };
        let planes = vec![
            copy(&self.pic.y, cx, cy, w, h),
            copy(&self.pic.cb, cx / 2, cy / 2, w.div_ceil(2), h.div_ceil(2)),
            copy(&self.pic.cr, cx / 2, cy / 2, w.div_ceil(2), h.div_ceil(2)),
        ];
        let mut frame = VideoFrame::try_new(PixelFormat::I420, w as u32, h as u32, planes)?;
        if let Some(vui) = &sps.vui {
            frame.color.full_range = vui.video_full_range;
            if let Some((p, t, m)) = vui.colour_description {
                frame.color.primaries = p;
                frame.color.transfer = t;
                frame.color.matrix = m;
            }
        }
        Ok(frame)
    }

    /// True while a picture is open (started, not completed).
    pub fn picture_open(&self) -> bool {
        self.has_picture && !self.pic.complete
    }

    /// Drop picture state after a seek. Parameter sets survive, because the
    /// container does not resend them at every seek point.
    pub fn reset_pictures(&mut self) {
        self.has_picture = false;
        self.pic.complete = false;
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
        if self.has_picture && !self.pic.complete && first_mb == 0 && self.pic.decoded_mbs > 0 {
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
        if sh.slice_type != SliceType::I {
            return Err(Error::unsupported(
                "non-I slice",
                "P/B/SP/SI slices need inter prediction and a decoded picture \
                 buffer (clause 8.4); only intra slices are decoded",
            ));
        }

        if !self.has_picture || self.pic.complete {
            self.pic.start(sps);
            self.has_picture = true;
            if self.active_pps != Some(pps.id) {
                let lists = pps.scaling_lists.as_ref().or(sps.scaling_lists.as_ref());
                let weights = match lists {
                    Some(l) => [&l.list_4x4[0], &l.list_4x4[1], &l.list_4x4[2]],
                    None => [&[16u8; 16]; 3],
                };
                self.ls = [
                    LevelScale4x4::new(weights[0]),
                    LevelScale4x4::new(weights[1]),
                    LevelScale4x4::new(weights[2]),
                ];
                self.active_pps = Some(pps.id);
            }
        }
        if self.pic.sps_id != sps.id {
            return Err(Error::corrupt("SPS changed mid-picture"));
        }
        if self.pic.slices.len() >= usize::from(u16::MAX) {
            return Err(Error::corrupt("more than 65534 slices in a picture"));
        }
        let slice_id = self.pic.slices.len() as u16;
        self.pic.slices.push(SliceParams {
            disable_deblock_idc: sh.deblock.disable_idc,
            alpha_offset: sh.deblock.alpha_c0_offset,
            beta_offset: sh.deblock.beta_offset,
            cb_qp_offset: pps.chroma_qp_index_offset,
            cr_qp_offset: pps.second_chroma_qp_index_offset,
        });

        let mut r = if pps.entropy_coding_mode {
            Entropy::cabac(rbsp, sh.header_bits, sh.slice_qp)?
        } else {
            Entropy::cavlc(rbsp, sh.header_bits)
        };

        let mut ctx = SliceCtx {
            slice_id,
            qp: sh.slice_qp,
            cb_qp_offset: pps.chroma_qp_index_offset,
            cr_qp_offset: pps.second_chroma_qp_index_offset,
            transform_8x8_mode: pps.transform_8x8_mode,
            qp_delta_inc: 0,
        };
        let mbs = self.pic.mb_w * self.pic.mb_h;
        let mut mb_addr = first_mb as usize;
        loop {
            if mb_addr >= mbs {
                return Err(Error::corrupt("macroblock address beyond the picture"));
            }
            decode_macroblock(&mut self.pic, &self.ls, &mut r, &mut ctx, mb_addr)?;
            if !r.more_macroblocks()? {
                break;
            }
            mb_addr += 1;
        }
        Ok(NalOutcome::SliceDecoded)
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
    Ok(())
}

/// Mutable per-slice decode state.
struct SliceCtx {
    slice_id: u16,
    qp: i32,
    cb_qp_offset: i32,
    cr_qp_offset: i32,
    transform_8x8_mode: bool,
    /// ctxIdxInc for the next macroblock's first mb_qp_delta bin: set when the
    /// macroblock just decoded coded a non-zero mb_qp_delta (9.3.3.1.1.5).
    qp_delta_inc: u8,
}

/// Neighbour-availability snapshot for one macroblock (left, top,
/// top-right). Top-left availability follows from the modes that read it
/// being conformance-restricted; inter prediction will add it back.
#[derive(Clone, Copy)]
struct MbNeighbors {
    a: bool,
    b: bool,
    c: bool,
}

fn mb_neighbors(pic: &Picture, mb_x: usize, mb_y: usize, slice_id: u16) -> MbNeighbors {
    let w = pic.mb_w;
    let addr = mb_y * w + mb_x;
    let same = |a: usize| pic.mb_slice[a] == slice_id;
    MbNeighbors {
        a: mb_x > 0 && same(addr - 1),
        b: mb_y > 0 && same(addr - w),
        c: mb_y > 0 && mb_x + 1 < w && same(addr - w + 1),
    }
}

/// Non-zero counts of the left and above neighbours of the luma 4x4 block at
/// global block coords `(bx, by)`, `None` when that neighbour is unavailable
/// (clause 6.4.11.4 plus the availability rules of 6.4.8).
#[inline]
fn luma_nz_pair(pic: &Picture, n: &MbNeighbors, bx: usize, by: usize) -> (Option<u8>, Option<u8>) {
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
fn chroma_nz_pair(
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

/// Decode one macroblock of an I slice (spec 7.3.5, 7.4.5) and reconstruct
/// it. `ctx.qp` carries the QPY prediction chain.
// Block loops are indexed: `blk` is simultaneously a bitstream position, a
// geometry key (BLK4_POS) and a grid coordinate; iterators obscure that.
#[allow(clippy::needless_range_loop)]
fn decode_macroblock(
    pic: &mut Picture,
    ls: &[LevelScale4x4; 3],
    r: &mut Entropy<'_>,
    ctx: &mut SliceCtx,
    mb_addr: usize,
) -> Result<()> {
    let mb_x = mb_addr % pic.mb_w;
    let mb_y = mb_addr / pic.mb_w;
    if pic.mb_slice[mb_addr] != NO_SLICE {
        return Err(Error::corrupt("macroblock decoded twice"));
    }
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
    let w4 = pic.mb_w * 4;
    let w2 = pic.mb_w * 2;
    let (bx0, by0) = (mb_x * 4, mb_y * 4);
    let (cx0, cy0) = (mb_x * 2, mb_y * 2);

    let mb_type = r.mb_type_i()?;

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

    // I_NxN transform size flag (High profile only).
    if !is_i16 && ctx.transform_8x8_mode && r.transform_size_8x8_flag()? {
        return Err(Error::unsupported(
            "8x8 transform",
            "transform_size_8x8_flag 1 selects the 8x8 intra prediction of \
             clause 8.3.2 and the 8x8 transform of 8.5.13; not implemented",
        ));
    }

    // mb_pred (7.3.5.1): intra 4x4 modes, then chroma mode.
    let mut modes = [2u8; 16];
    if !is_i16 {
        for blk in 0..16 {
            let (dx, dy) = BLK4_POS[blk];
            let (bx, by) = (bx0 + dx as usize, by0 + dy as usize);
            // Predicted mode = min of neighbours, DC when either unavailable
            // (8.3.1.1; non-I4x4 neighbours were written as 2).
            let left_avail = if bx % 4 == 0 { nbr.a } else { true };
            let top_avail = if by % 4 == 0 { nbr.b } else { true };
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

    // mb_qp_delta (7.4.5).
    let mut qp_delta = 0;
    if cbp_luma != 0 || cbp_chroma != 0 || is_i16 {
        qp_delta = r.mb_qp_delta()?;
        ctx.qp = (ctx.qp + qp_delta + 52) % 52;
    }
    ctx.qp_delta_inc = u8::from(qp_delta != 0);
    pic.mb_qp[mb_addr] = ctx.qp as u8;
    pic.mb_flags[mb_addr] = FLAG_DECODED
        | if is_i16 { FLAG_I16 } else { 0 }
        | if chroma_mode != 0 {
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

    // ---- residual parse fused with reconstruction (7.3.5.3 + 8.5) ----
    //
    // Bitstream order (luma blocks in Z-order, then chroma DC, then chroma
    // AC) equals reconstruction dependency order, so each block reconstructs
    // the moment its levels are parsed: no per-MB coefficient arrays, and
    // empty blocks cost one coeff_token read plus a prediction write.
    let mut dc_cbf = 0u8;
    let mut scan = [0i32; 16];
    let mut raster = [0i32; 16];
    let mut resid = [0i32; 16];
    if is_i16 {
        let mut w = PlaneWindow {
            data: &mut pic.y.data,
            stride: pic.y.stride,
            origin: pic.y.origin + (mb_y * 16) * pic.y.stride + mb_x * 16,
        };
        pred_16x16(i16_mode, &mut w, nbr.b, nbr.a);
        // Luma DC: un-zigzag over the 4x4 DC array, Hadamard + scale.
        let (na, nb) = luma_nz_pair(pic, &nbr, bx0, by0);
        let dc_tc = r.residual_block(&mut scan, BlockCat::LumaDc, na, nb)?;
        dc_cbf |= u8::from(dc_tc != 0);
        unzigzag(&scan, &mut raster);
        luma_dc_transform(&mut raster, &ls[0], qp_y);
        let dc = raster;
        for blk in 0..16 {
            let (dx, dy) = BLK4_POS[blk];
            let (bx, by) = (bx0 + dx as usize, by0 + dy as usize);
            let tc = if cbp_luma != 0 {
                let (na, nb) = luma_nz_pair(pic, &nbr, bx, by);
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
    } else {
        for blk in 0..16 {
            let (dx, dy) = BLK4_POS[blk];
            let (bx, by) = (bx0 + dx as usize, by0 + dy as usize);
            let tc = if cbp_luma & (1 << (blk >> 2)) != 0 {
                let (na, nb) = luma_nz_pair(pic, &nbr, bx, by);
                r.residual_block(&mut scan, BlockCat::Luma4x4, na, nb)?
            } else {
                0
            };
            pic.nz_y[by * w4 + bx] = tc;
            let (x, y) = (mb_x * 16 + dx as usize * 4, mb_y * 16 + dy as usize * 4);
            let n = gather_nbr4(pic, &nbr, blk, x, y);
            let mut p = [0u8; 16];
            pred_4x4(modes[blk], &n, &mut p);
            let origin = pic.y.at(x, y);
            let stride = pic.y.stride;
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
        }
    }

    // Chroma: predict both components, then DC (Cb, Cr), then AC in
    // bitstream order (all Cb blocks, all Cr blocks), reconstructing as
    // levels arrive.
    for comp in 0..2 {
        let plane = if comp == 0 { &mut pic.cb } else { &mut pic.cr };
        let mut w = PlaneWindow {
            data: &mut plane.data,
            stride: plane.stride,
            origin: plane.origin + (mb_y * 8) * plane.stride + mb_x * 8,
        };
        pred_chroma_8x8(chroma_mode, &mut w, nbr.b, nbr.a);
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
                let (na, nb) = chroma_nz_pair(pic, &nbr, comp, cx, cy);
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

/// Gather the 13 neighbour samples of a luma 4x4 block (spec 8.3.1.2),
/// including the top-right substitution rule.
fn gather_nbr4(pic: &Picture, nbr: &MbNeighbors, blk: usize, x: usize, y: usize) -> Nbr4 {
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

/// Whole-picture deblocking (spec 8.7): macroblocks in raster order, all
/// vertical edges then all horizontal edges per macroblock, then chroma.
fn deblock_picture(pic: &mut Picture) {
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

            // ---- luma ----
            {
                let stride = pic.y.stride;
                let base = pic.y.at(mb_x * 16, mb_y * 16);
                // Vertical edges (filter across columns), left to right.
                for e in 0..4 {
                    if e == 0 && !filter_left {
                        continue;
                    }
                    let (bs, qp_p) = if e == 0 {
                        (4, qp_of(addr - 1))
                    } else {
                        (3, qp_q)
                    };
                    let params =
                        edge_params((qp_p + qp_q + 1) >> 1, sp.alpha_offset, sp.beta_offset, bs);
                    if params.alpha == 0 || params.beta == 0 {
                        continue;
                    }
                    // Vertical edges stay scalar: a gather/scatter SIMD
                    // variant measured slower than this loop (strided
                    // per-lane loads dominate).
                    for k in 0..16 {
                        filter_luma_line(&mut pic.y.data, base + k * stride + e * 4, 1, &params);
                    }
                }
                // Horizontal edges, top to bottom.
                for e in 0..4 {
                    if e == 0 && !filter_top {
                        continue;
                    }
                    let (bs, qp_p) = if e == 0 {
                        (4, qp_of(addr - mb_w))
                    } else {
                        (3, qp_q)
                    };
                    let params =
                        edge_params((qp_p + qp_q + 1) >> 1, sp.alpha_offset, sp.beta_offset, bs);
                    if params.alpha == 0 || params.beta == 0 {
                        continue;
                    }
                    filter_luma_h_edge16(&mut pic.y.data, base + e * 4 * stride, stride, &params);
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
                for e in 0..2 {
                    if e == 0 && !filter_left {
                        continue;
                    }
                    let (bs, cq_p) = if e == 0 {
                        (4, cqp(addr - 1))
                    } else {
                        (3, cq_q)
                    };
                    let params =
                        edge_params((cq_p + cq_q + 1) >> 1, sp.alpha_offset, sp.beta_offset, bs);
                    if params.alpha == 0 || params.beta == 0 {
                        continue;
                    }
                    for k in 0..8 {
                        filter_chroma_line(&mut plane.data, base + k * stride + e * 4, 1, &params);
                    }
                }
                for e in 0..2 {
                    if e == 0 && !filter_top {
                        continue;
                    }
                    let (bs, cq_p) = if e == 0 {
                        (4, cqp(addr - mb_w))
                    } else {
                        (3, cq_q)
                    };
                    let params =
                        edge_params((cq_p + cq_q + 1) >> 1, sp.alpha_offset, sp.beta_offset, bs);
                    if params.alpha == 0 || params.beta == 0 {
                        continue;
                    }
                    for k in 0..8 {
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

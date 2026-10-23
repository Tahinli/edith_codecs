//! Stateful HEVC decoder: parameter sets, picture order count, reference
//! picture sets, the decoded picture buffer and output order (ITU-T H.265
//! clauses 8.1 to 8.3 and C.5.2).
//!
//! Scope is the base layer of Main, Main 10 and range extension streams. Every slice
//! goes through [`cu_syntax::parse_slice_into`], which parses the slice data
//! and pixel-decodes it. Inter slices write placeholder samples (see
//! [`inter::placeholder_cu`]) until motion compensation lands. With pixel
//! decode off ([`Decoder::set_pixel_decode`]) every slice is
//! parsed and every picture is managed, but no samples are produced; that is
//! the mode the picture-management tests use. [`Decoder::set_syntax_check`]
//! adds the slice-data parse ([`cu_syntax::parse_slice`]) to that mode, for
//! every slice type.
//!
//! Decoded: Main, Main 10 and the range extension formats (4:0:0, 4:2:0, 4:2:2, 4:4:4 at
//! 8 to 12 bits) with transform-skip extensions, implicit and explicit RDPCM, persistent
//! Rice adaptation, cross-component prediction, chroma QP offset lists and SAO offset
//! scaling. Refused by name, never decoded wrong: separate colour planes, bit depths above
//! 12, extended precision, CABAC bypass alignment, and the SCC,
//! multilayer and 3D SPS/PPS extensions. NAL units with `nuh_layer_id > 0`
//! are skipped silently, as a base-layer decoder must, so a multi-layer stream
//! decodes its base layer; so are AUD, filler, reserved types and types 62 and 63
//! (Dolby Vision RPU and enhancement layer).
//!
//! SEI NAL units are read for metadata only (see [`meta`]): the HDR static
//! metadata, the decoded picture hash, and the SPS colour description go onto
//! the frames. A malformed SEI is skipped, and the stream decodes on.

pub mod amvp;
pub mod cu_syntax;
pub mod deblock;
pub mod dpb;
pub mod inter;
pub mod mc;
pub mod merge;
pub mod meta;
pub mod motion_field;
pub mod pic;
pub mod picjob;
pub mod poc;
pub mod recon;
pub mod rext;
pub mod rowfilter;
pub mod rps;
pub mod runner;
pub mod sao;
pub mod shared;
pub mod slice_seg;
pub mod tiles;
pub mod trace;
pub mod weighted;

use std::collections::VecDeque;
use std::sync::Arc;

use dpb::{Dpb, DpbLimits, Marking};
pub use dpb::{DpbPicture, Frame, PendingFrame};
use ec_core::bitio::BitReader;
use ec_core::error::{Error, Result};
use ec_h265_syntax::nal::{NalHeader, NalUnitType, split_annex_b, unescape_rbsp};
use ec_h265_syntax::sei::{SeiMessage, parse_sei_rbsp};
use ec_h265_syntax::slice::{LongTermRef, ParsePositions, SliceHeader, SliceType};
use ec_h265_syntax::{Pps, PpsRangeExt, ShortTermRefPicSet, Sps, SpsRangeExt, Vps};
pub use meta::PictureMeta;
use picjob::{PicJob, SliceJob};
use rps::RpsInput;
pub use rps::{RefEntry, RefPicLists, RefPicSet};
use runner::{Budget, Checksum, Runner};
use shared::SharedPic;
use std::sync::Mutex;
use std::thread::JoinHandle;

/// What [`Decoder::push_nal`] did with a NAL unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NalOutcome {
    /// A VPS, SPS or PPS was stored.
    ParameterSet,
    /// A slice segment of a picture that is being decoded.
    Slice,
    /// A RASL picture of a random-access point that starts decoding (or a slice
    /// of one): not decoded and not output, as 8.1.3 requires.
    SkippedPicture,
    /// A NAL unit a base-layer decoder ignores.
    Skipped,
    /// An end of sequence or end of bitstream NAL unit. The open picture is
    /// finished; pending pictures wait for the next IRAP (C.5.2.2).
    EndOfSequence,
}

/// The picture being decoded, from its first slice segment until the next
/// picture starts or the stream ends.
struct CurPicture {
    sps: Arc<Sps>,
    poc: i32,
    au_index: u32,
    output: bool,
    rps: RefPicSet,
    lists: RefPicLists,
    /// Where the picture is decoded (see [`Runner`]).
    runner: Runner,
    /// The published picture (the samples and collocated motion that later pictures read).
    shared: Arc<SharedPic>,
    /// The motion checksum, set when the picture finishes.
    checksum: Arc<Checksum>,
    /// The header of the picture's latest independent slice segment; the
    /// dependent segments after it take their values from it.
    header: Option<SliceHeader>,
    /// False once a slice of the picture is not an I slice.
    all_intra: bool,
    meta: PictureMeta,
}

/// The most worker threads the decoder uses.
pub const MAX_THREADS: usize = 6;

/// Pictures allowed in flight beyond the thread count, when the thread count fits the
/// machine's cores. A picture waiting on the rows of a reference holds its thread, so with
/// exactly `threads` pictures in flight the next picture cannot start until that wait ends.
/// One spare picture lets it start. Measured on host b (4 cores), 1080p and 4K: 4 threads
/// about 14% less wall time, 2 threads 20 to 28% less; 6 threads on 4 cores got 4 to 8%
/// slower with a spare, so the spare is dropped there. Two spares are no faster at 4
/// threads. The oldest live picture never waits on a younger one, so this cannot deadlock.
const PICTURE_SLACK: usize = 1;

/// The most pictures in flight for `threads` workers.
fn picture_limit(threads: usize) -> usize {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    if threads <= cores {
        threads + PICTURE_SLACK
    } else {
        threads
    }
}

/// The worker-thread budget: `EC_H265_THREADS` when it is a positive number, else
/// the machine's available parallelism. Either way capped at [`MAX_THREADS`].
pub fn configured_threads() -> usize {
    let requested = std::env::var("EC_H265_THREADS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
    requested.min(MAX_THREADS)
}

/// Streaming HEVC decoder with the same surface as the H.264 one: feed NAL
/// units (or a whole Annex-B buffer), then drain frames in display order.
pub struct Decoder {
    vps: Vec<Option<Vps>>,
    sps: Vec<Option<Arc<Sps>>>,
    pps: Vec<Option<Arc<Pps>>>,
    pixel_decode: bool,
    /// Worker threads for the in-loop filters (see [`configured_threads`]).
    threads: usize,
    /// The worker threads the pictures and their wavefront rows share.
    budget: Arc<Budget>,
    /// Parse every slice's data (all types) with [`cu_syntax`] when pixel decode is off.
    syntax_check: bool,
    dpb: Dpb,
    cur: Option<CurPicture>,
    /// Set while the slices of a skipped RASL picture go by.
    skipping: bool,
    seen_picture: bool,
    after_eos: bool,
    /// NoRaslOutputFlag of the most recent IRAP picture. RASL pictures follow
    /// it; none has come yet, so the initial value skips them.
    irap_no_rasl: bool,
    /// POC of `prevTid0Pic` (8.3.1).
    prev_tid0_poc: i32,
    au_index: u32,
    out: VecDeque<PendingFrame>,
    /// HDR SEI that arrived since the last picture started (the access unit
    /// being collected); folded into `hdr_persist` at the next picture start.
    hdr_pending: PictureMeta,
    /// The HDR SEI in force in the current coded video sequence.
    hdr_persist: PictureMeta,
    /// The motion checksum of the finished pictures (see [`Decoder::motion_checksum`]).
    checksums: Mutex<ChecksumFold>,
    /// Worker threads of the pictures that finished but may still run, oldest first.
    in_flight: VecDeque<JoinHandle<()>>,
    /// The most pictures in flight (see [`picture_limit`]).
    max_in_flight: usize,
}

/// The motion checksum folded over the finished pictures in decoding order.
#[derive(Default)]
struct ChecksumFold {
    /// The fold of every picture whose checksum has been waited for.
    fold: u64,
    /// The checksums of the finished pictures not yet folded, in decoding order.
    pending: VecDeque<Arc<Checksum>>,
}

impl Default for Decoder {
    fn default() -> Self {
        Decoder::new()
    }
}

impl Decoder {
    /// A decoder with no parameter sets and pixel decode on.
    pub fn new() -> Decoder {
        Decoder {
            vps: vec![None; 16],
            sps: vec![None; 16],
            pps: vec![None; 64],
            pixel_decode: true,
            threads: configured_threads(),
            budget: Budget::new(configured_threads()),
            syntax_check: false,
            dpb: Dpb::new(),
            cur: None,
            skipping: false,
            seen_picture: false,
            after_eos: false,
            irap_no_rasl: true,
            prev_tid0_poc: 0,
            au_index: 0,
            out: VecDeque::new(),
            hdr_pending: PictureMeta::default(),
            hdr_persist: PictureMeta::default(),
            checksums: Mutex::new(ChecksumFold::default()),
            in_flight: VecDeque::new(),
            max_in_flight: picture_limit(configured_threads()),
        }
    }

    /// Worker threads the in-loop filters may use (at least 1).
    pub fn set_threads(&mut self, threads: usize) {
        self.threads = threads.clamp(1, MAX_THREADS);
        self.max_in_flight = picture_limit(self.threads);
        self.budget = Budget::new(self.threads);
    }

    /// Turn pixel decode on or off. Off parses every slice and manages every
    /// picture (RPS, POC, bumping) without producing samples; inter slices are
    /// accepted in that mode.
    pub fn set_pixel_decode(&mut self, on: bool) {
        self.pixel_decode = on;
    }

    /// Whether pixel decode is on.
    pub fn pixel_decode(&self) -> bool {
        self.pixel_decode
    }

    /// A hash of the motion field of every picture finished so far, in decoding order.
    /// Two decodes of the same stream give the same value; a change in any derived
    /// motion vector, reference index or prediction flag changes it.
    pub fn motion_checksum(&self) -> u64 {
        let mut fold = self.checksums.lock().unwrap_or_else(|e| e.into_inner());
        while let Some(slot) = fold.pending.pop_front() {
            if let Some(sum) = slot.wait() {
                fold.fold = (fold.fold ^ sum).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        fold.fold
    }

    /// With pixel decode off, also parse every slice's data with the CU syntax
    /// parser. A parse that does not reach `end_of_slice_segment_flag` at the
    /// end of the slice is an error from [`Decoder::push_nal`].
    pub fn set_syntax_check(&mut self, on: bool) {
        self.syntax_check = on;
    }

    /// The VPS stored under `id`.
    pub fn vps(&self, id: u32) -> Option<&Vps> {
        self.vps.get(id as usize)?.as_ref()
    }

    /// The SPS stored under `id`.
    pub fn sps(&self, id: u32) -> Option<&Sps> {
        self.sps.get(id as usize)?.as_deref()
    }

    /// The PPS stored under `id`.
    pub fn pps(&self, id: u32) -> Option<&Pps> {
        self.pps.get(id as usize)?.as_deref()
    }

    /// `RefPicList0` and `RefPicList1` of the most recent slice of the picture
    /// being decoded.
    pub fn ref_pic_lists(&self) -> Option<&RefPicLists> {
        self.cur.as_ref().map(|c| &c.lists)
    }

    /// The reference picture set of the picture being decoded.
    pub fn ref_pic_set(&self) -> Option<&RefPicSet> {
        self.cur.as_ref().map(|c| &c.rps)
    }

    /// A picture in the decoded picture buffer, by id.
    pub fn picture(&self, id: u32) -> Option<&DpbPicture> {
        self.dpb.get(id)
    }

    /// Feed one NAL unit: the two-byte header, then the payload, with no start
    /// code. Emulation prevention bytes stay in.
    pub fn push_nal(&mut self, nal: &[u8]) -> Result<NalOutcome> {
        let header = NalHeader::parse(nal)?;
        self.handle(header, &nal[2..])
    }

    /// Feed a whole Annex-B byte stream.
    pub fn decode_annex_b(&mut self, data: &[u8]) -> Result<()> {
        for nal in split_annex_b(data) {
            self.handle(nal.header, nal.payload)?;
        }
        Ok(())
    }

    /// End of stream: finish the open picture and output everything pending.
    pub fn flush(&mut self) -> Result<()> {
        self.finish_picture();
        self.dpb.drain(&mut self.out);
        // Every picture is complete now; a failed one reports its error here.
        for pending in &self.out {
            pending.picture.wait_done()?;
        }
        let t = trace::now();
        for handle in self.in_flight.drain(..) {
            let _ = handle.join();
        }
        trace::since(trace::Phase::WaitJoin, t);
        trace::report();
        Ok(())
    }

    /// The next frame ready for output, in display (POC) order.
    pub fn next_frame(&mut self) -> Option<Frame> {
        // A frame whose picture failed is dropped here; `flush` reports the failure.
        self.out
            .pop_front()
            .and_then(|pending| pending.resolve().ok())
    }

    fn handle(&mut self, header: NalHeader, payload: &[u8]) -> Result<NalOutcome> {
        if header.layer_id > 0 {
            return Ok(NalOutcome::Skipped);
        }
        match header.nal_type.code() {
            code @ (0..=9 | 16..=21) => self.slice(header, code, payload),
            32 => {
                let vps = Vps::parse(&unescape_rbsp(payload))?;
                store(&mut self.vps, usize::from(vps.id), vps)?;
                Ok(NalOutcome::ParameterSet)
            }
            33 => {
                let mut sps = Sps::parse(&unescape_rbsp(payload))?;
                check_sps(&sps)?;
                if sps.chroma_format_idc == 0 {
                    // 4:0:0 has no chroma samples; the unused planes follow the luma depth.
                    sps.bit_depth_chroma_minus8 = sps.bit_depth_luma_minus8;
                }
                let id = sps.id as usize;
                store(&mut self.sps, id, Arc::new(sps))?;
                Ok(NalOutcome::ParameterSet)
            }
            34 => {
                let pps = Pps::parse(&unescape_rbsp(payload))?;
                check_pps(&pps)?;
                let id = pps.id as usize;
                store(&mut self.pps, id, Arc::new(pps))?;
                Ok(NalOutcome::ParameterSet)
            }
            39 | 40 => {
                // A malformed SEI is skipped: it is metadata, and the pictures are
                // unaffected. The picture hash is a suffix message (40) only.
                if let Ok(msgs) = parse_sei_rbsp(&unescape_rbsp(payload)) {
                    self.take_sei(msgs, header.nal_type.code() == 40);
                }
                Ok(NalOutcome::Skipped)
            }
            36 | 37 => {
                // End of sequence or bitstream: the open picture is finished, but
                // nothing is output here. The next IRAP applies C.5.2.2 to what is
                // still pending (a CRA after EOS discards it, an IDR or BLA
                // outputs it unless no_output_of_prior_pics_flag is set). `flush`
                // outputs the rest at the true end of the stream.
                self.finish_picture();
                self.after_eos = true;
                Ok(NalOutcome::EndOfSequence)
            }
            _ => Ok(NalOutcome::Skipped),
        }
    }

    fn slice(&mut self, header: NalHeader, code: u8, payload: &[u8]) -> Result<NalOutcome> {
        let rbsp = unescape_rbsp(payload);
        // The first syntax elements locate the PPS (7.3.6.1).
        let mut r = BitReader::new(&rbsp);
        let first = r.read_bit()?;
        if (16..=21).contains(&code) {
            r.read_bit()?; // no_output_of_prior_pics_flag
        }
        let pps_id = r.read_ue()?;

        if first {
            self.finish_picture();
            self.skipping = false;
            // RASL pictures of an IRAP with NoRaslOutputFlag reference pictures
            // that are not there: skip them (8.1.3).
            if matches!(code, 8 | 9) && self.irap_no_rasl {
                self.skipping = true;
                self.hdr_pending = PictureMeta::default();
                self.au_index += 1;
                return Ok(NalOutcome::SkippedPicture);
            }
        } else if self.skipping {
            return Ok(NalOutcome::SkippedPicture);
        }

        let pps = self
            .pps
            .get(pps_id as usize)
            .and_then(|p| p.clone())
            .ok_or_else(|| {
                Error::corrupt(format!("HEVC slice: PPS {pps_id} was never received"))
            })?;
        let sps = self
            .sps
            .get(pps.sps_id as usize)
            .and_then(|s| s.clone())
            .ok_or_else(|| {
                Error::corrupt(format!(
                    "HEVC slice: SPS {} (for PPS {pps_id}) was never received",
                    pps.sps_id
                ))
            })?;
        let (slice, pos) = SliceHeader::parse(&rbsp, &sps, &pps, NalUnitType::from_code(code))?;

        if first {
            self.start_picture(&header, code, &slice, &sps);
        } else if self.cur.is_none() {
            return Err(Error::corrupt(
                "HEVC slice: segment whose picture's first slice was never seen".to_string(),
            ));
        }
        let slice = self.segment_header(slice)?;
        let substreams = substream_starts(payload, &pos, &slice.entry_point_offsets, rbsp.len());
        self.decode_slice(&slice, &pos, rbsp, substreams, &sps, &pps)?;
        Ok(NalOutcome::Slice)
    }

    /// The header a slice segment decodes with (7.4.7.1). An independent segment
    /// is remembered for the dependent segments that follow it.
    fn segment_header(&mut self, segment: SliceHeader) -> Result<SliceHeader> {
        let cur = self
            .cur
            .as_mut()
            .ok_or_else(|| Error::corrupt("HEVC slice: no picture is open".to_string()))?;
        if segment.dependent_slice_segment {
            let base = cur.header.as_ref().ok_or_else(|| {
                Error::corrupt("HEVC slice: dependent slice segment before its slice".to_string())
            })?;
            Ok(slice_seg::inherit(base, &segment))
        } else {
            cur.header = Some(segment.clone());
            Ok(segment)
        }
    }

    /// Fold the SEI of one NAL unit into the decoder state. Mastering display, content
    /// light level and alternative transfer characteristics wait for the next picture
    /// start. A picture hash goes to the open picture, and only from a suffix SEI.
    fn take_sei(&mut self, msgs: Vec<SeiMessage>, suffix: bool) {
        for msg in msgs {
            match msg {
                SeiMessage::MasteringDisplay(m) => self.hdr_pending.mastering_display = Some(m),
                SeiMessage::ContentLightLevel(c) => self.hdr_pending.content_light = Some(c),
                SeiMessage::AlternativeTransferCharacteristics(a) => {
                    self.hdr_pending.alternative_transfer = Some(a);
                }
                SeiMessage::DecodedPictureHash(h) if suffix => {
                    if let Some(cur) = self.cur.as_mut() {
                        cur.meta.picture_hash = Some(h);
                    }
                }
                SeiMessage::DecodedPictureHash(_) | SeiMessage::Unknown { .. } => {}
            }
        }
    }

    /// Picture-level work at the first slice segment of a picture: POC, RPS,
    /// the output and removal of buffered pictures (C.5.2.2).
    fn start_picture(&mut self, nal: &NalHeader, code: u8, slice: &SliceHeader, sps: &Arc<Sps>) {
        let first_picture = !self.seen_picture;
        self.seen_picture = true;
        let irap = (16..=21).contains(&code);
        let idr = matches!(code, 19 | 20);
        let cra = code == 21;
        let no_rasl = irap && (idr || (16..=18).contains(&code) || first_picture || self.after_eos);
        self.after_eos = false;
        if irap {
            self.irap_no_rasl = no_rasl;
        }

        // 8.3.1: prevTid0Pic excludes RASL, RADL and sub-layer non-reference pictures.
        let log2_max_poc_lsb = sps.log2_max_poc_lsb_minus4 + 4;
        let poc = poc::derive_poc(
            self.prev_tid0_poc,
            log2_max_poc_lsb,
            slice.poc_lsb,
            irap && no_rasl,
        );
        let sub_layer_non_ref = code <= 14 && code & 1 == 0;
        let radl_or_rasl = (6..=9).contains(&code);
        if nal.temporal_id == 0 && !radl_or_rasl && !sub_layer_non_ref {
            self.prev_tid0_poc = poc;
        }

        // 8.3.2: an IDR picture has no RPS; every other picture names one.
        let empty_st = ShortTermRefPicSet::default();
        let (st, lt): (&ShortTermRefPicSet, &[LongTermRef]) = if idr {
            (&empty_st, &[])
        } else {
            (&slice.short_term_ref_pic_set, &slice.long_term)
        };
        let input = RpsInput {
            poc,
            log2_max_poc_lsb,
            st,
            lt,
            irap_no_rasl: irap && no_rasl,
            width: sps.pic_width_in_luma_samples as usize,
            height: sps.pic_height_in_luma_samples as usize,
            fill: self.pixel_decode.then_some((
                sps.bit_depth_luma_minus8 + 8,
                sps.bit_depth_chroma_minus8 + 8,
            )),
            chroma_format_idc: sps.chroma_format_idc,
        };
        let rps = self.dpb.apply_rps(&input);

        // C.5.2.2: output or discard what the new picture displaces.
        let limits = DpbLimits::from_sps(sps);
        if irap && no_rasl && !first_picture {
            // C.5.2.2: a CRA here follows an end of sequence and its
            // NoOutputOfPriorPicsFlag is 1 by definition, so pending pictures are
            // discarded. An IDR or BLA discards them only when its flag says so.
            if cra || slice.no_output_of_prior_pics {
                self.dpb.clear();
            } else {
                self.dpb.drain(&mut self.out);
            }
        } else {
            self.dpb.remove_unneeded();
            while self.dpb.needs_bumping(&limits, true) && self.dpb.bump(&mut self.out) {}
        }

        // SEI state: a new coded video sequence starts from nothing, then this
        // access unit's SEI applies from here on.
        if irap && no_rasl {
            self.hdr_persist = PictureMeta::default();
        }
        let pending = std::mem::take(&mut self.hdr_pending);
        self.hdr_persist.absorb_sei(&pending);
        let mut meta = self.hdr_persist.clone();
        if let Some(vst) = sps.vui.as_ref().and_then(|v| v.video_signal_type) {
            meta.colour_description = vst.colour_description;
            meta.video_full_range_flag = Some(vst.video_full_range_flag);
        }

        // Frame threads: at most `threads` pictures in flight, so wait for the oldest.
        let threaded = self.threads > 1 && self.pixel_decode;
        if threaded {
            let t = trace::now();
            while self.in_flight.len() >= self.max_in_flight {
                if let Some(handle) = self.in_flight.pop_front() {
                    let _ = handle.join();
                }
            }
            trace::since(trace::Phase::WaitAdmit, t);
        }
        let shared = Arc::new(SharedPic::new(
            poc,
            sps.pic_width_in_luma_samples as usize,
            sps.pic_height_in_luma_samples as usize,
            (
                8 + sps.bit_depth_luma_minus8,
                8 + sps.bit_depth_chroma_minus8,
            ),
            self.pixel_decode,
            sps.chroma_format_idc,
        ));
        let checksum = Arc::new(Checksum::default());
        let runner = Runner::start(
            PicJob::new(Arc::clone(&shared), Arc::clone(&self.budget)),
            Arc::clone(&checksum),
            threaded,
        );
        self.cur = Some(CurPicture {
            sps: Arc::clone(sps),
            poc,
            au_index: self.au_index,
            header: None,
            output: slice.pic_output_flag,
            rps,
            lists: RefPicLists::default(),
            runner,
            shared,
            checksum,
            all_intra: true,
            meta,
        });
        self.au_index += 1;
    }

    /// Build the slice's reference lists, then hand the slice data to the picture's
    /// job. Nothing runs when pixel decode and the syntax check are both off.
    fn decode_slice(
        &mut self,
        slice: &SliceHeader,
        pos: &ParsePositions,
        rbsp: Vec<u8>,
        substreams: Vec<usize>,
        sps: &Arc<Sps>,
        pps: &Arc<Pps>,
    ) -> Result<()> {
        let pixel = self.pixel_decode;
        let dpb = &self.dpb;
        let cur = self
            .cur
            .as_mut()
            .ok_or_else(|| Error::corrupt("HEVC decode: no picture is open".to_string()))?;
        if !slice.dependent_slice_segment && slice.slice_type != SliceType::I {
            cur.lists = rps::build_ref_lists(&cur.rps, slice)?;
        }
        if slice.slice_type != SliceType::I {
            cur.all_intra = false;
        }
        if !pixel && !self.syntax_check {
            return Ok(());
        }
        let data_off = (pos.header_bits / 8) as usize;
        if data_off > rbsp.len() {
            return Err(Error::corrupt(
                "HEVC slice: header runs past the RBSP".to_string(),
            ));
        }
        let lookup = |entries: &[RefEntry]| -> Result<Vec<Arc<SharedPic>>> {
            entries
                .iter()
                .map(|e| {
                    dpb.get(e.id)
                        .map(|p| Arc::clone(&p.picture))
                        .ok_or_else(|| {
                            Error::corrupt(
                                "HEVC decode: a reference picture is missing from the buffer"
                                    .to_string(),
                            )
                        })
                })
                .collect()
        };
        let refs = if pixel && slice.slice_type != SliceType::I {
            [lookup(&cur.lists.l0)?, lookup(&cur.lists.l1)?]
        } else {
            [Vec::new(), Vec::new()]
        };
        let col = motion_field::collocated(dpb, slice, &cur.lists);
        let t = trace::now();
        let submitted = cur.runner.submit(SliceJob {
            rbsp,
            data_off,
            header: slice.clone(),
            sps: Arc::clone(sps),
            pps: Arc::clone(pps),
            lists: cur.lists.clone(),
            refs,
            col,
            cur_poc: cur.poc,
            pixel,
            substreams,
        });
        trace::since(trace::Phase::Decode, t);
        submitted
    }

    /// C.5.2.3: store the open picture in the buffer and run the additional
    /// bumping. Idempotent when no picture is open. The picture's samples may still be
    /// being decoded (on its worker); output waits for them.
    fn finish_picture(&mut self) {
        let t = trace::now();
        self.finish_picture_inner();
        trace::since(trace::Phase::Serial, t);
    }

    fn finish_picture_inner(&mut self) {
        let Some(cur) = self.cur.take() else {
            return;
        };
        let CurPicture {
            sps,
            poc,
            au_index,
            output,
            runner,
            shared,
            checksum,
            all_intra,
            mut meta,
            ..
        } = cur;
        meta.intra_only = all_intra;
        self.checksums
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .push_back(checksum);
        if let Some(handle) = runner.finish() {
            self.in_flight.push_back(handle);
        }
        if output {
            for p in self.dpb.pictures_mut() {
                if p.needed_for_output && p.poc > poc {
                    p.latency += 1;
                }
            }
        }
        let id = self.dpb.insert(
            poc,
            Marking::ShortTerm,
            output,
            shared,
            sps.conf_win,
            au_index,
        );
        self.dpb.set_meta(id, meta);
        let limits = DpbLimits::from_sps(&sps);
        while self.dpb.needs_bumping(&limits, false) && self.dpb.bump(&mut self.out) {}
    }
}

/// Store a parameter set under its id, refusing ids the spec does not allow.
/// The RBSP offsets, from the start of the RBSP, of the substreams after the first of a
/// slice segment, from its entry points. The entry points count the escaped bytes of the
/// NAL unit payload, so each one is mapped back past the emulation prevention bytes before
/// it. Empty when the segment has no entry points, or when they do not fit the data.
fn substream_starts(
    payload: &[u8],
    pos: &ParsePositions,
    entries: &[u32],
    rbsp_len: usize,
) -> Vec<usize> {
    if entries.is_empty() {
        return Vec::new();
    }
    // The escaped index of every emulation prevention byte, as unescape_rbsp drops it.
    let mut removed = Vec::new();
    let mut zeros = 0usize;
    for (i, &b) in payload.iter().enumerate() {
        if zeros >= 2 && b == 3 {
            removed.push(i);
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    let data_off = (pos.header_bits / 8) as usize;
    let mut escaped = pos.slice_data_byte_offset(payload) - 2;
    let mut starts = Vec::with_capacity(entries.len());
    for &size in entries {
        escaped += size as usize;
        // Escaped index to RBSP index: the bytes removed before it are not in the RBSP.
        let before = removed.partition_point(|&r| r < escaped);
        let Some(rbsp_at) = escaped.checked_sub(before) else {
            return Vec::new();
        };
        if rbsp_at <= data_off || rbsp_at >= rbsp_len {
            return Vec::new();
        }
        starts.push(rbsp_at - data_off);
    }
    starts
}

fn store<T>(table: &mut [Option<T>], id: usize, value: T) -> Result<()> {
    let slot = table
        .get_mut(id)
        .ok_or_else(|| Error::corrupt(format!("HEVC parameter set id {id} is out of range")))?;
    *slot = Some(value);
    Ok(())
}

/// The extension flags this decoder refuses: bits 6 to 4 of the packed
/// `extension_flags` (multilayer, 3D, SCC). The range extension (bit 7) is parsed, and
/// its tools are refused one by one by [`unimplemented_sps_range`] and
/// [`unimplemented_pps_range`]. `sps_extension_4bits` in the low
/// nibble only says that extension data follows, which is skipped (7.4.3.2.1).
const EXT_REFUSED: u8 = 0x70;

/// The first refused extension in `flags`, named for the refusal message.
fn refused_extension(flags: u8) -> Option<&'static str> {
    if flags & 0x80 != 0 {
        Some("range extension (RExt tools)")
    } else if flags & 0x40 != 0 {
        Some("multi-layer extension (MV-HEVC, SHVC)")
    } else if flags & 0x20 != 0 {
        Some("3D extension (3D-HEVC)")
    } else if flags & 0x10 != 0 {
        Some("SCC extension (screen content coding)")
    } else {
        None
    }
}

/// Refuse an SPS this decoder does not implement, by name: separate colour planes, bit
/// depths above 12, the multilayer, 3D and SCC extensions, and the range extension tools
/// that cost more than they are worth (extended precision, CABAC bypass alignment).
fn check_sps(sps: &Sps) -> Result<()> {
    if sps.chroma_format_idc > 3 {
        return Err(Error::unsupported(
            "HEVC decode: chroma format unknown",
            "chroma_format_idc is 0 to 3",
        ));
    }
    if sps.chroma_format_idc == 3 && sps.separate_colour_plane {
        return Err(Error::unsupported(
            "HEVC decode: chroma format 4:4:4 with separate_colour_plane_flag",
            "the three colour planes coded as separate pictures are not implemented",
        ));
    }
    let chroma_depth =
        (sps.chroma_format_idc != 0).then_some(("chroma", sps.bit_depth_chroma_minus8));
    for (plane, minus8) in [("luma", sps.bit_depth_luma_minus8)]
        .into_iter()
        .chain(chroma_depth)
    {
        if minus8 > 4 {
            return Err(Error::unsupported(
                format!("HEVC decode: bit depth {} {plane}", minus8 + 8),
                "only bit depths 8 to 12 are implemented",
            ));
        }
    }
    if let Some(name) = refused_extension(sps.extension_flags & EXT_REFUSED) {
        return Err(Error::unsupported(
            format!("HEVC decode: SPS {name}"),
            "only Main, Main 10 and the range extension profiles are implemented",
        ));
    }
    if let Some(name) = unimplemented_sps_range(&sps.range) {
        return Err(Error::unsupported(
            format!("HEVC decode: SPS {name}"),
            "this range extension tool is not implemented",
        ));
    }
    Ok(())
}

/// The first range extension tool of an SPS that this decoder does not implement.
fn unimplemented_sps_range(r: &SpsRangeExt) -> Option<&'static str> {
    [
        (
            r.extended_precision_processing,
            "extended_precision_processing_flag",
        ),
        (
            r.cabac_bypass_alignment_enabled,
            "cabac_bypass_alignment_enabled_flag",
        ),
    ]
    .into_iter()
    .find(|&(on, _)| on)
    .map(|(_, name)| name)
}

/// The first range extension tool of a PPS that this decoder does not implement. Every
/// PPS range extension tool is implemented.
fn unimplemented_pps_range(_r: &PpsRangeExt) -> Option<&'static str> {
    None
}

/// Refuse a PPS that carries an extension this decoder does not implement.
fn check_pps(pps: &Pps) -> Result<()> {
    if let Some(name) = refused_extension(pps.extension_flags & EXT_REFUSED) {
        return Err(Error::unsupported(
            format!("HEVC decode: PPS {name}"),
            "only Main, Main 10 and Main 4:2:2 10 are implemented",
        ));
    }
    if let Some(name) = unimplemented_pps_range(&pps.range) {
        return Err(Error::unsupported(
            format!("HEVC decode: PPS {name}"),
            "this range extension tool is not implemented",
        ));
    }
    Ok(())
}

//! Decoded picture buffer (spec clause 8.2): picture storage, picture order
//! count, reference marking, reference list construction and output order.
//!
//! The buffer owns every decoded picture, including the one being decoded — the
//! decoder holds that one *out* of the pool while a picture is open, which is
//! what lets motion compensation borrow reference planes immutably while
//! writing the current planes mutably.
//!
//! Pictures are pooled, never freed: a picture that is neither referenced nor
//! awaiting output goes back on the free list with its plane and motion
//! allocations intact, so a steady-state decode loop reuses the same buffers
//! forever and allocates nothing.
//!
//! Output order is display order. Every picture leaves through the bumping
//! process of clause C.4.5.3 — smallest picture order count first, released
//! when the buffer is over its reordering or storage limit — so a stream with
//! B pictures comes out in the order a player has to present it, not the order
//! it was coded in.

use ec_core::error::{Error, Result};
use ec_h264_syntax::{DecRefPicMarking, RefPicListMod, SliceType, Sps};

/// Luma plane padding on every side, sized for inter-prediction filter
/// overreach: a 16-wide partition plus the two/three sample reach of the 6-tap
/// filter, rounded up.
pub(crate) const PAD_Y: usize = 32;
/// Chroma plane padding on every side (8-wide block plus one bilinear tap).
pub(crate) const PAD_C: usize = 16;
/// `mb_slice` value for a not-yet-decoded macroblock.
pub(crate) const NO_SLICE: u16 = u16::MAX;
/// Hard cap on stored pictures: 16 reference frames (Annex A) plus the current.
const MAX_STORED: usize = 17;

/// Per-4x4-block flags recorded for inter neighbour derivation.
/// The block belongs to an intra macroblock.
pub(crate) const BLK_INTRA: u8 = 1;
/// The block belongs to a P_Skip or B_Skip macroblock.
pub(crate) const BLK_SKIP: u8 = 2;
/// The block is predicted in direct mode (B_Skip, B_Direct_16x16 or a
/// B_Direct_8x8 sub-macroblock).
pub(crate) const BLK_DIRECT: u8 = 4;

/// Reference marking of a stored picture (clause 8.2.5).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Mark {
    /// "unused for reference".
    #[default]
    Unused,
    /// "used for short-term reference".
    Short,
    /// "used for long-term reference".
    Long,
}

/// A padded 8-bit plane.
#[derive(Debug, Default)]
pub(crate) struct Plane8 {
    pub data: Vec<u8>,
    pub stride: usize,
    /// Index of sample (0, 0) of the picture.
    pub origin: usize,
    pub width: usize,
    pub height: usize,
    pub pad: usize,
}

impl Plane8 {
    fn resize(&mut self, width: usize, height: usize, pad: usize) {
        self.stride = width + 2 * pad;
        self.origin = pad * self.stride + pad;
        self.width = width;
        self.height = height;
        self.pad = pad;
        self.data.clear();
        self.data.resize(self.stride * (height + 2 * pad), 0);
    }

    #[inline]
    pub(crate) fn at(&self, x: usize, y: usize) -> usize {
        self.origin + y * self.stride + x
    }

    /// Replicate the edge samples into the padding, which turns the per-sample
    /// `Clip3` of Equations 8-239/8-240 into ordinary reads (see
    /// [`crate::inter`]).
    fn extend_borders(&mut self) {
        let (w, h, pad, stride) = (self.width, self.height, self.pad, self.stride);
        for y in 0..h {
            let row = self.origin + y * stride;
            let (left, right) = (self.data[row], self.data[row + w - 1]);
            self.data[row - pad..row].fill(left);
            self.data[row + w..row + w + pad].fill(right);
        }
        let full = self.origin - pad;
        for y in 0..pad {
            let (src, dst) = (full, full - (y + 1) * stride);
            self.data.copy_within(src..src + stride, dst);
            let src = full + (h - 1) * stride;
            self.data.copy_within(src..src + stride, src + (y + 1) * stride);
        }
    }
}

/// Per-slice values the deblocking filter needs after slice decode ends.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SliceParams {
    pub disable_deblock_idc: u8,
    pub alpha_offset: i32,
    pub beta_offset: i32,
    pub cb_qp_offset: i32,
    pub cr_qp_offset: i32,
}

/// Flat per-picture decode context.
///
/// All per-macroblock and per-4x4-block state lives in struct-of-arrays keyed
/// by macroblock address or 4x4-block coordinate: no per-macroblock heap
/// objects anywhere, and the whole picture is one reusable allocation set.
#[derive(Debug, Default)]
pub(crate) struct Picture {
    pub sps_id: u8,
    pub mb_w: usize,
    pub mb_h: usize,
    pub y: Plane8,
    pub cb: Plane8,
    pub cr: Plane8,
    /// TotalCoeff per luma 4x4 block, `(mb_w * 4) x (mb_h * 4)`.
    pub nz_y: Vec<u8>,
    /// TotalCoeff per chroma 4x4 block, `(mb_w * 2) x (mb_h * 2)`, Cb and Cr.
    pub nz_c: [Vec<u8>; 2],
    /// Intra4x4PredMode per luma 4x4 block (2 = DC for non-I4x4 MBs).
    pub i4_modes: Vec<u8>,
    /// QPY per macroblock (the prediction-chain value; PCM keeps the chain).
    pub mb_qp: Vec<u8>,
    /// `FLAG_*` bits per macroblock.
    pub mb_flags: Vec<u8>,
    /// `CodedBlockPatternLuma | CodedBlockPatternChroma << 4` per macroblock.
    pub mb_cbp: Vec<u8>,
    /// coded_block_flag of the DC blocks per macroblock: bit 0 luma, 1 Cb, 2 Cr.
    pub mb_dc_cbf: Vec<u8>,
    /// Owning slice index per macroblock, `NO_SLICE` when undecoded.
    pub mb_slice: Vec<u16>,
    /// `MvLX[..][..]` per luma 4x4 block, `[list][component]`, quarter samples.
    pub mv: Vec<[[i16; 2]; 2]>,
    /// `RefIdxLX` per luma 4x4 block, -1 when the list is not used.
    pub ref_idx: Vec<[i8; 2]>,
    /// Identity ([`Picture::id`]) of the referenced picture per list, -1 when
    /// unused. Reference *indices* are per slice, so only the picture identity
    /// answers "same reference picture?" for boundary strength (8.7.2.1) and
    /// `MapColToList0` (8.4.1.2.3).
    pub ref_id: Vec<[i32; 2]>,
    /// `Abs( mvd_lX[..][..][comp] )` per block, `[l0x, l0y, l1x, l1y]`,
    /// saturated (the thresholds of 9.3.3.1.1.7 are 2 and 32).
    pub mvd_abs: Vec<[u8; 4]>,
    /// `BLK_*` flags per luma 4x4 block.
    pub blk: Vec<u8>,
    pub slices: Vec<SliceParams>,
    pub decoded_mbs: usize,
    pub complete: bool,

    // ---- clause 8.2 picture state ----
    /// Stream-unique identity, assigned when the picture is started.
    pub id: i32,
    /// `PicOrderCnt( )` of the frame.
    pub poc: i32,
    pub frame_num: u32,
    /// `FrameNumWrap` (8-27), recomputed for every decoded picture.
    pub frame_num_wrap: i32,
    /// `PicNum` (8-28) for short-term, `LongTermPicNum` (8-29) for long-term.
    pub pic_num: i32,
    pub long_term_frame_idx: u32,
    pub mark: Mark,
    /// The picture has not been output yet.
    pub output: bool,
    /// Inferred by the frame_num gap process of 8.2.5.2 rather than decoded.
    pub non_existing: bool,
    /// `FrameNumOffset` (8-6 / 8-11), carried to the next picture.
    pub frame_num_offset: i32,
    /// `PicOrderCntMsb` (8-3), carried to the next picture for POC type 0.
    pub poc_msb: i32,
    /// `pic_order_cnt_lsb` as coded, carried alongside `poc_msb`.
    pub poc_lsb: i32,
    /// Presentation timestamp of the access unit this picture came from. It
    /// travels with the picture rather than with the packet, because display
    /// order reorders the two apart.
    pub pts: Option<ec_core::timebase::Timestamp>,
}

impl Picture {
    /// Reset for a new picture of `sps`'s geometry, reallocating only when the
    /// geometry actually changed.
    pub(crate) fn start(&mut self, sps: &Sps) {
        let mb_w = sps.mb_width as usize;
        let mb_h = sps.mb_height as usize;
        if self.mb_w != mb_w || self.mb_h != mb_h || self.sps_id != sps.id {
            self.mb_w = mb_w;
            self.mb_h = mb_h;
            self.sps_id = sps.id;
            self.y.resize(mb_w * 16, mb_h * 16, PAD_Y);
            self.cb.resize(mb_w * 8, mb_h * 8, PAD_C);
            self.cr.resize(mb_w * 8, mb_h * 8, PAD_C);
            let blocks = mb_w * mb_h * 16;
            self.nz_y.resize(blocks, 0);
            for c in &mut self.nz_c {
                c.resize(mb_w * mb_h * 4, 0);
            }
            self.i4_modes.resize(blocks, 2);
            self.mb_qp.resize(mb_w * mb_h, 0);
            self.mb_flags.resize(mb_w * mb_h, 0);
            self.mb_cbp.resize(mb_w * mb_h, 0);
            self.mb_dc_cbf.resize(mb_w * mb_h, 0);
            self.mb_slice.resize(mb_w * mb_h, NO_SLICE);
            self.mv.resize(blocks, [[0; 2]; 2]);
            self.ref_idx.resize(blocks, [-1; 2]);
            self.ref_id.resize(blocks, [-1; 2]);
            self.mvd_abs.resize(blocks, [0; 4]);
            self.blk.resize(blocks, 0);
        }
        // Per-MB metadata is rewritten by each decoded macroblock; only the
        // "who is decoded" state must be wiped between pictures.
        self.mb_slice.fill(NO_SLICE);
        self.mb_flags.fill(0);
        self.slices.clear();
        self.decoded_mbs = 0;
        self.complete = false;
        self.mark = Mark::Unused;
        self.output = false;
        self.non_existing = false;
        self.long_term_frame_idx = 0;
        self.frame_num_wrap = 0;
        self.pic_num = 0;
    }

    /// Replicate every plane's borders once the picture is fully reconstructed.
    pub(crate) fn extend_borders(&mut self) {
        self.y.extend_borders();
        self.cb.extend_borders();
        self.cr.extend_borders();
    }

    /// Fill the picture with mid-grey, for a frame the stream never sent.
    fn fill_grey(&mut self) {
        self.y.data.fill(128);
        self.cb.data.fill(128);
        self.cr.data.fill(128);
    }

    fn copy_samples_from(&mut self, src: &Picture) {
        self.y.data.copy_from_slice(&src.y.data);
        self.cb.data.copy_from_slice(&src.cb.data);
        self.cr.data.copy_from_slice(&src.cr.data);
    }

    /// True while the picture occupies a decoded picture buffer slot.
    fn stored(&self) -> bool {
        self.mark != Mark::Unused || self.output
    }
}

/// One reference picture list (`RefPicList0` / `RefPicList1`).
///
/// Entries are indices into [`Dpb::frames`]; the list is a fixed array because
/// Annex A caps it at 32 entries, so building it allocates nothing.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RefList {
    entries: [u8; 33],
    len: usize,
}

impl Default for RefList {
    fn default() -> RefList {
        RefList {
            entries: [0; 33],
            len: 0,
        }
    }
}

impl RefList {
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// The DPB index of `RefPicListX[idx]`, `None` past the end.
    #[inline]
    pub(crate) fn get(&self, idx: usize) -> Option<usize> {
        (idx < self.len).then(|| usize::from(self.entries[idx]))
    }

    fn push(&mut self, idx: usize) {
        if self.len < self.entries.len() {
            self.entries[self.len] = idx as u8;
            self.len += 1;
        }
    }
}

/// The decoded picture buffer and the clause 8.2 state that spans pictures.
pub(crate) struct Dpb {
    /// Stored pictures, in no particular order.
    pub frames: Vec<Picture>,
    /// Recycled picture buffers.
    free: Vec<Picture>,
    next_id: i32,
    /// `max_dec_frame_buffering`.
    max_frames: usize,
    /// `max_num_reorder_frames`.
    max_reorder: usize,
    /// `MaxLongTermFrameIdx`, `None` = "no long-term frame indices".
    max_long_term_idx: Option<u32>,
    // Picture order count state (8.2.1).
    prev_poc_msb: i32,
    prev_poc_lsb: i32,
    prev_frame_num: u32,
    prev_frame_num_offset: i32,
    prev_has_mmco5: bool,
    /// `PrevRefFrameNum` for the gap detection of 8.2.5.2.
    prev_ref_frame_num: u32,
    /// Set once a picture has been decoded since the last reset.
    started: bool,
    /// Everything stored before [`Dpb::last_id`] must be output before it.
    pending_flush: bool,
    /// Identity of the most recently stored picture.
    last_id: i32,
}

impl Default for Dpb {
    fn default() -> Dpb {
        Dpb {
            frames: Vec::new(),
            free: Vec::new(),
            next_id: 0,
            max_frames: 16,
            max_reorder: 16,
            max_long_term_idx: None,
            prev_poc_msb: 0,
            prev_poc_lsb: 0,
            prev_frame_num: 0,
            prev_frame_num_offset: 0,
            prev_has_mmco5: false,
            prev_ref_frame_num: 0,
            started: false,
            pending_flush: false,
            last_id: -1,
        }
    }
}

/// Output of the picture order count derivation (clause 8.2.1).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Poc {
    /// `PicOrderCnt( )` of the frame.
    pub value: i32,
    /// `FrameNumOffset`, predicted from by the next picture.
    pub frame_num_offset: i32,
    /// `PicOrderCntMsb` (POC type 0 only).
    pub msb: i32,
    /// `pic_order_cnt_lsb` (POC type 0 only).
    pub lsb: i32,
}

/// Everything about the current picture the DPB needs, gathered from the first
/// slice header of that picture.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PicInfo {
    pub is_idr: bool,
    pub is_reference: bool,
    pub frame_num: u32,
    pub pic_order_cnt_lsb: u32,
    pub delta_pic_order_cnt_bottom: i32,
    pub delta_pic_order_cnt: [i32; 2],
}

impl Dpb {
    /// Adopt the buffer limits of `sps` (Annex A / VUI), keeping whatever is
    /// already stored.
    pub(crate) fn configure(&mut self, sps: &Sps) {
        let level_frames = max_dpb_frames(sps);
        let (buffering, reorder) = match sps.vui.as_ref().and_then(|v| v.bitstream_restriction) {
            Some(r) => (
                (r.max_dec_frame_buffering as usize).min(MAX_STORED - 1),
                (r.max_num_reorder_frames as usize).min(MAX_STORED - 1),
            ),
            // Without the VUI restriction the level limit is all we know. It
            // over-buffers a low-delay stream, which costs latency but never
            // order: pictures still leave smallest-POC-first.
            None => (level_frames, level_frames),
        };
        self.max_frames = buffering.max(sps.max_num_ref_frames as usize).max(1);
        self.max_reorder = reorder.min(self.max_frames);
    }

    /// A picture buffer to decode into, off the free list when possible.
    pub(crate) fn take_picture(&mut self) -> Picture {
        let mut pic = self.free.pop().unwrap_or_default();
        pic.id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        pic
    }

    /// Hand a picture buffer back for reuse.
    pub(crate) fn recycle(&mut self, pic: Picture) {
        if self.free.len() < MAX_STORED {
            self.free.push(pic);
        }
    }

    /// Drop every stored picture (seek / stream discontinuity).
    pub(crate) fn clear(&mut self) {
        while let Some(pic) = self.frames.pop() {
            self.recycle(pic);
        }
        self.max_long_term_idx = None;
        self.prev_poc_msb = 0;
        self.prev_poc_lsb = 0;
        self.prev_frame_num = 0;
        self.prev_frame_num_offset = 0;
        self.prev_has_mmco5 = false;
        self.prev_ref_frame_num = 0;
        self.started = false;
        self.pending_flush = false;
        self.last_id = -1;
    }

    /// `PicOrderCnt( )` of a picture about to be decoded (clause 8.2.1),
    /// together with the `FrameNumOffset` and `PicOrderCntMsb` the next
    /// picture predicts from.
    pub(crate) fn picture_order_count(&self, sps: &Sps, info: &PicInfo) -> Result<Poc> {
        match sps.pic_order_cnt_type {
            0 => {
                let max_lsb = 1i32 << sps.log2_max_pic_order_cnt_lsb;
                let (prev_msb, prev_lsb) = if info.is_idr {
                    (0, 0)
                } else {
                    (self.prev_poc_msb, self.prev_poc_lsb)
                };
                let lsb = info.pic_order_cnt_lsb as i32;
                let msb = if lsb < prev_lsb && prev_lsb - lsb >= max_lsb / 2 {
                    prev_msb + max_lsb
                } else if lsb > prev_lsb && lsb - prev_lsb > max_lsb / 2 {
                    prev_msb - max_lsb
                } else {
                    prev_msb
                };
                let top = msb + lsb;
                let bottom = top + info.delta_pic_order_cnt_bottom;
                Ok(Poc {
                    value: top.min(bottom),
                    frame_num_offset: 0,
                    msb,
                    lsb,
                })
            }
            1 => {
                let max_frame_num = 1i32 << sps.log2_max_frame_num;
                let offset = self.frame_num_offset(info, max_frame_num);
                let cycle_len = sps.offsets_for_ref_frame.len() as i32;
                let mut abs_frame_num = if cycle_len != 0 {
                    offset + info.frame_num as i32
                } else {
                    0
                };
                if !info.is_reference && abs_frame_num > 0 {
                    abs_frame_num -= 1;
                }
                let mut expected = 0i32;
                if abs_frame_num > 0 {
                    let expected_delta: i32 = sps.offsets_for_ref_frame.iter().sum();
                    let cycle_cnt = (abs_frame_num - 1) / cycle_len;
                    let in_cycle = (abs_frame_num - 1) % cycle_len;
                    expected = cycle_cnt.wrapping_mul(expected_delta);
                    for i in 0..=in_cycle as usize {
                        expected = expected.wrapping_add(sps.offsets_for_ref_frame[i]);
                    }
                }
                if !info.is_reference {
                    expected = expected.wrapping_add(sps.offset_for_non_ref_pic);
                }
                let top = expected.wrapping_add(info.delta_pic_order_cnt[0]);
                let bottom = top
                    .wrapping_add(sps.offset_for_top_to_bottom_field)
                    .wrapping_add(info.delta_pic_order_cnt[1]);
                Ok(Poc {
                    value: top.min(bottom),
                    frame_num_offset: offset,
                    msb: 0,
                    lsb: 0,
                })
            }
            2 => {
                let max_frame_num = 1i32 << sps.log2_max_frame_num;
                let offset = self.frame_num_offset(info, max_frame_num);
                let poc = if info.is_idr {
                    0
                } else if !info.is_reference {
                    2 * (offset + info.frame_num as i32) - 1
                } else {
                    2 * (offset + info.frame_num as i32)
                };
                Ok(Poc {
                    value: poc,
                    frame_num_offset: offset,
                    msb: 0,
                    lsb: 0,
                })
            }
            other => Err(Error::corrupt(format!("pic_order_cnt_type {other}"))),
        }
    }

    /// `FrameNumOffset` (Equations 8-6 and 8-11).
    fn frame_num_offset(&self, info: &PicInfo, max_frame_num: i32) -> i32 {
        if info.is_idr {
            return 0;
        }
        let prev = if self.prev_has_mmco5 {
            0
        } else {
            self.prev_frame_num_offset
        };
        if self.prev_frame_num > info.frame_num {
            prev + max_frame_num
        } else {
            prev
        }
    }

    /// True when `frame_num` skips values, i.e. pictures are missing
    /// (clause 8.2.5.2). Editor seeks and lossy transports both produce this;
    /// it is a normal condition, not a corrupt stream.
    pub(crate) fn frame_num_gap(&self, sps: &Sps, frame_num: u32) -> bool {
        let max = 1u32 << sps.log2_max_frame_num;
        self.started
            && frame_num != self.prev_ref_frame_num
            && frame_num != (self.prev_ref_frame_num + 1) % max
    }

    /// Insert the "non-existing" frames of clause 8.2.5.2 for a frame_num gap,
    /// so that reference numbering stays aligned with the encoder's and the
    /// pictures that follow decode instead of being dropped.
    pub(crate) fn fill_frame_num_gap(&mut self, sps: &Sps, frame_num: u32) -> Result<()> {
        let max = 1u32 << sps.log2_max_frame_num;
        let mut unused = (self.prev_ref_frame_num + 1) % max;
        // A gap can be as wide as MaxFrameNum; every inferred frame costs a
        // sliding-window step, so bound the work at the buffer size.
        let mut budget = MAX_STORED;
        while unused != frame_num && budget > 0 {
            budget -= 1;
            let mut pic = self.take_picture();
            pic.start(sps);
            // The samples are unspecified; the last reference picture is a far
            // better guess than grey when a later picture wrongly predicts
            // from the gap.
            match self.frames.iter().max_by_key(|p| p.frame_num_wrap) {
                Some(src) if src.y.data.len() == pic.y.data.len() => pic.copy_samples_from(src),
                _ => pic.fill_grey(),
            }
            pic.non_existing = true;
            pic.frame_num = unused;
            pic.output = false;
            pic.complete = true;
            let info = PicInfo {
                is_idr: false,
                is_reference: true,
                frame_num: unused,
                pic_order_cnt_lsb: 0,
                delta_pic_order_cnt_bottom: 0,
                delta_pic_order_cnt: [0; 2],
            };
            // 8.2.5.2: picture order count is only derived for a non-existing
            // frame when pic_order_cnt_type is not 0. Under type 0 the frame
            // has no output order, and 8.2.4.2.3 keeps it out of the B lists.
            let poc = if sps.pic_order_cnt_type == 0 {
                Poc::default()
            } else {
                self.picture_order_count(sps, &info)?
            };
            pic.poc = poc.value;
            pic.frame_num_offset = poc.frame_num_offset;
            pic.mark = Mark::Short;
            self.number_short_term(unused, sps);
            self.sliding_window(sps)?;
            pic.frame_num_wrap = unused as i32;
            pic.pic_num = unused as i32;
            self.frames.push(pic);
            self.prev_frame_num = unused;
            self.prev_frame_num_offset = poc.frame_num_offset;
            self.prev_has_mmco5 = false;
            self.prev_ref_frame_num = unused;
            unused = (unused + 1) % max;
        }
        Ok(())
    }

    /// Recompute `FrameNumWrap` and `PicNum` for every stored reference
    /// picture against the current `frame_num` (clause 8.2.4.1).
    pub(crate) fn number_short_term(&mut self, frame_num: u32, sps: &Sps) {
        let max_frame_num = 1i32 << sps.log2_max_frame_num;
        for pic in &mut self.frames {
            match pic.mark {
                Mark::Short => {
                    pic.frame_num_wrap = if pic.frame_num > frame_num {
                        pic.frame_num as i32 - max_frame_num
                    } else {
                        pic.frame_num as i32
                    };
                    pic.pic_num = pic.frame_num_wrap;
                }
                Mark::Long => pic.pic_num = pic.long_term_frame_idx as i32,
                Mark::Unused => {}
            }
        }
    }

    /// Build `RefPicList0` and `RefPicList1` for one slice: initialisation
    /// (8.2.4.2.1 / 8.2.4.2.3) followed by modification (8.2.4.3).
    pub(crate) fn build_ref_lists(
        &self,
        slice_type: SliceType,
        curr_poc: i32,
        curr_pic_num: i32,
        max_pic_num: i32,
        num_l0: usize,
        num_l1: usize,
        mods: (&[RefPicListMod], &[RefPicListMod]),
    ) -> Result<[RefList; 2]> {
        let mut lists = [RefList::default(), RefList::default()];
        // Short-term candidates, then long-term, both by index into `frames`.
        let mut short: [usize; MAX_STORED] = [0; MAX_STORED];
        let mut n_short = 0usize;
        let mut long: [usize; MAX_STORED] = [0; MAX_STORED];
        let mut n_long = 0usize;
        for (i, pic) in self.frames.iter().enumerate() {
            // A frame inferred for a frame_num gap has no output order, so it
            // cannot take part in the POC ordering a B list is built from
            // (8.2.4.2.3); it stays a legal P reference by PicNum.
            if pic.non_existing && slice_type == SliceType::B {
                continue;
            }
            match pic.mark {
                Mark::Short if n_short < MAX_STORED => {
                    short[n_short] = i;
                    n_short += 1;
                }
                Mark::Long if n_long < MAX_STORED => {
                    long[n_long] = i;
                    n_long += 1;
                }
                _ => {}
            }
        }
        let long = &mut long[..n_long];
        long.sort_unstable_by_key(|&i| self.frames[i].long_term_frame_idx);

        if slice_type == SliceType::B {
            let short = &mut short[..n_short];
            // List 0: earlier pictures by descending POC, then later ascending.
            short.sort_unstable_by_key(|&i| {
                let poc = self.frames[i].poc;
                if poc < curr_poc {
                    (0i32, -poc)
                } else {
                    (1, poc)
                }
            });
            for &i in short.iter() {
                lists[0].push(i);
            }
            for &i in long.iter() {
                lists[0].push(i);
            }
            // List 1: later pictures by ascending POC, then earlier descending.
            short.sort_unstable_by_key(|&i| {
                let poc = self.frames[i].poc;
                if poc > curr_poc {
                    (0i32, poc)
                } else {
                    (1, -poc)
                }
            });
            for &i in short.iter() {
                lists[1].push(i);
            }
            for &i in long.iter() {
                lists[1].push(i);
            }
            // 8.2.4.2.3 step 3.
            if lists[1].len > 1 && lists[1].entries[..lists[1].len] == lists[0].entries[..lists[0].len]
            {
                lists[1].entries.swap(0, 1);
            }
        } else {
            let short = &mut short[..n_short];
            short.sort_unstable_by_key(|&i| -self.frames[i].pic_num);
            for &i in short.iter() {
                lists[0].push(i);
            }
            for &i in long.iter() {
                lists[0].push(i);
            }
        }

        let wanted = [num_l0, num_l1];
        let mods = [mods.0, mods.1];
        for x in 0..2 {
            if x == 1 && slice_type != SliceType::B {
                lists[1].len = 0;
                continue;
            }
            if lists[x].len == 0 {
                return Err(Error::corrupt("inter slice with an empty reference list"));
            }
            // 8.2.4.2: the initial list is truncated to the active count. A
            // list shorter than that leaves the tail unspecified, and a
            // conformant stream fills it by modification; repeating entry 0
            // keeps every index in range meanwhile.
            let first = lists[x].entries[0];
            while lists[x].len < wanted[x] {
                lists[x].push(usize::from(first));
            }
            lists[x].len = wanted[x];
            self.modify_list(&mut lists[x], mods[x], curr_pic_num, max_pic_num);
        }
        Ok(lists)
    }

    /// Reference picture list modification (clause 8.2.4.3), following the
    /// insert-and-compact of Equations 8-37 and 8-38 with the one-longer
    /// temporary list those equations use.
    ///
    /// Comparing DPB indices stands in for `PicNumF`/`LongTermPicNumF`: a
    /// picture appears in the buffer once, and the sentinel those functions
    /// return for a wrongly marked picture can never equal the target.
    fn modify_list(
        &self,
        list: &mut RefList,
        mods: &[RefPicListMod],
        curr_pic_num: i32,
        max_pic_num: i32,
    ) {
        if mods.is_empty() {
            return;
        }
        let active = list.len;
        let mut tmp = [0u8; 34];
        tmp[..active].copy_from_slice(&list.entries[..active]);
        let mut pred = curr_pic_num;
        let mut ref_idx = 0usize;
        for op in mods {
            let target = match *op {
                RefPicListMod::ShortTerm {
                    abs_diff_pic_num_minus1,
                    add,
                } => {
                    let diff = abs_diff_pic_num_minus1 as i32 + 1;
                    let no_wrap = if add {
                        if pred + diff >= max_pic_num {
                            pred + diff - max_pic_num
                        } else {
                            pred + diff
                        }
                    } else if pred - diff < 0 {
                        pred - diff + max_pic_num
                    } else {
                        pred - diff
                    };
                    pred = no_wrap;
                    let pic_num = if no_wrap > curr_pic_num {
                        no_wrap - max_pic_num
                    } else {
                        no_wrap
                    };
                    self.frames
                        .iter()
                        .position(|p| p.mark == Mark::Short && p.pic_num == pic_num)
                }
                RefPicListMod::LongTerm(num) => self
                    .frames
                    .iter()
                    .position(|p| p.mark == Mark::Long && p.pic_num == num as i32),
            };
            // A modification naming a picture the buffer never received points
            // at something that was lost; keeping the initial entry decodes a
            // seek or a dropped packet instead of failing the slice.
            let Some(target) = target else { continue };
            if ref_idx >= active {
                break;
            }
            for c in (ref_idx + 1..=active).rev() {
                tmp[c] = tmp[c - 1];
            }
            tmp[ref_idx] = target as u8;
            ref_idx += 1;
            let mut n = ref_idx;
            for c in ref_idx..=active {
                if usize::from(tmp[c]) != target {
                    tmp[n] = tmp[c];
                    n += 1;
                }
            }
        }
        list.entries[..active].copy_from_slice(&tmp[..active]);
        list.len = active;
    }

    /// Store a decoded picture and apply the reference marking of 8.2.5.
    pub(crate) fn store(
        &mut self,
        mut pic: Picture,
        sps: &Sps,
        info: &PicInfo,
        marking: Option<&DecRefPicMarking>,
    ) -> Result<()> {
        let has_mmco5 = marking.is_some_and(|m| m.mmcos.iter().any(|c| c.op == 5));
        pic.output = true;
        pic.complete = true;
        // Clause 8.2.4.1 against this picture's frame_num, which is what both
        // the sliding window (8.2.5.3) and the MMCO picture numbers (8.2.5.4)
        // compare. An intra slice never built a reference list, so this is the
        // only place the numbering is guaranteed to be current — and the
        // picture being stored has no FrameNumWrap of its own until now.
        self.number_short_term(info.frame_num, sps);
        pic.frame_num_wrap = pic.frame_num as i32;
        pic.pic_num = pic.frame_num as i32;

        if info.is_idr {
            let no_output = marking.is_some_and(|m| m.no_output_of_prior_pics);
            self.drain_prior(no_output);
            pic.mark = Mark::Short;
            if marking.is_some_and(|m| m.long_term_reference) {
                pic.mark = Mark::Long;
                pic.long_term_frame_idx = 0;
                self.max_long_term_idx = Some(0);
            } else {
                self.max_long_term_idx = None;
            }
        } else if info.is_reference {
            let m = marking.expect("a reference picture carries dec_ref_pic_marking");
            pic.mark = Mark::Short;
            if m.adaptive {
                self.apply_mmco(&mut pic, m, info)?;
            } else {
                self.sliding_window(sps)?;
            }
        }

        if has_mmco5 {
            // 8.2.1: an MMCO 5 restarts frame_num and picture order count at
            // this picture, so everything before it must leave first.
            pic.poc = 0;
            pic.frame_num = 0;
            pic.frame_num_offset = 0;
            pic.poc_msb = 0;
            pic.poc_lsb = 0;
        }

        self.prev_frame_num = pic.frame_num;
        self.prev_frame_num_offset = pic.frame_num_offset;
        self.prev_has_mmco5 = has_mmco5;
        if info.is_reference {
            self.prev_ref_frame_num = pic.frame_num;
            self.prev_poc_msb = pic.poc_msb;
            self.prev_poc_lsb = pic.poc_lsb;
        }
        self.started = true;
        // An IDR or an MMCO 5 restarts picture order count, so POC no longer
        // orders across the boundary: everything already in the buffer belongs
        // ahead of this picture and has to leave first.
        if (info.is_idr || has_mmco5) && self.frames.iter().any(|p| p.output) {
            self.pending_flush = true;
        }
        self.last_id = pic.id;

        if self.frames.len() >= MAX_STORED {
            // Every stored picture is either a reference or waiting to be
            // output, and both limits were enforced above; a stream that still
            // overflows is not conformant.
            return Err(Error::corrupt("decoded picture buffer overflow"));
        }
        self.frames.push(pic);
        Ok(())
    }

    /// Output and remove every stored picture (IDR boundary or MMCO 5).
    fn drain_prior(&mut self, discard: bool) {
        for pic in &mut self.frames {
            pic.mark = Mark::Unused;
            if discard {
                pic.output = false;
            }
        }
    }

    /// Sliding window marking (clause 8.2.5.3).
    fn sliding_window(&mut self, sps: &Sps) -> Result<()> {
        let max_refs = (sps.max_num_ref_frames as usize).max(1);
        loop {
            let n = self
                .frames
                .iter()
                .filter(|p| p.mark != Mark::Unused)
                .count();
            if n < max_refs {
                return Ok(());
            }
            let Some(oldest) = self
                .frames
                .iter_mut()
                .filter(|p| p.mark == Mark::Short)
                .min_by_key(|p| p.frame_num_wrap)
            else {
                // Only long-term references left and the window is full: the
                // stream must free one by MMCO, so refusing here would break a
                // conformant stream. Leave the buffer as is.
                return Ok(());
            };
            oldest.mark = Mark::Unused;
        }
    }

    /// Adaptive marking (clause 8.2.5.4).
    fn apply_mmco(&mut self, pic: &mut Picture, m: &DecRefPicMarking, info: &PicInfo) -> Result<()> {
        let curr_pic_num = info.frame_num as i32;
        for op in &m.mmcos {
            match op.op {
                1 => {
                    let target = curr_pic_num - (op.arg1 as i32 + 1);
                    if let Some(p) = self
                        .frames
                        .iter_mut()
                        .find(|p| p.mark == Mark::Short && p.pic_num == target)
                    {
                        p.mark = Mark::Unused;
                    }
                }
                2 => {
                    if let Some(p) = self
                        .frames
                        .iter_mut()
                        .find(|p| p.mark == Mark::Long && p.pic_num == op.arg1 as i32)
                    {
                        p.mark = Mark::Unused;
                    }
                }
                3 => {
                    let target = curr_pic_num - (op.arg1 as i32 + 1);
                    let idx = op.arg2;
                    for p in &mut self.frames {
                        if p.mark == Mark::Long && p.long_term_frame_idx == idx {
                            p.mark = Mark::Unused;
                        }
                    }
                    if let Some(p) = self
                        .frames
                        .iter_mut()
                        .find(|p| p.mark == Mark::Short && p.pic_num == target)
                    {
                        p.mark = Mark::Long;
                        p.long_term_frame_idx = idx;
                        p.pic_num = idx as i32;
                    }
                }
                4 => {
                    self.max_long_term_idx = op.arg1.checked_sub(1);
                    let limit = self.max_long_term_idx;
                    for p in &mut self.frames {
                        if p.mark == Mark::Long
                            && limit.is_none_or(|max| p.long_term_frame_idx > max)
                        {
                            p.mark = Mark::Unused;
                        }
                    }
                }
                5 => {
                    for p in &mut self.frames {
                        p.mark = Mark::Unused;
                    }
                    self.max_long_term_idx = None;
                }
                6 => {
                    let idx = op.arg1;
                    for p in &mut self.frames {
                        if p.mark == Mark::Long && p.long_term_frame_idx == idx {
                            p.mark = Mark::Unused;
                        }
                    }
                    pic.mark = Mark::Long;
                    pic.long_term_frame_idx = idx;
                    pic.pic_num = idx as i32;
                }
                other => {
                    return Err(Error::corrupt(format!(
                        "memory_management_control_operation {other}"
                    )));
                }
            }
        }
        Ok(())
    }

    /// The next picture to output, or `None` while the buffer is still within
    /// its reordering and storage limits (the bumping process of C.4.5.3).
    ///
    /// `flush` empties the buffer, which is what end of stream and a seek do.
    pub(crate) fn next_output(&mut self, flush: bool) -> Option<usize> {
        if self.pending_flush {
            let last = self.last_id;
            let best = self
                .frames
                .iter()
                .enumerate()
                .filter(|(_, p)| p.output && p.id != last)
                .min_by_key(|(_, p)| p.poc)
                .map(|(i, _)| i);
            if best.is_some() {
                return best;
            }
            self.pending_flush = false;
        }
        let pending = self.frames.iter().filter(|p| p.output).count();
        let stored = self.frames.iter().filter(|p| p.stored()).count();
        if !flush && pending <= self.max_reorder && stored <= self.max_frames {
            return None;
        }
        let mut best: Option<usize> = None;
        for (i, pic) in self.frames.iter().enumerate() {
            if !pic.output {
                continue;
            }
            if best.is_none_or(|b| pic.poc < self.frames[b].poc) {
                best = Some(i);
            }
        }
        best
    }

    /// Mark picture `idx` as output and reclaim every slot that is now free.
    pub(crate) fn released(&mut self, idx: usize) {
        self.frames[idx].output = false;
        let mut i = 0;
        while i < self.frames.len() {
            if self.frames[i].stored() {
                i += 1;
                continue;
            }
            let pic = self.frames.swap_remove(i);
            self.recycle(pic);
        }
    }
}

/// `MaxDpbFrames` from the level limits of Table A-1, the buffer size a stream
/// is allowed to assume when its VUI does not state one.
fn max_dpb_frames(sps: &Sps) -> usize {
    let max_dpb_mbs = match sps.level_idc {
        0..=10 => 396,
        11 => {
            // constraint_set3_flag distinguishes level 1b from level 1.1; both
            // have the same MaxDpbMbs, so the flag does not change the answer.
            396
        }
        12..=20 => 2376,
        21 => 4752,
        22..=30 => 8100,
        31 => 18000,
        32 => 20480,
        33..=41 => 32768,
        42 => 34816,
        50 => 110400,
        51..=52 => 184320,
        _ => 696320,
    };
    let per_frame = (sps.mb_width as usize * sps.mb_height as usize).max(1);
    (max_dpb_mbs / per_frame).clamp(1, 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane(w: usize, h: usize) -> Plane8 {
        let mut p = Plane8::default();
        p.resize(w, h, 4);
        for y in 0..h {
            for x in 0..w {
                let at = p.at(x, y);
                p.data[at] = (x + y * 10) as u8;
            }
        }
        p
    }

    /// Border replication makes an out-of-picture read equal the clipped
    /// in-picture read, which is what [`crate::inter`] relies on.
    #[test]
    fn extend_borders_replicates_edges() {
        let mut p = plane(6, 5);
        p.extend_borders();
        let at = |x: i32, y: i32| -> u8 {
            p.data[(p.origin as i32 + y * p.stride as i32 + x) as usize]
        };
        for y in -4..9i32 {
            for x in -4..10i32 {
                let cx = x.clamp(0, 5) as usize;
                let cy = y.clamp(0, 4) as usize;
                assert_eq!(at(x, y), p.data[p.at(cx, cy)], "({x}, {y})");
            }
        }
    }

    fn sps_for(poc_type: u8) -> Sps {
        use ec_core::BitWriter;
        let mut w = BitWriter::new();
        w.write_bits(66, 8);
        w.write_bits(0, 8);
        w.write_bits(30, 8);
        w.write_ue(0); // sps id
        w.write_ue(4); // log2_max_frame_num_minus4 -> 8
        w.write_ue(u32::from(poc_type));
        match poc_type {
            0 => w.write_ue(4), // log2_max_pic_order_cnt_lsb_minus4 -> 8
            1 => {
                w.write_bit(false); // delta_pic_order_always_zero
                w.write_se(0);
                w.write_se(0);
                w.write_ue(1); // one offset_for_ref_frame
                w.write_se(2);
            }
            _ => {}
        }
        w.write_ue(2); // max_num_ref_frames
        w.write_bit(false);
        w.write_ue(10);
        w.write_ue(8);
        w.write_bit(true); // frame_mbs_only
        w.write_bit(true); // direct_8x8_inference
        w.write_bit(false); // no cropping
        w.write_bit(false); // no vui
        w.align_to_byte();
        Sps::parse(w.as_bytes()).unwrap()
    }

    fn info(frame_num: u32, lsb: u32, is_ref: bool, is_idr: bool) -> PicInfo {
        PicInfo {
            is_idr,
            is_reference: is_ref,
            frame_num,
            pic_order_cnt_lsb: lsb,
            delta_pic_order_cnt_bottom: 0,
            delta_pic_order_cnt: [0; 2],
        }
    }

    /// Picture order count type 0 wraps when pic_order_cnt_lsb rolls over.
    #[test]
    fn poc_type_0_tracks_the_msb_across_a_wrap() {
        let sps = sps_for(0);
        let mut dpb = Dpb::default();
        let mut expect = |i: PicInfo, want: i32| {
            let poc = dpb.picture_order_count(&sps, &i).unwrap();
            assert_eq!(poc.value, want, "{i:?}");
            // Emulate the reference-picture bookkeeping of `store`.
            if i.is_reference {
                dpb.prev_poc_msb = poc.msb;
                dpb.prev_poc_lsb = poc.lsb;
            }
        };
        expect(info(0, 0, true, true), 0);
        expect(info(1, 100, true, false), 100);
        expect(info(2, 200, true, false), 200);
        // 200 -> 40 is more than half of MaxPicOrderCntLsb backwards, so it is
        // read as a wrap forward rather than a jump back.
        expect(info(3, 40, true, false), 296);
        // A genuine step back stays a step back.
        expect(info(4, 100, true, false), 356);
    }

    /// Type 2 counts pictures in decode order, so a non-reference picture sits
    /// just before the reference picture that follows it.
    #[test]
    fn poc_type_2_is_twice_the_frame_number() {
        let sps = sps_for(2);
        let dpb = Dpb::default();
        assert_eq!(dpb.picture_order_count(&sps, &info(0, 0, true, true)).unwrap().value, 0);
        assert_eq!(dpb.picture_order_count(&sps, &info(3, 0, true, false)).unwrap().value, 6);
        assert_eq!(dpb.picture_order_count(&sps, &info(3, 0, false, false)).unwrap().value, 5);
    }

    /// Type 1 walks the offset_for_ref_frame cycle.
    #[test]
    fn poc_type_1_follows_the_offset_cycle() {
        let sps = sps_for(1);
        let dpb = Dpb::default();
        // One-entry cycle of +2: frame n has expectedPicOrderCnt 2n.
        assert_eq!(dpb.picture_order_count(&sps, &info(0, 0, true, true)).unwrap().value, 0);
        assert_eq!(dpb.picture_order_count(&sps, &info(1, 0, true, false)).unwrap().value, 2);
        assert_eq!(dpb.picture_order_count(&sps, &info(4, 0, true, false)).unwrap().value, 8);
    }

    fn push_ref(dpb: &mut Dpb, frame_num: u32, poc: i32, mark: Mark) {
        let mut pic = dpb.take_picture();
        pic.frame_num = frame_num;
        pic.frame_num_wrap = frame_num as i32;
        pic.pic_num = frame_num as i32;
        pic.poc = poc;
        pic.mark = mark;
        dpb.frames.push(pic);
    }

    /// P list 0 is short-term by descending PicNum then long-term ascending.
    #[test]
    fn p_list_initialisation_orders_by_pic_num() {
        let mut dpb = Dpb::default();
        push_ref(&mut dpb, 2, 4, Mark::Short);
        push_ref(&mut dpb, 5, 10, Mark::Short);
        push_ref(&mut dpb, 3, 6, Mark::Short);
        let mut lt = dpb.take_picture();
        lt.mark = Mark::Long;
        lt.long_term_frame_idx = 1;
        lt.pic_num = 1;
        dpb.frames.push(lt);
        let lists = dpb
            .build_ref_lists(SliceType::P, 12, 6, 256, 4, 0, (&[], &[]))
            .unwrap();
        let nums: Vec<i32> = (0..lists[0].len())
            .map(|i| dpb.frames[lists[0].get(i).unwrap()].pic_num)
            .collect();
        assert_eq!(nums, vec![5, 3, 2, 1]);
    }

    /// B lists split around the current POC and mirror each other.
    #[test]
    fn b_list_initialisation_splits_around_the_current_poc() {
        let mut dpb = Dpb::default();
        push_ref(&mut dpb, 0, 0, Mark::Short);
        push_ref(&mut dpb, 1, 8, Mark::Short);
        push_ref(&mut dpb, 2, 4, Mark::Short);
        push_ref(&mut dpb, 3, 12, Mark::Short);
        let lists = dpb
            .build_ref_lists(SliceType::B, 6, 4, 256, 4, 4, (&[], &[]))
            .unwrap();
        let poc = |l: &RefList| -> Vec<i32> {
            (0..l.len()).map(|i| dpb.frames[l.get(i).unwrap()].poc).collect()
        };
        assert_eq!(poc(&lists[0]), vec![4, 0, 8, 12]);
        assert_eq!(poc(&lists[1]), vec![8, 12, 4, 0]);
    }

    /// A single-entry list identical in both directions gets its first two
    /// entries swapped (8.2.4.2.3 step 3 only fires above one entry).
    #[test]
    fn identical_b_lists_swap_the_first_two_entries() {
        let mut dpb = Dpb::default();
        push_ref(&mut dpb, 0, 0, Mark::Short);
        push_ref(&mut dpb, 1, 2, Mark::Short);
        // Both references precede the current picture, so initialisation gives
        // list 0 and list 1 the same descending-POC order.
        let lists = dpb
            .build_ref_lists(SliceType::B, 10, 2, 256, 2, 2, (&[], &[]))
            .unwrap();
        let l0: Vec<i32> = (0..2).map(|i| dpb.frames[lists[0].get(i).unwrap()].poc).collect();
        let l1: Vec<i32> = (0..2).map(|i| dpb.frames[lists[1].get(i).unwrap()].poc).collect();
        assert_eq!(l0, vec![2, 0]);
        assert_eq!(l1, vec![0, 2], "identical lists swap");
    }

    /// A short-term modification moves the named picture to the front.
    #[test]
    fn short_term_modification_reorders_the_list() {
        let mut dpb = Dpb::default();
        push_ref(&mut dpb, 5, 10, Mark::Short);
        push_ref(&mut dpb, 4, 8, Mark::Short);
        push_ref(&mut dpb, 3, 6, Mark::Short);
        // CurrPicNum 6, abs_diff 3 -> picNum 3, which is the last entry.
        let mods = [RefPicListMod::ShortTerm {
            abs_diff_pic_num_minus1: 2,
            add: false,
        }];
        let lists = dpb
            .build_ref_lists(SliceType::P, 12, 6, 256, 3, 0, (&mods, &[]))
            .unwrap();
        let nums: Vec<i32> = (0..3)
            .map(|i| dpb.frames[lists[0].get(i).unwrap()].pic_num)
            .collect();
        assert_eq!(nums, vec![3, 5, 4]);
    }
}

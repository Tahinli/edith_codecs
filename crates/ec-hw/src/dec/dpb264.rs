//! The H.264 decoded picture buffer, clause 8.2, over surfaces.
//!
//! Same clauses as `ec-h264`'s software DPB — picture order count (8.2.1),
//! reference numbering (8.2.4.1), list initialisation and modification
//! (8.2.4.2/8.2.4.3), marking (8.2.5) and bumping (C.4.5.3) — but a stored
//! picture here is a *surface*, not a plane of samples. A stateless driver is
//! told the whole buffer on every picture, so these lists are the parameter
//! buffer's contents rather than an internal detail.

use std::sync::Arc;

use ec_h264_syntax::{DecRefPicMarking, RefPicListMod, SliceType, Sps};

use crate::error::{Error, Result};
use crate::pool::PooledSurface;

/// Maximum pictures held at once (Annex A caps the DPB at 16 frames; the extra
/// slot is the picture being decoded).
pub(crate) const MAX_STORED: usize = 17;

/// How a stored picture is marked for reference (8.2.5.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    /// Not used for reference.
    Unused,
    /// Used for short-term reference.
    Short,
    /// Used for long-term reference.
    Long,
}

/// One picture in the buffer.
pub(crate) struct Picture {
    /// Identity, so "the picture just stored" survives reordering.
    pub id: i32,
    /// The surface it decoded into.
    pub surface: Arc<PooledSurface>,
    /// Presentation timestamp of the access unit it came from.
    pub timestamp: i64,
    /// `frame_num`.
    pub frame_num: u32,
    /// `FrameNumWrap` (8.2.4.1).
    pub frame_num_wrap: i32,
    /// `PicNum` for short-term, `LongTermPicNum` for long-term pictures.
    pub pic_num: i32,
    /// `LongTermFrameIdx`.
    pub long_term_frame_idx: u32,
    /// `PicOrderCnt()`.
    pub poc: i32,
    /// `PicOrderCntMsb`, predicted from by the next picture.
    pub poc_msb: i32,
    /// `pic_order_cnt_lsb`.
    pub poc_lsb: i32,
    /// `FrameNumOffset`.
    pub frame_num_offset: i32,
    /// Reference marking.
    pub mark: Mark,
    /// Inferred for a `frame_num` gap (8.2.5.2): a legal P reference with
    /// unspecified samples, never output.
    pub non_existing: bool,
    /// Still waiting to be handed to the caller.
    pub output: bool,
}

impl Picture {
    fn stored(&self) -> bool {
        self.output || self.mark != Mark::Unused
    }
}

/// One reference picture list (`RefPicList0` / `RefPicList1`), as DPB indices.
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

/// Output of the picture order count derivation (8.2.1).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Poc {
    pub value: i32,
    pub frame_num_offset: i32,
    pub msb: i32,
    pub lsb: i32,
}

/// What the DPB needs to know about the picture being decoded, taken from its
/// first slice header.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PicInfo {
    pub is_idr: bool,
    pub is_reference: bool,
    pub frame_num: u32,
    pub pic_order_cnt_lsb: u32,
    pub delta_pic_order_cnt_bottom: i32,
    pub delta_pic_order_cnt: [i32; 2],
}

/// The decoded picture buffer and the cross-picture state of clause 8.2.
pub(crate) struct Dpb {
    pub frames: Vec<Picture>,
    next_id: i32,
    max_frames: usize,
    max_reorder: usize,
    max_long_term_idx: Option<u32>,
    prev_poc_msb: i32,
    prev_poc_lsb: i32,
    prev_frame_num: u32,
    prev_frame_num_offset: i32,
    prev_has_mmco5: bool,
    prev_ref_frame_num: u32,
    started: bool,
    pending_flush: bool,
    last_id: i32,
}

impl Default for Dpb {
    fn default() -> Dpb {
        Dpb {
            frames: Vec::new(),
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

impl Dpb {
    /// Adopt the buffer limits of `sps` (Annex A / VUI).
    pub(crate) fn configure(&mut self, sps: &Sps) {
        let level_frames = max_dpb_frames(sps);
        let (buffering, reorder) = match sps.vui.as_ref().and_then(|v| v.bitstream_restriction) {
            Some(r) => (
                (r.max_dec_frame_buffering as usize).min(MAX_STORED - 1),
                (r.max_num_reorder_frames as usize).min(MAX_STORED - 1),
            ),
            None => (level_frames, level_frames),
        };
        self.max_frames = buffering.max(sps.max_num_ref_frames as usize).max(1);
        self.max_reorder = reorder.min(self.max_frames);
    }

    /// How many frames the level allows, for sizing the surface pool.
    pub(crate) fn capacity(&self) -> usize {
        self.max_frames
    }

    pub(crate) fn next_id(&mut self) -> i32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    /// Drop every stored picture (seek or stream discontinuity).
    pub(crate) fn clear(&mut self) {
        self.frames.clear();
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

    /// `PicOrderCnt()` of the picture about to be decoded (8.2.1).
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
            other => Err(Error::Stream(ec_core::Error::corrupt(format!(
                "H.264 pic_order_cnt_type {other}"
            )))),
        }
    }

    /// `FrameNumOffset` (8-6, 8-11).
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

    /// True when `frame_num` skips values (8.2.5.2).
    ///
    /// This is a *normal* condition — an editor seek or a lossy transport
    /// produces it — and treating it as corruption is what used to knock a
    /// hardware session back to software mid-timeline.
    pub(crate) fn frame_num_gap(&self, sps: &Sps, frame_num: u32) -> bool {
        let max = 1u32 << sps.log2_max_frame_num;
        self.started
            && frame_num != self.prev_ref_frame_num
            && frame_num != (self.prev_ref_frame_num + 1) % max
    }

    /// Insert the "non-existing" frames of 8.2.5.2 so that reference numbering
    /// stays aligned with the encoder's.
    ///
    /// The inferred frames share the newest reference picture's surface. Their
    /// samples are unspecified by the spec, and the previous reference is both
    /// a better guess than grey and the only choice this driver accepts: a
    /// reference surface it has never decoded into makes `vaEndPicture` fail
    /// with `operation failed` (measured on radeonsi with a two-picture gap).
    pub(crate) fn fill_frame_num_gap(&mut self, sps: &Sps, frame_num: u32) -> Result<usize> {
        let max = 1u32 << sps.log2_max_frame_num;
        let mut unused = (self.prev_ref_frame_num + 1) % max;
        let mut budget = MAX_STORED;
        let mut filled = 0;
        while unused != frame_num && budget > 0 {
            budget -= 1;
            let Some(surface) = self
                .frames
                .iter()
                .max_by_key(|p| p.frame_num_wrap)
                .map(|p| Arc::clone(&p.surface))
            else {
                // Nothing decoded yet: there is no picture to stand in for the
                // missing ones, and the next IDR will resynchronise anyway.
                break;
            };
            let info = PicInfo {
                is_idr: false,
                is_reference: true,
                frame_num: unused,
                pic_order_cnt_lsb: 0,
                delta_pic_order_cnt_bottom: 0,
                delta_pic_order_cnt: [0; 2],
            };
            // 8.2.5.2: a non-existing frame gets a picture order count only
            // when pic_order_cnt_type is not 0; under type 0 it has no output
            // order, and 8.2.4.2.3 keeps it out of the B lists.
            let poc = if sps.pic_order_cnt_type == 0 {
                Poc::default()
            } else {
                self.picture_order_count(sps, &info)?
            };
            self.number_short_term(unused, sps);
            self.sliding_window(sps);
            let id = self.next_id();
            self.frames.push(Picture {
                id,
                surface,
                timestamp: 0,
                frame_num: unused,
                frame_num_wrap: unused as i32,
                pic_num: unused as i32,
                long_term_frame_idx: 0,
                poc: poc.value,
                poc_msb: poc.msb,
                poc_lsb: poc.lsb,
                frame_num_offset: poc.frame_num_offset,
                mark: Mark::Short,
                non_existing: true,
                output: false,
            });
            self.prev_frame_num = unused;
            self.prev_frame_num_offset = poc.frame_num_offset;
            self.prev_has_mmco5 = false;
            self.prev_ref_frame_num = unused;
            unused = (unused + 1) % max;
            filled += 1;
        }
        Ok(filled)
    }

    /// Recompute `FrameNumWrap` and `PicNum` against the current `frame_num`
    /// (8.2.4.1).
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

    /// Build `RefPicList0`/`RefPicList1` for one slice: initialisation
    /// (8.2.4.2.1 / 8.2.4.2.3) then modification (8.2.4.3).
    #[allow(clippy::too_many_arguments)]
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
        let mut short: [usize; MAX_STORED] = [0; MAX_STORED];
        let mut n_short = 0usize;
        let mut long: [usize; MAX_STORED] = [0; MAX_STORED];
        let mut n_long = 0usize;
        for (i, pic) in self.frames.iter().enumerate() {
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
            if lists[1].len > 1
                && lists[1].entries[..lists[1].len] == lists[0].entries[..lists[0].len]
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
                return Err(Error::Stream(ec_core::Error::corrupt(
                    "H.264 inter slice with an empty reference list",
                )));
            }
            let first = lists[x].entries[0];
            while lists[x].len < wanted[x] {
                lists[x].push(usize::from(first));
            }
            lists[x].len = wanted[x];
            self.modify_list(&mut lists[x], mods[x], curr_pic_num, max_pic_num);
        }
        Ok(lists)
    }

    /// Reference picture list modification (8.2.4.3), by the insert-and-compact
    /// of equations 8-37 and 8-38.
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
            pic.mark = Mark::Short;
            match marking {
                Some(m) if m.adaptive => self.apply_mmco(&mut pic, m, info)?,
                _ => self.sliding_window(sps),
            }
        }

        if has_mmco5 {
            // 8.2.1: MMCO 5 restarts frame_num and picture order count here.
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
        if (info.is_idr || has_mmco5) && self.frames.iter().any(|p| p.output) {
            self.pending_flush = true;
        }
        self.last_id = pic.id;

        if self.frames.len() >= MAX_STORED {
            return Err(Error::Stream(ec_core::Error::corrupt(
                "H.264 decoded picture buffer overflow",
            )));
        }
        self.frames.push(pic);
        Ok(())
    }

    /// Output and release every stored picture (IDR boundary or MMCO 5).
    fn drain_prior(&mut self, discard: bool) {
        for pic in &mut self.frames {
            pic.mark = Mark::Unused;
            if discard {
                pic.output = false;
            }
        }
    }

    /// Sliding window marking (8.2.5.3).
    fn sliding_window(&mut self, sps: &Sps) {
        let max_refs = (sps.max_num_ref_frames as usize).max(1);
        loop {
            let n = self
                .frames
                .iter()
                .filter(|p| p.mark != Mark::Unused)
                .count();
            if n < max_refs {
                return;
            }
            let Some(oldest) = self
                .frames
                .iter_mut()
                .filter(|p| p.mark == Mark::Short)
                .min_by_key(|p| p.frame_num_wrap)
            else {
                // Only long-term references left: the stream must free one by
                // MMCO, and refusing here would break a conformant stream.
                return;
            };
            oldest.mark = Mark::Unused;
        }
    }

    /// Adaptive marking (8.2.5.4).
    fn apply_mmco(
        &mut self,
        pic: &mut Picture,
        m: &DecRefPicMarking,
        info: &PicInfo,
    ) -> Result<()> {
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
                    return Err(Error::Stream(ec_core::Error::corrupt(format!(
                        "H.264 memory_management_control_operation {other}"
                    ))));
                }
            }
        }
        Ok(())
    }

    /// The next picture to output, or `None` while the buffer is still within
    /// its reordering and storage limits (bumping, C.4.5.3).
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
            if !pic.output || pic.non_existing {
                continue;
            }
            if best.is_none_or(|b| pic.poc < self.frames[b].poc) {
                best = Some(i);
            }
        }
        best
    }

    /// Mark picture `idx` as output and drop every slot that is now free.
    ///
    /// Dropping is what releases the surface back to the pool: nothing else
    /// holds it once the buffer and the caller are done, which is the leak this
    /// design makes structurally impossible.
    pub(crate) fn released(&mut self, idx: usize) {
        self.frames[idx].output = false;
        self.frames.retain(|p| p.stored());
    }
}

/// `MaxDpbFrames` from the level limits of Annex A, Table A-1.
fn max_dpb_frames(sps: &Sps) -> usize {
    // MaxDpbMbs per level, in the level_idc order of Table A-1.
    const LIMITS: [(u8, u32); 17] = [
        (10, 396),
        (11, 900),
        (12, 2376),
        (13, 2376),
        (20, 2376),
        (21, 4752),
        (22, 8100),
        (30, 8100),
        (31, 18000),
        (32, 20480),
        (40, 32768),
        (41, 32768),
        (42, 34816),
        (50, 110400),
        (51, 184320),
        (52, 184320),
        (60, 696320),
    ];
    let mbs = sps.mbs_per_picture().max(1);
    let max_dpb_mbs = LIMITS
        .iter()
        .find(|(level, _)| *level >= sps.level_idc)
        .map(|(_, mbs)| *mbs)
        .unwrap_or(696_320);
    ((max_dpb_mbs / mbs) as usize).clamp(1, 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sps_for(poc_type: u8) -> Sps {
        let mut sps = Sps {
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
            pic_order_cnt_type: poc_type,
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
            height: 1088,
        };
        if poc_type == 1 {
            sps.offsets_for_ref_frame = vec![2, -2];
        }
        sps
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

    #[test]
    fn poc_type_0_tracks_the_msb_across_a_wrap() {
        let sps = sps_for(0);
        let mut dpb = Dpb::default();
        let poc = dpb
            .picture_order_count(&sps, &info(0, 0, true, true))
            .unwrap();
        assert_eq!(poc.value, 0);
        dpb.prev_poc_msb = poc.msb;
        dpb.prev_poc_lsb = poc.lsb;

        let poc = dpb
            .picture_order_count(&sps, &info(1, 14, true, false))
            .unwrap();
        assert_eq!(poc.value, 14);
        dpb.prev_poc_msb = poc.msb;
        dpb.prev_poc_lsb = poc.lsb;

        // lsb wraps 14 -> 2 with MaxPicOrderCntLsb 16: the msb must step up.
        let poc = dpb
            .picture_order_count(&sps, &info(2, 2, true, false))
            .unwrap();
        assert_eq!(poc.value, 18);
    }

    #[test]
    fn frame_num_gaps_are_detected_but_not_at_the_start() {
        let sps = sps_for(0);
        let mut dpb = Dpb::default();
        // Nothing decoded yet: a jump is a seek, not a gap.
        assert!(!dpb.frame_num_gap(&sps, 7));
        dpb.started = true;
        dpb.prev_ref_frame_num = 3;
        assert!(!dpb.frame_num_gap(&sps, 3));
        assert!(!dpb.frame_num_gap(&sps, 4));
        assert!(dpb.frame_num_gap(&sps, 6));
        // MaxFrameNum is 16, so 15 -> 0 is contiguous, 15 -> 1 is a gap.
        dpb.prev_ref_frame_num = 15;
        assert!(!dpb.frame_num_gap(&sps, 0));
        assert!(dpb.frame_num_gap(&sps, 1));
    }

    #[test]
    fn level_limits_size_the_buffer() {
        let mut sps = sps_for(0);
        let mut dpb = Dpb::default();
        dpb.configure(&sps);
        // 1080p at level 4.0: 32768 / 8160 = 4 frames.
        assert_eq!(dpb.capacity(), 4);
        sps.level_idc = 51;
        dpb.configure(&sps);
        assert_eq!(dpb.capacity(), 16);
    }
}

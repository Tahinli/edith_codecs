//! Storage, reference marking and reference list construction for streams
//! with `frame_mbs_only_flag` 0 (clauses 8.2.4 and 8.2.5 with field pictures).
//!
//! A stored frame (a [`Picture`] with `structure == FRAME`) is a frame store:
//! the woven frame planes, plus two field views in [`Picture::fields`]. A frame
//! coded as two field pictures keeps the decoded half-height fields as those
//! views (with their motion data) and has its frame planes woven from them
//! when the second field arrives; a frame coded as a frame has its field views
//! split out of the frame planes. Either way a field reference reads the
//! padded planes of one field and a frame reference reads the woven frame.
//!
//! Marking is kept per field ([`Picture::fmark`]); `Picture::mark` is the
//! aggregate the buffer's slot accounting reads. The progressive paths of the
//! parent module are not touched: a stream with `frame_mbs_only_flag` 1 never
//! comes through here.

use super::*;

/// `PicNum` of field `parity` (0 top, 1 bottom) of a short-term entry whose
/// `FrameNumWrap` is `wrap`, seen from a picture of `structure` (8-28, 8-30,
/// 8-31).
fn short_pic_num(wrap: i32, parity: usize, structure: u8) -> i32 {
    if structure == FRAME {
        wrap
    } else if parity + 1 == usize::from(structure) {
        2 * wrap + 1
    } else {
        2 * wrap
    }
}

/// `LongTermPicNum` of a long-term entry (8-29, 8-32, 8-33).
fn long_pic_num(idx: u32, parity: usize, structure: u8) -> i32 {
    let idx = idx as i32;
    if structure == FRAME {
        idx
    } else if parity + 1 == usize::from(structure) {
        2 * idx + 1
    } else {
        2 * idx
    }
}

impl<S: Sample> Picture<S> {
    /// Size the three planes for a field of an `mb_w` by `mb_h_field`
    /// macroblock picture, leaving the per-macroblock arrays alone.
    fn size_field_planes(&mut self, bd: u32, cat: u8, mb_w: usize, mb_h_field: usize, sps_id: u8) {
        let (cw, cmb_h, cpad) = chroma_geometry(cat);
        self.bd = bd;
        self.cmb_h = cmb_h;
        self.cw = cw;
        self.cat = cat;
        self.mb_w = mb_w;
        self.mb_h = mb_h_field;
        self.sps_id = sps_id;
        if self.y.width != mb_w * 16
            || self.y.height != mb_h_field * 16
            || self.cb.height != mb_h_field * cmb_h
            || self.cb.width != mb_w * cw
        {
            self.y.resize(mb_w * 16, mb_h_field * 16, PAD_Y);
            self.cb.resize(mb_w * cw, mb_h_field * cmb_h, cpad);
            self.cr.resize(mb_w * cw, mb_h_field * cmb_h, cpad);
        }
    }

    /// Recompute the aggregate marking and the picture order count of a frame
    /// store from its fields, and copy both into the field views.
    pub(crate) fn refresh(&mut self) {
        self.mark = if self.fmark.contains(&Mark::Short) {
            Mark::Short
        } else if self.fmark.contains(&Mark::Long) {
            Mark::Long
        } else {
            Mark::Unused
        };
        self.poc = match self.have {
            [true, true] => self.fpoc[0].min(self.fpoc[1]),
            [true, false] => self.fpoc[0],
            [false, true] => self.fpoc[1],
            [false, false] => self.poc,
        };
        let (marks, pocs, idx) = (self.fmark, self.fpoc, self.long_term_frame_idx);
        for (p, f) in self.fields.iter_mut().enumerate().take(2) {
            f.mark = marks[p];
            f.poc = pocs[p];
            f.long_term_frame_idx = idx;
        }
    }
}

/// Copy field `parity` of `src` into `dst` (a half-height plane) and replicate
/// its borders.
fn split_plane<S: Sample>(src: &PicPlane<S>, parity: usize, dst: &mut PicPlane<S>) {
    let w = src.width;
    for y in 0..dst.height {
        let s = src.at(0, 2 * y + parity);
        let d = dst.at(0, y);
        dst.data[d..d + w].copy_from_slice(&src.data[s..s + w]);
    }
    dst.extend_borders();
}

/// Interleave two field planes into a frame plane and replicate its borders.
/// A field missing from the pair is stood in for by the other one.
fn weave_plane<S: Sample>(dst: &mut PicPlane<S>, top: &PicPlane<S>, bottom: &PicPlane<S>) {
    let w = dst.width;
    for y in 0..top.height {
        for (parity, src) in [top, bottom].into_iter().enumerate() {
            let s = src.at(0, y);
            let d = dst.at(0, 2 * y + parity);
            dst.data[d..d + w].copy_from_slice(&src.data[s..s + w]);
        }
    }
    dst.extend_borders();
}

impl<S: Sample> Dpb<S> {
    /// Give a finished frame its two field views, split out of its planes.
    pub(crate) fn attach_fields_from_frame(&mut self, pic: &mut Picture<S>) {
        while pic.fields.len() < 2 {
            let f = self.take_field_picture();
            pic.fields.push(f);
        }
        pic.fields.truncate(2);
        let (bd, cat, mb_w, sps_id) = (pic.bd, pic.cat, pic.mb_w, pic.sps_id);
        let half = pic.mb_h / 2;
        let (planes, fields) = ([&pic.y, &pic.cb, &pic.cr], &mut pic.fields);
        for (p, f) in fields.iter_mut().enumerate() {
            f.size_field_planes(bd, cat, mb_w, half, sps_id);
            f.structure = p as u8 + 1;
            split_plane(planes[0], p, &mut f.y);
            split_plane(planes[1], p, &mut f.cb);
            split_plane(planes[2], p, &mut f.cr);
        }
        let (id, marks, pocs) = (pic.id, pic.fmark, pic.fpoc);
        for (p, f) in pic.fields.iter_mut().enumerate() {
            f.id = id + 1 + p as i32;
            f.mark = marks[p];
            f.poc = pocs[p];
            f.complete = true;
            f.decoded_mbs = 0;
        }
    }

    /// Weave the frame planes of a frame store from its decoded fields.
    fn weave(pic: &mut Picture<S>) {
        let [top, bottom] = match pic.have {
            [true, true] => [0, 1],
            [true, false] => [0, 0],
            _ => [1, 1],
        };
        let (t, b) = (&pic.fields[top], &pic.fields[bottom]);
        weave_plane(&mut pic.y, &t.y, &b.y);
        weave_plane(&mut pic.cb, &t.cb, &b.cb);
        weave_plane(&mut pic.cr, &t.cr, &b.cr);
    }

    /// The frame store whose first field was the previous picture and which
    /// `structure` (of a picture with `frame_num`) completes, if any.
    pub(crate) fn pending_first_field(
        &self,
        frame_num: u32,
        structure: u8,
        is_idr: bool,
    ) -> Option<usize> {
        if structure == FRAME || is_idr {
            return None;
        }
        let p = usize::from(structure) - 1;
        self.frames.iter().position(|f| {
            f.awaiting_second && f.id == self.last_id && f.frame_num == frame_num && f.have[1 - p]
        })
    }

    /// A picture that is not the second field of a waiting first field has
    /// arrived: that field stays a non-paired field, and is complete.
    pub(crate) fn finalize_unpaired(&mut self) {
        for pic in &mut self.frames {
            if pic.awaiting_second {
                pic.awaiting_second = false;
                Self::weave(pic);
                pic.refresh();
            }
        }
    }

    /// `FrameNumWrap` for every stored frame with a short-term field (8.2.4.1).
    pub(crate) fn number_short_term_i(&mut self, frame_num: u32, sps: &Sps) {
        let max_frame_num = 1i32 << sps.log2_max_frame_num;
        for pic in &mut self.frames {
            if pic.fmark.contains(&Mark::Short) {
                pic.frame_num_wrap = if pic.frame_num > frame_num {
                    pic.frame_num as i32 - max_frame_num
                } else {
                    pic.frame_num as i32
                };
            }
        }
    }

    /// `RefPicList0/1` for a slice of a picture with `structure`
    /// (8.2.4.2.1 to 8.2.4.2.5, then 8.2.4.3).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build_ref_lists_i(
        &self,
        slice_type: SliceType,
        structure: u8,
        curr_poc: i32,
        curr_pic_num: i32,
        max_pic_num: i32,
        num_l0: usize,
        num_l1: usize,
        mods: (&[RefPicListMod], &[RefPicListMod]),
    ) -> Result<([RefList; 2], bool)> {
        let b = slice_type == SliceType::B;
        let field = structure != FRAME;
        let mut short = [0usize; MAX_STORED];
        let mut n_short = 0;
        let mut long = [0usize; MAX_STORED];
        let mut n_long = 0;
        for (i, pic) in self.frames.iter().enumerate() {
            if pic.non_existing && b {
                continue;
            }
            let (is_short, is_long) = if field {
                (
                    (0..2).any(|p| pic.have[p] && pic.fmark[p] == Mark::Short),
                    (0..2).any(|p| pic.have[p] && pic.fmark[p] == Mark::Long),
                )
            } else {
                (
                    pic.have == [true, true] && pic.fmark == [Mark::Short; 2],
                    pic.have == [true, true] && pic.fmark == [Mark::Long; 2],
                )
            };
            if is_short && n_short < MAX_STORED {
                short[n_short] = i;
                n_short += 1;
            }
            if is_long && n_long < MAX_STORED {
                long[n_long] = i;
                n_long += 1;
            }
        }
        let long = &mut long[..n_long];
        long.sort_unstable_by_key(|&i| self.frames[i].long_term_frame_idx);
        let short = &mut short[..n_short];

        let mut lists = [RefList::default(), RefList::default()];
        let cur_parity = usize::from(structure == BOTTOM);
        let emit = |lists: &mut [RefList; 2], x: usize, short: &[usize]| {
            if field {
                self.alternate(short, cur_parity, Mark::Short, &mut lists[x]);
                self.alternate(long, cur_parity, Mark::Long, &mut lists[x]);
            } else {
                for &i in short {
                    lists[x].push(i);
                }
                for &i in long.iter() {
                    lists[x].push(i);
                }
            }
        };
        if b {
            // 8.2.4.2.3 / 8.2.4.2.4: by output order around the current picture.
            // A field accepts equal counts on the "before" side (the first field
            // of its own frame can tie it).
            let before = |poc: i32| {
                if field {
                    poc <= curr_poc
                } else {
                    poc < curr_poc
                }
            };
            short.sort_unstable_by_key(|&i| {
                let poc = self.frames[i].poc;
                if before(poc) { (0i32, -poc) } else { (1, poc) }
            });
            emit(&mut lists, 0, short);
            short.sort_unstable_by_key(|&i| {
                let poc = self.frames[i].poc;
                if !before(poc) { (0i32, poc) } else { (1, -poc) }
            });
            emit(&mut lists, 1, short);
            if lists[1].len > 1
                && lists[1].entries[..lists[1].len] == lists[0].entries[..lists[0].len]
            {
                lists[1].entries.swap(0, 1);
            }
        } else {
            short.sort_unstable_by_key(|&i| -self.frames[i].frame_num_wrap);
            emit(&mut lists, 0, short);
        }

        let wanted = [num_l0, num_l1];
        let mods = [mods.0, mods.1];
        let mut padded = false;
        for x in 0..2 {
            if x == 1 && !b {
                lists[1].len = 0;
                continue;
            }
            if lists[x].len == 0 {
                return Err(Error::corrupt("inter slice with an empty reference list"));
            }
            let first = lists[x].entries[0];
            if lists[x].len < wanted[x] {
                padded = true;
            }
            while lists[x].len < wanted[x] {
                let (i, s) = (usize::from(first & 31), first >> 5);
                lists[x].push_ref(i, s);
            }
            lists[x].len = wanted[x];
            self.modify_list_i(&mut lists[x], mods[x], curr_pic_num, max_pic_num, structure);
        }
        Ok((lists, padded))
    }

    /// 8.2.4.2.5: fields of `frames` (in list order) of marking `kind`,
    /// alternating parity from the current field's own.
    fn alternate(&self, frames: &[usize], cur_parity: usize, kind: Mark, out: &mut RefList) {
        let has = |i: usize, p: usize| {
            let f = &self.frames[i];
            f.have[p] && f.fmark[p] == kind
        };
        let mut cursor = [0usize; 2];
        let mut want = cur_parity;
        loop {
            let mut found = None;
            while cursor[want] < frames.len() {
                let i = frames[cursor[want]];
                cursor[want] += 1;
                if has(i, want) {
                    found = Some(i);
                    break;
                }
            }
            match found {
                Some(i) => {
                    out.push_ref(i, want as u8 + 1);
                    want ^= 1;
                }
                None => {
                    let o = want ^ 1;
                    while cursor[o] < frames.len() {
                        let i = frames[cursor[o]];
                        cursor[o] += 1;
                        if has(i, o) {
                            out.push_ref(i, o as u8 + 1);
                        }
                    }
                    break;
                }
            }
        }
    }

    /// Reference picture list modification (8.2.4.3) over entries that name a
    /// frame or one field of a frame.
    fn modify_list_i(
        &self,
        list: &mut RefList,
        mods: &[RefPicListMod],
        curr_pic_num: i32,
        max_pic_num: i32,
        structure: u8,
    ) {
        if mods.is_empty() {
            return;
        }
        let active = list.len;
        let mut tmp = [0u8; 34];
        tmp[..active].copy_from_slice(&list.entries[..active]);
        let mut pred = curr_pic_num;
        let mut ref_idx = 0usize;
        // The picture an entry names, searched over every stored frame (and
        // field): `want` tests (frame store, parity).
        let find = |want: &dyn Fn(&Picture<S>, usize) -> bool| -> Option<u8> {
            for (i, f) in self.frames.iter().enumerate() {
                if structure == FRAME {
                    if want(f, 2) {
                        return Some(i as u8);
                    }
                } else {
                    for p in 0..2 {
                        if f.have[p] && want(f, p) {
                            return Some(i as u8 | ((p as u8 + 1) << 5));
                        }
                    }
                }
            }
            None
        };
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
                    find(&|f, p| {
                        if p == 2 {
                            f.have == [true, true]
                                && f.fmark == [Mark::Short; 2]
                                && f.frame_num_wrap == pic_num
                        } else {
                            f.fmark[p] == Mark::Short
                                && short_pic_num(f.frame_num_wrap, p, structure) == pic_num
                        }
                    })
                }
                RefPicListMod::LongTerm(num) => find(&|f, p| {
                    if p == 2 {
                        f.have == [true, true]
                            && f.fmark == [Mark::Long; 2]
                            && f.long_term_frame_idx as i32 == num as i32
                    } else {
                        f.fmark[p] == Mark::Long
                            && long_pic_num(f.long_term_frame_idx, p, structure) == num as i32
                    }
                }),
            };
            let Some(target) = target else { continue };
            if ref_idx >= active {
                break;
            }
            for c in (ref_idx + 1..=active).rev() {
                tmp[c] = tmp[c - 1];
            }
            tmp[ref_idx] = target;
            ref_idx += 1;
            let mut n = ref_idx;
            for c in ref_idx..=active {
                if tmp[c] != target {
                    tmp[n] = tmp[c];
                    n += 1;
                }
            }
        }
        list.entries[..active].copy_from_slice(&tmp[..active]);
        list.len = active;
    }

    /// Mark every stored picture unused (IDR and MMCO 5).
    fn unmark_all(&mut self) {
        for pic in &mut self.frames {
            pic.fmark = [Mark::Unused; 2];
            pic.refresh();
        }
    }

    /// Sliding window marking (8.2.5.3) for a picture of `info.structure`.
    fn sliding_window_i(&mut self, sps: &Sps, info: &PicInfo) -> Result<()> {
        if info.structure != FRAME
            && let Some(k) = info.second_of
        {
            // The second field of a pair whose first field is a short-term
            // reference joins it without displacing anything.
            let first = usize::from(info.structure == TOP);
            if self.frames[k].fmark[first] == Mark::Short {
                return Ok(());
            }
        }
        let max_refs = (sps.max_num_ref_frames as usize).max(1);
        loop {
            let n = self
                .frames
                .iter()
                .filter(|p| p.fmark != [Mark::Unused; 2])
                .count();
            if n < max_refs {
                return Ok(());
            }
            let Some(oldest) = self
                .frames
                .iter_mut()
                .filter(|p| p.fmark.contains(&Mark::Short))
                .min_by_key(|p| p.frame_num_wrap)
            else {
                return Ok(());
            };
            for m in &mut oldest.fmark {
                if *m == Mark::Short {
                    *m = Mark::Unused;
                }
            }
            oldest.refresh();
        }
    }

    /// Free `LongTermFrameIdx` `idx` for assignment to a picture of frame store
    /// `keep` (8.2.5.4.3, 8.2.5.4.6): long-term fields elsewhere that hold it
    /// become unused; the other field of `keep` may share it.
    fn free_long_term_idx(&mut self, idx: u32, keep: Option<usize>) {
        for (j, p) in self.frames.iter_mut().enumerate() {
            if Some(j) == keep {
                continue;
            }
            if p.long_term_frame_idx == idx && p.fmark.contains(&Mark::Long) {
                for m in &mut p.fmark {
                    if *m == Mark::Long {
                        *m = Mark::Unused;
                    }
                }
                p.refresh();
            }
        }
    }

    /// Adaptive marking (8.2.5.4). Returns the marking the current picture
    /// ends with and its `LongTermFrameIdx`.
    fn apply_mmco_i(&mut self, m: &DecRefPicMarking, info: &PicInfo) -> Result<(Mark, u32)> {
        let structure = info.structure;
        let curr_pic_num = if structure == FRAME {
            info.frame_num as i32
        } else {
            2 * info.frame_num as i32 + 1
        };
        let mut cur = (Mark::Short, 0u32);
        for op in &m.mmcos {
            match op.op {
                1 | 3 => {
                    let target = curr_pic_num - (op.arg1 as i32 + 1);
                    // The short-term frame, or field, picNumX names.
                    let found = self.frames.iter().enumerate().find_map(|(k, f)| {
                        if structure == FRAME {
                            (f.fmark == [Mark::Short; 2] && f.frame_num_wrap == target)
                                .then_some((k, 2))
                        } else {
                            (0..2)
                                .find(|&p| {
                                    f.fmark[p] == Mark::Short
                                        && short_pic_num(f.frame_num_wrap, p, structure) == target
                                })
                                .map(|p| (k, p))
                        }
                    });
                    let Some((k, p)) = found else { continue };
                    if op.op == 1 {
                        if p == 2 {
                            self.frames[k].fmark = [Mark::Unused; 2];
                        } else {
                            self.frames[k].fmark[p] = Mark::Unused;
                        }
                    } else {
                        let idx = op.arg2;
                        self.free_long_term_idx(idx, Some(k));
                        let f = &mut self.frames[k];
                        if p == 2 {
                            f.fmark = [Mark::Long; 2];
                        } else {
                            f.fmark[p] = Mark::Long;
                        }
                        f.long_term_frame_idx = idx;
                    }
                    self.frames[k].refresh();
                }
                2 => {
                    let num = op.arg1 as i32;
                    let found = self.frames.iter().enumerate().find_map(|(k, f)| {
                        if structure == FRAME {
                            (f.fmark == [Mark::Long; 2] && f.long_term_frame_idx as i32 == num)
                                .then_some((k, 2))
                        } else {
                            (0..2)
                                .find(|&p| {
                                    f.fmark[p] == Mark::Long
                                        && long_pic_num(f.long_term_frame_idx, p, structure) == num
                                })
                                .map(|p| (k, p))
                        }
                    });
                    if let Some((k, p)) = found {
                        if p == 2 {
                            self.frames[k].fmark = [Mark::Unused; 2];
                        } else {
                            self.frames[k].fmark[p] = Mark::Unused;
                        }
                        self.frames[k].refresh();
                    }
                }
                4 => {
                    self.max_long_term_idx = op.arg1.checked_sub(1);
                    let limit = self.max_long_term_idx;
                    for p in &mut self.frames {
                        if p.long_term_frame_idx_over(limit) {
                            for mk in &mut p.fmark {
                                if *mk == Mark::Long {
                                    *mk = Mark::Unused;
                                }
                            }
                            p.refresh();
                        }
                    }
                }
                5 => {
                    self.unmark_all();
                    self.max_long_term_idx = None;
                }
                6 => {
                    let idx = op.arg1;
                    self.free_long_term_idx(idx, info.second_of);
                    cur = (Mark::Long, idx);
                }
                other => {
                    return Err(Error::corrupt(format!(
                        "memory_management_control_operation {other}"
                    )));
                }
            }
        }
        Ok(cur)
    }

    /// Output and unmark everything (IDR boundary).
    fn drain_prior_i(&mut self, discard: bool) {
        for pic in &mut self.frames {
            pic.fmark = [Mark::Unused; 2];
            pic.refresh();
            if discard {
                pic.output = false;
            }
        }
    }

    /// Insert the "non-existing" frames of 8.2.5.2 for a `frame_num` gap.
    pub(crate) fn fill_frame_num_gap_i(&mut self, sps: &Sps, frame_num: u32) -> Result<()> {
        let max = 1u32 << sps.log2_max_frame_num;
        let mut unused = (self.prev_ref_frame_num + 1) % max;
        let mut budget = MAX_STORED;
        while unused != frame_num && budget > 0 {
            budget -= 1;
            let mut pic = self.take_picture();
            pic.start(sps);
            match self.frames.iter().max_by_key(|p| p.frame_num_wrap) {
                Some(src) if src.y.data.len() == pic.y.data.len() => pic.copy_samples_from(src),
                _ => pic.fill_grey(),
            }
            pic.non_existing = true;
            pic.frame_num = unused;
            pic.output = false;
            pic.complete = true;
            let info = PicInfo {
                structure: FRAME,
                second_of: None,
                is_idr: false,
                is_reference: true,
                frame_num: unused,
                pic_order_cnt_lsb: 0,
                delta_pic_order_cnt_bottom: 0,
                delta_pic_order_cnt: [0; 2],
            };
            let poc = if sps.pic_order_cnt_type == 0 {
                Poc::default()
            } else {
                self.picture_order_count(sps, &info)?
            };
            pic.poc = poc.value;
            pic.fpoc = [poc.top, poc.bottom];
            pic.have = [true, true];
            pic.frame_num_offset = poc.frame_num_offset;
            self.number_short_term_i(unused, sps);
            self.sliding_window_i(sps, &info)?;
            pic.fmark = [Mark::Short; 2];
            pic.frame_num_wrap = unused as i32;
            self.attach_fields_from_frame(&mut pic);
            pic.refresh();
            self.frames.push(pic);
            self.prev_frame_num = unused;
            self.prev_frame_num_offset = poc.frame_num_offset;
            self.prev_has_mmco5 = false;
            self.prev_ref_frame_num = unused;
            unused = (unused + 1) % max;
        }
        Ok(())
    }

    /// Store a decoded frame or field and apply the reference marking of 8.2.5.
    ///
    /// A first field creates its frame store and leaves it awaiting the second;
    /// a second field (`info.second_of`) completes the store it names.
    pub(crate) fn store_i(
        &mut self,
        mut pic: Picture<S>,
        sps: &Sps,
        info: &PicInfo,
        marking: Option<&DecRefPicMarking>,
        output_ok: bool,
    ) -> Result<()> {
        let structure = info.structure;
        let has_mmco5 = marking.is_some_and(|m| m.mmcos.iter().any(|c| c.op == 5));
        let (frame_num_offset, poc_msb, poc_lsb) = (pic.frame_num_offset, pic.poc_msb, pic.poc_lsb);
        self.number_short_term_i(info.frame_num, sps);

        let mut cur = (Mark::Unused, 0u32);
        if info.is_idr {
            let no_output = marking.is_some_and(|m| m.no_output_of_prior_pics);
            self.drain_prior_i(no_output);
            cur = (Mark::Short, 0);
            if marking.is_some_and(|m| m.long_term_reference) {
                cur = (Mark::Long, 0);
                self.max_long_term_idx = Some(0);
            } else {
                self.max_long_term_idx = None;
            }
        } else if info.is_reference {
            let Some(m) = marking else {
                return Err(Error::corrupt(
                    "a reference picture carries no dec_ref_pic_marking",
                ));
            };
            cur = (Mark::Short, 0);
            if m.adaptive {
                cur = self.apply_mmco_i(m, info)?;
            } else {
                self.sliding_window_i(sps, info)?;
            }
        }
        // A second field's store index may have shifted if MMCO emptied slots;
        // the buffer only reclaims slots after output, so it has not.
        let own = info.second_of;

        if own.is_none() && self.frames.len() >= MAX_STORED {
            return Err(Error::corrupt("decoded picture buffer overflow"));
        }

        // After MMCO 5 the picture counts as frame_num 0 with its POC rebased
        // (8.2.1).
        let mut fpoc_rebase = [0i32; 2];
        if has_mmco5 {
            pic.frame_num = 0;
            pic.frame_num_offset = 0;
            pic.poc_msb = 0;
            match structure {
                FRAME => {
                    let t = pic.fpoc[0].min(pic.fpoc[1]);
                    fpoc_rebase = [t, t];
                }
                TOP => fpoc_rebase[0] = pic.fpoc[0],
                _ => fpoc_rebase[1] = pic.fpoc[1],
            }
        }

        let prev_lsb_after_mmco5;
        let stored_id;
        match structure {
            FRAME => {
                pic.have = [true, true];
                pic.fmark = [cur.0; 2];
                if cur.0 == Mark::Long {
                    pic.long_term_frame_idx = cur.1;
                }
                pic.output = output_ok;
                pic.complete = true;
                pic.awaiting_second = false;
                for (v, r) in pic.fpoc.iter_mut().zip(fpoc_rebase) {
                    *v -= r;
                }
                self.attach_fields_from_frame(&mut pic);
                pic.refresh();
                prev_lsb_after_mmco5 = pic.fpoc[0];
                stored_id = pic.id;
                self.frames.push(pic);
            }
            _ => {
                let p = usize::from(structure) - 1;
                let k = match own {
                    Some(k) => k,
                    None => {
                        let mut shell = self.take_picture();
                        shell.start(sps);
                        shell.coding = CODED_FLD;
                        shell.frame_num = pic.frame_num;
                        shell.frame_num_offset = pic.frame_num_offset;
                        shell.poc_msb = pic.poc_msb;
                        shell.poc_lsb = pic.poc_lsb;
                        shell.pts = pic.pts;
                        shell.color = pic.color;
                        shell.output = output_ok;
                        shell.awaiting_second = true;
                        shell.complete = true;
                        shell.long_term_frame_idx = 0;
                        while shell.fields.len() < 2 {
                            let mut f = self.take_field_picture();
                            f.structure = shell.fields.len() as u8 + 1;
                            shell.fields.push(f);
                        }
                        self.frames.push(shell);
                        self.frames.len() - 1
                    }
                };
                pic.fpoc[p] -= fpoc_rebase[p];
                pic.poc = pic.fpoc[p];
                let store = &mut self.frames[k];
                pic.id = store.id + 1 + p as i32;
                pic.complete = true;
                pic.mark = cur.0;
                let old = core::mem::replace(&mut store.fields[p], pic);
                store.have[p] = true;
                store.fpoc[p] = store.fields[p].poc;
                store.fmark[p] = cur.0;
                if cur.0 == Mark::Long {
                    store.long_term_frame_idx = cur.1;
                }
                if own.is_some() {
                    store.awaiting_second = false;
                    Self::weave(store);
                }
                if has_mmco5 {
                    store.frame_num = 0;
                }
                store.refresh();
                prev_lsb_after_mmco5 = if structure == TOP { store.fpoc[0] } else { 0 };
                stored_id = store.id;
                self.recycle(old);
            }
        }

        self.prev_frame_num = if has_mmco5 { 0 } else { info.frame_num };
        self.prev_frame_num_offset = if has_mmco5 { 0 } else { frame_num_offset };
        self.prev_has_mmco5 = has_mmco5;
        if info.is_reference {
            self.prev_ref_frame_num = if has_mmco5 { 0 } else { info.frame_num };
            self.prev_poc_msb = if has_mmco5 { 0 } else { poc_msb };
            self.prev_poc_lsb = if has_mmco5 {
                prev_lsb_after_mmco5
            } else {
                poc_lsb
            };
        }
        self.started = true;
        if (info.is_idr || has_mmco5) && self.frames.iter().any(|p| p.output) {
            self.pending_flush = true;
        }
        self.last_id = stored_id;
        Ok(())
    }
}

impl<S: Sample> Picture<S> {
    /// True when a long-term field of this store sits above the limit.
    fn long_term_frame_idx_over(&self, limit: Option<u32>) -> bool {
        self.fmark.contains(&Mark::Long) && limit.is_none_or(|max| self.long_term_frame_idx > max)
    }
}

//! Reference picture sets, marking and reference picture lists (8.3.2 to 8.3.4).

use super::dpb::{Dpb, Marking};
use super::shared::SharedPic;
use crate::decode::DecodedPicture;
use ec_core::error::{Error, Result};
use ec_h265_syntax::slice::{LongTermRef, SliceHeader, SliceType};
use ec_h265_syntax::{ConformanceWindow, ShortTermRefPicSet};
use std::sync::Arc;

/// What clause 8.3.2 needs from the current picture and its slice header.
#[derive(Debug, Clone, Copy)]
pub struct RpsInput<'a> {
    /// `PicOrderCntVal` of the current picture.
    pub poc: i32,
    /// `log2_max_pic_order_cnt_lsb`.
    pub log2_max_poc_lsb: u32,
    /// The slice's short-term reference picture set (empty for an IDR picture).
    pub st: &'a ShortTermRefPicSet,
    /// The slice's long-term entries (empty for an IDR picture).
    pub lt: &'a [LongTermRef],
    /// The current picture is an IRAP with NoRaslOutputFlag: every reference
    /// picture in the buffer is marked unused first.
    pub irap_no_rasl: bool,
    /// Luma width of the picture, for generated pictures.
    pub width: usize,
    /// Luma height of the picture, for generated pictures.
    pub height: usize,
    /// Bit depths (luma, chroma) of a generated (8.3.3) picture, whose samples are
    /// grey. `None` generates a picture without samples, for a decoder that does
    /// not decode pixels.
    pub fill: Option<(u32, u32)>,
    /// `chroma_format_idc` of the generated picture.
    pub chroma_format_idc: u32,
}

/// One entry of a reference picture set or list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefEntry {
    /// The picture's id in the decoded picture buffer.
    pub id: u32,
    /// Its `PicOrderCntVal`.
    pub poc: i32,
    /// Whether it is a long-term reference.
    pub long_term: bool,
}

/// The five reference picture subsets of 8.3.2, as buffer ids. Missing
/// `Curr` entries are generated (8.3.3); missing `Foll` entries are left out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefPicSet {
    /// `RefPicSetStCurrBefore`.
    pub st_curr_before: Vec<RefEntry>,
    /// `RefPicSetStCurrAfter`.
    pub st_curr_after: Vec<RefEntry>,
    /// `RefPicSetStFoll`.
    pub st_foll: Vec<RefEntry>,
    /// `RefPicSetLtCurr`.
    pub lt_curr: Vec<RefEntry>,
    /// `RefPicSetLtFoll`.
    pub lt_foll: Vec<RefEntry>,
}

/// `RefPicList0` and `RefPicList1` of one slice (8.3.4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefPicLists {
    /// `RefPicList0`.
    pub l0: Vec<RefEntry>,
    /// `RefPicList1`; empty unless the slice is a B slice.
    pub l1: Vec<RefEntry>,
}

/// A long-term or short-term candidate: found in the buffer, or missing.
#[derive(Debug, Clone, Copy)]
enum Slot {
    Found(RefEntry),
    Missing(i32),
}

impl Dpb {
    /// Derive the reference picture set of the current picture and apply its
    /// marking to the buffer (8.3.2), generating any missing `Curr` picture
    /// (8.3.3) so that inter prediction always has something to read.
    pub fn apply_rps(&mut self, input: &RpsInput<'_>) -> RefPicSet {
        let max_lsb = 1i64 << input.log2_max_poc_lsb;
        let poc = i64::from(input.poc);
        if input.irap_no_rasl {
            for p in self.pictures_mut() {
                p.marking = Marking::Unused;
            }
        }

        // Long-term entries (8-5): a picture named here becomes long-term. The
        // candidates are reference pictures of either marking.
        let mut lt_slots: Vec<(Slot, bool)> = Vec::new();
        for e in input.lt {
            let msb_present = e.delta_poc_msb_present;
            let mut target = i64::from(e.poc_lsb_lt);
            if msb_present {
                target += poc - i64::from(e.delta_poc_msb_cycle) * max_lsb - (poc & (max_lsb - 1));
            }
            let found = self.pictures().iter().find(|p| {
                if p.marking == Marking::Unused {
                    return false;
                }
                let p_poc = i64::from(p.poc);
                if msb_present {
                    p_poc == target
                } else {
                    (p_poc & (max_lsb - 1)) == target
                }
            });
            let slot = match found {
                Some(p) => Slot::Found(RefEntry {
                    id: p.id,
                    poc: p.poc,
                    long_term: true,
                }),
                None => Slot::Missing(target as i32),
            };
            lt_slots.push((slot, e.used_by_curr));
        }
        for (slot, _) in &lt_slots {
            if let Slot::Found(entry) = slot {
                self.mark(entry.id, Marking::LongTerm);
            }
        }

        // Short-term entries (8-6 to 8-8): short-term pictures only, after the
        // long-term conversion above.
        let mut st_before: Vec<(Slot, bool)> = Vec::new();
        let mut st_after: Vec<(Slot, bool)> = Vec::new();
        let st = input.st;
        for i in 0..st.num_negative as usize {
            let target = poc + i64::from(st.delta_poc_s0[i]);
            st_before.push((self.find_short(target), st.used_s0[i]));
        }
        for i in 0..st.num_positive as usize {
            let target = poc + i64::from(st.delta_poc_s1[i]);
            st_after.push((self.find_short(target), st.used_s1[i]));
        }

        // Everything not named by one of the five lists is no longer a reference.
        let named: Vec<u32> = lt_slots
            .iter()
            .chain(st_before.iter())
            .chain(st_after.iter())
            .filter_map(|(slot, _)| match slot {
                Slot::Found(e) => Some(e.id),
                Slot::Missing(_) => None,
            })
            .collect();
        for p in self.pictures_mut() {
            if p.marking != Marking::Unused && !named.contains(&p.id) {
                p.marking = Marking::Unused;
            }
        }

        let mut set = RefPicSet::default();
        for (slot, used) in lt_slots {
            if let Some(entry) = self.resolve(slot, used, true, input) {
                if used {
                    set.lt_curr.push(entry);
                } else {
                    set.lt_foll.push(entry);
                }
            }
        }
        for (slot, used) in st_before {
            if let Some(entry) = self.resolve(slot, used, false, input) {
                if used {
                    set.st_curr_before.push(entry);
                } else {
                    set.st_foll.push(entry);
                }
            }
        }
        for (slot, used) in st_after {
            if let Some(entry) = self.resolve(slot, used, false, input) {
                if used {
                    set.st_curr_after.push(entry);
                } else {
                    set.st_foll.push(entry);
                }
            }
        }
        set
    }

    fn find_short(&self, target: i64) -> Slot {
        match self
            .pictures()
            .iter()
            .find(|p| p.marking == Marking::ShortTerm && i64::from(p.poc) == target)
        {
            Some(p) => Slot::Found(RefEntry {
                id: p.id,
                poc: p.poc,
                long_term: false,
            }),
            None => Slot::Missing(target as i32),
        }
    }

    fn mark(&mut self, id: u32, marking: Marking) {
        for p in self.pictures_mut() {
            if p.id == id {
                p.marking = marking;
            }
        }
    }

    /// Turn a candidate into an entry. A missing `Curr` entry is generated; a
    /// missing `Foll` entry is dropped.
    fn resolve(
        &mut self,
        slot: Slot,
        used: bool,
        long_term: bool,
        input: &RpsInput<'_>,
    ) -> Option<RefEntry> {
        match slot {
            Slot::Found(entry) => Some(entry),
            Slot::Missing(poc) if used => {
                let marking = if long_term {
                    Marking::LongTerm
                } else {
                    Marking::ShortTerm
                };
                let id = self.generate(poc, marking, input);
                Some(RefEntry { id, poc, long_term })
            }
            Slot::Missing(_) => None,
        }
    }

    /// Generate a missing reference picture (8.3.3.2): grey samples, no output.
    fn generate(&mut self, poc: i32, marking: Marking, input: &RpsInput<'_>) -> u32 {
        let picture = match input.fill {
            Some((bit_depth, bit_depth_c)) => DecodedPicture::grey_planes_fmt(
                input.width,
                input.height,
                bit_depth,
                bit_depth_c,
                input.chroma_format_idc,
            ),
            None => DecodedPicture {
                width: input.width,
                height: input.height,
                bit_depth: 8,
                bit_depth_c: 8,
                chroma_format_idc: input.chroma_format_idc,
                y: Vec::new(),
                cb: Vec::new(),
                cr: Vec::new(),
            },
        };
        self.insert(
            poc,
            marking,
            false,
            Arc::new(SharedPic::from_planes(poc, picture)),
            ConformanceWindow::default(),
            u32::MAX,
        )
    }
}

/// Build `RefPicList0` and `RefPicList1` for a slice (8.3.4). Entries cycle
/// through the `Curr` subsets until the list is as long as the active count;
/// `ref_pic_list_modification` picks entries out of that cycle.
pub fn build_ref_lists(set: &RefPicSet, header: &SliceHeader) -> Result<RefPicLists> {
    let mut lists = RefPicLists::default();
    if header.slice_type == SliceType::I {
        return Ok(lists);
    }
    let total = set.st_curr_before.len() + set.st_curr_after.len() + set.lt_curr.len();
    if total == 0 {
        return Err(Error::corrupt(
            "HEVC slice: inter slice whose RPS has no reference picture".to_string(),
        ));
    }
    let num0 = header.num_ref_idx_l0_active_minus1 as usize + 1;
    let temp0 = cycle(
        &[&set.st_curr_before, &set.st_curr_after, &set.lt_curr],
        num0.max(total),
    );
    lists.l0 = pick(&temp0, &header.list_entry[0], num0)?;
    if header.slice_type == SliceType::B {
        let num1 = header.num_ref_idx_l1_active_minus1 as usize + 1;
        let temp1 = cycle(
            &[&set.st_curr_after, &set.st_curr_before, &set.lt_curr],
            num1.max(total),
        );
        lists.l1 = pick(&temp1, &header.list_entry[1], num1)?;
    }
    Ok(lists)
}

fn cycle(parts: &[&Vec<RefEntry>], len: usize) -> Vec<RefEntry> {
    let base: Vec<RefEntry> = parts.iter().flat_map(|p| p.iter().copied()).collect();
    if base.is_empty() {
        return Vec::new();
    }
    (0..len).map(|i| base[i % base.len()]).collect()
}

fn pick(temp: &[RefEntry], entries: &[u32], n: usize) -> Result<Vec<RefEntry>> {
    (0..n)
        .map(|i| {
            let idx = entries.get(i).map_or(i, |&e| e as usize);
            temp.get(idx).copied().ok_or_else(|| {
                Error::corrupt(format!(
                    "HEVC slice: list entry {idx} is past the {} candidates",
                    temp.len()
                ))
            })
        })
        .collect()
}

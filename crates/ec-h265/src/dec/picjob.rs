//! The decoding of one picture: its sample and map state, its motion field, the position
//! of its in-loop filters, and the publication of its rows to the [`SharedPic`] that
//! later pictures read.
//!
//! The job runs one slice segment at a time ([`PicJob::run_slice`]), with everything a
//! slice needs passed in owned, in a [`SliceJob`]. That is what lets the job run on a
//! worker thread. Slices are parsed in decoding order, and each finished CTB row goes
//! through the in-loop filters ([`RowFilter`]) and is published as soon as it is final.

use std::sync::Arc;

use super::cu_syntax::{parse_slice_into, wpp};
use super::inter::InterRefs;
use super::motion_field::{MotionField, SliceMotion, SliceRefs};
use super::pic::PicState;
use super::rowfilter::RowFilter;
use super::rps::RefPicLists;
use super::runner::{Budget, PictureSlot};
use super::shared::SharedPic;
use super::tiles::TileScan;
use ec_core::error::{Error, Result};
use ec_h265_syntax::slice::{SliceHeader, SliceType};
use ec_h265_syntax::{Pps, Sps};

/// One slice segment of a picture, with everything it reads.
pub struct SliceJob {
    /// The slice segment's RBSP (emulation prevention removed).
    pub rbsp: Vec<u8>,
    /// Where the slice data starts in `rbsp`.
    pub data_off: usize,
    /// The slice segment's header (dependent segments already filled in).
    pub header: SliceHeader,
    /// The active SPS and PPS.
    pub sps: Arc<Sps>,
    /// See `sps`.
    pub pps: Arc<Pps>,
    /// `RefPicList0` and `RefPicList1` of the slice.
    pub lists: RefPicLists,
    /// The pictures of each list, in list order (empty for an I slice).
    pub refs: [Vec<Arc<SharedPic>>; 2],
    /// The collocated picture (8.5.3.2.8), when temporal motion prediction applies.
    pub col: Option<Arc<SharedPic>>,
    /// `PicOrderCntVal` of the picture.
    pub cur_poc: i32,
    /// Reconstruct the samples (pixel decode). Off parses the syntax and the motion only.
    pub pixel: bool,
    /// Where the substreams after the first start, relative to `data_off` (one per CTB row
    /// after the segment's first, from the entry points). Empty when there are none.
    pub substreams: Vec<usize>,
}

/// The decoding state of one picture, from its first slice to its finish.
pub struct PicJob {
    /// The picture as later pictures see it.
    shared: Arc<SharedPic>,
    /// Maps and samples, from the first slice on.
    pic: Option<PicState>,
    /// The motion of every block, from the first slice on.
    motion: Option<MotionField>,
    /// How far the in-loop filters have got.
    filter: RowFilter,
    /// The worker threads this picture's row workers may take from.
    budget: Arc<Budget>,
    /// This picture's hold on its thread (see [`PictureSlot`]).
    _slot: PictureSlot,
}

impl PicJob {
    /// The job of the picture whose storage is `shared`, drawing threads from `budget`.
    pub fn new(shared: Arc<SharedPic>, budget: Arc<Budget>) -> PicJob {
        PicJob {
            shared,
            pic: None,
            motion: None,
            filter: RowFilter::new(),
            _slot: PictureSlot::new(Arc::clone(&budget)),
            budget,
        }
    }

    /// The published picture of this job.
    pub fn shared(&self) -> &Arc<SharedPic> {
        &self.shared
    }

    /// Parse one slice segment into the picture, and reconstruct it when `job.pixel`.
    /// Rows that become final are published as the slice finishes them.
    pub fn run_slice(&mut self, job: SliceJob) -> Result<()> {
        let PicJob {
            shared,
            pic,
            motion,
            filter,
            budget,
            ..
        } = self;
        let (sps, pps) = (&*job.sps, &*job.pps);
        let pic = pic.get_or_insert_with(|| PicState::for_sps_pps(sps, pps, job.pixel));
        let (w, h) = (
            sps.pic_width_in_luma_samples as usize,
            sps.pic_height_in_luma_samples as usize,
        );
        let field = motion.get_or_insert_with(|| MotionField::new(w, h));
        // 6.5.1: the tile of every CTB, for the spatial neighbours of 6.4.1.
        let ctb_log2 = sps.log2_min_cb_size_minus3 + 3 + sps.log2_diff_max_min_cb_size;
        let ctb = 1usize << ctb_log2;
        let (cols, rows) = (w.div_ceil(ctb), h.div_ceil(ctb));
        if let Ok(scan) = TileScan::from_pps(pps, cols, rows) {
            field.set_tiles(scan.tile_of_rs, ctb_log2, cols);
        }
        if !job.header.dependent_slice_segment {
            field.slices.push(SliceRefs::from_lists(&job.lists));
        }
        let slice = u16::try_from(field.slices.len().saturating_sub(1)).unwrap_or(u16::MAX);
        let refs = if job.pixel && job.header.slice_type != SliceType::I {
            Some(InterRefs::new(job.refs, &job.header, sps)?)
        } else {
            None
        };
        let data = job
            .rbsp
            .get(job.data_off..)
            .ok_or_else(|| Error::corrupt("HEVC slice: header runs past the RBSP".to_string()))?;
        let mut source = SliceMotion::new(
            sps,
            pps,
            &job.header,
            &job.lists,
            job.cur_poc,
            field,
            job.col,
            slice,
        );
        source.publish = Some(Arc::clone(shared));
        let shared: &SharedPic = shared;
        let mut rows = |pic: &mut PicState, ry: usize| {
            let final_rows = filter.advance(pic, ry + 1);
            shared.publish_rows(&pic.planes, final_rows);
        };
        // Wavefront rows run on the threads the budget has free; otherwise the serial parse.
        let (cols, ctb_rows) = (pic.ctb_cols, pic.ctb_rows);
        let extra = wpp::can_run(
            pps,
            &job.header,
            cols,
            ctb_rows,
            &job.substreams,
            data.len(),
        )
        .then(|| budget.take_extra(job.substreams.len()))
        .filter(|extra| extra.count() > 0);
        match extra {
            Some(extra) => {
                wpp::parse_segment(
                    sps,
                    pps,
                    &job.header,
                    data,
                    &job.substreams,
                    pic,
                    &mut source,
                    refs.as_ref(),
                    extra.count() + 1,
                    &mut rows,
                )?;
            }
            None => {
                parse_slice_into(
                    sps,
                    pps,
                    &job.header,
                    data,
                    &mut source,
                    pic,
                    refs.as_ref(),
                    &mut |_| {},
                    &mut rows,
                )?;
            }
        }
        Ok(())
    }

    /// The picture is complete: run the rest of the in-loop filters, publish every
    /// row and the collocated motion, and return the picture's motion checksum (`None`
    /// when no slice ran).
    pub fn finish(&mut self) -> Option<u64> {
        let PicJob {
            shared,
            pic,
            motion,
            filter,
            ..
        } = self;
        let Some(mut pic) = pic.take() else {
            shared.complete();
            return None;
        };
        let h = shared.height;
        // Slices that finished no CTB row (or several tiles) left the motion of the
        // units for the filter to read; fill every unit now.
        let checksum = motion.take().map(|field| {
            if pic.has_pixels() {
                field.deblock_rows(&mut pic.motion, 0, h);
            }
            shared.publish_col(&field, h);
            field.checksum()
        });
        let rows = pic.ctb_rows;
        let final_rows = filter.advance(&mut pic, rows);
        shared.publish_rows(&pic.planes, final_rows);
        shared.complete();
        checksum
    }
}

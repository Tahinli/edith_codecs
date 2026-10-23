//! The decoded picture buffer and output order (C.5.2): storage, marking, the
//! bumping process, and the frames handed to the caller.

use std::collections::VecDeque;
use std::sync::Arc;

use super::meta::PictureMeta;
use super::shared::SharedPic;
use crate::decode::DecodedPicture;
use ec_core::error::Result;
use ec_h265_syntax::sei::{ContentLightLevel, MasteringDisplay, PictureHash};
use ec_h265_syntax::{ColourDescription, ConformanceWindow, Sps};

/// Reference marking of a buffered picture (8.3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marking {
    /// Not used for reference: it can only wait for output.
    Unused,
    /// Used for short-term reference.
    ShortTerm,
    /// Used for long-term reference.
    LongTerm,
}

/// One picture in the decoded picture buffer.
#[derive(Debug, Clone)]
pub struct DpbPicture {
    /// Identity within this decoder; stable for the picture's whole life.
    pub id: u32,
    /// `PicOrderCntVal`.
    pub poc: i32,
    /// Reference marking.
    pub marking: Marking,
    /// `PicOutputFlag` still owed: the picture waits for the bumping process.
    pub needed_for_output: bool,
    /// `PicLatencyCount` (C.5.2.3).
    pub latency: u32,
    /// Decode-order index of the access unit this picture came from. Generated
    /// (8.3.3) pictures carry `u32::MAX`.
    pub au_index: u32,
    /// Cropping of the SPS the picture was decoded under.
    pub window: ConformanceWindow,
    /// The samples and the collocated motion (8.5.3.2.8). Empty samples when the decoder
    /// runs without pixel decode. The samples may still be being decoded.
    pub picture: Arc<SharedPic>,
    /// HDR, colour and hash metadata of the picture.
    pub meta: Arc<PictureMeta>,
}

/// A frame waiting for output. Its samples may still be being decoded; [`PendingFrame::resolve`]
/// waits for them.
#[derive(Debug, Clone)]
pub struct PendingFrame {
    /// See [`Frame::poc`].
    pub poc: i32,
    /// See [`Frame::au_index`].
    pub au_index: u32,
    /// See [`Frame::window`].
    pub window: ConformanceWindow,
    /// The picture, complete once [`PendingFrame::resolve`] returns.
    pub picture: Arc<SharedPic>,
    /// See [`Frame::meta`].
    pub meta: Arc<PictureMeta>,
}

impl PendingFrame {
    /// Wait for the picture to be complete and return it as a [`Frame`].
    pub fn resolve(self) -> Result<Frame> {
        let picture = self.picture.snapshot()?;
        Ok(Frame {
            poc: self.poc,
            au_index: self.au_index,
            window: self.window,
            picture,
            meta: self.meta,
        })
    }
}

/// A picture the decoder hands out, in output order.
#[derive(Debug, Clone)]
pub struct Frame {
    /// `PicOrderCntVal` of the picture.
    pub poc: i32,
    /// Decode-order index of the access unit (0 for the first picture start).
    pub au_index: u32,
    /// Cropping to apply for display.
    pub window: ConformanceWindow,
    /// The samples, at the coded size.
    pub picture: Arc<DecodedPicture>,
    /// HDR, colour and hash metadata of the picture.
    pub meta: Arc<PictureMeta>,
}

impl Frame {
    /// The mastering display colour volume in force for this frame (SEI 137).
    pub fn mastering_display(&self) -> Option<&MasteringDisplay> {
        self.meta.mastering_display.as_ref()
    }

    /// The content light level of this frame: MaxCLL and MaxFALL (SEI 144).
    pub fn content_light_level(&self) -> Option<&ContentLightLevel> {
        self.meta.content_light.as_ref()
    }

    /// `preferred_transfer_characteristics` of the alternative transfer SEI (147).
    pub fn alternative_transfer_characteristics(&self) -> Option<u8> {
        self.meta.alternative_transfer
    }

    /// The VUI colour description of the SPS this frame was decoded under.
    pub fn colour_description(&self) -> Option<ColourDescription> {
        self.meta.colour_description
    }

    /// The VUI `video_full_range_flag` of the SPS this frame was decoded under.
    pub fn video_full_range_flag(&self) -> Option<bool> {
        self.meta.video_full_range_flag
    }

    /// The `decoded_picture_hash` of this frame, when its suffix SEI came.
    pub fn picture_hash(&self) -> Option<&PictureHash> {
        self.meta.picture_hash.as_ref()
    }
}

/// The buffer sizes and reordering limits of the active SPS at HighestTid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DpbLimits {
    /// `sps_max_dec_pic_buffering_minus1[HighestTid] + 1`.
    pub max_dec_pic_buffering: usize,
    /// `sps_max_num_reorder_pics[HighestTid]`.
    pub max_num_reorder: usize,
    /// `sps_max_latency_increase_plus1[HighestTid]`.
    pub max_latency_plus1: u32,
}

impl DpbLimits {
    /// The limits an SPS declares.
    pub fn from_sps(sps: &Sps) -> DpbLimits {
        DpbLimits {
            max_dec_pic_buffering: sps.max_dec_pic_buffering_minus1 as usize + 1,
            max_num_reorder: sps.max_num_reorder_pics as usize,
            max_latency_plus1: sps.max_latency_increase_plus1,
        }
    }

    /// `SpsMaxLatencyPictures` (C.5.2.1). Meaningful only when `max_latency_plus1` is set.
    pub fn max_latency_pictures(&self) -> u64 {
        self.max_num_reorder as u64 + u64::from(self.max_latency_plus1).saturating_sub(1)
    }
}

/// The decoded picture buffer. Pictures are addressed by [`DpbPicture::id`].
#[derive(Debug, Default)]
pub struct Dpb {
    pics: Vec<DpbPicture>,
    next_id: u32,
}

impl Dpb {
    /// An empty buffer.
    pub fn new() -> Dpb {
        Dpb::default()
    }

    /// Every buffered picture, in storage order.
    pub fn pictures(&self) -> &[DpbPicture] {
        &self.pics
    }

    /// Mutable access for the picture-level bookkeeping in the decoder.
    pub fn pictures_mut(&mut self) -> &mut [DpbPicture] {
        &mut self.pics
    }

    /// Number of buffered pictures.
    pub fn len(&self) -> usize {
        self.pics.len()
    }

    /// True when nothing is buffered.
    pub fn is_empty(&self) -> bool {
        self.pics.is_empty()
    }

    /// The picture with this id, if still buffered.
    pub fn get(&self, id: u32) -> Option<&DpbPicture> {
        self.pics.iter().find(|p| p.id == id)
    }

    /// Store a picture and return its id.
    pub fn insert(
        &mut self,
        poc: i32,
        marking: Marking,
        needed_for_output: bool,
        picture: Arc<SharedPic>,
        window: ConformanceWindow,
        au_index: u32,
    ) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.pics.push(DpbPicture {
            id,
            poc,
            marking,
            needed_for_output,
            latency: 0,
            au_index,
            window,
            picture,
            meta: Arc::new(PictureMeta::default()),
        });
        id
    }

    /// Attach metadata to a buffered picture by id.
    pub fn set_meta(&mut self, id: u32, meta: PictureMeta) {
        if let Some(p) = self.pics.iter_mut().find(|p| p.id == id) {
            p.meta = Arc::new(meta);
        }
    }

    /// Drop every picture, without output.
    pub fn clear(&mut self) {
        self.pics.clear();
    }

    /// Drop pictures that are neither waiting for output nor used for reference (C.5.2.2).
    pub fn remove_unneeded(&mut self) {
        self.pics
            .retain(|p| p.needed_for_output || p.marking != Marking::Unused);
    }

    /// True when the bumping process must run (C.5.2.2 and C.5.2.3): too many
    /// pictures waiting for output, a picture past its latency limit, or, when
    /// `check_fullness` is set, a full buffer.
    pub fn needs_bumping(&self, limits: &DpbLimits, check_fullness: bool) -> bool {
        let waiting = self.pics.iter().filter(|p| p.needed_for_output).count();
        if waiting > limits.max_num_reorder {
            return true;
        }
        if limits.max_latency_plus1 != 0 {
            let max = limits.max_latency_pictures();
            if self
                .pics
                .iter()
                .any(|p| p.needed_for_output && u64::from(p.latency) >= max)
            {
                return true;
            }
        }
        check_fullness && self.pics.len() >= limits.max_dec_pic_buffering
    }

    /// One step of the bumping process (C.5.2.4): output the picture with the
    /// smallest POC among those waiting, and drop it if it is no longer used for
    /// reference. Returns false when nothing waits for output.
    pub fn bump(&mut self, out: &mut VecDeque<PendingFrame>) -> bool {
        let next = self
            .pics
            .iter()
            .enumerate()
            .filter(|(_, p)| p.needed_for_output)
            .min_by_key(|(_, p)| p.poc)
            .map(|(i, _)| i);
        let Some(idx) = next else {
            return false;
        };
        let pic = &mut self.pics[idx];
        pic.needed_for_output = false;
        out.push_back(PendingFrame {
            poc: pic.poc,
            au_index: pic.au_index,
            window: pic.window,
            picture: Arc::clone(&pic.picture),
            meta: Arc::clone(&pic.meta),
        });
        if pic.marking == Marking::Unused {
            self.pics.remove(idx);
        }
        true
    }

    /// Output every waiting picture in POC order, then empty the buffer.
    pub fn drain(&mut self, out: &mut VecDeque<PendingFrame>) {
        while self.bump(out) {}
        self.clear();
    }
}

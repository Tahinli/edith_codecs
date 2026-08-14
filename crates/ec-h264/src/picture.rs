//! The decoded picture and the neighbour derivations of clause 6.4.
//!
//! Everything the decoding process asks about a neighbour — "is it available",
//! "how many coefficients did it have", "what was its prediction mode" — is a
//! question about a *location*, so the state is kept in picture-wide grids
//! indexed by location rather than in per-macroblock structures. That is the
//! shape clause 6.4 is written in, and it makes the slice-boundary rules a
//! single comparison instead of a special case per caller.

use ec_core::error::{Error, Result};
use ec_core::frame::{PixelFormat, Plane, VideoFrame};

/// How a macroblock was coded, as far as the rest of the decoding process cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MbKind {
    /// `I_NxN` with `transform_size_8x8_flag` equal to 0.
    Intra4x4,
    /// One of `I_16x16_*`.
    Intra16x16,
    /// `I_PCM`.
    IPcm,
}

/// Per-macroblock state the neighbour derivations and the deblocking filter
/// read back.
#[derive(Debug, Clone, Copy)]
pub struct MbInfo {
    /// Index of the slice this macroblock belongs to, or -1 when it has not
    /// been decoded. Clause 6.4.8 availability is exactly equality of this
    /// value with the current slice's index.
    pub slice_id: i32,
    /// How the macroblock was coded.
    pub kind: MbKind,
    /// `QPY` of the macroblock, as the deblocking filter needs it.
    pub qpy: i32,
    /// `disable_deblocking_filter_idc` of the slice that coded it.
    pub disable_deblocking_filter_idc: u32,
    /// `FilterOffsetA` = `slice_alpha_c0_offset_div2 << 1`.
    pub filter_offset_a: i32,
    /// `FilterOffsetB` = `slice_beta_offset_div2 << 1`.
    pub filter_offset_b: i32,
}

impl Default for MbInfo {
    fn default() -> MbInfo {
        MbInfo {
            slice_id: -1,
            kind: MbKind::Intra4x4,
            qpy: 0,
            disable_deblocking_filter_idc: 0,
            filter_offset_a: 0,
            filter_offset_b: 0,
        }
    }
}

/// A decoded picture: three planes plus the per-location state of clause 6.4.
#[derive(Debug, Clone)]
pub struct Picture {
    /// `PicWidthInMbs`.
    pub width_mbs: usize,
    /// `PicHeightInMbs`.
    pub height_mbs: usize,
    /// Luma plane, `16 * width_mbs` bytes per row.
    pub luma: Vec<u8>,
    /// Cb plane, `8 * width_mbs` bytes per row.
    pub cb: Vec<u8>,
    /// Cr plane, `8 * width_mbs` bytes per row.
    pub cr: Vec<u8>,
    /// Per macroblock state, in raster order.
    pub mb: Vec<MbInfo>,
    /// Per 4x4 luma block: has the block been reconstructed yet? Intra
    /// prediction may only read samples of blocks that have.
    pub constructed: Vec<bool>,
    /// Per 4x4 luma block: `TotalCoeff` of its residual, for the `nC`
    /// prediction of clause 9.2.1.
    pub total_coeff_luma: Vec<u8>,
    /// Per 4x4 Cb block: `TotalCoeff`.
    pub total_coeff_cb: Vec<u8>,
    /// Per 4x4 Cr block: `TotalCoeff`.
    pub total_coeff_cr: Vec<u8>,
    /// Per 4x4 luma block: `Intra4x4PredMode`, or -1 when the macroblock is not
    /// coded in `Intra_4x4` (clause 8.3.1.1 then predicts DC).
    pub intra4x4_pred_mode: Vec<i8>,
}

impl Picture {
    /// An all-zero picture of `width_mbs` x `height_mbs` macroblocks.
    pub fn new(width_mbs: usize, height_mbs: usize) -> Picture {
        let mbs = width_mbs * height_mbs;
        Picture {
            width_mbs,
            height_mbs,
            luma: vec![0; mbs * 256],
            cb: vec![0; mbs * 64],
            cr: vec![0; mbs * 64],
            mb: vec![MbInfo::default(); mbs],
            constructed: vec![false; mbs * 16],
            total_coeff_luma: vec![0; mbs * 16],
            total_coeff_cb: vec![0; mbs * 4],
            total_coeff_cr: vec![0; mbs * 4],
            intra4x4_pred_mode: vec![-1; mbs * 16],
        }
    }

    /// Luma plane width in samples, `PicWidthInSamplesL`.
    pub fn luma_width(&self) -> usize {
        self.width_mbs * 16
    }

    /// Luma plane height in samples.
    pub fn luma_height(&self) -> usize {
        self.height_mbs * 16
    }

    /// Chroma plane width in samples, `PicWidthInSamplesC` for 4:2:0.
    pub fn chroma_width(&self) -> usize {
        self.width_mbs * 8
    }

    /// Chroma plane height in samples for 4:2:0.
    pub fn chroma_height(&self) -> usize {
        self.height_mbs * 8
    }

    /// Luma sample at `(x, y)`.
    pub fn luma_at(&self, x: usize, y: usize) -> u8 {
        self.luma[y * self.luma_width() + x]
    }

    /// Set the luma sample at `(x, y)`.
    pub fn set_luma(&mut self, x: usize, y: usize, value: u8) {
        let w = self.luma_width();
        self.luma[y * w + x] = value;
    }

    /// Chroma sample at `(x, y)` of component `i_cb_cr` (0 = Cb, 1 = Cr).
    pub fn chroma_at(&self, i_cb_cr: usize, x: usize, y: usize) -> u8 {
        let w = self.chroma_width();
        if i_cb_cr == 0 {
            self.cb[y * w + x]
        } else {
            self.cr[y * w + x]
        }
    }

    /// Set the chroma sample at `(x, y)` of component `i_cb_cr`.
    pub fn set_chroma(&mut self, i_cb_cr: usize, x: usize, y: usize, value: u8) {
        let w = self.chroma_width();
        if i_cb_cr == 0 {
            self.cb[y * w + x] = value;
        } else {
            self.cr[y * w + x] = value;
        }
    }

    /// Macroblock address of the macroblock at `(mb_x, mb_y)`.
    pub fn mb_addr(&self, mb_x: usize, mb_y: usize) -> usize {
        mb_y * self.width_mbs + mb_x
    }

    /// Index into the per-4x4-luma-block grids for the block containing luma
    /// sample `(x, y)`.
    pub fn luma_blk_index(&self, x: usize, y: usize) -> usize {
        (y / 4) * (self.width_mbs * 4) + x / 4
    }

    /// Index into the per-4x4-chroma-block grids for chroma sample `(x, y)`.
    pub fn chroma_blk_index(&self, x: usize, y: usize) -> usize {
        (y / 4) * (self.width_mbs * 2) + x / 4
    }

    /// Clause 6.4.8: is the macroblock containing luma sample `(x, y)`
    /// available to a macroblock of slice `slice_id`?
    ///
    /// Availability is "inside the picture, already decoded, and in the same
    /// slice" — and because slice ids are handed out in decoding order and this
    /// grid starts at -1, all three reduce to one comparison.
    pub fn mb_available_at(&self, x: isize, y: isize, slice_id: i32) -> bool {
        if x < 0 || y < 0 || x >= self.luma_width() as isize || y >= self.luma_height() as isize {
            return false;
        }
        let addr = self.mb_addr(x as usize / 16, y as usize / 16);
        self.mb[addr].slice_id == slice_id
    }

    /// Is luma sample `(x, y)` available for intra prediction in slice
    /// `slice_id`? Adds the "block already reconstructed" rule of clause 8.3.1.2
    /// to macroblock availability, which is what makes the above-right samples
    /// of a 4x4 block unavailable inside the current macroblock.
    pub fn luma_sample_available(&self, x: isize, y: isize, slice_id: i32) -> bool {
        self.mb_available_at(x, y, slice_id)
            && self.constructed[self.luma_blk_index(x as usize, y as usize)]
    }

    /// Reference the macroblock state at `(mb_x, mb_y)`.
    pub fn mb_at(&self, mb_x: usize, mb_y: usize) -> &MbInfo {
        &self.mb[self.mb_addr(mb_x, mb_y)]
    }

    /// Mark the 4x4 luma block whose top-left sample is `(x, y)` reconstructed.
    pub fn mark_constructed(&mut self, x: usize, y: usize) {
        let i = self.luma_blk_index(x, y);
        self.constructed[i] = true;
    }

    /// Crop to the cropping rectangle and hand the picture over as an I420
    /// [`VideoFrame`].
    ///
    /// The crop offsets are the ones the SPS states, already multiplied by
    /// `CropUnitX`/`CropUnitY`; a frame that is not a whole number of chroma
    /// samples wide would leave the two planes describing different rectangles,
    /// so it is refused rather than rounded.
    pub fn to_frame(
        &self,
        crop_left: usize,
        crop_top: usize,
        width: u32,
        height: u32,
    ) -> Result<VideoFrame> {
        if !crop_left.is_multiple_of(2) || !crop_top.is_multiple_of(2) {
            return Err(Error::corrupt(format!(
                "H.264: 4:2:0 cropping offset ({crop_left}, {crop_top}) is not a whole chroma sample"
            )));
        }
        let (w, h) = (width as usize, height as usize);
        if crop_left + w > self.luma_width() || crop_top + h > self.luma_height() {
            return Err(Error::corrupt(
                "H.264: cropping rectangle leaves the decoded picture",
            ));
        }
        let mut y_plane = Vec::with_capacity(w * h);
        for row in 0..h {
            let start = (crop_top + row) * self.luma_width() + crop_left;
            y_plane.extend_from_slice(&self.luma[start..start + w]);
        }
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let mut planes = vec![Plane::new(y_plane, w)];
        for source in [&self.cb, &self.cr] {
            let mut plane = Vec::with_capacity(cw * ch);
            for row in 0..ch {
                let start = (crop_top / 2 + row) * self.chroma_width() + crop_left / 2;
                plane.extend_from_slice(&source[start..start + cw]);
            }
            planes.push(Plane::new(plane, cw));
        }
        VideoFrame::try_new(PixelFormat::I420, width, height, planes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_follows_slice_and_construction() {
        let mut p = Picture::new(2, 2);
        // Nothing is decoded yet, so nothing is available.
        assert!(!p.mb_available_at(0, 0, 0));
        p.mb[0].slice_id = 0;
        assert!(p.mb_available_at(0, 0, 0));
        assert!(!p.mb_available_at(0, 0, 1), "a different slice");
        assert!(!p.mb_available_at(-1, 0, 0), "outside the picture");
        assert!(!p.mb_available_at(16, 0, 0), "the next macroblock");
        // Samples additionally need their 4x4 block reconstructed.
        assert!(!p.luma_sample_available(0, 0, 0));
        p.mark_constructed(0, 0);
        assert!(p.luma_sample_available(3, 3, 0));
        assert!(!p.luma_sample_available(4, 0, 0), "block to the right");
    }

    #[test]
    fn cropping_produces_the_visible_rectangle() {
        let mut p = Picture::new(2, 1); // 32x16
        for y in 0..16 {
            for x in 0..32 {
                p.set_luma(x, y, (x + y) as u8);
            }
        }
        for y in 0..8 {
            for x in 0..16 {
                p.set_chroma(0, x, y, x as u8);
                p.set_chroma(1, x, y, y as u8);
            }
        }
        let frame = p.to_frame(2, 4, 28, 10).unwrap();
        assert_eq!((frame.width, frame.height), (28, 10));
        assert_eq!(frame.planes[0].stride, 28);
        assert_eq!(frame.planes[0].data[0], 6, "luma (2, 4)");
        assert_eq!(frame.planes[1].data[0], 1, "Cb (1, 2)");
        assert_eq!(frame.planes[2].data[0], 2, "Cr (1, 2)");
        assert_eq!(frame.planes[1].data.len(), 14 * 5);
        assert!(p.to_frame(1, 0, 30, 16).is_err(), "odd chroma offset");
        assert!(p.to_frame(0, 0, 33, 16).is_err(), "outside the picture");
    }
}

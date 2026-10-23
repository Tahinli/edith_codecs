//! Intra reconstruction of one transform block into the picture (8.4.4.2 and
//! the sample sum of 8.6.7): the reference samples with their z-scan and
//! constrained-intra availability, the prediction, and the residual added and
//! clipped.
//!
//! The parser calls these in decoding order, so every neighbour that the
//! availability rules admit is already reconstructed when it is read.

use super::pic::PicState;
use crate::intra::{self, Availability, Refs};

impl PicState {
    /// Whether the sample of component `c` at `(xn, yn)` (component units) is
    /// available as an intra reference for the block at `(x, y)` of the same
    /// component (8.4.4.2.2): decoded already, and, with constrained intra
    /// prediction, an intra coding unit.
    fn intra_ref_available(&self, c: usize, x: usize, y: usize, xn: isize, yn: isize) -> bool {
        if xn < 0 || yn < 0 {
            return false;
        }
        let (sx, sy) = if c == 0 {
            (0, 0)
        } else {
            (self.sub_x(), self.sub_y())
        };
        let (xl, yl) = ((x << sx) as isize, (y << sy) as isize);
        let (xnl, ynl) = (xn << sx, yn << sy);
        if !self.available(xl as usize, yl as usize, xnl, ynl) {
            return false;
        }
        !self.constrained_intra_pred || self.info[self.unit(xnl as usize, ynl as usize)].intra
    }

    /// Predict the `1 << log2` square of component `c` at component position
    /// `(x, y)` with intra mode `mode`, writing the prediction into the plane.
    pub fn predict_intra(&mut self, c: usize, x: usize, y: usize, log2: u32, mode: u8) {
        self.predict_intra_ext(c, x, y, log2, mode, false);
    }

    /// [`PicState::predict_intra`] with `no_boundary` set for a transquant-bypass unit under
    /// implicit RDPCM (`disableIntraBoundaryFilter`, 8.4.4.2.6).
    pub fn predict_intra_ext(
        &mut self,
        c: usize,
        x: usize,
        y: usize,
        log2: u32,
        mode: u8,
        no_boundary: bool,
    ) {
        let n = 1usize << log2;
        let bd = self.bit_depth(c);
        let (stride, plane) = self.plane_view(c);
        let origin = self.plane_origin(c);
        let mut refs = Refs::<u16>::default();
        let mut avail = Availability::default();
        let at = |xn: usize, yn: usize| -> u16 { plane[(yn - origin) * stride + xn] };
        // Availability is constant over a 4x4 luma unit, so test once per unit (4 samples
        // of a luma plane; in a chroma plane 4 luma samples are 4 >> sub samples) and fill
        // the run. The left column steps by the vertical, the top row by the horizontal.
        let (sub_x, sub_y) = if c == 0 {
            (0, 0)
        } else {
            (self.sub_x() as usize, self.sub_y() as usize)
        };
        let step_left = (4usize >> sub_y).min(n);
        let step_top = (4usize >> sub_x).min(n);
        for k in (0..2 * n).step_by(step_left) {
            if self.intra_ref_available(c, x, y, x as isize - 1, (y + k) as isize) {
                for j in k..k + step_left {
                    avail.left |= 1 << j;
                    refs.left[j] = at(x - 1, y + j);
                }
            }
        }
        for k in (0..2 * n).step_by(step_top) {
            if self.intra_ref_available(c, x, y, (x + k) as isize, y as isize - 1) {
                for j in k..k + step_top {
                    avail.top |= 1 << j;
                    refs.top[j] = at(x + j, y - 1);
                }
            }
        }
        if self.intra_ref_available(c, x, y, x as isize - 1, y as isize - 1) {
            avail.corner = true;
            refs.corner = at(x - 1, y - 1);
        }
        intra::substitute(&mut refs, &avail, n, bd);
        let mut pred = vec![0u16; n * n];
        let flags = intra::PredFlags {
            smooth: (c == 0 || self.chroma_format_idc == 3) && !self.intra_smoothing_disabled,
            strong: c == 0 && self.strong_intra_smoothing,
            edge: c == 0,
            no_boundary,
        };
        intra::predict_ext(&refs, mode, n, flags, bd, &mut pred);
        let origin = self.plane_origin(c);
        let (stride, plane) = self.plane_view_mut(c);
        for j in 0..n {
            let row = (y + j - origin) * stride + x;
            plane[row..row + n].copy_from_slice(&pred[j * n..j * n + n]);
        }
    }

    /// Add a residual block to the prediction already in the plane, clipping
    /// to the sample range (8.6.7).
    pub fn add_residual(&mut self, c: usize, x: usize, y: usize, n: usize, residual: &[i32]) {
        let max = (1i32 << self.bit_depth(c)) - 1;
        let origin = self.plane_origin(c);
        let (stride, plane) = self.plane_view_mut(c);
        for j in 0..n {
            let start = (y + j - origin) * stride + x;
            let row = &mut plane[start..start + n];
            for (i, sample) in row.iter_mut().enumerate() {
                let v = i32::from(*sample) + residual[j * n + i];
                *sample = v.clamp(0, max) as u16;
            }
        }
    }

    /// Write `value` into every sample of an `n` x `n` block of component `c`.
    pub fn fill_plane(&mut self, c: usize, x: usize, y: usize, n: usize, value: u16) {
        let origin = self.plane_origin(c);
        let (stride, plane) = self.plane_view_mut(c);
        for j in 0..n {
            let start = (y + j - origin) * stride + x;
            plane[start..start + n].fill(value);
        }
    }

    /// The row stride of the chroma planes.
    fn chroma_stride(&self) -> usize {
        self.width.div_ceil(1 << self.sub_x())
    }

    /// The row stride and the samples of component `c`.
    fn plane_view(&self, c: usize) -> (usize, &[u16]) {
        match c {
            0 => (self.planes.width, &self.planes.y),
            1 => (self.chroma_stride(), &self.planes.cb),
            _ => (self.chroma_stride(), &self.planes.cr),
        }
    }

    /// Mutable form of [`PicState::plane_view`].
    pub(crate) fn plane_view_mut(&mut self, c: usize) -> (usize, &mut Vec<u16>) {
        match c {
            0 => (self.planes.width, &mut self.planes.y),
            1 => (self.chroma_stride(), &mut self.planes.cb),
            _ => (self.chroma_stride(), &mut self.planes.cr),
        }
    }
}

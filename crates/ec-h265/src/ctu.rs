//! Coding tree unit encoding: the quadtree, the mode decision, the transform
//! unit and the reconstruction (7.3.8.4 - 7.3.8.10).
//!
//! # The cost model
//!
//! Every decision minimises `J = SSD + lambda * bits`, where `SSD` is the sum of
//! squared differences between source and *reconstructed* samples and `bits` is
//! what the CABAC coder actually spends — measured by encoding the candidate
//! into the real coder and rolling it back, not estimated from a table.
//! `lambda = 0.57 * 2^((QP - 12) / 3)` is the usual intra Lagrangian; the 35-mode
//! pre-screen uses `sqrt(lambda)` against a Hadamard SATD, because SATD
//! estimates rate and distortion together where SSD estimates only distortion.
//!
//! # The shape of the tree
//!
//! Coding units are 32, 16 or 8 luma samples square, one transform unit per
//! coding unit, `PART_2Nx2N` only, chroma always in derived mode (`DM`). A 64x64
//! coding tree block therefore always writes one `split_cu_flag` of 1 and
//! decides freely below it. What that gives up is named rather than hidden:
//! `PART_NxN` with 4x4 luma transforms would hold fine detail at low QP, and a
//! 64x64 coding unit would save a few bits on flat sky. Both cost search time,
//! which this encoder spends on the wavefront that makes one `encode` call use
//! every core instead.

use crate::cabac::{CabacEncoder, CabacState, ctx};
use crate::intra::{self, Availability, Refs};
use crate::residual::encode_residual;
use crate::transform::{
    chroma_qp, dequantize, forward_transform, inverse_transform, quantize, uses_dst,
};
use std::sync::atomic::{AtomicU8, Ordering};

/// `log2` of the smallest coding block this encoder makes: 8x8.
pub const MIN_CB_LOG2: u32 = 3;

/// Rate-distortion lambda for an intra picture at `qp`.
pub fn lambda_for(qp: i32) -> f64 {
    0.57 * 2f64.powf((qp - 12) as f64 / 3.0)
}

/// The samples of one CTB row that the row below may predict from: its bottom
/// line, plus the coding depths along it.
///
/// This is the *entire* dependency between two wavefront rows. Intra prediction
/// reads one row above the block it predicts, and the only other thing a row
/// below asks about the row above is `CtDepth` for one context — so a worker
/// publishes a line per CTB and nothing else.
pub struct RowBoundary {
    /// Bottom luma line, one entry per column.
    pub y: Vec<AtomicU8>,
    /// Bottom Cb line.
    pub cb: Vec<AtomicU8>,
    /// Bottom Cr line.
    pub cr: Vec<AtomicU8>,
    /// `CtDepth` along the bottom, one entry per four columns.
    pub depth: Vec<AtomicU8>,
}

impl RowBoundary {
    /// Storage for a picture `width` luma samples wide.
    pub fn new(width: usize) -> RowBoundary {
        let make = |n: usize| (0..n).map(|_| AtomicU8::new(0)).collect();
        RowBoundary {
            y: make(width),
            cb: make(width / 2),
            cr: make(width / 2),
            depth: make(width / 4),
        }
    }
}

/// Source planes for one picture, already padded to the coded size.
pub struct SourcePlanes<'a> {
    /// Luma plane, `width * height`.
    pub y: &'a [u8],
    /// Cb plane, `width / 2 * height / 2`.
    pub cb: &'a [u8],
    /// Cr plane.
    pub cr: &'a [u8],
}

/// The mutable state one wavefront worker owns while coding one CTB row.
pub struct RowState<'a> {
    /// Reconstructed luma for this row band.
    pub rec_y: &'a mut [u8],
    /// Reconstructed Cb for this row band.
    pub rec_cb: &'a mut [u8],
    /// Reconstructed Cr for this row band.
    pub rec_cr: &'a mut [u8],
    /// The boundary this row publishes for the row below.
    pub publish: &'a RowBoundary,
    /// The boundary the row above published; absent for the first row.
    pub above: Option<&'a RowBoundary>,
}

/// Scratch buffers, allocated once per row rather than per block.
struct Scratch {
    pred: Vec<u8>,
    residual: Vec<i32>,
    coeffs: Vec<i32>,
    scaled: Vec<i32>,
    recon: Vec<u8>,
    best_recon: Vec<u8>,
    levels: Vec<i32>,
    best_levels: Vec<i32>,
    source: Vec<u8>,
    luma_levels: Vec<i32>,
    cb_levels: Vec<i32>,
    cr_levels: Vec<i32>,
}

impl Scratch {
    fn new() -> Scratch {
        Scratch {
            pred: vec![0; 32 * 32],
            residual: vec![0; 32 * 32],
            coeffs: vec![0; 32 * 32],
            scaled: vec![0; 32 * 32],
            recon: vec![0; 32 * 32],
            best_recon: vec![0; 32 * 32],
            levels: vec![0; 32 * 32],
            best_levels: vec![0; 32 * 32],
            source: vec![0; 32 * 32],
            luma_levels: vec![0; 32 * 32],
            cb_levels: vec![0; 16 * 16],
            cr_levels: vec![0; 16 * 16],
        }
    }
}

/// Encoder for one CTB row.
pub struct CtuEncoder<'a> {
    src: SourcePlanes<'a>,
    row: RowState<'a>,
    width: usize,
    height: usize,
    ctb_size: usize,
    band_y0: usize,
    band_rows: usize,
    qp: i32,
    qp_c: i32,
    lambda: f64,
    lambda_satd: f64,
    strong_smoothing: bool,
    /// How many of the 35 modes get a full rate-distortion trial.
    candidates: usize,
    /// Intra mode per 4x4 block of the band; 255 = not coded yet.
    modes: Vec<u8>,
    /// Coding depth per 4x4 block of the band.
    depths: Vec<u8>,
    /// Which 4x4 blocks of the *current* CTB are reconstructed.
    coded: [bool; 256],
    ctu_x: usize,
    /// Whether the coding unit just coded spent no residual at all — the cue
    /// that splitting it further would only spend flags.
    last_cu_empty: bool,
    scratch: Scratch,
}

impl<'a> CtuEncoder<'a> {
    /// A worker for the CTB row whose first luma row is `band_y0`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        src: SourcePlanes<'a>,
        row: RowState<'a>,
        width: usize,
        height: usize,
        ctb_size: usize,
        band_y0: usize,
        qp: i32,
        strong_smoothing: bool,
        candidates: usize,
    ) -> CtuEncoder<'a> {
        let band_rows = (height - band_y0).min(ctb_size);
        CtuEncoder {
            src,
            row,
            width,
            height,
            ctb_size,
            band_y0,
            band_rows,
            qp,
            qp_c: chroma_qp(qp, 0),
            lambda: lambda_for(qp),
            lambda_satd: lambda_for(qp).sqrt(),
            strong_smoothing,
            candidates: candidates.clamp(1, 35),
            modes: vec![255; (width / 4) * (ctb_size / 4)],
            depths: vec![0; (width / 4) * (ctb_size / 4)],
            coded: [false; 256],
            ctu_x: 0,
            last_cu_empty: false,
            scratch: Scratch::new(),
        }
    }

    /// Code one coding tree unit at CTB column `ctu_x`.
    pub fn encode_ctu(&mut self, ctu_x: usize, enc: &mut CabacEncoder) {
        self.ctu_x = ctu_x;
        self.coded = [false; 256];
        let log2_ctb = self.ctb_size.trailing_zeros();
        self.code_quadtree(ctu_x * self.ctb_size, self.band_y0, log2_ctb, 0, enc);
        self.publish_boundary(ctu_x);
    }

    /// Copy this CTB's bottom line into the boundary the next row reads.
    fn publish_boundary(&self, ctu_x: usize) {
        let last = self.band_y0 + self.band_rows - 1;
        let x0 = ctu_x * self.ctb_size;
        let x1 = (x0 + self.ctb_size).min(self.width);
        for x in x0..x1 {
            self.row.publish.y[x].store(self.rec_y(x, last), Ordering::Relaxed);
        }
        let cy = (self.band_y0 + self.band_rows) / 2 - 1;
        for x in x0 / 2..x1 / 2 {
            self.row.publish.cb[x].store(self.rec_c(x, cy, 1), Ordering::Relaxed);
            self.row.publish.cr[x].store(self.rec_c(x, cy, 2), Ordering::Relaxed);
        }
        for x in x0 / 4..x1 / 4 {
            let depth = self.depths[((self.band_rows - 1) / 4) * (self.width / 4) + x];
            self.row.publish.depth[x].store(depth, Ordering::Relaxed);
        }
    }

    fn rec_y(&self, x: usize, y: usize) -> u8 {
        if y < self.band_y0 {
            self.row
                .above
                .map_or(128, |above| above.y[x].load(Ordering::Relaxed))
        } else {
            self.row.rec_y[(y - self.band_y0) * self.width + x]
        }
    }

    /// Reconstructed chroma sample, in chroma coordinates.
    fn rec_c(&self, x: usize, y: usize, plane: usize) -> u8 {
        let band_c0 = self.band_y0 / 2;
        if y < band_c0 {
            self.row.above.map_or(128, |above| {
                let line = if plane == 1 { &above.cb } else { &above.cr };
                line[x].load(Ordering::Relaxed)
            })
        } else {
            let data = if plane == 1 {
                &*self.row.rec_cb
            } else {
                &*self.row.rec_cr
            };
            data[(y - band_c0) * (self.width / 2) + x]
        }
    }

    /// Availability of a luma position: inside the picture and already coded in
    /// raster CTB order, then z-scan order inside the current CTB.
    fn available(&self, x: isize, y: isize) -> bool {
        if x < 0 || y < 0 || x >= self.width as isize || y >= self.height as isize {
            return false;
        }
        let (x, y) = (x as usize, y as usize);
        if y < self.band_y0 {
            // Every CTB in a row above is coded. The wavefront guarantees the
            // ones a block can actually reach — at most one CTB to the right —
            // are also finished.
            debug_assert!(x < (self.ctu_x + 2) * self.ctb_size);
            return self.row.above.is_some();
        }
        if y >= self.band_y0 + self.band_rows {
            return false;
        }
        let cx = x / self.ctb_size;
        match cx.cmp(&self.ctu_x) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Equal => {
                let bx = (x - self.ctu_x * self.ctb_size) / 4;
                let by = (y - self.band_y0) / 4;
                self.coded[by * 16 + bx]
            }
        }
    }

    fn mode_at(&self, x: usize, y: usize) -> u8 {
        self.modes[((y - self.band_y0) / 4) * (self.width / 4) + x / 4]
    }

    fn depth_at(&self, x: usize, y: usize) -> u8 {
        if y < self.band_y0 {
            return self
                .row
                .above
                .map_or(0, |above| above.depth[x / 4].load(Ordering::Relaxed));
        }
        self.depths[((y - self.band_y0) / 4) * (self.width / 4) + x / 4]
    }

    // ---- the quadtree ----------------------------------------------------

    fn code_quadtree(
        &mut self,
        x: usize,
        y: usize,
        log2: u32,
        depth: u8,
        enc: &mut CabacEncoder,
    ) -> f64 {
        let size = 1usize << log2;
        let half = size / 2;
        if x + size > self.width || y + size > self.height {
            // split_cu_flag is inferred to 1 and only the children inside the
            // picture are coded (7.3.8.4).
            let mut cost = 0.0;
            for (dx, dy) in [(0, 0), (half, 0), (0, half), (half, half)] {
                if x + dx < self.width && y + dy < self.height {
                    cost += self.code_quadtree(x + dx, y + dy, log2 - 1, depth + 1, enc);
                }
            }
            return cost;
        }
        if log2 == MIN_CB_LOG2 {
            return self.code_cu(x, y, log2, depth, enc);
        }
        let split_ctx = ctx::SPLIT_CU
            + usize::from(
                self.available(x as isize - 1, y as isize) && self.depth_at(x - 1, y) > depth,
            )
            + usize::from(
                self.available(x as isize, y as isize - 1) && self.depth_at(x, y - 1) > depth,
            );
        if log2 > 5 {
            // 64x64 coding units are not searched; the decision starts at 32x32.
            let before = enc.bit_count();
            enc.encode_bin(split_ctx, 1);
            let mut cost = self.lambda * (enc.bit_count() - before) as f64;
            for (dx, dy) in [(0, 0), (half, 0), (0, half), (half, half)] {
                cost += self.code_quadtree(x + dx, y + dy, log2 - 1, depth + 1, enc);
            }
            return cost;
        }

        let start = enc.snapshot();
        // The z-scan availability map has to go back with everything else: the
        // leaf trial marks the whole region reconstructed, and a split trial run
        // against that map would predict from samples the *decoder* considers
        // unavailable and derive its contexts from them too. That is a
        // desynchronisation, not an inefficiency — it was worth a whole picture
        // of garbage before this line existed.
        let coded_before = self.coded;
        let before = enc.bit_count();
        enc.encode_bin(split_ctx, 0);
        let flag_bits = enc.bit_count() - before;
        let leaf_cost = self.lambda * flag_bits as f64 + self.code_cu(x, y, log2, depth, enc);
        if self.last_cu_empty {
            // Every coefficient quantised to zero: the four sub-trees can only
            // add split flags and mode bits on top of the same prediction, so
            // the split is not searched. This is where most of the picture is.
            return leaf_cost;
        }
        let leaf_state = enc.snapshot_since(&start);
        let leaf_rec = self.save_region(x, y, size);
        let leaf_meta = self.save_meta(x, y, size);

        enc.restore(&start);
        self.coded = coded_before;
        let before = enc.bit_count();
        enc.encode_bin(split_ctx, 1);
        let mut split_cost = self.lambda * (enc.bit_count() - before) as f64;
        for (dx, dy) in [(0, 0), (half, 0), (0, half), (half, half)] {
            split_cost += self.code_quadtree(x + dx, y + dy, log2 - 1, depth + 1, enc);
        }

        if leaf_cost <= split_cost {
            enc.restore(&leaf_state);
            self.load_region(x, y, size, &leaf_rec);
            self.load_meta(x, y, size, &leaf_meta);
            leaf_cost
        } else {
            split_cost
        }
    }

    /// Reconstructed samples of a square luma region and its chroma, packed.
    fn save_region(&self, x: usize, y: usize, size: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(size * size * 3 / 2);
        for yy in y..y + size {
            for xx in x..x + size {
                out.push(self.rec_y(xx, yy));
            }
        }
        for plane in [1usize, 2] {
            for yy in y / 2..y / 2 + size / 2 {
                for xx in x / 2..x / 2 + size / 2 {
                    out.push(self.rec_c(xx, yy, plane));
                }
            }
        }
        out
    }

    fn load_region(&mut self, x: usize, y: usize, size: usize, data: &[u8]) {
        let mut i = 0;
        for yy in y..y + size {
            for xx in x..x + size {
                self.write_rec_y(xx, yy, data[i]);
                i += 1;
            }
        }
        for plane in [1usize, 2] {
            for yy in y / 2..y / 2 + size / 2 {
                for xx in x / 2..x / 2 + size / 2 {
                    self.write_rec_c(xx, yy, plane, data[i]);
                    i += 1;
                }
            }
        }
    }

    fn save_meta(&self, x: usize, y: usize, size: usize) -> Vec<u8> {
        let stride = self.width / 4;
        let mut out = Vec::with_capacity(2 * (size / 4) * (size / 4));
        for by in (y..y + size).step_by(4) {
            for bx in (x..x + size).step_by(4) {
                let idx = ((by - self.band_y0) / 4) * stride + bx / 4;
                out.push(self.modes[idx]);
                out.push(self.depths[idx]);
            }
        }
        out
    }

    fn load_meta(&mut self, x: usize, y: usize, size: usize, data: &[u8]) {
        let stride = self.width / 4;
        let mut i = 0;
        for by in (y..y + size).step_by(4) {
            for bx in (x..x + size).step_by(4) {
                let idx = ((by - self.band_y0) / 4) * stride + bx / 4;
                self.modes[idx] = data[i];
                self.depths[idx] = data[i + 1];
                i += 2;
            }
        }
    }

    fn write_rec_y(&mut self, x: usize, y: usize, value: u8) {
        let stride = self.width;
        self.row.rec_y[(y - self.band_y0) * stride + x] = value;
    }

    fn write_rec_c(&mut self, x: usize, y: usize, plane: usize, value: u8) {
        let stride = self.width / 2;
        let band_c0 = self.band_y0 / 2;
        let data = if plane == 1 {
            &mut *self.row.rec_cb
        } else {
            &mut *self.row.rec_cr
        };
        data[(y - band_c0) * stride + x] = value;
    }

    // ---- one coding unit -------------------------------------------------

    /// Code one coding unit, returning `SSD + lambda * bits` for it. The split
    /// flag above it is the caller's to count.
    fn code_cu(&mut self, x: usize, y: usize, log2: u32, depth: u8, enc: &mut CabacEncoder) -> f64 {
        let n = 1usize << log2;
        let before = enc.bit_count();
        if log2 == MIN_CB_LOG2 {
            // part_mode: PART_2Nx2N is the single bin 1.
            enc.encode_bin(ctx::PART_MODE, 1);
        }
        let mpm = self.mpm_at(x, y);
        let mode = self.choose_luma_mode(x, y, n, log2, &mpm, enc);
        self.write_mode_syntax(enc, mode, &mpm);
        // intra_chroma_pred_mode = 4 (derived from luma): the single bin 0.
        enc.encode_bin(ctx::INTRA_CHROMA_PRED_MODE, 0);

        // Luma was reconstructed by the mode search; chroma is coded now.
        let luma_cbf = self.scratch.luma_levels[..n * n].iter().any(|&v| v != 0);
        let cb_cbf = self.code_chroma(x, y, n, mode, 1);
        let cr_cbf = self.code_chroma(x, y, n, mode, 2);

        enc.encode_bin(ctx::CBF_CHROMA, u32::from(cb_cbf));
        enc.encode_bin(ctx::CBF_CHROMA, u32::from(cr_cbf));
        enc.encode_bin(ctx::CBF_LUMA + 1, u32::from(luma_cbf));
        if luma_cbf {
            let scan = intra::scan_index(mode, log2, true);
            let levels = std::mem::take(&mut self.scratch.luma_levels);
            encode_residual(enc, &levels[..n * n], log2, 0, scan);
            self.scratch.luma_levels = levels;
        }
        let chroma_scan = intra::scan_index(mode, log2 - 1, false);
        if cb_cbf {
            let levels = std::mem::take(&mut self.scratch.cb_levels);
            encode_residual(enc, &levels[..n * n / 4], log2 - 1, 1, chroma_scan);
            self.scratch.cb_levels = levels;
        }
        if cr_cbf {
            let levels = std::mem::take(&mut self.scratch.cr_levels);
            encode_residual(enc, &levels[..n * n / 4], log2 - 1, 2, chroma_scan);
            self.scratch.cr_levels = levels;
        }

        self.mark_coded(x, y, n, mode, depth);
        self.last_cu_empty = !(luma_cbf || cb_cbf || cr_cbf);
        let bits = enc.bit_count() - before;
        self.region_ssd(x, y, n) + self.lambda * bits as f64
    }

    fn write_mode_syntax(&self, enc: &mut CabacEncoder, mode: u8, mpm: &[u8; 3]) {
        match mpm.iter().position(|&m| m == mode) {
            Some(idx) => {
                enc.encode_bin(ctx::PREV_INTRA_LUMA_PRED, 1);
                // mpm_idx: truncated rice, cMax 2, all bypass.
                if idx == 0 {
                    enc.encode_bypass(0);
                } else {
                    enc.encode_bypass(1);
                    enc.encode_bypass(u32::from(idx == 2));
                }
            }
            None => {
                enc.encode_bin(ctx::PREV_INTRA_LUMA_PRED, 0);
                let mut sorted = *mpm;
                sorted.sort_unstable();
                let mut rem = mode;
                for &m in sorted.iter().rev() {
                    if mode > m {
                        rem -= 1;
                    }
                }
                enc.encode_bypass_bits(u32::from(rem), 5);
            }
        }
    }

    fn mpm_at(&self, x: usize, y: usize) -> [u8; 3] {
        let left = if self.available(x as isize - 1, y as isize) {
            let m = self.mode_at(x - 1, y);
            if m == 255 { None } else { Some(m) }
        } else {
            None
        };
        // A neighbour in the CTB row above counts as DC (8.4.2 step 2), which is
        // also what keeps the wavefront from needing a mode line.
        let above = if y > self.band_y0 && self.available(x as isize, y as isize - 1) {
            let m = self.mode_at(x, y - 1);
            if m == 255 { None } else { Some(m) }
        } else {
            None
        };
        intra::mpm_list(left, above)
    }

    fn mark_coded(&mut self, x: usize, y: usize, n: usize, mode: u8, depth: u8) {
        let stride = self.width / 4;
        for by in (y..y + n).step_by(4) {
            for bx in (x..x + n).step_by(4) {
                let idx = ((by - self.band_y0) / 4) * stride + bx / 4;
                self.modes[idx] = mode;
                self.depths[idx] = depth;
                let cx = (bx - self.ctu_x * self.ctb_size) / 4;
                let cy = (by - self.band_y0) / 4;
                self.coded[cy * 16 + cx] = true;
            }
        }
    }

    /// Sum of squared differences over a coding unit, all three planes.
    fn region_ssd(&self, x: usize, y: usize, n: usize) -> f64 {
        let mut ssd = 0i64;
        for yy in y..y + n {
            let src_row = &self.src.y[yy * self.width..yy * self.width + self.width];
            for xx in x..x + n {
                let d = i64::from(src_row[xx]) - i64::from(self.rec_y(xx, yy));
                ssd += d * d;
            }
        }
        let cw = self.width / 2;
        for plane in [1usize, 2] {
            let src = if plane == 1 { self.src.cb } else { self.src.cr };
            for yy in y / 2..y / 2 + n / 2 {
                for xx in x / 2..x / 2 + n / 2 {
                    let d = i64::from(src[yy * cw + xx]) - i64::from(self.rec_c(xx, yy, plane));
                    ssd += d * d;
                }
            }
        }
        ssd as f64
    }

    // ---- luma mode decision ----------------------------------------------

    /// Pick the luma mode, leaving its levels in `scratch.luma_levels` and its
    /// reconstruction written into the band.
    fn choose_luma_mode(
        &mut self,
        x: usize,
        y: usize,
        n: usize,
        log2: u32,
        mpm: &[u8; 3],
        enc: &mut CabacEncoder,
    ) -> u8 {
        let refs = self.luma_refs(x, y, n);
        let mut source = std::mem::take(&mut self.scratch.source);
        for row in 0..n {
            let sy = y + row;
            source[row * n..row * n + n]
                .copy_from_slice(&self.src.y[sy * self.width + x..sy * self.width + x + n]);
        }

        // Pre-screen: planar, DC and every second angle, then the two
        // neighbours of whichever angle won. Nineteen predictions instead of
        // thirty-five for the same winner in all but a rounding of cases —
        // the SATD surface over the angles is smooth by construction.
        let mut scored: Vec<(f64, u8)> = Vec::with_capacity(24);
        let score = |this: &mut Self, mode: u8, scored: &mut Vec<(f64, u8)>| {
            intra::predict(
                &refs,
                mode,
                n,
                true,
                this.strong_smoothing,
                &mut this.scratch.pred,
            );
            let satd = satd(&source, &this.scratch.pred[..n * n], n) as f64;
            let mode_bits = if mpm.contains(&mode) { 2.0 } else { 6.0 };
            scored.push((satd + this.lambda_satd * mode_bits, mode));
        };
        for mode in [0u8, 1] {
            score(self, mode, &mut scored);
        }
        for mode in (2u8..=34).step_by(2) {
            score(self, mode, &mut scored);
        }
        let coarse_best = scored
            .iter()
            .filter(|(_, m)| *m >= 2)
            .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
            .map(|&(_, m)| m)
            .unwrap_or(26);
        for delta in [-1i16, 1] {
            let refined = coarse_best as i16 + delta;
            if (2..=34).contains(&refined) {
                score(self, refined as u8, &mut scored);
            }
        }
        scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        // Full rate-distortion on the survivors, plus the most probable modes,
        // which are cheap to code and often win despite a worse SATD.
        let mut candidates: Vec<u8> = scored.iter().take(self.candidates).map(|s| s.1).collect();
        // The first most probable mode joins the trial whatever its SATD: it is
        // two bins where a non-MPM mode is six, and that gap decides plenty of
        // flat blocks.
        if !candidates.contains(&mpm[0]) {
            candidates.push(mpm[0]);
        }

        let start = enc.snapshot();
        enc.set_counting(true);
        let mut best = (f64::MAX, 0u8);
        let mut best_levels = std::mem::take(&mut self.scratch.best_levels);
        for &mode in &candidates {
            let bits_before = enc.bit_count();
            self.write_mode_syntax(enc, mode, mpm);
            let (ssd, cbf) = self.transform_luma(n, mode, &source, &refs);
            enc.encode_bin(ctx::CBF_LUMA + 1, u32::from(cbf));
            if cbf {
                let scan = intra::scan_index(mode, log2, true);
                let levels = std::mem::take(&mut self.scratch.levels);
                encode_residual(enc, &levels[..n * n], log2, 0, scan);
                self.scratch.levels = levels;
            }
            let bits = enc.bit_count() - bits_before;
            enc.restore(&start);
            let cost = ssd + self.lambda * bits as f64;
            if cost < best.0 {
                best = (cost, mode);
                best_levels[..n * n].copy_from_slice(&self.scratch.levels[..n * n]);
                self.scratch.best_recon[..n * n].copy_from_slice(&self.scratch.recon[..n * n]);
            }
        }
        enc.set_counting(false);
        // Commit the winner's reconstruction and levels.
        for row in 0..n {
            for col in 0..n {
                self.write_rec_y(x + col, y + row, self.scratch.best_recon[row * n + col]);
            }
        }
        self.scratch.luma_levels[..n * n].copy_from_slice(&best_levels[..n * n]);
        self.scratch.best_levels = best_levels;
        self.scratch.source = source;
        best.1
    }

    /// Predict, transform, quantise and reconstruct one luma block into
    /// `scratch.recon` / `scratch.levels`; returns `(SSD, cbf)`.
    fn transform_luma(&mut self, n: usize, mode: u8, source: &[u8], refs: &Refs) -> (f64, bool) {
        intra::predict(
            refs,
            mode,
            n,
            true,
            self.strong_smoothing,
            &mut self.scratch.pred,
        );
        for i in 0..n * n {
            self.scratch.residual[i] = i32::from(source[i]) - i32::from(self.scratch.pred[i]);
        }
        let dst = uses_dst(n, true);
        forward_transform(&self.scratch.residual, &mut self.scratch.coeffs, n, dst);
        let nonzero = quantize(&self.scratch.coeffs, &mut self.scratch.levels, n, self.qp);
        let cbf = nonzero > 0;
        if cbf {
            dequantize(&self.scratch.levels, &mut self.scratch.scaled, n, self.qp);
            inverse_transform(&self.scratch.scaled, &mut self.scratch.residual, n, dst);
        } else {
            self.scratch.residual[..n * n].fill(0);
        }
        let mut ssd = 0i64;
        for i in 0..n * n {
            let value =
                (i32::from(self.scratch.pred[i]) + self.scratch.residual[i]).clamp(0, 255) as u8;
            self.scratch.recon[i] = value;
            let d = i64::from(source[i]) - i64::from(value);
            ssd += d * d;
        }
        (ssd as f64, cbf)
    }

    /// Code one chroma transform block in derived mode; returns its `cbf` and
    /// leaves the levels in the matching scratch buffer.
    fn code_chroma(&mut self, x: usize, y: usize, n: usize, luma_mode: u8, plane: usize) -> bool {
        let cn = n / 2;
        let (cx, cy) = (x / 2, y / 2);
        let refs = self.chroma_refs(cx, cy, cn, plane);
        let cw = self.width / 2;
        let src = if plane == 1 { self.src.cb } else { self.src.cr };
        intra::predict(
            &refs,
            luma_mode,
            cn,
            false,
            self.strong_smoothing,
            &mut self.scratch.pred,
        );
        for row in 0..cn {
            for col in 0..cn {
                let s = i32::from(src[(cy + row) * cw + cx + col]);
                self.scratch.residual[row * cn + col] =
                    s - i32::from(self.scratch.pred[row * cn + col]);
            }
        }
        forward_transform(&self.scratch.residual, &mut self.scratch.coeffs, cn, false);
        let nonzero = quantize(&self.scratch.coeffs, &mut self.scratch.levels, cn, self.qp_c);
        let cbf = nonzero > 0;
        if cbf {
            dequantize(&self.scratch.levels, &mut self.scratch.scaled, cn, self.qp_c);
            inverse_transform(&self.scratch.scaled, &mut self.scratch.residual, cn, false);
        } else {
            self.scratch.residual[..cn * cn].fill(0);
        }
        for row in 0..cn {
            for col in 0..cn {
                let value = (i32::from(self.scratch.pred[row * cn + col])
                    + self.scratch.residual[row * cn + col])
                    .clamp(0, 255) as u8;
                self.write_rec_c(cx + col, cy + row, plane, value);
            }
        }
        let levels = if plane == 1 {
            &mut self.scratch.cb_levels
        } else {
            &mut self.scratch.cr_levels
        };
        levels[..cn * cn].copy_from_slice(&self.scratch.levels[..cn * cn]);
        cbf
    }

    // ---- reference samples -----------------------------------------------

    fn luma_refs(&self, x: usize, y: usize, n: usize) -> Refs {
        let mut refs = Refs::default();
        let mut avail = Availability::default();
        let span = 2 * n;
        avail.corner = self.available(x as isize - 1, y as isize - 1);
        if avail.corner {
            refs.corner = self.rec_y(x - 1, y - 1);
        }
        for i in 0..span {
            if self.available((x + i) as isize, y as isize - 1) {
                avail.top |= 1 << i;
                refs.top[i] = self.rec_y(x + i, y - 1);
            }
            if self.available(x as isize - 1, (y + i) as isize) {
                avail.left |= 1 << i;
                refs.left[i] = self.rec_y(x - 1, y + i);
            }
        }
        intra::substitute(&mut refs, &avail, n, 8);
        refs
    }

    fn chroma_refs(&self, cx: usize, cy: usize, cn: usize, plane: usize) -> Refs {
        let mut refs = Refs::default();
        let mut avail = Availability::default();
        let span = 2 * cn;
        avail.corner = self.available(cx as isize * 2 - 1, cy as isize * 2 - 1);
        if avail.corner {
            refs.corner = self.rec_c(cx - 1, cy - 1, plane);
        }
        for i in 0..span {
            if self.available(((cx + i) * 2) as isize, cy as isize * 2 - 1) {
                avail.top |= 1 << i;
                refs.top[i] = self.rec_c(cx + i, cy - 1, plane);
            }
            if self.available(cx as isize * 2 - 1, ((cy + i) * 2) as isize) {
                avail.left |= 1 << i;
                refs.left[i] = self.rec_c(cx - 1, cy + i, plane);
            }
        }
        intra::substitute(&mut refs, &avail, cn, 8);
        refs
    }
}

/// Sum of absolute transformed differences, 4x4 Hadamard per sub-block.
fn satd(source: &[u8], pred: &[u8], n: usize) -> u32 {
    let mut total = 0u32;
    for by in (0..n).step_by(4) {
        for bx in (0..n).step_by(4) {
            let mut block = [0i32; 16];
            for row in 0..4 {
                for col in 0..4 {
                    let idx = (by + row) * n + bx + col;
                    block[row * 4 + col] = i32::from(source[idx]) - i32::from(pred[idx]);
                }
            }
            // Rows, then columns.
            for row in 0..4 {
                let b = &mut block[row * 4..row * 4 + 4];
                let (s0, s1, s2, s3) = (b[0] + b[2], b[1] + b[3], b[0] - b[2], b[1] - b[3]);
                b[0] = s0 + s1;
                b[1] = s0 - s1;
                b[2] = s2 + s3;
                b[3] = s2 - s3;
            }
            for col in 0..4 {
                let (a, b2, c, d) = (
                    block[col],
                    block[4 + col],
                    block[8 + col],
                    block[12 + col],
                );
                let (s0, s1, s2, s3) = (a + c, b2 + d, a - c, b2 - d);
                total += (s0 + s1).unsigned_abs();
                total += (s0 - s1).unsigned_abs();
                total += (s2 + s3).unsigned_abs();
                total += (s2 - s3).unsigned_abs();
            }
        }
    }
    (total + 1) / 2
}

/// A snapshot type alias so callers do not need the cabac module for rollback.
pub type EncoderState = CabacState;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn satd_is_zero_for_an_exact_prediction_and_grows_with_error() {
        let source: Vec<u8> = (0..64).map(|i| (i * 3 % 251) as u8).collect();
        assert_eq!(satd(&source, &source, 8), 0);
        let mut worse = source.clone();
        worse[9] = worse[9].wrapping_add(40);
        assert!(satd(&source, &worse, 8) > 0);
        // A constant offset shows up entirely in the DC term.
        let shifted: Vec<u8> = source.iter().map(|v| v.saturating_sub(10)).collect();
        assert!(satd(&source, &shifted, 8) >= 8 * 8 * 10 / 2);
    }

    #[test]
    fn lambda_grows_with_qp() {
        assert!(lambda_for(37) > lambda_for(27));
        assert!((lambda_for(12) - 0.57).abs() < 1e-9);
    }
}

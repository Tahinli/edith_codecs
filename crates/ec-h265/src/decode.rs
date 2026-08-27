//! Intra-only HEVC decode: the mirror of [`crate::ctu`], minus the
//! rate-distortion search.
//!
//! # Module map
//!
//! - [`cabac::CabacDecoder`](crate::cabac::CabacDecoder) mirrors
//!   [`CabacEncoder`](crate::cabac::CabacEncoder) bin for bin, sharing its
//!   context tables (`RANGE_TAB_LPS`, `TRANS_IDX_LPS`/`MPS`, [`Contexts`]).
//! - [`residual::decode_residual`](crate::residual::decode_residual) mirrors
//!   `encode_residual` bin for bin.
//! - [`crate::intra`] and [`crate::transform`] are reused as-is: prediction,
//!   the inverse transform and dequantisation do not care which direction the
//!   bits came from.
//! - This module supplies what the encoder's [`crate::ctu::CtuEncoder`] does
//!   *not* have a decode-shaped twin for: the coding-tree and transform-tree
//!   walk, which the encoder interleaves with a mode search but a decoder
//!   just reads.
//!
//! # What is genuinely a search heuristic vs. genuine syntax
//!
//! `code_quadtree`'s special-cased branches for `min_cu_size` and `cu64`, and
//! `code_cu`'s `intra_nxn` gate, all still write a real `split_cu_flag` /
//! `part_mode` bit — they only fix *which* value the search settles for
//! without trying the alternative. A decoder does not need to know any of
//! those encoder config knobs: it reads the bit that is there. Only
//! `EncoderConfig::rqt`, `sign_hiding` and `transform_skip` change whether a
//! bit is *present* at all (mirroring `max_transform_hierarchy_depth_intra`,
//! `sign_data_hiding_enabled_flag`, `transform_skip_enabled_flag`), so those
//! three travel from the parsed SPS/PPS into [`CtuDecoder`].

use crate::cabac::{CabacDecoder, Contexts, ctx};
use crate::ctu::MIN_CB_LOG2;
use crate::deblock::{TuMap, deblock};
use crate::intra::{self, Availability, Refs};
use crate::residual::decode_residual;
use crate::transform::{
    chroma_qp, dequantize, inverse_transform, inverse_transform_skip, uses_dst,
};
use ec_core::error::{Error, Result};
use ec_h265_syntax::nal::split_annex_b;
use ec_h265_syntax::slice::{SliceHeader, SliceType};
use ec_h265_syntax::{Pps, Sps};

/// A decoded picture: three planes at the SPS's *coded* size (before the
/// conformance window crops it), 8-bit, one byte per sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedPicture {
    /// Coded width.
    pub width: usize,
    /// Coded height.
    pub height: usize,
    /// Luma, `width * height`.
    pub y: Vec<u8>,
    /// Cb, `width/2 * height/2`.
    pub cb: Vec<u8>,
    /// Cr, `width/2 * height/2`.
    pub cr: Vec<u8>,
}

/// Decode one IDR access unit: VPS/SPS/PPS/slice NALs in Annex-B framing, one
/// I slice covering the whole picture. Anything but an intra slice is refused
/// with a named reason rather than silently mis-decoded — inter prediction is
/// not implemented yet (see the module doc for what is).
pub fn decode_idr_au(data: &[u8]) -> Result<DecodedPicture> {
    let mut sps: Option<Sps> = None;
    let mut pps: Option<Pps> = None;
    for nal in split_annex_b(data) {
        match nal.header.nal_type {
            ec_h265_syntax::nal::NalUnitType::Sps => sps = Some(Sps::parse(&nal.rbsp())?),
            ec_h265_syntax::nal::NalUnitType::Pps => pps = Some(Pps::parse(&nal.rbsp())?),
            ec_h265_syntax::nal::NalUnitType::IdrWRadl
            | ec_h265_syntax::nal::NalUnitType::IdrNLp => {
                let sps = sps.as_ref().ok_or_else(|| {
                    Error::corrupt("HEVC decode: slice before an SPS".to_string())
                })?;
                let pps = pps
                    .as_ref()
                    .ok_or_else(|| Error::corrupt("HEVC decode: slice before a PPS".to_string()))?;
                let rbsp = nal.rbsp();
                let (header, pos) = SliceHeader::parse(&rbsp, sps, pps, nal.header.nal_type)?;
                if header.slice_type != SliceType::I {
                    return Err(Error::unsupported(
                        "HEVC decode: inter slice",
                        "no inter prediction implemented yet; only IDR I-slice streams decode",
                    ));
                }
                let byte_off = (pos.header_bits / 8) as usize;
                return decode_i_slice(sps, pps, &header, &rbsp[byte_off..]);
            }
            _ => {}
        }
    }
    Err(Error::corrupt(
        "HEVC decode: no IDR slice found in the access unit".to_string(),
    ))
}

fn decode_i_slice(
    sps: &Sps,
    pps: &Pps,
    header: &SliceHeader,
    slice_data: &[u8],
) -> Result<DecodedPicture> {
    // The coded picture, not the CTB-grid-rounded size: HEVC's last CTU in a
    // row/column is legally partial (7.3.8.4's boundary-forced split exists
    // precisely for this), and the encoder's own boundary checks are against
    // `pic_width_in_luma_samples`/`pic_height_in_luma_samples`, not the next
    // multiple of the CTB size. Rounding up here desynchronised the decoder's
    // split_cu_flag inference from the encoder's whenever the two bases
    // differed — every size that is not itself an exact CTB-size multiple.
    let width = sps.pic_width_in_luma_samples as usize;
    let height = sps.pic_height_in_luma_samples as usize;
    let ctb = sps.ctb_size() as usize;
    let cols = width.div_ceil(ctb);
    let rows = height.div_ceil(ctb);
    let qp = 26 + pps.init_qp_minus26 + header.qp_delta;
    let rqt = sps.max_transform_hierarchy_depth_intra > 0;

    let mut decoder = CtuDecoder::new(
        width,
        height,
        ctb,
        qp,
        rqt,
        pps.sign_data_hiding_enabled,
        pps.transform_skip_enabled,
        sps.strong_intra_smoothing,
    );

    let mut wpp_contexts: Vec<Option<Contexts>> = vec![None; rows];
    let mut byte_pos = 0usize;
    'rows: for row in 0..rows {
        let contexts = if row == 0 || cols < 2 {
            Contexts::new(qp)
        } else {
            wpp_contexts[row - 1]
                .clone()
                .unwrap_or_else(|| Contexts::new(qp))
        };
        if byte_pos > slice_data.len() {
            return Err(Error::corrupt(
                "HEVC decode: slice data ran out before the last CTB row".to_string(),
            ));
        }
        let mut dec = CabacDecoder::new(&slice_data[byte_pos..], contexts);
        for col in 0..cols {
            decoder.decode_ctu(col * ctb, row * ctb, &mut dec);
            if col == 1 {
                wpp_contexts[row] = Some(Contexts::clone(&dec.contexts));
            }
            let last_in_picture = row + 1 == rows && col + 1 == cols;
            let term = dec.decode_terminate();
            if term != u32::from(last_in_picture) {
                return Err(Error::corrupt(format!(
                    "HEVC decode: end_of_slice_segment_flag {term} at CTB ({col},{row}), expected {}",
                    u32::from(last_in_picture)
                )));
            }
            if last_in_picture {
                break 'rows;
            }
            if col + 1 == cols {
                let subset_end = dec.decode_terminate();
                if subset_end != 1 {
                    return Err(Error::corrupt(
                        "HEVC decode: end_of_subset_one_bit was not set at a WPP row boundary"
                            .to_string(),
                    ));
                }
            }
        }
        byte_pos += dec.byte_position_aligned();
    }

    if !header.deblocking_filter_disabled {
        let mut tus = TuMap::new(width, height);
        tus.absorb_band(0, height / 4, &decoder.tu_log2);
        deblock(
            &mut decoder.y_buf,
            &mut decoder.cb_buf,
            &mut decoder.cr_buf,
            width,
            height,
            qp,
            &tus,
        );
    }

    Ok(DecodedPicture {
        width,
        height,
        y: decoder.y_buf,
        cb: decoder.cb_buf,
        cr: decoder.cr_buf,
    })
}

/// Decoder state for one picture: the reconstruction, and the per-4x4-unit
/// bookkeeping [`crate::ctu::CtuEncoder`] keeps per row-band, kept here for
/// the whole picture instead since a decoder has no wavefront to shard by.
struct CtuDecoder {
    width: usize,
    height: usize,
    ctb_size: usize,
    qp: i32,
    qp_c: i32,
    rqt: bool,
    sign_hiding: bool,
    transform_skip_enabled: bool,
    strong_smoothing: bool,
    y_buf: Vec<u8>,
    cb_buf: Vec<u8>,
    cr_buf: Vec<u8>,
    /// Whether each 4x4 luma unit has been reconstructed yet, in the same
    /// raster-CTB-then-z-scan order the encoder's `available()` assumes. One
    /// picture-wide flag serves every purpose the encoder split across a
    /// row-boundary struct and a per-CTB reset: a single-threaded decoder
    /// visits positions in exactly the order that made those two exist.
    decoded: Vec<bool>,
    modes: Vec<u8>,
    depths: Vec<u8>,
    tu_log2: Vec<u8>,
}

impl CtuDecoder {
    #[allow(clippy::too_many_arguments)]
    fn new(
        width: usize,
        height: usize,
        ctb_size: usize,
        qp: i32,
        rqt: bool,
        sign_hiding: bool,
        transform_skip_enabled: bool,
        strong_smoothing: bool,
    ) -> CtuDecoder {
        let units = (width / 4) * (height / 4);
        CtuDecoder {
            width,
            height,
            ctb_size,
            qp,
            qp_c: chroma_qp(qp, 0),
            rqt,
            sign_hiding,
            transform_skip_enabled,
            strong_smoothing,
            y_buf: vec![0; width * height],
            cb_buf: vec![0; width / 2 * height / 2],
            cr_buf: vec![0; width / 2 * height / 2],
            decoded: vec![false; units],
            modes: vec![255; units],
            depths: vec![0; units],
            tu_log2: vec![2; units],
        }
    }

    fn available(&self, x: isize, y: isize) -> bool {
        if x < 0 || y < 0 || x >= self.width as isize || y >= self.height as isize {
            return false;
        }
        self.decoded[(y as usize / 4) * (self.width / 4) + x as usize / 4]
    }

    fn mode_at(&self, x: usize, y: usize) -> u8 {
        self.modes[(y / 4) * (self.width / 4) + x / 4]
    }

    fn depth_at(&self, x: usize, y: usize) -> u8 {
        self.depths[(y / 4) * (self.width / 4) + x / 4]
    }

    /// Record a reconstructed region: mode, coding depth and availability for
    /// every 4x4 unit it covers.
    fn mark_coded(&mut self, x: usize, y: usize, n: usize, mode: u8, depth: u8) {
        let stride = self.width / 4;
        for by in (y..y + n).step_by(4) {
            for bx in (x..x + n).step_by(4) {
                let idx = (by / 4) * stride + bx / 4;
                self.modes[idx] = mode;
                self.depths[idx] = depth;
                self.decoded[idx] = true;
            }
        }
    }

    fn mark_tu(&mut self, x: usize, y: usize, n: usize) {
        let stride = self.width / 4;
        let log2 = n.trailing_zeros() as u8;
        for by in (y..y + n).step_by(4) {
            for bx in (x..x + n).step_by(4) {
                self.tu_log2[(by / 4) * stride + bx / 4] = log2;
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
        let band_y0 = (y / self.ctb_size) * self.ctb_size;
        let above = if y > band_y0 && self.available(x as isize, y as isize - 1) {
            let m = self.mode_at(x, y - 1);
            if m == 255 { None } else { Some(m) }
        } else {
            None
        };
        intra::mpm_list(left, above)
    }

    fn luma_refs(&self, x: usize, y: usize, n: usize) -> Refs {
        let mut refs = Refs::default();
        let mut avail = Availability::default();
        let span = 2 * n;
        avail.corner = self.available(x as isize - 1, y as isize - 1);
        if avail.corner {
            refs.corner = self.y_buf[(y - 1) * self.width + x - 1];
        }
        for i in 0..span {
            if self.available((x + i) as isize, y as isize - 1) {
                avail.top |= 1 << i;
                refs.top[i] = self.y_buf[(y - 1) * self.width + x + i];
            }
            if self.available(x as isize - 1, (y + i) as isize) {
                avail.left |= 1 << i;
                refs.left[i] = self.y_buf[(y + i) * self.width + x - 1];
            }
        }
        intra::substitute(&mut refs, &avail, n, 8);
        refs
    }

    fn chroma_refs(&self, cx: usize, cy: usize, cn: usize, plane: &[u8]) -> Refs {
        let mut refs = Refs::default();
        let mut avail = Availability::default();
        let cw = self.width / 2;
        let span = 2 * cn;
        avail.corner = self.available(cx as isize * 2 - 1, cy as isize * 2 - 1);
        if avail.corner {
            refs.corner = plane[(cy - 1) * cw + cx - 1];
        }
        for i in 0..span {
            if self.available(((cx + i) * 2) as isize, cy as isize * 2 - 1) {
                avail.top |= 1 << i;
                refs.top[i] = plane[(cy - 1) * cw + cx + i];
            }
            if self.available(cx as isize * 2 - 1, ((cy + i) * 2) as isize) {
                avail.left |= 1 << i;
                refs.left[i] = plane[(cy + i) * cw + cx - 1];
            }
        }
        intra::substitute(&mut refs, &avail, cn, 8);
        refs
    }

    // ---- one coding tree unit ---------------------------------------------

    fn decode_ctu(&mut self, x0: usize, y0: usize, dec: &mut CabacDecoder) {
        let log2_ctb = self.ctb_size.trailing_zeros();
        self.decode_quadtree(x0, y0, log2_ctb, 0, dec);
    }

    fn decode_quadtree(
        &mut self,
        x: usize,
        y: usize,
        log2: u32,
        depth: u8,
        dec: &mut CabacDecoder,
    ) {
        let size = 1usize << log2;
        let half = size / 2;
        if x + size > self.width || y + size > self.height {
            for (dx, dy) in [(0, 0), (half, 0), (0, half), (half, half)] {
                if x + dx < self.width && y + dy < self.height {
                    self.decode_quadtree(x + dx, y + dy, log2 - 1, depth + 1, dec);
                }
            }
            return;
        }
        if log2 == MIN_CB_LOG2 {
            self.decode_cu(x, y, log2, depth, dec);
            return;
        }
        let split_ctx = ctx::SPLIT_CU
            + usize::from(
                self.available(x as isize - 1, y as isize) && self.depth_at(x - 1, y) > depth,
            )
            + usize::from(
                self.available(x as isize, y as isize - 1) && self.depth_at(x, y - 1) > depth,
            );
        if dec.decode_bin(split_ctx) != 0 {
            for (dx, dy) in [(0, 0), (half, 0), (0, half), (half, half)] {
                self.decode_quadtree(x + dx, y + dy, log2 - 1, depth + 1, dec);
            }
        } else {
            self.decode_cu(x, y, log2, depth, dec);
        }
    }

    fn decode_cu(&mut self, x: usize, y: usize, log2: u32, depth: u8, dec: &mut CabacDecoder) {
        if log2 == MIN_CB_LOG2 {
            if dec.decode_bin(ctx::PART_MODE) != 0 {
                self.decode_cu_2nx2n(x, y, log2, depth, dec);
            } else {
                self.decode_cu_nxn(x, y, depth, dec);
            }
        } else {
            self.decode_cu_2nx2n(x, y, log2, depth, dec);
        }
    }

    fn decode_intra_mode(&self, dec: &mut CabacDecoder, mpm: &[u8; 3]) -> u8 {
        let is_mpm = dec.decode_bin(ctx::PREV_INTRA_LUMA_PRED) != 0;
        if is_mpm {
            let idx = if dec.decode_bypass() == 0 {
                0
            } else {
                1 + dec.decode_bypass()
            };
            mpm[idx as usize]
        } else {
            let rem = dec.decode_bypass_bits(5) as u8;
            let mut sorted = *mpm;
            sorted.sort_unstable();
            let mut mode = rem;
            for m in sorted {
                if mode >= m {
                    mode += 1;
                }
            }
            mode
        }
    }

    fn decode_chroma_mode(&self, dec: &mut CabacDecoder, luma_mode: u8) -> u8 {
        if dec.decode_bin(ctx::INTRA_CHROMA_PRED_MODE) == 0 {
            return luma_mode;
        }
        let idx = dec.decode_bypass_bits(2) as usize;
        let mut explicit = [0u8, 26, 10, 1];
        for m in explicit.iter_mut() {
            if *m == luma_mode {
                *m = 34;
            }
        }
        explicit[idx]
    }

    fn decode_cu_2nx2n(
        &mut self,
        x: usize,
        y: usize,
        log2: u32,
        depth: u8,
        dec: &mut CabacDecoder,
    ) {
        let n = 1usize << log2;
        let mpm = self.mpm_at(x, y);
        let mode = self.decode_intra_mode(dec, &mpm);
        let chroma_mode = self.decode_chroma_mode(dec, mode);
        if log2 > 5 {
            self.decode_split_tu(x, y, n, log2, mode, chroma_mode, depth, dec);
        } else if self.rqt && log2 > 2 {
            let split_ctx = ctx::SPLIT_TRANSFORM + (5 - log2) as usize;
            if dec.decode_bin(split_ctx) != 0 {
                self.decode_split_tu(x, y, n, log2, mode, chroma_mode, depth, dec);
            } else {
                self.decode_single_tu(x, y, n, log2, mode, chroma_mode, dec);
            }
        } else {
            self.decode_single_tu(x, y, n, log2, mode, chroma_mode, dec);
        }
        self.mark_coded(x, y, n, mode, depth);
    }

    /// The four-4x4-partition form of the smallest coding unit.
    fn decode_cu_nxn(&mut self, x: usize, y: usize, depth: u8, dec: &mut CabacDecoder) {
        let offsets = [(0usize, 0usize), (4, 0), (0, 4), (4, 4)];
        // prev_intra_luma_pred_flag for all four, then every remainder — the
        // batching the encoder's two separate loops produce; only the second
        // loop's decode needs `mpm`, and by the time it reaches partition i
        // partitions 0..i already have a resolved mode in `self.modes` (this
        // loop resolves both flag and remainder together per partition, so
        // that still holds).
        let is_mpm: [bool; 4] =
            std::array::from_fn(|_| dec.decode_bin(ctx::PREV_INTRA_LUMA_PRED) != 0);
        let mut modes = [0u8; 4];
        for (part, &(dx, dy)) in offsets.iter().enumerate() {
            let (px, py) = (x + dx, y + dy);
            let mpm = self.mpm_at(px, py);
            let mode = if is_mpm[part] {
                let idx = if dec.decode_bypass() == 0 {
                    0
                } else {
                    1 + dec.decode_bypass()
                };
                mpm[idx as usize]
            } else {
                let rem = dec.decode_bypass_bits(5) as u8;
                let mut sorted = mpm;
                sorted.sort_unstable();
                let mut mode = rem;
                for m in sorted {
                    if mode >= m {
                        mode += 1;
                    }
                }
                mode
            };
            modes[part] = mode;
            // Mode only, so the next partition's MPM sees it; pixels follow
            // once the residual syntax below is read.
            let stride = self.width / 4;
            self.modes[(py / 4) * stride + px / 4] = mode;
        }

        let chroma_mode = self.decode_chroma_mode(dec, modes[0]);
        let cb_cbf = dec.decode_bin(ctx::CBF_CHROMA) != 0;
        let cr_cbf = dec.decode_bin(ctx::CBF_CHROMA) != 0;

        for (part, &(dx, dy)) in offsets.iter().enumerate() {
            let (px, py) = (x + dx, y + dy);
            let luma_cbf = dec.decode_bin(ctx::CBF_LUMA) != 0;
            self.reconstruct_luma(px, py, 4, modes[part], 2, luma_cbf, dec);
            self.mark_coded(px, py, 4, modes[part], depth);
            self.mark_tu(px, py, 4);
            if part == 3 {
                // `reconstruct_chroma`'s `log2_luma` is the covering luma
                // block's log2 (it derives `clog2 = log2_luma - 1` for the
                // transform-split convention); an NxN CU's covering block is
                // the whole 8x8 CU (`MIN_CB_LOG2`), not a 4x4 partition —
                // passing `2` here derived `clog2 = 1` and underflowed
                // `decode_residual`'s `log2_size - 2`.
                self.reconstruct_chroma(x, y, 8, chroma_mode, MIN_CB_LOG2, cb_cbf, cr_cbf, dec);
            }
        }
    }

    /// Predict, decode residual if `cbf`, reconstruct one luma transform
    /// block of side `n` at `(x, y)` and write it into `y_buf`.
    fn reconstruct_luma(
        &mut self,
        x: usize,
        y: usize,
        n: usize,
        mode: u8,
        log2: u32,
        cbf: bool,
        dec: &mut CabacDecoder,
    ) {
        let refs = self.luma_refs(x, y, n);
        let mut pred = vec![0u8; n * n];
        intra::predict(&refs, mode, n, true, self.strong_smoothing, &mut pred);
        let mut residual = vec![0i32; n * n];
        if cbf {
            let scan = intra::scan_index(mode, log2, true);
            let mut levels = vec![0i32; n * n];
            let skip = decode_residual(
                dec,
                &mut levels,
                log2,
                0,
                scan,
                self.sign_hiding,
                self.transform_skip_enabled,
            );
            let mut scaled = vec![0i32; n * n];
            dequantize(&levels, &mut scaled, n, self.qp);
            if skip {
                inverse_transform_skip(&scaled, &mut residual, n);
            } else {
                inverse_transform(&scaled, &mut residual, n, uses_dst(n, true));
            }
        }
        for row in 0..n {
            for col in 0..n {
                let i = row * n + col;
                let v = (i32::from(pred[i]) + residual[i]).clamp(0, 255) as u8;
                self.y_buf[(y + row) * self.width + x + col] = v;
            }
        }
    }

    /// The chroma pair (Cb then Cr) of one transform block of side `n`
    /// (luma-equivalent side; the chroma block is `n/2`).
    fn reconstruct_chroma(
        &mut self,
        x: usize,
        y: usize,
        n: usize,
        mode: u8,
        log2_luma: u32,
        cb_cbf: bool,
        cr_cbf: bool,
        dec: &mut CabacDecoder,
    ) {
        let cn = n / 2;
        let (cx, cy) = (x / 2, y / 2);
        let clog2 = log2_luma - 1;
        let scan = intra::scan_index(mode, clog2, false);
        for (plane_idx, cbf) in [(1usize, cb_cbf), (2usize, cr_cbf)] {
            let cw = self.width / 2;
            let refs = {
                let plane = if plane_idx == 1 {
                    &self.cb_buf
                } else {
                    &self.cr_buf
                };
                self.chroma_refs(cx, cy, cn, plane)
            };
            let mut pred = vec![0u8; cn * cn];
            intra::predict(&refs, mode, cn, false, self.strong_smoothing, &mut pred);
            let mut residual = vec![0i32; cn * cn];
            if cbf {
                let mut levels = vec![0i32; cn * cn];
                let skip = decode_residual(
                    dec,
                    &mut levels,
                    clog2,
                    plane_idx,
                    scan,
                    self.sign_hiding,
                    self.transform_skip_enabled,
                );
                let mut scaled = vec![0i32; cn * cn];
                dequantize(&levels, &mut scaled, cn, self.qp_c);
                if skip {
                    inverse_transform_skip(&scaled, &mut residual, cn);
                } else {
                    inverse_transform(&scaled, &mut residual, cn, false);
                }
            }
            let plane = if plane_idx == 1 {
                &mut self.cb_buf
            } else {
                &mut self.cr_buf
            };
            for row in 0..cn {
                for col in 0..cn {
                    let i = row * cn + col;
                    let v = (i32::from(pred[i]) + residual[i]).clamp(0, 255) as u8;
                    plane[(cy + row) * cw + cx + col] = v;
                }
            }
        }
    }

    /// One unsplit transform tree: `cbf_cb`, `cbf_cr`, `cbf_luma` (context 1,
    /// `trafoDepth == 0`), then the residuals.
    fn decode_single_tu(
        &mut self,
        x: usize,
        y: usize,
        n: usize,
        log2: u32,
        mode: u8,
        chroma_mode: u8,
        dec: &mut CabacDecoder,
    ) {
        let cb_cbf = dec.decode_bin(ctx::CBF_CHROMA) != 0;
        let cr_cbf = dec.decode_bin(ctx::CBF_CHROMA) != 0;
        let luma_cbf = dec.decode_bin(ctx::CBF_LUMA + 1) != 0;
        self.reconstruct_luma(x, y, n, mode, log2, luma_cbf, dec);
        self.reconstruct_chroma(x, y, n, chroma_mode, log2, cb_cbf, cr_cbf, dec);
        self.mark_tu(x, y, n);
    }

    /// A split transform tree: the parent's chroma flags, then four
    /// half-size children (`cbf_luma` context 0, `trafoDepth >= 1`). A child
    /// carries its own chroma when the chroma blocks stay 4x4 or larger;
    /// below that the parent's chroma rides on the last child.
    #[allow(clippy::too_many_arguments)]
    fn decode_split_tu(
        &mut self,
        x: usize,
        y: usize,
        n: usize,
        log2: u32,
        mode: u8,
        chroma_mode: u8,
        depth: u8,
        dec: &mut CabacDecoder,
    ) {
        let half = n / 2;
        let cb_cbf = dec.decode_bin(ctx::CBF_CHROMA) != 0;
        let cr_cbf = dec.decode_bin(ctx::CBF_CHROMA) != 0;
        let chroma_split = log2 > 3;
        let offsets = [(0usize, 0usize), (half, 0), (0, half), (half, half)];
        for (part, &(dx, dy)) in offsets.iter().enumerate() {
            let (cx, cy) = (x + dx, y + dy);
            let (child_cb, child_cr) = if chroma_split {
                (
                    cb_cbf && dec.decode_bin(ctx::CBF_CHROMA + 1) != 0,
                    cr_cbf && dec.decode_bin(ctx::CBF_CHROMA + 1) != 0,
                )
            } else {
                (false, false)
            };
            let child_luma_cbf = dec.decode_bin(ctx::CBF_LUMA) != 0;
            self.reconstruct_luma(cx, cy, half, mode, log2 - 1, child_luma_cbf, dec);
            // The next child predicts from this one, and (below the 8x8
            // luma-tree case) so does its own chroma.
            self.mark_coded(cx, cy, half, mode, depth);
            self.mark_tu(cx, cy, half);
            if chroma_split {
                self.reconstruct_chroma(
                    cx,
                    cy,
                    half,
                    chroma_mode,
                    log2 - 1,
                    child_cb,
                    child_cr,
                    dec,
                );
            } else if part == 3 {
                self.reconstruct_chroma(x, y, n, chroma_mode, log2, cb_cbf, cr_cbf, dec);
            }
        }
    }
}

//! Frame decoding: per-row mode parsing, token decoding, intra
//! prediction and reconstruction (RFC 6386 §5, §11, §12, §13, §14).
//!
//! The decoder mirrors the reference decoder's row pipeline: per
//! macroblock row, all first-partition mode records are parsed, then
//! each macroblock's tokens are decoded from its row-assigned token
//! partition (row `r` uses partition `r % partitions`, exactly like
//! libvpx/dixie) and immediately reconstructed. The loop filter runs as
//! a post-pass in raster MB order — for a non-threaded decode this is
//! edge-for-edge identical to the reference's row-delayed filtering,
//! because an MB's filter inputs only ever come from MBs already
//! filtered earlier in that order.

use crate::bool::BoolDecoder;
use crate::frame::{FrameType, KeyFrameDims, mb_geometry};
use crate::header::FrameHeader;
use crate::intra;
use crate::loopfilter::{self, MbFilterInfo};
use crate::modes;
use crate::tables::KF_BMODE_PROB;
use crate::tokens::{self, Dq, MbCoeffs, TokenContexts};
use crate::transform;
use crate::transform::Dequant;
use crate::{Error, PersistedState, Result};

/// Reconstructed-image plane: a padded buffer with a 1-pixel left/top
/// border (127 on top including the corner, 129 on the left — RFC 6386
/// §12) so intra prediction reads neighbours without special cases.
struct Plane {
    data: Vec<u8>,
    /// Row pitch: `1 + mb_cols * mb_size`.
    stride: usize,
}

impl Plane {
    fn new(mb_cols: usize, mb_rows: usize, mb_size: usize) -> Self {
        let stride = 1 + mb_cols * mb_size;
        let rows = 1 + mb_rows * mb_size;
        let mut data = vec![0u8; stride * rows];
        for cell in data[..stride].iter_mut() {
            *cell = 127;
        }
        for row in 1..rows {
            data[row * stride] = 129;
        }
        Plane { data, stride }
    }

    fn resize(&mut self, mb_cols: usize, mb_rows: usize, mb_size: usize) {
        *self = Plane::new(mb_cols, mb_rows, mb_size);
    }

    /// Offset of pixel (x, y); the border occupies x = -1 / y = -1.
    fn at(&self, x: usize, y: usize) -> usize {
        (1 + y) * self.stride + 1 + x
    }

    /// Offset of the pixel row ABOVE pixel row `y` (the border row for
    /// y = 0), at pixel column `x`.
    fn at_above(&self, x: usize, y: usize) -> usize {
        y * self.stride + 1 + x
    }

    /// Offset of the pixel column LEFT of pixel column `x` (the border
    /// column for x = 0), at pixel row `y`.
    fn at_left(&self, x: usize, y: usize) -> usize {
        (1 + y) * self.stride + x
    }

    /// Offset of the above-left pixel (border corner for x = y = 0).
    fn at_above_left(&self, x: usize, y: usize) -> usize {
        y * self.stride + x
    }
}

/// One macroblock's parsed prediction record.
#[derive(Clone, Default)]
struct MbInfo {
    /// [`modes::YMode`] as its coded integer.
    y_mode: u8,
    uv_mode: u8,
    b_modes: [u8; 16],
    segment_id: u8,
    skip: bool,
    /// Any non-zero coefficient decoded for this MB (bit 31 of dixie's
    /// eob_mask); decides the loop filter's interior edges (§15.1).
    has_nz: bool,
}

impl MbInfo {
    fn has_y2(&self) -> bool {
        self.y_mode != modes::YMode::BPred.as_u8()
    }
}

/// The VP8 decoder. Feed complete frames (one full VP8 frame payload,
/// exactly what a container demuxer hands over for the `VP80`/`vp08`
/// codec).
pub struct Decoder {
    state: PersistedState,
    dims: KeyFrameDims,
    mb_cols: usize,
    mb_rows: usize,
    y: Plane,
    u: Plane,
    v: Plane,
    /// `(mb_rows + 1) * (mb_cols + 1)` records; row/column index 0 is the
    /// DC_PRED border (dixie's `mb_info` border trick).
    mb_info: Vec<MbInfo>,
    token_ctxs: TokenContexts,
    frame_decoded: bool,
}

/// A decoded, displayable frame: cropped contiguous planes.
pub struct Picture {
    /// Luma plane, `stride`-spaced rows of `width` pixels.
    pub y: Vec<u8>,
    /// U (Cb) plane, half resolution.
    pub u: Vec<u8>,
    /// V (Cr) plane, half resolution.
    pub v: Vec<u8>,
    pub width: u16,
    pub height: u16,
    /// Luma row pitch of the returned planes.
    pub stride: usize,
    /// Chroma row pitch of the returned planes.
    pub uv_stride: usize,
}

impl Decoder {
    /// Create an empty decoder; the first key frame sets the size.
    pub fn new() -> Self {
        Self {
            state: PersistedState::default(),
            dims: KeyFrameDims {
                width: 0,
                height: 0,
                h_scale: 0,
                v_scale: 0,
            },
            mb_cols: 0,
            mb_rows: 0,
            y: Plane {
                data: Vec::new(),
                stride: 0,
            },
            u: Plane {
                data: Vec::new(),
                stride: 0,
            },
            v: Plane {
                data: Vec::new(),
                stride: 0,
            },
            mb_info: Vec::new(),
            token_ctxs: TokenContexts::new(0),
            frame_decoded: false,
        }
    }

    /// Size of the frames this decoder produces, once known.
    pub fn dimensions(&self) -> Option<(u16, u16)> {
        self.frame_decoded
            .then_some((self.dims.width, self.dims.height))
    }

    /// Decode one frame; returns the picture when it is for display
    /// (`show_frame`). Hidden frames still update the decode state.
    pub fn decode(&mut self, frame: &[u8]) -> Result<Option<Picture>> {
        let (header, _part0) = FrameHeader::parse(frame, &mut self.state)?;
        match header.tag.frame_type {
            FrameType::Key => self.decode_keyframe(frame, header),
            FrameType::Inter => Err(Error::unsupported(
                "VP8 inter frame",
                "inter-frame decoding is the next milestone of this crate",
            )),
        }
    }

    fn ensure_buffers(&mut self, kf: KeyFrameDims) {
        let (mb_cols, mb_rows) = mb_geometry(kf.width, kf.height);
        if self.mb_cols != mb_cols || self.mb_rows != mb_rows {
            self.y.resize(mb_cols, mb_rows, 16);
            self.u.resize(mb_cols, mb_rows, 8);
            self.v.resize(mb_cols, mb_rows, 8);
            self.mb_info = vec![MbInfo::default(); (mb_cols + 1) * (mb_rows + 1)];
            self.token_ctxs = TokenContexts::new(mb_cols);
        }
        self.mb_cols = mb_cols;
        self.mb_rows = mb_rows;
        self.dims = kf;
        // Reset the mode border (row 0 / column 0) so the top-left mode
        // contexts read DC_PRED like a fresh decode.
        for col in 0..=mb_cols {
            self.mb_info[col] = MbInfo::default();
        }
        for row in 0..=mb_rows {
            self.mb_info[row * (mb_cols + 1)] = MbInfo::default();
        }
    }

    /// Mode-record index for MB (row, col); row/col 0 is the border.
    fn mbi(&self, row: usize, col: usize) -> &MbInfo {
        &self.mb_info[row * (self.mb_cols + 1) + col]
    }

    fn mbi_mut(&mut self, row: usize, col: usize) -> &mut MbInfo {
        let w = self.mb_cols + 1;
        &mut self.mb_info[row * w + col]
    }

    fn decode_keyframe(&mut self, frame: &[u8], header: FrameHeader) -> Result<Option<Picture>> {
        let kf = header.dims.expect("key frame header carries dimensions");
        self.ensure_buffers(kf);

        // Token partition bool decoders (§9.5).
        let mut partitions = Vec::with_capacity(header.partition_sizes.len());
        let mut off = header.token_data_offset + 3 * (header.partition_sizes.len() - 1);
        for &sz in &header.partition_sizes {
            let end = off + sz as usize;
            let slice = frame
                .get(off..end)
                .ok_or_else(|| Error::corrupt("VP8 token partition extends past frame end"))?;
            partitions.push(BoolDecoder::new(slice)?);
            off = end;
        }

        // Dequant factors per segment (dixie dequant_init, §10/§14.1).
        let seg = &header.segmentation;
        let mut dqf = [Dequant {
            y1_dc: 0,
            y1_ac: 0,
            y2_dc: 0,
            y2_ac: 0,
            uv_dc: 0,
            uv_ac: 0,
        }; 4];
        for (i, dq) in dqf.iter_mut().enumerate() {
            let q = if seg.enabled {
                if seg.abs_delta {
                    seg.quant_idx[i]
                } else {
                    i32::from(header.quant.yac_qi) + seg.quant_idx[i]
                }
            } else {
                i32::from(header.quant.yac_qi)
            };
            *dq = transform::dequant(
                q,
                header.quant.ydc_delta,
                header.quant.y2dc_delta,
                header.quant.y2ac_delta,
                header.quant.uvdc_delta,
                header.quant.uvac_delta,
            );
        }

        let hdr_start = crate::frame::FRAME_TAG_SZ + crate::frame::KEYFRAME_HEADER_SZ;
        let mut hdr_dec = BoolDecoder::new(&frame[hdr_start..header.token_data_offset])?;

        self.token_ctxs.reset_above_row();

        for row in 1..=self.mb_rows {
            self.token_ctxs.reset_left();
            // Phase 1: parse every mode record of the row (partition 0).
            for col in 1..=self.mb_cols {
                let mut mb = MbInfo::default();
                if header.segmentation.enabled && header.segmentation.update_map {
                    mb.segment_id =
                        hdr_dec.read_tree(&SEGMENT_TREE, &header.segmentation.tree_probs);
                }
                if header.coeff_skip_enabled {
                    mb.skip = hdr_dec.read_bool(header.prob_skip_false);
                }
                self.decode_kf_modes(&mut hdr_dec, row, col, &mut mb);
                *self.mbi_mut(row, col) = mb;
            }

            // Phases 2+3 per MB: tokens then reconstruction. Token row
            // `r` (0-based) reads partition `r % n` like the reference.
            let part = (row - 1) % partitions.len();
            for col in 1..=self.mb_cols {
                let mb = self.mbi(row, col).clone();
                let has_y2 = mb.has_y2();
                let seg_i = usize::from(header.segmentation.enabled) * usize::from(mb.segment_id);
                let dq = &dqf[seg_i];
                let mb_dq = Dq {
                    y1_dc: dq.y1_dc,
                    y1_ac: dq.y1_ac,
                    uv_dc: dq.uv_dc,
                    uv_ac: dq.uv_ac,
                    y2_dc: dq.y2_dc,
                    y2_ac: dq.y2_ac,
                };
                let probs = self.state.coeff_probs;
                let coeffs = if mb.skip {
                    tokens::skip_mb_tokens(&mut self.token_ctxs, col - 1, has_y2)
                } else {
                    tokens::decode_mb_tokens(
                        &mut partitions[part],
                        &probs,
                        &mb_dq,
                        has_y2,
                        &mut self.token_ctxs,
                        col - 1,
                    )
                };
                self.mb_info[row * (self.mb_cols + 1) + col].has_nz = coeffs.has_nonzero();
                self.reconstruct_intra_mb(row - 1, col - 1, &mb, &coeffs);
            }
        }

        // Loop filter post-pass (see module docs for the ordering
        // equivalence argument).
        if header.filter_level > 0 {
            let infos: Vec<MbFilterInfo> = (0..self.mb_rows)
                .flat_map(|r| (0..self.mb_cols).map(move |c| (r, c)))
                .map(|(r, c)| {
                    let mb = self.mbi(r + 1, c + 1);
                    MbFilterInfo {
                        level: self.mb_filter_level(&header, mb),
                        inner_edges: mb.has_nz || !mb.has_y2(),
                    }
                })
                .collect();
            // The planes are bordered; the filter sees the unbordered
            // image (same row pitch) starting at pixel (0, 0).
            let (ystride, ustride, vstride) = (self.y.stride, self.u.stride, self.v.stride);
            let (mcols, mrows) = (self.mb_cols, self.mb_rows);
            let simple = header.filter_type == 1;
            let sharpness = header.sharpness_level;
            let yimg = &mut self.y.data[1 + ystride..];
            let uimg = &mut self.u.data[1 + ustride..];
            let vimg = &mut self.v.data[1 + vstride..];
            loopfilter::filter_frame_ex(
                yimg, uimg, vimg, ystride, ustride, mcols, mrows, sharpness, simple, &infos, true,
            );
        }

        self.frame_decoded = true;
        Ok(header.tag.show_frame.then(|| self.crop()))
    }

    /// Per-MB loop filter level (dixie `calculate_filter_parameters`):
    /// segment adjustment, then ref/mode deltas, clamped 0..63. Key
    /// frames are all intra — ref index 0 (`CURRENT_FRAME`), with
    /// `mode_delta[0]` applying to B_PRED.
    fn mb_filter_level(&self, header: &FrameHeader, mb: &MbInfo) -> u8 {
        let seg = &header.segmentation;
        let mut level = i32::from(header.filter_level);
        if seg.enabled {
            let d = seg.lf_level[usize::from(mb.segment_id)];
            level = if seg.abs_delta { d } else { level + d };
        }
        level = level.clamp(0, 63);
        if header.lf_delta_enabled {
            level += self.state.ref_lf_delta[0];
            if mb.y_mode == modes::YMode::BPred.as_u8() {
                level += self.state.mode_lf_delta[0];
            }
            level = level.clamp(0, 63);
        }
        level as u8
    }

    /// Key-frame mode record (§11): Y mode at fixed probabilities, then
    /// 16 context-conditioned subblock modes for B_PRED, then chroma.
    fn decode_kf_modes(
        &mut self,
        d: &mut BoolDecoder<'_>,
        row: usize,
        col: usize,
        mb: &mut MbInfo,
    ) {
        let ymode = modes::read_kf_ymode(d);
        mb.y_mode = ymode.as_u8();
        if ymode == modes::YMode::BPred {
            for j in 0..16u8 {
                let a = self.above_block_mode(row, col, j, mb);
                let l = self.left_block_mode(row, col, j, mb);
                mb.b_modes[usize::from(j)] =
                    modes::read_bmode(d, &KF_BMODE_PROB[usize::from(a)][usize::from(l)]).as_u8();
            }
        }
        mb.uv_mode = modes::read_uvmode(d, &modes::KF_UV_MODE_PROBS).as_u8();
    }

    /// Above subblock mode context (dixie `above_block_mode`): subblocks
    /// 0-3 take the above MB's derived mode, inner subblocks the mode
    /// `j-4` of this same MB (already decoded into `mb`).
    fn above_block_mode(&self, row: usize, col: usize, j: u8, mb: &MbInfo) -> u8 {
        if j < 4 {
            let above = self.mbi(row - 1, col);
            if above.y_mode == modes::YMode::BPred.as_u8() {
                above.b_modes[usize::from(j) + 12]
            } else {
                ymode_as_bmode(above.y_mode)
            }
        } else {
            mb.b_modes[usize::from(j) - 4]
        }
    }

    /// Left subblock mode context (dixie `left_block_mode`).
    fn left_block_mode(&self, row: usize, col: usize, j: u8, mb: &MbInfo) -> u8 {
        if j % 4 == 0 {
            let left = self.mbi(row, col - 1);
            if left.y_mode == modes::YMode::BPred.as_u8() {
                left.b_modes[usize::from(j) + 3]
            } else {
                ymode_as_bmode(left.y_mode)
            }
        } else {
            mb.b_modes[usize::from(j) - 1]
        }
    }

    /// Predict and reconstruct one intra key-frame MB at pixel (x, y).
    ///
    /// Whole-MB modes predict once from the pre-MB edges and then fold
    /// the 24 residual blocks in; B_PRED interleaves: each 4x4 subblock
    /// predicts from the reconstructed image (which already carries the
    /// earlier subblocks' residuals, §12.3) and is written back before
    /// the next subblock is predicted.
    fn reconstruct_intra_mb(&mut self, row: usize, col: usize, mb: &MbInfo, coeffs: &MbCoeffs) {
        let x = col * 16;
        let y = row * 16;
        let y_mode = mb.y_mode;
        let uv_mode = mb.uv_mode;
        let b_modes = mb.b_modes;
        let have_above = row > 0;
        let have_left = col > 0;

        // The Y2 inverse WHT feeds the 16 Y subblock DCs (§14.2).
        let y_dcs: [i16; 16] = if coeffs.has_y2 {
            transform::iwht4x4(&coeffs.y2)
        } else {
            [0; 16]
        };
        if std::env::var_os("EC_VP8_DEBUG").is_some() {
            eprintln!(
                "MB({row},{col}) ymode={} uv={} y2={:?} y2has={} y0[:4]={:?} eobs24={} dcs={:?}",
                y_mode,
                uv_mode,
                coeffs.y2,
                coeffs.has_y2,
                &coeffs.y[0][..4],
                coeffs.eobs[24],
                &y_dcs[..4]
            );
        }

        if y_mode == modes::YMode::BPred.as_u8() {
            for b in 0..16usize {
                let bx = (b % 4) * 4;
                let by = (b / 4) * 4;
                // A row: 4 above pixels then 4 above-right (§12.3); the
                // above-right extras are shared by right-edge subblocks
                // 3, 7, 11 and 15.
                let mut a8 = [0u8; 8];
                let arow = self.y.at_above(x + bx, y + by);
                a8[..4].copy_from_slice(&self.y.data[arow..arow + 4]);
                if bx + 4 == 16 {
                    a8[4..].copy_from_slice(&self.above_right(x, y));
                } else {
                    let ar = self.y.at_above(x + bx + 4, y + by);
                    a8[4..].copy_from_slice(&self.y.data[ar..ar + 4]);
                }
                let mut l4 = [0u8; 4];
                for (i, cell) in l4.iter_mut().enumerate() {
                    *cell = self.y.data[self.y.at_left(x + bx, y + by + i)];
                }
                let p = self.y.data[self.y.at_above_left(x + bx, y + by)];
                let mut block = [0u8; 16];
                intra::predict4(b_modes[b], &a8, &l4, p, &mut block);
                for i in 0..4 {
                    let d = self.y.at(x + bx, y + by + i);
                    self.y.data[d..d + 4].copy_from_slice(&block[i * 4..i * 4 + 4]);
                }
                let mut c = coeffs.y[b];
                if coeffs.has_y2 {
                    c[0] = y_dcs[b];
                }
                if c.iter().any(|&v| v != 0) {
                    let dst = self.y.at(x + bx, y + by);
                    let stride = self.y.stride;
                    idct_add_into(&mut self.y.data, &c, &block, 4, 0, dst, stride);
                }
            }
        } else {
            // Gather luma edges (border cells carry the 127/129 fills).
            let (above, left, above_left) = {
                let ay = self.y.at_above(x, y);
                let mut a = [0u8; 16];
                let mut l = [0u8; 16];
                a.copy_from_slice(&self.y.data[ay..ay + 16]);
                for (i, cell) in l.iter_mut().enumerate() {
                    *cell = self.y.data[self.y.at_left(x, y + i)];
                }
                let al = self.y.data[self.y.at_above_left(x, y)];
                (a, l, al)
            };
            let mut pred = [0u8; 256];
            intra::predict16(
                y_mode, have_above, have_left, &above, &left, above_left, &mut pred,
            );
            for b in 0..16usize {
                let bx = (b % 4) * 4;
                let by = (b / 4) * 4;
                let mut c = coeffs.y[b];
                if coeffs.has_y2 {
                    c[0] = y_dcs[b];
                }
                if c.iter().all(|&v| v == 0) {
                    for i in 0..4 {
                        let d = self.y.at(x + bx, y + by + i);
                        let s = (by + i) * 16 + bx;
                        self.y.data[d..d + 4].copy_from_slice(&pred[s..s + 4]);
                    }
                } else {
                    let dst = self.y.at(x + bx, y + by);
                    let stride = self.y.stride;
                    idct_add_into(&mut self.y.data, &c, &pred, 16, by * 16 + bx, dst, stride);
                }
            }
        }

        // Chroma: 8x8 prediction then four residual blocks per plane.
        let (above_u, left_u, al_u) = self.chroma_edges(col, row, 0);
        let (above_v, left_v, al_v) = self.chroma_edges(col, row, 1);
        let mut pred_u = [0u8; 64];
        let mut pred_v = [0u8; 64];
        intra::predict8(
            uv_mode,
            have_above,
            have_left,
            &above_u,
            &left_u,
            al_u,
            &mut pred_u,
        );
        intra::predict8(
            uv_mode,
            have_above,
            have_left,
            &above_v,
            &left_v,
            al_v,
            &mut pred_v,
        );
        let (cx, cy) = (x / 2, y / 2);
        for (plane_data, coeffs_uv, pred8, stride) in [
            (&mut self.u.data, &coeffs.u, &pred_u, self.u.stride),
            (&mut self.v.data, &coeffs.v, &pred_v, self.v.stride),
        ] {
            for b in 0..4usize {
                let bx = (b % 2) * 4;
                let by = (b / 2) * 4;
                let c = coeffs_uv[b];
                let dst = (1 + cy + by) * stride + 1 + cx + bx;
                if c.iter().all(|&v| v == 0) {
                    for i in 0..4 {
                        let s = (by + i) * 8 + bx;
                        let d = dst + i * stride;
                        plane_data[d..d + 4].copy_from_slice(&pred8[s..s + 4]);
                    }
                } else {
                    idct_add_into(plane_data, &c, pred8, 8, by * 8 + bx, dst, stride);
                }
            }
        }
    }

    /// Above-right extras (§12.3): real pixels from the above row,
    /// replicated last pixel on the rightmost MB, 127 on the top row.
    fn above_right(&self, x: usize, y: usize) -> [u8; 4] {
        if y == 0 {
            [127; 4]
        } else if x + 16 >= self.mb_cols * 16 {
            [self.y.data[self.y.at_above(x + 15, y)]; 4]
        } else {
            let off = self.y.at_above(x + 16, y);
            [
                self.y.data[off],
                self.y.data[off + 1],
                self.y.data[off + 2],
                self.y.data[off + 3],
            ]
        }
    }

    fn chroma_edges(&self, col: usize, row: usize, plane: usize) -> ([u8; 8], [u8; 8], u8) {
        let p = if plane == 0 { &self.u } else { &self.v };
        let x = col * 8;
        let y = row * 8;
        let mut a = [0u8; 8];
        let mut l = [0u8; 8];
        let aoff = p.at_above(x, y);
        a.copy_from_slice(&p.data[aoff..aoff + 8]);
        for (i, cell) in l.iter_mut().enumerate() {
            *cell = p.data[p.at_left(x, y + i)];
        }
        let al = p.data[p.at_above_left(x, y)];
        (a, l, al)
    }

    /// Crop the padded planes to the real frame size.
    fn crop(&self) -> Picture {
        let w = usize::from(self.dims.width);
        let h = usize::from(self.dims.height);
        let mut y = Vec::with_capacity(w * h);
        for row in 0..h {
            let off = self.y.at(0, row);
            y.extend_from_slice(&self.y.data[off..off + w]);
        }
        let (cw, ch) = ((w + 1) / 2, (h + 1) / 2);
        let mut u = Vec::with_capacity(cw * ch);
        let mut v = Vec::with_capacity(cw * ch);
        for row in 0..ch {
            let off = self.u.at(0, row);
            u.extend_from_slice(&self.u.data[off..off + cw]);
            let off = self.v.at(0, row);
            v.extend_from_slice(&self.v.data[off..off + cw]);
        }
        Picture {
            y,
            u,
            v,
            width: self.dims.width,
            height: self.dims.height,
            stride: w,
            uv_stride: cw,
        }
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Segment-map coding tree (RFC 6386 §10): plain two-level split of 0-3.
const SEGMENT_TREE: [i8; 6] = [2, 4, 0, -1, -2, -3];

fn ymode_as_bmode(y: u8) -> u8 {
    match y {
        0 => 0, // DC_PRED -> B_DC_PRED
        1 => 2, // V_PRED -> B_VE_PRED
        2 => 3, // H_PRED -> B_HE_PRED
        3 => 1, // TM_PRED -> B_TM_PRED
        _ => unreachable!("non-B_PRED luma mode {y}"),
    }
}

/// Add one 4x4 block's dequantized-coefficient residual to its
/// prediction: reads the prediction patch from `pred`, clamps, writes
/// into the strided plane (§14.4/§14.5).
fn idct_add_into(
    plane: &mut [u8],
    coeffs: &[i16; 16],
    pred: &[u8],
    pred_stride: usize,
    pred_off: usize,
    dst_off: usize,
    dst_stride: usize,
) {
    let mut p = [0u8; 16];
    for i in 0..4 {
        let s = pred_off + i * pred_stride;
        p[i * 4..i * 4 + 4].copy_from_slice(&pred[s..s + 4]);
    }
    let mut out = [0u8; 16];
    transform::idct4x4_add(coeffs, &p, &mut out);
    for i in 0..4 {
        let d = dst_off + i * dst_stride;
        plane[d..d + 4].copy_from_slice(&out[i * 4..i * 4 + 4]);
    }
}

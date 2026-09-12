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
use crate::mc;
use crate::intra;
use crate::loopfilter::{self, MbFilterInfo};
use crate::modes;
use crate::tables::KF_BMODE_PROB;
use crate::tokens::{self, Dq, MbCoeffs, TokenContexts};
use crate::transform;
use crate::transform::Dequant;
use crate::{Error, PersistedState, Result};
/// Copy a visible MB-aligned image out of the 1-pixel-bordered working
/// plane into the top-left visible corner of a bordered reference
/// buffer.
fn copy_plane(
    dst: &mut [u8],
    dst_stride: usize,
    src: &[u8],
    src_stride: usize,
    w: usize,
    h: usize,
) {
    for r in 0..h {
        let s = (1 + r) * src_stride + 1;
        let d = (MC_BORDER + r) * dst_stride + MC_BORDER;
        dst[d..d + w].copy_from_slice(&src[s..s + w]);
    }
}

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
    /// 0 = intra, 1 = last, 2 = golden, 3 = altref (§16.2). Key-frame
    /// MBs and the border are intra.
    ref_frame: u8,
    /// Inter mv_ref leaf (§16.2): 0 = zero, 1 = nearest, 2 = near,
    /// 3 = new, 4 = split. 0 when intra.
    mv_ref: u8,
    /// Whole-MB motion vector in eighth-pels; the bottom-right subblock's
    /// vector for SPLITMV (what future neighbours' census sees).
    mv: (i16, i16),
    /// Per-subblock motion vectors (SPLITMV).
    bmi: [(i16, i16); 16],
    /// `need_to_clamp_mvs` (§16.2): the luma MC clamps this MV into the
    /// extended-frame range.
    mv_clamp: bool,
    /// Any non-zero coefficient decoded for this MB (bit 31 of dixie's
    /// eob_mask); decides the loop filter's interior edges (§15.1).
    has_nz: bool,
}

/// One reference-frame slot: bordered planes, 32-pixel edge-replicated
/// borders around the MB-aligned image (dixie `extend_frame`).
#[derive(Clone)]
struct RefFrame {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    stride: usize,
    uv_stride: usize,
}

/// MC border width in pixels (libvpx `VP8BORDERINPIXELS`).
const MC_BORDER: usize = 32;

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
    /// Reference frame slots: [unused, last, golden, altref] (§16.2
    /// indices). Empty until the first key frame.
    refs: [Option<RefFrame>; 4],
    /// Persisted per-MB segment ids, `mb_rows * mb_cols` (§9.3: when a
    /// frame does not re-code the map, ids carry over).
    segment_map: Vec<u8>,
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
    /// Frame width in pixels.
    pub width: u16,
    /// Frame height in pixels.
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
            refs: [None, None, None, None],
            segment_map: Vec::new(),
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
        // Entropy revert (§9.7): the probability updates this frame
        // applies are transient unless `refresh_entropy` says they
        // persist. Snapshots are cheap array copies; segmentation and
        // loop-filter deltas live outside the frame context and never
        // revert (libvpx saves only `fc`).
        let coeff_probs = self.state.coeff_probs;
        let mv_probs = self.state.mv_probs;
        let ymode_probs = self.state.ymode_probs;
        let uv_mode_probs = self.state.uv_mode_probs;
        let (header, hdr_dec) = FrameHeader::parse(frame, &mut self.state)?;
        let refresh_entropy = header.refresh.refresh_entropy;
        let pic = match header.tag.frame_type {
            FrameType::Key => self.decode_keyframe(frame, header, hdr_dec),
            FrameType::Inter => self.decode_interframe(frame, header, hdr_dec),
        }?;
        if !refresh_entropy {
            self.state.coeff_probs = coeff_probs;
            self.state.mv_probs = mv_probs;
            self.state.ymode_probs = ymode_probs;
            self.state.uv_mode_probs = uv_mode_probs;
        }
        Ok(pic)
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

    fn decode_keyframe(
        &mut self,
        frame: &[u8],
        header: FrameHeader,
        mut hdr_dec: BoolDecoder<'_>,
    ) -> Result<Option<Picture>> {
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
                if std::env::var_os("EC_VP8_TRACE").is_some() {
                    eprintln!("M {} {}", row - 1, col - 1);
                }
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
                if std::env::var_os("EC_VP8_TRACE").is_some() {
                    eprintln!("T {} {} hy2={}", row - 1, col - 1, has_y2);
                }
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
        self.run_loopfilter(&header, true);

        // A key frame refreshes every reference slot and clears the
        // persisted segment map (§9.3, §9.7).
        self.segment_map = vec![0; self.mb_cols * self.mb_rows];
        let rf = self.make_ref();
        self.refs[1] = Some(rf.clone());
        self.refs[2] = Some(rf.clone());
        self.refs[3] = Some(rf);

        self.frame_decoded = true;
        Ok(header.tag.show_frame.then(|| self.crop()))
    }

    /// Loop-filter post-pass over the whole frame (dixie order-equivalent
    /// driver; `inner_edges` follows libvpx's `skip_lf`: skipped MBs
    /// filter only their boundary edges, except 4x4-coded modes).
    fn run_loopfilter(&mut self, header: &FrameHeader, is_keyframe: bool) {
        if header.filter_level == 0 {
            return;
        }
        let infos: Vec<MbFilterInfo> = (0..self.mb_rows)
            .flat_map(|r| (0..self.mb_cols).map(move |c| (r, c)))
            .map(|(r, c)| {
                let mb = self.mbi(r + 1, c + 1);
                MbFilterInfo {
                    level: self.mb_filter_level(header, mb),
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
            yimg, uimg, vimg, ystride, ustride, mcols, mrows, sharpness, simple, &infos,
            is_keyframe,
        );
    }

    /// Snapshot the reconstructed (post-LF) frame into a bordered
    /// reference buffer with 32-pixel edge-replicated borders.
    fn make_ref(&self) -> RefFrame {
        let w = self.mb_cols * 16;
        let h = self.mb_rows * 16;
        let (cw, ch) = (self.mb_cols * 8, self.mb_rows * 8);
        let mut rf = RefFrame {
            y: vec![0; (w + 2 * MC_BORDER) * (h + 2 * MC_BORDER)],
            u: vec![0; (cw + 2 * MC_BORDER) * (ch + 2 * MC_BORDER)],
            v: vec![0; (cw + 2 * MC_BORDER) * (ch + 2 * MC_BORDER)],
            stride: w + 2 * MC_BORDER,
            uv_stride: cw + 2 * MC_BORDER,
        };
        copy_plane(&mut rf.y, rf.stride, &self.y.data, self.y.stride, w, h);
        copy_plane(&mut rf.u, rf.uv_stride, &self.u.data, self.u.stride, cw, ch);
        copy_plane(&mut rf.v, rf.uv_stride, &self.v.data, self.v.stride, cw, ch);
        mc::extend_plane(&mut rf.y, rf.stride, w, h, MC_BORDER);
        mc::extend_plane(&mut rf.u, rf.uv_stride, cw, ch, MC_BORDER);
        mc::extend_plane(&mut rf.v, rf.uv_stride, cw, ch, MC_BORDER);
        rf
    }


    /// Per-MB loop filter level (libvpx `loop_filter_frame_init`):
    /// segment level (clamped), then the reference delta, then the mode
    /// delta, clamped again. Mode slots: intra B_PRED → 0, other intra
    /// and ZEROMV → 1 (no delta for other intra), the moving modes → 2,
    /// SPLITMV → 3.
    fn mb_filter_level(&self, header: &FrameHeader, mb: &MbInfo) -> u8 {
        let seg = &header.segmentation;
        let mut level = i32::from(header.filter_level);
        if seg.enabled {
            let d = seg.lf_level[usize::from(mb.segment_id)];
            level = if seg.abs_delta { d } else { level + d };
        }
        level = level.clamp(0, 63);
        if header.lf_delta_enabled {
            level += self.state.ref_lf_delta[usize::from(mb.ref_frame)];
            let mode_idx = if mb.ref_frame == 0 {
                if mb.y_mode == modes::YMode::BPred.as_u8() {
                    Some(0)
                } else {
                    None
                }
            } else {
                match mb.mv_ref {
                    0 => Some(1), // ZEROMV
                    4 => Some(3), // SPLITMV
                    _ => Some(2), // NEAREST / NEAR / NEW
                }
            };
            if let Some(mode_idx) = mode_idx {
                level += self.state.mode_lf_delta[mode_idx];
            }
            level = level.clamp(0, 63);
        }
        level as u8
    }
    /// Decode one inter frame (§16): all mode records off partition 0
    /// (row-major), then per-row tokens and reconstruction off the row's
    /// token partition, loop filter, then the reference updates (§9.7).
    fn decode_interframe(
        &mut self,
        frame: &[u8],
        header: FrameHeader,
        mut hdr_dec: BoolDecoder<'_>,
    ) -> Result<Option<Picture>> {
        if self.mb_cols == 0 || self.refs[1..4].iter().any(|r| r.is_none()) {
            return Err(Error::corrupt("VP8 inter frame before the first key frame"));
        }

        // Token partition bool decoders (§9.5) — identical tiling to
        // key frames.
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

        // Dequant factors per segment (persisted segmentation state).
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
        let ymode_probs = self.state.ymode_probs;
        let uv_mode_probs = self.state.uv_mode_probs;
        let sign_bias = [
            false,
            header.refresh.sign_bias_golden,
            header.refresh.sign_bias_altref,
        ];

        // Phase 1: every mode record of the frame, row-major (§16).
        eprintln!("STAGE modes");
        for row in 1..=self.mb_rows {
            for col in 1..=self.mb_cols {
                let mut mb = MbInfo::default();
                if header.segmentation.enabled {
                    if header.segmentation.update_map {
                        mb.segment_id =
                            hdr_dec.read_tree(&SEGMENT_TREE, &header.segmentation.tree_probs);
                        self.segment_map[(row - 1) * self.mb_cols + (col - 1)] = mb.segment_id;
                    } else {
                        mb.segment_id = self.segment_map[(row - 1) * self.mb_cols + (col - 1)];
                    }
                }
                if header.coeff_skip_enabled {
                    mb.skip = hdr_dec.read_bool(header.prob_skip_false);
                }
                self.parse_inter_modes(
                    &mut hdr_dec,
                    row,
                    col,
                    &mut mb,
                    &header,
                    &ymode_probs,
                    &uv_mode_probs,
                    &sign_bias,
                );
                *self.mbi_mut(row, col) = mb;
            }
        }

        // Phases 2+3 per row: tokens, then reconstruction (MC or intra).
        for row in 1..=self.mb_rows {
            self.token_ctxs.reset_left();
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
                {
                    // "Special case: force the loop filter to skip when
                    // eobtotal is zero" (libvpx decodeframe.c): a decoded
                    // MB without non-zero coefficients filters like a
                    // skipped one.
                    let slot = &mut self.mb_info[row * (self.mb_cols + 1) + col];
                    slot.has_nz = coeffs.has_nonzero();
                    slot.skip = mb.skip || !coeffs.has_nonzero();
                }
                self.reconstruct_inter_mb(row - 1, col - 1, &mb, &coeffs);
            }
        }

        self.run_loopfilter(&header, false);

        // Reference buffer updates, libvpx `swap_frame_buffers` order:
        // the copies run first (copy_gf = 2 must see a possibly
        // just-updated altref), then the refreshes, then last.
        let new = self.make_ref();
        match header.refresh.copy_arf {
            1 => self.refs[3] = self.refs[1].clone(),
            2 => self.refs[3] = self.refs[2].clone(),
            _ => {}
        }
        match header.refresh.copy_gf {
            1 => self.refs[2] = self.refs[1].clone(),
            2 => self.refs[2] = self.refs[3].clone(),
            _ => {}
        }
        if header.refresh.refresh_gf {
            self.refs[2] = Some(new.clone());
        }
        if header.refresh.refresh_arf {
            self.refs[3] = Some(new.clone());
        }
        if header.refresh.refresh_last {
            self.refs[1] = Some(new);
        }

        self.frame_decoded = true;
        Ok(header.tag.show_frame.then(|| self.crop()))
    }

    /// Inter-frame mode record (§16, libvpx `read_mb_modes_mv`): intra
    /// at the persisted mode probabilities, or an inter mode with its
    /// motion vectors off the neighborhood census.
    #[allow(clippy::too_many_arguments)]
    fn parse_inter_modes(
        &mut self,
        d: &mut BoolDecoder<'_>,
        row: usize,
        col: usize,
        mb: &mut MbInfo,
        header: &FrameHeader,
        ymode_probs: &[u8; 4],
        uv_mode_probs: &[u8; 3],
        sign_bias: &[bool; 3],
    ) {
        if !d.read_bool(header.prob_intra) {
            // Intra-coded MB inside an inter frame (§16.1). Subblock
            // modes use the same fixed context table as key frames.
            mb.ref_frame = 0;
            let ymode = modes::read_ymode(d, ymode_probs);
            mb.y_mode = ymode.as_u8();
            if ymode == modes::YMode::BPred {
                for j in 0..16u8 {
                    let a = self.above_block_mode(row, col, j, mb);
                    let l = self.left_block_mode(row, col, j, mb);
                    mb.b_modes[usize::from(j)] =
                        modes::read_bmode(d, &KF_BMODE_PROB[usize::from(a)][usize::from(l)])
                            .as_u8();
                }
            }
            mb.uv_mode = modes::read_uvmode(d, uv_mode_probs).as_u8();
            return;
        }

        mb.ref_frame = if !d.read_bool(header.prob_last) {
            1
        } else if !d.read_bool(header.prob_gf) {
            2
        } else {
            3
        };
        let to_bias = sign_bias[usize::from(mb.ref_frame) - 1];

        let edges = modes::mb_edges(col - 1, row - 1, self.mb_cols, self.mb_rows);
        let above = self.mv_neighbor(row - 1, col, sign_bias);
        let left = self.mv_neighbor(row, col - 1, sign_bias);
        let aboveleft = self.mv_neighbor(row - 1, col - 1, sign_bias);
        let (mut near_mvs, cnt) = modes::find_near_mvs(above, left, aboveleft, to_bias);
        if std::env::var_os("EC_VP8_TRACE").is_some() {
            eprintln!("CNT r={} c={} cnt={:?} mvs={:?}", row - 1, col - 1, cnt, near_mvs);
        }

        if !d.read_bool(modes::MODE_CONTEXTS[cnt[modes::CNT_INTRA] as usize][0]) {
            mb.mv_ref = 0; // ZEROMV
            return;
        }
        if !d.read_bool(modes::MODE_CONTEXTS[cnt[modes::CNT_NEAREST] as usize][1]) {
            mb.mv_ref = 1; // NEARESTMV
            mb.mv = near_mvs[modes::CNT_NEAREST];
            modes::clamp_mv2(&mut mb.mv, edges);
            return;
        }
        if !d.read_bool(modes::MODE_CONTEXTS[cnt[modes::CNT_NEAR] as usize][2]) {
            mb.mv_ref = 2; // NEARMV
            mb.mv = near_mvs[modes::CNT_NEAR];
            modes::clamp_mv2(&mut mb.mv, edges);
            return;
        }

        // NEWMV or SPLITMV: both code vectors as deltas off the best
        // candidate — the nearest one when it dominates the census —
        // clamped into range first (§16.2-§16.4).
        let near_index = usize::from(cnt[modes::CNT_NEAREST] >= cnt[modes::CNT_INTRA]);
        modes::clamp_mv2(&mut near_mvs[near_index], edges);
        let best = near_mvs[near_index];
        if !d.read_bool(modes::MODE_CONTEXTS[cnt[modes::CNT_SPLITMV] as usize][3]) {
            mb.mv_ref = 3; // NEWMV
            let mut mv = modes::read_mv(d, &self.state.mv_probs);
            mv.0 = mv.0.wrapping_add(best.0);
            mv.1 = mv.1.wrapping_add(best.1);
            mb.mv_clamp = modes::mv_out_of_bounds(mv, edges);
            mb.mv = mv;
        } else {
            self.parse_split_mv(d, row, col, mb, best, edges);
        }
    }

    /// Census input from one neighbour slot (§16.3); border and intra
    /// MBs contribute nothing, zero-vector inter MBs count like intra.
    fn mv_neighbor(&self, row: usize, col: usize, sign_bias: &[bool; 3]) -> modes::MvNeighbor {
        if row == 0 || col == 0 || self.mbi(row, col).ref_frame == 0 {
            return modes::MvNeighbor {
                intra: true,
                zero: false,
                mv: (0, 0),
                is_split: false,
                sign_bias: false,
            };
        }
        let mb = self.mbi(row, col);
        modes::MvNeighbor {
            intra: false,
            zero: mb.mv == (0, 0),
            mv: mb.mv,
            is_split: mb.mv_ref == 4,
            sign_bias: sign_bias[usize::from(mb.ref_frame) - 1],
        }
    }

    /// SPLITMV mode record (§16.4, libvpx `decode_split_mv`): partition
    /// shape, then one vector per subset coded against its left/above
    /// contexts, each filling a whole group of subblocks.
    fn parse_split_mv(
        &mut self,
        d: &mut BoolDecoder<'_>,
        row: usize,
        col: usize,
        mb: &mut MbInfo,
        best: (i16, i16),
        edges: (i32, i32, i32, i32),
    ) {
        let s = modes::read_mv_partition(d);
        let num_p = modes::MV_PARTITION_COUNT[s];
        let mv_probs = self.state.mv_probs;
        for j in 0..num_p {
            let k = usize::from(modes::MBSPLIT_OFFSET[s][j]);
            let leftmv = if k % 4 == 0 {
                let lm = self.mbi(row, col - 1);
                if lm.mv_ref == 4 {
                    lm.bmi[k + 3]
                } else {
                    lm.mv
                }
            } else {
                mb.bmi[k - 1]
            };
            let abovemv = if k < 4 {
                let am = self.mbi(row - 1, col);
                if am.mv_ref == 4 {
                    am.bmi[k + 12]
                } else {
                    am.mv
                }
            } else {
                mb.bmi[k - 4]
            };
            let ctx = modes::mv_cont(leftmv, abovemv);
            let blockmv = match modes::read_sub_mv_ref(d, ctx) {
                modes::SubMvRef::Left4x4 => leftmv,
                modes::SubMvRef::Above4x4 => abovemv,
                modes::SubMvRef::Zero4x4 => (0, 0),
                modes::SubMvRef::New4x4 => {
                    let mut mv = modes::read_mv(d, &mv_probs);
                    mv.0 = mv.0.wrapping_add(best.0);
                    mv.1 = mv.1.wrapping_add(best.1);
                    mv
                }
            };
            if std::env::var_os("EC_VP8_TRACE").is_some() {
                eprintln!(
                    "SP r={} c={} j={} k={} l=({},{}) a=({},{}) ctx={} mv=({},{}) best=({},{})",
                    row - 1,
                    col - 1,
                    j,
                    k,
                    leftmv.0,
                    leftmv.1,
                    abovemv.0,
                    abovemv.1,
                    ctx,
                    blockmv.0,
                    blockmv.1,
                    best.0,
                    best.1
                );
            }
            mb.mv_clamp |= modes::mv_out_of_bounds(blockmv, edges);
            let count = modes::MBSPLIT_FILL_COUNT[s];
            for &off in &modes::MBSPLIT_FILL_OFFSET[s][j * count..(j + 1) * count] {
                mb.bmi[usize::from(off)] = blockmv;
            }
        }
        mb.mv_ref = 4;
        // 4x4 residual coding without a Y2 block (like B_PRED).
        mb.y_mode = modes::YMode::BPred.as_u8();
        // Neighbours' census sees the bottom-right subblock's vector.
        mb.mv = mb.bmi[15];
    }

    /// Reconstruct one inter-frame MB: motion compensation from the
    /// reference (or intra prediction), then the residual (§16, §14).
    fn reconstruct_inter_mb(&mut self, row: usize, col: usize, mb: &MbInfo, coeffs: &MbCoeffs) {
        if mb.ref_frame == 0 {
            self.reconstruct_intra_mb(row, col, mb, coeffs);
            return;
        }
        if std::env::var_os("EC_VP8_TRACE").is_some() {
            eprintln!(
                "IMB r={} c={} ref={} mvref={} mv=({},{}) clamp={} skip={}",
                row, col, mb.ref_frame, mb.mv_ref, mb.mv.0, mb.mv.1, mb.mv_clamp, mb.skip
            );
        }
        let reff = self.refs[usize::from(mb.ref_frame)]
            .as_ref()
            .expect("reference slots checked at frame start");
        let x = col * 16;
        let y = row * 16;
        let w = self.mb_cols * 16;
        let h = self.mb_rows * 16;
        let (cw, ch) = (w / 2, h / 2);
        // Luma: one 16x16 predict, or 16 independent 4x4 predicts for
        // SPLITMV (identical output to libvpx's grouped predicts). Each
        // predict lands at its own (bx, by) slot of one 16x16 patch.
        let mut py = [0u8; 256];
        if mb.mv_ref == 4 {
            for b in 0..16usize {
                let (bx, by) = ((b % 4) * 4, (b / 4) * 4);
                mc::predict_luma(
                    &reff.y, reff.stride, MC_BORDER, w, h, x + bx, y + by, mb.bmi[b], false,
                    &mut py[by * 16 + bx..], 16, 4, 4,
                );
            }
        } else {
            mc::predict_luma(
                &reff.y, reff.stride, MC_BORDER, w, h, x, y, mb.mv, mb.mv_clamp, &mut py, 16,
                16, 16,
            );
        }

        // Chroma: one 8x8 predict per quadrant per plane. Whole-MB modes
        // halve the single vector; SPLITMV sums its quadrant's four
        // subblock vectors (§16.4, libvpx build_4x4uvmvs). The whole-MB
        // path re-clamps UNCONDITIONALLY (reconinter.c: rounding the
        // derived chroma MV to full-pel can move it outside the tap
        // window even when the luma MV was in range); the SPLITMV path
        // clamps only under need_to_clamp_mvs.
        let mut pu = [0u8; 256];
        let mut pv = [0u8; 256];
        for q in 0..4usize {
            let (qx16, qy16) = ((q % 2) * 8, (q / 2) * 8);
            let (cx, cy) = (x / 2 + qx16, y / 2 + qy16);
            let (uvmv, uv_clamp) = if mb.mv_ref == 4 {
                let (qx, qy) = ((q % 2) * 2, (q / 2) * 2);
                (
                    modes::chroma_mv_split(&[
                        mb.bmi[qy * 4 + qx],
                        mb.bmi[qy * 4 + qx + 1],
                        mb.bmi[qy * 4 + qx + 4],
                        mb.bmi[qy * 4 + qx + 5],
                    ]),
                    mb.mv_clamp,
                )
            } else {
                (modes::chroma_mv_whole(mb.mv), true)
            };
            mc::predict_chroma(
                &reff.u, reff.uv_stride, MC_BORDER, cw, ch, cx, cy, uvmv, uv_clamp,
                &mut pu[qy16 * 16 + qx16..], 16, 8, 8,
            );
            mc::predict_chroma(
                &reff.v, reff.uv_stride, MC_BORDER, cw, ch, cx, cy, uvmv, uv_clamp,
                &mut pv[qy16 * 16 + qx16..], 16, 8, 8,
            );
        }
        if std::env::var_os("EC_VP8_TRACE").is_some() && row == 0 && col == 0 {
            eprintln!(
                "CHROMA pred_u[0..8]={:?} refu_vis[0..8]={:?} coeffs.u0dcsum={}",
                &pu[0..8],
                &reff.u[MC_BORDER * reff.uv_stride + MC_BORDER..MC_BORDER * reff.uv_stride + MC_BORDER + 8],
                coeffs.u[0].iter().sum::<i16>()
            );
        }

        // Residual: every block adds onto its MC prediction.
        let y_dcs: [i16; 16] = if coeffs.has_y2 {
            transform::iwht4x4(&coeffs.y2)
        } else {
            [0; 16]
        };
        for b in 0..16usize {
            let (bx, by) = ((b % 4) * 4, (b / 4) * 4);
            let mut c = coeffs.y[b];
            if coeffs.has_y2 {
                c[0] = y_dcs[b];
            }
            let dst = self.y.at(x + bx, y + by);
            idct_add_into(&mut self.y.data, &c, &py, 16, by * 16 + bx, dst, self.y.stride);
        }
        for (plane_data, coeffs_uv, pred, stride) in [
            (&mut self.u.data, &coeffs.u, &pu, self.u.stride),
            (&mut self.v.data, &coeffs.v, &pv, self.v.stride),
        ] {
            for b in 0..4usize {
                let (bx, by) = ((b % 2) * 4, (b / 2) * 4);
                let dst = (1 + y / 2 + by) * stride + 1 + x / 2 + bx;
                idct_add_into(plane_data, &coeffs_uv[b], pred, 16, by * 16 + bx, dst, stride);
            }
        }
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

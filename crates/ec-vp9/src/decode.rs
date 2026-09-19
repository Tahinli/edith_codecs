//! Frame decoding: superframe split, uncompressed header (via
//! [`ec_vp9_syntax::Vp9Parser`]), compressed header, tile parsing,
//! partition recursion, intra prediction, token decoding,
//! reconstruction and the loop filter (spec 6, 7, 8; libvpx
//! `vp9_decodeframe.c`).
//!
//! Capability: profile-0 8-bit 4:2:0 keyframes, intra-only frames and inter
//! frames (motion compensation, residual reconstruction, the loop filter and
//! an 8-slot reference DPB). A reference whose coded size differs from the
//! current frame is scaled during prediction; other profiles/subsamplings are
//! refused by name; `show_existing_frame` returns the buffered reference
//! picture instead of pretending to decode. Odd coded extents are supported:
//! chroma stores and crops at `(w + 1) / 2` (`uv_crop_width`), matching
//! libvpx.

use crate::header::{FrameContext, read_compressed_header};
use crate::intra::build_intra_predictors;
use crate::loopfilter::LfGrids;
use crate::mc::{
    RefPlane, ScaleFactors, average_split_mvs_chroma, build_inter_predictors, split_mv,
};
use crate::modes::{MiInfo, MiState, read_intra_frame_mode_info};
use crate::tables::*;
use crate::tokens::{PlaneContexts, decode_coefs, scan_for};
use crate::transform::inverse_transform_add;
use crate::{Error, Result};
use ec_vp9_syntax::{FrameHeader, FrameType, Vp9Parser, superframe};

const SB_MI: usize = 8; // a 64x64 superblock in mi (8x8) units

/// A decoded, displayable frame: cropped contiguous planes.
#[derive(Clone, Debug)]
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
    /// Luma row pitch of the returned planes (== `width`; the crop packs rows).
    pub stride: usize,
    /// Chroma row pitch of the returned planes (`(width + 1) / 2`; chroma
    /// ceils an odd coded width).
    pub uv_stride: usize,
}

struct Planes {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    ys: usize,
    uvs: usize,
    aw: usize,
    ah: usize,
}

impl Planes {
    /// Immutable plane + stride (the predictor reads references).
    fn plane_ref(&self, plane: usize) -> (&[u8], usize) {
        match plane {
            0 => (&self.y, self.ys),
            1 => (&self.u, self.uvs),
            _ => (&self.v, self.uvs),
        }
    }
    fn plane(&mut self, plane: usize) -> (&mut [u8], usize) {
        match plane {
            0 => (&mut self.y, self.ys),
            1 => (&mut self.u, self.uvs),
            _ => (&mut self.v, self.uvs),
        }
    }
    fn dims(&self, plane: usize) -> (usize, usize) {
        if plane == 0 {
            (self.aw, self.ah)
        } else {
            (self.aw / 2, self.ah / 2)
        }
    }
}

/// The VP9 decoder. Feed complete frames — one full VP9 frame payload
/// or one superframe chunk, exactly what a container demuxer hands over
/// for the `VP90` codec.
pub struct Decoder {
    parser: Vp9Parser,
    ctx: FrameContext,
    refs: [Option<RefFrame>; 8],
    /// Frame scratch, live only inside `decode_keyframe`.
    planes: Option<Planes>,
    /// The four stored frame contexts (spec 6.2 `frame_context_idx`).
    frame_ctxs: [FrameContext; 4],
    /// The previous frame's per-8x8 MV field and segment map
    /// (`prev_frame->mvs` / `last_frame_seg_map`).
    prev_mvs: Vec<crate::inter::MvRef>,
    prev_seg: Vec<u8>,
    last: LastFrameFacts,
    /// Live only while an inter frame is being parsed.
    iw: Option<InterWalk>,
    /// Frames parsed by `decode_syntax` (the dump's frame index).
    frames_seen: usize,
    /// Frames decoded by `decode` (the dumps' `frame=` field for this path;
    /// `decode_syntax` counts the same way in `frames_seen`).
    decode_index: usize,
    /// Mode-info blocks parsed in the most recent inter frame (0 otherwise).
    syntax_blocks: usize,
    /// `twd->extend_and_predict_buf`: the per-prediction bordered reference
    /// window built when a block's read leaves the reference frame.
    mc_scratch: Vec<u8>,
    /// The `vpx_convolve8` stride-64 intermediate plus the second scratch its
    /// averaging variant needs.
    mc_temp: Vec<u8>,
}

/// One DPB slot: a decoded frame buffer at its ALIGNED (64-multiple) extent
/// plus the coded size it was decoded at.
///
/// The aligned extent is load-bearing, not slack: libvpx writes a block at the
/// frame edge at its FULL size (`decodeframe.c:1216`), so the rows past the
/// coded height hold real reconstructed samples, and `dec_build_inter_predictors`
/// reads them directly whenever a block's window is inside the frame with a
/// whole-pixel MV (`build_mc_border` is only reached when the window leaves the
/// frame or the coded size is not a multiple of 8). Cropping the reference to
/// the visible size would drop exactly those samples.
#[derive(Clone)]
struct RefFrame {
    planes: std::sync::Arc<Planes>,
    /// Coded width — the predictor's `y_crop_width`.
    width: u16,
    /// Coded height — the predictor's `y_crop_height`.
    height: u16,
}

/// What `use_prev_frame_mvs` (decodeframe.c:3023) tests about the last frame.
#[derive(Clone, Copy, Default)]
struct LastFrameFacts {
    width: u32,
    height: u32,
    intra_only: bool,
    show_frame: bool,
    key: bool,
}

/// State of one inter-syntax frame walk.
struct InterWalk {
    ifs: crate::inter::InterFrameState,
    sign_bias: [bool; 4],
    use_prev: bool,
    seg_map: Vec<u8>,
    mvs: Vec<crate::inter::MvRef>,
    prev_seg: Vec<u8>,
    prev_mvs: Vec<crate::inter::MvRef>,
    tile_col_end: usize,
    frame_index: usize,
    /// Mode-info blocks parsed in this frame (the harness's count gate).
    blocks: usize,
}

impl Decoder {
    /// Create an empty decoder; the first key frame sets the size.
    pub fn new() -> Self {
        Decoder {
            parser: Vp9Parser::new(),
            ctx: FrameContext::new(true),
            refs: [const { None }; 8],
            planes: None,
            frame_ctxs: {
                let d = FrameContext::new(false);
                [d.clone(), d.clone(), d.clone(), d]
            },
            prev_mvs: Vec::new(),
            prev_seg: Vec::new(),
            last: LastFrameFacts::default(),
            iw: None,
            frames_seen: 0,
            decode_index: 0,
            syntax_blocks: 0,
            mc_scratch: Vec::new(),
            mc_temp: vec![0; 64 * 135 + 64 * 64],
        }
    }

    /// Mode-info blocks the most recent inter frame parsed; `0` for a keyframe
    /// or before any frame. The syntax harness pins this number.
    pub fn last_frame_blocks(&self) -> usize {
        self.syntax_blocks
    }

    /// Decode one frame (or one superframe chunk); returns the picture
    /// when a frame is for display. Hidden frames still update state.
    pub fn decode(&mut self, frame: &[u8]) -> Result<Option<Picture>> {
        let mut out = None;
        for sub in superframe::split(frame)? {
            if let Some(pic) = self.decode_one(sub)? {
                out = Some(pic);
            }
        }
        Ok(out)
    }

    fn decode_one(&mut self, frame: &[u8]) -> Result<Option<Picture>> {
        let hdr = self.parser.parse_frame(frame)?;
        if hdr.show_existing_frame {
            let slot = hdr.frame_to_show_map_idx as usize;
            return match self.refs.get(slot).and_then(|p| p.as_ref()) {
                Some(p) => Ok(Some(crop_picture(&p.planes, p.width, p.height))),
                None => Err(corrupt("show_existing_frame names an empty slot")),
            };
        }
        if hdr.profile != 0 {
            return Err(Error::unsupported(
                format!("vp9 profile {}", hdr.profile),
                "this lane decodes profile 0 only",
            ));
        }
        if hdr.subsampling_x != 1 || hdr.subsampling_y != 1 {
            return Err(Error::unsupported(
                "vp9 subsampling",
                format!(
                    "this lane decodes 4:2:0 only (subsampling {}/{} at profile {})",
                    hdr.subsampling_x, hdr.subsampling_y, hdr.profile
                ),
            ));
        }
        // One index per decoded frame (the dumps' `frame=` field), so this
        // path and `decode_syntax` label the same frame the same way.
        let index = self.decode_index;
        self.decode_index += 1;
        if hdr.frame_type == FrameType::Key {
            return self.decode_key_frame(&hdr, frame);
        }
        if hdr.intra_only {
            return self.decode_intra_only_frame(&hdr, frame);
        }
        self.decode_inter_frame(&hdr, frame, index, true)
    }

    /// Decode one key frame through to pixels and refresh every slot.
    fn decode_key_frame(&mut self, hdr: &FrameHeader, frame: &[u8]) -> Result<Option<Picture>> {
        let hdr = hdr.clone();
        // setup_past_independence (spec 7.2): a key frame resets the frame
        // context, so every STORED context goes back to the inter defaults.
        // The keyframe's own mode info reads the const `vp9_kf_partition_probs`
        // (libvpx `get_partition_probs`), which our `new(true)` models on the
        // active copy only.
        let d = FrameContext::new(false);
        self.frame_ctxs = [d.clone(), d.clone(), d.clone(), d];
        self.ctx = FrameContext::new(true);
        self.prev_mvs.clear();
        self.prev_seg.clear();
        self.last = LastFrameFacts {
            width: hdr.width,
            height: hdr.height,
            intra_only: hdr.intra_only,
            show_frame: hdr.show_frame,
            key: true,
        };
        // [scratch probe] EC_VP9_FORCE_UH / EC_VP9_FORCE_HSZ: override the
        // uncompressed-header size / header_size_in_bytes split for diagnosis.
        let hdr = {
            let mut h = hdr;
            if let Ok(v) = std::env::var("EC_VP9_FORCE_UH") {
                h.uncompressed_header_size = v.parse().unwrap_or(h.uncompressed_header_size);
            }
            if let Ok(v) = std::env::var("EC_VP9_FORCE_HSZ") {
                h.header_size_in_bytes = v.parse().unwrap_or(h.header_size_in_bytes);
            }
            h
        };
        let hdr_start = hdr.uncompressed_header_size as usize;
        let ch = read_compressed_header(
            &frame[hdr_start..hdr_start + hdr.header_size_in_bytes as usize],
            &mut self.ctx,
            &hdr,
        )?;

        let pic = self.decode_keyframe(&hdr, frame, ch.tx_mode)?;
        if hdr.refresh_frame_context {
            self.ctx.reset_inter_partition();
            self.frame_ctxs[hdr.frame_context_idx as usize] = self.ctx.clone();
        }
        // Reference refresh (spec 8.10): a keyframe updates all slots.
        let frame = RefFrame {
            planes: std::sync::Arc::new(self.planes.take().expect("frame scratch")),
            width: hdr.width as u16,
            height: hdr.height as u16,
        };
        for slot in self.refs.iter_mut() {
            *slot = Some(frame.clone());
        }
        Ok(if hdr.show_frame { Some(pic) } else { None })
    }

    /// Decode an intra-only frame through to pixels. `libvpx
    /// vp9_decodeframe.c:2708-2745` makes it an INTER frame that never shows
    /// and refreshes only the slots its `refresh_frame_flags` names, but its
    /// body is a keyframe's: intra-only mode info, intra prediction, the intra
    /// coefficient axis and the intra loop-filter levels. Two header decisions
    /// differ from a keyframe (`vp9_setup_past_independence`,
    /// `vp9/common/vp9_entropymode.c:424`):
    ///
    /// - the ACTIVE frame context comes from storage
    ///   (`*cm->fc = cm->frame_contexts[frame_context_idx]`,
    ///   `vp9_decodeframe.c:3008`) — reset to the defaults only for
    ///   `reset_frame_context` 2 (selected) or 3 (all), or under
    ///   error-resilient mode, never unconditionally like a keyframe (whose
    ///   `cm->frame_type == KEY_FRAME` clause resets every stored context);
    /// - partitions read the const `vp9_kf_partition_probs`
    ///   (`set_partition_probs`), so the stored context's own `partition`
    ///   field survives the frame untouched.
    fn decode_intra_only_frame(
        &mut self,
        hdr: &FrameHeader,
        frame: &[u8],
    ) -> Result<Option<Picture>> {
        let hdr = hdr.clone();
        if hdr.error_resilient_mode || hdr.reset_frame_context == 3 {
            let d = FrameContext::new(false);
            self.frame_ctxs = [d.clone(), d.clone(), d.clone(), d];
        } else if hdr.reset_frame_context == 2 {
            self.frame_ctxs[0] = FrameContext::new(false);
        }
        // The parser forces frame_context_idx to 0 for an intra frame.
        let mut ctx = self.frame_ctxs[0].clone();
        let stored_partition = ctx.partition;
        ctx.use_key_partition();
        self.ctx = ctx;
        self.prev_seg.clear();
        self.prev_mvs.clear();
        self.last = LastFrameFacts {
            width: hdr.width,
            height: hdr.height,
            intra_only: true,
            show_frame: hdr.show_frame,
            key: false,
        };
        let hdr_start = hdr.uncompressed_header_size as usize;
        let ch = read_compressed_header(
            &frame[hdr_start..hdr_start + hdr.header_size_in_bytes as usize],
            &mut self.ctx,
            &hdr,
        )?;
        let pic = self.decode_keyframe(&hdr, frame, ch.tx_mode)?;
        if hdr.refresh_frame_context {
            // Store back with the partition probs the context arrived with:
            // the const key table is not a `FRAME_CONTEXT` field.
            self.ctx.partition = stored_partition;
            self.frame_ctxs[0] = self.ctx.clone();
        }
        // Reference refresh (spec 8.10): only `refresh_frame_flags` slots.
        let frame = RefFrame {
            planes: std::sync::Arc::new(self.planes.take().expect("frame scratch")),
            width: hdr.width as u16,
            height: hdr.height as u16,
        };
        for slot in 0..8 {
            if hdr.refresh_frame_flags & (1 << slot) != 0 {
                self.refs[slot] = Some(frame.clone());
            }
        }
        Ok(if hdr.show_frame { Some(pic) } else { None })
    }

    /// Parse one frame's syntax: keyframes decode through to pixels, inter
    /// frames are parsed (headers, mode info, MVs) with no reconstruction and
    /// no picture. `decode` still refuses inter frames.
    pub fn decode_syntax(&mut self, frame: &[u8]) -> Result<()> {
        for sub in superframe::split(frame)? {
            let index = self.frames_seen;
            self.frames_seen += 1;
            let hdr = self.parser.parse_frame(sub)?;
            if hdr.show_existing_frame {
                continue;
            }
            if hdr.profile != 0 {
                return Err(Error::unsupported(
                    format!("vp9 profile {}", hdr.profile),
                    "this lane decodes profile 0 only",
                ));
            }
            if hdr.frame_type == FrameType::Key {
                // Propagate: a corrupt keyframe leaves a half-updated frame
                // context, prev-frame MV field and reference slots behind, so
                // any inter-frame dump after it would be meaningless evidence.
                self.decode_key_frame(&hdr, sub)?;
            } else if hdr.intra_only {
                // Same body as a keyframe, but an inter frame that refreshes
                // only the slots it names.
                self.decode_intra_only_frame(&hdr, sub)?;
            } else {
                self.decode_inter_frame(&hdr, sub, index, false)?;
            }
        }
        Ok(())
    }

    /// Inter-frame decode: compressed header, the tile walk (mode info, MVs
    /// and tokens always; prediction, residual and the loop filter when
    /// `pixels`), then the reference refresh.
    ///
    /// `pixels == false` is the syntax-only walk the instrumented harnesses
    /// drive: tokens keep the entropy stream aligned, no pixel is touched and
    /// the reference slots are left alone.
    fn decode_inter_frame(
        &mut self,
        hdr: &FrameHeader,
        frame: &[u8],
        index: usize,
        pixels: bool,
    ) -> Result<Option<Picture>> {
        // Frame-context bookkeeping (spec 6.2 / libvpx `vp9_decode_frame`):
        // 3 resets every stored context, 2 resets the selected one; 1 means
        // "reset after this frame" and takes no action at frame start.
        if hdr.reset_frame_context == 3 {
            let d = FrameContext::new(false);
            self.frame_ctxs = [d.clone(), d.clone(), d.clone(), d];
        } else if hdr.reset_frame_context == 2 {
            self.frame_ctxs[hdr.frame_context_idx as usize] = FrameContext::new(false);
        }
        self.ctx = self.frame_ctxs[hdr.frame_context_idx as usize].clone();

        // Reference slots (spec 7.2): `ref_frame_idx[i]` names the slot LAST,
        // GOLDEN and ALTREF use. libvpx validates every named reference's
        // format here (`vp9_decodeframe.c:1620-1627`); a reference whose coded
        // size differs from this frame's is SCALED during prediction
        // (`vp9_setup_scale_factors_for_frame` per reference), not refused.
        if pixels {
            for (i, name) in ["last", "golden", "altref"].iter().enumerate() {
                let slot = hdr.ref_frame_idx[i] as usize;
                if slot >= self.refs.len() || hdr.ref_frame_idx[i] >= 8 {
                    return Err(corrupt("vp9 inter names a reference slot past the DPB"));
                }
                if self.refs[slot].is_none() {
                    return Err(Error::corrupt(format!(
                        "vp9 inter reference `{name}` names empty slot {slot}"
                    )));
                }
            }
            let width = hdr.width as usize;
            let height = hdr.height as usize;
            let aw = width.div_ceil(64) * 64;
            let ah = height.div_ceil(64) * 64;
            self.planes = Some(Planes {
                y: vec![128u8; aw * ah],
                u: vec![128u8; aw / 2 * ah / 2],
                v: vec![128u8; aw / 2 * ah / 2],
                ys: aw,
                uvs: aw / 2,
                aw,
                ah,
            });
        }

        let hdr_start = hdr.uncompressed_header_size as usize;
        if crate::trace_enabled() {
            eprintln!("CHSTART frame={index}");
        }
        let ch = read_compressed_header(
            &frame[hdr_start..hdr_start + hdr.header_size_in_bytes as usize],
            &mut self.ctx,
            hdr,
        )?;

        if crate::interdump_enabled() {
            let mut line = format!("PPFLAT frame={index}");
            for row in self.ctx.partition.iter() {
                for v in row.iter() {
                    line.push_str(&format!(" {v}"));
                }
            }
            eprintln!("{line}");
        }

        // INTRA_FRAME's sign bias is 0 by definition; the coded three land at
        // LAST/GOLDEN/ALTREF.
        let sign_bias = [
            false,
            hdr.ref_frame_sign_bias[0],
            hdr.ref_frame_sign_bias[1],
            hdr.ref_frame_sign_bias[2],
        ];
        let mut ifs = crate::inter::InterFrameState {
            reference_mode: ch.reference_mode,
            ..Default::default()
        };
        if ch.reference_mode != crate::inter::SINGLE_REFERENCE {
            crate::inter::setup_compound_reference_mode(&mut ifs, sign_bias);
        }
        let use_prev = !hdr.error_resilient_mode
            && hdr.width == self.last.width
            && hdr.height == self.last.height
            && !self.last.intra_only
            && self.last.show_frame
            && !self.last.key;

        let mi_cols = hdr.mi_cols() as usize;
        let mi_rows = hdr.mi_rows() as usize;
        let sb64_cols = mi_cols.div_ceil(SB_MI);
        let sb64_rows = mi_rows.div_ceil(SB_MI);
        let tile_cols = 1usize << hdr.tile_info.cols_log2;
        let tile_rows = 1usize << hdr.tile_info.rows_log2;

        self.iw = Some(InterWalk {
            ifs,
            sign_bias,
            use_prev,
            seg_map: vec![0; mi_cols * mi_rows],
            mvs: vec![crate::inter::MvRef::default(); mi_cols * mi_rows],
            prev_seg: std::mem::take(&mut self.prev_seg),
            prev_mvs: std::mem::take(&mut self.prev_mvs),
            tile_col_end: mi_cols,
            frame_index: index,
            blocks: 0,
        });

        let mut data: &[u8] = &frame[hdr_start + hdr.header_size_in_bytes as usize..];
        let mut tiles: Vec<&[u8]> = Vec::with_capacity(tile_cols * tile_rows);
        for tr in 0..tile_rows {
            for tc in 0..tile_cols {
                let last = tr == tile_rows - 1 && tc == tile_cols - 1;
                if last {
                    tiles.push(data);
                } else {
                    ensure(data.len() >= 4, "truncated tile size")?;
                    let sz = u32::from_be_bytes(data[..4].try_into().unwrap()) as usize;
                    data = &data[4..];
                    ensure(data.len() >= sz, "truncated tile data")?;
                    tiles.push(&data[..sz]);
                    data = &data[sz..];
                }
            }
        }

        // The loop-filter grids are only written when pixels are built, but
        // the walk's signature carries them.
        let mut grids = LfGrids {
            level8: vec![0; mi_cols * mi_rows],
            blk: vec![(0u8, 0u32, 0u32); mi_cols * mi_rows],
            otx: vec![0; mi_cols * mi_rows],
            skip_inter: vec![false; mi_cols * mi_rows],
            mi_cols,
            mi_rows,
        };
        // `above_context` / `above_seg_context` and the mi grid are
        // FRAME-scoped in libvpx (memset once per frame before the tile loop,
        // `vp9_decodeframe.c:2079-2083`; indexed by absolute mi_row/mi_col by
        // `set_skip_context`). Tile rows are disjoint in COLUMN only, so a
        // tile row k > 0 reads above state written by tile row k - 1 — the
        // same invariant the keyframe path documents. `above_context` is
        // sized by `mi_cols_aligned_to_sb`: a TX_32X32 block indexes up to 8
        // 4x4 units past the last mi col.
        let aligned_cols = mi_cols.div_ceil(SB_MI) * SB_MI;
        let mut ectx = [
            PlaneContexts::new(aligned_cols * 2),
            PlaneContexts::new(aligned_cols),
            PlaneContexts::new(aligned_cols),
        ];
        ectx.iter_mut().for_each(|p| p.above.fill(0));
        let mut mi = MiState::new(mi_cols, mi_rows);
        mi.above_seg.fill(0);
        let mut ti = 0usize;
        for tr in 0..tile_rows {
            let row_lo = ((sb64_rows * tr) >> hdr.tile_info.rows_log2) * SB_MI;
            let row_hi = (((sb64_rows * (tr + 1)) >> hdr.tile_info.rows_log2) * SB_MI).min(mi_rows);
            for tc in 0..tile_cols {
                let col_lo = ((sb64_cols * tc) >> hdr.tile_info.cols_log2) * SB_MI;
                let col_hi =
                    (((sb64_cols * (tc + 1)) >> hdr.tile_info.cols_log2) * SB_MI).min(mi_cols);
                if let Some(w) = self.iw.as_mut() {
                    w.tile_col_end = col_hi;
                }
                if crate::trace_enabled() {
                    eprintln!("TILE {} bytes frame={index}", tiles[ti].len());
                }
                let mut r = crate::bool::BoolDecoder::new(tiles[ti])?;
                ti += 1;
                for sb_row in (row_lo..row_hi).step_by(SB_MI) {
                    // Left entropy/partition contexts are zeroed at the start
                    // of every superblock row (the SB row's left edge has no
                    // neighbours); the above contexts stay frame-scoped.
                    ectx.iter_mut().for_each(|p| p.left = [0; 32]);
                    mi.left_seg = [0; 32];
                    for sb_col in (col_lo..col_hi).step_by(SB_MI) {
                        self.decode_partition(
                            &mut r,
                            &mut mi,
                            &mut ectx,
                            &mut grids,
                            &[[0u8; 2]; 8],
                            hdr,
                            sb_row,
                            sb_col,
                            4,
                            col_lo,
                            ch.tx_mode,
                            pixels,
                        )?;
                    }
                }
                ensure(r.overreads() == 0, "tile bool decoder desync")?;
            }
        }

        // The walk state is consumed below, so the block count the harness pins
        // has to be captured first.
        self.syntax_blocks = self.iw.as_ref().map_or(0, |w| w.blocks);
        let w = self.iw.take().expect("inter walk state");
        self.prev_mvs = w.mvs;
        self.prev_seg = w.seg_map;
        self.last = LastFrameFacts {
            width: hdr.width,
            height: hdr.height,
            intra_only: hdr.intra_only,
            show_frame: hdr.show_frame,
            key: false,
        };
        if hdr.refresh_frame_context {
            self.frame_ctxs[hdr.frame_context_idx as usize] = self.ctx.clone();
        }
        if crate::interdump_enabled() {
            eprintln!(
                "CHSUM frame={index} tx_mode={} ref_mode={} allow_hp={} interp={} tile_rows={} tile_cols={} pp12={:?} pp0={:?} pp13={:?}",
                ch.tx_mode,
                ch.reference_mode,
                hdr.allow_high_precision_mv,
                hdr.interpolation_filter as u8,
                hdr.tile_info.rows_log2,
                hdr.tile_info.cols_log2,
                self.ctx.partition[12],
                self.ctx.partition[0],
                self.ctx.partition[13]
            );
        }

        if !pixels {
            return Ok(None);
        }

        // Loop filter post-pass: same extents as the keyframe path, but the
        // per-cell level comes from the block's own reference and mode class
        // (`get_filter_level`, vp9_loopfilter.c:249).
        if hdr.loop_filter.level > 0 && std::env::var_os("EC_VP9_SKIP_LF").is_none() {
            let p = self.planes.as_mut().expect("frame scratch");
            let (vw, vh) = (mi_cols * 8, mi_rows * 8);
            grids.filter_frame(
                &mut p.y,
                p.ys,
                &mut p.u,
                &mut p.v,
                p.uvs,
                vw,
                vh,
                vh / 2,
                hdr.loop_filter.sharpness,
            );
        }

        let pic = self.take_picture(hdr);
        let frame = RefFrame {
            planes: std::sync::Arc::new(self.planes.take().expect("frame scratch")),
            width: hdr.width as u16,
            height: hdr.height as u16,
        };

        // Reference refresh (spec 8.10 / `swap_frame_buffers`): bit i of
        // `refresh_frame_flags` replaces slot i with the frame just decoded —
        // the LOOP-FILTERED frame, which is what a later frame's prediction
        // reads.
        for slot in 0..8 {
            if hdr.refresh_frame_flags & (1 << slot) != 0 {
                self.refs[slot] = Some(frame.clone());
            }
        }
        Ok(if hdr.show_frame { Some(pic) } else { None })
    }

    /// Crop the frame scratch to a [`Picture`].
    fn take_picture(&self, hdr: &FrameHeader) -> Picture {
        crop_picture(
            self.planes.as_ref().expect("frame scratch"),
            hdr.width as u16,
            hdr.height as u16,
        )
    }

    /// One inter block's syntax (`read_inter_frame_mode_info` + the MV field
    /// `vp9_read_mode_info` stores for the next frame).
    #[allow(clippy::too_many_arguments)]
    fn read_inter_block_info(
        &mut self,
        r: &mut crate::bool::BoolDecoder,
        mi: &MiState,
        hdr: &FrameHeader,
        row: usize,
        col: usize,
        sb_type: usize,
        x_mis: usize,
        y_mis: usize,
        tile_col_start: usize,
        tx_mode: u8,
    ) -> Result<MiInfo> {
        // BLOCK_4X4/4X8/8X4 are the SPLIT/VERT/HORZ partitions of an 8x8.
        let partition = match sb_type {
            0 => 3u8,
            1 => 2u8,
            2 => 1u8,
            _ => 0u8,
        };
        let w = self.iw.as_mut().expect("inter walk state");
        let prev_seg = std::mem::take(&mut w.prev_seg);
        let prev_mvs = std::mem::take(&mut w.prev_mvs);
        let ctx = crate::inter::InterBlockCtx {
            seg: &hdr.segmentation,
            ifs: w.ifs,
            sign_bias: w.sign_bias,
            allow_hp: hdr.allow_high_precision_mv,
            frame_interp: interp_filter_of(hdr),
            tx_mode,
            mi_rows: mi.mi_rows,
            mi_cols: mi.mi_cols,
            tile_col_start,
            tile_col_end: w.tile_col_end,
            last_seg: (!prev_seg.is_empty()).then_some(&prev_seg[..]),
            prev_mvs: (w.use_prev && !prev_mvs.is_empty()).then_some(&prev_mvs[..]),
        };
        let info = crate::inter::read_inter_frame_mode_info(
            r,
            mi,
            &ctx,
            &self.ctx,
            row,
            col,
            sb_type,
            x_mis,
            y_mis,
            partition,
            &mut w.seg_map,
        );
        if let Ok(info) = &info {
            w.blocks += 1;
            for dy in 0..y_mis {
                for dx in 0..x_mis {
                    let i = (row + dy) * mi.mi_cols + col + dx;
                    w.mvs[i] = crate::inter::MvRef {
                        ref_frame: info.ref_frame,
                        mv: info.mv,
                    };
                }
            }
            if crate::interdump_enabled() {
                eprintln!(
                    "MODE {} {} {} {} {} {} {} {} {} tpos=0 ref0={} ref1={} mvrow={} mvcol={} if={}",
                    w.frame_index,
                    row,
                    col,
                    sb_type,
                    info.tx_size,
                    info.mode,
                    info.uv_mode,
                    u8::from(info.skip),
                    info.segment,
                    info.ref_frame[0],
                    info.ref_frame[1],
                    info.mv[0].0,
                    info.mv[0].1,
                    info.interp_filter
                );
            }
        }
        w.prev_seg = prev_seg;
        w.prev_mvs = prev_mvs;
        info
    }

    /// Tokens for one inter block: same entropy walk as the keyframe path,
    /// but no prediction and no residual add. `is_inter` selects the
    /// coefficient-probability ref axis.
    #[allow(clippy::too_many_arguments)]
    fn decode_inter_block_tokens(
        &mut self,
        r: &mut crate::bool::BoolDecoder,
        mi: &MiState,
        ectx: &mut [PlaneContexts; 3],
        hdr: &FrameHeader,
        info: &MiInfo,
        row: usize,
        col: usize,
        sb_type: usize,
        bw: usize,
        bh: usize,
    ) -> Result<()> {
        let n4w = plane_n4_w(sb_type, bw);
        let n4h = plane_n4_h(sb_type, bh);
        if info.skip {
            // dec_reset_skip_context
            reset_skip_context(ectx, row, col, sb_type, n4w, n4h);
            return Ok(());
        }
        let mb_to_right = ((mi.mi_cols as isize - bw as isize - col as isize) * 64) as i32;
        let mb_to_bottom = ((mi.mi_rows as isize - bh as isize - row as isize) * 64) as i32;
        for plane in 0..3usize {
            let s = usize::from(plane != 0);
            let tx_size = if plane == 0 {
                info.tx_size
            } else {
                get_uv_tx_size(info.tx_size, sb_type)
            };
            let step = 1usize << tx_size;
            let max_blocks_wide =
                (n4w[plane] as i32 + (mb_to_right.min(0) >> (5 + s))).max(0) as usize;
            let max_blocks_high =
                (n4h[plane] as i32 + (mb_to_bottom.min(0) >> (5 + s))).max(0) as usize;
            for brow in (0..max_blocks_high).step_by(step) {
                for bcol in (0..max_blocks_wide).step_by(step) {
                    let _ = decode_inter_tx_tokens(
                        r,
                        ectx,
                        hdr,
                        info,
                        &self.ctx,
                        plane,
                        row,
                        col,
                        brow,
                        bcol,
                        tx_size,
                        sb_type,
                        bw,
                        bh,
                        mb_to_right,
                        mb_to_bottom,
                    );
                }
            }
        }
        Ok(())
    }

    /// `decode_block`'s inter arm with pixels (`vp9_decodeframe.c:990-1044`):
    /// `dec_build_inter_predictors_sb` for the whole block, then
    /// `reconstruct_inter_block` per transform block. Intra blocks inside an
    /// inter frame take the intra path (`:964-989`).
    #[allow(clippy::too_many_arguments)]
    fn reconstruct_inter_pixels(
        &mut self,
        r: &mut crate::bool::BoolDecoder,
        ectx: &mut [PlaneContexts; 3],
        grids: &mut LfGrids,
        hdr: &FrameHeader,
        info: &MiInfo,
        row: usize,
        col: usize,
        sb_type: usize,
        bw: usize,
        bh: usize,
        mi: &mut MiState,
        tile_col_start: usize,
    ) -> Result<()> {
        let x_mis = bw.min(mi.mi_cols - col);
        let y_mis = bh.min(mi.mi_rows - row);
        let n4w = plane_n4_w(sb_type, bw);
        let n4h = plane_n4_h(sb_type, bh);
        let mb_to_right = ((mi.mi_cols as isize - bw as isize - col as isize) * 64) as i32;
        let mb_to_bottom = ((mi.mi_rows as isize - bh as isize - row as isize) * 64) as i32;

        // `get_filter_level` (vp9_loopfilter.c:249):
        // `lvl[segment_id][mi->ref_frame[0]][mode_lf_lut[mi->mode]]`, with
        // `lvl` built by `vp9_loop_filter_frame_init` from the frame level,
        // the per-reference deltas and the per-mode-class deltas.
        let levels = hdr.loop_filter_levels(info.segment as usize);
        let ref0 = if info.is_inter {
            info.ref_frame[0].clamp(0, 3) as usize
        } else {
            // An intra block inside an inter frame: `mi->ref_frame[0]` is
            // INTRA_FRAME and its mode is an intra mode, so the mode class is
            // 0 and the reference slot is intra's.
            0
        };
        let level = levels[ref0][mode_lf_lut(info.mode)];
        for dy in 0..y_mis {
            for dx in 0..x_mis {
                let cell = (row + dy) * mi.mi_cols + col + dx;
                grids.level8[cell] = level;
                grids.blk[cell] = (sb_type as u8, row as u32, col as u32);
                grids.otx[cell] = if sb_type < 3 {
                    TX_4X4 as u8
                } else {
                    info.tx_size as u8
                };
                // `build_masks`'s early return is gated on
                // `mi->skip && is_inter_block(mi)` — a skipped inter block
                // keeps ONLY the above/left prediction masks.
                grids.skip_inter[cell] = info.is_inter && info.skip;
            }
        }

        // `if (mi->skip) dec_reset_skip_context(xd);` — libvpx does this at the
        // top of `decode_block` (decodeframe.c:960), BEFORE the intra/inter
        // split, so skipped intra blocks inside an inter frame reset too.
        if info.skip {
            reset_skip_context(ectx, row, col, sb_type, n4w, n4h);
        }

        if !info.is_inter {
            // `predict_and_reconstruct_intra_block` per transform block.
            for plane in 0..3usize {
                let s = usize::from(plane != 0);
                let tx_size = if plane == 0 {
                    info.tx_size
                } else {
                    get_uv_tx_size(info.tx_size, sb_type)
                };
                let step = 1usize << tx_size;
                let max_blocks_wide =
                    (n4w[plane] as i32 + (mb_to_right.min(0) >> (5 + s))).max(0) as usize;
                let max_blocks_high =
                    (n4h[plane] as i32 + (mb_to_bottom.min(0) >> (5 + s))).max(0) as usize;
                for brow in (0..max_blocks_high).step_by(step) {
                    for bcol in (0..max_blocks_wide).step_by(step) {
                        self.predict_and_reconstruct(
                            r,
                            ectx,
                            hdr,
                            info,
                            plane,
                            row,
                            col,
                            brow,
                            bcol,
                            tx_size,
                            sb_type,
                            bw,
                            bh,
                            mb_to_right,
                            mb_to_bottom,
                            // Intra prediction's left availability is
                            // tile-scoped (`tile->mi_col_start`), which the
                            // inter walk passes as the tile's first mi col.
                            tile_col_start,
                        )?;
                    }
                }
            }
            return Ok(());
        }

        // Prediction: `dec_build_inter_predictors_sb`, every reference, every
        // plane, before any token is read.
        {
            let planes = self.planes.as_mut().expect("frame scratch");
            let refs = &self.refs;
            let scratch = &mut self.mc_scratch;
            let temp = &mut self.mc_temp;
            for plane in 0..3usize {
                inter_predict_plane(
                    planes,
                    refs,
                    scratch,
                    temp,
                    hdr,
                    info,
                    plane,
                    row,
                    col,
                    &n4w,
                    &n4h,
                    mb_to_right,
                    mb_to_bottom,
                    sb_type,
                )?;
            }
        }

        if info.skip {
            return Ok(());
        }

        // Reconstruction: `reconstruct_inter_block` per transform block.
        let mut eobtotal = 0usize;
        for plane in 0..3usize {
            let s = usize::from(plane != 0);
            let tx_size = if plane == 0 {
                info.tx_size
            } else {
                get_uv_tx_size(info.tx_size, sb_type)
            };
            let step = 1usize << tx_size;
            let max_blocks_wide =
                (n4w[plane] as i32 + (mb_to_right.min(0) >> (5 + s))).max(0) as usize;
            let max_blocks_high =
                (n4h[plane] as i32 + (mb_to_bottom.min(0) >> (5 + s))).max(0) as usize;
            for brow in (0..max_blocks_high).step_by(step) {
                for bcol in (0..max_blocks_wide).step_by(step) {
                    let coef = decode_inter_tx_tokens(
                        r,
                        ectx,
                        hdr,
                        info,
                        &self.ctx,
                        plane,
                        row,
                        col,
                        brow,
                        bcol,
                        tx_size,
                        sb_type,
                        bw,
                        bh,
                        mb_to_right,
                        mb_to_bottom,
                    );
                    eobtotal += coef.eob;
                    if coef.eob == 0 {
                        continue;
                    }
                    // `inverse_transform_block_inter`: inter blocks always use
                    // the DEFAULT scan and DCT_DCT (`reconstruct_inter_block`,
                    // decodeframe.c:413).
                    let planes = self.planes.as_mut().expect("frame scratch");
                    let stride = planes.dims(plane).0;
                    let x0 = ((col * 8) >> s) + bcol * 4;
                    let y0 = ((row * 8) >> s) + brow * 4;
                    let dst_off = y0 * stride + x0;
                    let (data, _) = planes.plane(plane);
                    inverse_transform_add(
                        tx_size,
                        DCT_DCT,
                        &coef.coeffs,
                        data,
                        dst_off,
                        stride,
                        hdr.quantization.lossless(),
                    );
                }
            }
        }

        // `if (!less8x8 && eobtotal == 0) mi->skip = 1;  // skip loopfilter`
        // (decodeframe.c:1043).
        if sb_type >= 3 && eobtotal == 0 {
            for dy in 0..y_mis {
                for dx in 0..x_mis {
                    grids.skip_inter[(row + dy) * mi.mi_cols + col + dx] = true;
                }
            }
        }
        Ok(())
    }

    fn decode_keyframe(&mut self, hdr: &FrameHeader, frame: &[u8], tx_mode: u8) -> Result<Picture> {
        let width = hdr.width as usize;
        let height = hdr.height as usize;
        // The decode buffer is padded to whole superblocks: libvpx decodes a
        // block at the frame edge at its FULL size (decodeframe.c:1216,
        // `PARTITION_NONE` needs only `mi_row < mi_rows`) and writes the
        // overhang into the frame buffer's border. Clipping to 8 pixels
        // instead panicked on real 1080p content (a 32x32 block starting at
        // mi_row 132 writes 8 rows past a 1080-row buffer).
        let aw = width.div_ceil(64) * 64;
        let ah = height.div_ceil(64) * 64;
        self.planes = Some(Planes {
            y: vec![128u8; aw * ah],
            u: vec![128u8; aw / 2 * ah / 2],
            v: vec![128u8; aw / 2 * ah / 2],
            ys: aw,
            uvs: aw / 2,
            aw,
            ah,
        });
        let tail_start = std::env::var("EC_VP9_FORCE_TAIL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| {
                hdr.uncompressed_header_size as usize + hdr.header_size_in_bytes as usize
            });
        let tail: Vec<u8> = frame[tail_start..].to_vec();
        self.decode_keyframe_inner(hdr, tx_mode, &tail)
    }

    fn decode_keyframe_inner(
        &mut self,
        hdr: &FrameHeader,
        tx_mode: u8,
        tail: &[u8],
    ) -> Result<Picture> {
        let mi_cols = hdr.mi_cols() as usize;
        let mi_rows = hdr.mi_rows() as usize;
        let sb64_cols = mi_cols.div_ceil(SB_MI);
        let sb64_rows = mi_rows.div_ceil(SB_MI);
        let tile_cols = 1usize << hdr.tile_info.cols_log2;
        let tile_rows = 1usize << hdr.tile_info.rows_log2;

        // Tile data: every tile except the overall last carries a
        // 32-bit BIG-endian size prefix (spec 6.4; libvpx `get_tile_buffer`
        // reads it with `mem_get_be32`, vp9_decodeframe.c:1688/1690).
        let mut data: &[u8] = tail;
        let mut tiles: Vec<&[u8]> = Vec::with_capacity(tile_cols * tile_rows);
        for tr in 0..tile_rows {
            for tc in 0..tile_cols {
                let last = tr == tile_rows - 1 && tc == tile_cols - 1;
                if last {
                    tiles.push(data);
                } else {
                    ensure(data.len() >= 4, "truncated tile size")?;
                    let sz = u32::from_be_bytes(data[..4].try_into().unwrap()) as usize;
                    data = &data[4..];
                    ensure(data.len() >= sz, "truncated tile data")?;
                    tiles.push(&data[..sz]);
                    data = &data[sz..];
                }
            }
        }

        // Keyframe loop filter levels: intra ref, no mode class, per
        // segment (spec 8.8.1).
        let mut level_of_seg = [[0u8; 2]; 8];
        for (seg, slot) in level_of_seg.iter_mut().enumerate() {
            *slot = hdr.loop_filter_levels(seg)[0];
        }

        let mut grids = LfGrids {
            level8: vec![0; mi_cols * mi_rows],
            blk: vec![(0u8, 0u32, 0u32); mi_cols * mi_rows],
            otx: vec![0; mi_cols * mi_rows],
            skip_inter: vec![false; mi_cols * mi_rows],
            mi_cols,
            mi_rows,
        };
        let mut ti = 0usize;
        // `above_context` / `above_seg_context` and the mi grid are
        // FRAME-scoped in libvpx: `cm->above_context` is memset once per
        // frame BEFORE the tile loop (vp9_decodeframe.c:2079-2080 / :2082-2083,
        // inside the :2077-2083 block), the
        // shared `cm->mi_grid_visible` accumulates every tile's records, and
        // the buffers are indexed by ABSOLUTE mi_row/mi_col
        // (`set_skip_context`, vp9_onyxc_int.h:405-413). A tile row k > 0
        // therefore reads above state written by tile row k - 1 — tiles are
        // disjoint in COLUMN only. Allocating these per tile would discard
        // that above state and mis-gate `xd->above_mi`, so they live here and
        // are zeroed exactly once per frame.
        // libvpx sizes `cm->above_context` by `mi_cols_aligned_to_sb` and the
        // mi grid stride by `mi_cols`; the above arrays are written/indexed up
        // to a whole TX_32X32 (8 4x4 units) past the last mi col, so a
        // non-multiple-of-8 width would otherwise index out of bounds.
        let aligned_cols = mi_cols.div_ceil(SB_MI) * SB_MI;
        let mut ectx = [
            PlaneContexts::new(aligned_cols * 2),
            PlaneContexts::new(aligned_cols),
            PlaneContexts::new(aligned_cols),
        ];
        ectx.iter_mut().for_each(|p| p.above.fill(0));
        let mut mi = MiState::new(mi_cols, mi_rows);
        mi.above_seg.fill(0);
        for tr in 0..tile_rows {
            let row_lo = ((sb64_rows * tr) >> hdr.tile_info.rows_log2) * SB_MI;
            let row_hi = (((sb64_rows * (tr + 1)) >> hdr.tile_info.rows_log2) * SB_MI).min(mi_rows);
            for tc in 0..tile_cols {
                let col_lo = ((sb64_cols * tc) >> hdr.tile_info.cols_log2) * SB_MI;
                let col_hi =
                    (((sb64_cols * (tc + 1)) >> hdr.tile_info.cols_log2) * SB_MI).min(mi_cols);
                if crate::trace_enabled() {
                    eprintln!(
                        "TILE {} bytes first={:02x?}",
                        tiles[ti].len(),
                        &tiles[ti][..8.min(tiles[ti].len())]
                    );
                }
                let mut r = crate::bool::BoolDecoder::new(tiles[ti])?;
                ti += 1;
                for sb_row in (row_lo..row_hi).step_by(SB_MI) {
                    // libvpx zeroes the left ENTROPY and PARTITION contexts at
                    // the start of EVERY superblock row, not just the tile's
                    // first one (vp9_decodeframe.c:2260-2261 plain
                    // `decode_tiles`; the row-mt twin is :2117-2118 and the
                    // per-tile-row twins :1828-1829 / :1901-1902): the left
                    // edge of an SB row has no pixels to its left. Our luma
                    // left index wraps at mi_row 16, so a stale value from two
                    // SB rows earlier would otherwise be read here.
                    // `above_context`/`above_seg_context` are frame-scoped
                    // (memset once per frame at :2079-2083, before the tile
                    // loop) and indexed by ABSOLUTE mi_col, so they persist
                    // across tile rows and are NOT reset here.
                    ectx.iter_mut().for_each(|p| p.left = [0; 32]);
                    mi.left_seg = [0; 32];
                    for sb_col in (col_lo..col_hi).step_by(SB_MI) {
                        self.decode_partition(
                            &mut r,
                            &mut mi,
                            &mut ectx,
                            &mut grids,
                            &level_of_seg,
                            hdr,
                            sb_row,
                            sb_col,
                            4,
                            col_lo,
                            tx_mode,
                            true,
                        )?;
                    }
                }
                ensure(r.overreads() == 0, "tile bool decoder desync")?;
            }
        }

        // Loop filter post-pass (level 0 disables it for the frame).
        // The extents are the MI-aligned VISIBLE plane (libvpx's masks clear
        // everything past `mi_cols`/`mi_rows`), not the padded buffer size.
        if hdr.loop_filter.level > 0 && std::env::var_os("EC_VP9_SKIP_LF").is_none() {
            let p = self.planes.as_mut().expect("frame scratch");
            let (vw, vh) = (mi_cols * 8, mi_rows * 8);
            grids.filter_frame(
                &mut p.y,
                p.ys,
                &mut p.u,
                &mut p.v,
                p.uvs,
                vw,
                vh,
                vh / 2,
                hdr.loop_filter.sharpness,
            );
        }

        Ok(self.take_picture(hdr))
    }

    /// `decode_partition` (decodeframe.c:1173).
    #[allow(clippy::too_many_arguments)]
    fn decode_partition(
        &mut self,
        r: &mut crate::bool::BoolDecoder,
        mi: &mut MiState,
        ectx: &mut [PlaneContexts; 3],
        grids: &mut LfGrids,
        level_of_seg: &[[u8; 2]; 8],
        hdr: &FrameHeader,
        row: usize,
        col: usize,
        n4: usize,
        tile_col_start: usize,
        tx_mode: u8,
        // Whether this walk reconstructs pixels. Consulted by the inter
        // branch only: a keyframe always reconstructs its intra blocks, and
        // the syntax-only inter walk parses tokens without predicting.
        pixels: bool,
    ) -> Result<()> {
        if row >= mi.mi_rows || col >= mi.mi_cols {
            return Ok(());
        }
        let n8 = n4 - 1;
        let hbs = (1usize << n8) >> 1;
        let has_rows = row + hbs < mi.mi_rows;
        let has_cols = col + hbs < mi.mi_cols;
        // libvpx reads the partition symbol at EVERY level INCLUDING 8x8
        // (decodeframe.c:1188 runs before the `!hbs` branch): the 8x8
        // symbol picks 8x8 / 8x4 / 4x8 / 4x4-split — the sub-8x8 shapes.
        // Only the 4x4 level has no symbol, and SPLIT recursion never
        // reaches it (it stops at n4 == 1).
        let partition = {
            let ctx = mi.partition_ctx(row, col, n8 as u32);
            let probs = &self.ctx.partition[ctx];
            if has_rows && has_cols {
                r.read_tree(&PARTITION_TREE, probs)
            } else if !has_rows && has_cols {
                if r.read_bool(probs[1]) { 3 } else { 1 }
            } else if has_rows && !has_cols {
                if r.read_bool(probs[2]) { 3 } else { 2 }
            } else {
                3
            }
        };
        let bsize = bsize_of_n4(n4);
        let subsize = SUBSIZE_LOOKUP[partition as usize * 13 + bsize] as usize;
        if hbs == 0 {
            self.decode_block(
                r,
                mi,
                ectx,
                grids,
                level_of_seg,
                hdr,
                row,
                col,
                subsize,
                1,
                1,
                tile_col_start,
                tx_mode,
                pixels,
            )?;
        } else {
            match partition {
                0 => self.decode_block(
                    r,
                    mi,
                    ectx,
                    grids,
                    level_of_seg,
                    hdr,
                    row,
                    col,
                    subsize,
                    n4,
                    n4,
                    tile_col_start,
                    tx_mode,
                    pixels,
                )?,
                1 => {
                    self.decode_block(
                        r,
                        mi,
                        ectx,
                        grids,
                        level_of_seg,
                        hdr,
                        row,
                        col,
                        subsize,
                        n4,
                        n8,
                        tile_col_start,
                        tx_mode,
                        pixels,
                    )?;
                    if has_rows {
                        self.decode_block(
                            r,
                            mi,
                            ectx,
                            grids,
                            level_of_seg,
                            hdr,
                            row + hbs,
                            col,
                            subsize,
                            n4,
                            n8,
                            tile_col_start,
                            tx_mode,
                            pixels,
                        )?;
                    }
                }
                2 => {
                    self.decode_block(
                        r,
                        mi,
                        ectx,
                        grids,
                        level_of_seg,
                        hdr,
                        row,
                        col,
                        subsize,
                        n8,
                        n4,
                        tile_col_start,
                        tx_mode,
                        pixels,
                    )?;
                    if has_cols {
                        self.decode_block(
                            r,
                            mi,
                            ectx,
                            grids,
                            level_of_seg,
                            hdr,
                            row,
                            col + hbs,
                            subsize,
                            n8,
                            n4,
                            tile_col_start,
                            tx_mode,
                            pixels,
                        )?;
                    }
                }
                _ => {
                    for (rr, cc) in [
                        (row, col),
                        (row, col + hbs),
                        (row + hbs, col),
                        (row + hbs, col + hbs),
                    ] {
                        self.decode_partition(
                            r,
                            mi,
                            ectx,
                            grids,
                            level_of_seg,
                            hdr,
                            rr,
                            cc,
                            n8,
                            tile_col_start,
                            tx_mode,
                            pixels,
                        )?;
                    }
                }
            }
        }
        if bsize >= 3 && (bsize == 3 || partition != 3) {
            mi.update_partition_ctx(row, col, subsize, 1 << n8);
        }
        Ok(())
    }

    /// `decode_block` intra path (decodeframe.c:913).
    #[allow(clippy::too_many_arguments)]
    fn decode_block(
        &mut self,
        r: &mut crate::bool::BoolDecoder,
        mi: &mut MiState,
        ectx: &mut [PlaneContexts; 3],
        grids: &mut LfGrids,
        level_of_seg: &[[u8; 2]; 8],
        hdr: &FrameHeader,
        row: usize,
        col: usize,
        sb_type: usize,
        bwl: usize,
        bhl: usize,
        tile_col_start: usize,
        tx_mode: u8,
        pixels: bool,
    ) -> Result<()> {
        let bw = 1 << (bwl - 1);
        let bh = 1 << (bhl - 1);
        let x_mis = bw.min(mi.mi_cols - col);
        let y_mis = bh.min(mi.mi_rows - row);

        // `frame_is_intra_only` (onyxc_int.h:363): a keyframe OR an
        // intra-only frame walks the intra mode-info path.
        let intra_frame = hdr.frame_type == FrameType::Key || hdr.intra_only;
        let info = if intra_frame {
            read_intra_frame_mode_info(
                r,
                mi,
                &hdr.segmentation,
                &self.ctx.partition,
                row,
                col,
                sb_type,
                x_mis,
                y_mis,
                tile_col_start,
                tx_mode,
                &self.ctx,
            )?
        } else {
            self.read_inter_block_info(
                r,
                mi,
                hdr,
                row,
                col,
                sb_type,
                x_mis,
                y_mis,
                tile_col_start,
                tx_mode,
            )?
        };
        if !intra_frame {
            if pixels {
                // Full inter decode: prediction, tokens, residual. The
                // per-cell loop-filter level is the block's own reference and
                // mode class (`get_filter_level`), so the grids are written
                // inside the reconstruction rather than from `level_of_seg`.
                self.reconstruct_inter_pixels(
                    r,
                    ectx,
                    grids,
                    hdr,
                    &info,
                    row,
                    col,
                    sb_type,
                    bw,
                    bh,
                    mi,
                    tile_col_start,
                )?;
            } else {
                // Inter syntax only: tokens keep the entropy stream aligned,
                // the block lands in the mode grid, and no pixel is touched.
                self.decode_inter_block_tokens(r, mi, ectx, hdr, &info, row, col, sb_type, bw, bh)?;
            }
            for dy in 0..y_mis {
                for dx in 0..x_mis {
                    if crate::mvdbg2_enabled() && row + dy == 7 && col + dx >= 30 && col + dx <= 40
                    {
                        eprintln!(
                            "GWR cell=({} {}) idx={} from=({row} {col}) dy={dy} dx={dx} sb={sb_type} bw={bw} bh={bh} x_mis={x_mis} y_mis={y_mis} mv={:?} ref0={} mi_cols={}",
                            row + dy,
                            col + dx,
                            (row + dy) * mi.mi_cols + col + dx,
                            info.mv,
                            info.ref_frame[0],
                            mi.mi_cols
                        );
                    }
                    mi.grid[(row + dy) * mi.mi_cols + col + dx] = Some(info.clone());
                }
            }
            return Ok(());
        }

        // Per-plane 4x4 counts. Sub-8x8 shapes still occupy the WHOLE 8x8
        // mi area (libvpx set_plane_n4 with bw=bh=1): luma walks 2x2 4x4
        // TUs, chroma is one 4x4 (n4 >> subsampling), coded once per 8x8.
        let n4w = if sb_type < 3 {
            [2, 1, 1]
        } else {
            [bw * 2, bw, bw]
        };
        let n4h = if sb_type < 3 {
            [2, 1, 1]
        } else {
            [bh * 2, bh, bh]
        };
        let mb_to_right = ((mi.mi_cols as isize - bw as isize - col as isize) * 64) as i32;
        let mb_to_bottom = ((mi.mi_rows as isize - bh as isize - row as isize) * 64) as i32;

        let level = level_of_seg[info.segment as usize][0];
        for dy in 0..y_mis {
            for dx in 0..x_mis {
                let cell = (row + dy) * mi.mi_cols + col + dx;
                grids.level8[cell] = level;
                grids.blk[cell] = (sb_type as u8, row as u32, col as u32);
                grids.otx[cell] = if sb_type < 3 {
                    TX_4X4 as u8
                } else {
                    info.tx_size as u8
                };
            }
        }

        if info.skip {
            for plane in 0..3usize {
                let s = usize::from(plane != 0);
                let offx = (col << 1) >> s;
                let offy = (row << 1) >> s;
                for i in 0..n4w[plane] {
                    ectx[plane].above[offx + i] = 0;
                }
                let lrows = 32usize >> s;
                for i in 0..n4h[plane].min(lrows) {
                    ectx[plane].left[(offy + i) % lrows] = 0;
                }
            }
        }

        for plane in 0..3usize {
            let s = usize::from(plane != 0);
            let tx_size = if plane == 0 {
                info.tx_size
            } else {
                get_uv_tx_size(info.tx_size, sb_type)
            };
            let step = 1usize << tx_size;
            let max_blocks_wide =
                (n4w[plane] as i32 + (mb_to_right.min(0) >> (5 + s))).max(0) as usize;
            let max_blocks_high =
                (n4h[plane] as i32 + (mb_to_bottom.min(0) >> (5 + s))).max(0) as usize;
            for brow in (0..max_blocks_high).step_by(step) {
                for bcol in (0..max_blocks_wide).step_by(step) {
                    self.predict_and_reconstruct(
                        r,
                        ectx,
                        hdr,
                        &info,
                        plane,
                        row,
                        col,
                        brow,
                        bcol,
                        tx_size,
                        sb_type,
                        bw,
                        bh,
                        mb_to_right,
                        mb_to_bottom,
                        tile_col_start,
                    )?;
                }
            }
        }

        for dy in 0..y_mis {
            for dx in 0..x_mis {
                mi.grid[(row + dy) * mi.mi_cols + col + dx] = Some(info.clone());
            }
        }
        Ok(())
    }

    /// `predict_and_reconstruct_intra_block` (decodeframe.c:309):
    /// predict, then tokens, then residual add; plus
    /// `vp9_decode_block_tokens` context bookkeeping (the v1.15 edge
    /// shifts included).
    #[allow(clippy::too_many_arguments)]
    fn predict_and_reconstruct(
        &mut self,
        r: &mut crate::bool::BoolDecoder,
        ectx: &mut [PlaneContexts; 3],
        hdr: &FrameHeader,
        info: &MiInfo,
        plane: usize,
        mi_row: usize,
        mi_col: usize,
        tx_row: usize,
        tx_col: usize,
        tx_size: usize,
        sb_type: usize,
        bw: usize,
        bh: usize,
        mb_to_right: i32,
        mb_to_bottom: i32,
        tile_col_start: usize,
    ) -> Result<()> {
        let s = usize::from(plane != 0);
        let mode = if plane == 0 {
            if sb_type < 3 {
                info.bmi[(tx_row << 1) + tx_col]
            } else {
                info.mode
            }
        } else {
            info.uv_mode
        };
        let planes = self.planes.as_mut().expect("frame scratch");
        let stride = planes.dims(plane).0;
        let bs = 4 << tx_size;
        let x0 = ((mi_col * 8) >> s) + tx_col * 4;
        let y0 = ((mi_row * 8) >> s) + tx_row * 4;
        // vp9_predict_intra_block `have_top = loff || (xd->above_mi != NULL)`
        // (vp9_reconintra.c:411); `above_mi` is FRAME-gated
        // (`set_mi_row_col`, vp9_onyxc_int.h:430: `mi_row != 0`), NOT
        // tile-row-gated. The above row of a tile row k > 0 is decoded by
        // tile row k - 1 and is available.
        let up = tx_row != 0 || mi_row > 0;
        let lft = tx_col != 0 || mi_col > tile_col_start;
        // vp9_predict_intra_block `have_right`: the tx block is not in the
        // last tx column of ITS OWN block (pd->n4_w units) — above-right
        // past the block's right edge belongs to a not-yet-decoded
        // neighbour, so it must stage as unavailable.
        let rgt = tx_col + (1usize << tx_size) < bw * 2 >> s;

        // `xd->cur_buf->y_width/y_height` (vp9_reconintra.c:288-292) are the
        // MI-ALIGNED (8-multiple) buffer extent, not the visible coded size:
        // confirmed by instrumenting the oracle's staging for a 320x242 frame
        // (`fh=248`, not 242). The extend-vs-direct DECISION is `mb_to_*_edge`
        // (`:302/326/354`, passed in as `bot_ext`/`right_ext`).
        let vw = (hdr.mi_cols() as usize * 8) >> s;
        let vh = (hdr.mi_rows() as usize * 8) >> s;

        // 1. Intra prediction (reads reconstructed neighbours).
        {
            let (data, _) = planes.plane(plane);
            build_intra_predictors(
                data,
                stride,
                vw,
                vh,
                x0,
                y0,
                mode,
                bs,
                up,
                lft,
                rgt,
                mb_to_bottom < 0,
                mb_to_right < 0,
            );
        }

        if info.skip {
            return Ok(());
        }

        // 2. Tokens.
        let lossless = hdr.quantization.lossless();
        let tx_type = if plane != 0 || lossless {
            DCT_DCT
        } else {
            MODE_TO_TXTYPE_LOOKUP[mode as usize] as usize
        };
        let (scan, nb) = scan_for(tx_size, tx_type);
        let q = hdr.segment_dequant(info.segment as usize);
        let dequant: [i16; 2] = if plane == 0 {
            [q.luma_dc, q.luma_ac]
        } else {
            [q.chroma_dc, q.chroma_ac]
        };
        let ptype = usize::from(plane != 0);
        let ax = ((mi_col << 1) >> s) + tx_col;
        let lrows = 32usize >> s;
        let ay = (((mi_row << 1) >> s) + tx_row) % lrows;
        // libvpx reads the context as !!*(uintN_t *)a / l over the WHOLE
        // tx-block span (uint16 for 8x8, uint32 for 16x16, uint64 for
        // 32x32), not just the first entry.
        let nblocks = 1usize << tx_size;
        let ctx_in = usize::from(ectx[plane].above[ax..ax + nblocks].iter().any(|&v| v != 0))
            + usize::from(ectx[plane].left[ay..ay + nblocks].iter().any(|&v| v != 0));
        if crate::trace_enabled() {
            eprintln!(
                "CTX p={} prob={:?} ax={} ay={} a={} l={} ctx={}",
                plane,
                self.ctx.coef[tx_size][ptype][0][0][ctx_in],
                ax,
                ay,
                ectx[plane].above[ax],
                ectx[plane].left[ay],
                ctx_in
            );
        }
        if crate::trace_enabled() {
            eprintln!(
                "TXB p={} x={} y={} tx={} a={} l={} deq={},{}",
                plane,
                x0,
                y0,
                tx_size,
                ectx[plane].above[ax],
                ectx[plane].left[ay],
                dequant[0],
                dequant[1]
            );
        }
        let coef = decode_coefs(
            r,
            ptype,
            tx_size,
            &dequant,
            ctx_in,
            scan,
            nb,
            &self.ctx.coef[tx_size],
            false,
            (x0, y0),
            plane,
        );

        // 3. Context write-back (vp9_decode_block_tokens, v1.15 shifts).
        let n4w = bw * 2 >> s;
        let n4h = bh * 2 >> s;
        let maxw = (n4w as i32 + (mb_to_right.min(0) >> (5 + s))).max(0) as usize;
        let maxh = (n4h as i32 + (mb_to_bottom.min(0) >> (5 + s))).max(0) as usize;
        let nblocks = 1usize << tx_size;
        let shift_a = if maxw > 0 && nblocks + tx_col > maxw {
            (nblocks - (maxw - tx_col)) * 8
        } else {
            0
        };
        let shift_l = if maxh > 0 && nblocks + tx_row > maxh {
            (nblocks - (maxh - tx_row)) * 8
        } else {
            0
        };
        write_tx_context(ectx, plane, tx_size, ax, ay, coef.eob > 0, shift_a, shift_l);
        if crate::tokdbg_enabled() {
            eprintln!(
                "TDW row={mi_row} col={mi_col} plane={plane} brow={tx_row} bcol={tx_col} tx={tx_size} eob={} sa={} sl={} aw={:?} lw={:?} intra=1",
                coef.eob,
                shift_a,
                shift_l,
                &ectx[plane].above[ax..ax + nblocks],
                &ectx[plane].left[ay..ay + nblocks],
            );
        }

        // 4. Residual add.
        if coef.eob > 0 {
            let dst_off = y0 * stride + x0;
            let (data, _) = planes.plane(plane);
            let probe = crate::trace_enabled()
                && plane == 0
                && std::env::var("EC_VP9_PROBE_TU")
                    .ok()
                    .and_then(|s| {
                        let mut it = s.split(',');
                        Some((it.next()?.parse::<usize>().ok()?, it.next()?.parse().ok()?))
                    })
                    .is_some_and(|(px, py)| x0 == px && y0 == py);
            let t: Option<Vec<Vec<u8>>> = if probe {
                Some(
                    (0..4usize)
                        .map(|rr| data[dst_off + rr * stride..dst_off + rr * stride + 4].to_vec())
                        .collect(),
                )
            } else {
                None
            };
            inverse_transform_add(
                tx_size,
                tx_type,
                &coef.coeffs,
                data,
                dst_off,
                stride,
                lossless,
            );
            if let Some(before) = t {
                let after: Vec<Vec<u8>> = (0..4usize)
                    .map(|rr| data[dst_off + rr * stride..dst_off + rr * stride + 4].to_vec())
                    .collect();
                eprintln!("RC x0={x0} y0={y0} tx={tx_size} mode={mode}");
                for rr in 0..4 {
                    eprintln!("  pred{rr}={:?} after{rr}={:?}", before[rr], after[rr]);
                }
            }
        }
        Ok(())
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

/// `BLOCK_SIZE` from `log2(4x4s per side)`.
fn bsize_of_n4(n4: usize) -> usize {
    match n4 {
        4 => 12, // BLOCK_64X64
        3 => 9,  // BLOCK_32X32
        2 => 6,  // BLOCK_16X16
        1 => 3,  // BLOCK_8X8
        _ => 0,  // BLOCK_4X4 (recursion never reaches n4 == 0)
    }
}

/// Crop an aligned plane set to a displayable [`Picture`]: rows are packed to
/// the visible width, so `stride` describes the RETURNED buffers, not the
/// aligned decode buffer.
fn crop_picture(planes: &Planes, width: u16, height: u16) -> Picture {
    let (w, h) = (width as usize, height as usize);
    let crop = |src: &[u8], stride: usize, cw: usize, ch: usize| -> Vec<u8> {
        let mut out = Vec::with_capacity(cw * ch);
        for row in 0..ch {
            out.extend_from_slice(&src[row * stride..row * stride + cw]);
        }
        out
    };
    // Chroma CEILS: the 4:2:0 plane of an odd extent carries one extra
    // half-sample column/row (`uv_crop_width = (w + 1) / 2`).
    let (cw, chh) = ((w + 1) / 2, (h + 1) / 2);
    Picture {
        y: crop(&planes.y, planes.ys, w, h),
        u: crop(&planes.u, planes.uvs, cw, chh),
        v: crop(&planes.v, planes.uvs, cw, chh),
        width,
        height,
        stride: w,
        uv_stride: cw,
    }
}

/// `pd->n4_w` (`set_plane_n4`, decodeframe.c:809): the plane block's width in
/// its own 4x4 units. Sub-8x8 shapes occupy a whole 8x8 mi cell
/// (`set_plane_n4(xd, bw=1, bh=1)`) so luma walks 2x2 and chroma 1x1.
fn plane_n4_w(sb_type: usize, bw: usize) -> [usize; 3] {
    if sb_type < 3 {
        [2, 1, 1]
    } else {
        [bw * 2, bw, bw]
    }
}

/// `pd->n4_h`, the height twin of [`plane_n4_w`].
fn plane_n4_h(sb_type: usize, bh: usize) -> [usize; 3] {
    if sb_type < 3 {
        [2, 1, 1]
    } else {
        [bh * 2, bh, bh]
    }
}

/// `dec_reset_skip_context` (`vp9_decodeframe.c:800`).
fn reset_skip_context(
    ectx: &mut [PlaneContexts; 3],
    row: usize,
    col: usize,
    sb_type: usize,
    n4w: [usize; 3],
    n4h: [usize; 3],
) {
    let _ = sb_type;
    for plane in 0..3usize {
        let s = usize::from(plane != 0);
        let offx = (col << 1) >> s;
        let offy = (row << 1) >> s;
        for i in 0..n4w[plane] {
            ectx[plane].above[offx + i] = 0;
        }
        let lrows = 32usize >> s;
        for i in 0..n4h[plane].min(lrows) {
            ectx[plane].left[(offy + i) % lrows] = 0;
        }
    }
    if crate::tokdbg_enabled() {
        // Mirror the oracle's `SKIPCTX` print: the CLEARED window, indexed from
        // the reset offset (libvpx's `left_context` is already offset to the
        // block), not the whole rolling buffer from base index 0.
        let mut line = format!("TDK row={row} col={col} n4w={n4w:?} n4h={n4h:?}");
        for plane in 0..3usize {
            let s = usize::from(plane != 0);
            let offx = (col << 1) >> s;
            let offy = (row << 1) >> s;
            let lrows = 32usize >> s;
            let a: Vec<u8> = (0..n4w[plane])
                .map(|i| ectx[plane].above[offx + i])
                .collect();
            let l: Vec<u8> = (0..n4h[plane].min(lrows))
                .map(|i| ectx[plane].left[(offy + i) % lrows])
                .collect();
            line.push_str(&format!(" p{plane} a={a:?} l={l:?}"));
        }
        eprintln!("{line}");
    }
}

/// The entropy-context write-back of one transform block
/// (`vp9_decode_block_tokens`, `vp9_detokenize.c:273-323`): the block's
/// `eob` spreads over its `1 << tx_size` 4x4 slots, and `shift_a`/`shift_l`
/// (the `get_ctx_shift` frame-edge rule) zero the slots past the frame edge.
///
/// libvpx applies the shift to a value whose WIDTH IS THE WINDOW'S — a
/// `(uint16_t)` for `TX_8X8`, `(uint32_t)` for `TX_16X16`, `(uint64_t)` for
/// `TX_32X32` — so the shift zeroes the window's TAIL. Shifting a 64-bit
/// spread instead only zeroes bytes past the window and leaves the in-window
/// tail set: the last-SB-row chroma blocks then hand a stale 1 to the next
/// row's context read (the frame-72 desync).
fn write_tx_context(
    ectx: &mut [PlaneContexts; 3],
    plane: usize,
    tx_size: usize,
    ax: usize,
    ay: usize,
    eob: bool,
    shift_a: usize,
    shift_l: usize,
) {
    const ONES: u64 = 0x0101_0101_0101_0101;
    let nblocks = 1usize << tx_size;
    let eob_pos = u64::from(eob);
    // Narrow to the window first (libvpx's cast), then apply the edge shift;
    // the clamp keeps a fully-outside window (libvpx: undefined shift) total.
    let narrow = 64 - 8 * nblocks;
    let spread_a = ((eob_pos * ONES) >> narrow) >> shift_a.min(8 * nblocks);
    let spread_l = ((eob_pos * ONES) >> narrow) >> shift_l.min(8 * nblocks);
    for k in 0..nblocks {
        ectx[plane].above[ax + k] = (spread_a >> (8 * k)) as u8;
        ectx[plane].left[ay + k] = (spread_l >> (8 * k)) as u8;
    }
}

/// `mode_lf_lut` (`vp9/common/vp9_loopfilter.c:223`):
/// `{0 × INTRA_MODES, 1, 1, 0, 1}` — NEARESTMV, NEARMV and NEWMV take the
/// inter mode delta, ZEROMV does not, and intra modes never do.
fn mode_lf_lut(mode: u8) -> usize {
    match mode {
        10 | 11 | 13 => 1,
        _ => 0,
    }
}

/// One inter transform block's tokens plus the context write-back
/// (`vp9_decode_block_tokens`, `decodeframe.c:414`). Inter blocks always use
/// the DEFAULT scan and `DCT_DCT`.
#[allow(clippy::too_many_arguments)]
fn decode_inter_tx_tokens(
    r: &mut crate::bool::BoolDecoder,
    ectx: &mut [PlaneContexts; 3],
    hdr: &FrameHeader,
    info: &MiInfo,
    fc: &FrameContext,
    plane: usize,
    row: usize,
    col: usize,
    brow: usize,
    bcol: usize,
    tx_size: usize,
    sb_type: usize,
    bw: usize,
    bh: usize,
    mb_to_right: i32,
    mb_to_bottom: i32,
) -> crate::tokens::CoeffBlock {
    let s = usize::from(plane != 0);
    // `predict_and_reconstruct_intra_block` (decodeframe.c:320-345): the luma
    // mode of a sub-8x8 block is the SUB-BLOCK's, and an intra block inside an
    // inter frame picks its tx type from that mode — `reconstruct_inter_block`
    // hard-codes DCT_DCT instead, which is why the inter guard is here.
    let tx_type = if plane != 0 || hdr.quantization.lossless() || info.is_inter {
        DCT_DCT
    } else {
        let mode = if sb_type < 3 {
            info.bmi[(brow << 1) + bcol]
        } else {
            info.mode
        };
        MODE_TO_TXTYPE_LOOKUP[mode as usize] as usize
    };
    let (scan, nb) = scan_for(tx_size, tx_type);
    let q = hdr.segment_dequant(info.segment as usize);
    let dequant: [i16; 2] = if plane == 0 {
        [q.luma_dc, q.luma_ac]
    } else {
        [q.chroma_dc, q.chroma_ac]
    };
    let ptype = usize::from(plane != 0);
    let ax = ((col << 1) >> s) + bcol;
    let lrows = 32usize >> s;
    let ay = (((row << 1) >> s) + brow) % lrows;
    let nblocks = 1usize << tx_size;
    let ctx_in = usize::from(ectx[plane].above[ax..ax + nblocks].iter().any(|&v| v != 0))
        + usize::from(ectx[plane].left[ay..ay + nblocks].iter().any(|&v| v != 0));
    if crate::tokdbg_enabled() {
        eprintln!(
            "TDB row={row} col={col} plane={plane} brow={brow} bcol={bcol} tx={tx_size} sbt={sb_type} ptype={ptype} inter={} mode={} tt={tx_type} ax={ax} ay={ay} cin={ctx_in} a={:?} l={:?}",
            u8::from(info.is_inter),
            info.mode,
            &ectx[plane].above[ax..ax + nblocks],
            &ectx[plane].left[ay..ay + nblocks],
        );
    }
    let coef = decode_coefs(
        r,
        ptype,
        tx_size,
        &dequant,
        ctx_in,
        scan,
        nb,
        &fc.coef[tx_size],
        info.is_inter,
        (0, 0),
        plane,
    );
    // vp9_decode_block_tokens context write-back (v1.15 edge shifts).
    let n4w_s = bw * 2 >> s;
    let n4h_s = bh * 2 >> s;
    let maxw = (n4w_s as i32 + (mb_to_right.min(0) >> (5 + s))).max(0) as usize;
    let maxh = (n4h_s as i32 + (mb_to_bottom.min(0) >> (5 + s))).max(0) as usize;
    let shift_a = if maxw > 0 && nblocks + bcol > maxw {
        (nblocks - (maxw - bcol)) * 8
    } else {
        0
    };
    let shift_l = if maxh > 0 && nblocks + brow > maxh {
        (nblocks - (maxh - brow)) * 8
    } else {
        0
    };
    write_tx_context(ectx, plane, tx_size, ax, ay, coef.eob > 0, shift_a, shift_l);
    if crate::tokdbg_enabled() {
        eprintln!(
            "TDW row={row} col={col} plane={plane} brow={brow} bcol={bcol} tx={tx_size} eob={} sa={} sl={} aw={:?} lw={:?}",
            coef.eob,
            shift_a,
            shift_l,
            &ectx[plane].above[ax..ax + nblocks],
            &ectx[plane].left[ay..ay + nblocks],
        );
    }
    coef
}

/// `dec_build_inter_predictors_sb` (`vp9_decodeframe.c:731`) for one plane:
/// every reference's prediction for one block, in reference order (compound
/// blocks average the second into the first).
#[allow(clippy::too_many_arguments)]
fn inter_predict_plane(
    planes: &mut Planes,
    refs: &[Option<RefFrame>; 8],
    scratch: &mut Vec<u8>,
    temp: &mut Vec<u8>,
    hdr: &FrameHeader,
    info: &MiInfo,
    plane: usize,
    row: usize,
    col: usize,
    n4w: &[usize; 3],
    n4h: &[usize; 3],
    mb_to_right: i32,
    mb_to_bottom: i32,
    sb_type: usize,
) -> Result<()> {
    let s = usize::from(plane != 0);
    let ss_x = s;
    let ss_y = s;
    let pw = n4w[plane] * 4;
    let ph = n4h[plane] * 4;
    let mb_left = -((col as i32) * 64);
    let mb_top = -((row as i32) * 64);
    let is_compound = info.ref_frame[1] > crate::inter::INTRA_FRAME;
    let refs_used = 1 + usize::from(is_compound);
    let kernel = usize::from(info.interp_filter);
    let dst_stride = planes.dims(plane).0;
    for rf in 0..refs_used {
        let ref_name = info.ref_frame[rf];
        let slot = hdr.ref_frame_idx[(ref_name - 1) as usize] as usize;
        let Some(ref_frame) = refs[slot].as_ref() else {
            return Err(corrupt("vp9 inter prediction read an empty reference slot"));
        };
        let (ref_data, ref_stride) = ref_frame.planes.plane_ref(plane);
        // `vp9_setup_scale_factors_for_frame` (`vp9_decodeframe.c:2758`): the
        // scale from the REFERENCE's coded luma size to this frame's, shared
        // by every plane. `vp9_is_valid_scale` is enforced where libvpx
        // enforces it — at prediction time, for the references actually used.
        let sf = ScaleFactors::setup(
            ref_frame.width as usize,
            ref_frame.height as usize,
            hdr.width as usize,
            hdr.height as usize,
        );
        if !sf.is_valid() {
            return Err(Error::unsupported(
                "vp9 reference scale",
                format!(
                    "reference `{ref_name}` in slot {slot} is {}x{} against a {}x{} frame; \
                     `valid_ref_frame_size` rejects that ratio",
                    ref_frame.width, ref_frame.height, hdr.width, hdr.height
                ),
            ));
        }
        let refp = RefPlane {
            data: ref_data,
            stride: ref_stride,
            // `y_crop_width` / `uv_crop_width`: luma is the coded size, chroma
            // CEILS (`(w + 1) / 2`). libvpx stores `uv_crop_width` as
            // `(y_crop_width + 1) >> 1` (`vpx_scale/yv12config.c`), so a
            // plain `>> 1` is one short whenever the coded extent is odd and
            // the predictor's border/inside test then clips the last chroma
            // column (or row).
            width: (ref_frame.width as usize + s) >> s,
            height: (ref_frame.height as usize + s) >> s,
        };
        if sb_type < 3 {
            // libvpx builds one 4x4 prediction per sub-block, with the MV from
            // `average_split_mvs` (luma: the sub-block's own MV; 4:2:0 chroma:
            // the q4-rounded average of the four).
            let n4x = n4w[plane];
            let n4y = n4h[plane];
            for y in 0..n4y {
                for xx in 0..n4x {
                    let i = y * n4x + xx;
                    let mv = if plane == 0 {
                        split_mv(&info.bmi_mv, i, rf)
                    } else {
                        average_split_mvs_chroma(&info.bmi_mv, rf)
                    };
                    let (data, _) = planes.plane(plane);
                    let dst_off =
                        (((row * 8) >> s) + y * 4) * dst_stride + ((col * 8) >> s) + xx * 4;
                    build_inter_predictors(
                        &mut data[dst_off..],
                        dst_stride,
                        &refp,
                        col,
                        row,
                        xx * 4,
                        y * 4,
                        4,
                        4,
                        mv,
                        mb_left,
                        mb_to_right,
                        mb_top,
                        mb_to_bottom,
                        ss_x,
                        ss_y,
                        kernel,
                        rf == 1,
                        &sf,
                        scratch,
                        temp,
                    );
                }
            }
        } else {
            let (data, _) = planes.plane(plane);
            let dst_off = ((row * 8) >> s) * dst_stride + ((col * 8) >> s);
            build_inter_predictors(
                &mut data[dst_off..],
                dst_stride,
                &refp,
                col,
                row,
                0,
                0,
                pw,
                ph,
                info.mv[rf],
                mb_left,
                mb_to_right,
                mb_top,
                mb_to_bottom,
                ss_x,
                ss_y,
                kernel,
                rf == 1,
                &sf,
                scratch,
                temp,
            );
        }
    }
    Ok(())
}

/// `read_interp_filter` (`vp9_decodeframe.c:1482`): the header's filter is a
/// literal index through `literal_to_filter[] = {EIGHTTAP_SMOOTH, EIGHTTAP,
/// EIGHTTAP_SHARP, BILINEAR}` — the spec's literal order is NOT libvpx's
/// filter numbering (`#define EIGHTTAP 0`, `EIGHTTAP_SMOOTH 1`,
/// `EIGHTTAP_SHARP 2`, `BILINEAR 3`), and `vp9_filter_kernels` (the kernel
/// table) is indexed by the latter. A switchable frame stores SWITCHABLE (4)
/// and picks per block, where the tree's leaves already use libvpx numbering.
fn interp_filter_of(hdr: &FrameHeader) -> u8 {
    const LITERAL_TO_FILTER: [u8; 4] = [1, 0, 2, 3];
    let v = hdr.interpolation_filter as u8;
    if v == crate::inter::SWITCHABLE {
        v
    } else {
        LITERAL_TO_FILTER[v as usize]
    }
}

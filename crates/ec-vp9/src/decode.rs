//! Frame decoding: superframe split, uncompressed header (via
//! [`ec_vp9_syntax::Vp9Parser`]), compressed header, tile parsing,
//! partition recursion, intra prediction, token decoding,
//! reconstruction and the loop filter (spec 6, 7, 8; libvpx
//! `vp9_decodeframe.c`).
//!
//! Lane scope: profile-0 8-bit 4:2:0 keyframes. Inter frames and other
//! profiles/subsamplings are refused by name; `show_existing_frame`
//! returns the buffered reference picture instead of pretending to
//! decode.

use crate::header::{FrameContext, read_compressed_header};
use crate::intra::build_intra_predictors;
use crate::loopfilter::LfGrids;
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
    /// Chroma row pitch of the returned planes (== `width / 2`).
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
    refs: [Option<Picture>; 8],
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
    /// Mode-info blocks parsed in the most recent inter frame (0 otherwise).
    syntax_blocks: usize,
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
            syntax_blocks: 0,
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
                Some(p) => Ok(Some(p.clone())),
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
        if hdr.frame_type != FrameType::Key {
            return Err(Error::unsupported(
                "vp9 inter",
                "this lane decodes keyframes only",
            ));
        }
        self.decode_key_frame(&hdr, frame)
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
        for slot in self.refs.iter_mut() {
            *slot = Some(pic.clone());
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
                return Err(Error::unsupported(
                    "vp9 intra-only",
                    "the inter-syntax lane covers inter frames only",
                ));
            } else {
                self.decode_inter_syntax_frame(&hdr, sub, index)?;
            }
        }
        Ok(())
    }

    /// Inter-frame syntax walk: compressed header, then every block's mode
    /// info and MVs. Tokens are decoded to keep the entropy stream aligned;
    /// nothing is predicted or reconstructed.
    fn decode_inter_syntax_frame(&mut self, hdr: &FrameHeader, frame: &[u8], index: usize) -> Result<()> {
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

        // The loop-filter grids are never written on this path (no
        // reconstruction), but the walk's signature carries them.
        let mut grids = LfGrids {
            level8: vec![0; mi_cols * mi_rows],
            blk: vec![(0u8, 0u32, 0u32); mi_cols * mi_rows],
            otx: vec![0; mi_cols * mi_rows],
            mi_cols,
            mi_rows,
        };
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
                let mut ectx = [
                    PlaneContexts::new(mi_cols * 2),
                    PlaneContexts::new(mi_cols),
                    PlaneContexts::new(mi_cols),
                ];
                let mut mi = MiState::new(mi_cols, mi_rows);
                ectx.iter_mut().for_each(|p| p.above.fill(0));
                mi.above_seg.fill(0);
                for sb_row in (row_lo..row_hi).step_by(SB_MI) {
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
        Ok(())
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
            frame_interp: hdr.interpolation_filter as u8,
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
        if info.skip {
            // dec_reset_skip_context
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
            return Ok(());
        }
        let mb_to_right = ((mi.mi_cols as isize - bw as isize - col as isize) * 64) as i32;
        let mb_to_bottom = ((mi.mi_rows as isize - bh as isize - row as isize) * 64) as i32;
        let lossless = hdr.quantization.lossless();
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
                    let tx_type = if plane != 0 || lossless || info.is_inter {
                        DCT_DCT
                    } else {
                        MODE_TO_TXTYPE_LOOKUP[info.mode as usize] as usize
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
                    let coef = decode_coefs(
                        r,
                        ptype,
                        tx_size,
                        &dequant,
                        ctx_in,
                        scan,
                        nb,
                        &self.ctx.coef[tx_size],
                        info.is_inter,
                        (0, 0),
                        plane,
                    );
                    // vp9_decode_block_tokens context write-back.
                    let n4w_s = bw * 2 >> s;
                    let n4h_s = bh * 2 >> s;
                    let maxw =
                        (n4w_s as i32 + (mb_to_right.min(0) >> (5 + s))).max(0) as usize;
                    let maxh =
                        (n4h_s as i32 + (mb_to_bottom.min(0) >> (5 + s))).max(0) as usize;
                    let eob_pos = u64::from(coef.eob > 0);
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
                    let spread_a = (eob_pos * 0x0101_0101_0101_0101) >> shift_a;
                    let spread_l = (eob_pos * 0x0101_0101_0101_0101) >> shift_l;
                    for k in 0..nblocks {
                        ectx[plane].above[ax + k] = (spread_a >> (8 * k)) as u8;
                        ectx[plane].left[ay + k] = (spread_l >> (8 * k)) as u8;
                    }
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
        let res = self.decode_keyframe_inner(hdr, tx_mode, &tail);
        self.planes = None;
        res
    }

    fn decode_keyframe_inner(
        &mut self,
        hdr: &FrameHeader,
        tx_mode: u8,
        tail: &[u8],
    ) -> Result<Picture> {
        let width = hdr.width as usize;
        let height = hdr.height as usize;
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

        let crop = |src: &[u8], stride: usize, w: usize, h: usize| -> Vec<u8> {
            let mut out = Vec::with_capacity(w * h);
            for row in 0..h {
                out.extend_from_slice(&src[row * stride..row * stride + w]);
            }
            out
        };
        let planes = self.planes.as_ref().expect("frame scratch");
        Ok(Picture {
            y: crop(&planes.y, planes.ys, width, height),
            u: crop(&planes.u, planes.uvs, width / 2, height / 2),
            v: crop(&planes.v, planes.uvs, width / 2, height / 2),
            width: width as u16,
            height: height as u16,
            // The crop above packs rows: `stride` describes the RETURNED
            // buffers, not the aligned decode buffer (`aw`).
            stride: width,
            uv_stride: width / 2,
        })
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
    ) -> Result<()> {
        let bw = 1 << (bwl - 1);
        let bh = 1 << (bhl - 1);
        let x_mis = bw.min(mi.mi_cols - col);
        let y_mis = bh.min(mi.mi_rows - row);

        let info = if hdr.frame_type == FrameType::Key {
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
        if hdr.frame_type != FrameType::Key {
            // Inter syntax only: tokens keep the entropy stream aligned, the
            // block lands in the mode grid, and no pixel is touched.
            self.decode_inter_block_tokens(r, mi, ectx, hdr, &info, row, col, sb_type, bw, bh)?;
            for dy in 0..y_mis {
                for dx in 0..x_mis {
                    if crate::mvdbg2_enabled()
                        && row + dy == 7
                        && col + dx >= 30
                        && col + dx <= 40
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
        let eob_pos = usize::from(coef.eob > 0);
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
        match tx_size {
            TX_4X4 => {
                ectx[plane].above[ax] = eob_pos as u8;
                ectx[plane].left[ay] = eob_pos as u8;
            }
            TX_8X8 => {
                let va = ((eob_pos as u16) * 0x0101) >> shift_a;
                ectx[plane].above[ax] = va as u8;
                ectx[plane].above[ax + 1] = (va >> 8) as u8;
                let vl = ((eob_pos as u16) * 0x0101) >> shift_l;
                ectx[plane].left[ay] = vl as u8;
                ectx[plane].left[ay + 1] = (vl >> 8) as u8;
            }
            TX_16X16 => {
                for k in 0..4 {
                    let va = ((eob_pos as u32) * 0x01010101) >> shift_a;
                    ectx[plane].above[ax + k] = (va >> (8 * k)) as u8;
                    let vl = ((eob_pos as u32) * 0x01010101) >> shift_l;
                    ectx[plane].left[ay + k] = (vl >> (8 * k)) as u8;
                }
            }
            _ => {
                for k in 0..8 {
                    let va = ((eob_pos as u64) * 0x0101010101010101) >> shift_a;
                    ectx[plane].above[ax + k] = (va >> (8 * k)) as u8;
                    let vl = ((eob_pos as u64) * 0x0101010101010101) >> shift_l;
                    ectx[plane].left[ay + k] = (vl >> (8 * k)) as u8;
                }
            }
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

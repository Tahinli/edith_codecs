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

use crate::header::{read_compressed_header, FrameContext};
use crate::intra::build_intra_predictors;
use crate::loopfilter::LfGrids;
use crate::modes::{read_intra_frame_mode_info, MiInfo, MiState};
use crate::tables::*;
use crate::tokens::{decode_coefs, scan_for, PlaneContexts};
use crate::transform::inverse_transform_add;
use crate::{Error, Result};
use ec_vp9_syntax::{superframe, FrameHeader, FrameType, Vp9Parser};

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
    /// Luma row pitch of the returned planes.
    pub stride: usize,
    /// Chroma row pitch of the returned planes.
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
}

impl Decoder {
    /// Create an empty decoder; the first key frame sets the size.
    pub fn new() -> Self {
        Decoder {
            parser: Vp9Parser::new(),
            ctx: FrameContext::new(true),
            refs: [const { None }; 8],
            planes: None,
        }
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
        // A keyframe resets the frame context (spec 7.2) before the
        // compressed header codes this frame's updates on top.
        self.ctx = FrameContext::new(hdr.frame_type == FrameType::Key);
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
        let tx_mode = read_compressed_header(
            &frame[hdr_start..hdr_start + hdr.header_size_in_bytes as usize],
            &mut self.ctx,
            hdr.quantization.lossless(),
        )?;

        let pic = self.decode_keyframe(&hdr, frame, tx_mode)?;
        // Reference refresh (spec 8.10): a keyframe updates all slots.
        for slot in self.refs.iter_mut() {
            *slot = Some(pic.clone());
        }
        Ok(if hdr.show_frame { Some(pic) } else { None })
    }

    fn decode_keyframe(&mut self, hdr: &FrameHeader, frame: &[u8], tx_mode: u8) -> Result<Picture> {
        let width = hdr.width as usize;
        let height = hdr.height as usize;
        let aw = width.div_ceil(8) * 8;
        let ah = height.div_ceil(8) * 8;
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
        let aw = self.planes.as_ref().expect("frame scratch").aw;
        let ah = self.planes.as_ref().expect("frame scratch").ah;
        let width = hdr.width as usize;
        let height = hdr.height as usize;
        let mi_cols = hdr.mi_cols() as usize;
        let mi_rows = hdr.mi_rows() as usize;
        let sb64_cols = mi_cols.div_ceil(SB_MI);
        let sb64_rows = mi_rows.div_ceil(SB_MI);
        let tile_cols = 1usize << hdr.tile_info.cols_log2;
        let tile_rows = 1usize << hdr.tile_info.rows_log2;

        // Tile data: every tile except the overall last carries a
        // 32-bit LE size prefix (spec 6.4).
        let mut data: &[u8] = tail;
        let mut tiles: Vec<&[u8]> = Vec::with_capacity(tile_cols * tile_rows);
        for tr in 0..tile_rows {
            for tc in 0..tile_cols {
                let last = tr == tile_rows - 1 && tc == tile_cols - 1;
                if last {
                    tiles.push(data);
                } else {
                    ensure(data.len() >= 4, "truncated tile size")?;
                    let sz = u32::from_le_bytes(data[..4].try_into().unwrap()) as usize;
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
            tx4: vec![0; mi_cols * 2 * mi_rows * 2],
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
                if crate::trace_enabled() {
                    eprintln!(
                        "TILE {} bytes first={:02x?}",
                        tiles[ti].len(),
                        &tiles[ti][..8.min(tiles[ti].len())]
                    );
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
                    if sb_row == row_lo {
                        ectx.iter_mut().for_each(|p| p.left = [0; 32]);
                        mi.left_seg = [0; 32];
                    }
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
                            row_lo,
                            col_lo,
                            tx_mode,
                        )?;
                    }
                }
                ensure(r.overreads() == 0, "tile bool decoder desync")?;
            }
        }

        // Loop filter post-pass (level 0 disables it for the frame).
        if hdr.loop_filter.level > 0 && std::env::var_os("EC_VP9_SKIP_LF").is_none() {
            let p = self.planes.as_mut().expect("frame scratch");
            grids.filter_frame(
                &mut p.y,
                p.ys,
                &mut p.u,
                &mut p.v,
                p.uvs,
                aw,
                ah,
                aw / 2,
                ah / 2,
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
            stride: aw,
            uv_stride: aw / 2,
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
        tile_row_start: usize,
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
                if r.read_bool(probs[1]) {
                    3
                } else {
                    1
                }
            } else if has_rows && !has_cols {
                if r.read_bool(probs[2]) {
                    3
                } else {
                    2
                }
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
                tile_row_start,
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
                    tile_row_start,
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
                        tile_row_start,
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
                            tile_row_start,
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
                        tile_row_start,
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
                            tile_row_start,
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
                            tile_row_start,
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
        tile_row_start: usize,
        tile_col_start: usize,
        tx_mode: u8,
    ) -> Result<()> {
        let bw = 1 << (bwl - 1);
        let bh = 1 << (bhl - 1);
        let x_mis = bw.min(mi.mi_cols - col);
        let y_mis = bh.min(mi.mi_rows - row);

        let info = read_intra_frame_mode_info(
            r,
            mi,
            &hdr.segmentation,
            &self.ctx.partition,
            row,
            col,
            sb_type,
            x_mis,
            y_mis,
            tile_row_start,
            tile_col_start,
            tx_mode,
            &self.ctx.skip,
        )?;

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
        // Luma tx4 grid (block-uniform in VP9).
        {
            let maxw = (n4w[0] as i32 + (mb_to_right.min(0) >> 5)).max(0) as usize;
            let maxh = (n4h[0] as i32 + (mb_to_bottom.min(0) >> 5)).max(0) as usize;
            for rr in 0..maxh.min(n4h[0]) {
                for cc in 0..maxw.min(n4w[0]) {
                    grids.tx4[(row * 2 + rr) * mi.mi_cols * 2 + col * 2 + cc] = info.tx_size as u8;
                }
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
                        tile_row_start,
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
        tile_row_start: usize,
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
        let (pw, ph) = planes.dims(plane);
        let stride = pw;
        let bs = 4 << tx_size;
        let x0 = ((mi_col * 8) >> s) + tx_col * 4;
        let y0 = ((mi_row * 8) >> s) + tx_row * 4;
        let up = tx_row != 0 || mi_row > tile_row_start;
        let lft = tx_col != 0 || mi_col > tile_col_start;
        // vp9_predict_intra_block `have_right`: the tx block is not in the
        // last tx column of ITS OWN block (pd->n4_w units) — above-right
        // past the block's right edge belongs to a not-yet-decoded
        // neighbour, so it must stage as unavailable.
        let rgt = tx_col + (1usize << tx_size) < bw * 2 >> s;

        // 1. Intra prediction (reads reconstructed neighbours).
        {
            let (data, _) = planes.plane(plane);
            build_intra_predictors(data, stride, pw, ph, x0, y0, mode, bs, up, lft, rgt);
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

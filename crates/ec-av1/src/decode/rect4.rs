//! Rect4 and 16-wide block decoders, including the intrabc and strip paths. //! av1split: moved out of `decode/mod.rs`; the items are unchanged apart from visibility. (D18)

use super::*;

/// Decodes one 32x8 / 8x32 strip of a 32x32-level `PARTITION_HORZ_4` /
/// `PARTITION_VERT_4` (lane-tx64x16 r3): [`decode_block_rect`]'s 4:1 sibling,
/// addressed by REAL mi coordinates for the same reason [`decode_leaf_rect`]
/// is -- strips 1 and 3 start 8 px into a 16-px [`SUB`] cell, which an
/// `at`-derived `px = c * SUB` cannot name. `prev_strip` carries the previous
/// strip's mode so this one's mode context sees its true above/left
/// neighbour inside the same 32x32 (`decode_leaf_rect`'s convention).
///
/// Tables: LUMA `TX_32X8`/`TX_8X32` averages to the 16x16 entropy context
/// ([`TxbSet::LumaRect32x8`], 256 positions) and scans with
/// [`SCAN_32X8`]/[`SCAN_8X32`]; CHROMA `TX_16X4`/`TX_4X16` averages to the
/// 8x8 one ([`TxbSet::Chroma8`], 64 positions) and scans with
/// [`SCAN_16X4`]/[`SCAN_4X16`]. Neither shape takes the rect sqrt2 scale --
/// `get_rect_tx_log_ratio` fires at ratio 1 only, and this is ratio 2
/// (`inverse_transform_2d_typed_wh`'s own `rect_scale`).
///
/// A split transform (`tx_depth != 0`) refuses by name: `depth_to_tx_size` of
/// a 4:1 strip is not simply `bw.min(bh) >> (depth - 1)` the way a 2:1
/// strip's is, so [`decode_rect_split`]'s per-unit walk cannot be reused
/// unexamined.
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_block_rect4(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    strip_mi: (usize, usize),
    bw: usize,
    bh: usize,
    prev_strip: Option<((usize, usize), usize)>,
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    enable_filter_intra: bool,
    allow_screen_content_tools: bool,
    base_q_idx: u8,
    tx_select: bool,
    fctx: &crate::decode::FrameCtx,
) -> Result<(usize, usize)> {
    let _ = base_q_idx;
    let (mi_r, mi_c) = strip_mi;
    let (r, c) = (mi_r / (SUB / MI), mi_c / (SUB / MI));
    let (mi_w, mi_h) = (bw / MI, bh / MI);
    let mut above_mode = neighbours.above_mode[c];
    let mut left_mode = neighbours.left_mode[r];
    if let Some(((pr, pc), pmode)) = prev_strip {
        if pc == mi_c && mi_r == pr + mi_h {
            above_mode = pmode;
        } else if pr == mi_r && mi_c == pc + mi_w {
            left_mode = pmode;
        }
    }
    // lane-rectsplitx r2, ROOT CAUSE of the 1:4 mismatch: mi-exact +
    // availability-correct neighbour modes (see
    // [`Neighbours::modes_above_left_mi`]). The coarse 16x16 bands alias every
    // 8-px strip of a `HORZ_4`/`VERT_4` onto one cell.
    let (above_mode, left_mode) =
        neighbours.modes_above_left_mi(mi_r, mi_c, (above_mode, left_mode));
    // lane-kf900 r4: this strip's own palette-Y mode ctx and colour cache.
    // Passing `None`/`&[]` read `palette_y_mode` off CDF row 0 and every
    // `palette_colors_y` cache hit off an EMPTY cache -- right symbol count,
    // wrong interval, so a screen-content tile desynced at the first strip
    // whose neighbour used a palette (class [[cdf-row-held-constant]]).
    let pal_at = (mi_r, mi_c);
    let (palette_ctx, palette_cache) = neighbours.palette_ctx_and_cache_mi(pal_at);
    let palette_uv_cache = neighbours.palette_uv_cache_mi(pal_at);
    let (
        skip,
        mode,
        angle_delta_y,
        uv_mode,
        angle_delta_uv,
        alpha,
        filter_intra,
        palette_y,
        palette_uv,
        intrabc,
    ) = read_intra_mode_rect(
        dec,
        cdfs,
        above_mode,
        left_mode,
        true,
        true,
        bw,
        bh,
        enable_filter_intra,
        neighbours.skip_txfm_ctx(mi_r, mi_c),
        allow_screen_content_tools,
        Some((palette_ctx, &palette_cache)),
        &palette_uv_cache,
        mi_r,
        mi_c,
        fctx,
    )?;
    if let Some((dv_row, dv_col)) = intrabc {
        decode_intrabc_rect(
            dec,
            cdfs,
            neighbours,
            strip_mi,
            (bw, bh),
            skip,
            (dv_row, dv_col),
            y,
            u,
            v,
            base_q_idx,
            fctx,
        )?;
        return Ok((DC_PRED, DC_PRED));
    }
    // lane-kf900 r4: palette prediction on this shape, exactly as
    // [`decode_block_rect`] does one aspect ratio up -- the colour-index map
    // mapped through the base colours IS the prediction (it reads no edge
    // pixels), handed to each `reconstruct_rect` through [`set_palette_pred`].
    if palette_y.is_some() || palette_uv.is_some() {
        hit!(RECT4_PALETTE_HITS);
    }
    let palette_y_buf: Option<Vec<u16>> = palette_y
        .as_ref()
        .map(|p| p.map.iter().map(|&i| p.colors[i as usize]).collect());
    let palette_uv_bufs: Option<(Vec<u16>, Vec<u16>)> = palette_uv.as_ref().map(|p| {
        (
            p.map.iter().map(|&i| p.u_colors[i as usize]).collect(),
            p.map.iter().map(|&i| p.v_colors[i as usize]).collect(),
        )
    });
    let (pal_y_size, pal_y_colors) = palette_y
        .as_ref()
        .map_or((0, [0u16; 8]), |p| (p.size, p.colors));
    let (pal_uv_size, pal_uv_colors) = palette_uv
        .as_ref()
        .map_or((0, [0u16; 8]), |p| (p.size, p.u_colors));
    let smooth_neighbor = is_smooth_mode(above_mode) || is_smooth_mode(left_mode);
    if smooth_neighbor {
        hit!(SMOOTH_LUMA_HITS);
    }
    // lane-rectsplitx r2: this strip's REAL mi, not the [`SUB`] cell's
    // top-left -- a 1:4 strip starts 8 px into a 16-px cell, so
    // `r * (SUB / MI)` names a different mi column and the chroma
    // edge-filter strength was read from the wrong neighbour (measured:
    // seed 43, the last 8x32 strip at luma (152,32), chroma-only mismatch
    // with luma bit-exact).
    let smooth_neighbor_uv = neighbours.smooth_uv_neighbour(mi_r, mi_c, r, c, fctx);
    if filter_intra.is_some() {
        hit!(FILTER_INTRA_RECT_HITS);
        if bw > bh {
            hit!(FILTER_INTRA_RECT4_HORZ_HITS);
        } else {
            hit!(FILTER_INTRA_RECT4_VERT_HITS);
        }
    }
    let uv_predict_mode = if uv_mode == UV_CFL_PRED {
        DC_PRED
    } else {
        uv_mode
    };
    // lane-av1mixloss: the lossless-segment early return (`decodeframe.c:1203`)
    // precedes `TX_MODE_SELECT`, so a lossless-segment rect4 block codes no
    // depth symbol here either (class sweep of the sub8 leaf's fix).
    let depth = if tx_select && !lossless(fctx) {
        // lane-av1txbands: EVERY frame now reads the real `TXFM_CONTEXT`
        // bands (`get_tx_size_context`, libaom `pred_common.h:342`) -- an
        // intra-only frame publishes them from every block too
        // ([`Neighbours::fill_lf_grid_rect`]). Measured on the 10-bit
        // 1920x792 128-superblock stream,
        // decode-order frame 1: the 32x8 HORZ_4 strip at mi(48,176) read
        // `tx_size_cat2` row 1 where libaom reads row 2.
        let ctx = tx_size_context_txfm_rect(neighbours, (mi_r, mi_c), bw, bh, fctx);
        let d = dec.symbol(&mut cdfs.tx_size_cat2[ctx]);
        if crate::envflags::env_flag!("EC_TRACE_MODE_STEP") {
            eprintln!(
                "EC_ISTEP mi_row={mi_r} mi_col={mi_c} name=tx_depth val={d} ctx={ctx} cat=2 rng={}",
                dec.debug_state().0
            );
        }
        d
    } else {
        0
    };
    // lane-t900 r1: this strip's own `set_txfm_ctxs` when it is an intra
    // block of an INTER frame -- see [`publish_txfm_bands_if_in_inter`].
    publish_txfm_bands_if_in_inter(
        neighbours,
        (mi_r, mi_c),
        depth_to_tx_wh(bw, bh, depth, fctx),
        (bw, bh),
        fctx,
    );
    if depth != 0 || lossless(fctx) {
        // lane-rectsplitx r1: the per-transform-unit walk, at this strip's
        // real mi position (strips 1 and 3 of a HORZ_4 start 8 px into a
        // 16-px [`SUB`] cell). depth 1 is the RECT unit TX_16X8/TX_8X16
        // (`sub_tx_size_map[TX_32X8] == TX_16X8`), depth 2 the square TX_8X8;
        // both interleave in every real stream.
        hit!(TX_DEPTH_HITS);
        let (tx_w, tx_h) = depth_to_tx_wh(bw, bh, depth, fctx);
        // lane-rectsplitx r2: both depths ship together, as the r1 handoff
        // predicted -- depth 1 fires FIRST in every attempt that splits at
        // all, so a depth-2-only lift could never be gated. The r1 mismatch
        // was never the rect unit: it was the neighbour MODE the strip reads
        // (see `modes_above_left_mi`).
        let which = if depth == 1 {
            &RECT4_SPLIT_DEPTH1_HITS
        } else {
            &RECT4_SPLIT_DEPTH2_HITS
        };
        hit_do! { which.with(|c| c.set(c.get() + 1)); }
        let modes = RectStripModes {
            skip,
            mode,
            angle_delta_y,
            uv_predict_mode,
            angle_delta_uv,
            alpha,
            filter_intra,
            smooth_neighbor,
            smooth_neighbor_uv,
            palette_y: palette_y_buf.clone(),
            palette_uv: palette_uv_bufs.clone(),
        };
        if palette_y.is_some() || palette_uv.is_some() {
            hit!(PALETTE_SPLIT_TX_HITS);
        }
        decode_rect_split(
            dec,
            cdfs,
            neighbours,
            (mi_r, mi_c),
            bw,
            bh,
            tx_w,
            tx_h,
            &modes,
            y,
            u,
            v,
            base_q_idx,
            false,
            fctx,
        )?;
        record_strip_palette(
            neighbours,
            pal_at,
            bw,
            bh,
            pal_y_size,
            pal_y_colors,
            pal_uv_size,
            pal_uv_colors,
        );
        if bw > bh {
            hit!(RECT4_32_HORZ_HITS);
        } else {
            hit!(RECT4_32_VERT_HITS);
        }
        if !skip {
            hit!(RECT4_COEFF_HITS);
        }
        // lane-frame36 r2 (same shape as `decode_block_rect64`'s split arm,
        // swept this round): `set_txfm_ctxs` runs for EVERY block of an inter
        // frame, so this 1:4 strip -- which is also the INTER frame's reader
        // for 32x8/8x32 ([`decode_intra_rect4_in_inter`]) -- publishes its
        // transform size in both TXFM_CONTEXT bands. Without it the next block
        // reads the row's stale value.
        txfm_partition_update_rect(neighbours, (mi_r, mi_c), (tx_w, tx_h), (bw, bh));
        return Ok((mode, uv_predict_mode));
    }
    // The coarse (16-px [`SUB`] cell) mode/side arrays cannot name a strip
    // that is 8 px tall, so each strip stamps every cell it touches and the
    // LAST strip wins -- which is the right answer for the block below/right
    // of the 32x32, the only reader outside it. Written after this strip's
    // own context read above, never before.
    for cell in 0..bw.div_ceil(SUB) {
        neighbours.above_mode[c + cell] = mode;
        neighbours.above_uv_mode[c + cell] = uv_predict_mode;
        neighbours.above_side[c + cell] = bw;
    }
    for cell in 0..bh.div_ceil(SUB) {
        neighbours.left_mode[r + cell] = mode;
        neighbours.left_uv_mode[r + cell] = uv_predict_mode;
        neighbours.left_side[r + cell] = bh;
    }
    // ...and the mi-EXACT maps the coarse cells alias away, exactly as
    // [`decode_leaf_rect`] does (the split arm above records these through
    // `record_mi_luma_rect`). Without this an unsplit 32x8 strip left
    // `sub8_mode_row` holding the previous block, and the next 32x32's
    // strips read a stale `kf_y_mode` row (measured: mi(4,8) on the band
    // fixture).
    neighbours.record_mode_mi(mi_r, mi_c, mi_w, mi_h, mode);
    neighbours.record_uv_mode_mi(mi_r, mi_c, mi_w, mi_h, uv_predict_mode);
    let (px, py) = (mi_c * MI, mi_r * MI);
    let (cpx, cpy) = (px >> ss_x(fctx), py >> ss_y(fctx));
    let (chroma_w, chroma_h) = (bw >> ss_x(fctx), bh >> ss_y(fctx));
    let reach = Reach::of_rect(bw, bh, px, py, y.width, y.height, fctx);
    // lane-av1-444: the strip's chroma shape follows the subsampling.
    // 4:2:0 codes 16x4/4x16 (`TxbSet::Chroma8`); 4:4:4 codes the strip
    // itself, 32x8/8x32 (`get_txsize_entropy_ctx(TX_32X8)` =
    // `(wide_log2 + high_log2 + 1) >> 1` = the 16x16 set,
    // `TxbSet::Chroma16`). The caller set is closed -- this reader only ever
    // sees 32x8 / 8x32 luma. lane-av1422lift: this sentence used to end "and
    // the sequence header admits ss 0/0 and 1/1 only -- so the table is
    // exhaustive", which was a claim about a guard rather than about this
    // reader. The header admits ss (1,1), (1,0) and (0,0), and the (1,0)
    // 4:2:2 arm is dispatched a few lines below (`chroma422_rect32`), so the
    // table's exhaustiveness comes from the reader's own luma shape -- which
    // is what the sentence now claims.
    let chroma422_rect32 = (chroma_w, chroma_h) == (4, 32) && ss_x(fctx) == 1 && ss_y(fctx) == 0;
    let (chroma_set, chroma_scan): (TxbSet, &[u16]) = match (chroma_w, chroma_h) {
        (16, 4) => (TxbSet::Chroma8, &SCAN_16X4[..]),
        (4, 16) => (TxbSet::Chroma8, &SCAN_4X16[..]),
        (32, 8) => (TxbSet::Chroma16, &SCAN_32X8[..]),
        (8, 32) => (TxbSet::Chroma16, &SCAN_8X32[..]),
        // lane-av1-422: a 4:2:2 1:4 strip halves only the width, so the
        // landscape leaves (32x8, 16x4 luma) code their chroma as the RECT
        // (16,8)/(8,4) -- the same TxbSet/scan pair the (16,8)/(8,4) rows of
        // [`decode_rect_split`]'s chroma table use.
        // lane-av1-422p: the PORTRAIT leaf occurs too -- the pinned
        // fixture's frame 2 carries an 8x32 strip at ss(1,0), whose (4, 32)
        // chroma block `ss_size_lookup[BLOCK_8X32][1][0]` marks
        // BLOCK_INVALID (255, enums.h). BLOCK_INVALID does not mean
        // "uncodable": `av1_get_tx_size` reads `max_txsize_rect_lookup[255]`
        // OUT OF BOUNDS there, the byte at +255 in the pinned Release oracle
        // is 0x05 = TX_4X8 (the `decode_token_recon_block` copy at
        // 0xc0d030, objdump-measured), and `av1_get_adjusted_tx_size` has no
        // TX_4X8 case. libaom HAS the transform, so the shape CHUNKS (the
        // `chroma422_rect32` arm below) into FOUR stacked TX_4X8 units per
        // plane (`tx_size_high_unit[TX_4X8] = 2`: blk_row 0, 2, 4, 6)
        // instead of refusing; no coefficient-table row is added, because
        // BLOCK_4X32 is not a table libaom has. 4:2:0 and 4:4:4 never land
        // here (their 8x32 strips read (4, 16) and (8, 32)), so the row is
        // 4:2:2-only.
        (16, 8) => (TxbSet::ChromaRect16x8, &SCAN_16X8[..]),
        (8, 4) => (TxbSet::ChromaRect8x4, &SCAN_8X4[..]),
        // lane-av1-422p: the units' own set/scan -- the whole-rect read
        // below never sees this row, the `chroma422_rect32` arm chunks
        // first. Anything else is still a named-lane gap.
        (4, 32) if ss_x(fctx) == 1 && ss_y(fctx) == 0 => (TxbSet::ChromaRect8x4, &SCAN_4X8[..]),
        _ => unreachable!("decode_block_rect4 chroma shape set is closed: {chroma_w}x{chroma_h}"),
    };
    let luma_scan: &[u16] = if bw > bh { &SCAN_32X8 } else { &SCAN_8X32 };
    let (luma_levels, u_levels, v_levels) = if skip {
        (
            vec![0i32; bw * bh],
            vec![0i32; chroma_w * chroma_h],
            vec![0i32; chroma_w * chroma_h],
        )
    } else if chroma422_rect32 {
        // lane-av1-422p: the (4, 32) plane as FOUR TX_4X8 units per plane,
        // plane-major (every U unit, then every V) -- the walk the pinned
        // oracle's `decode_token_recon_block` makes for the BLOCK_INVALID
        // plane block: TX_4X8, so stepr = `tx_size_high_unit[TX_4X8]` = 2
        // 4-px rows and stepc = 1, unit rows at chroma y 0, 8, 16, 24.
        // Luma is the same single TX_8X32 rect the general arm reads.
        let around = neighbours.around_mi_rect((mi_r, mi_c), bw, bh);
        let mut luma_coding = cdfs.txb(TxbSet::LumaRect32x8, mode);
        let (luma_levels, luma_tx_type) = read_coeffs_rect(
            dec,
            &mut luma_coding,
            luma_scan,
            bw,
            bh,
            0,
            dc_sign_ctx(around[0].2),
            TxType::DctDct,
            0,
        )?;
        hit!(RECT4_COEFF_HITS);
        let luma_residual = dequant_and_inverse_typed_wh(
            &luma_levels,
            bw,
            bh,
            crate::decode::bit_depth(fctx),
            block_q_idx(fctx),
            plane_q_delta(0, fctx).0,
            plane_q_delta(0, fctx).1,
            luma_tx_type,
            block_iqmatrix(fctx, 0, bw, bh, luma_tx_type),
        );
        if let Some(b) = &palette_y_buf {
            set_palette_pred(b.clone(), fctx);
        }
        push_intra_rect(
            y,
            u,
            v,
            0,
            px,
            py,
            bw,
            bh,
            mode,
            angle_delta_y,
            reach,
            &luma_residual,
            None,
            filter_intra,
            smooth_neighbor,
            fctx,
        );
        // CfL parameters are computed ONCE per block over the strip's luma
        // (`are_parameters_computed`) and shared by all four units, as in
        // every other chunk arm.
        let ac = alpha.map(|_| cfl_src_rect(px, py, bw, bh));
        let mut unit_grids: Vec<((usize, usize), usize, Grid)> = Vec::new();
        for plane in [1usize, 2] {
            for unit_row in 0..4usize {
                let unit_mi = (mi_r + unit_row * 2, mi_c);
                let unit_reach = tu_reach_rect(
                    bw,
                    bh,
                    0,
                    unit_row * 8,
                    8,
                    8,
                    reach,
                    px,
                    py,
                    y.width,
                    y.height,
                    fctx,
                );
                if let Some((ub, vb)) = &palette_uv_bufs {
                    let pbuf = if plane == 1 { ub } else { vb };
                    set_palette_pred(palette_window(pbuf, chroma_w, 0, unit_row * 8, 4, 8), fctx);
                }
                // Each unit is read and recorded IMMEDIATELY: unit row 0's
                // `txb_skip_ctx` still sees the real above/left neighbour
                // states, a lower unit sees this block's own upper unit's
                // (libaom's per-unit `set_entropy_context`), and the lower
                // unit predicts off the upper one's reconstructed samples.
                let chroma_around = neighbours.around_mi_422_chroma(unit_mi, 8, 8);
                let unit_grid = read_rect_chroma_unit(
                    dec,
                    cdfs,
                    TxbSet::ChromaRect8x4,
                    &SCAN_4X8,
                    4,
                    8,
                    plane,
                    chroma_around[plane],
                    uv_predict_mode,
                    angle_delta_uv,
                    unit_reach,
                    y,
                    u,
                    v,
                    cpx,
                    cpy + unit_row * 8,
                    base_q_idx,
                    // `get_txb_ctx_general`'s chroma branch
                    // (txb_common.h): plane_bsize is BLOCK_INVALID (255)
                    // here, `num_pels_log2_lookup[255]` = 255 (the +255
                    // byte of the decodetxb copy at 0xc0de70,
                    // objdump-measured) exceeds `BLOCK_4X8`'s 5, so the
                    // ctx_offset is 10 -- the port's offset-7-first
                    // `txb_skip_chroma_8` stores the offset-10 rows at
                    // rows 3..6, hence +3.
                    Some(3),
                    alpha
                        .zip(ac)
                        .map(|((au, av), ac)| (if plane == 1 { au } else { av }, ac)),
                    smooth_neighbor_uv,
                    fctx,
                )?;
                neighbours.record_mi_chroma(unit_mi, 8, 8, plane, &unit_grid);
                unit_grids.push((unit_mi, plane, unit_grid));
            }
        }
        // The whole-block stamp rewrites the chroma cells ONE state wide,
        // which would erase the unit boundary the next block reads (class
        // `override-slot-on-one-arm`), so it carries zero chroma grids and
        // the per-unit writes replay after it. Its luma cells and side
        // spans are this arm's own bookkeeping.
        let zero_chroma = vec![0i32; chroma_w * chroma_h];
        neighbours.record_mi_rect(
            (mi_r, mi_c),
            bw,
            bh,
            &[&luma_levels[..], &zero_chroma[..], &zero_chroma[..]],
        );
        for (unit_mi, plane, grid) in &unit_grids {
            neighbours.record_mi_chroma(*unit_mi, 8, 8, *plane, grid);
        }
        neighbours.fill_skip_grid_rect((mi_r, mi_c), mi_w, mi_h, skip);
        neighbours.fill_lf_grid_rect((mi_r, mi_c), mi_w, mi_h, bw as u8, bh as u8, 0, fctx);
        record_strip_palette(
            neighbours,
            pal_at,
            bw,
            bh,
            pal_y_size,
            pal_y_colors,
            pal_uv_size,
            pal_uv_colors,
        );
        if bw > bh {
            hit!(RECT4_32_HORZ_HITS);
        } else {
            hit!(RECT4_32_VERT_HITS);
        }
        // lane-frame36 r2 (same shape as `decode_block_rect64`'s split arm,
        // swept this round): `set_txfm_ctxs` runs for EVERY block of an inter
        // frame, so this 1:4 strip -- which is also the INTER frame's reader
        // for 32x8/8x32 ([`decode_intra_rect4_in_inter`]) -- publishes its
        // transform size in both TXFM_CONTEXT bands. Without it the next block
        // reads the row's stale value.
        txfm_partition_update_rect(neighbours, (mi_r, mi_c), (bw, bh), (bw, bh));
        return Ok((mode, uv_predict_mode));
    } else {
        let around = neighbours.around_mi_rect((mi_r, mi_c), bw, bh);
        // lane-av1lr422: the chroma planes of a 4:2:2 strip take their
        // above votes per chroma column (see `around_mi_422_chroma`), not
        // per luma mi. The full-rect gather counts each chroma column twice,
        // which flips `dc_sign_ctx` and desyncs the tile (libaom reads
        // dc_sign_ctx 0 where this read 2, at 10-bit 4:2:2 200x120).
        let chroma_around = if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
            neighbours.around_mi_422_chroma((mi_r, mi_c), bw, bh)
        } else {
            around
        };
        let mut luma_coding = cdfs.txb(TxbSet::LumaRect32x8, mode);
        let (luma_levels, luma_tx_type) = read_coeffs_rect(
            dec,
            &mut luma_coding,
            luma_scan,
            bw,
            bh,
            0,
            dc_sign_ctx(around[0].2),
            TxType::DctDct,
            0,
        )?;
        let mut planes = Vec::with_capacity(2);
        // Index loop mirrors the spec's per-position form over several offsets; an iterator would obscure it.
        #[allow(clippy::needless_range_loop)]
        for plane in 1..3 {
            // `av1_get_tx_type` (`blockd.h:1278`): the mode-indexed type
            // stands only while it is a member of this tx size's set; a
            // 32x8/8x32 chroma transform (4:4:4) squares up to TX_32X32,
            // whose intra set is `EXT_TX_SET_DCTONLY` -- every ADST clamps
            // to DCT_DCT there, and ADST is undefined at widths above 16.
            let default_tx = if chroma_w.max(chroma_h) >= 32 {
                TxType::DctDct
            } else {
                default_intra_tx_type(uv_predict_mode as u8)
            };
            let skip_ctx =
                usize::from(chroma_around[plane].0) + usize::from(chroma_around[plane].1);
            let mut coding = cdfs.txb(chroma_set, uv_predict_mode);
            let (levels, tx_type) = read_chroma_coeffs_rect(
                dec,
                &mut coding,
                chroma_scan,
                chroma_w,
                chroma_h,
                skip_ctx,
                dc_sign_ctx(chroma_around[plane].2),
                default_tx,
                fctx,
                plane,
            )?;
            planes.push((levels, tx_type));
        }
        let (v_levels, v_tx_type) = planes.pop().expect("plane 2");
        let (u_levels, u_tx_type) = planes.pop().expect("plane 1");
        hit!(RECT4_COEFF_HITS);
        let luma_residual = dequant_and_inverse_typed_wh(
            &luma_levels,
            bw,
            bh,
            crate::decode::bit_depth(fctx),
            block_q_idx(fctx),
            plane_q_delta(0, fctx).0,
            plane_q_delta(0, fctx).1,
            luma_tx_type,
            block_iqmatrix(fctx, 0, bw, bh, luma_tx_type),
        );
        if let Some(b) = &palette_y_buf {
            set_palette_pred(b.clone(), fctx);
        }
        push_intra_rect(
            y,
            u,
            v,
            0,
            px,
            py,
            bw,
            bh,
            mode,
            angle_delta_y,
            reach,
            &luma_residual,
            None,
            filter_intra,
            smooth_neighbor,
            fctx,
        );
        let ac = alpha.map(|_| cfl_src_rect(px, py, bw, bh));
        for (plane, levels, tx_type) in [(1usize, &u_levels, u_tx_type), (2, &v_levels, v_tx_type)]
        {
            let residual = dequant_and_inverse_typed_wh(
                levels,
                chroma_w,
                chroma_h,
                crate::decode::bit_depth(fctx),
                block_q_idx(fctx),
                plane_q_delta(plane, fctx).0,
                plane_q_delta(plane, fctx).1,
                tx_type,
                block_iqmatrix(fctx, plane, chroma_w, chroma_h, tx_type),
            );
            if let Some((ub, vb)) = &palette_uv_bufs {
                set_palette_pred(if plane == 1 { ub.clone() } else { vb.clone() }, fctx);
            }
            push_intra_rect(
                y,
                u,
                v,
                plane,
                cpx,
                cpy,
                chroma_w,
                chroma_h,
                uv_predict_mode,
                angle_delta_uv,
                reach,
                &residual,
                alpha
                    .zip(ac)
                    .map(|((au, av), ac)| (if plane == 1 { au } else { av }, ac)),
                None,
                smooth_neighbor_uv,
                fctx,
            );
        }
        neighbours.record_mi_rect(
            (mi_r, mi_c),
            bw,
            bh,
            &[&luma_levels[..], &u_levels[..], &v_levels[..]],
        );
        neighbours.fill_skip_grid_rect((mi_r, mi_c), mi_w, mi_h, skip);
        neighbours.fill_lf_grid_rect((mi_r, mi_c), mi_w, mi_h, bw as u8, bh as u8, 0, fctx);
        record_strip_palette(
            neighbours,
            pal_at,
            bw,
            bh,
            pal_y_size,
            pal_y_colors,
            pal_uv_size,
            pal_uv_colors,
        );
        if bw > bh {
            hit!(RECT4_32_HORZ_HITS);
        } else {
            hit!(RECT4_32_VERT_HITS);
        }
        // lane-frame36 r2 (same shape as `decode_block_rect64`'s split arm,
        // swept this round): `set_txfm_ctxs` runs for EVERY block of an inter
        // frame, so this 1:4 strip -- which is also the INTER frame's reader
        // for 32x8/8x32 ([`decode_intra_rect4_in_inter`]) -- publishes its
        // transform size in both TXFM_CONTEXT bands. Without it the next block
        // reads the row's stale value.
        txfm_partition_update_rect(neighbours, (mi_r, mi_c), (bw, bh), (bw, bh));
        return Ok((mode, uv_predict_mode));
    };
    if let Some(b) = &palette_y_buf {
        set_palette_pred(b.clone(), fctx);
    }
    push_intra_rect(
        y,
        u,
        v,
        0,
        px,
        py,
        bw,
        bh,
        mode,
        angle_delta_y,
        reach,
        &luma_levels,
        None,
        filter_intra,
        smooth_neighbor,
        fctx,
    );
    let ac = alpha.map(|_| cfl_src_rect(px, py, bw, bh));
    if chroma422_rect32 {
        // lane-av1-422p: `predict_and_reconstruct_intra_block` (decodeframe.c
        // ~1000) predicts EVERY unit regardless of `skip_txfm`, so the skip
        // arm walks the same four TX_4X8 units per plane -- the lower unit
        // predicts off the upper one's predicted samples. No coefficients
        // are coded; the tail's whole-block zero stamp below is the state
        // libaom's `set_entropy_context` leaves.
        for plane in [1usize, 2] {
            for unit_row in 0..4usize {
                let unit_reach = tu_reach_rect(
                    bw,
                    bh,
                    0,
                    unit_row * 8,
                    8,
                    8,
                    reach,
                    px,
                    py,
                    y.width,
                    y.height,
                    fctx,
                );
                if let Some((ub, vb)) = &palette_uv_bufs {
                    let pbuf = if plane == 1 { ub } else { vb };
                    set_palette_pred(palette_window(pbuf, chroma_w, 0, unit_row * 8, 4, 8), fctx);
                }
                push_intra_rect(
                    y,
                    u,
                    v,
                    plane,
                    cpx,
                    cpy + unit_row * 8,
                    4,
                    8,
                    uv_predict_mode,
                    angle_delta_uv,
                    unit_reach,
                    &ZERO_RESIDUAL[..4 * 8],
                    alpha
                        .zip(ac)
                        .map(|((au, av), ac)| (if plane == 1 { au } else { av }, ac)),
                    None,
                    smooth_neighbor_uv,
                    fctx,
                );
            }
        }
    } else {
        if let Some((ub, _)) = &palette_uv_bufs {
            set_palette_pred(ub.clone(), fctx);
        }
        push_intra_rect(
            y,
            u,
            v,
            1,
            cpx,
            cpy,
            chroma_w,
            chroma_h,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            &u_levels,
            alpha.zip(ac).map(|((au, _), ac)| (au, ac)),
            None,
            smooth_neighbor_uv,
            fctx,
        );
        if let Some((_, vb)) = &palette_uv_bufs {
            set_palette_pred(vb.clone(), fctx);
        }
        push_intra_rect(
            y,
            u,
            v,
            2,
            cpx,
            cpy,
            chroma_w,
            chroma_h,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            &v_levels,
            alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
            None,
            smooth_neighbor_uv,
            fctx,
        );
    }
    neighbours.record_mi_rect(
        (mi_r, mi_c),
        bw,
        bh,
        &[&luma_levels[..], &u_levels[..], &v_levels[..]],
    );
    neighbours.fill_skip_grid_rect((mi_r, mi_c), mi_w, mi_h, skip);
    neighbours.fill_lf_grid_rect((mi_r, mi_c), mi_w, mi_h, bw as u8, bh as u8, 0, fctx);
    record_strip_palette(
        neighbours,
        pal_at,
        bw,
        bh,
        pal_y_size,
        pal_y_colors,
        pal_uv_size,
        pal_uv_colors,
    );
    if bw > bh {
        hit!(RECT4_32_HORZ_HITS);
    } else {
        hit!(RECT4_32_VERT_HITS);
    }
    // lane-frame36 r2 (same shape as `decode_block_rect64`'s split arm,
    // swept this round): `set_txfm_ctxs` runs for EVERY block of an inter
    // frame, so this 1:4 strip -- which is also the INTER frame's reader
    // for 32x8/8x32 ([`decode_intra_rect4_in_inter`]) -- publishes its
    // transform size in both TXFM_CONTEXT bands. Without it the next block
    // reads the row's stale value.
    txfm_partition_update_rect(neighbours, (mi_r, mi_c), (bw, bh), (bw, bh));
    Ok((mode, uv_predict_mode))
}

/// Decodes the four 16x4 / 4x16 strips of a 16x16-level `PARTITION_HORZ_4` /
/// `PARTITION_VERT_4` (lane-r14) -- [`decode_block_rect4`]'s pair one level
/// down, and the first shape here whose luma block is a single mi row (or
/// column).
///
/// What is new at this size is CHROMA. `is_chroma_reference`
/// (`av1_common_int.h`) is false for a 4:2:0 block one mi tall (wide) on an
/// EVEN mi row (column), so strips 0 and 2 read no `uv_mode`/`cfl_alpha`/
/// `angle_delta_uv` at all and code no chroma residual; strips 1 and 3 close
/// the pair and carry the chroma of the whole 16x8 (8x16) luma pair --
/// `setup_pred_plane`'s own `if (subsampling_y && (mi_row & 1) &&
/// mi_size_high[bsize] == 1) mi_row -= 1` offset, i.e. an 8x4 (4x8) chroma
/// transform at the PAIR's origin, with CfL averaging over the pair's luma.
///
/// Tables: LUMA `TX_16X4`/`TX_4X16` averages to the 8x8 entropy context
/// ((0 + 2 + 1) >> 1 = 1) with 64 coded positions, so its CDF set is
/// [`TxbSet::LumaRect16x4`] (`Luma8`'s tables, the `TX_4X4` `tx_type` row);
/// scans are [`SCAN_16X4`]/[`SCAN_4X16`]. CHROMA `TX_8X4`/`TX_4X8` is
/// [`TxbSet::ChromaRect8x4`] with [`SCAN_8X4`]/[`SCAN_4X8`]. Angle deltas ARE
/// coded (`av1_use_angle_delta` compares the BLOCK_SIZES_ALL enum index, and
/// BLOCK_16X4 is 17), and so is `use_filter_intra` (both sides <= 32) off this
/// shape's own CDF row.
///
/// A split transform (`tx_depth != 0`, `sub_tx_size_map[TX_16X4] == TX_8X4`)
/// refuses by name rather than guess-decode.
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_rect4_16(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    at16: (usize, usize),
    horz: bool,
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    enable_filter_intra: bool,
    allow_screen_content_tools: bool,
    base_q_idx: u8,
    tx_select: bool,
    reduced_tx_set: bool,
    fctx: &crate::decode::FrameCtx,
) -> Result<()> {
    let (r, c) = at16;
    let (mi_row0, mi_col0) = (r * (SUB / MI), c * (SUB / MI));
    let (bw, bh) = if horz { (16, 4) } else { (4, 16) };
    let (_mi_w, _mi_h) = (bw / MI, bh / MI);
    let _scan: &[u16] = if horz { &SCAN_16X4 } else { &SCAN_4X16 };
    let _chroma_scan: &[u16] = if horz { &SCAN_8X4 } else { &SCAN_4X8 };
    let mut prev: Option<((usize, usize), usize)> = None;
    let mut last_mode = DC_PRED;
    let mut last_uv_mode = DC_PRED;
    for i in 0..4usize {
        let lmi = if horz {
            (mi_row0 + i, mi_col0)
        } else {
            (mi_row0, mi_col0 + i)
        };
        // lane-av1-444: at 4:4:4 every strip is its own chroma reference
        // (no mi-parity test at ss 0/0, and the strip's own chroma block is
        // already >= 4x4) -- there is no 4:2:0 pair to close.
        // lane-av1-422o: the parity rule is per axis, straight out of
        // `is_chroma_reference`: a HORZ 16x4 strip (1 mi tall) keeps the
        // `ss_y == 0 -> every strip a reference` row rule, but a VERT 4x16
        // strip (1 mi wide) keeps the COLUMN clause `(mi_col & 1) || !(bw &
        // 1) || !ss_x`, which with bw == 1 mi reduces to `mi_col odd` for
        // every ss_x == 1 format -- the ODD strip closes the pair and reads
        // the chroma, exactly like 4:2:0. (VERT_4 partitions start on an
        // even mi_col, so `i % 2 == 1` is that clause.)
        let has_chroma = i % 2 == 1 || (horz && ss_y(fctx) == 0) || (!horz && ss_x(fctx) == 0);
        let (mode, uv, _tx) = decode_rect4_16_strip(
            dec,
            cdfs,
            neighbours,
            lmi,
            horz,
            has_chroma,
            prev,
            y,
            u,
            v,
            enable_filter_intra,
            allow_screen_content_tools,
            base_q_idx,
            tx_select,
            reduced_tx_set,
            fctx,
        )?;
        if let Some(m) = uv {
            last_uv_mode = m;
        }
        last_mode = mode;
        prev = Some((lmi, mode));
    }
    // The coarse 16-px cell holds one entry for the whole 16x16: the last
    // strip is what a block below (right) sees through it, the mi-exact maps
    // above answering everything finer.
    neighbours.above_mode[c] = last_mode;
    neighbours.left_mode[r] = last_mode;
    neighbours.above_uv_mode[c] = last_uv_mode;
    neighbours.left_uv_mode[r] = last_uv_mode;
    neighbours.above_side[c] = bw;
    neighbours.left_side[r] = bh;
    Ok(())
}

/// One 16x4 / 4x16 strip of a 16x16-level 1:4 partition, intra-coded --
/// [`decode_rect4_16`]'s per-strip body, lifted (lane-intra16x4) so the INTER
/// block path can decode a SINGLE intra strip of a partition whose other
/// strips are inter. `prev` is the previous strip's `(mi, mode)` (the
/// key-frame caller's coarse-cell stand-in; `None` on the inter path, where
/// every strip publishes its own mi-exact mode). Returns
/// `(y_mode, uv_predict_mode when this strip closes the 4:2:0 chroma pair)`.
/// Intrabc reconstruction for a rect strip that OWNS its chroma
/// (`bw/2 x bh/2` at the strip origin). 16x4/4x16 is not this shape -- that
/// pair goes through [`decode_rect4_16_intrabc`].
///
/// This is the same control flow as the unmerged lane's `decode_intrabc_rect`
/// (predict once, then one residual per transform unit on the INTER sets,
/// chroma inherits luma's `tx_type`), written against the helpers that exist
/// on this tree so a 2:1 strip no longer hides the 16x4 pair witness. The
/// merge with lane-av1-intrabc should keep one of the two bodies; the call
/// shape is the same.
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_intrabc_owned_rect(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    (mi_r, mi_c): (usize, usize),
    (bw, bh): (usize, usize),
    skip: bool,
    (dv_row, dv_col): (i32, i32),
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    base_q_idx: u8,
    tx_select: bool,
    reduced_tx_set: bool,
    fctx: &crate::decode::FrameCtx,
) -> Result<()> {
    let _ = base_q_idx;
    let (px, py) = (mi_c * MI, mi_r * MI);
    // lane-av1chromarect: libaom does NOT halve a 128-root intra-BC strip's
    // chroma plane block. `av1_get_max_uv_txsize` (blockd.h:1372) is
    // `max_txsize_rect_lookup[get_plane_block_size(bsize, ss_x, ss_y)]` run
    // through `av1_get_adjusted_tx_size`, and `get_plane_block_size` is
    // `ss_size_lookup[bsize]` -- at 4:4:4 the chroma plane block is the block's
    // OWN footprint, so the extent below and the eight chroma MV origin pairs
    // that follow must be `>> ss_x` / `>> ss_y`, not `/ 2`. At 4:2:0
    // (`ss_x == ss_y == 1`) the two forms are the same arithmetic.
    // MEASURED on the 4:4:4 LOSSY witness `fixtures/r512.obu` (6948 B, 3
    // frames, 5 strips through this function): with the halving, frame 0
    // diverges from the aomdec oracle in 166229 Y / 155383 U / 153498 V
    // samples and both later frames keep luma byte-exact while 74708 U +
    // 73501 V (f1) and 9216 U + 8192 V (f2) are wrong -- chroma-only, which is
    // what makes this a chroma-sizing defect and not a block-geometry one.
    // Re-measured by this lane; see lanes/av1chromarect.report.md.
    if crate::envflags::env_flag!("EC_HALVSWEEP") {
        eprintln!("EC_HALV ibc_owned mi=({mi_r},{mi_c}) bw={bw} bh={bh} skip={skip}");
    }
    let (cw, ch) = (bw >> ss_x(fctx), bh >> ss_y(fctx));
    let (cpx, cpy) = (px >> ss_x(fctx), py >> ss_y(fctx));
    let side = bw.max(bh);
    let cside = cw.max(ch);
    if ss_x(fctx) == 0 && ss_y(fctx) == 0 {
        hit!(IBC_OWNED_RECT_CHROMA_FOOTPRINT_444_HITS);
    }
    let saved_reduced = fctx.reduced_tx_set_inter.with(std::cell::Cell::get);
    let saved_tx = fctx.tx_select_inter.with(std::cell::Cell::get);
    fctx.reduced_tx_set_inter.with(|c| c.set(reduced_tx_set));
    // `read_block_tx_size_rect` keys on this cell. A key frame forces it
    // false; the real bit is the caller's `tx_select`. Restored before return.
    fctx.tx_select_inter.with(|c| c.set(tx_select));
    let restore = ReducedTxRestore {
        fctx,
        saved_reduced,
        saved_tx,
    };

    let pred_y = predict_intrabc_window(y, px, py, bw, bh, dv_col, dv_row, true, side, fctx);
    let pred_u = predict_intrabc_window(u, cpx, cpy, cw, ch, dv_col, dv_row, false, cside, fctx);
    let pred_v = predict_intrabc_window(v, cpx, cpy, cw, ch, dv_col, dv_row, false, cside, fctx);

    let mi_cols = 2 * y.true_width.div_ceil(8);
    let mi_rows = 2 * y.true_height.div_ceil(8);
    let leaves = read_block_tx_size_rect(
        dec,
        cdfs,
        neighbours,
        (mi_r, mi_c),
        (bw, bh),
        (mi_cols, mi_rows),
        skip,
        fctx,
    )?;
    let mut luma_tx_type = TxType::DctDct;
    let luma_grid;
    let u_grid;
    let v_grid;
    if skip {
        // `reconstruct_mc_rect` indexes the residual at the square stride.
        // `bw * bh` underruns on a VERT strip (8x16: stride 16, 16 rows, index 136).
        push_mc_rect(
            y,
            u,
            v,
            0,
            px,
            py,
            side,
            bw,
            bh,
            Pred::Inline(&pred_y, side),
            &ZERO_RESIDUAL[..side * bh],
            fctx,
        );
        push_mc_rect(
            y,
            u,
            v,
            1,
            cpx,
            cpy,
            cside,
            cw,
            ch,
            Pred::Inline(&pred_u, cside),
            &ZERO_RESIDUAL[..cside * ch],
            fctx,
        );
        push_mc_rect(
            y,
            u,
            v,
            2,
            cpx,
            cpy,
            cside,
            cw,
            ch,
            Pred::Inline(&pred_v, cside),
            &ZERO_RESIDUAL[..cside * ch],
            fctx,
        );
        if let Some(ls) = leaves.as_ref() {
            // libaom `av1_reset_entropy_context` (`if (mbmi->skip_txfm)`,
            // decodeframe.c:1262 -> blockd.c:58): a SKIPPED block zeroes its
            // whole entropy-band footprint. The non-skip arm below writes
            // those bands per transform unit and the tail's
            // `record_split_luma_rect_mi` writes chroma only, so without
            // this walk a skipped strip whose `leaves` resolved (every
            // lossless frame -- `read_block_tx_size_rect`'s lossless arm
            // returns the 4x4 grid before it looks at `skip`) leaked the
            // PREVIOUS occupant's luma bands into the next block's
            // `txb_skip`/coefficient contexts (lane-av1-loss64: the skipped
            // 8x16 intrabc strip at mi(48,66) of the 320x242 lossless sb128
            // stream left a stale 7 in left[48] and mi(48,68)'s first
            // transform unit read `txb_skip_ctx` 3 where aomdec reads 1).
            hit!(SKIP_LOSSLESS_BAND_RESET_HITS);
            for &(row, col, tw, th) in ls {
                neighbours.record_mi_luma_rect(
                    (mi_r + row, mi_c + col),
                    tw,
                    th,
                    &ZERO_RESIDUAL[..tw * th],
                );
            }
        }
        luma_grid = Grid::Zero(side * side);
        u_grid = Grid::Zero(cside * cside);
        v_grid = Grid::Zero(cside * cside);
    } else if let Some(ls) = leaves.as_ref() {
        let mut leaf_types: Vec<(usize, usize, usize, usize, TxType)> =
            Vec::with_capacity(ls.len());
        for (idx, &(row, col, tw, th)) in ls.iter().enumerate() {
            let tu_mi = (mi_r + row, mi_c + col);
            let (tu_px, tu_py) = (px + col * MI, py + row * MI);
            let tu_pred = Pred::Inline(&pred_y, side).offset(row * MI * side + col * MI);
            let (g, tt) = if tw == th {
                read_inter_plane(
                    dec,
                    cdfs,
                    inter_txbset_for(tw, reduced_tx_set),
                    default_scan(tw.min(32)),
                    0,
                    neighbours.around_mi(tu_mi, tw)[0],
                    0,
                    y,
                    u,
                    v,
                    tu_px,
                    tu_py,
                    tw,
                    base_q_idx,
                    tu_pred,
                    None,
                    Some(neighbours.luma_skip_ctx(tu_mi, tw / MI)),
                    fctx,
                )?
            } else {
                read_inter_plane_rect(
                    dec,
                    cdfs,
                    rect_inter_luma_set(tw, th, fctx)?,
                    (tw, th),
                    side,
                    0,
                    neighbours.around_mi_rect(tu_mi, tw, th)[0],
                    0,
                    y,
                    u,
                    v,
                    tu_px,
                    tu_py,
                    tu_pred,
                    None,
                    Some(neighbours.luma_skip_ctx_rect(tu_mi, tw / MI, th / MI)),
                    fctx,
                )?
            };
            leaf_types.push((row, col, tw, th, tt));
            if idx == 0 {
                luma_tx_type = tt;
            }
            neighbours.record_mi_luma_rect(tu_mi, tw, th, &g);
            if tw != th {
                neighbours.fill_lf_grid_leaf_luma(
                    (tu_mi.0, tu_mi.1),
                    tw / MI,
                    th / MI,
                    tw as u8,
                    th as u8,
                );
            }
        }
        luma_grid = Grid::Zero(side * side);
        if cw.min(32) != cw || ch.min(32) != ch {
            // lane-av1chromarect: more than one chroma transform unit (the
            // 4:4:4 32x64/64x32 plane block). See the helper.
            let (ug, vg) = read_intrabc_rect_chroma_split(
                y,
                u,
                v,
                dec,
                cdfs,
                neighbours,
                (mi_r, mi_c),
                (cpx, cpy),
                (cw, ch),
                cside,
                &pred_u,
                &pred_v,
                &leaf_types,
                luma_tx_type,
                fctx,
            )?;
            u_grid = ug;
            v_grid = vg;
        } else {
            // lane-av1422ibctxsel: at ss (1, 0) a chroma 4-px cell spans TWO
            // luma mi columns, so a luma-span gather counts each chroma
            // column twice; libaom's `get_txb_ctx_general` reads one vote per
            // CHROMA cell, which is what `around_mi_422_chroma`'s
            // every-second sampling produces. Both non-split chroma arms of
            // this function route through it under the ss (1, 0) guard; every
            // other subsampling keeps the plain gather verbatim. WITNESS: the
            // no-leaves twin below (24 entries on
            // `fixtures/ibc422_nonsplit_ctx.obu`, five of them V-plane reads
            // whose two gathers disagree -- `dc_sign_ctx` 1 vs 0; gate
            // `a_422_intrabc_owned_rect_nonsplit_chroma_context_uses_the_pair_rule`).
            // THIS branch has no stream that splits a transform on the strip:
            // it needs `TX_MODE_SELECT` plus a `txfm_partition` split of the
            // 64x32 strip, a conjunction no measured stream has produced
            // (`lane-av1422ibcrect` measured 0 entries under
            // `TX_MODE_LARGEST`); the routing here is identical, so a future
            // witness takes the same code.
            let around = if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                hit!(IBC_OWNED_RECT_422_PAIR_HITS);
                neighbours.around_mi_422_chroma((mi_r, mi_c), bw, bh)
            } else {
                neighbours.around_mi_rect((mi_r, mi_c), bw, bh)
            };
            let (ug, _) = read_inter_plane_rect(
                dec,
                cdfs,
                rect_inter_chroma_set(cw, ch)?,
                (cw, ch),
                cside,
                1,
                around[1],
                0,
                y,
                u,
                v,
                cpx,
                cpy,
                Pred::Inline(&pred_u, cside),
                Some(luma_tx_type),
                None,
                fctx,
            )?;
            let (vg, _) = read_inter_plane_rect(
                dec,
                cdfs,
                rect_inter_chroma_set(cw, ch)?,
                (cw, ch),
                cside,
                2,
                around[2],
                0,
                y,
                u,
                v,
                cpx,
                cpy,
                Pred::Inline(&pred_v, cside),
                Some(luma_tx_type),
                None,
                fctx,
            )?;
            u_grid = ug;
            v_grid = vg;
        }
    } else {
        let around = neighbours.around_mi_rect((mi_r, mi_c), bw, bh);
        let (g, tt) = read_inter_plane_rect(
            dec,
            cdfs,
            rect_inter_luma_set(bw, bh, fctx)?,
            (bw, bh),
            side,
            0,
            around[0],
            0,
            y,
            u,
            v,
            px,
            py,
            Pred::Inline(&pred_y, side),
            None,
            None,
            fctx,
        )?;
        luma_tx_type = tt;
        luma_grid = g;
        if cw.min(32) != cw || ch.min(32) != ch {
            // lane-av1chromarect: more than one chroma transform unit (the
            // 4:4:4 32x64/64x32 plane block). See the helper.
            let (ug, vg) = read_intrabc_rect_chroma_split(
                y,
                u,
                v,
                dec,
                cdfs,
                neighbours,
                (mi_r, mi_c),
                (cpx, cpy),
                (cw, ch),
                cside,
                &pred_u,
                &pred_v,
                &[],
                luma_tx_type,
                fctx,
            )?;
            u_grid = ug;
            v_grid = vg;
        } else {
            // lane-av1422ibctxsel: the twin of the tree branch above -- the
            // CHROMA context at ss (1, 0) is the pair rule (one vote per
            // chroma 4-px cell); `around` stays the plain luma-span gather
            // for plane 0 (lane-av1422ibcrect measured this arm entered 24
            // times by a 4:2:2 64x32 strip whose two gathers happened to
            // agree -- `fixtures/ibc422_nonsplit_ctx.obu` is the stream where
            // they do not).
            let around_chroma = if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                hit!(IBC_OWNED_RECT_422_PAIR_HITS);
                neighbours.around_mi_422_chroma((mi_r, mi_c), bw, bh)
            } else {
                around
            };
            let (ug, _) = read_inter_plane_rect(
                dec,
                cdfs,
                rect_inter_chroma_set(cw, ch)?,
                (cw, ch),
                cside,
                1,
                around_chroma[1],
                0,
                y,
                u,
                v,
                cpx,
                cpy,
                Pred::Inline(&pred_u, cside),
                Some(luma_tx_type),
                None,
                fctx,
            )?;
            let (vg, _) = read_inter_plane_rect(
                dec,
                cdfs,
                rect_inter_chroma_set(cw, ch)?,
                (cw, ch),
                cside,
                2,
                around_chroma[2],
                0,
                y,
                u,
                v,
                cpx,
                cpy,
                Pred::Inline(&pred_v, cside),
                Some(luma_tx_type),
                None,
                fctx,
            )?;
            u_grid = ug;
            v_grid = vg;
        }
    }
    drop(restore);
    // lane-av1chromarect: when the chroma plane block was walked as SEVERAL
    // transform units, each unit stamped its own span through
    // `record_mi_chroma` and the whole-block stamp below would overwrite them
    // with one state (class `override-slot-on-one-arm`). libaom keeps the
    // per-unit above/left contexts distinct; the next block's first unit
    // reads them.
    // `!skip` matters: a SKIPPED strip reads no chroma unit at all (its whole
    // chroma plane block is one `push_mc_rect` of `ZERO_RESIDUAL`), so no
    // per-unit `record_mi_chroma` ran and the whole-block chroma stamp below
    // is still the only one that happens.
    let chroma_split_units = !skip && (cw.min(32) != cw || ch.min(32) != ch);
    if leaves.is_some() {
        if chroma_split_units {
            neighbours.record_split_luma_rect_mi_modes((mi_r, mi_c), bw, bh, DC_PRED, DC_PRED);
        } else {
            neighbours.record_split_luma_rect_mi(
                (mi_r, mi_c),
                bw,
                bh,
                DC_PRED,
                DC_PRED,
                [&u_grid, &v_grid],
            );
        }
    } else if chroma_split_units {
        neighbours.record_rect_mi_luma_only((mi_r, mi_c), bw, bh, DC_PRED, DC_PRED, &luma_grid);
    } else {
        neighbours.record_rect_mi(
            (mi_r, mi_c),
            bw,
            bh,
            DC_PRED,
            DC_PRED,
            &[&luma_grid, &u_grid, &v_grid],
        );
    }
    neighbours.record_palette_y_rect((mi_r, mi_c), bw, bh, 0, [0u16; 8]);
    neighbours.record_palette_uv_rect((mi_r, mi_c), bw, bh, 0, [0u16; 8]);
    neighbours.fill_skip_grid_rect((mi_r, mi_c), bw / MI, bh / MI, skip);
    // lane-av1oddheightfork3: see `decode_intrabc_rect`'s call -- the tree
    // published the bands, so no whole-footprint publish here.
    neighbours.fill_lf_grid_rect_after_vartx(
        (mi_r, mi_c),
        bw / MI,
        bh / MI,
        bw.min(64) as u8,
        bh.min(64) as u8,
        0,
        fctx,
    );
    record_intrabc_mi_rect(
        mi_r,
        mi_c,
        bw / MI,
        bh / MI,
        Some((dv_row, dv_col)),
        neighbours,
        fctx,
    );
    let _ = luma_tx_type;
    Ok(())
}

/// The chroma plane block of a 128-root intra-BC HORZ/VERT strip when that
/// plane block needs MORE THAN ONE transform unit -- the 4:4:4 cell, and the
/// 4:2:0 cell of a strip with a 128-px axis.
///
/// libaom never forms a 64-point CHROMA transform. `get_vartx_max_txsize`
/// (blockd.h:1447) returns `av1_get_adjusted_tx_size(max_txsize_rect_lookup
/// [plane_bsize])` and `av1_get_adjusted_tx_size` maps `TX_32X64`/`TX_64X32`
/// to `TX_32X32`; `decode_token_recon_block` (decodeframe.c:994-1004) then
/// steps the plane block by that size's mi units, and
/// `decode_reconstruct_tx`'s `tx_size == plane_tx_size || plane` arm never
/// recurses for chroma -- the same statement from the other side. So a 4:4:4
/// 32x64 chroma plane block is TWO stacked `TX_32X32` units, each with its
/// own eob, its own scan and its own coefficient context, NOT one 32x64 unit
/// whose coded corner happens to be 32x32: `av1_get_max_eob`'s 64-axis
/// truncation is a LUMA rule, and a chroma plane block is never 64 points
/// long to begin with.
///
/// That is exactly the tiling [`read_inter_rect_chroma`] implements
/// (`(write_chroma_w.min(32), write_chroma_h.min(32))` and the `nx * ny`
/// walk); this is the same walk for a caller that keeps its own grid
/// assembly and neighbour recording. The single-unit case is deliberately
/// NOT routed through here, so at 4:2:0 and at every 4:4:4 shape below 64
/// the caller keeps its own one-unit read verbatim.
#[allow(clippy::too_many_arguments)]
fn read_intrabc_rect_chroma_split(
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    (mi_r, mi_c): (usize, usize),
    (cpx, cpy): (usize, usize),
    (cw, ch): (usize, usize),
    cside: usize,
    pred_u: &[u16],
    pred_v: &[u16],
    leaf_tx_types: &[(usize, usize, usize, usize, TxType)],
    luma_tx_type: TxType,
    fctx: &crate::decode::FrameCtx,
) -> Result<(Grid, Grid)> {
    let (uw, uh) = (cw.min(32), ch.min(32));
    let (nx, ny) = (cw / uw, ch / uh);
    let set = rect_inter_chroma_set(uw, uh)?;
    let assembled_h = ch.max(cside);
    let mut assembled: [Vec<i32>; 2] = [
        vec![0i32; cside * assembled_h],
        vec![0i32; cside * assembled_h],
    ];
    for plane_idx in 1..=2 {
        let src = if plane_idx == 1 { pred_u } else { pred_v };
        for cu_row in 0..ny {
            for cu_col in 0..nx {
                let (span_x, span_y) = (uw << ss_x(fctx), uh << ss_y(fctx));
                let cu_mi = (mi_r + cu_row * (span_y / MI), mi_c + cu_col * (span_x / MI));
                // lane-av1422llintra: the same ss (1, 0) above-column
                // double-count as the lossless walk at 12735 -- one chroma
                // column spans two luma mi columns and libaom reads one.
                // Identical at 4:2:0 and 4:4:4, where the plane block is
                // square.
                let cu_around = if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                    neighbours.around_mi_422_chroma(cu_mi, span_x, span_y)
                } else {
                    neighbours.around_mi_rect(cu_mi, span_x, span_y)
                };
                let (cu_x, cu_y) = (cpx + cu_col * uw, cpy + cu_row * uh);
                let cu_pred = Pred::Inline(src, cside).offset(cu_row * uh * cside + cu_col * uw);
                // libaom's `av1_get_tx_type` (blockd.h:1287) reads
                // `xd->tx_type_map` at the chroma unit's OWN position scaled
                // back to luma, so a unit sitting over a luma leaf that coded
                // nothing takes that leaf's stamp, not the block-level one.
                let rel_mi = (cu_mi.0 - mi_r, cu_mi.1 - mi_c);
                let inherit_tx_type =
                    covering_leaf_tx_type(leaf_tx_types, rel_mi).or(Some(luma_tx_type));
                let cu_grid = read_inter_plane_rect(
                    dec,
                    cdfs,
                    set,
                    (uw, uh),
                    cside,
                    plane_idx,
                    cu_around[plane_idx],
                    0,
                    y,
                    u,
                    v,
                    cu_x,
                    cu_y,
                    cu_pred,
                    inherit_tx_type,
                    // `get_txb_ctx`: plus 10 when the plane block is bigger
                    // than the transform -- the same offset-3 convention
                    // `read_inter_rect_chroma` passes on this arm.
                    Some(3),
                    fctx,
                )?
                .0;
                // Immediately, so the next unit of this block reads this one's
                // coefficient context (libaom updates the chroma above/left
                // contexts per unit).
                neighbours.record_mi_chroma(cu_mi, span_x, span_y, plane_idx, &cu_grid);
                let dst = &mut assembled[plane_idx - 1];
                for rr in 0..uh {
                    let start = (cu_row * uh + rr) * cside + cu_col * uw;
                    dst[start..start + uw].copy_from_slice(&cu_grid[rr * uw..][..uw]);
                }
            }
        }
    }
    let [ug, vg] = assembled;
    Ok((Grid::Own(ug), Grid::Own(vg)))
}

// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
fn predict_intrabc_window(
    plane: &PlaneBuf<'_>,
    px: usize,
    py: usize,
    w: usize,
    h: usize,
    dv_col: i32,
    dv_row: i32,
    luma: bool,
    stride: usize,
    fctx: &crate::decode::FrameCtx,
) -> Vec<u16> {
    let mut dense = vec![0u16; w * h];
    mc::predict_with_filter(
        &plane.data,
        plane.width,
        plane.true_width,
        plane.true_height,
        mv_to_q4(px, dv_col, if luma { 0 } else { ss_x(fctx) }),
        mv_to_q4(py, dv_row, if luma { 0 } else { ss_y(fctx) }),
        w,
        h,
        mc::InterpFilterKind::Bilinear,
        &mut dense,
        fctx,
    );
    let mut laid = vec![0u16; stride * stride];
    for row in 0..h {
        laid[row * stride..][..w].copy_from_slice(&dense[row * w..][..w]);
    }
    laid
}

pub(super) struct ReducedTxRestore<'a> {
    pub(super) fctx: &'a crate::decode::FrameCtx,
    pub(super) saved_reduced: bool,
    pub(super) saved_tx: bool,
}
impl Drop for ReducedTxRestore<'_> {
    fn drop(&mut self) {
        self.fctx
            .reduced_tx_set_inter
            .with(|c| c.set(self.saved_reduced));
        self.fctx.tx_select_inter.with(|c| c.set(self.saved_tx));
    }
}

/// Reconstruction of one intrabc 16x4 / 4x16 strip (lane-av1-rect14).
///
/// A 1:4 strip is a PAIR. Both children share one chroma block, and only the
/// odd-mi member codes it (`is_chroma_reference`, `av1_common_int.h:1454`:
/// the bottom strip of a `HORZ_4`, the right strip of a `VERT_4`). The chroma
/// plane block is `ss_size_lookup[BLOCK_16X4][1][1] == BLOCK_8X4` (and
/// `BLOCK_4X8` for `VERT_4`) at the PAIR origin -- `setup_pred_plane`'s
/// `mi_row -= 1` when the block is one mi tall on an odd row. Luma is this
/// strip's own footprint, copied from this strip's DV. The block is
/// `is_inter_block`, so the residual uses the INTER sets and chroma inherits
/// the luma unit's coded `tx_type`.
///
/// Not [`decode_intrabc_rect`]: that helper (lane-av1-intrabc, not on this
/// tree) predicts chroma at `bw/2 x bh/2` of the strip itself. At 16x4 that
/// is an 8x2 half-pair at the odd strip -- wrong size, wrong origin, and it
/// would code chroma on every strip.
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_rect4_16_intrabc(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    lmi: (usize, usize),
    horz: bool,
    has_chroma: bool,
    skip: bool,
    (dv_row, dv_col): (i32, i32),
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    base_q_idx: u8,
    tx_select: bool,
    reduced_tx_set: bool,
    fctx: &crate::decode::FrameCtx,
) -> Result<(usize, Option<usize>, (usize, usize))> {
    let _ = base_q_idx;
    hit!(RECT4_16_INTRABC_HITS);
    let (bw, bh) = if horz { (16usize, 4usize) } else { (4, 16) };
    let (mi_w, mi_h) = (bw / MI, bh / MI);
    let (px, py) = (lmi.1 * MI, lmi.0 * MI);
    let side = bw.max(bh);
    // `rect_inter_luma_set` reads the frame cell. A key frame's tile decoder
    // publishes `reduced_tx_set` into it; set it here too so a caller that
    // passed the bit explicitly cannot disagree with the set we read.
    let saved_reduced = fctx.reduced_tx_set_inter.with(std::cell::Cell::get);
    fctx.reduced_tx_set_inter.with(|c| c.set(reduced_tx_set));
    let restored = scopeguard_reduced(fctx, saved_reduced);

    let mut pred_y_dense = vec![0u16; bw * bh];
    mc::predict_with_filter(
        &y.data,
        y.width,
        y.true_width,
        y.true_height,
        mv_to_q4(px, dv_col, 0),
        mv_to_q4(py, dv_row, 0),
        bw,
        bh,
        mc::InterpFilterKind::Bilinear,
        &mut pred_y_dense,
        fctx,
    );
    let mut pred_y = vec![0u16; side * side];
    for row in 0..bh {
        pred_y[row * side..][..bw].copy_from_slice(&pred_y_dense[row * bw..][..bw]);
    }

    // lane-av1444rect r2 (the class this lane named, second instance): the
    // pair geometry below is the 4:2:0 one and was applied at every `ss`. At
    // ss (0,0) `is_chroma_reference` (av1_common_int.h:1454) is
    // unconditionally true -- the strip is its own chroma reference, there is
    // no pair, and its chroma plane block IS the strip: 16x4 (HORZ_4) / 4x16
    // (VERT_4) at the strip's own origin, with its coefficient context read
    // over that same extent. The old constants instead predicted and coded an
    // 8x4 block at `(lmi - 1) * MI / 2`, a quarter of the position.
    //
    // MEASURED on a `testsrc2` 640x480 `--tune-content=screen --enable-intrabc=1
    // --enable-palette=1 --cq-level=60 --min-partition-size=4
    // --max-partition-size=64 --sb-size=64 --profile=1` stream (1016 B, 17
    // intrabc 16x4 strips at ss (0,0)): 455240 of 921600 samples wrong, the
    // entropy fork at read 11750 (`decodetxb.c:158`, the chroma `txb_skip`).
    // `--enable-intrabc=0` at the same settings is byte-exact, and the 4:2:0
    // twin of the same recipe is byte-exact.
    //
    // 4:2:0 alone keeps every constant below verbatim: a 1:4 strip's chroma
    // plane block is 4:2:0's PAIR (the block above / beside it), because at
    // ss (1,1) the strip's 8x2 chroma lies INSIDE that neighbour's chroma.
    //
    // lane-av1422luma: at ss (1,0) that containment is gone -- 4:2:2 subsamples
    // x only, so the strip's chroma is 8x4 (full height) at the STRIP'S OWN
    // origin, and libaom's own clause agrees: `is_chroma_reference`
    // (av1_common_int.h:1454) ends in `|| !subsampling_y`, which answers TRUE
    // for every block at ss_y 0. Reading the strip's chroma at the 4:2:0 pair
    // origin instead puts the unit one mi row high: on `s422_416x250_10b`
    // (416x250 10-bit, sha256 d6aa7f45...) that is the V/U pair's `txb_skip`
    // read at mi (51,84) -- ctx 2 where the oracle reads 8, `all_zero` decoded
    // as 1 instead of 0, and the frame desyncs from there (luma included).
    // The origin half is `lane/av1422seed`'s unlanded hunk, reproduced here so
    // the arm is measurable whole; the gather and the publication extents
    // below are this lane's. VERT_4 stays on the pair: `ss_size_lookup
    // [BLOCK_4X16][1][0]` is BLOCK_INVALID at 4:2:2, so only HORZ reaches it.
    let own444 = ss_x(fctx) == 0 && ss_y(fctx) == 0;
    let own422 = ss_x(fctx) == 1 && ss_y(fctx) == 0 && horz;
    if own422 && has_chroma {
        hit!(INTRABC_RECT4_OWN_CHROMA422_HITS);
    }
    // One predicate for "this strip IS its own chroma reference": the strip's
    // own mi position, its own luma extent, and per-axis subsampled sizes.
    let own_chroma = own444 || own422;
    if ss_x(fctx) == 0 && ss_y(fctx) == 0 && has_chroma {
        hit!(INTRABC_RECT4_OWN_CHROMA444_HITS);
    }
    let (pw, ph) = if own444 {
        (bw, bh)
    } else if horz {
        (16usize, 8usize)
    } else {
        (8, 16)
    };
    let (cw, ch) = if own_chroma {
        (bw >> ss_x(fctx), bh >> ss_y(fctx))
    } else {
        // lane/av1422rect4dead: this arm is the 4:2:0 PAIR geometry, and at ss
        // (1, 0) it cannot run. The sole caller is `decode_rect4_16_strip`,
        // reached only through the two 16x16-level 1:4 arms; both sit under
        // `refuse_invalid_subsize((16, 16), part16)`, and at ss (1, 0) that
        // guard refuses exactly the shape that would land here:
        // `partition_subsize_dims((16, 16), PARTITION_VERT_4) == (4, 16)`,
        // which is in `PLANE_BLOCK_INVALID_422`. The surviving 16x4 HORZ_4
        // strip takes `own422` above, not this arm. At ss (1, 1) the arm is
        // live and correct (the pair IS the geometry), and at ss (0, 0)
        // `own444` already won, so this assert never fires there. The
        // assertion is the closure the repo rule asks for; the census test
        // `decode_rect4_16_intrabc_else_arm_stays_closed_at_422` below reds if
        // either this assert or its guard chain is removed.
        assert!(
            !(ss_x(fctx) == 1 && ss_y(fctx) == 0),
            "decode_rect4_16_intrabc else arm entered at ss (1, 0) with a 4x16 \
             VERT_4 strip: (4, 16) is BLOCK_INVALID at 4:2:2 and the 16x16-level \
             refuse_invalid_subsize above both callers must have refused this \
             before the dispatch -- the closure is gone"
        );
        (pw / 2, ph / 2)
    };
    let pair_mi = if has_chroma {
        if own_chroma {
            lmi
        } else if horz {
            (lmi.0 - 1, lmi.1)
        } else {
            (lmi.0, lmi.1 - 1)
        }
    } else {
        lmi
    };
    let (cpx, cpy) = if has_chroma {
        if own_chroma {
            (px >> ss_x(fctx), py >> ss_y(fctx))
        } else {
            (pair_mi.1 * MI / 2, pair_mi.0 * MI / 2)
        }
    } else {
        (0, 0)
    };
    let cside = cw.max(ch);
    let (pred_u, pred_v) = if has_chroma {
        let mut ud = vec![0u16; cw * ch];
        mc::predict_with_filter(
            &u.data,
            u.width,
            u.true_width,
            u.true_height,
            mv_to_q4(cpx, dv_col, ss_x(fctx)),
            mv_to_q4(cpy, dv_row, ss_y(fctx)),
            cw,
            ch,
            mc::InterpFilterKind::Bilinear,
            &mut ud,
            fctx,
        );
        let mut us = vec![0u16; cside * cside];
        for row in 0..ch {
            us[row * cside..][..cw].copy_from_slice(&ud[row * cw..][..cw]);
        }
        let mut vd = vec![0u16; cw * ch];
        mc::predict_with_filter(
            &v.data,
            v.width,
            v.true_width,
            v.true_height,
            mv_to_q4(cpx, dv_col, ss_x(fctx)),
            mv_to_q4(cpy, dv_row, ss_y(fctx)),
            cw,
            ch,
            mc::InterpFilterKind::Bilinear,
            &mut vd,
            fctx,
        );
        let mut vs = vec![0u16; cside * cside];
        for row in 0..ch {
            vs[row * cside..][..cw].copy_from_slice(&vd[row * cw..][..cw]);
        }
        (us, vs)
    } else {
        (Vec::new(), Vec::new())
    };

    // Key-frame `tx_select` is the parameter. `tx_select_inter` is forced
    // false for a key frame, so `read_block_tx_size_rect` would skip the
    // symbol; read it here against the parameter the intra path already uses.
    // A skipped intrabc block predicts the whole footprint once and codes
    // no residual (`decode_token_recon_block`'s inter branch). Lossless
    // still forces TX_4X4 when the block is coded.
    let leaves = if skip || (!lossless(fctx) && !tx_select) {
        None
    } else if lossless(fctx) {
        let mut leaves = Vec::new();
        for row in 0..bh / MI {
            for col in 0..bw / MI {
                leaves.push((row, col, 4usize, 4usize));
            }
        }
        // libaom's `else` branch (`!xd->lossless` fails the var-tx
        // condition): `set_txfm_ctxs(TX_4X4, ..)` over the block's own
        // width/height. Same reason as `read_block_tx_size_rect`'s arm --
        // it is what makes this body's `fill_lf_grid_rect_after_vartx`
        // suppression sound.
        txfm_partition_update_rect(neighbours, lmi, (4, 4), (bw, bh));
        Some(leaves)
    } else {
        let ctx = txfm_partition_ctx_rect(
            neighbours.above_txfm[lmi.1],
            neighbours.left_txfm[lmi.0],
            side,
            bw,
            bh,
        );
        let split = dec.symbol(&mut cdfs.txfm_partition[ctx]) == 1;
        if crate::envflags::env_flag!("EC_TRACE_MODE_STEP") {
            let (rng, _) = dec.debug_state();
            eprintln!(
                "EC_ISTEP mi_row={} mi_col={} name=txfm_split_rect val={} ctx={ctx} rng={rng}",
                lmi.0,
                lmi.1,
                u8::from(split)
            );
        }
        if !split {
            None
        } else {
            let (sub_w, sub_h) = sub_tx_size_map(bw, bh);
            let mi_cols = 2 * y.true_width.div_ceil(8);
            let mi_rows = 2 * y.true_height.div_ceil(8);
            let max_w_mi = (bw / MI).min(mi_cols.saturating_sub(lmi.1));
            let max_h_mi = (bh / MI).min(mi_rows.saturating_sub(lmi.0));
            let mut out = Vec::new();
            for row in (0..bh / MI).step_by(sub_h / MI) {
                for col in (0..bw / MI).step_by(sub_w / MI) {
                    read_var_tx_size(
                        dec,
                        cdfs,
                        neighbours,
                        lmi,
                        side,
                        (max_w_mi, max_h_mi),
                        (sub_w, sub_h),
                        1,
                        row,
                        col,
                        &mut out,
                    );
                }
            }
            Some(out)
        }
    };
    let (tx_w, tx_h) = leaves
        .as_ref()
        .and_then(|ls| ls.first())
        .map_or((bw, bh), |l| (l.2, l.3));

    let mut luma_tx_type = TxType::DctDct;
    let luma_grid;
    let mut u_grid = Grid::Zero(cw * ch);
    let mut v_grid = Grid::Zero(cw * ch);
    let chroma_stamped_per_unit;

    if skip {
        push_mc_rect(
            y,
            u,
            v,
            0,
            px,
            py,
            side,
            bw,
            bh,
            Pred::Inline(&pred_y, side),
            &ZERO_RESIDUAL[..side * bh],
            fctx,
        );
        luma_grid = Grid::Zero(bw * bh);
        if has_chroma {
            push_mc_rect(
                y,
                u,
                v,
                1,
                cpx,
                cpy,
                cside,
                cw,
                ch,
                Pred::Inline(&pred_u, cside),
                &ZERO_RESIDUAL[..cside * ch],
                fctx,
            );
            push_mc_rect(
                y,
                u,
                v,
                2,
                cpx,
                cpy,
                cside,
                cw,
                ch,
                Pred::Inline(&pred_v, cside),
                &ZERO_RESIDUAL[..cside * ch],
                fctx,
            );
        }
        chroma_stamped_per_unit = false;
    } else if let Some(ls) = leaves.as_ref() {
        if lossless(fctx) {
            for &(row, col, _tw, _th) in ls {
                let tu_mi = (lmi.0 + row, lmi.1 + col);
                let (tu_px, tu_py) = (px + col * MI, py + row * MI);
                let tu_pred = Pred::Inline(&pred_y, side).offset(row * MI * side + col * MI);
                let (g, _) = read_inter_plane(
                    dec,
                    cdfs,
                    if reduced_tx_set {
                        TxbSet::Luma4Inter
                    } else {
                        TxbSet::Luma4InterSet1
                    },
                    default_scan(4),
                    0,
                    neighbours.around_mi(tu_mi, 4)[0],
                    0,
                    y,
                    u,
                    v,
                    tu_px,
                    tu_py,
                    4,
                    base_q_idx,
                    tu_pred,
                    None,
                    Some(neighbours.luma_skip_ctx(tu_mi, 1)),
                    fctx,
                )?;
                neighbours.record_mi_luma_rect(tu_mi, 4, 4, &g);
            }
        } else {
            for (idx, &(row, col, tw, th)) in ls.iter().enumerate() {
                let tu_mi = (lmi.0 + row, lmi.1 + col);
                let (tu_px, tu_py) = (px + col * MI, py + row * MI);
                let tu_pred = Pred::Inline(&pred_y, side).offset(row * MI * side + col * MI);
                let (g, tt) = if tw == th {
                    read_inter_plane(
                        dec,
                        cdfs,
                        inter_txbset_for(tw, reduced_tx_set),
                        default_scan(tw.min(32)),
                        0,
                        neighbours.around_mi(tu_mi, tw)[0],
                        0,
                        y,
                        u,
                        v,
                        tu_px,
                        tu_py,
                        tw,
                        base_q_idx,
                        tu_pred,
                        None,
                        Some(neighbours.luma_skip_ctx(tu_mi, tw / MI)),
                        fctx,
                    )?
                } else {
                    read_inter_plane_rect(
                        dec,
                        cdfs,
                        rect_inter_luma_set(tw, th, fctx)?,
                        (tw, th),
                        side,
                        0,
                        neighbours.around_mi_rect(tu_mi, tw, th)[0],
                        0,
                        y,
                        u,
                        v,
                        tu_px,
                        tu_py,
                        tu_pred,
                        None,
                        Some(neighbours.luma_skip_ctx_rect(tu_mi, tw / MI, th / MI)),
                        fctx,
                    )?
                };
                if idx == 0 {
                    luma_tx_type = tt;
                }
                neighbours.record_mi_luma_rect(tu_mi, tw, th, &g);
            }
        }
        luma_grid = Grid::Zero(bw * bh);
        chroma_stamped_per_unit = has_chroma && lossless(fctx);
        if has_chroma && lossless(fctx) {
            let (mut uo, mut vo) = (Vec::new(), Vec::new());
            read_inter_chroma_lossless(
                y,
                u,
                v,
                dec,
                cdfs,
                neighbours,
                Pred::Inline(&pred_u, cside),
                Pred::Inline(&pred_v, cside),
                pair_mi,
                (cpx, cpy),
                (0, 0),
                (cw, ch),
                (cw, ch),
                cside,
                0,
                base_q_idx,
                &mut uo,
                &mut vo,
                fctx,
            )?;
            u_grid = if uo.is_empty() {
                Grid::Zero(cw * ch)
            } else {
                Grid::Own(uo)
            };
            v_grid = if vo.is_empty() {
                Grid::Zero(cw * ch)
            } else {
                Grid::Own(vo)
            };
        } else if has_chroma {
            // lane-av1422luma: the own-chroma route reads its context over its
            // OWN extent; at ss (1,0) it additionally goes through the
            // every-second-column gather (one chroma 4-px column spans two
            // luma mi columns, so the per-mi gather would count each column's
            // whole-unit dc sign twice and OR in cells the unit does not
            // cover) -- the same choice the `decode_rect4_16` chroma arm
            // makes at `decode.rs:19577`. 4:4:4 keeps the per-mi gather: at
            // ss_x 0 a chroma column IS a luma mi column, so sampling would
            // halve its own above span.
            let (gmi, gw, gh) = if own_chroma {
                (lmi, bw, bh)
            } else {
                (pair_mi, pw, ph)
            };
            let around = if own422 {
                neighbours.around_mi_422_chroma(gmi, gw, gh)
            } else {
                neighbours.around_mi_rect(gmi, gw, gh)
            };
            let (ug, _) = read_inter_plane_rect(
                dec,
                cdfs,
                rect_inter_chroma_set(cw, ch)?,
                (cw, ch),
                cside,
                1,
                around[1],
                0,
                y,
                u,
                v,
                cpx,
                cpy,
                Pred::Inline(&pred_u, cside),
                Some(luma_tx_type),
                None,
                fctx,
            )?;
            u_grid = ug;
            let (vg, _) = read_inter_plane_rect(
                dec,
                cdfs,
                rect_inter_chroma_set(cw, ch)?,
                (cw, ch),
                cside,
                2,
                around[2],
                0,
                y,
                u,
                v,
                cpx,
                cpy,
                Pred::Inline(&pred_v, cside),
                Some(luma_tx_type),
                None,
                fctx,
            )?;
            v_grid = vg;
        }
    } else {
        let around = neighbours.around_mi_rect(lmi, bw, bh);
        let (g, tt) = read_inter_plane_rect(
            dec,
            cdfs,
            rect_inter_luma_set(bw, bh, fctx)?,
            (bw, bh),
            side,
            0,
            around[0],
            0,
            y,
            u,
            v,
            px,
            py,
            Pred::Inline(&pred_y, side),
            None,
            None,
            fctx,
        )?;
        luma_tx_type = tt;
        luma_grid = g;
        chroma_stamped_per_unit = false;
        if has_chroma {
            // lane-av1422luma: as the lossless twin above -- own extent, and
            // the sampled gather at 4:2:2 only.
            let (gmi, gw, gh) = if own_chroma {
                (lmi, bw, bh)
            } else {
                (pair_mi, pw, ph)
            };
            let around = if own422 {
                neighbours.around_mi_422_chroma(gmi, gw, gh)
            } else {
                neighbours.around_mi_rect(gmi, gw, gh)
            };
            let (ug, _) = read_inter_plane_rect(
                dec,
                cdfs,
                rect_inter_chroma_set(cw, ch)?,
                (cw, ch),
                cside,
                1,
                around[1],
                0,
                y,
                u,
                v,
                cpx,
                cpy,
                Pred::Inline(&pred_u, cside),
                Some(luma_tx_type),
                None,
                fctx,
            )?;
            u_grid = ug;
            let (vg, _) = read_inter_plane_rect(
                dec,
                cdfs,
                rect_inter_chroma_set(cw, ch)?,
                (cw, ch),
                cside,
                2,
                around[2],
                0,
                y,
                u,
                v,
                cpx,
                cpy,
                Pred::Inline(&pred_v, cside),
                Some(luma_tx_type),
                None,
                fctx,
            )?;
            v_grid = vg;
        }
    }
    drop(restored);

    if leaves.is_none() {
        neighbours.record_mi_luma_rect(lmi, bw, bh, &luma_grid);
    }
    neighbours.record_mode_mi(lmi.0, lmi.1, mi_w, mi_h, DC_PRED);
    neighbours.fill_skip_grid_rect(lmi, mi_w, mi_h, skip);
    // lane-av1oddheightfork3: this body reads its own var-tx tree inline
    // (`read_var_tx_size` per child), so the bands are per-leaf already; the
    // whole-footprint publish overwrote them with the block's own size.
    neighbours.fill_lf_grid_rect_after_vartx(lmi, mi_w, mi_h, tx_w as u8, tx_h as u8, 0, fctx);
    for cell in 0..mi_w {
        if let Some(slot) = neighbours.above_side_mi.get_mut(lmi.1 + cell) {
            *slot = bw as u8;
        }
    }
    for cell in 0..mi_h {
        if let Some(slot) = neighbours.left_side_mi.get_mut(lmi.0 + cell) {
            *slot = bh as u8;
        }
    }
    let mut last_uv = None;
    if has_chroma {
        hit!(RECT4_16_CHROMA_HITS);
        // lane-av1422luma: the uv-mode publication and the left/above skip
        // state below are the STRIP's own at 4:2:2 / 4:4:4 -- publishing the
        // pair extent (one mi row off, `ph`/`pw`) is what the NEXT block's
        // chroma skip context then reads.
        let (rmi, rw, rh) = if own_chroma {
            (lmi, bw, bh)
        } else {
            (pair_mi, pw, ph)
        };
        neighbours.record_uv_mode_mi(rmi.0, rmi.1, rw / MI, rh / MI, DC_PRED);
        if !chroma_stamped_per_unit {
            let u_state = neighbour_state(&u_grid);
            let v_state = neighbour_state(&v_grid);
            let round_up_even = |n: usize| n.div_ceil(2) * 2;
            let bound_h = round_up_even(neighbours.mi_rows);
            let bound_w = round_up_even(neighbours.mi_cols);
            for cell in 0..(rh / MI) {
                if rmi.0 + cell < bound_h
                    && let Some(slot) = neighbours.left.get_mut(rmi.0 + cell)
                {
                    slot[1] = u_state;
                    slot[2] = v_state;
                }
            }
            for cell in 0..(rw / MI) {
                if rmi.1 + cell < bound_w
                    && let Some(slot) = neighbours.above.get_mut(rmi.1 + cell)
                {
                    slot[1] = u_state;
                    slot[2] = v_state;
                }
            }
        }
        last_uv = Some(DC_PRED);
    }
    if horz {
        hit!(RECT4_16_HORZ_HITS);
    } else {
        hit!(RECT4_16_VERT_HITS);
    }
    record_strip_palette(neighbours, lmi, bw, bh, 0, [0u16; 8], 0, [0u16; 8]);
    if leaves.is_none() {
        // Skip and TX_MODE_LARGEST publish the block size (`set_txfm_ctxs`
        // with `skip && is_inter`, or the unsplit unit). A split tree already
        // published per leaf inside `read_var_tx_size`.
        txfm_partition_update_rect(neighbours, lmi, (tx_w, tx_h), (bw, bh));
    }
    record_intrabc_mi_rect(
        lmi.0,
        lmi.1,
        mi_w,
        mi_h,
        Some((dv_row, dv_col)),
        neighbours,
        fctx,
    );
    Ok((DC_PRED, last_uv, (tx_w, tx_h)))
}

/// Restores `reduced_tx_set_inter` when the intrabc pair reader returns,
/// including through `?`.
pub(super) struct ReducedRestore<'a> {
    pub(super) fctx: &'a crate::decode::FrameCtx,
    pub(super) saved: bool,
}
impl Drop for ReducedRestore<'_> {
    fn drop(&mut self) {
        self.fctx.reduced_tx_set_inter.with(|c| c.set(self.saved));
    }
}
fn scopeguard_reduced(fctx: &crate::decode::FrameCtx, saved: bool) -> ReducedRestore<'_> {
    ReducedRestore { fctx, saved }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn decode_rect4_16_strip(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    lmi: (usize, usize),
    horz: bool,
    has_chroma: bool,
    prev: Option<((usize, usize), usize)>,
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    enable_filter_intra: bool,
    allow_screen_content_tools: bool,
    base_q_idx: u8,
    tx_select: bool,
    reduced_tx_set: bool,
    fctx: &crate::decode::FrameCtx,
) -> Result<(usize, Option<usize>, (usize, usize))> {
    let (r, c) = (lmi.0 / (SUB / MI), lmi.1 / (SUB / MI));
    let (bw, bh) = if horz { (16usize, 4usize) } else { (4, 16) };
    let (mi_w, mi_h) = (bw / MI, bh / MI);
    let scan: &[u16] = if horz { &SCAN_16X4 } else { &SCAN_4X16 };
    let mut last_uv_mode: Option<usize> = None;
    // `decode_block_rect4`'s own neighbour rule: the coarse 16-px cell
    // aliases all four strips onto one entry, so the mi-exact map wins and
    // the previous strip stands in on the split axis.
    let mut above_mode = neighbours.above_mode[c];
    let mut left_mode = neighbours.left_mode[r];
    if let Some(((pr, pc), pmode)) = prev {
        if pc == lmi.1 && lmi.0 == pr + mi_h {
            above_mode = pmode;
        } else if pr == lmi.0 && lmi.1 == pc + mi_w {
            left_mode = pmode;
        }
    }
    let (above_mode, left_mode) =
        neighbours.modes_above_left_mi(lmi.0, lmi.1, (above_mode, left_mode));
    // lane-kf900 r4: the strip's real palette ctx + colour cache (see
    // [`decode_block_rect4`]); `None`/`&[]` here read `palette_y_mode`
    // off CDF row 0 and every cache hit off an empty cache.
    let (palette_ctx, palette_cache) = neighbours.palette_ctx_and_cache_mi(lmi);
    let palette_uv_cache = neighbours.palette_uv_cache_mi(lmi);
    let (
        skip,
        mode,
        angle_delta_y,
        uv_mode,
        angle_delta_uv,
        alpha,
        filter_intra,
        palette_y,
        palette_uv,
        intrabc,
    ) = read_intra_mode_rect(
        dec,
        cdfs,
        above_mode,
        left_mode,
        true,
        has_chroma,
        bw,
        bh,
        enable_filter_intra,
        neighbours.skip_txfm_ctx(lmi.0, lmi.1),
        allow_screen_content_tools,
        Some((palette_ctx, &palette_cache)),
        &palette_uv_cache,
        lmi.0,
        lmi.1,
        fctx,
    )?;
    if let Some(dv) = intrabc {
        return decode_rect4_16_intrabc(
            dec,
            cdfs,
            neighbours,
            lmi,
            horz,
            has_chroma,
            skip,
            dv,
            y,
            u,
            v,
            base_q_idx,
            tx_select,
            reduced_tx_set,
            fctx,
        );
    }
    if palette_y.is_some() || palette_uv.is_some() {
        hit!(RECT4_PALETTE_HITS);
    }
    // The colour-index map through the base colours IS the prediction.
    // Chroma's map is the PAIR's 8x4 (4x8) block, matching the one
    // chroma transform this closing strip codes.
    let palette_y_buf: Option<Vec<u16>> = palette_y
        .as_ref()
        .map(|p| p.map.iter().map(|&i| p.colors[i as usize]).collect());
    let palette_uv_bufs: Option<(Vec<u16>, Vec<u16>)> = palette_uv.as_ref().map(|p| {
        (
            p.map.iter().map(|&i| p.u_colors[i as usize]).collect(),
            p.map.iter().map(|&i| p.v_colors[i as usize]).collect(),
        )
    });
    let smooth_neighbor = is_smooth_mode(above_mode) || is_smooth_mode(left_mode);
    if smooth_neighbor {
        hit!(SMOOTH_LUMA_HITS);
    }
    if filter_intra.is_some() {
        hit!(FILTER_INTRA_RECT_HITS);
    }
    // `bsize_to_tx_size_cat(BLOCK_16X4)`: `max_txsize_rect_lookup` is
    // TX_16X4 and `sub_tx_size_map` reaches TX_4X4 in two steps, so the
    // depth symbol is the 3-symbol `tx_size_cat1` -- not the 2-symbol
    // `cat0` a 4x8 leaf reads nor the `cat2` a 32x8 strip does.
    // lane-av1mixloss: the lossless-segment early return
    // (`decodeframe.c:1203`) precedes `TX_MODE_SELECT`, so a
    // lossless-segment strip codes no depth symbol here either (class
    // sweep of the sub8 leaf's fix).
    let depth = if tx_select && !lossless(fctx) {
        // lane-av1txbands: every frame reads the real `TXFM_CONTEXT` bands
        // (`get_tx_size_context`, libaom `pred_common.h:342`).
        let ctx = tx_size_context_txfm_rect(neighbours, lmi, bw, bh, fctx);
        let d = dec.symbol(&mut cdfs.tx_size_cat1[ctx]);
        if crate::envflags::env_flag!("EC_TRACE_MODE_STEP") {
            eprintln!(
                "EC_ISTEP mi_row={} mi_col={} name=tx_depth val={d} ctx={ctx} cat=1 rng={}",
                lmi.0,
                lmi.1,
                dec.debug_state().0
            );
        }
        d
    } else {
        0
    };
    if depth != 0 {
        hit!(TX_DEPTH_HITS);
        hit!(RECT4_16_SPLIT_HITS);
    }
    let (tx_w, tx_h) = depth_to_tx_wh(bw, bh, depth, fctx);
    let (px, py) = (lmi.1 * MI, lmi.0 * MI);
    let reach = Reach::of_rect(bw, bh, px, py, y.width, y.height, fctx);
    neighbours.record_mode_mi(lmi.0, lmi.1, mi_w, mi_h, mode);
    if depth != 0 || lossless(fctx) {
        // [`decode_rect_split`]'s per-unit walk, inlined because the
        // CHROMA of this shape belongs to the strip's 4:2:0 pair, not to
        // the strip (that function codes one chroma transform per block).
        // depth 1 is the 2:1 rect unit TX_8X4/TX_4X8
        // (`sub_tx_size_map[TX_16X4]`), depth 2 the square TX_4X4; each
        // unit predicts off the units before it in raster order.
        let rect_unit = tx_w != tx_h;
        let zero_tu = vec![0i32; tx_w * tx_h];
        for tu_row in 0..bh / tx_h {
            for tu_col in 0..bw / tx_w {
                let tu_mi = (lmi.0 + tu_row * (tx_h / MI), lmi.1 + tu_col * (tx_w / MI));
                let (tu_px, tu_py) = (px + tu_col * tx_w, py + tu_row * tx_h);
                let tu_reach =
                    Reach::of_tu(bw, bh, tu_col * tx_w, tu_row * tx_h, tx_w, tx_h, reach);
                if let Some(b) = &palette_y_buf {
                    let tu_buf: Vec<u16> = (0..tx_h)
                        .flat_map(|i| {
                            let base = (tu_row * tx_h + i) * bw + tu_col * tx_w;
                            b[base..base + tx_w].iter().copied()
                        })
                        .collect();
                    set_palette_pred(tu_buf, fctx);
                }
                if skip {
                    push_intra_rect(
                        y,
                        u,
                        v,
                        0,
                        tu_px,
                        tu_py,
                        tx_w,
                        tx_h,
                        mode,
                        angle_delta_y,
                        tu_reach,
                        &zero_tu,
                        None,
                        filter_intra,
                        smooth_neighbor,
                        fctx,
                    );
                    neighbours.record_mi_luma_rect(tu_mi, tx_w, tx_h, &zero_tu);
                } else if rect_unit {
                    // `get_txsize_entropy_ctx(TX_8X4)` is the 8x8 class with
                    // 32 coded positions and the `TX_4X4` `tx_type` row:
                    // exactly [`TxbSet::LumaRect4x8`].
                    let tu_around = neighbours.around_mi_rect(tu_mi, tx_w, tx_h)[0];
                    let tu_skip_ctx = neighbours.luma_skip_ctx_rect(tu_mi, tx_w / MI, tx_h / MI);
                    let set = if reduced_tx_set {
                        TxbSet::LumaRect4x8
                    } else {
                        TxbSet::LumaRect4x8Set1
                    };
                    let tu_scan: &[u16] = if tx_w > tx_h { &SCAN_8X4 } else { &SCAN_4X8 };
                    let mut coding = cdfs.txb(set, fi_tx_row(mode, filter_intra));
                    let (tu_grid, tu_tx_type) = read_coeffs_rect(
                        dec,
                        &mut coding,
                        tu_scan,
                        tx_w,
                        tx_h,
                        tu_skip_ctx,
                        dc_sign_ctx(tu_around.2),
                        TxType::DctDct,
                        0,
                    )?;
                    let residual = dequant_and_inverse_typed_wh(
                        &tu_grid,
                        tx_w,
                        tx_h,
                        crate::decode::bit_depth(fctx),
                        block_q_idx(fctx),
                        plane_q_delta(0, fctx).0,
                        plane_q_delta(0, fctx).1,
                        tu_tx_type,
                        block_iqmatrix(fctx, 0, tx_w, tx_h, tu_tx_type),
                    );
                    push_intra_rect(
                        y,
                        u,
                        v,
                        0,
                        tu_px,
                        tu_py,
                        tx_w,
                        tx_h,
                        mode,
                        angle_delta_y,
                        tu_reach,
                        &residual,
                        None,
                        filter_intra,
                        smooth_neighbor,
                        fctx,
                    );
                    if tu_grid.iter().any(|&l| l != 0) {
                        hit!(RECT4_16_COEFF_HITS);
                    }
                    neighbours.record_mi_luma_rect(tu_mi, tx_w, tx_h, &tu_grid);
                } else {
                    let tu_around = neighbours.around_mi(tu_mi, tx_w)[0];
                    let tu_skip_ctx = neighbours.luma_skip_ctx(tu_mi, tx_w / MI);
                    let tu_grid = read_plane(
                        dec,
                        cdfs,
                        txbset_for(tx_w, reduced_tx_set),
                        default_scan(tx_w),
                        0,
                        tu_around,
                        mode,
                        mode,
                        angle_delta_y,
                        tu_reach,
                        y,
                        u,
                        v,
                        tu_px,
                        tu_py,
                        tx_w,
                        tx_w,
                        base_q_idx,
                        None,
                        filter_intra,
                        Some(tu_skip_ctx),
                        smooth_neighbor,
                        fctx,
                    )?;
                    if tu_grid.iter().any(|&l| l != 0) {
                        hit!(RECT4_16_COEFF_HITS);
                    }
                    neighbours.record_mi_luma(tu_mi, tx_w, &tu_grid);
                }
            }
        }
    } else {
        let levels = if skip {
            let zeros = Grid::Zero(bw * bh);
            if let Some(b) = &palette_y_buf {
                set_palette_pred(b.clone(), fctx);
            }
            push_intra_rect(
                y,
                u,
                v,
                0,
                px,
                py,
                bw,
                bh,
                mode,
                angle_delta_y,
                reach,
                &zeros,
                None,
                filter_intra,
                smooth_neighbor,
                fctx,
            );
            zeros
        } else {
            // The strip's luma TU *is* the strip (`plane_bsize ==
            // txsize_to_bsize[tx_size]`), so `get_txb_ctx` fixes
            // `txb_skip_ctx` at 0.
            let around = neighbours.around_mi_rect(lmi, bw, bh)[0];
            let set = if reduced_tx_set {
                TxbSet::LumaRect16x4
            } else {
                TxbSet::LumaRect16x4Set1
            };
            let mut coding = cdfs.txb(set, fi_tx_row(mode, filter_intra));
            let (levels, tx_type) = read_coeffs_rect(
                dec,
                &mut coding,
                scan,
                bw,
                bh,
                0,
                dc_sign_ctx(around.2),
                TxType::DctDct,
                0,
            )?;
            let residual = dequant_and_inverse_typed_wh(
                &levels,
                bw,
                bh,
                crate::decode::bit_depth(fctx),
                block_q_idx(fctx),
                plane_q_delta(0, fctx).0,
                plane_q_delta(0, fctx).1,
                tx_type,
                block_iqmatrix(fctx, 0, bw, bh, tx_type),
            );
            if let Some(b) = &palette_y_buf {
                set_palette_pred(b.clone(), fctx);
            }
            push_intra_rect(
                y,
                u,
                v,
                0,
                px,
                py,
                bw,
                bh,
                mode,
                angle_delta_y,
                reach,
                &residual,
                None,
                filter_intra,
                smooth_neighbor,
                fctx,
            );
            hit!(RECT4_16_COEFF_HITS);
            levels
        };
        neighbours.record_mi_luma_rect(lmi, bw, bh, &levels);
    }
    neighbours.fill_skip_grid_rect(lmi, mi_w, mi_h, skip);
    neighbours.fill_lf_grid_rect(lmi, mi_w, mi_h, tx_w as u8, tx_h as u8, 0, fctx);
    // `update_partition_context` writes `partition_context_lookup[
    // BLOCK_16X4]` over the whole 16x16: full width above, a 4-px side on
    // the left (mirrored under VERT_4).
    for cell in 0..mi_w {
        if let Some(slot) = neighbours.above_side_mi.get_mut(lmi.1 + cell) {
            *slot = bw as u8;
        }
    }
    for cell in 0..mi_h {
        if let Some(slot) = neighbours.left_side_mi.get_mut(lmi.0 + cell) {
            *slot = bh as u8;
        }
    }
    if has_chroma {
        hit!(RECT4_16_CHROMA_HITS);
        // lane-av1-444: at 4:4:4 there is no pair -- every strip is its
        // own chroma reference and codes its own 16x4 / 4x16 chroma
        // block. (`set_mi_offsets`'s chroma snap is the zero offset at
        // ss 0/0, so the neighbour reads below anchor at the strip.)
        let (pair_mi, pw, ph) = if ss_x(fctx) == 0 && ss_y(fctx) == 0 {
            (lmi, bw, bh)
        } else if ss_y(fctx) == 0 && horz {
            // lane-av1-422: chroma height is NOT subsampled, so a HORZ
            // 16x4 strip is its own chroma reference (`is_chroma_reference`
            // at ss(1,0): bh == 1 mi, so the row clause reduces to
            // `!ss_y`) and codes its own `ss_size_lookup[BLOCK_16X4][1][0]`
            // = BLOCK_8X4 chroma block -- a valid transform, no pair.
            // (lane-av1-422o: a VERTICAL 4x16 strip now falls through to
            // the pair merge below -- its own chroma block
            // `ss_size_lookup[BLOCK_4X16][1][0]` is BLOCK_INVALID, the
            // walk proved such leaves exist, and libaom codes the pair's
            // chroma from the closing strip; the chunk lives in the
            // `chroma422_pair16` arm below.)
            (lmi, bw, bh)
        } else if horz {
            ((lmi.0 - 1, lmi.1), 16, 8)
        } else {
            ((lmi.0, lmi.1 - 1), 8, 16)
        };
        let (ppx, ppy) = (pair_mi.1 * MI, pair_mi.0 * MI);
        let (cpx, cpy) = (ppx >> ss_x(fctx), ppy >> ss_y(fctx));
        let (cw, ch) = (pw >> ss_x(fctx), ph >> ss_y(fctx));
        // lane-av1-422o: the 4:2:2 pair's chroma of a VERT 4x16 strip
        // pair: (4, 16) at ss(1,0). The bsize is still BLOCK_4X16, whose
        // `ss_size_lookup[..][1][0]` is BLOCK_INVALID, so libaom's
        // `av1_get_tx_size` reads `max_txsize_rect_lookup[255]` -- the
        // out-of-bounds byte is TX_4X8 in the pinned Release oracle, and
        // `av1_get_adjusted_tx_size` has no TX_4X8 case to adjust it.
        // `decode_token_recon_block` walks unit_width =
        // ROUND_POWER_OF_TWO(1, 1) = 1, unit_height = 4, with stepr =
        // tx_size_high_unit[TX_4X8] = 2: TWO TX_4X8 units per plane
        // (chroma rows 0..8 and 8..16), plane-major. Even the skip arm
        // walks per unit: `predict_and_reconstruct_intra_block` predicts
        // every unit regardless of `skip_txfm`.
        let chroma422_pair16 = (cw, ch) == (4, 16) && ss_x(fctx) == 1;
        let uv_predict_mode = if uv_mode == UV_CFL_PRED {
            DC_PRED
        } else {
            uv_mode
        };
        if (V_PRED..=D67_PRED).contains(&uv_predict_mode) {
            hit!(RECT4_16_CHROMA_DIR_HITS);
        }
        let pair_reach = Reach::of_rect(pw, ph, ppx, ppy, y.width, y.height, fctx);
        // lane-kf1200 r1: the chroma edge-filter type reads the neighbours
        // of the CHROMA block, which for a 16x4/4x16 pair is `pair_mi`'s
        // 16x8/8x16 region -- libaom builds `chroma_above_mbmi`/
        // `chroma_left_mbmi` from `mi[-((mi_row & ss_y) * stride + (mi_col
        // & ss_x))]` (`set_mi_offsets`, av1/common/blockd.h ~1050), i.e.
        // the pair's top-left luma mi, never this leaf's odd row/column.
        // Reading `lmi` here named the leaf directly above (a non-chroma
        // -reference 4-row block, `uv_mode == UV_DC_PRED`) and so lost a
        // SMOOTH neighbour: filt_type 0 instead of 1 turned
        // `av1_use_intra_edge_upsample` on (blk_wh 12 <= 16) for a
        // directional chroma strip, moving 4 samples by 1.
        let smooth_neighbor_uv = neighbours.smooth_uv_neighbour(pair_mi.0, pair_mi.1, r, c, fctx);
        // lane-av1422warp: `smooth_uv_neighbour_unsnapped` now takes the
        // frame's own `ss_x` -- the unsnapped read is the pair-merge witness
        // counter's mirror of the snapping form, and the shipped read offsets
        // the above COLUMN by `+ss_x` (libaom's `chroma_above_mi`).
        //
        // lane-av1leftwit: this mirror is NOT evidence about the left read's
        // `+ss_y` term, whatever a report once said. It reads the leaf's own
        // (odd) mi row, which no decode call site does -- every one of them
        // uses the snapping form, whose base row is even -- and its result
        // feeds `RECT4_16_UV_PAIR_FILT_HITS` only. What it measures is real
        // and worth keeping (the pair's chroma-reference read against the
        // leaf's own row: 51/24/121/0/1 hits on the five fixtures censused),
        // so it stays; the decode path's own reach for the `+ss_y` term is
        // `uv_left_1mi_split_hits()`.
        if smooth_neighbor_uv
            != neighbours.smooth_uv_neighbour_unsnapped(
                lmi.0,
                lmi.1,
                r,
                c,
                ss_x(fctx),
                ss_y(fctx),
                false,
            )
        {
            hit!(RECT4_16_UV_PAIR_FILT_HITS);
        }
        // lane-av1-rect14: a lossless frame forces TX_4X4 on every plane
        // (`av1_get_tx_size`, blockd.h:1381). The pair's chroma block is
        // 8x4 (HORZ_4) or 4x8 (VERT_4) -- two TX_4X4s, plane-major
        // (`decode_token_recon_block`), not one `ChromaRect8x4`. Reading
        // the rect here consumed one symbol block too few and desynced
        // the next strip's `skip` (measured: lossless sb128 320x240,
        // mi(1,20) uv_mode matched, the following skip did not).
        let lossless_pair = lossless(fctx);
        let (u_levels, v_levels) = if lossless_pair {
            hit!(RECT4_16_LOSSLESS_CHROMA_HITS);
            if skip {
                for plane in 1..3usize {
                    for rr in 0..ch / 4 {
                        for cc in 0..cw / 4 {
                            let (ox, oy) = (cc * 4, rr * 4);
                            // The unit's origin and extent in LUMA pixels: `<< ss`
                            // at 4:2:0 (8x8 luma for a 4x4 chroma unit), identity
                            // at 4:4:4 -- the same form as the block-walk sibling
                            // at decode.rs:35342. The hardcoded `ox * 2 / 8` is
                            // only right at ss (1,1): at 4:4:4 it claims a 4x4
                            // chroma unit is 8x8 luma, so `of_tu`'s
                            // `col_off + tx_w < bw` test answers against a
                            // footprint twice the real one and every unit past
                            // the block's midpoint wrongly reports
                            // `above_right = false` -- a directional chroma
                            // strip then predicts from a neighbour libaom
                            // never reads (measured: key frame 0, 4:4:4 lossless
                            // 16x4 strip at (104,26), U 29 + V 34 samples wrong,
                            // entropy bit-identical over 351109 EC_SYMR reads).
                            // lane-av1422llpred r2: this site had the OTHER half
                            // of the same shape -- `tu_reach` takes ONE size for
                            // both axes, so a subsampled chroma unit's height
                            // came from `<< ss_x` instead of `<< ss_y` (a TX_4x4
                            // chroma unit is `4 << ss_x` luma px wide but
                            // `4 << ss_y` tall). Identical at 4:2:0/4:4:4.
                            let cu_reach = tu_reach_rect(
                                pw,
                                ph,
                                ox << ss_x(fctx),
                                oy << ss_y(fctx),
                                4 << ss_x(fctx),
                                4 << ss_y(fctx),
                                pair_reach,
                                ppx,
                                ppy,
                                y.width,
                                y.height,
                                fctx,
                            );
                            if let Some((ub, vb)) = &palette_uv_bufs {
                                let buf = if plane == 1 { ub } else { vb };
                                set_palette_pred(palette_window(buf, cw, ox, oy, 4, 4), fctx);
                            }
                            push_intra(
                                y,
                                u,
                                v,
                                plane,
                                cpx + ox,
                                cpy + oy,
                                4,
                                uv_predict_mode,
                                angle_delta_uv,
                                cu_reach,
                                &ZERO_RESIDUAL[..16],
                                None,
                                None,
                                smooth_neighbor_uv,
                                fctx,
                            );
                            let cu_mi = (
                                pair_mi.0 + (oy << ss_y(fctx)) / MI,
                                pair_mi.1 + (ox << ss_x(fctx)) / MI,
                            );
                            neighbours.record_mi_chroma(
                                cu_mi,
                                4 << ss_x(fctx),
                                4 << ss_y(fctx),
                                plane,
                                &ZERO_RESIDUAL[..16],
                            );
                        }
                    }
                }
                (Grid::Zero(cw * ch), Grid::Zero(cw * ch))
            } else {
                let scan4 = default_scan(4);
                // `get_txb_ctx`: offset 10 (our tables store it at +3)
                // when the plane block is larger than the transform.
                let offset = Some(3usize);
                let mut uo = vec![0i32; cw * ch];
                let mut vo = vec![0i32; cw * ch];
                for plane in 1..3usize {
                    for rr in 0..ch / 4 {
                        for cc in 0..cw / 4 {
                            let (ox, oy) = (cc * 4, rr * 4);
                            if cpx + ox >= u.true_width || cpy + oy >= u.true_height {
                                continue;
                            }
                            // The unit's stamp is its own span: aom
                            // `av1_set_entropy_contexts` (blockd.c:29)
                            // memsets exactly `tx_size_wide_unit` x
                            // `tx_size_high_unit` cells at the unit's own
                            // `(aoff, loff)` -- for TX_4X4 ONE above cell
                            // and ONE left cell. `(oy / 2, ox / 2)` +
                            // `(8, 8)` is the 4:2:0 subsampled form (one
                            // chroma cell = 8 luma px = 2 mi cells); at
                            // 4:4:4 a unit is ONE mi cell, and the 2x2
                            // smear let a 4-wide strip's later units
                            // overwrite the NEXT COLUMN's above cell with
                            // their own state -- a column aom never sees
                            // this block write -- so the following block's
                            // `all_zero` read the wrong `txb_skip` row
                            // (frame-0 read #452, the llkf named gap).
                            let cu_mi = (
                                pair_mi.0 + (oy << ss_y(fctx)) / MI,
                                pair_mi.1 + (ox << ss_x(fctx)) / MI,
                            );
                            // One luma-mi cell: the chroma entropy arrays
                            // are subsampled, and our bands replicate that
                            // one entry over the unit's two cells
                            // (`read_intra_chroma_lossless`).
                            let cu_around = neighbours.around_mi(cu_mi, MI);
                            // Same ss-aware luma footprint as the skip arm
                            // above -- `ox * 2 / 8` is the 4:2:0-only form and
                            // mis-answers `of_tu` at 4:4:4 (see there).
                            // lane-av1422llpred r2: and the SKIP arm's fix
                            // missed the other half of the same shape -- one
                            // size for both axes makes the height come from
                            // `<< ss_x`, so only 4:2:2 can reach it.
                            let cu_reach = tu_reach_rect(
                                pw,
                                ph,
                                ox << ss_x(fctx),
                                oy << ss_y(fctx),
                                4 << ss_x(fctx),
                                4 << ss_y(fctx),
                                pair_reach,
                                ppx,
                                ppy,
                                y.width,
                                y.height,
                                fctx,
                            );
                            if let Some((ub, vb)) = &palette_uv_bufs {
                                let buf = if plane == 1 { ub } else { vb };
                                set_palette_pred(palette_window(buf, cw, ox, oy, 4, 4), fctx);
                            }
                            let cu_grid = read_plane(
                                dec,
                                cdfs,
                                TxbSet::Chroma4,
                                scan4,
                                plane,
                                cu_around[plane],
                                mode,
                                uv_predict_mode,
                                angle_delta_uv,
                                cu_reach,
                                y,
                                u,
                                v,
                                cpx + ox,
                                cpy + oy,
                                4,
                                4,
                                base_q_idx,
                                None,
                                None,
                                offset,
                                smooth_neighbor_uv,
                                fctx,
                            )?;
                            neighbours.record_mi_chroma(
                                cu_mi,
                                4 << ss_x(fctx),
                                4 << ss_y(fctx),
                                plane,
                                &cu_grid,
                            );
                            hit!(CHROMA_SPLIT_TX_HITS);
                            let dst = if plane == 1 { &mut uo } else { &mut vo };
                            for row in 0..4 {
                                let start = (oy + row) * cw + ox;
                                dst[start..start + 4].copy_from_slice(&cu_grid[row * 4..][..4]);
                            }
                        }
                    }
                }
                (Grid::Own(uo), Grid::Own(vo))
            }
        } else if skip {
            let zeros = Grid::Zero(cw * ch);
            // libaom `predict_and_reconstruct_intra_block` (decodeframe.c
            // ~1000) predicts EVERY plane and only guards the residual on
            // `mbmi->skip_txfm`, so a skipped `UV_CFL_PRED` block is still
            // DC + alpha * luma-AC. Dropping the CfL term here left a
            // chroma-only bounded box on ReelC's key frames.
            let ac = alpha.map(|_| cfl_src_rect(ppx, ppy, pw, ph));
            if alpha.is_some() {
                hit!(CHROMA_SKIP_CFL_HITS);
            }
            for plane in [1usize, 2] {
                if chroma422_pair16 {
                    // lane-av1-422o: two TX_4X8 units, each predicted on
                    // its own edges (the lower one off the upper one's
                    // samples); no coefficients are coded, and libaom's
                    // `set_entropy_context` zeroed the block's own cells,
                    // so the pair-wide zero stamp below stays correct.
                    for unit_row in 0..2usize {
                        let unit_reach = tu_reach_rect(
                            pw,
                            ph,
                            0,
                            unit_row * 8,
                            8,
                            8,
                            pair_reach,
                            ppx,
                            ppy,
                            y.width,
                            y.height,
                            fctx,
                        );
                        if let Some((ub, vb)) = &palette_uv_bufs {
                            let pbuf = if plane == 1 { ub } else { vb };
                            set_palette_pred(palette_window(pbuf, cw, 0, unit_row * 8, 4, 8), fctx);
                        }
                        push_intra_rect(
                            y,
                            u,
                            v,
                            plane,
                            cpx,
                            cpy + unit_row * 8,
                            4,
                            8,
                            uv_predict_mode,
                            angle_delta_uv,
                            unit_reach,
                            &ZERO_RESIDUAL[..4 * 8],
                            alpha
                                .zip(ac)
                                .map(|((au, av), ac)| (if plane == 1 { au } else { av }, ac)),
                            None,
                            smooth_neighbor_uv,
                            fctx,
                        );
                    }
                } else {
                    if let Some((ub, vb)) = &palette_uv_bufs {
                        set_palette_pred(if plane == 1 { ub.clone() } else { vb.clone() }, fctx);
                    }
                    push_intra_rect(
                        y,
                        u,
                        v,
                        plane,
                        cpx,
                        cpy,
                        cw,
                        ch,
                        uv_predict_mode,
                        angle_delta_uv,
                        pair_reach,
                        &zeros,
                        alpha
                            .zip(ac)
                            .map(|((au, av), ac)| (if plane == 1 { au } else { av }, ac)),
                        None,
                        smooth_neighbor_uv,
                        fctx,
                    );
                }
            }
            (zeros.clone(), zeros)
        } else if chroma422_pair16 {
            // lane-av1-422o: the pair's 4x16 chroma as TWO TX_4X8 units
            // per plane, plane-major (every U unit, then every V). Each
            // unit: the `ChromaRect8x4` tables (`get_txsize_entropy_ctx`
            // squares TX_4X8 up to the 8x8 class), `SCAN_4X8`, and a
            // `txb_skip_ctx` of above+left in the OFFSET-10 rows -- the
            // offset comes from `num_pels_log2_lookup[plane_bsize] >
            // num_pels_log2_lookup[BLOCK_4X8]` with plane_bsize =
            // BLOCK_INVALID (255), and the byte at that table's +255 in
            // the pinned Release oracle is 255 (`cmpb` against the
            // `0xc0db70` copy decodetxb.c links), i.e. the bigger-plane
            // arm: +3 on the offset-7-first chroma tables. No tx_type
            // symbol (only y plane's is transmitted); the unit's type is
            // `intra_mode_to_tx_type(UV mode)` kept by the all-16 ext-tx
            // set -- [`read_rect_chroma_unit`]'s own default. CfL params
            // are computed ONCE per block (`are_parameters_computed`)
            // over the pair's luma, shared by both units. Each unit is
            // predicted and recorded immediately, so the lower unit
            // reads the upper one's coefficient context.
            let ac = alpha.map(|_| cfl_src_rect(ppx, ppy, pw, ph));
            for plane in [1usize, 2] {
                for unit_row in 0..2usize {
                    let unit_mi = (pair_mi.0 + unit_row * 2, pair_mi.1);
                    let unit_reach = tu_reach_rect(
                        pw,
                        ph,
                        0,
                        unit_row * 8,
                        8,
                        8,
                        pair_reach,
                        ppx,
                        ppy,
                        y.width,
                        y.height,
                        fctx,
                    );
                    if let Some((ub, vb)) = &palette_uv_bufs {
                        let pbuf = if plane == 1 { ub } else { vb };
                        set_palette_pred(palette_window(pbuf, cw, 0, unit_row * 8, 4, 8), fctx);
                    }
                    let chroma_around = neighbours.around_mi_422_chroma(unit_mi, 8, 8);
                    let unit_grid = read_rect_chroma_unit(
                        dec,
                        cdfs,
                        TxbSet::ChromaRect8x4,
                        &SCAN_4X8,
                        4,
                        8,
                        plane,
                        chroma_around[plane],
                        uv_predict_mode,
                        angle_delta_uv,
                        unit_reach,
                        y,
                        u,
                        v,
                        cpx,
                        cpy + unit_row * 8,
                        base_q_idx,
                        // lane-av1-422q: the offset-10 rows -- the same
                        // measured rule as the 4x32 arm (plane_bsize
                        // BLOCK_INVALID for the pair's BLOCK_4X16 too;
                        // num_pels_log2_lookup[255] = 0xff at the
                        // 0xc0de70 copy -> +3 on the offset-7-first
                        // chroma tables; see lanes/av1422q.report.md).
                        Some(3),
                        alpha
                            .zip(ac)
                            .map(|((au, av), ac)| (if plane == 1 { au } else { av }, ac)),
                        smooth_neighbor_uv,
                        fctx,
                    )?;
                    neighbours.record_mi_chroma(unit_mi, 8, 8, plane, &unit_grid);
                }
            }
            (Grid::Zero(cw * ch), Grid::Zero(cw * ch))
        } else {
            // lane-av1422ctxgather: 4:2:2's chroma above votes sample
            // every second luma-mi cell
            // ([`Neighbours::around_mi_422_chroma`]: at ss_x 1 one
            // chroma 4-px column spans two luma mi columns, and the
            // plain per-mi gather counts each column's whole-unit dc
            // sign twice). The left extent is unsubsampled at ss_y 0,
            // so its per-mi cells already line up one-to-one.
            let around = if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                neighbours.around_mi_422_chroma(pair_mi, pw, ph)
            } else {
                neighbours.around_mi_rect(pair_mi, pw, ph)
            };
            // lane-av1-444: the strip's own 16x4 / 4x16 chroma at ss 0/0
            // (`TxbSet::Chroma8`, `get_txsize_entropy_ctx(TX_16X4)` =
            // the 8x8 set); 4:2:0 keeps the pair's 8x4 / 4x8.
            let (chroma_set, chroma_scan): (TxbSet, &[u16]) = match (cw, ch) {
                (8, 4) => (TxbSet::ChromaRect8x4, &SCAN_8X4[..]),
                (4, 8) => (TxbSet::ChromaRect8x4, &SCAN_4X8[..]),
                (16, 4) => (TxbSet::Chroma8, &SCAN_16X4[..]),
                // lane-av1-444: 4:4:4's own-strip vert chroma is (4,16)
                // -- one TX_4X16. (lane-av1-422o: the 4:2:2 pair's
                // (4,16) never reaches this row; `chroma422_pair16`
                // intercepts it above and chunks it into TX_4X8 units.)
                (4, 16) => (TxbSet::Chroma8, &SCAN_4X16[..]),
                // lane-av1-422: a 4:2:2 HORZ pair (16x8 luma) keeps the
                // pair's height: an 8x8 SQUARE chroma block, the same
                // `TxbSet::Chroma8` set any 8x8 chroma plane reads. The
                // 4:2:0 pair's chroma is 8x4 and 4:4:4's is 16x4, so this
                // row is unreachable there. It is also the witness arm:
                // a restored `/2 chroma height` lands on (8,4) and the
                // counter reads 0.
                (8, 8) => {
                    hit!(CHROMA422_SQUARE_HITS);
                    (TxbSet::Chroma8, default_scan(8))
                }
                // lane-av1-422o: with the `chroma422_pair16` arm above,
                // the (2,16) own-strip shape and the 4:4:4 shapes are the
                // only residents; anything else is a named-lane gap.
                _ => unreachable!("decode_rect4_16_strip chroma shape set is closed"),
            };
            let mut planes = Vec::with_capacity(2);
            // Index loop mirrors the spec's per-position form over several offsets; an iterator would obscure it.
            #[allow(clippy::needless_range_loop)]
            for plane in 1..3 {
                let default_tx = default_intra_tx_type(uv_predict_mode as u8);
                let skip_ctx = usize::from(around[plane].0) + usize::from(around[plane].1);
                let mut coding = cdfs.txb(chroma_set, uv_predict_mode);
                let (levels, tx_type) = read_chroma_coeffs_rect(
                    dec,
                    &mut coding,
                    chroma_scan,
                    cw,
                    ch,
                    skip_ctx,
                    dc_sign_ctx(around[plane].2),
                    default_tx,
                    fctx,
                    plane,
                )?;
                planes.push((levels, tx_type));
            }
            let (v_levels, v_tx_type) = planes.pop().expect("plane 2");
            let (u_levels, u_tx_type) = planes.pop().expect("plane 1");
            // CfL averages the luma of the WHOLE pair, the region this one
            // chroma transform covers.
            let ac = alpha.map(|_| cfl_src_rect(ppx, ppy, pw, ph));
            for (plane, levels, tx_type) in
                [(1usize, &u_levels, u_tx_type), (2, &v_levels, v_tx_type)]
            {
                let residual = dequant_and_inverse_typed_wh(
                    levels,
                    cw,
                    ch,
                    crate::decode::bit_depth(fctx),
                    block_q_idx(fctx),
                    plane_q_delta(plane, fctx).0,
                    plane_q_delta(plane, fctx).1,
                    tx_type,
                    block_iqmatrix(fctx, plane, cw, ch, tx_type),
                );
                if let Some((ub, vb)) = &palette_uv_bufs {
                    set_palette_pred(if plane == 1 { ub.clone() } else { vb.clone() }, fctx);
                }
                push_intra_rect(
                    y,
                    u,
                    v,
                    plane,
                    cpx,
                    cpy,
                    cw,
                    ch,
                    uv_predict_mode,
                    angle_delta_uv,
                    pair_reach,
                    &residual,
                    alpha
                        .zip(ac)
                        .map(|((au, av), ac)| (if plane == 1 { au } else { av }, ac)),
                    None,
                    smooth_neighbor_uv,
                    fctx,
                );
            }
            (u_levels, v_levels)
        };
        neighbours.record_uv_mode_mi(pair_mi.0, pair_mi.1, pw / MI, ph / MI, uv_predict_mode);
        // A lossless pair already stamped each TX_4X4 (`record_mi_chroma`).
        // The single-state rewrite below is the non-lossless TX_8X4/TX_4X8.
        // lane-av1-422o: the chunked pair's cells are stamped PER UNIT in
        // the arm above (libaom's coefficient contexts are per
        // 4x4-cell); a pair-wide state would erase the unit boundary the
        // next block reads.
        if !lossless_pair && !chroma422_pair16 {
            // The chroma planes' coefficient context, over the PAIR's mi span
            // (`decode_leaf_rect8`'s own rule: a subsampled plane's bound is
            // the luma one rounded up to its 4x4 unit).
            let u_state = neighbour_state(&u_levels);
            let v_state = neighbour_state(&v_levels);
            let round_up_even = |n: usize| n.div_ceil(2) * 2;
            let bound_h = round_up_even(neighbours.mi_rows);
            let bound_w = round_up_even(neighbours.mi_cols);
            for cell in 0..(ph / MI) {
                if pair_mi.0 + cell < bound_h
                    && let Some(slot) = neighbours.left.get_mut(pair_mi.0 + cell)
                {
                    slot[1] = u_state;
                    slot[2] = v_state;
                }
            }
            for cell in 0..(pw / MI) {
                if pair_mi.1 + cell < bound_w
                    && let Some(slot) = neighbours.above.get_mut(pair_mi.1 + cell)
                {
                    slot[1] = u_state;
                    slot[2] = v_state;
                }
            }
        }
        last_uv_mode = Some(uv_predict_mode);
    }
    if horz {
        hit!(RECT4_16_HORZ_HITS);
    } else {
        hit!(RECT4_16_VERT_HITS);
    }
    let (pal_y_size, pal_y_colors) = palette_y
        .as_ref()
        .map_or((0, [0u16; 8]), |p| (p.size, p.colors));
    let (pal_uv_size, pal_uv_colors) = palette_uv
        .as_ref()
        .map_or((0, [0u16; 8]), |p| (p.size, p.u_colors));
    record_strip_palette(
        neighbours,
        lmi,
        bw,
        bh,
        pal_y_size,
        pal_y_colors,
        pal_uv_size,
        pal_uv_colors,
    );
    // `set_txfm_ctxs(mbmi->tx_size, n4_w, n4_h, skip && is_inter, xd)`
    // (libaom `decode_token_recon_block`, run for EVERY block of an inter
    // frame): the above row records `tx_size_wide` over the block's width and
    // the left column `tx_size_high` over its height. This 1:4 strip reader is
    // also the INTER frame's reader for the shape ([`decode_intra_rect4_in_inter`]),
    // and it published neither band -- so the block after a 64x16 strip read
    // the row's init value 64 as its left transform height and took
    // `tx_size_cat3` row 1 where libaom takes row 0 (lane-frame36 r1: the
    // 3840x1608 10-bit stream's first silent wall, decode-order frame 33 at
    // mi(16,480)). Same write as [`set_txfm_ctxs`], rect-aware.
    txfm_partition_update_rect(neighbours, lmi, (tx_w, tx_h), (bw, bh));
    Ok((mode, last_uv_mode, (tx_w, tx_h)))
}

/// Decodes one true `bw`x`bh` superblock-level `PARTITION_HORZ`/
/// `PARTITION_VERT` strip (lane-sbpart r2, `bw, bh` one of `(64, 32)` /
/// `(32, 64)` -- [`decode_block_rect`]'s own sibling one level up). Unlike
/// that 32x16/16x32 strip, both dimensions here exceed the 32-coefficient
/// cap, so LUMA needs the same corner truncation [`decode_block`] already
/// does for a plain 64x64 square (spec 5.11.40): `get_txsize_entropy_ctx`
/// resolves TX_64X32/TX_32X64 to TX_64X64 regardless of orientation (see
/// this function's own charter/report), so both strips read a real 32x32
/// corner through [`read_coeffs`] with [`TxbSet::Luma64`]/[`SCAN_32`] and
/// embed it top-left of a zeroed `bw`x`bh` grid, mirroring
/// [`read_plane`]'s `tx_side != side` branch generalized to non-square.
/// CHROMA (TX_32X16/TX_16X32) resolves to TX_32X32 -- a real, *untruncated*
/// `bw/2`x`bh/2` transform via [`read_coeffs_rect`] with
/// [`TxbSet::ChromaRect32x16`] and the matching [`SCAN_32X16`]/
/// [`SCAN_16X32`], same as [`decode_block_rect`]'s own chroma path. Like
/// that function, only the single-transform-unit (`tx_depth == 0`) case is
/// supported: a split transform refuses by name rather than guess-decode.
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_block_rect64(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    at: (usize, usize),
    bw: usize,
    bh: usize,
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    enable_filter_intra: bool,
    allow_screen_content_tools: bool,
    base_q_idx: u8,
    tx_select: bool,
    reduced_tx_set: bool,
    fctx: &crate::decode::FrameCtx,
) -> Result<()> {
    let (r, c) = at;
    let (px, py) = (c * SUB, r * SUB);
    let (nb_above_mode, nb_left_mode) = neighbours.modes_above_left(r, c);
    let (palette_ctx, palette_cache) = neighbours.palette_ctx_and_cache(at);
    let palette_uv_cache = neighbours.palette_uv_cache(at);
    let (
        skip,
        mode,
        angle_delta_y,
        uv_mode,
        angle_delta_uv,
        alpha,
        filter_intra,
        palette_y,
        palette_uv,
        intrabc,
    ) = read_intra_mode_rect(
        dec,
        cdfs,
        nb_above_mode,
        nb_left_mode,
        // `is_cfl_allowed` (spec 5.11.5) caps CFL at <=32x32; these
        // superblock-level HORZ/VERT strips are 64x32/32x64, so unlike
        // `decode_block_rect`'s own 32x16/16x32 strips (where `true` is
        // correct), CFL must never be offered here -- passing `true`
        // read `uv_mode` off the 14-symbol `uv_mode_cfl` CDF instead of
        // the real 13-symbol `uv_mode_no_cfl` one and desynced the tile
        // from this block's very first symbol (lane-sbpart r3 bisect).
        false,
        true,
        bw,
        bh,
        enable_filter_intra,
        neighbours.skip_txfm_ctx(r * (SUB / MI), c * (SUB / MI)),
        allow_screen_content_tools,
        Some((palette_ctx, &palette_cache)),
        &palette_uv_cache,
        r * (SUB / MI),
        c * (SUB / MI),
        fctx,
    )?;
    if let Some((dv_row, dv_col)) = intrabc {
        return decode_intrabc_owned_rect(
            dec,
            cdfs,
            neighbours,
            (r * (SUB / MI), c * (SUB / MI)),
            (bw, bh),
            skip,
            (dv_row, dv_col),
            y,
            u,
            v,
            base_q_idx,
            tx_select,
            reduced_tx_set,
            fctx,
        );
    }
    if (palette_y.is_some() || palette_uv.is_some()) && !skip {
        // lane-t900 r34: a non-skip palette block on a superblock-level strip
        // -- counted so its gate cannot go vacuous (this was a refusal until
        // r34; see the gate `a_real_aomenc_palette_stream_with_8x8_leaves_
        // decodes_pixel_exact`).
        hit!(PALETTE_SB_STRIP_HITS);
    }
    let smooth_neighbor = is_smooth_mode(nb_above_mode) || is_smooth_mode(nb_left_mode);
    if smooth_neighbor {
        hit!(SMOOTH_LUMA_HITS);
    }
    let smooth_neighbor_uv =
        neighbours.smooth_uv_neighbour(r * (SUB / MI), c * (SUB / MI), r, c, fctx);
    if filter_intra.is_some() {
        // `filter_intra_size_class_rect` already returns `None` for both
        // `(64, 32)` and `(32, 64)` (`av1_filter_intra_allowed_bsize` caps at
        // 32 on both axes), so this symbol is never read at this level --
        // kept only as the same defensive refusal [`decode_block_rect`] has.
        return Err(unsupported(
            "filter intra on a superblock-level HORZ/VERT strip (never expected -- \
             av1_filter_intra_allowed_bsize caps at 32x32)",
        ));
    }
    let uv_predict_mode = if uv_mode == UV_CFL_PRED {
        DC_PRED
    } else {
        uv_mode
    };
    let (mi_r, mi_c) = (r * (SUB / MI), c * (SUB / MI));
    // lane-av1mixloss: the lossless-segment early return (`decodeframe.c:1203`)
    // precedes `TX_MODE_SELECT`, so a lossless-segment 64-axis rect block codes
    // no depth symbol here either (class sweep of the sub8 leaf's fix).
    let depth = if tx_select && !lossless(fctx) {
        // lane-av1txbands: EVERY frame now reads the real `TXFM_CONTEXT`
        // bands (`get_tx_size_context`, libaom `pred_common.h:342`) -- an
        // intra-only frame publishes them from every block too
        // ([`Neighbours::fill_lf_grid_rect`]). Measured on the 10-bit
        // 1920x792 128-superblock stream,
        // decode-order frame 1: the 32x8 HORZ_4 strip at mi(48,176) read
        // `tx_size_cat2` row 1 where libaom reads row 2.
        let ctx = tx_size_context_txfm_rect(neighbours, (mi_r, mi_c), bw, bh, fctx);
        // lane-rectsplit r1: `bsize_to_tx_size_cat` (libaom `decodemv.c`)
        // counts `sub_tx_size_map` steps from the block's own max rect
        // transform down to TX_4X4, minus one -- BLOCK_64X32's chain
        // (TX_64X32 -> TX_32X32 -> TX_16X16 -> TX_8X8 -> TX_4X4) is four
        // steps, so this level's category is 3 (the same one a 64x64 square
        // block uses), not the 2 a 32x16 strip uses. Reading the cat-2 CDF
        // here was a wrong-table read of a real symbol.
        let d = dec.symbol(&mut cdfs.tx_size_cat3[ctx]);
        if crate::envflags::env_flag!("EC_TRACE_MODE_STEP") {
            eprintln!(
                "EC_ISTEP mi_row={mi_r} mi_col={mi_c} name=tx_depth val={d} ctx={ctx} cat=3 rng={}",
                dec.debug_state().0
            );
        }
        d
    } else {
        0
    };
    // lane-t900 r1: this strip's own `set_txfm_ctxs` when it is an intra
    // block of an INTER frame -- see [`publish_txfm_bands_if_in_inter`].
    publish_txfm_bands_if_in_inter(
        neighbours,
        (mi_r, mi_c),
        depth_to_tx_wh(bw, bh, depth, fctx),
        (bw, bh),
        fctx,
    );
    if depth != 0 || lossless(fctx) {
        hit!(TX_DEPTH_HITS);
        // lane-rectsplit r1/r4: the superblock-level strip splits its
        // transform through the very same per-unit path as its 32x32-level
        // sibling; [`depth_to_tx_wh`] names the unit, which for a 64x16/16x64
        // 1:4 strip is the 2:1 rect TX_32X16 at depth 1 (lane-rectchroma r1
        // measured the old square formula mis-tiling exactly that strip).
        let (tx_w, tx_h) = depth_to_tx_wh(bw, bh, depth, fctx);
        if bw.max(bh) / bw.min(bh) == 4 {
            let which = if depth == 1 {
                &RECT4_SPLIT_DEPTH1_HITS
            } else {
                &RECT4_SPLIT_DEPTH2_HITS
            };
            hit_do! { which.with(|c| c.set(c.get() + 1)); }
        }
        if crate::envflags::env_flag!("EC_SBPART_DUMP64") {
            eprintln!(
                "DUMP64SPLIT mi_r={mi_r} mi_c={mi_c} px={px} py={py} bw={bw} bh={bh} \
                 depth={depth} tx={tx_w}x{tx_h} mode={mode} uv={uv_predict_mode} skip={skip} \
                 angle_y={angle_delta_y}"
            );
        }
        let modes = RectStripModes {
            skip,
            mode,
            angle_delta_y,
            uv_predict_mode,
            angle_delta_uv,
            alpha,
            filter_intra,
            smooth_neighbor,
            smooth_neighbor_uv,
            palette_y: palette_y
                .as_ref()
                .map(|p| p.map.iter().map(|&i| p.colors[i as usize]).collect()),
            palette_uv: palette_uv.as_ref().map(|p| {
                (
                    p.map.iter().map(|&i| p.u_colors[i as usize]).collect(),
                    p.map.iter().map(|&i| p.v_colors[i as usize]).collect(),
                )
            }),
        };
        if palette_y.is_some() || palette_uv.is_some() {
            hit!(PALETTE_SPLIT_TX_HITS);
        }
        decode_rect_split(
            dec,
            cdfs,
            neighbours,
            (mi_r, mi_c),
            bw,
            bh,
            tx_w,
            tx_h,
            &modes,
            y,
            u,
            v,
            base_q_idx,
            reduced_tx_set,
            fctx,
        )?;
        // Same early-return palette stamp as [`decode_block_rect`]'s split arm
        // (lane-palette2 r10): this return skips the fn's own tail, so a
        // split-tx superblock strip would otherwise leave the above/left
        // palette arrays holding a dead block's colours.
        let (py_size, py_colors) = palette_y
            .as_ref()
            .map_or((0, [0u16; 8]), |p| (p.size, p.colors));
        neighbours.record_palette_y_rect((mi_r, mi_c), bw, bh, py_size, py_colors);
        let (puv_size, puv_colors) = palette_uv
            .as_ref()
            .map_or((0, [0u16; 8]), |p| (p.size, p.u_colors));
        neighbours.record_palette_uv_rect((mi_r, mi_c), bw, bh, puv_size, puv_colors);
        // lane-frame36 r2: `set_txfm_ctxs` runs for EVERY block and this early
        // return skips the fn's own tail, so a SPLIT superblock-level strip
        // published no TXFM_CONTEXT band and the next block read the row's init
        // value 64 as its left transform height. Measured on the 10-bit
        // 3840x1608 stream, decode-order frame 33: after the split 16x64 strip
        // at mi(192,140) (`tx_depth=1` -> TX_16X32) the 32x64 block at
        // mi(192,144) took `tx_size_cat3` row 2 where libaom takes row 1. The
        // bands carry the SPLIT unit's size, not the block's.
        txfm_partition_update_rect(neighbours, (mi_r, mi_c), (tx_w, tx_h), (bw, bh));
        hit!(RECT64_SPLIT_TXFM_PUBLISH_HITS);
        hit!(RECT_PARTITION_HITS);
        return Ok(());
    }
    let (tx_w, tx_h) = (bw, bh);
    let (cpx, cpy) = (px >> ss_x(fctx), py >> ss_y(fctx));
    let (chroma_w, chroma_h) = (bw >> ss_x(fctx), bh >> ss_y(fctx));
    let reach = Reach::of_rect(bw, bh, px, py, y.width, y.height, fctx);
    if let Some(ref pyv) = palette_y {
        let buf: Vec<u16> = pyv
            .map
            .iter()
            .map(|&idx| pyv.colors[idx as usize])
            .collect();
        set_palette_pred(buf, fctx);
    }
    let palette_uv_bufs: Option<(Vec<u16>, Vec<u16>)> = palette_uv.as_ref().map(|puv| {
        (
            puv.map
                .iter()
                .map(|&idx| puv.u_colors[idx as usize])
                .collect(),
            puv.map
                .iter()
                .map(|&idx| puv.v_colors[idx as usize])
                .collect(),
        )
    });
    if skip {
        push_intra_rect(
            y,
            u,
            v,
            0,
            px,
            py,
            bw,
            bh,
            mode,
            angle_delta_y,
            reach,
            &ZERO_RESIDUAL[..bw * bh],
            None,
            filter_intra,
            smooth_neighbor,
            fctx,
        );
        let ac = alpha.map(|_| cfl_src_rect(px, py, bw, bh));
        if let Some((ub, _)) = &palette_uv_bufs {
            set_palette_pred(ub.clone(), fctx);
        }
        push_intra_rect(
            y,
            u,
            v,
            1,
            cpx,
            cpy,
            chroma_w,
            chroma_h,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            &ZERO_RESIDUAL[..chroma_w * chroma_h],
            alpha.zip(ac).map(|((au, _), ac)| (au, ac)),
            None,
            smooth_neighbor_uv,
            fctx,
        );
        if let Some((_, vb)) = &palette_uv_bufs {
            set_palette_pred(vb.clone(), fctx);
        }
        push_intra_rect(
            y,
            u,
            v,
            2,
            cpx,
            cpy,
            chroma_w,
            chroma_h,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            &ZERO_RESIDUAL[..chroma_w * chroma_h],
            alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
            None,
            smooth_neighbor_uv,
            fctx,
        );
        neighbours.record_rect(
            at,
            bw,
            bh,
            mode,
            uv_predict_mode,
            &[
                &ZERO_RESIDUAL[..bw * bh],
                &ZERO_RESIDUAL[..chroma_w * chroma_h],
                &ZERO_RESIDUAL[..chroma_w * chroma_h],
            ],
        );
    } else {
        let around = neighbours.around_rect(at, bw, bh);
        // LUMA: the coded corner, embedded top-left of the true bw x bh grid
        // (see this function's own doc comment). A 64-length axis codes only
        // its low 32 coefficients, so the corner is `min(bw, 32)` x
        // `min(bh, 32)`: 32x32 under a 64x32/32x64 HORZ/VERT strip, and
        // 32x16/16x32 under a 64x16/16x64 1:4 strip (lane-tx64x16).
        let (luma_cw, luma_ch) = (bw.min(32), bh.min(32));
        let scan32 = default_scan(TX32);
        if coeff_trace_on() {
            let (rng, _) = dec.debug_state();
            eprintln!(
                "EC_COEFF plane=0 row={mi_r} col={mi_c} tx_size={luma_cw}x{luma_ch} rng={rng}"
            );
        }
        let (luma_corner, luma_tx_type) = if (luma_cw, luma_ch) == (32, 32) {
            let mut luma_coding = cdfs.txb(TxbSet::Luma64, mode);
            read_coeffs(
                dec,
                &mut luma_coding,
                scan32,
                0,
                dc_sign_ctx(around[0].2),
                TxType::DctDct,
                Some((bw, bh)),
            )?
        } else {
            // `av1_get_adjusted_tx_size` (`blockd.h:1361`) maps TX_64X16 ->
            // TX_32X16 and TX_16X64 -> TX_16X32, and `av1_scan_orders`
            // (`scan.c`) gives both the very same `default_scan_32x16` /
            // `default_scan_16x32` the 2:1 sizes use ("Half of the
            // coefficients of tx64 at higher frequencies are set to zeros. So
            // tx32's scan order is used"). `get_txsize_entropy_ctx` is
            // (txsize_sqr + txsize_sqr_up + 1) >> 1 = (TX_16X16 + TX_64X64 +
            // 1) >> 1 = TX_32X32 and `txsize_log2_minus4[TX_64X16]` is 5 (a
            // 512-position eob group) -- BOTH identical to TX_32X16, so the
            // 2:1 CDF set `LumaRect32x16` is bit-exact here and no new table
            // is owed. `av1_nz_map_ctx_offset[TX_64X16]` is literally
            // `av1_nz_map_ctx_offset_32x16` (`txb_common.c:340`), which is
            // what `base_ctx_rect` computes at (32, 16).
            let luma_scan: &[u16] = if bw == 64 { &SCAN_32X16 } else { &SCAN_16X32 };
            let mut luma_coding = cdfs.txb(TxbSet::LumaRect32x16, mode);
            read_coeffs_rect(
                dec,
                &mut luma_coding,
                luma_scan,
                luma_cw,
                luma_ch,
                0,
                dc_sign_ctx(around[0].2),
                TxType::DctDct,
                0,
            )?
        };
        if coeff_trace_on() {
            let (rng, _) = dec.debug_state();
            eprintln!("EC_COEFF_VAL plane=0 row={mi_r} col={mi_c} rng={rng}");
        }
        let mut luma_levels = vec![0i32; bw * bh];
        for row in 0..luma_ch {
            luma_levels[row * bw..][..luma_cw]
                .copy_from_slice(&luma_corner[row * luma_cw..][..luma_cw]);
        }
        {
            let cur = block_q_idx(fctx);
            if cur != i32::from(base_q_idx) {
                hit!(RECT64_QIDX_DRIFT_HITS);
            }
            if crate::envflags::env_flag!("EC_AV1_TRACE") {
                eprintln!(
                    "TRACE rect64_dequant plane=0 base_q_idx={base_q_idx} current_q_idx={cur}"
                );
            }
        }
        let luma_residual = dequant_and_inverse_typed_wh(
            &luma_levels,
            bw,
            bh,
            crate::decode::bit_depth(fctx),
            block_q_idx(fctx),
            plane_q_delta(0, fctx).0,
            plane_q_delta(0, fctx).1,
            luma_tx_type,
            block_iqmatrix(fctx, 0, bw, bh, luma_tx_type),
        );
        push_intra_rect(
            y,
            u,
            v,
            0,
            px,
            py,
            bw,
            bh,
            mode,
            angle_delta_y,
            reach,
            &luma_residual,
            None,
            filter_intra,
            smooth_neighbor,
            fctx,
        );
        if crate::envflags::env_flag!("EC_SBPART_DUMP64") {
            let leftcol: Vec<u16> = if px > 0 {
                (py..py + bh)
                    .map(|row| y.data[row * y.width + px - 1])
                    .collect()
            } else {
                vec![]
            };
            eprintln!(
                "DUMP64 mi_r={mi_r} mi_c={mi_c} px={px} py={py} bw={bw} bh={bh} mode={mode} \
                 skip={skip} eob_nonzero={} row0={:?} leftcol={:?}",
                luma_corner.iter().any(|&v| v != 0),
                &y.data[py * y.width + px..][..bw],
                leftcol,
            );
        }
        // CHROMA. At 4:2:0 the plane block is already one adjusted UV
        // transform (<=32 on each axis). At 4:4:4 a 64-axis strip's chroma
        // is larger than that, so it is tiled into 32-capped units.
        let _ac = alpha.map(|_| cfl_src_rect(px, py, bw, bh));
        let (uw, uh) = (chroma_w.min(32), chroma_h.min(32));
        let (nw, nh) = (chroma_w / uw, chroma_h / uh);
        let (chroma_set, chroma_scan): (TxbSet, &[u16]) = match (uw, uh) {
            (32, 32) => (TxbSet::Chroma32, default_scan(32)),
            (32, 16) => (TxbSet::ChromaRect32x16, &SCAN_32X16),
            (16, 32) => (TxbSet::ChromaRect32x16, &SCAN_16X32),
            (32, 8) => (TxbSet::Chroma16, &SCAN_32X8),
            (8, 32) => (TxbSet::Chroma16, &SCAN_8X32),
            _ => {
                return Err(unsupported(
                    "a 64-axis strip whose chroma unit has no coefficient table",
                ));
            }
        };
        let bigger = nw * nh > 1;
        // lane-av1444palw: the reach census for the horizontal arm of this
        // walk. `EC_PALWALK=1` prints, per rect64 block, the plane-block
        // geometry, the unit grid it was tiled into, and whether a chroma
        // palette was present -- so a fixture search can tell `nw >= 2` (the
        // arm nothing had reached) from `nh >= 2` (the arm the corpus census
        // already covered), and can see WHY a candidate block did not fire.
        // Diagnostic only; no decode behaviour depends on it.
        if crate::envflags::env_flag!("EC_PALWALK") {
            eprintln!(
                "EC_PALWALK mi={r},{c} px={px} py={py} bw={bw} bh={bh} cw={chroma_w} \
                 ch={chroma_h} uw={uw} uh={uh} nw={nw} nh={nh} ast={allow_screen_content_tools} \
                 puv={} py_pal={}",
                palette_uv.is_some() as i32,
                palette_y.is_some() as i32
            );
        }
        let mut u_levels = vec![0i32; chroma_w * chroma_h];
        let mut v_levels = vec![0i32; chroma_w * chroma_h];
        // The tiled units below stamp their own coefficient context
        // immediately, then the whole-strip `record_rect` rewrite paints
        // every chroma cell with ONE whole-strip state and the per-unit DC
        // signs and levels are lost (class `override-slot-on-one-arm`, same
        // shape as `decode_rect_split`'s chroma_tiled clobber). Collected
        // here and replayed after that record, the c8 `mu_chroma_units` way.
        let mut tiled_chroma_units: Vec<((usize, usize), usize, Grid, usize, usize)> = Vec::new();
        for plane_idx in 1..=2 {
            for cu_row in 0..nh {
                for cu_col in 0..nw {
                    let cu_around = if nw == 1 && nh == 1 {
                        around
                    } else {
                        let lsx = uw << ss_x(fctx);
                        let lsy = uh << ss_y(fctx);
                        let cu_mi = (mi_r + cu_row * (lsy / MI), mi_c + cu_col * (lsx / MI));
                        // lane-av1422llintra: same ss (1, 0) above-column
                        // double-count as 12735 -- one chroma column spans two
                        // luma mi columns, libaom reads one. Identical number
                        // at 4:2:0 and 4:4:4.
                        if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                            neighbours.around_mi_422_chroma(cu_mi, lsx, lsy)
                        } else {
                            neighbours.around_mi_rect(cu_mi, lsx, lsy)
                        }
                    };
                    let lsx = uw << ss_x(fctx);
                    let lsy = uh << ss_y(fctx);
                    let cfl = alpha.map(|(au, av)| {
                        let a = if plane_idx == 1 { au } else { av };
                        (
                            a,
                            cfl_src_rect(px + cu_col * lsx, py + cu_row * lsy, lsx, lsy),
                        )
                    });
                    let levels = if uw == uh {
                        if let Some((ub, vb)) = &palette_uv_bufs {
                            let pbuf = if plane_idx == 1 { ub } else { vb };
                            set_palette_pred(
                                palette_window(pbuf, chroma_w, cu_col * uw, cu_row * uh, uw, uh),
                                fctx,
                            );
                            // Counted only when the window is a STRICT sub-slice of the
                            // block's buffer, i.e. when the 32-cap actually tiled the
                            // block: at nw * nh == 1 the window is a re-stride copy of
                            // the whole buffer and windowing is load-bearing for
                            // nothing. The `bigger` guard is what makes a hit PROOF
                            // that the tiled arm ran, rather than proof that some
                            // palette chroma block existed.
                            if bigger {
                                hit!(CHROMA_PALETTE_WINDOW_HITS);
                            }
                        }
                        read_plane(
                            dec,
                            cdfs,
                            chroma_set,
                            chroma_scan,
                            plane_idx,
                            cu_around[plane_idx],
                            mode,
                            uv_predict_mode,
                            angle_delta_uv,
                            reach,
                            y,
                            u,
                            v,
                            cpx + cu_col * uw,
                            cpy + cu_row * uh,
                            uw,
                            uw,
                            base_q_idx,
                            cfl,
                            None,
                            bigger.then_some(3),
                            smooth_neighbor_uv,
                            fctx,
                        )?
                    } else {
                        let skip_ctx = usize::from(cu_around[plane_idx].0)
                            + usize::from(cu_around[plane_idx].1)
                            + usize::from(bigger) * 3;
                        let mut coding = cdfs.txb(chroma_set, uv_predict_mode);
                        let (levels, tx_type) = read_chroma_coeffs_rect(
                            dec,
                            &mut coding,
                            chroma_scan,
                            uw,
                            uh,
                            skip_ctx,
                            dc_sign_ctx(cu_around[plane_idx].2),
                            TxType::DctDct,
                            fctx,
                            plane_idx,
                        )?;
                        let residual = dequant_and_inverse_typed_wh(
                            &levels,
                            uw,
                            uh,
                            crate::decode::bit_depth(fctx),
                            block_q_idx(fctx),
                            plane_q_delta(plane_idx, fctx).0,
                            plane_q_delta(plane_idx, fctx).1,
                            tx_type,
                            block_iqmatrix(fctx, plane_idx, uw, uh, tx_type),
                        );
                        if let Some((ub, vb)) = &palette_uv_bufs {
                            let pbuf = if plane_idx == 1 { ub } else { vb };
                            set_palette_pred(
                                palette_window(pbuf, chroma_w, cu_col * uw, cu_row * uh, uw, uh),
                                fctx,
                            );
                            // Counted only when the window is a STRICT sub-slice of the
                            // block's buffer, i.e. when the 32-cap actually tiled the
                            // block: at nw * nh == 1 the window is a re-stride copy of
                            // the whole buffer and windowing is load-bearing for
                            // nothing. The `bigger` guard is what makes a hit PROOF
                            // that the tiled arm ran, rather than proof that some
                            // palette chroma block existed.
                            if bigger {
                                hit!(CHROMA_PALETTE_WINDOW_HITS);
                            }
                        }
                        push_intra_rect(
                            y,
                            u,
                            v,
                            plane_idx,
                            cpx + cu_col * uw,
                            cpy + cu_row * uh,
                            uw,
                            uh,
                            uv_predict_mode,
                            angle_delta_uv,
                            reach,
                            &residual,
                            cfl,
                            None,
                            smooth_neighbor_uv,
                            fctx,
                        );
                        levels
                    };
                    let dst = if plane_idx == 1 {
                        &mut u_levels
                    } else {
                        &mut v_levels
                    };
                    for row in 0..uh {
                        let d = (cu_row * uh + row) * chroma_w + cu_col * uw;
                        dst[d..d + uw].copy_from_slice(&levels[row * uw..][..uw]);
                    }
                    if bigger {
                        neighbours.record_mi_chroma(
                            (mi_r + cu_row * (lsy / MI), mi_c + cu_col * (lsx / MI)),
                            lsx,
                            lsy,
                            plane_idx,
                            &levels,
                        );
                        tiled_chroma_units.push((
                            (mi_r + cu_row * (lsy / MI), mi_c + cu_col * (lsx / MI)),
                            plane_idx,
                            levels.clone(),
                            lsx,
                            lsy,
                        ));
                    }
                }
            }
        }
        neighbours.record_rect(
            at,
            bw,
            bh,
            mode,
            uv_predict_mode,
            &[&luma_levels[..], &u_levels[..], &v_levels[..]],
        );
        // The per-unit re-stamp: `record_rect` above wrote the whole strip's
        // chroma cells from the assembled grids' one whole-strip state,
        // overwriting exactly the stamps the tiled loop just made.
        for (cu_mi, plane, unit, span_w, span_h) in &tiled_chroma_units {
            neighbours.record_mi_chroma(*cu_mi, *span_w, *span_h, *plane, unit);
        }
        hit!(RECT_COEFF_HITS);
    }
    let (py_size, py_colors) = palette_y
        .as_ref()
        .map_or((0, [0u16; 8]), |p| (p.size, p.colors));
    neighbours.record_palette_y_rect((mi_r, mi_c), bw, bh, py_size, py_colors);
    let (puv_size, puv_colors) = palette_uv
        .as_ref()
        .map_or((0, [0u16; 8]), |p| (p.size, p.u_colors));
    neighbours.record_palette_uv_rect((mi_r, mi_c), bw, bh, puv_size, puv_colors);
    neighbours.fill_skip_grid_rect((mi_r, mi_c), bw / MI, bh / MI, skip);
    neighbours.fill_lf_grid_rect(
        (mi_r, mi_c),
        bw / MI,
        bh / MI,
        tx_w as u8,
        tx_h as u8,
        0,
        fctx,
    );
    // `set_txfm_ctxs(mbmi->tx_size, n4_w, n4_h, skip && is_inter, xd)`
    // (libaom `decode_token_recon_block`, run for EVERY block of an inter
    // frame): the above row records `tx_size_wide` over the block's width and
    // the left column `tx_size_high` over its height. This 1:4 strip reader is
    // also the INTER frame's reader for the shape ([`decode_intra_rect4_in_inter`]),
    // and it published neither band -- so the block after a 64x16 strip read
    // the row's init value 64 as its left transform height and took
    // `tx_size_cat3` row 1 where libaom takes row 0 (lane-frame36 r1: the
    // 3840x1608 10-bit stream's first silent wall, decode-order frame 33 at
    // mi(16,480)). Same write as [`set_txfm_ctxs`], rect-aware.
    txfm_partition_update_rect(neighbours, (mi_r, mi_c), (tx_w, tx_h), (bw, bh));
    hit!(SB_RECT_HITS);
    match (bw, bh) {
        (64, 16) => hit!(SB_RECT4_HORZ_HITS),
        (16, 64) => hit!(SB_RECT4_VERT_HITS),
        _ => {}
    }
    if crate::envflags::env_flag!("EC_AV1_TRACE") {
        let (rng, _) = dec.debug_state();
        eprintln!("TRACE_RECT64_END mi_row={mi_r} mi_col={mi_c} bw={bw} bh={bh} rng={rng}");
    }
    Ok(())
}

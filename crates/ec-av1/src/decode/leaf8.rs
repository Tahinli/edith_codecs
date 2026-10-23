//! 8x8 leaf decoder and the 128-rect and sub-8 intra mode readers. //! av1split: moved out of `decode/mod.rs`; the items are unchanged apart from visibility. (D16)

use super::*;

/// The 128 root's HORZ/VERT arm on an INTRA frame: ONE 128x64 (HORZ) or
/// 64x128 (VERT) intra block (lane-sb128c r8; the inter twin is
/// `decode_inter_block`'s own rect path).
///
/// libaom `decode_block` -> `predict_and_reconstruct_intra_block` per
/// transform unit, with the 128-specific geometry:
///  * `max_txsize_rect_lookup[BLOCK_128X64]` is TX_64X64, so every luma unit
///    is SQUARE and there are at least two of them -- depth 0: 64x64 x2,
///    depth 1: 32x32 x8, depth 2: 16x16 x32 (`bsize_to_tx_size_cat` counts
///    the `sub_tx_size_map` chain TX_64X64->32->16->8->4, i.e. cat 3, the
///    same CDF a 64x64 square block reads).
///  * `av1_get_max_uv_txsize` = `av1_get_adjusted_tx_size(
///    max_txsize_rect_lookup[BLOCK_64X32])` caps at TX_32X32, so the block's
///    64x32 uv plane block is TWO 32x32 chroma units per plane -- one per
///    64x64 luma "mu" chunk, and `decode_token_recon_block` codes Y, U, V of
///    a chunk before moving to the next (the interleave lane-sb128b r3
///    measured at the 128x128 NONE root).
///  * `get_txb_ctx`: the uv plane block is bigger than the uv transform, so
///    chroma `txb_skip_ctx` takes the offset-10 rows (7 + 3).
///    No CfL, no filter intra (both cap at 32x32) and no palette
///    (`av1_allow_palette` caps at 64x64) exist at this size -- the mode
///    reader's own size gates already say so, and the guard below is defensive.
///
/// lane-lossless128: on a LOSSLESS frame every one of those sizes collapses to
/// TX_4X4 -- `read_tx_mode` returns `ONLY_4X4` before the frame codes a tx-size
/// symbol and `read_tx_size` returns `TX_4X4` before it looks at anything
/// (`decodeframe.c:141`, `:1183`) -- so the block is tiled into 4x4 luma units
/// and (because a lossless frame forces `TX_4X4` on CHROMA too) 4x4 chroma
/// units, plane-major inside each 64x64 mu chunk exactly as
/// `decode_token_recon_block` codes them. Reading the 64-point geometry there
/// reconstructs into a `side`-sized plane with a 16-sample residual (the panic
/// this lane closed); the geometry and the unit walks below are normalized on
/// `lossless_frame`.
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_block_128rect(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    at: (usize, usize),
    bw: usize,
    bh: usize,
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    base_q_idx: u8,
    enable_filter_intra: bool,
    allow_screen_content_tools: bool,
    tx_select: bool,
    reduced_tx_set: bool,
    fctx: &crate::decode::FrameCtx,
) -> Result<()> {
    let (r, c) = at;
    let (px, py) = (c * SUB, r * SUB);
    let (mi_r, mi_c) = (r * (SUB / MI), c * (SUB / MI));
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
        // `is_cfl_allowed` caps CFL at 32x32: `uv_mode` comes off the
        // 13-symbol no-CFL CDF here, never the 14-symbol one.
        false,
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
        // lane-av1-ibcrect: the reconstruction is ported -- an intrabc block
        // is is_inter_block, so libaom predicts the whole 128x64/64x128 strip
        // from this frame's own reconstruction and runs the INTER residual
        // walk (`decode_token_recon_block`'s else arm) over it.
        return decode_intrabc_128rect(
            dec,
            cdfs,
            neighbours,
            (mi_r, mi_c),
            (bw, bh),
            skip,
            (dv_row, dv_col),
            y,
            u,
            v,
            base_q_idx,
            fctx,
        );
    }
    if alpha.is_some() || filter_intra.is_some() || palette_y.is_some() || palette_uv.is_some() {
        return Err(unsupported(
            "CfL, filter intra or a palette on a 128-root HORZ/VERT intra block (every one of \
             their size gates caps at 64x64 or below, so none of these symbols exists there)",
        ));
    }
    let uv_predict_mode = if uv_mode == UV_CFL_PRED {
        DC_PRED
    } else {
        uv_mode
    };
    let smooth_neighbor = is_smooth_mode(nb_above_mode) || is_smooth_mode(nb_left_mode);
    if smooth_neighbor {
        hit!(SMOOTH_LUMA_HITS);
    }
    let smooth_neighbor_uv = neighbours.smooth_uv_neighbour(mi_r, mi_c, r, c, fctx);
    // spec 5.11.16 `read_tx_size`: read for every intra block, skipped or
    // not. `bsize_to_tx_size_cat(BLOCK_128X64)` is 3 (see this fn's doc).
    // lane-lossless128: a LOSSLESS frame codes no tx-size symbol at all --
    // libaom's `read_tx_mode` returns `ONLY_4X4` for a `coded_lossless` frame
    // before the symbol is even considered (`decodeframe.c:141`) and
    // `read_tx_size` returns `TX_4X4` before it looks at anything
    // (`decodeframe.c:1183`). So `tx_select` is off, every plane is walked as
    // TX_4X4 transform units, and no `tx_size_cat3` symbol may be consumed.
    let lossless_frame = lossless(fctx);
    let in_inter = fctx
        .intra_in_inter_mode
        .with(std::cell::Cell::get)
        .is_some();
    let depth = if tx_select && !lossless_frame {
        // lane-inter128intra r1: `get_tx_size_context` compares the bands
        // against `tx_size_wide/high[max_txsize_rect_lookup[bsize]]` --
        // TX_64X64 for every 128-axis block, hence the `min(64)`.
        let ctx = tx_size_context_txfm_rect(neighbours, (mi_r, mi_c), bw.min(64), bh.min(64), fctx);
        dec.symbol(&mut cdfs.tx_size_cat3[ctx])
    } else {
        0
    };
    if depth != 0 {
        hit!(TX_DEPTH_HITS);
    }
    hit_do! {
        INTRA_SB128_DEPTH_HITS.with(|c| {
            let mut h = c.get();
            h[depth.min(2)] += 1;
            c.set(h);
        });
    }
    let logical_tx = if lossless_frame { 4 } else { 64 >> depth };
    if lossless_frame {
        hit!(INTRA128_LOSSLESS_HITS);
    }
    // A 64-point transform codes nothing past its low 32 coefficients.
    let coeff_tx_side = logical_tx.min(32);
    let luma_set = if lossless_frame {
        TxbSet::Luma4
    } else {
        txbset_for(logical_tx, reduced_tx_set)
    };
    let luma_scan = default_scan(coeff_tx_side);
    // `av1_get_max_uv_txsize` caps at TX_32X32 normally; a lossless frame
    // forces every plane to TX_4X4 (its own `Chroma4` set and 4x4 scan).
    let chroma_tx = if lossless_frame { 4 } else { 32 };
    let chroma_set = if lossless_frame {
        TxbSet::Chroma4
    } else {
        TxbSet::Chroma32
    };
    let chroma_scan = default_scan(chroma_tx);
    let (cpx, cpy) = (px >> ss_x(fctx), py >> ss_y(fctx));
    let (chroma_w, chroma_h) = (bw >> ss_x(fctx), bh >> ss_y(fctx));
    let reach = Reach::of_rect(bw, bh, px, py, y.width, y.height, fctx);
    let (nch_x, nch_y) = (bw / 64, bh / 64);
    // The frame's own mi grid (`av1_calc_mi_size`, 8-px aligned): libaom's
    // `decode_token_recon_block` clips each vector's walk to
    // `max_block_wide/high`, so a chroma unit whose top-left is off the frame
    // codes nothing.
    let mi_cols = 2 * y.true_width.div_ceil(8);
    let mi_rows = 2 * y.true_height.div_ceil(8);
    let max_w_mi = (bw / MI).min(mi_cols.saturating_sub(mi_c));
    let max_h_mi = (bh / MI).min(mi_rows.saturating_sub(mi_r));
    // Non-lossless: per mu chunk, `(64 >> ss) / chroma_tx` chroma units per
    // axis -- one 32x32 at 4:2:0, two stacked on the tall axis at 4:2:2
    // (lane-av1-422bigblock: the chunk's chroma plane block is 32x64 there),
    // four at 4:4:4 (lane-av1-444sb's own finding on this same walk).
    debug_assert!(
        lossless_frame
            || (chroma_w / chroma_tx, chroma_h / chroma_tx)
                == (
                    nch_x * ((64 >> ss_x(fctx)) / chroma_tx),
                    nch_y * ((64 >> ss_y(fctx)) / chroma_tx)
                )
    );
    let tpc = 64 / logical_tx;
    let zero_tu = vec![0i32; logical_tx * logical_tx];
    let zero_cu = vec![0i32; chroma_tx * chroma_tx];
    let mut last_grids = [Grid::Zero(0), Grid::Zero(0)];
    let mut units: Vec<(usize, (usize, usize), Grid)> = Vec::new();
    // The chroma context arrays are indexed in LUMA mi; every chroma unit of
    // every chunk shares this luma span (2 chroma pixels per luma unit).
    let luma_span = chroma_tx << ss_x(fctx);
    let luma_span_h = chroma_tx << ss_y(fctx);
    for ch_row in 0..nch_y {
        for ch_col in 0..nch_x {
            for tu_row in ch_row * tpc..(ch_row + 1) * tpc {
                for tu_col in ch_col * tpc..(ch_col + 1) * tpc {
                    let tu_mi = (
                        mi_r + tu_row * (logical_tx / MI),
                        mi_c + tu_col * (logical_tx / MI),
                    );
                    let (tu_px, tu_py) = (px + tu_col * logical_tx, py + tu_row * logical_tx);
                    // libaom clips the transform loop to
                    // `max_blocks_wide/high`: a unit whose top-left sample is
                    // outside the frame is never coded.
                    if tu_px >= y.true_width || tu_py >= y.true_height {
                        continue;
                    }
                    // Edge availability of a unit INSIDE a block, with the
                    // 128 geometry -- `has_top_right` is false for a block
                    // whose largest side is > 64, which [`Reach::of_rect`]
                    // already answered for the block and [`tu_reach`] carries
                    // down to the units on its own top row.
                    let tu_r = tu_reach(
                        bw,
                        bh,
                        tu_col * logical_tx,
                        tu_row * logical_tx,
                        logical_tx,
                        reach,
                        px,
                        py,
                        y.width,
                        y.height,
                        fctx,
                    );
                    if skip {
                        push_intra(
                            y,
                            u,
                            v,
                            0,
                            tu_px,
                            tu_py,
                            logical_tx,
                            mode,
                            angle_delta_y,
                            tu_r,
                            &zero_tu,
                            None,
                            None,
                            smooth_neighbor,
                            fctx,
                        );
                        neighbours.record_mi_luma(tu_mi, logical_tx, &zero_tu);
                        continue;
                    }
                    // The unit is smaller than its block, so `txb_skip_ctx`
                    // is the neighbour-magnitude table, never the lone-TU 0.
                    let tu_skip_ctx = neighbours.luma_skip_ctx(tu_mi, logical_tx / MI);
                    let tu_around = neighbours.around_mi(tu_mi, logical_tx)[0];
                    let tu_grid = read_plane(
                        dec,
                        cdfs,
                        luma_set,
                        luma_scan,
                        0,
                        tu_around,
                        mode,
                        mode,
                        angle_delta_y,
                        tu_r,
                        y,
                        u,
                        v,
                        tu_px,
                        tu_py,
                        logical_tx,
                        coeff_tx_side,
                        base_q_idx,
                        None,
                        None,
                        Some(tu_skip_ctx),
                        smooth_neighbor,
                        fctx,
                    )?;
                    neighbours.record_mi_luma(tu_mi, logical_tx, &tu_grid);
                }
            }
            // Transform units per axis inside this mu chunk's chroma span:
            // a 64x64 luma chunk covers `64 >> ss` chroma pixels per axis,
            // so it holds `(64 >> ss) / chroma_tx` units -- one 32x32 at
            // 4:2:0, four at 4:4:4 (lane-av1-444sb), TWO STACKED TX_32X32 on
            // the tall axis at 4:2:2 (lane-av1-422bigblock: the chunk's
            // chroma plane block is 32x64 there, so the two axes' counts
            // part ways -- 1 x 2); 8x8 TX_4X4 units when lossless.
            let cn = ((64 >> ss_x(fctx)) / chroma_tx).max(1);
            let cn_h = ((64 >> ss_y(fctx)) / chroma_tx).max(1);
            // libaom's per-chunk `unit_width/height` (plane-mi units of the
            // luma mi grid): `min(mu_blocks + col, max_blocks) >> ss`.
            let unit_w = ((ch_col + 1) * 16).min(max_w_mi) >> ss_x(fctx);
            let unit_h = ((ch_row + 1) * 16).min(max_h_mi) >> ss_y(fctx);
            let stepc = (chroma_tx / 4).max(1);
            // `decode_token_recon_block` codes a mu chunk PLANE-MAJOR: every U
            // unit of the chunk, then every V unit. At one unit per plane per
            // chunk (every non-lossless 128-axis block) that is the same
            // symbol order the interleaved pair used to read; a lossless
            // frame's 4x4 chroma units are where it becomes observable.
            for plane_pass in 1..=2usize {
                for ci_row in 0..cn_h {
                    for ci_col in 0..cn {
                        // The clamp strides are the CHUNK's chroma mi extent
                        // per axis: 16 luma mi >> the axis's own ss.
                        if ch_col * (16 >> ss_x(fctx)) + ci_col * stepc >= unit_w
                            || ch_row * (16 >> ss_y(fctx)) + ci_row * stepc >= unit_h
                        {
                            continue;
                        }
                        let cu_x = cpx + ch_col * (64 >> ss_x(fctx)) + ci_col * chroma_tx;
                        let cu_y = cpy + ch_row * (64 >> ss_y(fctx)) + ci_row * chroma_tx;
                        let cu_mi = (
                            mi_r + ch_row * 16 + ci_row * ((chroma_tx << ss_y(fctx)) / 4),
                            mi_c + ch_col * 16 + ci_col * ((chroma_tx << ss_x(fctx)) / 4),
                        );
                        let cu_r = tu_reach_rect(
                            bw,
                            bh,
                            ch_col * 64 + ci_col * luma_span,
                            ch_row * 64 + ci_row * luma_span_h,
                            luma_span,
                            luma_span_h,
                            reach,
                            px,
                            py,
                            y.width,
                            y.height,
                            fctx,
                        );
                        if skip {
                            push_intra(
                                y,
                                u,
                                v,
                                plane_pass,
                                cu_x,
                                cu_y,
                                chroma_tx,
                                uv_predict_mode,
                                angle_delta_uv,
                                cu_r,
                                &zero_cu,
                                None,
                                None,
                                smooth_neighbor_uv,
                                fctx,
                            );
                            continue;
                        }
                        // lane-av1422gather3: libaom's `get_txb_ctx_general`
                        // reads the unit's above votes over `txb_w_unit` =
                        // `chroma_tx / 4` CHROMA cells, and at ss_x 1 one
                        // chroma cell spans TWO luma mi columns -- the same
                        // pair rule the mu-chunk walks take
                        // (`around_mi_422_chroma`). Reachability measured on
                        // the pinned `422_intrabc_sb128_strip*.obu` (72 unit
                        // gathers each, `sb128rect_chroma_replay_hits().0`);
                        // output-inert on all 21 committed 4:2:2 cells, so
                        // the source-scan gate
                        // `the_422_block_128rect_chroma_chunk_gather_stays_per_axis`
                        // is what stops a revert.
                        let cu_around = if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                            neighbours.around_mi_422_chroma(cu_mi, luma_span, luma_span_h)
                        } else {
                            neighbours.around_mi_rect(cu_mi, luma_span, luma_span_h)
                        };
                        let grid = read_plane(
                            dec,
                            cdfs,
                            chroma_set,
                            chroma_scan,
                            plane_pass,
                            cu_around[plane_pass],
                            mode,
                            uv_predict_mode,
                            angle_delta_uv,
                            cu_r,
                            y,
                            u,
                            v,
                            cu_x,
                            cu_y,
                            chroma_tx,
                            chroma_tx,
                            base_q_idx,
                            None,
                            None,
                            Some(3),
                            smooth_neighbor_uv,
                            fctx,
                        )?;
                        last_grids[plane_pass - 1] = grid.clone();
                        // Immediately, so the NEXT unit of this same block
                        // reads this one's coefficient context; replayed after
                        // `record_split_luma_rect` below, which would otherwise
                        // clobber them whole-block.
                        neighbours.record_mi_chroma(
                            cu_mi,
                            luma_span,
                            luma_span_h,
                            plane_pass,
                            &grid,
                        );
                        units.push((plane_pass, cu_mi, grid));
                    }
                }
            }
            if !skip {
                hit!(CHROMA_SPLIT_TX_HITS);
            }
        }
    }
    if skip {
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
        neighbours.record_split_luma_rect(
            at,
            bw,
            bh,
            mode,
            uv_predict_mode,
            [&last_grids[0], &last_grids[1]],
        );
        // lane-av1-128rectspan: the replay must re-stamp each unit over the
        // SAME cells the walk above stamped it over -- `record_mi_chroma`'s
        // height drives `left[]` and its width `above[]`, and the walk used
        // the per-axis `luma_span_h`. Passing the width on both axes doubled
        // the `left[]` run wherever the two differ (4:2:2), reaching 8 mi
        // rows below the block on the last unit of a 128x64/64x128 strip and
        // smearing a neighbour chunk's unit over its neighbour's rows.
        //
        // 4:2:2 (ss (1,0)) is the one format where the two spans differ, and
        // it is ADMITTED since lane-av1422lift: this branch does fire there
        // (`SB128RECT_REPLAY_SPAN_MISMATCH_HITS` reads 9 on each of the two
        // pinned `422_intrabc_sb128_strip*.obu` cells, whose five frames are
        // byte-exact against ffmpeg) -- that is the replay's own designed
        // 4:2:2 case, not a defect signature. Nothing here asserts the
        // mismatch is zero.
        if luma_span != luma_span_h {
            hit!(SB128RECT_REPLAY_SPAN_MISMATCH_HITS);
        }
        for (plane, cu_mi, grid) in &units {
            neighbours.record_mi_chroma(*cu_mi, luma_span, luma_span_h, *plane, grid);
            hit!(SB128RECT_CHROMA_REPLAY_HITS);
        }
    }
    // No palette exists at this size; the unconditional stamp clears any
    // stale neighbour palette state, as every other block path does.
    neighbours.record_palette_y_rect(at, bw, bh, 0, [0u16; 8]);
    neighbours.record_palette_uv_rect(at, bw, bh, 0, [0u16; 8]);
    neighbours.fill_skip_grid_rect((mi_r, mi_c), bw / MI, bh / MI, skip);
    neighbours.fill_lf_grid_rect(
        (mi_r, mi_c),
        bw / MI,
        bh / MI,
        logical_tx as u8,
        logical_tx as u8,
        0,
        fctx,
    );
    if in_inter || allow_intrabc_frame(fctx) {
        // `set_txfm_ctxs(tx_size, n4_w, n4_h, skip && is_inter, xd)`
        // (libaom `decodeframe.c` `decode_block`) runs for EVERY block of an
        // inter frame, intra ones included: an intra block stamps its
        // TRANSFORM size over its own footprint in both TXFM_CONTEXT bands.
        // A key frame reads those bands only when it allows intrabc
        // ([`read_block_tx_size_rect`]'s var-tx context), and then libaom
        // stamps here too -- lane-av1-intrabc r3.
        txfm_partition_update_rect(neighbours, (mi_r, mi_c), (logical_tx, logical_tx), (bw, bh));
    }
    if in_inter {
        hit_do! {
            INTRA128_IN_INTER_HITS.with(|c| {
                let mut h = c.get();
                h[usize::from(bw < bh)] += 1;
                c.set(h);
            });
        }
    } else if bw > bh {
        hit!(INTRA_SB128_HORZ_HITS);
    } else {
        hit!(INTRA_SB128_VERT_HITS);
    }
    Ok(())
}

/// Decodes one 8x8 leaf of a straddling 16x16 block (lane-av1-rect), mirroring
/// [`crate::tile::write_leaf8`]: its intra-mode context comes from the
/// *enclosing* 16x16 slot's `above_mode`/`left_mode` (`outer_at`, [`SUB`]-grid),
/// overridden on whichever axis `prev_leaf` sits along -- the previous leaf's
/// just-decoded mode, not the enclosing slot's stale one, is the true
/// neighbour there. `leaf_mi` is this leaf's own position in 4x4 mode-info
/// units, which is what its coefficient context (finer than [`SUB`]) is read
/// at. Returns the mode this leaf decoded, for the caller's own
/// `prev_leaf`/final mode-writeback bookkeeping.
#[allow(clippy::too_many_arguments)]
/// The coarse 16x16 `above_mode`/`left_mode` cells a four-way SPLIT of one
/// 16x16 parent leaves behind (lane-rectx r3). The next block DOWN reads
/// `above_mode[col]`, and its true above neighbour is the mi directly above
/// its own top-left corner -- the parent's BOTTOM-LEFT leaf, not the last one
/// decoded; the next block RIGHT reads `left_mode[row]`, whose true left
/// neighbour is the parent's TOP-RIGHT leaf. Writing the z-order last leaf
/// (bottom-right) into both, as this path did, is right only when all four
/// leaves happen to share a mode. `done` may be short at the true frame edge
/// (the straddle caller filters positions off the frame), so each side falls
/// back to the last leaf actually decoded.
pub(super) fn write_back_split_modes(
    neighbours: &mut Neighbours,
    done: &[((usize, usize), usize)],
    parent_mi: (usize, usize),
    slot: (usize, usize),
) {
    let (sr, sc) = slot;
    let Some(&(_, last)) = done.last() else {
        return;
    };
    let find = |mi: (usize, usize)| done.iter().find(|&&(at, _)| at == mi).map(|&(_, m)| m);
    neighbours.above_mode[sc] = find((parent_mi.0 + 2, parent_mi.1)).unwrap_or(last);
    neighbours.left_mode[sr] = find((parent_mi.0, parent_mi.1 + 2)).unwrap_or(last);
}

// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_leaf8(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    outer_at: (usize, usize),
    leaf_mi: (usize, usize),
    scans: (&[u16], &[u16]),
    prev_leaf: Option<((usize, usize), usize)>,
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    base_q_idx: u8,
    enable_filter_intra: bool,
    allow_screen_content_tools: bool,
    allow_intrabc: bool,
    tx_select: bool,
    reduced_tx_set: bool,
    fctx: &crate::decode::FrameCtx,
) -> Result<usize> {
    let (r, c) = outer_at;
    // lane-modectx r1: the group's own availability-correct fallback pair, not
    // the raw coarse bands (which leak across a tile edge -- see
    // `modes_above_left`).
    let (mut above_mode, mut left_mode) = neighbours.modes_above_left(r, c);
    if let Some(((pr, pc), pmode)) = prev_leaf {
        if pc == leaf_mi.1 && leaf_mi.0 == pr + 2 {
            above_mode = pmode;
        } else if pr == leaf_mi.0 && leaf_mi.1 == pc + 2 {
            left_mode = pmode;
        }
    }
    // The mi-exact neighbour wins over both the coarse 16x16 slot and
    // `prev_leaf`: one 16x16 slot cannot hold the two different modes its
    // two 8x8 (or four 4x4) columns leave behind, so a leaf whose above/left
    // neighbour sits in a split cell read the wrong `kf_y_mode` row.
    // lane-rectsplitx r2 swept this arm into
    // [`Neighbours::modes_above_left_mi`]: the mi-exact map ALSO has to answer
    // "no neighbour" at a tile edge, where the coarse cell holds the block
    // above/left of it inside the same 16x16.
    let (above_mode, left_mode) =
        neighbours.modes_above_left_mi(leaf_mi.0, leaf_mi.1, (above_mode, left_mode));
    let smooth_neighbor = is_smooth_mode(above_mode) || is_smooth_mode(left_mode);
    // Chroma's edge-filter type reads the CHROMA neighbour's `uv_mode`
    // (`get_intra_edge_filter_type`, reconintra.c:974), never the luma mode
    // this leaf's `smooth_neighbor` is built from -- passing `false` here
    // filtered a directional chroma block's edge at the wrong strength
    // (lane-sub8 r5: 4 pixels of one 8x8 leaf's chroma, deltas 1..3).
    // The `uv_mode` neighbours are mi-exact (`uv_mode_col`/`uv_mode_row`),
    // with the coarse [`SUB`] slot as the fallback when no block recorded
    // that exact mi cell.
    let smooth_neighbor_uv = neighbours.smooth_uv_neighbour(leaf_mi.0, leaf_mi.1, r, c, fctx);
    if smooth_neighbor {
        hit!(SMOOTH_LUMA_HITS);
    }
    // An 8x8 leaf is well within `is_cfl_allowed`'s <=32x32 bound (spec
    // 5.11.5), so it reads the CFL-allowed `uv_mode_cfl` CDF, like every other
    // `decode_block` caller at 16x16 and up.
    // lane-pal8 r1: this leaf's own palette-Y mode ctx and colour cache
    // (`av1_get_palette_mode_ctx` / `av1_get_palette_cache`, decodemv.c:567,
    // `read_palette_mode_info`), at its REAL mi position -- passing
    // `None`/`&[]` read `palette_y_mode` off CDF row 0 and refused by name the
    // moment a real 8x8 palette block fired, which is where every 8-bit
    // `--tune-content=screen --min-partition-size=8` stream stopped.
    let (palette_ctx, palette_cache) = neighbours.palette_ctx_and_cache_mi(leaf_mi);
    let palette_uv_cache = neighbours.palette_uv_cache_mi(leaf_mi);
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
    ) = read_intra_mode(
        dec,
        cdfs,
        above_mode,
        left_mode,
        true,
        8,
        enable_filter_intra,
        neighbours.skip_txfm_ctx(leaf_mi.0, leaf_mi.1),
        allow_screen_content_tools,
        allow_intrabc,
        Some((palette_ctx, &palette_cache)),
        &palette_uv_cache,
        leaf_mi.0,
        leaf_mi.1,
        fctx,
    )?;
    // lane-kf900 r7: an 8x8 leaf consumed `use_intrabc` (and its DV) through
    // `read_intra_mode` and then decoded the block as if it were ordinary
    // intra. libaom's `is_inter_block` counts an intrabc block
    // (`blockd.h:373`), so its luma `tx_type` comes off the INTER ext-tx set
    // (`av1_get_ext_tx_set_type(TX_8X8, is_inter=1, 0)` = `EXT_TX_SET_ALL16`,
    // a 16-symbol CDF) while we read the 7-symbol `TX_SET_INTRA_1` row: the
    // same value with a different interval, so the tile desynced at the very
    // next symbol (class `wrong-alphabet-same-value`). Everything below
    // mirrors `decode_block`'s own intrabc handling at 16x16 and up.
    let intrabc_dv = fctx.intrabc_dv.with(|c| c.take());
    if intrabc_dv.is_some() {
        hit!(LEAF8_INTRABC_HITS);
    }
    // `av1_visit_palette`'s reconstruction (decodeframe.c:1135): the colour
    // index map, mapped through the base colours, IS the block's prediction --
    // carried to the next `reconstruct`/`read_plane` through the same
    // [`PALETTE_PRED`] slot [`decode_block`] uses at 16x16 and up.
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
    if palette_y.is_some() || palette_uv.is_some() {
        hit!(PALETTE_LEAF8_HITS);
    }
    let uv_predict_mode = if uv_mode == UV_CFL_PRED {
        DC_PRED
    } else {
        uv_mode
    };
    neighbours.above_uv_mode[c] = uv_predict_mode;
    neighbours.left_uv_mode[r] = uv_predict_mode;
    neighbours.record_uv_mode_mi(leaf_mi.0, leaf_mi.1, 2, 2, uv_predict_mode);
    // `TxMode::Select`'s `tx_depth` at an 8x8 leaf (spec 5.11.16): the only
    // depths an 8x8 block offers are `TX8` (depth 0) and `TX4` (depth 1,
    // which splits the leaf's own 8x8 prediction into a 2x2 grid of 4x4
    // transform units, same raster-order per-TU pattern `decode_block`'s
    // multi-TU branch runs).
    // ... and an intrabc leaf reads no intra `tx_depth` symbol at all: libaom
    // dispatches `read_block_tx_size` on `is_inter_block`, which counts an
    // intrabc block, so a SKIPPED one takes the largest transform and an
    // UNSKIPPED one reads the inter var-tx tree (lane-t900 r32). At 8x8 that
    // tree is one `txfm_split` symbol: TX_8X8, or TX_4X4 over all four mi
    // cells (`read_var_tx_size`'s `sub_txs == TX_4X4` early return).
    // `parse_decode_block`'s tree condition carries
    // `!xd->lossless[mbmi->segment_id]` (see [`decode_block`]'s own copy):
    // a lossless segment's intrabc leaf reads no tree, TX_4X4 via
    // `read_tx_size`'s first line (lane-av1mixloss).
    let resolved = if tx_select && intrabc_dv.is_some() && !skip && !lossless(fctx) {
        let mut leaves = Vec::new();
        let mi_cols = 2 * y.true_width.div_ceil(8);
        let mi_rows = 2 * y.true_height.div_ceil(8);
        read_var_tx_size(
            dec,
            cdfs,
            neighbours,
            leaf_mi,
            8,
            (
                2usize.min(mi_cols.saturating_sub(leaf_mi.1)),
                2usize.min(mi_rows.saturating_sub(leaf_mi.0)),
            ),
            (8, 8),
            0,
            0,
            0,
            &mut leaves,
        );
        hit!(INTRABC_VARTX_HITS);
        leaves.first().map_or(8, |l| l.2)
    } else if tx_select && intrabc_dv.is_none() {
        read_tx_size(dec, cdfs, neighbours, leaf_mi, 8, fctx)
    } else {
        // lane-lossless: TX_4X4 on a lossless frame, no symbol
        // (libaom `read_tx_size`).
        lossless_tx(8, fctx)
    };
    let (px, py) = (leaf_mi.1 * MI, leaf_mi.0 * MI);
    let reach = Reach::of(8, px, py, y.width, y.height, fctx);
    let (cpx, cpy) = (px >> ss_x(fctx), py >> ss_y(fctx));
    // 4:4:4: this 8x8 block's chroma plane block is 8x8 (same scan as luma,
    // `scans.0`). 4:2:0 stays the 4x4 unit this leaf was written for.
    // lane-av1-422d: 4:2:2 (ss 1/0) halves only the width, so the plane block
    // is 4x8 -- ONE rect transform unit per plane (`av1_get_max_uv_txsize(
    // BLOCK_8X8)` at ss (1,0) is `max_txsize_rect_lookup[BLOCK_4X8]` ==
    // TX_4X8): one txb_skip + one scan per plane in the
    // [`TxbSet::ChromaRect8x4`] class [`decode_rect_split`] proves for the
    // shape, read through [`leaf8_chroma422_unit`] in each arm below. The
    // `(csz, cset, cscan)` selection keeps serving the square arms (444's
    // 8x8, 420's 4x4); the rect arms branch on [`chroma_422`] instead.
    let chroma_444 = ss_x(fctx) == 0 && ss_y(fctx) == 0;
    let chroma_422 = ss_x(fctx) == 1 && ss_y(fctx) == 0;
    let (csz, cset, cscan) = if chroma_444 {
        (8usize, TxbSet::Chroma8, scans.0)
    } else {
        (4usize, TxbSet::Chroma4, scans.1)
    };
    // spec 7.11.3.4 with `use_intrabc`, same as `decode_block`: predict from
    // the CURRENT frame's own pre-loop-filter reconstruction (every loop
    // filter is off whenever `allow_intrabc`, frame.rs:194/210/252/272) at
    // the full-pel block vector, with the BILINEAR kernel libaom forces.
    let intrabc_bufs = intrabc_dv.map(|(dv_row, dv_col)| {
        let mut yb = vec![0u16; 8 * 8];
        mc::predict_with_filter(
            &y.data,
            y.width,
            y.true_width,
            y.true_height,
            mv_to_q4(px, dv_col, 0),
            mv_to_q4(py, dv_row, 0),
            8,
            8,
            mc::InterpFilterKind::Bilinear,
            &mut yb,
            fctx,
        );
        // The chroma extent is THIS frame's subsampled size of the 8x8 luma
        // leaf (`get_plane_block_size`): 4x4 at 4:2:0, 4x8 at 4:2:2, 8x8 at
        // 4:4:4. The literal 4x4 this replaces hardcoded the 4:2:0 halving --
        // at 4:4:4 the 16-sample override fed this leaf's own 8x8 chroma
        // reconstruct, which indexed `prediction[16]` of a 16-sample buffer
        // (len 16, index 16 at `PlaneBuf::reconstruct`), and at 4:2:2 it
        // covered half the leaf's 4x8 chroma plane block (lane-av1-422d's
        // `if chroma_422 { (4, 8) }` arm, which the ss-derived form subsumes:
        // `8 >> ss_x` is 4 and `8 >> ss_y` is 8 there). The same buffers
        // are windowed at stride 8 by `read_intra_chroma_lossless` (the TX4
        // 4:4:4-lossless arm) and consumed whole by the TX8 and skip arms,
        // so all three heal with this one sizing.
        let (cw, ch) = (8 >> ss_x(fctx), 8 >> ss_y(fctx));
        let mut ub = vec![0u16; cw * ch];
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
            &mut ub,
            fctx,
        );
        let mut vb = vec![0u16; cw * ch];
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
            &mut vb,
            fctx,
        );
        (yb, ub, vb)
    });
    // The per-plane prediction override rides the same [`PALETTE_PRED`] slot
    // `decode_block` uses: set fresh right before each plane's own
    // `reconstruct`/`read_plane` call (the slot is taken, not copied).
    // lane-t900 r32: through `palette_y_buf`, so the 4x4 TU loop below windows
    // it per unit (an intrabc leaf whose var-tx tree split to TX_4X4 is the
    // first shape that reaches that loop with an override set).
    let palette_y_buf = match &intrabc_bufs {
        Some((yb, _, _)) => Some(yb.clone()),
        None => palette_y_buf,
    };
    // lane-av1intrapred: the chroma twin of the `palette_y_buf` wrap above --
    // `decode_block` does the same for its own `palette_uv_bufs`
    // (decode.rs's single-TU and multi-TU chroma reads). Without it, an
    // intrabc leaf whose var-tx tree split to TX_4X4 reached the TX4 arm's
    // chroma reads with the override unset: the unit predicted INTRA from
    // the UV mode (intrabc's is DC) instead of copying the frame at the DV,
    // R5 first differing pixel U(184,4), block mi (2,92).
    let palette_uv_bufs = match &intrabc_bufs {
        Some((_, ub, vb)) => Some((ub.clone(), vb.clone())),
        None => palette_uv_bufs,
    };
    let (luma_grid, u_grid, v_grid);
    if skip {
        // lane-hgkf r2 (same shape as `decode_block`'s own skip branch): an
        // 8x8 leaf whose `tx_depth` resolved to TX_4X4 predicts FOUR 4x4
        // transform units, each off the units already written left/above of
        // it inside this same leaf -- an intra block's transform size is read
        // whether or not it is skipped, and spec 7.11.2 predicts per unit.
        // A palette block's prediction is the whole-block index map, never
        // per-transform-unit edges ([`decode_block`]'s own `split_tx_skip`
        // rule) -- so a palette leaf takes the single 8x8 reconstruct below
        // even when `tx_depth` resolved to TX_4X4.
        if resolved == 4 && palette_y_buf.is_none() {
            hit!(SKIP_SPLIT_TX_HITS);
            for tu_row in 0..2 {
                for tu_col in 0..2 {
                    let (tu_px, tu_py) = (px + tu_col * 4, py + tu_row * 4);
                    if tu_px >= y.true_width || tu_py >= y.true_height {
                        continue;
                    }
                    let tu_reach = tu_reach(
                        8,
                        8,
                        tu_col * 4,
                        tu_row * 4,
                        4,
                        reach,
                        px,
                        py,
                        y.width,
                        y.height,
                        fctx,
                    );
                    push_intra(
                        y,
                        u,
                        v,
                        0,
                        tu_px,
                        tu_py,
                        4,
                        mode,
                        angle_delta_y,
                        tu_reach,
                        &ZERO_RESIDUAL[..16],
                        None,
                        filter_intra,
                        smooth_neighbor,
                        fctx,
                    );
                }
            }
        } else {
            if let Some(buf) = &palette_y_buf {
                set_palette_pred(buf.clone(), fctx);
            }
            push_intra(
                y,
                u,
                v,
                0,
                px,
                py,
                8,
                mode,
                angle_delta_y,
                reach,
                &ZERO_RESIDUAL[..64],
                None,
                filter_intra,
                smooth_neighbor,
                fctx,
            );
        }
        let ac = alpha.map(|_| cfl_src(px, py, 8));
        // Three skip-arm shapes, dispatched on THIS frame's subsampling.
        // lane-av1-422d: at 4:2:2 a skipped leaf still predicts its 4x8 rect
        // unit per plane (CfL keeps its contribution, [`decode_block`]'s own
        // skip-arm rule); no coefficient symbol exists, so the entropy
        // context never enters the rect read.
        // lane-av1422llintra: NOT in a LOSSLESS frame -- `av1_get_tx_size`'s
        // first line returns TX_4X4 on every plane there, so the 4x8 plane
        // block is two 4x4 units and the `else` arm's per-unit walk is the
        // right one.
        if chroma_422 && !lossless(fctx) {
            u_grid = leaf8_chroma422_unit(
                dec,
                cdfs,
                (false, false, 0),
                uv_predict_mode,
                angle_delta_uv,
                reach,
                1,
                alpha.zip(ac).map(|((au, _), ac)| (au, ac)),
                true,
                intrabc_bufs
                    .as_ref()
                    .map(|(_, ub, _)| ub.clone())
                    .or_else(|| palette_uv_bufs.as_ref().map(|(ub, _)| ub.clone())),
                y,
                u,
                v,
                cpx,
                cpy,
                smooth_neighbor_uv,
                fctx,
            )?;
            v_grid = leaf8_chroma422_unit(
                dec,
                cdfs,
                (false, false, 0),
                uv_predict_mode,
                angle_delta_uv,
                reach,
                2,
                alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
                true,
                intrabc_bufs
                    .as_ref()
                    .map(|(_, _, vb)| vb.clone())
                    .or_else(|| palette_uv_bufs.as_ref().map(|(_, vb)| vb.clone())),
                y,
                u,
                v,
                cpx,
                cpy,
                smooth_neighbor_uv,
                fctx,
            )?;
            luma_grid = Grid::Zero(64);
        } else {
            // 4:4:4 AND 4:2:2 lossless: the leaf's chroma plane block is a
            // raster of TX_4X4 units (`av1_get_tx_size` returns TX_4X4 for
            // EVERY plane of a lossless block, so `av1_get_max_uv_txsize` --
            // the TX_4X8 the 4:2:2 arm above reads -- is never reached), so a
            // skipped leaf predicts per unit -- each from its own edges -- like
            // the luma `SKIP_SPLIT_TX` loop above. At 4:2:2 that plane block is
            // 4x8 (`av1_ss_size_lookup[BLOCK_8X8][1][0]`), so it is one 4x4
            // unit per chroma row pair. CfL never fires here (`is_cfl_allowed`
            // at lossless needs plane_bsize == BLOCK_4X4; this plane block is
            // 8x8 at 4:4:4 and 4x8 at 4:2:2, neither of which is BLOCK_4X4),
            // and intrabc/palette keep the whole-plane-block prediction they
            // already run.
            let chroma_units_ll = (chroma_444 || chroma_422)
                && lossless(fctx)
                && intrabc_bufs.is_none()
                && palette_uv_bufs.is_none();
            let cu_shape: (usize, usize) = if chroma_422 { (4, 8) } else { (8, 8) };
            if let Some((_, ub, _)) = &intrabc_bufs {
                set_palette_pred(ub.clone(), fctx);
            } else if let Some((ub, _)) = &palette_uv_bufs {
                set_palette_pred(ub.clone(), fctx);
            }
            if chroma_units_ll {
                for cu_row in 0..cu_shape.1 / 4 {
                    for cu_col in 0..cu_shape.0 / 4 {
                        let cu_reach = tu_reach(
                            8,
                            8,
                            cu_col * 4,
                            cu_row * 4,
                            4,
                            reach,
                            px,
                            py,
                            y.width,
                            y.height,
                            fctx,
                        );
                        push_intra(
                            y,
                            u,
                            v,
                            1,
                            cpx + cu_col * 4,
                            cpy + cu_row * 4,
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
                    }
                }
            } else {
                // lane-av1422mixll10: with a prediction override (intrabc's
                // frame copy at the DV, or a palette colour map), a skipped
                // leaf predicts its WHOLE chroma plane block, not one csz x
                // csz corner of it. At 4:2:2 that plane block is 4x8 (csz
                // stays 4), so the single 4x4 push below left rows
                // `cpy+4..cpy+8` unwritten -- sentinel-poisoned at frame 0
                // (cell5 U(140..143,188..191) read 0 against the oracle's
                // 664) because an intrabc+lossless leaf takes exactly this
                // arm (`chroma_units_ll` requires `intrabc_bufs.is_none()`).
                // Window the override per 4x4 unit off the cw-stride buffer,
                // the same slice `read_intra_chroma_lossless` feeds
                // `set_palette_pred` with.
                let (cw, ch) = (8 >> ss_x(fctx), 8 >> ss_y(fctx));
                let has_override = intrabc_bufs.is_some() || palette_uv_bufs.is_some();
                if chroma_422 && has_override && ch == 8 {
                    let ub = intrabc_bufs
                        .as_ref()
                        .map(|(_, ub, _)| ub.clone())
                        .or_else(|| palette_uv_bufs.as_ref().map(|(ub, _)| ub.clone()));
                    let vb = intrabc_bufs
                        .as_ref()
                        .map(|(_, _, vb)| vb.clone())
                        .or_else(|| palette_uv_bufs.as_ref().map(|(_, vb)| vb.clone()));
                    for (buf, plane) in [(ub, 1usize), (vb, 2usize)] {
                        if let Some(buf) = buf {
                            for cu_row in 0..2 {
                                let cu_reach = tu_reach(
                                    8,
                                    8,
                                    0,
                                    cu_row * 4,
                                    4,
                                    reach,
                                    px,
                                    py,
                                    y.width,
                                    y.height,
                                    fctx,
                                );
                                set_palette_pred(
                                    palette_window(&buf, cw, 0, cu_row * 4, 4, 4),
                                    fctx,
                                );
                                push_intra(
                                    y,
                                    u,
                                    v,
                                    plane,
                                    cpx,
                                    cpy + cu_row * 4,
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
                            }
                        }
                    }
                } else {
                    push_intra(
                        y,
                        u,
                        v,
                        1,
                        cpx,
                        cpy,
                        csz,
                        uv_predict_mode,
                        angle_delta_uv,
                        reach,
                        &ZERO_RESIDUAL[..csz * csz],
                        alpha.zip(ac).map(|((au, _), ac)| (au, ac)),
                        None,
                        smooth_neighbor_uv,
                        fctx,
                    );
                }
            }
            if let Some((_, _, vb)) = &intrabc_bufs {
                set_palette_pred(vb.clone(), fctx);
            } else if let Some((_, vb)) = &palette_uv_bufs {
                set_palette_pred(vb.clone(), fctx);
            }
            if chroma_units_ll {
                for cu_row in 0..cu_shape.1 / 4 {
                    for cu_col in 0..cu_shape.0 / 4 {
                        let cu_reach = tu_reach(
                            8,
                            8,
                            cu_col * 4,
                            cu_row * 4,
                            4,
                            reach,
                            px,
                            py,
                            y.width,
                            y.height,
                            fctx,
                        );
                        push_intra(
                            y,
                            u,
                            v,
                            2,
                            cpx + cu_col * 4,
                            cpy + cu_row * 4,
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
                    }
                }
            } else {
                // lane-av1422mixll10: same whole-plane-block rule as the U
                // plane above -- with an override, both 4x4 units of the 4x8
                // plane block predict, not just the first.
                let (cw, ch) = (8 >> ss_x(fctx), 8 >> ss_y(fctx));
                let has_override = intrabc_bufs.is_some() || palette_uv_bufs.is_some();
                if chroma_422 && has_override && ch == 8 {
                    let vb = intrabc_bufs
                        .as_ref()
                        .map(|(_, _, vb)| vb.clone())
                        .or_else(|| palette_uv_bufs.as_ref().map(|(_, vb)| vb.clone()));
                    if let Some(vb) = vb {
                        for cu_row in 0..2 {
                            let cu_reach = tu_reach(
                                8,
                                8,
                                0,
                                cu_row * 4,
                                4,
                                reach,
                                px,
                                py,
                                y.width,
                                y.height,
                                fctx,
                            );
                            set_palette_pred(palette_window(&vb, cw, 0, cu_row * 4, 4, 4), fctx);
                            push_intra(
                                y,
                                u,
                                v,
                                2,
                                cpx,
                                cpy + cu_row * 4,
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
                        }
                    }
                } else {
                    push_intra(
                        y,
                        u,
                        v,
                        2,
                        cpx,
                        cpy,
                        csz,
                        uv_predict_mode,
                        angle_delta_uv,
                        reach,
                        &ZERO_RESIDUAL[..csz * csz],
                        alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
                        None,
                        smooth_neighbor_uv,
                        fctx,
                    );
                }
            }
            luma_grid = Grid::Zero(64);
            u_grid = Grid::Zero(csz * csz);
            v_grid = Grid::Zero(csz * csz);
        }
    } else if resolved != 4 {
        let around = neighbours.around_mi(leaf_mi, 8);
        // lane-av1-422h: the 4:2:2 chroma rect unit's `get_txb_ctx_general`
        // reads its above votes over CHROMA 4-px cells -- at ss_x 1 one chroma
        // cell spans TWO luma mi columns, so the per-luma-mi sum counts each
        // above chroma column twice and `dc_sign_ctx`'s signum flips whenever
        // the doubled sum crosses zero (the same class
        // [`Neighbours::around_mi_422_chroma`] fixes for the rect readers;
        // measured at mi(2,24) plane 2: ours dc_sign row 0, oracle row 2).
        // The left extent is unsubsquared at ss_y 0 and stays one-to-one.
        let chroma_around = if chroma_422 {
            neighbours.around_mi_422_chroma(leaf_mi, 8, 8)
        } else {
            around
        };
        if let Some(buf) = &palette_y_buf {
            set_palette_pred(buf.clone(), fctx);
        }
        luma_grid = read_plane(
            dec,
            cdfs,
            if intrabc_dv.is_some() {
                inter_txbset_for(8, reduced_tx_set)
            } else if reduced_tx_set {
                TxbSet::Luma8
            } else {
                TxbSet::Luma8Set1
            },
            scans.0,
            0,
            around[0],
            mode,
            mode,
            angle_delta_y,
            reach,
            y,
            u,
            v,
            px,
            py,
            8,
            TX8,
            base_q_idx,
            None,
            filter_intra,
            None,
            smooth_neighbor,
            fctx,
        )?;
        // `av1_get_tx_type` branches `PLANE_TYPE_UV` on `is_inter_block`, so
        // an intrabc block's chroma tx_type is the colocated LUMA transform's
        // coded type (lane-kf900 r6's own fix, `decode_block`'s line).
        if intrabc_dv.is_some() {
            fctx.intrabc_chroma_tx
                .with(|c| c.set(Some(fctx.luma_tx_type.with(std::cell::Cell::get))));
        }
        let ac = alpha.map(|_| cfl_src(px, py, 8));
        if chroma_422 {
            // lane-av1-422d: ONE 4x8 rect unit per plane (`av1_get_max_uv_txsize`
            // at ss (1,0) == TX_4X8), covering the plane block exactly, off the
            // leaf's own 8x8-span entropy context (`chroma_tiled`'s convention).
            u_grid = leaf8_chroma422_unit(
                dec,
                cdfs,
                chroma_around[1],
                uv_predict_mode,
                angle_delta_uv,
                reach,
                1,
                alpha.zip(ac).map(|((au, _), ac)| (au, ac)),
                false,
                intrabc_bufs
                    .as_ref()
                    .map(|(_, ub, _)| ub.clone())
                    .or_else(|| palette_uv_bufs.as_ref().map(|(ub, _)| ub.clone())),
                y,
                u,
                v,
                cpx,
                cpy,
                smooth_neighbor_uv,
                fctx,
            )?;
            v_grid = leaf8_chroma422_unit(
                dec,
                cdfs,
                chroma_around[2],
                uv_predict_mode,
                angle_delta_uv,
                reach,
                2,
                alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
                false,
                intrabc_bufs
                    .as_ref()
                    .map(|(_, _, vb)| vb.clone())
                    .or_else(|| palette_uv_bufs.as_ref().map(|(_, vb)| vb.clone())),
                y,
                u,
                v,
                cpx,
                cpy,
                smooth_neighbor_uv,
                fctx,
            )?;
        } else {
            if let Some((_, ub, _)) = &intrabc_bufs {
                set_palette_pred(ub.clone(), fctx);
            } else if let Some((ub, _)) = &palette_uv_bufs {
                set_palette_pred(ub.clone(), fctx);
            }
            u_grid = read_plane(
                dec,
                cdfs,
                cset,
                cscan,
                1,
                around[1],
                mode,
                uv_predict_mode,
                angle_delta_uv,
                reach,
                y,
                u,
                v,
                cpx,
                cpy,
                csz,
                csz,
                base_q_idx,
                alpha.zip(ac).map(|((au, _), ac)| (au, ac)),
                None,
                None,
                smooth_neighbor_uv,
                fctx,
            )?;
            if let Some((_, _, vb)) = &intrabc_bufs {
                set_palette_pred(vb.clone(), fctx);
            } else if let Some((_, vb)) = &palette_uv_bufs {
                set_palette_pred(vb.clone(), fctx);
            }
            v_grid = read_plane(
                dec,
                cdfs,
                cset,
                cscan,
                2,
                around[2],
                mode,
                uv_predict_mode,
                angle_delta_uv,
                reach,
                y,
                u,
                v,
                cpx,
                cpy,
                csz,
                csz,
                base_q_idx,
                alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
                None,
                None,
                smooth_neighbor_uv,
                fctx,
            )?;
        }
        // Disarm: `INTRABC_CHROMA_TX` is `Some` across exactly this block's
        // two chroma reads (`decode_block`'s own tail does the same) -- left
        // set, every later chroma block would inherit this luma tx_type,
        // which picks the scan and the inverse transform.
        fctx.intrabc_chroma_tx.with(|c| c.set(None));
    } else {
        // `tx_depth` resolved this leaf's luma to `TX4`: a 2x2 raster-order
        // grid of 4x4 transform units, each predicted from the leaf's own
        // 8x8 `mode` and reading its own fresh context off the earlier
        // units in this same leaf (the same per-TU pattern `decode_block`'s
        // multi-TU branch runs at bigger blocks).
        let mut first_leaf_tx: Option<TxType> = None;
        for tu_row in 0..2 {
            for tu_col in 0..2 {
                let tu_mi = (leaf_mi.0 + tu_row, leaf_mi.1 + tu_col);
                let tu_px = px + tu_col * 4;
                let tu_py = py + tu_row * 4;
                let tu_around = neighbours.around_mi(tu_mi, 4)[0];
                if crate::encode::Reach::in_vert_ab() {
                    hit!(VERT_AB_TX4_HITS);
                }
                let tu_reach = tu_reach(
                    8,
                    8,
                    tu_col * 4,
                    tu_row * 4,
                    4,
                    reach,
                    px,
                    py,
                    y.width,
                    y.height,
                    fctx,
                );
                let tu_skip_ctx = neighbours.luma_skip_ctx(tu_mi, 1);
                // Each 4x4 unit predicts from its own window of the block's
                // 8x8 colour-index map ([`decode_block`]'s per-TU slice at
                // 16x16 and up) -- the whole-block buffer would be taken by
                // the FIRST unit and indexed at the wrong stride.
                if let Some(buf) = &palette_y_buf {
                    hit!(PALETTE_SPLIT_TX_HITS);
                    let mut window = vec![0u16; 16];
                    for row in 0..4 {
                        let src = (tu_row * 4 + row) * 8 + tu_col * 4;
                        window[row * 4..][..4].copy_from_slice(&buf[src..][..4]);
                    }
                    set_palette_pred(window, fctx);
                }
                let tu_grid = read_plane(
                    dec,
                    cdfs,
                    if intrabc_dv.is_some() {
                        inter_txbset_for(4, reduced_tx_set)
                    } else if reduced_tx_set {
                        TxbSet::Luma4
                    } else {
                        TxbSet::Luma4Set1
                    },
                    scans.1,
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
                    4,
                    4,
                    base_q_idx,
                    None,
                    filter_intra,
                    Some(tu_skip_ctx),
                    smooth_neighbor,
                    fctx,
                )?;
                if crate::envflags::env_flag!("EC_AV1_TRACE") {
                    eprintln!(
                        "TRACE tu_bitpos mi=({},{}) tell={}",
                        tu_mi.0,
                        tu_mi.1,
                        dec.debug_bitpos()
                    );
                }
                neighbours.record_mi_luma(tu_mi, 4, &tu_grid);
                // The FIRST (top-left, co-located with this group's chroma)
                // TU's coded tx_type is what the group's chroma inherits --
                // the same capture [`decode_block`]'s multi-TU branch makes
                // for `first_leaf_tx` (libaom `av1_get_tx_type`, blockd.h:
                // an inter-classified block's `PLANE_TYPE_UV` reads
                // `tx_type_map`, and intrabc is inter-classified).
                if first_leaf_tx.is_none() {
                    first_leaf_tx = Some(fctx.luma_tx_type.with(std::cell::Cell::get));
                }
            }
        }
        // lane-av1intrapred: an intrabc leaf whose var-tx tree split to
        // TX_4X4 never armed [`INTRABC_CHROMA_TX`], so its chroma reads fell
        // back to `default_intra_tx_type(DC_PRED)` = `DCT_DCT` and took the
        // 2D scan, the 2D eob row and the 2D nz contexts where libaom takes
        // the inherited luma type's (R5's first fork: mi (36,66), luma
        // (0,0) coded `H_ADST` off the 16-symbol inter set, chroma U then
        // decoded its `eob` from the wrong CDF row). Same arm the
        // `resolved != 4` branch above runs for its whole-leaf unit.
        if intrabc_dv.is_some()
            && let Some(t) = first_leaf_tx
        {
            fctx.intrabc_chroma_tx.with(|c| c.set(Some(t)));
        }
        let ac = alpha.map(|_| cfl_src(px, py, 8));
        // lane-av1-422h: same ss_x-1 dc-vote sampling as the whole-leaf arm
        // above -- the doubled above sum crosses zero at mi(2,24) plane 2.
        let chroma_around = if chroma_422 {
            neighbours.around_mi_422_chroma(leaf_mi, 8, 8)
        } else {
            neighbours.around_mi(leaf_mi, 8)
        };
        // lane-av1intrapred: the non-vacuity counter -- armed slot consulted
        // by this arm's chroma reads means the frame-copy + inherited-type
        // route ran; zero at gate time means the fixture stopped reaching it.
        if fctx.intrabc_chroma_tx.with(std::cell::Cell::get).is_some() {
            hit!(INTRABC_TX4_CHROMA_COPY_HITS);
        }
        if chroma_422 && !lossless(fctx) {
            // lane-av1-422d: the chroma unit does NOT follow the luma var-tx
            // split -- `uv_txsize` is `av1_get_max_uv_txsize` of the block,
            // still the one 4x8 rect unit per plane at ss (1,0).
            u_grid = leaf8_chroma422_unit(
                dec,
                cdfs,
                chroma_around[1],
                uv_predict_mode,
                angle_delta_uv,
                reach,
                1,
                alpha.zip(ac).map(|((au, _), ac)| (au, ac)),
                false,
                intrabc_bufs
                    .as_ref()
                    .map(|(_, ub, _)| ub.clone())
                    .or_else(|| palette_uv_bufs.as_ref().map(|(ub, _)| ub.clone())),
                y,
                u,
                v,
                cpx,
                cpy,
                smooth_neighbor_uv,
                fctx,
            )?;
            v_grid = leaf8_chroma422_unit(
                dec,
                cdfs,
                chroma_around[2],
                uv_predict_mode,
                angle_delta_uv,
                reach,
                2,
                alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
                false,
                intrabc_bufs
                    .as_ref()
                    .map(|(_, _, vb)| vb.clone())
                    .or_else(|| palette_uv_bufs.as_ref().map(|(_, vb)| vb.clone())),
                y,
                u,
                v,
                cpx,
                cpy,
                smooth_neighbor_uv,
                fctx,
            )?;
        } else if (chroma_444 || chroma_422) && lossless(fctx) {
            // lane-av1422llintra: a LOSSLESS 4:2:2 leaf is the 4:2:2 twin of
            // the 4:4:4 arm below. libaom's `av1_get_tx_size` (`blockd.h:1381`)
            // returns TX_4X4 for EVERY plane of a lossless block, and its
            // FIRST line is the lossless test -- so `av1_get_max_uv_txsize`
            // (the `Tx_4X8` the `chroma_422` arm above reads) is never reached
            // in a lossless frame. At ss (1,0) the leaf's chroma PLANE BLOCK is
            // 4x8 (`av1_ss_size_lookup[BLOCK_8X8][1][0]`), so it is a
            // `(4, 8)` region walked as two 4x4 units per plane, PLANE-MAJOR
            // -- the same walk `read_intra_chroma_lossless` runs for whole
            // blocks, at this leaf's own 4x8 plane block.
            //
            // MEASURED: the pinned 4:2:2 all-intra LOSSLESS control
            // `ll422_allintra.obu` (43 255 B, sha256 below) against the
            // instrumented `aomdec`: its block at mi(0,2) is `EC_IMODE_VAL
            // mi_row=0 mi_col=2 bsize=3` (BLOCK_8X8) and the oracle's
            // `EC_COEFF` units are `plane=1 row=0`, `plane=1 row=1`,
            // `plane=2 row=0`, `plane=2 row=1` -- four 4x4 units, two per
            // plane. This arm read ONE `TxbSet::ChromaRect8x4` 4x8 unit per
            // plane instead (our `EC_PRED` trace:
            // `x=4 y=0 plane=1 bw=4 bh=8 mode=1`, the first line that
            // disagrees with the oracle's `EC_PREDOUT8
            // plane=1 row_off=0 txw=4 txh=4` + `row_off=1 txh=4` pair), so
            // the tile desynced at that block's second chroma symbol and
            // every later pixel decoded from the wrong place.
            //
            // CfL never rides here: `is_cfl_allowed` (`cfl.h:24`) allows it in
            // a lossless block only when the chroma PLANE BLOCK is
            // BLOCK_4X4, and a 4:2:2 8x8 leaf's is BLOCK_4X8 -- so
            // `read_intra_chroma_lossless`'s no-CfL `read_plane` is the
            // correct arm for this shape too.
            let (blk_w, blk_h) = if chroma_422 {
                (4usize, 8usize)
            } else {
                (8usize, 8usize)
            };
            let (mut uo, mut vo) = (Vec::new(), Vec::new());
            read_intra_chroma_lossless(
                y,
                u,
                v,
                dec,
                cdfs,
                neighbours,
                leaf_mi,
                (px, py),
                (cpx, cpy),
                8,
                (blk_w, blk_h),
                (0, 0),
                (blk_w, blk_h),
                blk_w,
                reach,
                (y.width, y.height),
                mode,
                uv_predict_mode,
                angle_delta_uv,
                palette_uv_bufs.as_ref().map(|(a, b)| (&a[..], &b[..])),
                smooth_neighbor_uv,
                base_q_idx,
                &mut uo,
                &mut vo,
                fctx,
            )?;
            u_grid = if uo.is_empty() {
                Grid::Zero(blk_w * blk_h)
            } else {
                Grid::Own(uo)
            };
            v_grid = if vo.is_empty() {
                Grid::Zero(blk_w * blk_h)
            } else {
                Grid::Own(vo)
            };
            hit!(CHROMA422_LOSSLESS_4X4_HITS);
        } else if chroma_444 && lossless(fctx) {
            // 4:4:4 lossless: the leaf's chroma plane block is a 2x2 raster
            // of TX_4X4 units per plane, PLANE-MAJOR (`av1_get_tx_size`:
            // lossless forces TX_4X4 on EVERY plane) -- the same walk
            // `read_intra_chroma_lossless` runs for whole blocks. Reading one
            // TX_8X8 unit here panicked before: the lossless inverse is the
            // 4x4-only WHT (`TxParams::run`'s own `(w, h) == (4, 4)`
            // invariant), and the unit count desyncs the tile either way.
            // Per-unit `record_mi_chroma` calls stamp the neighbour bands
            // inside, which is why the tail's whole-leaf chroma stamp below
            // is skipped for this shape.
            let (mut uo, mut vo) = (Vec::new(), Vec::new());
            read_intra_chroma_lossless(
                y,
                u,
                v,
                dec,
                cdfs,
                neighbours,
                leaf_mi,
                (px, py),
                (cpx, cpy),
                8,
                (8, 8),
                (0, 0),
                (8, 8),
                8,
                reach,
                (y.width, y.height),
                mode,
                uv_predict_mode,
                angle_delta_uv,
                palette_uv_bufs.as_ref().map(|(a, b)| (&a[..], &b[..])),
                smooth_neighbor_uv,
                base_q_idx,
                &mut uo,
                &mut vo,
                fctx,
            )?;
            u_grid = if uo.is_empty() {
                Grid::Zero(64)
            } else {
                Grid::Own(uo)
            };
            v_grid = if vo.is_empty() {
                Grid::Zero(64)
            } else {
                Grid::Own(vo)
            };
        } else {
            if let Some((ub, _)) = &palette_uv_bufs {
                set_palette_pred(ub.clone(), fctx);
            }
            u_grid = read_plane(
                dec,
                cdfs,
                cset,
                cscan,
                1,
                chroma_around[1],
                mode,
                uv_predict_mode,
                angle_delta_uv,
                reach,
                y,
                u,
                v,
                cpx,
                cpy,
                csz,
                csz,
                base_q_idx,
                alpha.zip(ac).map(|((au, _), ac)| (au, ac)),
                None,
                None,
                smooth_neighbor_uv,
                fctx,
            )?;
            if let Some((_, vb)) = &palette_uv_bufs {
                set_palette_pred(vb.clone(), fctx);
            }
            v_grid = read_plane(
                dec,
                cdfs,
                cset,
                cscan,
                2,
                chroma_around[2],
                mode,
                uv_predict_mode,
                angle_delta_uv,
                reach,
                y,
                u,
                v,
                cpx,
                cpy,
                csz,
                csz,
                base_q_idx,
                alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
                None,
                None,
                smooth_neighbor_uv,
                fctx,
            )?;
        }
        // The two chroma reads above are the only consulters of this leaf's
        // armed [`INTRABC_CHROMA_TX`]; leaving it set would make the NEXT
        // leaf's chroma inherit this leaf's luma type instead of its own
        // derivation (class armed-slot-leak-into-next-leaf, seen live: the
        // regular-intra leaf after an intrabc TX4 leaf took the intrabc
        // luma's `V_ADST` for its chroma eob row).
        if intrabc_dv.is_some() {
            fctx.intrabc_chroma_tx.with(|c| c.set(None));
        }
        // `record_split_luma` expects a `record`-style `(r, c)` position in
        // `SUB`-grid units and rescales it by `SUB / MI`; `leaf_mi` is
        // already in mi units, so calling it here double-scaled the
        // position. Plane 0 is already correct per-TU from the
        // `record_mi_luma` calls above; this inlines `record_mi`'s tail
        // (spec `get_txb_ctx_general`'s neighbour-magnitude bookkeeping) for
        // the two chroma planes only, at `leaf_mi` directly, side_mi=2 (this
        // leaf's own 8x8 side over `MI`).
        let side_mi = 2;
        for cell in 0..side_mi {
            neighbours.left_side_mi[leaf_mi.0 + cell] = 8;
            neighbours.above_side_mi[leaf_mi.1 + cell] = 8;
        }
        let round_up_even = |n: usize| n.div_ceil(2) * 2;
        let bound_h = round_up_even(neighbours.mi_rows);
        let bound_w = round_up_even(neighbours.mi_cols);
        let u_state = neighbour_state(&u_grid);
        let v_state = neighbour_state(&v_grid);
        // The 4:4:4 AND 4:2:2 lossless walks above already stamped these
        // bands PER UNIT (their raster-order last writer per cell is exactly
        // libaom's per-entry last update); this whole-leaf aggregate would
        // clobber distinct unit states with one, so it runs only for the
        // shapes whose chroma really is one unit.
        if !((chroma_444 || chroma_422) && lossless(fctx)) {
            for cell in 0..side_mi {
                if cell < side_mi.min(bound_h.saturating_sub(leaf_mi.0)) {
                    neighbours.left[leaf_mi.0 + cell][1] = u_state;
                    neighbours.left[leaf_mi.0 + cell][2] = v_state;
                }
                if cell < side_mi.min(bound_w.saturating_sub(leaf_mi.1)) {
                    neighbours.above[leaf_mi.1 + cell][1] = u_state;
                    neighbours.above[leaf_mi.1 + cell][2] = v_state;
                }
            }
        }
        // This leaf's own palette state into the mi-granular above/left bands
        // (sizes 0 when it used none, which is what clears a previous block's
        // palette out of the next block's ctx and colour cache).
        {
            neighbours.record_palette_y_rect(leaf_mi, 8, 8, pal_y_size, pal_y_colors);
            neighbours.record_palette_uv_rect(leaf_mi, 8, 8, pal_uv_size, pal_uv_colors);
        }
        neighbours.fill_skip_grid(leaf_mi, 2, skip);
        neighbours.fill_lf_grid(leaf_mi, 2, 4, 0, fctx);
        // lane-t900 r32: `set_txfm_ctxs` for an 8x8 leaf -- see `decode_block`'s
        // own copy; only an intrabc var-tx read consumes these bands on the intra
        // path, so the write is armed by `allow_intrabc`.
        if allow_intrabc {
            set_txfm_ctxs(neighbours, leaf_mi, 4, 2, 2, skip && intrabc_dv.is_some());
        }
        neighbours.record_mode_mi(leaf_mi.0, leaf_mi.1, 2, 2, mode);
        record_intrabc_mi(leaf_mi.0, leaf_mi.1, 2, intrabc_dv, neighbours, fctx);
        return Ok(mode);
    }
    neighbours.record_mi(leaf_mi, 8, &[&luma_grid[..], &u_grid[..], &v_grid[..]]);
    // This leaf's own palette state into the mi-granular above/left bands
    // (sizes 0 when it used none, which is what clears a previous block's
    // palette out of the next block's ctx and colour cache).
    {
        neighbours.record_palette_y_rect(leaf_mi, 8, 8, pal_y_size, pal_y_colors);
        neighbours.record_palette_uv_rect(leaf_mi, 8, 8, pal_uv_size, pal_uv_colors);
    }
    hit!(INTER8_SKIP_BAND_HITS);
    neighbours.fill_skip_grid(leaf_mi, 2, skip);
    // lane-kf900 r1: the leaf's own RESOLVED transform size, never the literal
    // block side. libaom calls `set_txfm_ctxs(mbmi->tx_size, ..., skip_txfm &&
    // is_inter_block(mbmi), xd)` (decodemv.c) -- for an INTRA block that skip
    // term is always 0, so a skipped 8x8 leaf whose `tx_depth` resolved to
    // TX_4X4 still publishes 4 to the next block's `get_tx_size_context`
    // (pred_common.h:342). Writing 8 here made the 8x16 strip below the leaf
    // read `tx_size_cat1` row 2 where libaom reads row 1 -- same alphabet,
    // wrong CDF row, so the msac range diverged mid-frame with no refusal
    // (class `wrong-alphabet-same-value`; the ReelB key frame at
    // -ss 900, first bad luma sample row 186 col 1803).
    neighbours.fill_lf_grid(leaf_mi, 2, resolved as u8, 0, fctx);
    // lane-t900 r32 / lane-av1txbands: `set_txfm_ctxs` for an 8x8 leaf -- see
    // `decode_block`'s own copy for why this stays beside
    // [`Neighbours::fill_lf_grid_rect`]'s publish (the skipped-intrabc
    // BLOCK-size term).
    if allow_intrabc {
        set_txfm_ctxs(
            neighbours,
            leaf_mi,
            resolved,
            2,
            2,
            skip && intrabc_dv.is_some(),
        );
    }
    neighbours.record_mode_mi(leaf_mi.0, leaf_mi.1, 2, 2, mode);
    // The DV predictor of a LATER intrabc block is the ordinary `INTRA_FRAME`
    // MV stack over this grid (`av1_find_mv_refs`), so an 8x8 leaf has to
    // publish its own cells exactly like `decode_block` does.
    record_intrabc_mi(leaf_mi.0, leaf_mi.1, 2, intrabc_dv, neighbours, fctx);
    Ok(mode)
}

/// lane-av1-422d: ONE 4:2:2 chroma transform unit of an 8x8 leaf
/// ([`decode_leaf8`]). At ss (1,0) the plane block is 4x8 and
/// `av1_get_max_uv_txsize(BLOCK_8X8)` resolves to
/// `max_txsize_rect_lookup[BLOCK_4X8]` == TX_4X8, so the WHOLE plane block is
/// a single rect unit -- one txb_skip + one scan per plane in the
/// [`TxbSet::ChromaRect8x4`] class [`decode_rect_split`] proves for the
/// shape. Mirrors [`sub8_leaf_chroma444`]'s rect arm at the leaf's halved
/// position: the entropy context is the leaf's own 8x8 luma-span
/// above/left state (the `chroma_tiled` arm's convention; no `+3`
/// plane-block-bigger term -- the unit covers the plane block exactly), the
/// CFL source is the leaf's whole 8x8 luma block, and the plane's predicted
/// mode supplies the mode-derived default `tx_type` unless an armed
/// [`INTRABC_CHROMA_TX`] slot carries the intrabc luma's own (the caller
/// arms and disarms it, exactly as it does around its square reads).
#[allow(clippy::too_many_arguments)]
pub(super) fn leaf8_chroma422_unit(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    around: (bool, bool, i32),
    uv_predict_mode: usize,
    angle_delta_uv: i32,
    reach: Reach,
    plane_idx: usize,
    cfl: Option<(i32, CflSrc)>,
    skip: bool,
    // The prediction override the square arms feed through
    // `set_palette_pred`: an intrabc leaf's frame copy at the DV takes
    // priority over a palette block's colour-index map.
    pred_override: Option<Vec<u16>>,
    // Unread, like [`read_plane`]'s own: the borrow proves the caller holds
    // the plane exclusively while the unit's coefficients are parsed.
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    cpx: usize,
    cpy: usize,
    smooth_neighbor_uv: bool,
    fctx: &crate::decode::FrameCtx,
) -> Result<Grid> {
    // The unit's prediction runs whether or not a residual follows it -- a
    // skipped leaf still predicts (the CfL contribution survives a skip),
    // and a skipped intrabc leaf still copies the frame at its DV.
    if let Some(buf) = pred_override {
        set_palette_pred(buf, fctx);
    }
    if skip {
        push_intra_rect(
            y,
            u,
            v,
            plane_idx,
            cpx,
            cpy,
            4,
            8,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            &ZERO_RESIDUAL[..4 * 8],
            cfl,
            None,
            smooth_neighbor_uv,
            fctx,
        );
        return Ok(Grid::Zero(4 * 8));
    }
    let default_tx = fctx
        .intrabc_chroma_tx
        .with(std::cell::Cell::get)
        .unwrap_or_else(|| default_intra_tx_type(uv_predict_mode as u8));
    let mut coding = cdfs.txb(TxbSet::ChromaRect8x4, uv_predict_mode);
    let (levels, tx_type) = read_coeffs_rect(
        dec,
        &mut coding,
        &SCAN_4X8,
        4,
        8,
        usize::from(around.0) + usize::from(around.1),
        dc_sign_ctx(around.2),
        default_tx,
        plane_idx,
    )?;
    let residual = dequant_and_inverse_typed_wh(
        &levels,
        4,
        8,
        crate::decode::bit_depth(fctx),
        block_q_idx(fctx),
        plane_q_delta(plane_idx, fctx).0,
        plane_q_delta(plane_idx, fctx).1,
        tx_type,
        block_iqmatrix(fctx, plane_idx, 4, 8, tx_type),
    );
    push_intra_rect(
        y,
        u,
        v,
        plane_idx,
        cpx,
        cpy,
        4,
        8,
        uv_predict_mode,
        angle_delta_uv,
        reach,
        &residual,
        cfl,
        None,
        smooth_neighbor_uv,
        fctx,
    );
    Ok(levels)
}

/// `read_intra_frame_mode_info`'s mode-info read for one `BLOCK_4X4` leaf
/// below an 8x8 partition (spec `decode_partition`'s `bSize < BLOCK_8X8`
/// bottom, where no partition symbol is read at all -- `decode_block` runs
/// directly). No palette (`av1_allow_palette` needs both dims >= 8, spec
/// blockd.h) and no `read_tx_size` (a sub-8x8 block's transform is always
/// its own bsize, spec 5.11.16). `has_chroma` follows `is_chroma_reference`
/// (spec/libaom `av1_common_int.h`): for 4:2:0, only the bottom-right leaf
/// of the 2x2 4x4 group carries chroma syntax at all.
#[allow(clippy::too_many_arguments)]
pub(super) fn read_intra_mode_sub8(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    above_mode: usize,
    left_mode: usize,
    has_chroma: bool,
    enable_filter_intra: bool,
    // [`filter_intra_size_class_rect`] for the LEAF's own shape: 0 for a
    // `BLOCK_4X4` split leaf, 8/9 for a 4x8/8x4 rect leaf. Reading the flag
    // from the wrong row is the same VALUE with a different interval, so it
    // desyncs a block or two later, not here (class `wrong-alphabet-same-value`).
    fi_class: usize,
    skip_ctx: usize,
    allow_intrabc: bool,
    mi_r: usize,
    mi_c: usize,
    // The leaf's own mi footprint for `set_segment_id`: 1x1 for a `BLOCK_4X4`
    // split leaf, 2x1/1x2 for an 8x4/4x8 one. Stamping 1x1 for a rect leaf
    // left the second mi column at segment 0, which reads back as a
    // neighbour's `cdf_index`/prediction two blocks later.
    seg_w_mi: usize,
    seg_h_mi: usize,
    fctx: &crate::decode::FrameCtx,
) -> Result<(
    bool,
    usize,
    i32,
    Option<(usize, i32, Option<(i32, i32)>)>,
    Option<usize>,
    Option<(i32, i32)>,
)> {
    // lane-av1mono: a monochrome leaf codes no chroma mode (libaom
    // `decodemv.c:933`), so fold `NumPlanes == 1` into `has_chroma`.
    let has_chroma = has_chroma && !mono(fctx);
    let trace = crate::envflags::env_flag!("EC_AV1_TRACE");
    let ec_istep = crate::envflags::env_flag!("EC_TRACE_MODE_STEP");
    crate::msac::SymbolDecoder::set_symr_mi(mi_r as i64, mi_c as i64);
    if ec_istep {
        eprintln!(
            "EC_IMODE mi_row={mi_r} mi_col={mi_c} fn=sub8 rng={}",
            dec.debug_state().0
        );
    }
    // Same `EC_ISTEP` ladder the square/rect readers print, so a cross-decoder
    // range diff lands on the ELEMENT inside a 4x4 leaf, not just the block.
    macro_rules! istep {
        ($name:literal, $val:expr) => {
            if ec_istep {
                eprintln!(
                    "EC_ISTEP mi_row={mi_r} mi_col={mi_c} name={} val={} rng={}",
                    $name,
                    $val,
                    dec.debug_state().0
                );
            }
        };
    }
    // lane-seg2: same `intra_segment_id` placement as the square and rect
    // readers (spec 5.11.6) -- a `BLOCK_4X4` leaf codes its own `segment_id`
    // like any other block, over its own 1x1 mi footprint. Dropping it here
    // both ate a symbol (the tile desynced at the frame's first sub-8x8
    // split) and left `cur_segment_id` on the previous block, so the leaf
    // dequantized with a neighbour's `SEG_LVL_ALT_Q`.
    if seg_id_pre_skip(fctx) {
        intra_segment_id(dec, cdfs, mi_r, mi_c, seg_w_mi, seg_h_mi, false, fctx);
    }
    let skip = dec.symbol(&mut cdfs.skip[skip_ctx]) != 0;
    if trace {
        eprintln!(
            "TRACE sub8 skip mi=({mi_r},{mi_c}) ctx={skip_ctx} value={} rng={}",
            skip as i32,
            dec.debug_state().0
        );
    }
    istep!("skip", skip as i32);
    if !seg_id_pre_skip(fctx) {
        intra_segment_id(dec, cdfs, mi_r, mi_c, seg_w_mi, seg_h_mi, skip, fctx);
    }
    maybe_read_cdef_idx(dec, mi_r, mi_c, (2, 2), skip, fctx);
    istep!("cdef", 0);
    maybe_read_delta_q(dec, cdfs, mi_r, mi_c, false, skip, fctx);
    maybe_read_delta_lf(dec, cdfs, mi_r, mi_c, false, skip, fctx);
    istep!("dq", 0);
    if allow_intrabc {
        let use_intrabc = dec.symbol(&mut cdfs.intrabc) != 0;
        if ec_istep {
            eprintln!(
                "EC_ISTEP mi_row={mi_r} mi_col={mi_c} name=intrabc val={} rng={}",
                u8::from(use_intrabc),
                dec.debug_state().0
            );
        }
        if use_intrabc {
            // lane-av1txr: `read_intrabc_info` (spec 5.11.13) reads the block
            // vector right after the flag and RETURNS -- no y/uv mode, angle
            // delta, palette, CfL or filter-intra syntax follows, and the
            // block is reconstructed by the INTER path (libaom's
            // `is_inter_block` counts an intrabc block), so the caller takes
            // the frame-copy prediction + inter residual.
            let dv = read_intrabc_dv(dec, cdfs, mi_r, mi_c, (seg_w_mi, seg_h_mi), fctx);
            // lane-av1txr: the DV travels back in the RETURN value -- it is
            // NOT published through `FrameCtx::intrabc_dv`. That slot is the
            // SQUARE paths' handoff (`decode_block`/`decode_leaf8` arm it,
            // then `take()` it in the same call), so a sub-8x8 leaf that
            // armed it left the vector standing for the NEXT >=8x8 block:
            // `warped.obu`'s (38,60) 8x8 intra block took it as its own
            // intrabc and read an inter var-tx `txfm_split` where libaom read
            // an intra `tx_depth` (class `stale-slot-handoff`).
            hit!(INTRABC_HITS);
            return Ok((skip, DC_PRED, 0, None, None, Some(dv)));
        }
    }
    let above_ctx = INTRA_MODE_CTX[above_mode];
    let left_ctx = INTRA_MODE_CTX[left_mode];
    let mode = dec.symbol(&mut cdfs.kf_y_mode[above_ctx][left_ctx]);
    istep!("mode", mode);
    if trace {
        eprintln!(
            "TRACE sub8 y_mode mi=({mi_r},{mi_c}) ctx=({above_ctx},{left_ctx}) value={mode} rng={}",
            dec.debug_state().0
        );
    }
    // No `angle_delta` at all below 8x8: spec 5.11.6's `intra_angle_info_y`/
    // `_uv` are gated on `MiSize >= BLOCK_8X8` (libaom `av1_use_angle_delta`,
    // blockd.h), so a directional mode on a `BLOCK_4X4` leaf carries no delta
    // symbol. Reading one consumed a symbol the encoder never wrote and
    // desynced the tile at the very next element.
    let angle_delta_y = 0;
    let chroma = if has_chroma {
        // `is_cfl_allowed` (blockd.h): at lossless CfL needs the CHROMA plane
        // block to be a single TX_4X4 (`cfl_allowed_px`). True for this leaf
        // at 4:2:0 (the group's plane block is 4x4) and for a 4:4:4
        // `BLOCK_4X4` split leaf, but FALSE for a 4:4:4 `BLOCK_8X4`/`BLOCK_4X8`
        // rect leaf, whose plane block is the rect itself -- that shape reads
        // the 13-symbol `uv_mode_no_cfl` CDF, and reading the CFL alphabet
        // there desynced the tile at the rect leaf's own uv_mode symbol (measured:
        // ours consumed the 14-symbol interval where the oracle read 13, first
        // divergence at the rect leaf mi (8,16) of `ll444.obu`; class
        // `wrong-alphabet-same-value`, lane-av1-llsub8).
        let uv_mode = if cfl_allowed_px(seg_w_mi * MI, seg_h_mi * MI, fctx) {
            crate::msac::SymbolDecoder::set_symr_cdf("uv_mode");
            dec.symbol(&mut cdfs.uv_mode_cfl[mode])
        } else {
            crate::msac::SymbolDecoder::set_symr_cdf("uv_mode");
            dec.symbol(&mut cdfs.uv_mode_no_cfl[mode])
        };
        if trace {
            eprintln!(
                "TRACE sub8 uv_mode value={uv_mode} rng={}",
                dec.debug_state().0
            );
        }
        if (9..=12).contains(&uv_mode) {
            hit!(SMOOTH_UV_HITS);
        }
        let alpha = if uv_mode == UV_CFL_PRED {
            Some(read_cfl_alphas(dec, cdfs))
        } else {
            None
        };
        if (V_PRED..=D67_PRED).contains(&uv_mode) {
            hit!(DIRECTIONAL_UV_HITS);
        }
        // See the luma comment above: no angle delta below 8x8.
        let angle_delta_uv = 0;
        Some((uv_mode, angle_delta_uv, alpha))
    } else {
        None
    };
    let mut filter_intra = None;
    if mode == DC_PRED && enable_filter_intra {
        let use_filter_intra = dec.symbol(&mut cdfs.filter_intra[fi_class]) != 0;
        if trace {
            eprintln!(
                "TRACE sub8 use_filter_intra value={}",
                use_filter_intra as i32
            );
        }
        if use_filter_intra {
            hit!(FILTER_INTRA_HITS);
            let fi_mode = dec.symbol(&mut cdfs.filter_intra_mode);
            filter_intra = Some(fi_mode);
        }
    }
    Ok((skip, mode, angle_delta_y, chroma, filter_intra, None))
}

//! Intra arm of `decode_inter_block` (the `else` of its `is_inter` test), moved out (av1split D8a).
//! The body is unchanged apart from the signature, which carries the locals it reads and
//! returns the ones the caller reads after the branch. `Ok(None)` is the arm's early return.

use super::*;

/// The outputs the intra arm hands back to `decode_inter_block`, in the order it destructures them.
type IntraArmOut = (
    usize,
    usize,
    [u8; 2],
    i8,
    bool,
    Grid,
    Grid,
    Grid,
    Option<Vec<(usize, usize, usize, usize)>>,
    (usize, [u16; 8], usize, [u16; 8]),
    bool,
);

// the arm takes what decode_inter_block's inline branch read; its outputs are declared late, as they were inline
#[allow(clippy::too_many_arguments, clippy::needless_late_init)]
pub(super) fn decode_inter_block_intra_arm(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    grid: &mut MiGrid,
    at: (usize, usize),
    side: usize,
    mi_cols: u32,
    mi_rows: u32,
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    base_q_idx: u8,
    luma_set_intra: TxbSet,
    luma_tx: usize,
    scan_luma: &[u16],
    write_w: usize,
    write_h: usize,
    allow_screen_content_tools: bool,
    fctx: &crate::decode::FrameCtx,
    at_mi: (usize, usize),
    px: usize,
    py: usize,
    r: usize,
    c: usize,
    rmi: usize,
    cmi: usize,
    cpx: usize,
    cpy: usize,
    chroma_side: usize,
    chroma_stride: usize,
    chroma_buf_h: usize,
    chroma_set: TxbSet,
    chroma_tx: usize,
    scan_chroma: &[u16],
    strip_chroma: Option<InterStripChroma>,
    skip: bool,
    skip_mode: bool,
    mut mu_chroma: bool,
) -> Result<Option<IntraArmOut>> {
    let mode_for_tx;
    let uv_predict_mode;
    let block_filter;
    let ref_frame_for_lf;
    let globalmv_for_lf;
    let vartx_leaves;
    let (luma_grid, u_grid, v_grid);
    let palette_record: (usize, [u16; 8], usize, [u16; 8]);
    // lane-partitions r1: intra prediction (`PlaneBuf::reconstruct`) has
    // no rectangular counterpart -- an intra-coded HORZ/VERT strip needs
    // its own true-width/height edge/predict math, unlike the inter
    // skip path above (which only needed a clipped write of an
    // already-square-predicted buffer). Named refusal instead of a
    // silently wrong square-shaped intra prediction.
    // lane-inter16ab r2: `decode_intra_rect_in_inter` has no chroma-pair
    // path (it predicts and codes chroma at the strip's own halved
    // footprint), so an INTRA 16x4 / 4x16 strip inside a 1:4 partition is
    // refused by name rather than decoded with a 8x2 chroma transform
    // libaom never coded.
    // lane-intra16x4 r1: the strip DECODES through the key frame's own
    // per-strip body, [`decode_rect4_16_strip`].
    // lane-sub8x4 r3: pixel-proven, so the decode opt-in was lifted. r1
    // and r2 found no witness because every firing film frame ALSO carried
    // a sub-8x8 intra block, which refused until r2 lifted it; on this tip
    // the first 50 frame OBUs of the 10-bit 3840x1608 stream's head decode
    // all 33 frames pixel-exact while this arm fires 31x 16x4 + 17x 4x16
    // (23 of them chroma-paired). Pinned as
    // `fixtures/reelb_rect64_intra16x4_witness.obu` -- MEASURED WRONG, see
    // the lane-wit16x4 note below.
    // The refusal below is NARROWED, not deleted: it still stands for a
    // 16x4/4x16 strip with no chroma-pair record and for every other
    // sub-8 shape.
    //
    // lane-wit16x4 r1: RE-WIDENED to the whole shape, because r3's witness
    // was a PHANTOM (class `counter-from-refused-stream`): the pinned
    // fixture was measured while decode-order frame 33 was desynced, and
    // on the merged tip (lane-frame36 r2 fixed that desync) the very same
    // fixture decodes all 33 frames pixel-exact with `intra16x4_in_inter`
    // 16x4=0 4x16=0 chroma_ref=0 -- the 31/17/23 were counted out of a
    // wrong decode. Re-measured this round: NO stream anywhere decodes
    // exactly while this arm fires. Two 2 s 10-bit film cuts (3840x1608 at
    // 0 s: OK, 48 frames exact, 0 hits; at 300 s: 138/559 hits but frames
    // 1 and 2 mismatch ffmpeg by 14105680 / 12470780 bytes), four 1920x792
    // 128-SB cuts (hits only inside frames that refuse or are never shown
    // -- the 6300 s cut's key frame is exact with 0 hits and its no-show
    // frame 1 carries 169/445 with no display frame reachable before the
    // 128x64-intra-on-inter refusal), and the aomenc gate
    // `a_real_aomenc_inter_sequence_with_intra_16x4_strips_in_1to4_partitions_decodes_pixel_exact`
    // (run `--include-ignored` on this tip: FAILED, 0 pixel-exact attempts
    // carrying the strip, per-arm [0, 0, 0]). A lifted refusal with no
    // witness is a capability claim (class `refusal-lifted-without-a-gate`),
    // so the decode path stayed behind an opt-in for the round that found
    // a witness.
    //
    // lane-t900 r6: THAT WITNESS EXISTS, so the opt-in was lifted (the path decodes by default)
    // and the refusal is narrowed back to "no chroma-pair record". The
    // witness is a REAL 10-bit 1920x792 128-superblock film stream, not an
    // aomenc recipe: `fixtures/reelc_sb128_inter_witness.obu` (177773 bytes,
    // 23 frame-carrying OBUs, 16 decode-order frames, extended from 14 in
    // lane-t900 r7) decodes all 16 decode-order frames byte-exact against
    // instrumented aomdec
    // (`EC_AV1_FINAL_DUMP`) and all 15 shown frames pixel-exact against
    // ffmpeg, while this arm fires 78x 16x4 + 149x 4x16 (108 of them
    // chroma-paired). Unlike lane-sub8x4 r3's phantom (class
    // `counter-from-refused-stream`) every one of those hits lies inside a
    // frame that was decoded AND compared exact. Gate
    // `a_10bit_128sb_film_frames_with_warp_cdef_and_interintra_decode_pixel_exact`.
    let strip16 = strip_chroma
        .filter(|_| write_w.min(write_h) == 4 && write_w.max(write_h) == 16)
        .map(|s| (s.horz, s.has_chroma));
    if write_w.min(write_h) < 8 && strip16.is_none() {
        if crate::envflags::env_flag!("EC_INTRA16X4") {
            let (idx, hc, prev_inter) = match strip_chroma {
                Some(s) => (
                    if s.horz {
                        rmi - s.pair_mi.0
                    } else {
                        cmi - s.pair_mi.1
                    },
                    s.has_chroma,
                    s.prev.is_some(),
                ),
                None => (9, true, false),
            };
            eprintln!(
                "EC_INTRA16X4 w={write_w} h={write_h} pair_parity={idx} has_chroma={hc} prev_single_inter={prev_inter} skip={skip} mi={rmi},{cmi} tx_select={} scr={}",
                fctx.tx_select_inter.with(std::cell::Cell::get),
                allow_screen_content_tools,
            );
        }
        return Err(unsupported(
            "an intra 16x4/4x16 strip inside an inter 16x16-level 1:4 partition (its 4:2:0 chroma pair is coded once for two strips; only the inter path implements that pairing)",
        ));
    }
    if write_w != side || write_h != side {
        // lane-intrarect r1: the strip decodes through its own rect
        // machinery ([`decode_intra_rect_in_inter`] -> the key-frame
        // paths' [`decode_rect_split`]) instead of refusing. That call
        // already writes everything the shared tail below would for a
        // square block EXCEPT the inter-side bands and the mi map, so
        // this arm returns rather than falling through (a second
        // `record_rect`/`fill_lf_grid_rect` at block granularity would
        // overwrite the per-transform-unit records it just made).
        decode_intra_rect_in_inter(
            dec,
            cdfs,
            neighbours,
            at,
            write_w,
            write_h,
            skip,
            allow_screen_content_tools,
            strip16,
            y,
            u,
            v,
            base_q_idx,
            fctx,
        )?;
        // merge b51719f+lane-intrarect: `at` is in MI units on this path
        // (lane-inter4), so the strip's mi origin is `at` itself -- the
        // lane's `r * SUB_MI` re-derivation would snap a sub-SUB strip to
        // its enclosing 16-px cell.
        let (mi_r0, mi_c0) = (rmi, cmi);
        grid.fill_rect(
            mi_r0,
            mi_c0,
            write_h / MI,
            write_w / MI,
            MiInfo {
                is_inter: false,
                ref_frame: -1,
                ref_frame1: NO_REF1,
                mv1: (0, 0),
                mv: (0, 0),
                is_new_mv: false,
                size: (write_w / MI) as u8,
                size_h: (write_h / MI) as u8,
                is_global_mv0: false,
                is_global_mv1: false,
            },
        );
        neighbours.record_inter_rect_mi(
            at,
            write_w / MI,
            write_h / MI,
            skip,
            false,
            0,
            [3, 3],
            skip_mode,
        );
        return Ok(None);
    }
    crate::msac::SymbolDecoder::set_symr_cdf("y_mode");
    let mode = dec.symbol(&mut cdfs.y_mode[size_group_wh(write_w, write_h)]);
    if crate::envflags::env_flag!("EC_IIS") {
        eprintln!(
            "TRACE iis px={px} py={py} side={side} w={write_w} h={write_h} sg={} mode={mode} skip={skip}",
            size_group_wh(write_w, write_h)
        );
    }
    if mode >= 13 {
        return Err(unsupported(
            "an intra mode this decoder does not code (round 2)",
        ));
    }
    // lane-angleinter r1: `read_intra_angle_info` (libaom `decodemv.c`,
    // inside `read_intra_block_mode_info`) -- an intra block in an inter
    // frame reads `angle_delta_y` off the same
    // `angle_delta_cdf[mode - V_PRED]` row a key-frame block does, and
    // the delta feeds the directional predictor (and the edge
    // filter/upsample decision derived from the delta'd angle) unchanged.
    // The symbol was read and then refused before this round even though
    // aomenc enables `--enable-angle-delta` by default.
    let angle_delta_y = if (V_PRED..=D67_PRED).contains(&mode) {
        read_angle_delta(dec, &mut cdfs.angle_delta[mode - V_PRED])
    } else {
        0
    };
    if angle_delta_y != 0 {
        hit!(INTRA_IN_INTER_ANGLE_DELTA_Y_HITS);
    }
    // The delta'd angle decides whether the intra edge filter runs at
    // all, so this block needs the same luma edge-filter-type neighbour
    // check a key-frame block does (`get_intra_edge_filter_type`, spec
    // 7.11.2) -- this arm passed a hardcoded `false` while every
    // directional intra-in-inter block was refused above.
    let (nb_above_mode, nb_left_mode) = neighbours.modes_above_left(r, c);
    let smooth_neighbor = is_smooth_mode(nb_above_mode) || is_smooth_mode(nb_left_mode);
    // lane-sbrect10 r1 (class wrong-alphabet-same-value, second instance
    // after lane-sbpart r3's rect strips): `is_cfl_allowed` (libaom
    // `blockd.h` -- `block_size_wide[bsize] <= 32 && block_size_high[bsize]
    // <= 32`, spec 5.11.5) caps CFL at 32x32, so a 64x64 intra block inside
    // an INTER frame reads the 13-symbol `uv_mode_no_cfl` alphabet. Reading
    // the 14-symbol CFL one gave the same DC_PRED VALUE but narrowed the
    // arithmetic coder by the wrong interval, desyncing the tile from this
    // block on.
    // lane-av1mono: a monochrome block codes no chroma mode/CFL symbol
    // (libaom `decodemv.c:933`).
    let has_chroma = !mono(fctx);
    let cfl_allowed = has_chroma && cfl_allowed_px(write_w, write_h, fctx);
    if !cfl_allowed {
        hit!(NOCFL_UV_MODE_HITS);
    }
    let uv_mode = if !has_chroma {
        DC_PRED
    } else if cfl_allowed {
        crate::msac::SymbolDecoder::set_symr_cdf("uv_mode");
        dec.symbol(&mut cdfs.uv_mode_cfl[mode])
    } else {
        crate::msac::SymbolDecoder::set_symr_cdf("uv_mode");
        dec.symbol(&mut cdfs.uv_mode_no_cfl[mode])
    };
    if (9..=12).contains(&uv_mode) {
        hit!(SMOOTH_UV_HITS);
    }
    let alpha = if cfl_allowed && uv_mode == UV_CFL_PRED {
        Some(read_cfl_alphas(dec, cdfs))
    } else {
        None
    };
    // `get_uv_mode` (spec 9.3): `UV_CFL_PRED` predicts as `DC_PRED` for
    // the angle-delta question, same as [`read_intra_mode`]'s own copy
    // -- `uv_mode` (13) already falls outside `V_PRED..=D67_PRED` so the
    // `else` branch below is exact for it either way.
    let angle_delta_uv = if (V_PRED..=D67_PRED).contains(&uv_mode) {
        hit!(DIRECTIONAL_UV_HITS);
        read_angle_delta(dec, &mut cdfs.angle_delta[uv_mode - V_PRED])
    } else {
        0
    };
    if angle_delta_uv != 0 {
        hit!(UV_ANGLE_DELTA_HITS);
        hit!(INTRA_IN_INTER_ANGLE_DELTA_UV_HITS);
    }
    uv_predict_mode = if uv_mode == UV_CFL_PRED {
        DC_PRED
    } else {
        uv_mode
    };
    // lane-chroma r4: same chroma edge-filter-strength neighbour check
    // as [`read_intra_mode`]/[`read_intra_mode_rect`] -- the CHROMA
    // neighbour's own `uv_mode`, not the luma one.
    let smooth_neighbor_uv =
        neighbours.smooth_uv_neighbour(r * (SUB / MI), c * (SUB / MI), r, c, fctx);
    // `read_palette_mode_info` (decodemv.c:567, called from
    // `read_intra_block_mode_info` right after `xd->cfl.store_y`, same
    // gating and same corner-cut as `read_intra_mode`'s own copy above
    // -- `palette_mode_ctx`/`palette_uv_mode_ctx` hardcoded 0, provably
    // safe since a nonzero neighbour `palette_size` would already have
    // refused the decode that produced it.
    // lane-inter16ab r4 (class sweep): `av1_get_palette_bsize_ctx` is keyed
    // by the block's TRUE footprint -- a rect strip's row is not its
    // enclosing square's, and a 16x4 strip (64 pixels) reads no palette
    // symbol at all.
    // lane-t900 r22: reconstructed, not refused. Same syntax and same
    // deferral of the colour-index maps past `filter_intra`
    // (`av1_visit_palette` runs from `parse_decode_block`, after the whole
    // mode-info read) as the key frame's [`read_intra_mode`]; the mode
    // context and colour cache are this block's real above/left ones, at
    // its own mi position (`av1_get_palette_mode_ctx`/
    // `av1_get_palette_cache`), not the hardcoded 0/empty pair the refusal
    // could get away with while no neighbour could ever be a palette.
    let (palette_ctx, palette_cache) = neighbours.palette_ctx_and_cache_mi((rmi, cmi));
    let palette_uv_cache = neighbours.palette_uv_cache_mi((rmi, cmi));
    let mut palette_y_pending: Option<(usize, [u16; 8])> = None;
    let mut palette_uv_pending: Option<(usize, [u16; 8], [u16; 8])> = None;
    if allow_screen_content_tools && let Some(bsize_ctx) = palette_bsize_ctx_wh(write_w, write_h) {
        let use_palette_y =
            mode == DC_PRED && dec.symbol(&mut cdfs.palette_y_mode[bsize_ctx][palette_ctx]) != 0;
        if use_palette_y {
            let n = 2 + dec.symbol(&mut cdfs.palette_y_size[bsize_ctx]);
            palette_y_pending = Some((n, read_palette_colors_y(dec, n, &palette_cache, fctx)));
        }
        if crate::envflags::env_flag!("EC_PALSYN") {
            eprintln!(
                "EC_PALSYN_PRE sq mi={rmi},{cmi} bw={write_w} bh={write_h} mode={mode} uv={uv_mode} ctx={palette_ctx} y={} rng={}",
                palette_y_pending.map_or(0, |(n, _)| n),
                dec.debug_state().0
            );
        }
        let use_palette_uv = has_chroma
            && uv_mode == DC_PRED
            && dec.symbol(&mut cdfs.palette_uv_mode[usize::from(use_palette_y)]) != 0;
        if crate::envflags::env_flag!("EC_PALSYN") {
            eprintln!(
                "EC_PALSYN sq mi={rmi},{cmi} bw={write_w} bh={write_h} mode={mode} uv={uv_mode} ctx={palette_ctx} y={} uv={} rng={}",
                use_palette_y as i32,
                use_palette_uv as i32,
                dec.debug_state().0
            );
        }
        if use_palette_uv {
            let n = 2 + dec.symbol(&mut cdfs.palette_uv_size[bsize_ctx]);
            let (u_colors, v_colors) = read_palette_colors_uv(dec, n, &palette_uv_cache, fctx);
            palette_uv_pending = Some((n, u_colors, v_colors));
        }
    }
    // lane-fiinter r1: `read_filter_intra_mode_info` (libaom `decodemv.c`,
    // the tail of `read_intra_block_mode_info`) -- an intra block inside
    // an INTER frame reads `use_filter_intra` in exactly the same place a
    // key-frame block does, right after the palette syntax. Nothing read
    // it here before this round, and aomenc enables filter intra by
    // default, so every inter frame carrying one desynced at this symbol.
    // `av1_filter_intra_allowed`'s `palette_size[0] == 0` term is implied:
    // a real palette-Y block already returned above.
    let mut filter_intra = None;
    if mode == DC_PRED
        && fctx.enable_filter_intra_inter.with(std::cell::Cell::get)
        // `av1_filter_intra_allowed` (reconintra.h:77) excludes a
        // palette-Y block, so no `use_filter_intra` symbol exists there.
        && palette_y_pending.is_none()
        // lane-inter16ab r4 (class sweep): `cdf::FILTER_INTRA`'s row is the
        // true `BLOCK_SIZES_ALL` class of this strip, not its square side's.
        && let Some(class) = filter_intra_size_class_rect(write_w, write_h)
        && dec.symbol(&mut cdfs.filter_intra[class]) != 0
    {
        hit!(FILTER_INTRA_HITS);
        hit!(INTRA_IN_INTER_FILTER_INTRA_HITS);
        filter_intra = Some(dec.symbol(&mut cdfs.filter_intra_mode));
    }
    // `av1_visit_palette`: the two colour-index maps, Y then the shared
    // chroma one, after the WHOLE mode-info read.
    let palette_y = palette_y_pending.map(|(n, colors)| {
        let map = decode_color_index_map_wh(
            dec,
            cdfs,
            n,
            write_w,
            write_h,
            palette_onscreen(rmi, cmi, write_w, write_h, fctx),
            false,
        );
        hit!(PALETTE_HITS);
        hit!(INTRA_IN_INTER_PALETTE_Y_HITS);
        PaletteY {
            size: n,
            colors,
            map,
        }
    });
    let palette_uv = palette_uv_pending.map(|(n, u_colors, v_colors)| {
        // `av1_get_plane_block_size(bsize, 1, 1)` at the sequence's own
        // subsampling, floored at the 4-px transform -- the rect reader's
        // own copy; a hardcoded `/ 2` sized a 4:4:4 chroma map at 4:2:0
        // and the full-size reconstruct panicked on it (lane-av1-444).
        let (cw, ch) = (
            (write_w >> ss_x(fctx)).max(4),
            (write_h >> ss_y(fctx)).max(4),
        );
        let on = palette_onscreen_uv(
            (write_w, write_h),
            palette_onscreen(rmi, cmi, write_w, write_h, fctx),
            (cw, ch),
            (ss_x(fctx), ss_y(fctx)),
        );
        let map = decode_color_index_map_wh(dec, cdfs, n, cw, ch, on, true);
        hit!(PALETTE_UV_HITS);
        hit!(INTRA_IN_INTER_PALETTE_UV_HITS);
        PaletteUv {
            size: n,
            u_colors,
            v_colors,
            map,
        }
    });
    palette_record = (
        palette_y.as_ref().map_or(0, |p| p.size),
        palette_y.as_ref().map_or([0u16; 8], |p| p.colors),
        palette_uv.as_ref().map_or(0, |p| p.size),
        palette_uv.as_ref().map_or([0u16; 8], |p| p.u_colors),
    );
    // A palette block predicts from its own colour-index map, never from
    // the block edge (spec 7.11.2/7.11.4) -- these ride the [`PALETTE_PRED`]
    // slot into each `reconstruct`/`read_plane` call below, exactly as the
    // key-frame paths do.
    let palette_y_buf: Option<Vec<u16>> = palette_y
        .as_ref()
        .map(|p| p.map.iter().map(|&i| p.colors[i as usize]).collect());
    let palette_uv_bufs: Option<(Vec<u16>, Vec<u16>)> = palette_uv.as_ref().map(|p| {
        (
            p.map.iter().map(|&i| p.u_colors[i as usize]).collect(),
            p.map.iter().map(|&i| p.v_colors[i as usize]).collect(),
        )
    });
    mode_for_tx = mode;
    block_filter = [3, 3];
    ref_frame_for_lf = 0;
    globalmv_for_lf = false;
    let (mi_row, mi_col) = (rmi, cmi);
    grid.fill_rect(
        mi_row,
        mi_col,
        side / 4,
        side / 4,
        MiInfo {
            is_inter: false,
            ref_frame: -1,
            ref_frame1: NO_REF1,
            mv1: (0, 0),
            mv: (0, 0),
            is_new_mv: false,
            size: (side / 4) as u8,
            size_h: (side / 4) as u8,
            is_global_mv0: false,
            is_global_mv1: false,
        },
    );

    // lane-intrainter r1: an intra block in an inter frame reads the same
    // `tx_depth` symbol a key-frame block does, and when it splits, the
    // block codes one intra prediction + one coefficient set PER
    // transform unit (`decode_block`'s multi-TU branch), not one for the
    // whole block. The leaf list rides the existing `vartx_leaves` slot,
    // whose tail already does the split-block neighbour write-back
    // (`record_split_luma`) and the per-TU deblock edges.
    vartx_leaves = read_block_tx_size(
        dec,
        cdfs,
        neighbours,
        at_mi,
        side,
        (mi_cols as usize, mi_rows as usize),
        false,
        skip,
        fctx,
    )?
    .1;
    let reach = Reach::of(side, px, py, y.width, y.height, fctx);
    let split_luma = vartx_leaves.is_some();
    // lane-sb128c r3: a 128x128 INTRA block inside an inter frame codes
    // its chroma per 64x64 mu chunk, exactly as the inter half above --
    // `decode_token_recon_block` walks `mu_blocks_wide/high` and codes
    // every plane of a chunk before the next chunk's luma. These collect
    // the four TX_32X32 units per plane for the block-level write-back.
    let mut u_units: Vec<i32> = Vec::new();
    let mut v_units: Vec<i32> = Vec::new();
    if let Some(leaves) = vartx_leaves.as_ref() {
        let reduced = fctx.reduced_tx_set_inter.with(std::cell::Cell::get);
        // lane-av1h5: chunk-major token order at 128 (see [`mu_chunk_order`]).
        let (order, _mu_reordered) = mu_chunk_order(leaves, side);
        for (pos, &idx) in order.iter().enumerate() {
            let (row, col, tx_px, _) = leaves[idx];
            let tu_mi = (at_mi.0 + row, at_mi.1 + col);
            let (tu_px, tu_py) = (px + col * MI, py + row * MI);
            // A transform unit inside a block is not a block at that
            // position (class from lane-palette2 r12): top-right /
            // bottom-left availability is the block's, narrowed by the
            // unit's own offset inside it.
            let tu_reach = Reach::of_tu(side, side, col * MI, row * MI, tx_px, tx_px, reach);
            // This unit's own window on the block's palette prediction
            // (the whole-block buffer would be indexed at the wrong
            // stride by the first unit).
            if let Some(buf) = &palette_y_buf {
                hit!(PALETTE_SPLIT_TX_HITS);
                let window: Vec<u16> = (0..tx_px)
                    .flat_map(|i| {
                        let base = (row * MI + i) * side + col * MI;
                        buf[base..base + tx_px].iter().copied()
                    })
                    .collect();
                set_palette_pred(window, fctx);
            }
            let tu_grid = if skip {
                push_intra(
                    y,
                    u,
                    v,
                    0,
                    tu_px,
                    tu_py,
                    tx_px,
                    mode,
                    angle_delta_y,
                    tu_reach,
                    &ZERO_RESIDUAL[..tx_px * tx_px],
                    None,
                    filter_intra,
                    smooth_neighbor,
                    fctx,
                );
                Grid::Zero(tx_px * tx_px)
            } else {
                // The unit's bsize is smaller than the block it sits in,
                // so `txb_skip_ctx` is the neighbour-magnitude table, not
                // the lone-TU 0 (`get_txb_ctx_general`).
                let tu_skip_ctx = neighbours.luma_skip_ctx(tu_mi, tx_px / MI);
                read_plane(
                    dec,
                    cdfs,
                    txbset_for(tx_px, reduced),
                    // lane-sb128c r5: a TX_64X64 unit codes only its
                    // top-left 32x32 of frequencies (spec 5.11.40), so
                    // the scan and `tx_side` are the CODED corner while
                    // `side` stays the transform's true 64.
                    default_scan(tx_px.min(32)),
                    0,
                    neighbours.around_mi(tu_mi, tx_px)[0],
                    mode,
                    mode,
                    angle_delta_y,
                    tu_reach,
                    y,
                    u,
                    v,
                    tu_px,
                    tu_py,
                    tx_px,
                    tx_px.min(32),
                    base_q_idx,
                    None,
                    filter_intra,
                    Some(tu_skip_ctx),
                    smooth_neighbor,
                    fctx,
                )?
            };
            neighbours.record_mi_luma(tu_mi, tx_px, &tu_grid);
            if side > 64 {
                let chunk = (row / 16, col / 16);
                let done = pos + 1 == order.len()
                    || (leaves[order[pos + 1]].0 / 16, leaves[order[pos + 1]].1 / 16) != chunk;
                if !done {
                    continue;
                }
                let (cr, cc) = chunk;
                hit!(INTRA_128_IN_INTER_MU_CHROMA_HITS);
                // The unit's CHROMA size, its LUMA footprint, the
                // CHUNK's chroma extent and the unit's ORIGIN inside the
                // chunk are four different quantities; this arm had the
                // last three as one 4:2:0 literal and read ONE unit per
                // mu chunk at every subsampling.
                //
                // `cu_tx` is TX_32X32 at EVERY subsampling.
                // `av1_get_max_uv_txsize` (blockd.h:1371) is
                // `av1_get_adjusted_tx_size(max_txsize_rect_lookup[
                // get_plane_block_size(bsize, ss_x, ss_y)])`, and
                // `av1_get_adjusted_tx_size` (blockd.h:1361) takes NO
                // subsampling argument: it maps TX_64X64 / TX_64X32 /
                // TX_32X64 to TX_32X32 UNCONDITIONALLY, so even at ss (0,0)
                // (where `get_plane_block_size` returns BLOCK_128X128 and
                // the lookup entry is TX_64X64) the answer is TX_32X32.
                let cu_tx = 32usize;
                // The unit's own footprint on the LUMA grid, per axis
                // (`cu_tx << ss`): 64 luma px (16 mi) at 4:2:0, 32 luma px
                // (8 mi) at 4:4:4. This is the ss-aware form the repaired
                // siblings at decode.rs:17598 / 17685 use (`ox <<
                // ss_x(fctx)`, never `ox * 2`), and it is the size
                // `around_mi` / `record_mi_chroma` want.
                let unit_luma_w = cu_tx << ss_x(fctx);
                let unit_luma_h = cu_tx << ss_y(fctx);
                // The CHUNK's own extent, per axis: a mu chunk is a
                // 64x64 LUMA region at every subsampling, so on the luma
                // grid this is a literal 64, and its CHROMA plane block is
                // `64 >> ss` square. At 4:2:0 that is 32x32 = the one unit
                // this arm always coded; at 4:4:4 it is 64x64 = FOUR
                // TX_32X32 units. Measured on the gate's own 4:4:4 stream
                // against the oracle's `EC_TRACE_COEFF`: every chroma read
                // of a 128x128 intra-in-inter root is `tx_size=3`
                // (TX_32X32), and each 64x64 mu chunk contributes FOUR per
                // plane at (row,col) (0,0) (0,8) (8,0) (8,8) -- never a
                // TX_64X64 chroma unit.
                let chunk_luma = 64usize;
                let chunk_chroma_w = chunk_luma >> ss_x(fctx);
                let chunk_chroma_h = chunk_luma >> ss_y(fctx);
                let units_w = chunk_chroma_w / cu_tx;
                let units_h = chunk_chroma_h / cu_tx;
                let cu_scan = default_scan(cu_tx);
                if lossless(fctx) {
                    // lane-lossless2: TX_4x4 units over this chunk's
                    // chroma, not TX_32X32 ones.
                    read_intra_chroma_lossless(
                        y,
                        u,
                        v,
                        dec,
                        cdfs,
                        neighbours,
                        at_mi,
                        (px, py),
                        (cpx, cpy),
                        side,
                        // lane-av1422lpf site 6 (the intra-in-inter twin of
                        // site 5 at 41572): the plane block's own per-axis
                        // shape and stride, never the enclosing square.
                        // `palette_uv_bufs` is the decoded colour-index map
                        // at `(write_w >> ss_x, write_h >> ss_y)` (44144),
                        // i.e. per-axis already, and the lossless walk
                        // windows it at this `stride` -- at 4:2:2 a
                        // 16-wide square stride over an 8-wide map put the
                        // third unit row at index 180 of 128 samples
                        // (`palette_window`, decode.rs:2946; measured on
                        // `Y_intrabc_10b.obu`).
                        (chroma_stride, chroma_buf_h),
                        (cc * chunk_chroma_w, cr * chunk_chroma_h),
                        (chunk_chroma_w, chunk_chroma_h),
                        chroma_stride,
                        reach,
                        (y.width, y.height),
                        mode,
                        uv_predict_mode,
                        angle_delta_uv,
                        palette_uv_bufs.as_ref().map(|(a, b)| (&a[..], &b[..])),
                        smooth_neighbor_uv,
                        base_q_idx,
                        &mut u_units,
                        &mut v_units,
                        fctx,
                    )?;
                    mu_chroma = true;
                    continue;
                }
                // Plane-major per chunk (libaom's plane loop is outermost
                // inside the mu walk); within a plane the units walk raster.
                for plane_idx in 1..3 {
                    for ur in 0..units_h {
                        for uc in 0..units_w {
                            // The unit's own mi origin: the chunk's
                            // (16x16 mi) plus the unit's offset inside it.
                            let unit_mi = (
                                at_mi.0 + cr * 16 + ur * (unit_luma_h / MI),
                                at_mi.1 + cc * 16 + uc * (unit_luma_w / MI),
                            );
                            // The unit's own entropy-cell span, so the
                            // NEXT unit of this chunk reads THIS one's
                            // state and the next chunk reads the last.
                            //
                            // lane-av1muchunk422iis: the intra-in-inter twin
                            // of the compound mu-chunk gather. At ss (1, 0)
                            // one CHROMA column spans two luma mi columns,
                            // so libaom's `get_txb_ctx_general` reads its
                            // above votes over `txb_w_unit` chroma cells
                            // while a plain luma `around_mi` gathers
                            // `unit_luma_w / MI` -- twice as many -- and
                            // `dc_sign_ctx`'s signum flips whenever the
                            // doubled above vote cancels the left one. The
                            // LEFT extent is unsubsampled at ss_y 0, so the
                            // per-axis footprint is what the pair-aware
                            // helper needs; 4:2:0 / 4:4:4 have square
                            // footprints and keep the plain gather verbatim.
                            let cu_around = if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                                neighbours.around_mi_422_chroma(unit_mi, unit_luma_w, unit_luma_h)
                            } else {
                                neighbours.around_mi(unit_mi, unit_luma_w)
                            };
                            let (cu_x, cu_y) = (
                                cpx + cc * chunk_chroma_w + uc * cu_tx,
                                cpy + cr * chunk_chroma_h + ur * cu_tx,
                            );
                            let cu_reach = crate::decode::tu_reach(
                                side,
                                side,
                                cc * chunk_luma + uc * unit_luma_w,
                                cr * chunk_luma + ur * unit_luma_h,
                                unit_luma_w,
                                reach,
                                px,
                                py,
                                y.width,
                                y.height,
                                fctx,
                            );
                            let cu_grid = if skip {
                                push_intra(
                                    y,
                                    u,
                                    v,
                                    plane_idx,
                                    cu_x,
                                    cu_y,
                                    cu_tx,
                                    uv_predict_mode,
                                    angle_delta_uv,
                                    cu_reach,
                                    &ZERO_RESIDUAL[..cu_tx * cu_tx],
                                    None,
                                    None,
                                    smooth_neighbor_uv,
                                    fctx,
                                );
                                Grid::Zero(cu_tx * cu_tx)
                            } else {
                                read_plane(
                                    dec,
                                    cdfs,
                                    chroma_set,
                                    cu_scan,
                                    plane_idx,
                                    cu_around[plane_idx],
                                    mode,
                                    uv_predict_mode,
                                    angle_delta_uv,
                                    cu_reach,
                                    y,
                                    u,
                                    v,
                                    cu_x,
                                    cu_y,
                                    cu_tx,
                                    cu_tx,
                                    base_q_idx,
                                    // CfL is refused above 32x32
                                    // (`is_cfl_allowed`), so never here.
                                    None,
                                    None,
                                    // The uv plane block is larger than
                                    // the unit: `get_txb_ctx`'s offset-10
                                    // rows, +3 on the `txb_skip_chroma_32`
                                    // table (lane-sb128b r3).
                                    Some(3),
                                    smooth_neighbor_uv,
                                    fctx,
                                )?
                            };
                            // Immediately, so the NEXT unit reads this
                            // one's coefficient context.
                            neighbours.record_mi_chroma(
                                unit_mi,
                                unit_luma_w,
                                unit_luma_h,
                                plane_idx,
                                &cu_grid,
                            );
                            hit!(CHROMA_SPLIT_TX_HITS);
                            // lane-dpm1: the two INTER mu-chunk sites set
                            // this and this INTRA-in-inter one did not, so
                            // the whole-block chroma record below re-stamped
                            // all four 32x32 units with the TOP-LEFT unit's
                            // level and the next block read a txb_skip row
                            // libaom never selects (class
                            // `override-slot-on-one-arm`).
                            mu_chroma = true;
                            // lane-av1muchunk422b: the allocation of the intra-in-inter
                            // mu-chunk walk (site 8), per TX_32X32 unit.
                            hit!(MU_CHUNK_INTRA_IN_INTER_UNITS);
                            let dst = mu_units(
                                if plane_idx == 1 {
                                    &mut u_units
                                } else {
                                    &mut v_units
                                },
                                chroma_side * chroma_side,
                            );
                            // The ASSEMBLED grid stays at the square
                            // `chroma_side` stride the block tail's
                            // re-stamp indexes; the unit's true per-axis
                            // position inside its chunk is what changes.
                            for rr in 0..cu_tx {
                                let start = (cr * chunk_chroma_h + ur * cu_tx + rr) * chroma_side
                                    + cc * chunk_chroma_w
                                    + uc * cu_tx;
                                dst[start..start + cu_tx]
                                    .copy_from_slice(&cu_grid[rr * cu_tx..][..cu_tx]);
                            }
                        }
                    }
                }
            }
        }
    }
    // lane-av1-422k: the chroma PREDICTION shape is the plane block
    // (`side >> ss_x`, `side >> ss_y`) -- square at 4:2:0/4:4:4, a
    // (side/2, side) rect at 4:2:2. `chroma_side` (the 422j enclosing
    // square of that rect) is the right size for BOOKKEEPING buffers but
    // the wrong SHAPE to predict and reconstruct at: an intra-in-inter
    // block's chroma prediction indexes its CFL signal and residual
    // `row * chroma_w + col` over the plane block, and a 16x16 luma
    // block's 8x16 chroma CFL signal is 128 samples -- predicting at the
    // 16x16 square read `ac[128]` out of it (frame 1, px=192 py=96).
    let chroma_w = side >> ss_x(fctx);
    let chroma_h = side >> ss_y(fctx);
    if side > 64 {
        // Chroma is already coded and reconstructed per mu chunk above.
        luma_grid = Grid::Zero(side * side);
        u_grid = if u_units.is_empty() {
            Grid::Zero(chroma_stride * chroma_buf_h)
        } else {
            Grid::Own(std::mem::take(&mut u_units))
        };
        v_grid = if v_units.is_empty() {
            Grid::Zero(chroma_stride * chroma_buf_h)
        } else {
            Grid::Own(std::mem::take(&mut v_units))
        };
        hit!(INTRA_128_IN_INTER_HITS);
    } else if skip {
        if !split_luma {
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
                side,
                mode,
                angle_delta_y,
                reach,
                &ZERO_RESIDUAL[..side * side],
                None,
                filter_intra,
                smooth_neighbor,
                fctx,
            );
        }
        let ac = alpha.map(|_| cfl_src(px, py, side));
        if let Some((ub, _)) = &palette_uv_bufs {
            set_palette_pred(ub.clone(), fctx);
        }
        push_intra_unit(
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
        push_intra_unit(
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
        luma_grid = Grid::Zero(side * side);
        u_grid = Grid::Zero(chroma_w * chroma_h);
        v_grid = Grid::Zero(chroma_w * chroma_h);
    } else {
        let around = neighbours.around_mi(at, side);
        if split_luma {
            // Already coded and reconstructed unit by unit above; the
            // block-level grid is only the neighbour write-back's input,
            // which `record_split_luma` ignores for plane 0.
            luma_grid = Grid::Zero(side * side);
        } else {
            if let Some(buf) = &palette_y_buf {
                set_palette_pred(buf.clone(), fctx);
            }
            luma_grid = read_plane(
                dec,
                cdfs,
                luma_set_intra,
                scan_luma,
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
                side,
                luma_tx,
                base_q_idx,
                None,
                filter_intra,
                None,
                smooth_neighbor,
                fctx,
            )?;
        }
        let ac = alpha.map(|_| cfl_src(px, py, side));
        if lossless(fctx) {
            // lane-lossless2: the block's chroma plane is a raster of 4x4
            // units, plane-major (`read_intra_chroma_lossless`).
            let (mut uo, mut vo) = (Vec::new(), Vec::new());
            read_intra_chroma_lossless(
                y,
                u,
                v,
                dec,
                cdfs,
                neighbours,
                at_mi,
                (px, py),
                (cpx, cpy),
                side,
                // lane-av1422lpf site 6 (the whole-block twin of the mu
                // chunk fix at 44374): the plane block's own per-axis shape
                // and its own stride, never the enclosing square
                // `chroma_side`. At 4:2:0 and 4:4:4 all three are the same
                // number.
                (chroma_stride, chroma_buf_h),
                (0, 0),
                (chroma_stride, chroma_buf_h),
                chroma_stride,
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
            let zero = chroma_side * chroma_side;
            u_grid = if uo.is_empty() {
                Grid::Zero(zero)
            } else {
                Grid::Own(uo)
            };
            v_grid = if vo.is_empty() {
                Grid::Zero(zero)
            } else {
                Grid::Own(vo)
            };
            mu_chroma = true;
        } else if ss_x(fctx) == 0 && ss_y(fctx) == 0 && chroma_side > chroma_tx {
            // lane-av1-rect128recon: at ss 0/0 the uv plane block of a
            // 64x64 intra block inside an inter frame is 64x64 while
            // `av1_get_max_uv_txsize` caps the unit at TX_32X32 -- FOUR
            // plane-major TX_32X32 units (libaom
            // `decode_token_recon_block`'s plane loop), each with its own
            // neighbour context and `get_txb_ctx`'s offset-10 rows (+3 on
            // the `txb_skip_chroma_32` table, lane-sb128b r3), each
            // stamped immediately so the next unit reads it. The old arm
            // read ONE whole-plane unit with the block-level context and
            // no offset: three quarters of the plane's residual was never
            // read, the unit's `txb_skip` came off the offset-7 row (444
            // witness f2, right-half intra 64x64: U unit (0,0) ctx 0
            // where aom reads 10), and the coded corner was even
            // dequantized at the TX_64X64 dqDenom.
            let mu_units_n = chroma_side / chroma_tx;
            let mut u_acc = vec![0i32; chroma_side * chroma_side];
            let mut v_acc = vec![0i32; chroma_side * chroma_side];
            for plane_idx in 1..=2 {
                for cu_row in 0..mu_units_n {
                    for cu_col in 0..mu_units_n {
                        let cu_mi = (
                            at_mi.0 + cu_row * (chroma_tx / MI),
                            at_mi.1 + cu_col * (chroma_tx / MI),
                        );
                        let cu_around = neighbours.around_mi(cu_mi, chroma_tx);
                        let cu_reach = crate::decode::tu_reach(
                            side,
                            side,
                            cu_col * chroma_tx,
                            cu_row * chroma_tx,
                            chroma_tx,
                            reach,
                            px,
                            py,
                            y.width,
                            y.height,
                            fctx,
                        );
                        let cu_grid = read_plane(
                            dec,
                            cdfs,
                            chroma_set,
                            scan_chroma,
                            plane_idx,
                            cu_around[plane_idx],
                            mode,
                            uv_predict_mode,
                            angle_delta_uv,
                            cu_reach,
                            y,
                            u,
                            v,
                            cpx + cu_col * chroma_tx,
                            cpy + cu_row * chroma_tx,
                            chroma_tx,
                            chroma_tx,
                            base_q_idx,
                            // CfL is capped at 32x32 (`is_cfl_allowed`),
                            // so a block this size never carries alpha.
                            None,
                            None,
                            Some(3),
                            smooth_neighbor_uv,
                            fctx,
                        )?;
                        // Immediately, so the NEXT unit reads this one's
                        // coefficient context.
                        neighbours
                            .record_mi_chroma(cu_mi, chroma_tx, chroma_tx, plane_idx, &cu_grid);
                        hit!(CHROMA_SPLIT_TX_HITS);
                        let acc = if plane_idx == 1 {
                            &mut u_acc
                        } else {
                            &mut v_acc
                        };
                        for rr in 0..chroma_tx {
                            let start =
                                (cu_row * chroma_tx + rr) * chroma_side + cu_col * chroma_tx;
                            acc[start..start + chroma_tx]
                                .copy_from_slice(&cu_grid[rr * chroma_tx..][..chroma_tx]);
                        }
                    }
                }
            }
            mu_chroma = true;
            u_grid = Grid::Own(u_acc);
            v_grid = Grid::Own(v_acc);
        } else {
            // lane-av1-422k: the chroma plane block is (`chroma_w`,
            // `chroma_h`) -- the square read above at 4:2:0/4:4:4, one rect
            // TX_{side/2}X{side} unit through [`read_rect_chroma_unit`] at
            // 4:2:2 (the key-frame path's proven set/scan rows, and the
            // per-chroma-cell context read [`around_mi_422_chroma`]).
            let chroma_around = if chroma_w == chroma_h {
                around
            } else {
                neighbours.around_mi_422_chroma(at, side, side)
            };
            if let Some((ub, _)) = &palette_uv_bufs {
                set_palette_pred(ub.clone(), fctx);
            }
            u_grid = if chroma_w == chroma_h {
                read_plane(
                    dec,
                    cdfs,
                    chroma_set,
                    scan_chroma,
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
                    chroma_side,
                    chroma_tx,
                    base_q_idx,
                    alpha.zip(ac).map(|((au, _), ac)| (au, ac)),
                    None,
                    None,
                    smooth_neighbor_uv,
                    fctx,
                )?
            } else if side == 64 {
                // lane-av1-422inter: `av1_get_max_uv_txsize(BLOCK_64X64)` at
                // ss (1,0) adjusts `max_txsize_rect_lookup[BLOCK_32X64]` =
                // TX_32X64 down to TX_32X32 (`av1_get_adjusted_tx_size`), so
                // the (side/2, side) = (32, 64) plane block codes TWO stacked
                // SQUARE TX_32X32 units per plane, plane-major -- the
                // key-frame path's own model (`decode_block`'s 4:2:2
                // chroma_tx table, "64 and 128: TX_32X64 / TX_64X64 adjust
                // down to TX_32X32", proven byte-exact on this fixture's
                // frame 0) and the twin of this function's own side>64
                // mu-chunk walk above. The whole-plane-block rect read this
                // arm replaced handed [`read_coeffs_rect`] a 36x68 levels
                // frame it never had (the frame-9 panic) and would have read
                // a full TX_32X64 scan libaom never codes.
                let cu_scan = default_scan(32);
                let (cu_w, cu_h) = (32usize, 32usize);
                let luma_span_x = cu_w << ss_x(fctx);
                let luma_span_y = cu_h << ss_y(fctx);
                let mut assembled = vec![0i32; chroma_side * chroma_side];
                for cu_row in 0..chroma_h / cu_h {
                    let cu_mi = (at_mi.0 + cu_row * (luma_span_y / MI), at_mi.1);
                    let cu_around =
                        neighbours.around_mi_422_chroma(cu_mi, luma_span_x, luma_span_y);
                    let cu_reach = tu_reach_rect(
                        side,
                        side,
                        0,
                        cu_row * luma_span_y,
                        luma_span_x,
                        luma_span_y,
                        reach,
                        px,
                        py,
                        y.width,
                        y.height,
                        fctx,
                    );
                    // The 4:2:2 plane block is `(chroma_w, chroma_h)` --
                    // 32x64 -- but codes TWO stacked TX_32X32 units, so
                    // each unit needs its OWN window on the block-sized
                    // chroma palette prediction. Armed once above, the
                    // first unit consumed the whole 2048-entry buffer and
                    // the second found none pending, silently falling back
                    // to the edge DC prediction (s422_384x240 decode
                    // frame 1, U/V rows 170..189 cols 23..31).
                    if let Some((ub, _)) = &palette_uv_bufs {
                        set_palette_pred(
                            palette_window(ub, chroma_w, 0, cu_row * cu_h, cu_w, cu_h),
                            fctx,
                        );
                    }
                    hit!(PALETTE_422_UNIT_WINDOW_HITS);
                    let cu_grid = read_plane(
                        dec,
                        cdfs,
                        TxbSet::Chroma32,
                        cu_scan,
                        1,
                        cu_around[1],
                        mode,
                        uv_predict_mode,
                        angle_delta_uv,
                        cu_reach,
                        y,
                        u,
                        v,
                        cpx,
                        cpy + cu_row * cu_h,
                        cu_w,
                        cu_w,
                        base_q_idx,
                        alpha.zip(ac).map(|((au, _), ac)| (au, ac)),
                        None,
                        // The uv plane block (32x64) is larger than the unit
                        // (32x32): `get_txb_ctx`'s offset-10 rows, +3 on the
                        // `txb_skip_chroma_32` table (lane-sb128b r3).
                        Some(3),
                        smooth_neighbor_uv,
                        fctx,
                    )?;
                    hit!(CHROMA_SPLIT_TX_HITS);
                    hit!(CHROMA422_SQUARE_HITS);
                    // Immediately, so the next unit of this plane reads this
                    // one's coefficient context; the whole-block record below
                    // re-stamps the block over these and the tail's
                    // `mu_chroma_units` replay puts the per-unit state back
                    // (lane-dpm1).
                    neighbours.record_mi_chroma(cu_mi, luma_span_x, luma_span_y, 1, &cu_grid);
                    for rr in 0..cu_h {
                        let start = (cu_row * cu_h + rr) * chroma_side;
                        assembled[start..start + cu_w]
                            .copy_from_slice(&cu_grid[rr * cu_w..][..cu_w]);
                    }
                }
                mu_chroma = true;
                Grid::Own(assembled)
            } else {
                let (rect_set, rect_scan): (TxbSet, &[u16]) = match side {
                    4 => (TxbSet::Chroma4, default_scan(4)),
                    8 => (TxbSet::ChromaRect8x4, &SCAN_4X8[..]),
                    16 => (TxbSet::ChromaRect16x8, &SCAN_8X16[..]),
                    32 => (TxbSet::ChromaRect32x16, &SCAN_16X32[..]),
                    _ => (TxbSet::Chroma32, default_scan(32)),
                };
                read_rect_chroma_unit(
                    dec,
                    cdfs,
                    rect_set,
                    rect_scan,
                    chroma_w,
                    chroma_h,
                    1,
                    chroma_around[1],
                    uv_predict_mode,
                    angle_delta_uv,
                    reach,
                    y,
                    u,
                    v,
                    cpx,
                    cpy,
                    base_q_idx,
                    None,
                    alpha.zip(ac).map(|((au, _), ac)| (au, ac)),
                    smooth_neighbor_uv,
                    fctx,
                )?
            };
            if let Some((_, vb)) = &palette_uv_bufs {
                set_palette_pred(vb.clone(), fctx);
            }
            v_grid = if chroma_w == chroma_h {
                read_plane(
                    dec,
                    cdfs,
                    chroma_set,
                    scan_chroma,
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
                    chroma_side,
                    chroma_tx,
                    base_q_idx,
                    alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
                    None,
                    None,
                    smooth_neighbor_uv,
                    fctx,
                )?
            } else if side == 64 {
                // lane-av1-422inter: plane 2 of the TX_32X32-adjusted walk
                // above -- same two stacked square units, plane-major (every
                // U unit, then every V unit, libaom `decode_token_recon_block`'s
                // per-mu-chunk plane loop).
                let cu_scan = default_scan(32);
                let (cu_w, cu_h) = (32usize, 32usize);
                let luma_span_x = cu_w << ss_x(fctx);
                let luma_span_y = cu_h << ss_y(fctx);
                let mut assembled = vec![0i32; chroma_side * chroma_side];
                for cu_row in 0..chroma_h / cu_h {
                    let cu_mi = (at_mi.0 + cu_row * (luma_span_y / MI), at_mi.1);
                    let cu_around =
                        neighbours.around_mi_422_chroma(cu_mi, luma_span_x, luma_span_y);
                    let cu_reach = tu_reach_rect(
                        side,
                        side,
                        0,
                        cu_row * luma_span_y,
                        luma_span_x,
                        luma_span_y,
                        reach,
                        px,
                        py,
                        y.width,
                        y.height,
                        fctx,
                    );
                    // The V twin of the U arm's per-unit window above.
                    if let Some((_, vb)) = &palette_uv_bufs {
                        set_palette_pred(
                            palette_window(vb, chroma_w, 0, cu_row * cu_h, cu_w, cu_h),
                            fctx,
                        );
                    }
                    hit!(PALETTE_422_UNIT_WINDOW_HITS);
                    let cu_grid = read_plane(
                        dec,
                        cdfs,
                        TxbSet::Chroma32,
                        cu_scan,
                        2,
                        cu_around[2],
                        mode,
                        uv_predict_mode,
                        angle_delta_uv,
                        cu_reach,
                        y,
                        u,
                        v,
                        cpx,
                        cpy + cu_row * cu_h,
                        cu_w,
                        cu_w,
                        base_q_idx,
                        alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
                        None,
                        Some(3),
                        smooth_neighbor_uv,
                        fctx,
                    )?;
                    hit!(CHROMA_SPLIT_TX_HITS);
                    hit!(CHROMA422_SQUARE_HITS);
                    neighbours.record_mi_chroma(cu_mi, luma_span_x, luma_span_y, 2, &cu_grid);
                    for rr in 0..cu_h {
                        let start = (cu_row * cu_h + rr) * chroma_side;
                        assembled[start..start + cu_w]
                            .copy_from_slice(&cu_grid[rr * cu_w..][..cu_w]);
                    }
                }
                mu_chroma = true;
                Grid::Own(assembled)
            } else {
                let (rect_set, rect_scan): (TxbSet, &[u16]) = match side {
                    4 => (TxbSet::Chroma4, default_scan(4)),
                    8 => (TxbSet::ChromaRect8x4, &SCAN_4X8[..]),
                    16 => (TxbSet::ChromaRect16x8, &SCAN_8X16[..]),
                    32 => (TxbSet::ChromaRect32x16, &SCAN_16X32[..]),
                    _ => (TxbSet::Chroma32, default_scan(32)),
                };
                read_rect_chroma_unit(
                    dec,
                    cdfs,
                    rect_set,
                    rect_scan,
                    chroma_w,
                    chroma_h,
                    2,
                    chroma_around[2],
                    uv_predict_mode,
                    angle_delta_uv,
                    reach,
                    y,
                    u,
                    v,
                    cpx,
                    cpy,
                    base_q_idx,
                    None,
                    alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
                    smooth_neighbor_uv,
                    fctx,
                )?
            };
        }
    }
    Ok(Some((
        mode_for_tx,
        uv_predict_mode,
        block_filter,
        ref_frame_for_lf,
        globalmv_for_lf,
        luma_grid,
        u_grid,
        v_grid,
        vartx_leaves,
        palette_record,
        mu_chroma,
    )))
}

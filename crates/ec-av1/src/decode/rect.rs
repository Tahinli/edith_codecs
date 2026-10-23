//! Rect block decoders: intra mode for rects, rect split, the intrabc rects and decode_leaf_rect. //! av1split: moved out of `decode/mod.rs`; the items are unchanged apart from visibility. (D19)

use super::*;

/// [`read_intra_mode`] for a true `bw`x`bh` rect strip (lane-intradisp r1,
/// palette wired lane-palette2 r4): identical except the `use_filter_intra`
/// size class comes from [`filter_intra_size_class_rect`] instead of the
/// square-only [`filter_intra_size_class`], and the palette read (when
/// `allow_screen_content_tools`) uses [`palette_bsize_ctx_wh`]/
/// [`decode_color_index_map_wh`] instead of their square-only counterparts --
/// every other symbol this reads (`skip`, `y_mode`, `angle_delta`, `uv_mode`,
/// `cfl_alphas`) is indexed by mode or neighbour state, never by block size.
#[allow(clippy::too_many_arguments)]
pub(super) fn read_intra_mode_rect(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    above_mode: usize,
    left_mode: usize,
    cfl: bool,
    // lane-r14: `is_chroma_reference` -- false only for a strip one mi tall
    // (or wide) that is NOT the odd one of its 4:2:0 pair, where libaom reads
    // no `uv_mode`/`cfl_alpha`/`angle_delta_uv`/`palette_uv` at all (the pair's
    // chroma is coded once, on the closing strip). Every other caller is a
    // block big enough to own its chroma and passes `true`.
    has_chroma: bool,
    bw: usize,
    bh: usize,
    enable_filter_intra: bool,
    skip_ctx: usize,
    allow_screen_content_tools: bool,
    // As [`read_intra_mode`]'s own `palette`/`palette_uv_cache` params:
    // `Some((palette_mode_ctx, color_cache))` reconstructs a real palette-Y
    // block here, `None` still reads (and refuses on use) the symbol at ctx 0.
    palette: Option<(usize, &[u16])>,
    palette_uv_cache: &[u16],
    mi_r: usize,
    mi_c: usize,
    fctx: &crate::decode::FrameCtx,
) -> Result<(
    bool,
    usize,
    i32,
    usize,
    i32,
    Option<(i32, i32)>,
    Option<usize>,
    Option<PaletteY>,
    Option<PaletteUv>,
    // lane-av1-intrabc: the block vector, `Some` exactly when this block's
    // `use_intrabc` symbol came out 1 -- libaom's `read_intrabc_info` returns
    // straight out of `read_intra_frame_mode_info` then, so every later field
    // of the tuple is at its default (mode/uv_mode DC, no angle, no palette,
    // no filter intra). A caller that can reconstruct intrabc at its shape
    // takes this and calls [`decode_intrabc_rect`]; one that cannot refuses by
    // name with the same string it always did.
    Option<(i32, i32)>,
)> {
    // lane-av1mono: libaom guards every chroma symbol read with
    // `!cm->seq_params->monochrome && xd->is_chroma_ref` (`decodemv.c:933`),
    // so a monochrome block codes no `uv_mode`/`cfl`/`angle_delta_uv`/
    // `palette_uv` even on a chroma-reference strip. Fold the plane count
    // into the same `has_chroma` this reader already gates on.
    let has_chroma = has_chroma && !mono(fctx);
    crate::msac::SymbolDecoder::set_symr_mi(mi_r as i64, mi_c as i64);
    let ec_istep = crate::envflags::env_flag!("EC_TRACE_MODE_STEP");
    if ec_istep {
        // Mirrors the oracle's own `EC_IMODE mi_row=.. mi_col=.. rng=..` line
        // (decodemv.c:818), printed at the same point: before the first
        // mode-info symbol of the block, so a cross-decoder range ladder can
        // be keyed on the BLOCK, not just on the symbol sequence.
        eprintln!(
            "EC_IMODE mi_row={mi_r} mi_col={mi_c} fn=rect bw={bw} bh={bh} efi={} cls={:?} rng={}",
            enable_filter_intra as i32,
            filter_intra_size_class_rect(bw, bh),
            dec.debug_state().0
        );
    }
    macro_rules! istep {
        ($name:literal, $val:expr) => {
            if ec_istep {
                let (rng, _) = dec.debug_state();
                eprintln!(
                    "EC_ISTEP mi_row={mi_r} mi_col={mi_c} name={} val={} rng={rng}",
                    $name, $val
                );
            }
        };
    }
    // lane-sbrect10 r1: libaom `is_cfl_allowed` (blockd.h) is
    // `block_size_wide <= 32 && block_size_high <= 32` -- enforce it HERE, at
    // the one point every rect caller routes through, so no call site can
    // offer the 14-symbol CFL alphabet on a 64-axis block (class
    // wrong-alphabet-same-value: same value, wrong interval, silent desync).
    let cfl = if cfl && bw.max(bh) > 32 {
        hit!(NOCFL_UV_MODE_HITS);
        false
    } else {
        cfl
    };
    // lane-intra14 r1: on the INTER block path everything up to and including
    // `is_inter` was already read by [`decode_inter_block`], so this reader
    // starts at the mode syntax with the `skip` that caller read.
    let inter = fctx.intra_in_inter_mode.with(std::cell::Cell::get);
    let skip = if let Some((_, skip)) = inter {
        skip
    } else {
        // Same `intra_segment_id` placement as the square reader above.
        let (seg_w_mi, seg_h_mi) = (bw / 4, bh / 4);
        if seg_id_pre_skip(fctx) {
            intra_segment_id(dec, cdfs, mi_r, mi_c, seg_w_mi, seg_h_mi, false, fctx);
        }
        let skip = dec.symbol(&mut cdfs.skip[skip_ctx]) != 0;
        istep!("skip", skip as i32);
        if !seg_id_pre_skip(fctx) {
            intra_segment_id(dec, cdfs, mi_r, mi_c, seg_w_mi, seg_h_mi, skip, fctx);
        }
        maybe_read_cdef_idx(dec, mi_r, mi_c, (bw / MI, bh / MI), skip, fctx);
        istep!("cdef", 0);
        // A HORZ/VERT rect strip is never the whole superblock (`bw`/`bh` never
        // both 64 here -- see this fn's own doc), so `is_whole_sb` is always
        // `false`.
        maybe_read_delta_q(dec, cdfs, mi_r, mi_c, false, skip, fctx);
        maybe_read_delta_lf(dec, cdfs, mi_r, mi_c, false, skip, fctx);
        istep!("dq", 0);
        // `read_intrabc_info` (spec 5.11.13, libaom `decodemv.c:892`, right after
        // `skip`/`segment_id`/`cdef`/`delta_q` and before `mbmi->mode`): the
        // `use_intrabc` symbol is read for EVERY intra block of an `allow_intrabc`
        // frame -- `av1_allow_intrabc` is a frame-level test with no shape term, so
        // rect strips read it exactly like square leaves ([`read_intra_mode`],
        // [`read_intra_mode_sub8`]). Missing it here desynced the arithmetic
        // decoder at the FIRST symbol of the first rect block of every
        // `--tune-content=screen` key frame. Reconstruction of a real intrabc
        // block at this shape is out of scope, so the flag is consumed and
        // refused by name when it fires.
        if allow_intrabc_frame(fctx) {
            let use_intrabc = dec.symbol(&mut cdfs.intrabc) != 0;
            hit!(RECT_INTRABC_READS);
            istep!("intrabc", use_intrabc as i32);
            if use_intrabc {
                hit!(INTRABC_HITS);
                // libaom `read_intrabc_info`: the DV read, then an immediate
                // return out of the mode-info read -- no `y_mode`, no `uv_mode`,
                // no angle deltas, no palette, no filter intra. The caller owns
                // the reconstruction at its own shape.
                let dv = read_intrabc_dv(dec, cdfs, mi_r, mi_c, (bw / MI, bh / MI), fctx);
                return Ok((
                    skip,
                    DC_PRED,
                    0,
                    DC_PRED,
                    0,
                    None,
                    None,
                    None,
                    None,
                    Some(dv),
                ));
            }
        }
        skip
    };
    // libaom `read_intra_block_mode_info` (decodemv.c:1189) vs the key
    // frame's `read_intra_frame_mode_info` (decodemv.c:1065 `read_intra_mode`
    // off `kf_y_mode_cdf[above][left]`).
    let mode = if let Some((size_group, _)) = inter {
        crate::msac::SymbolDecoder::set_symr_cdf("y_mode");
        dec.symbol(&mut cdfs.y_mode[size_group])
    } else {
        let above_ctx = INTRA_MODE_CTX[above_mode];
        let left_ctx = INTRA_MODE_CTX[left_mode];
        dec.symbol(&mut cdfs.kf_y_mode[above_ctx][left_ctx])
    };
    istep!("mode", mode as i32);
    let angle_delta_y = if (V_PRED..=D67_PRED).contains(&mode) {
        read_angle_delta(dec, &mut cdfs.angle_delta[mode - V_PRED])
    } else {
        0
    };
    istep!("angle_y", angle_delta_y);
    // lane-sb128c r3 (class sweep): libaom `read_intra_mode_uv(.., is_cfl_allowed(xd), ..)`
    // and `is_cfl_allowed` (blockd.h) is `block_size_wide[bsize] <= 32 &&
    // block_size_high[bsize] <= 32` on the LUMA bsize. Deciding the alphabet
    // here, from the block's own size, is the single point every caller routes
    // through -- a caller passing `true` for a >32 strip used to pick the
    // 14-symbol CFL CDF, same value but a different range (class
    // wrong-alphabet-same-value).
    let cfl = cfl && cfl_allowed_px(bw, bh, fctx);
    let uv_mode = if !has_chroma {
        DC_PRED
    } else if cfl {
        crate::msac::SymbolDecoder::set_symr_cdf("uv_mode");
        dec.symbol(&mut cdfs.uv_mode_cfl[mode])
    } else {
        crate::msac::SymbolDecoder::set_symr_cdf("uv_mode");
        dec.symbol(&mut cdfs.uv_mode_no_cfl[mode])
    };
    istep!("uv_mode", uv_mode as i32);
    let alpha = if has_chroma && cfl && uv_mode == UV_CFL_PRED {
        Some(read_cfl_alphas(dec, cdfs))
    } else {
        None
    };
    if has_chroma && (9..=12).contains(&uv_mode) {
        hit!(SMOOTH_UV_HITS);
    }
    let angle_delta_uv = if has_chroma && (V_PRED..=D67_PRED).contains(&uv_mode) {
        hit!(DIRECTIONAL_UV_HITS);
        read_angle_delta(dec, &mut cdfs.angle_delta[uv_mode - V_PRED])
    } else {
        0
    };
    istep!("angle_uv", angle_delta_uv);
    if angle_delta_uv != 0 {
        hit!(UV_ANGLE_DELTA_HITS);
    }
    // `read_palette_mode_info` (spec 5.11.13), rect-generalised
    // (lane-palette2 r4): same bsize-ctx-gated read as [`read_intra_mode`],
    // just keyed by [`palette_bsize_ctx_wh`] instead of the square-only
    // [`palette_bsize_ctx`]. Order matches spec/libaom exactly: mode/size/
    // colours only here, the colour-index maps deferred past `filter_intra`
    // (see [`read_intra_mode`]'s own comment on why -- `av1_visit_palette`
    // runs from the caller, after the whole mode-info read).
    let mut palette_y_pending: Option<(usize, [u16; 8])> = None;
    let mut palette_uv_pending: Option<(usize, [u16; 8], [u16; 8])> = None;
    if allow_screen_content_tools && let Some(bsize_ctx) = palette_bsize_ctx_wh(bw, bh) {
        // r6: this is the exact site the removed blanket refusal used to
        // guard -- reached for EVERY rect strip in a screen-content frame,
        // independent of whether the block ends up using a palette.
        hit!(RECT_SCREEN_CONTENT_HITS);
        let (mode_ctx, cache) = palette.unwrap_or((0, &[]));
        let use_palette_y =
            mode == DC_PRED && dec.symbol(&mut cdfs.palette_y_mode[bsize_ctx][mode_ctx]) != 0;
        if use_palette_y {
            let n = 2 + dec.symbol(&mut cdfs.palette_y_size[bsize_ctx]);
            let colors = read_palette_colors_y(dec, n, cache, fctx);
            palette_y_pending = Some((n, colors));
        }
        let palette_uv_mode_ctx = usize::from(use_palette_y);
        // `read_palette_mode_info`'s own `xd->is_chroma_ref` guard.
        if has_chroma
            && uv_mode == DC_PRED
            && dec.symbol(&mut cdfs.palette_uv_mode[palette_uv_mode_ctx]) != 0
        {
            let n = 2 + dec.symbol(&mut cdfs.palette_uv_size[bsize_ctx]);
            let (u_colors, v_colors) = read_palette_colors_uv(dec, n, palette_uv_cache, fctx);
            palette_uv_pending = Some((n, u_colors, v_colors));
        }
    }
    let mut filter_intra = None;
    if crate::envflags::env_flag!("EC_AV1_TRACE") {
        let (rng, _) = dec.debug_state();
        eprintln!(
            "TRACE_RECT_PREFI mode={mode} uv_mode={uv_mode} enable_filter_intra={} \
             class={:?} rng={rng}",
            enable_filter_intra as i32,
            filter_intra_size_class_rect(bw, bh)
        );
    }
    // As the square path: `av1_filter_intra_allowed` excludes a palette-Y
    // block (reconintra.h:77), so no `use_filter_intra` symbol exists there.
    if mode == DC_PRED
        && enable_filter_intra
        && palette_y_pending.is_none()
        && let Some(class) = filter_intra_size_class_rect(bw, bh)
    {
        let use_filter_intra = dec.symbol(&mut cdfs.filter_intra[class]) != 0;
        if crate::envflags::env_flag!("EC_AV1_TRACE") {
            eprintln!("TRACE_RECT_USEFI value={}", use_filter_intra as i32);
        }
        istep!("use_filter_intra", use_filter_intra as i32);
        if use_filter_intra {
            hit!(FILTER_INTRA_HITS);
            let fi_mode = dec.symbol(&mut cdfs.filter_intra_mode);
            istep!("filter_intra_mode", fi_mode as i32);
            filter_intra = Some(fi_mode);
        }
    }
    // `av1_visit_palette`: plane 0 (Y) then plane 1 (chroma, shared U/V map),
    // same order as [`read_intra_mode`]'s own square path.
    let palette_y = palette_y_pending.map(|(n, colors)| {
        let map = decode_color_index_map_wh(
            dec,
            cdfs,
            n,
            bw,
            bh,
            palette_onscreen(mi_r, mi_c, bw, bh, fctx),
            false,
        );
        hit!(PALETTE_HITS);
        hit!(PALETTE_RECT_HITS);
        PaletteY {
            size: n,
            colors,
            map,
        }
    });
    let palette_uv = palette_uv_pending.map(|(n, u_colors, v_colors)| {
        // `av1_get_plane_block_size(bsize, 1, 1)` (`ss_size_lookup`), NOT a
        // plain halving: BLOCK_16X4 subsamples to BLOCK_8X4 and BLOCK_4X16 to
        // BLOCK_4X8, both floored at the 4-px transform. Halving the short
        // axis to 2 read HALF the colour-index symbols of a 1:4 strip's
        // chroma palette and desynced the tile from there.
        let (cw, ch) = ((bw >> ss_x(fctx)).max(4), (bh >> ss_y(fctx)).max(4));
        let on = palette_onscreen_uv(
            (bw, bh),
            palette_onscreen(mi_r, mi_c, bw, bh, fctx),
            (cw, ch),
            (ss_x(fctx), ss_y(fctx)),
        );
        let map = decode_color_index_map_wh(dec, cdfs, n, cw, ch, on, true);
        hit!(PALETTE_UV_HITS);
        PaletteUv {
            size: n,
            u_colors,
            v_colors,
            map,
        }
    });
    Ok((
        skip,
        mode,
        angle_delta_y,
        uv_mode,
        angle_delta_uv,
        alpha,
        filter_intra,
        palette_y,
        palette_uv,
        None,
    ))
}

/// Stamps a 1:4 strip's palette state into the mi-granular above/left arrays,
/// at the strip's REAL mi position. `av1_get_palette_cache` reads
/// `xd->above_mbmi`/`xd->left_mbmi`, i.e. the mi cell above/left of the
/// block, so each strip owns its own rows and columns and no aliasing rule
/// is needed (lane-kf900 r6).
#[allow(clippy::too_many_arguments)]
pub(super) fn record_strip_palette(
    n: &mut Neighbours,
    mi: (usize, usize),
    bw: usize,
    bh: usize,
    y_size: usize,
    y_colors: [u16; 8],
    uv_size: usize,
    uv_colors: [u16; 8],
) {
    let (r, c) = mi;
    // lane-t900 r23 rung: every publication of a block's palette state into
    // the neighbour bands (`EC_PALPUB=1`) -- the rung that names which decode
    // path left a stale `left_palette_size` behind when
    // `av1_get_palette_mode_ctx` disagrees with libaom's.
    if crate::envflags::env_flag!("EC_PALPUB") {
        eprintln!("EC_PALPUB strip mi={r},{c} bw={bw} bh={bh} y={y_size} uv={uv_size}");
    }
    // lane-kf900 r6: the palette side bands are mi-granular now, so a strip
    // writes its OWN span on BOTH axes -- the old `last` split-axis rule only
    // existed because the 16px [`SUB`] cell held one slot for all the strips
    // of a HORZ_4/VERT_4 and leaf 0 zeroing the other axis threw away the
    // real neighbour leaf 1 reads.
    {
        for cell in 0..(bw / MI).max(1) {
            if let Some(e) = n.above_palette_size.get_mut(c + cell) {
                *e = y_size as u8;
                n.above_palette_uv_size[c + cell] = uv_size as u8;
                // lane-publish2, see [`Neighbours::record_palette_y_rect`]:
                // colours of a size-0 neighbour are never read.
                if y_size > 0 {
                    n.above_palette_colors[c + cell] = y_colors;
                }
                if uv_size > 0 {
                    n.above_palette_uv_colors[c + cell] = uv_colors;
                }
            }
        }
    }
    {
        for cell in 0..(bh / MI).max(1) {
            if n.left_palette_size.get(r + cell).is_none() {
                break;
            }
            n.left_palette_size[r + cell] = y_size as u8;
            n.left_palette_uv_size[r + cell] = uv_size as u8;
            if y_size > 0 {
                n.left_palette_colors[r + cell] = y_colors;
            }
            if uv_size > 0 {
                n.left_palette_uv_colors[r + cell] = uv_colors;
            }
        }
    }
}

/// [`cfl_ac_q3`] for a true `bw`x`bh` rect strip (lane-intradisp r1).
pub(super) fn cfl_ac_q3_rect(
    y: &PlaneBuf<'_>,
    px: usize,
    py: usize,
    bw: usize,
    bh: usize,
) -> Vec<i32> {
    cfl_ac_q3_at(px, py, bw, bh, |lx, ly| {
        i32::from(y.data[ly * y.width + lx])
    })
}

/// Everything a rect strip already read out of the stream before its luma
/// transform units are decoded -- [`decode_rect_split`]'s own mode bundle
/// (nine values that would otherwise be nine more parameters).
pub(super) struct RectStripModes {
    pub(super) skip: bool,
    pub(super) mode: usize,
    pub(super) angle_delta_y: i32,
    pub(super) uv_predict_mode: usize,
    pub(super) angle_delta_uv: i32,
    pub(super) alpha: Option<(i32, i32)>,
    pub(super) filter_intra: Option<usize>,
    pub(super) smooth_neighbor: bool,
    pub(super) smooth_neighbor_uv: bool,
    /// lane-palette2 r9: the strip's palette-Y prediction (`bw`x`bh`, already
    /// mapped through the colour table), sliced per transform unit below --
    /// palette prediction reads no edge pixels, so a unit's slice of the
    /// whole-block map IS its prediction. Without this a palette strip whose
    /// transform splits predicted DC and came out flat grey while the entropy
    /// stream stayed in sync (the coefficients are read either way).
    pub(super) palette_y: Option<Vec<u16>>,
    /// As `palette_y`, for the un-split chroma transform (U, V).
    pub(super) palette_uv: Option<(Vec<u16>, Vec<u16>)>,
}

/// The transform unit size (w, h) of a `bw`x`bh` intra block at tx `depth`
/// -- libaom `depth_to_tx_size`, which walks `sub_tx_size_map` `depth` times
/// from `max_txsize_rect_lookup[bsize]`. Every entry of that map halves the
/// LONGER side of a rectangle and both sides of a square, so a 1:4 strip's
/// first step is a 2:1 rect unit (`TX_32X8 -> TX_16X8`, `TX_64X16 ->
/// TX_32X16`) and only the second reaches a square -- the reason
/// `bw.min(bh) >> (depth - 1)` (correct for a 2:1 strip, where step one is
/// already square) mis-tiled 1:4 strips.
pub(super) fn depth_to_tx_wh(
    bw: usize,
    bh: usize,
    depth: usize,
    fctx: &crate::decode::FrameCtx,
) -> (usize, usize) {
    // lane-lossless: TX_4X4 whatever the strip's own size and depth (libaom
    // `read_tx_size`: the lossless early return comes before
    // `max_txsize_rect_lookup`), and no `tx_depth` symbol was read to get here.
    if lossless(fctx) {
        return (4, 4);
    }
    let (mut w, mut h) = (bw.min(64), bh.min(64));
    for _ in 0..depth {
        match w.cmp(&h) {
            std::cmp::Ordering::Greater => w /= 2,
            std::cmp::Ordering::Less => h /= 2,
            std::cmp::Ordering::Equal => {
                w /= 2;
                h /= 2;
            }
        }
    }
    (w, h)
}

/// lane-av1-422stripanchor: [`decode_rect_split`]'s 4-wide-strip port of the
/// split4 floored-pair anchor ([`sub8_leaf_chroma422`]'s rule). A 1-mi-wide
/// strip (luma `(4, 16)` / `(4, 8)`) has NO chroma plane block of its own at
/// ss (1, 0) -- `av1_ss_size_lookup[BLOCK_4X16][1][0]` and
/// `[BLOCK_4X8][1][0]` are both BLOCK_INVALID (`common_data.c`), and a
/// 2-px-wide area is not a transform shape. The strip pairs with its
/// even-column sibling (`is_chroma_reference`'s `mi_col & 1` column clause,
/// the rule [`decode_rect4_16_strip`] implements one partition level up), and
/// the ODD-column strip codes the PAIR's `(4, bh)` plane anchored at
/// `av1_setup_dst_planes`' mi-exact floor `(mi_col & !1) * MI >> ss_x`, with
/// the entropy context on the floored cell. Returns
/// `(pair block width, pair block height, write anchor x, entropy mi column)`
/// for that pair, or `None` when the strip keeps its own plane block at its
/// own origin -- which is every 4:2:0 and 4:4:4 cell (their chroma is never
/// 2 px wide) and every 4:2:2 strip of 8 px or more.
fn chroma422_pair_plane(
    ss_x: bool,
    ss_y: bool,
    mi_col: usize,
    bw: usize,
    bh: usize,
) -> Option<(usize, usize, usize, usize)> {
    if ss_x && !ss_y && (bw >> 1) == 2 {
        Some((4, bh, ((mi_col & !1) * MI) >> 1, mi_col & !1))
    } else {
        None
    }
}

/// Decodes the planes of one `bw`x`bh` HORZ/VERT intra strip whose luma
/// transform is SPLIT (`tx_depth != 0`) -- lane-rectsplit r1, the case every
/// rect path here refused by name until now ("per-unit rect prediction is not
/// ported").
///
/// `depth_to_tx_size` (libaom `blockd.h`) walks `sub_tx_size_map` `depth`
/// times from the block's own `max_txsize_rect_lookup` entry, and for a 2:1
/// strip the very first step already lands on a SQUARE transform
/// (`sub_tx_size_map[TX_32X16] == TX_16X16`, `[TX_64X32] == TX_32X32`, and
/// every further step halves a square) -- so each unit is square and reads
/// through the ordinary square [`read_plane`]. What is genuinely rectangular
/// here is the *tiling* (`bw/tx` by `bh/tx` units, not `n` by `n` as
/// [`decode_block`]'s square split loop assumes) and the chroma plane, whose
/// own transform is `av1_get_max_uv_txsize` of the whole strip and is never
/// split by luma's depth.
///
/// Each unit predicts off whatever the earlier units of this same block
/// already reconstructed (spec 5.11.36 `predict_and_reconstruct_intra_block`
/// inside `av1_foreach_transformed_block`, raster order), which is exactly
/// what predicting the whole strip in one shot got wrong.
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_rect_split(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    at_mi: (usize, usize),
    bw: usize,
    bh: usize,
    // The transform unit's own width and height in pixels. Square for every
    // 2:1 strip from the first split on, but a 1:4 strip's FIRST split is
    // still a 2:1 RECT unit (`sub_tx_size_map[TX_32X8] == TX_16X8`,
    // `[TX_64X16] == TX_32X16`) -- see [`depth_to_tx_wh`].
    tx_w: usize,
    tx_h: usize,
    m: &RectStripModes,
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    base_q_idx: u8,
    reduced_tx_set: bool,
    fctx: &crate::decode::FrameCtx,
) -> Result<()> {
    let (mi_r, mi_c) = at_mi;
    let (px, py) = (mi_c * MI, mi_r * MI);
    let (cpx, cpy) = (px >> ss_x(fctx), py >> ss_y(fctx));
    let (chroma_w, chroma_h) = (bw >> ss_x(fctx), bh >> ss_y(fctx));
    // lane-av1-422blockhalf: at ss (1,0) the tall/wide 1:2 and 1:4 strips'
    // chroma plane block is BLOCK_INVALID (`av1_ss_size_lookup[...][1][0]`,
    // blockd.h -- the 4:2:2 column maps BLOCK_8X16/16X32/32X64/64X128 and
    // BLOCK_8X32/16X64, plus 4-wide BLOCK_4X16, to BLOCK_INVALID), so
    // `av1_get_max_uv_txsize` indexes `max_txsize_rect_lookup` OUT OF BOUNDS
    // and the pinned Release oracle byte there is TX_4X8 (the lane-av1-422m
    // measurement). The plane is still coded -- `decode_token_recon_block`
    // steps it in that (4, 8) unit shape -- so the strip's chroma tiles into
    // 4x8 rect units, plane-major.
    let chroma_422_oob = ss_x(fctx) == 1
        && ss_y(fctx) == 0
        && matches!(
            (bw, bh),
            (8, 16) | (16, 32) | (32, 64) | (64, 128) | (8, 32) | (16, 64)
        );
    // lane-av1-422stripanchor: the 4-wide strips' PAIR chroma -- the ported
    // floored-pair anchor. `chroma422_pair_plane` is `Some` exactly when this
    // strip's own chroma block would be 2 px wide at ss (1, 0); the pair's
    // plane is then `(4, chroma_h)` written at the FLOORED cell
    // (`chroma_x`), and the skip/tiled chroma arms below tile it in (4, 8)
    // units (the OOB `max_txsize_rect_lookup[BLOCK_INVALID]` byte, TX_4X8 --
    // the same walk the (8, 32) class takes). The lossless raster arm keeps
    // the strip's own geometry: no caller of this function can present a
    // 4-wide strip yet (the enumeration test pins the five callers at
    // `8 <= min`), and the skip/tiled arms are the routes a future caller
    // needs.
    let chroma422_pair = chroma422_pair_plane(ss_x(fctx) == 1, ss_y(fctx) == 0, mi_c, bw, bh);
    let chroma_422_pair_wide = chroma422_pair.is_some();
    let (chroma_block_w, chroma_block_h, chroma_x) =
        chroma422_pair.map_or((chroma_w, chroma_h, cpx), |(w, h, x, _)| (w, h, x));
    // The chroma transform is picked before any symbol is read, so an
    // unsupported chroma shape refuses before this strip desyncs the tile.
    let chroma: Option<(TxbSet, &[u16])> = match (chroma_w, chroma_h) {
        (16, 8) => Some((TxbSet::ChromaRect16x8, &SCAN_16X8[..])),
        (8, 16) => Some((TxbSet::ChromaRect16x8, &SCAN_8X16[..])),
        (32, 16) => Some((TxbSet::ChromaRect32x16, &SCAN_32X16[..])),
        (16, 32) => Some((TxbSet::ChromaRect32x16, &SCAN_16X32[..])),
        // The 1:4 and sub-16 strips (lane-rectchroma r1 added the 32x8/8x32
        // rows, lane-rectsplitx r1 the 16x4/4x16 ones). `get_txsize_entropy_ctx`
        // squares a rect transform down to a CDF set by
        // `(wide_log2 + high_log2 + 1) >> 1`: TX_32X8 -> the 16x16 set,
        // TX_16X4 -> the 8x8 set, TX_8X4 -> the 8x8 set -- exactly the sets
        // the UNSPLIT 1:4 (`decode_block_rect4`) and 16x8 (`decode_leaf_rect`)
        // paths already prove for these same shapes.
        (32, 8) => Some((TxbSet::Chroma16, &SCAN_32X8[..])),
        (8, 32) => Some((TxbSet::Chroma16, &SCAN_8X32[..])),
        (16, 4) => Some((TxbSet::Chroma8, &SCAN_16X4[..])),
        (4, 16) => Some((TxbSet::Chroma8, &SCAN_4X16[..])),
        (8, 4) => Some((TxbSet::ChromaRect8x4, &SCAN_8X4[..])),
        (4, 8) => Some((TxbSet::ChromaRect8x4, &SCAN_4X8[..])),
        _ => None,
    };
    // A 4:4:4 64-axis strip's chroma plane block is larger than one adjusted
    // UV transform (TX_64X32 adjusts to TX_32X32). Those sizes are tiled
    // below; everything else still needs a single-unit table.
    // lane-av1-422: a 4:2:2 strip's chroma plane keeps the strip's HEIGHT and
    // halves only the width, so a rect luma strip codes a SQUARE chroma block
    // (16x8 luma -> 8x8 chroma) -- the square rows below. At 4:2:0 and 4:4:4 a
    // rect strip's chroma is itself rect, so the square rows are unreachable
    // there and cannot change their results.
    // lane-av1-422blockhalf: the tiled arms below handle every shape this
    // strip can carry (including the 4:2:2 BLOCK_INVALID planes), so the
    // diagnostic names only the shapes that would actually refuse -- it used
    // to fire for every tiled square too (the parent lane's `chroma=8x8`
    // rows) and sent the next lane chasing handled shapes.
    let chroma_tiled = chroma_422_pair_wide
        || chroma_422_oob
        || matches!(
            (chroma_w, chroma_h),
            (64, 32) | (32, 64) | (64, 16) | (16, 64) | (32, 32) | (16, 16) | (8, 8)
        );
    if !m.skip {
        if let Some(i) = match (chroma_w, chroma_h) {
            (32, 8) => Some(0),
            (8, 32) => Some(1),
            (8, 4) => Some(2),
            (4, 8) => Some(3),
            _ => None,
        } {
            hit_do! {
                RECT_SPLIT_CHROMA_SHAPE_HITS.with(|c| {
                    let mut hits = c.get();
                    hits[i] += 1;
                    c.set(hits);
                });
            }
        }
        if !m.skip && chroma.is_none() && !chroma_tiled {
            // Diagnostic for the next caller that lands here (class
            // refusal-names-a-correlate: the message names the tool, not the
            // shape); the refusal below keeps its inventory-pinned wording.
            eprintln!(
                "EC_RECTCHROMA_GAP luma={bw}x{bh} tx={tx_w}x{tx_h} \
                 chroma={chroma_w}x{chroma_h}"
            );
        }
    }
    // lane-rectsplit r2 (verifier finding): with both callers wired
    // (`decode_block_rect`'s 32x16/16x32 and `decode_block_rect64`'s
    // 64x32/32x64) every shape that reaches here has a table above, so this
    // arm is UNREACHABLE today. It stays as the shape guard for the next
    // caller -- a new strip size would otherwise hit the `expect` below and
    // panic instead of refusing by name -- and so stays in
    // `refusal_inventory::REFUSALS`.
    if !m.skip && chroma.is_none() && !chroma_tiled {
        return Err(unsupported(
            "a coded HORZ/VERT strip whose chroma transform has no rect coefficient tables here",
        ));
    }
    hit!(RECT_SPLIT_TX_HITS);
    if bw.max(bh) == 64 && bw / tx_w > 1 && bh / tx_h > 1 {
        hit!(RECT_SPLIT_SB_INTERIOR_TU_HITS);
    }
    // LUMA: `bw/tx` x `bh/tx` square transform units, raster order.
    // A square unit reads through the ordinary square [`read_plane`]; a rect
    // one (a 1:4 strip's first split) reads through [`read_coeffs_rect`] with
    // the very tables the unsplit 16x8/32x16 paths use.
    let luma_rect: Option<(TxbSet, &[u16])> = match (tx_w, tx_h) {
        (w, h) if w == h => None,
        (16, 8) | (8, 16) => Some((
            if reduced_tx_set {
                TxbSet::LumaRect16x8
            } else {
                TxbSet::LumaRect16x8Set1
            },
            if tx_w == 16 {
                &SCAN_16X8[..]
            } else {
                &SCAN_8X16[..]
            },
        )),
        (32, 16) => Some((TxbSet::LumaRect32x16, &SCAN_32X16[..])),
        (16, 32) => Some((TxbSet::LumaRect32x16, &SCAN_16X32[..])),
        // lane-rect64port r1: the un-split TX_64X32/TX_32X64 of a 64-level
        // strip, read through the `luma_64corner` arm below instead of a rect
        // table -- a 64-length axis codes only its low 32 coefficients
        // (`av1_get_max_eob` / the zero-out of `av1_get_adjusted_tx_size`), so
        // the coded unit is the 32x32 corner [`decode_block_rect64`]'s own
        // depth-0 arm already reads on the key-frame path.
        //
        // lane-sub8x4 r3: WITNESS FOUND, so this is no longer behind
        // `EC_RECT64_SPLIT`. r1's "no witness" was true only while the
        // sub-8x8 intra leaves still refused: with those lifted (r2) the
        // first 50 frame OBUs of the 10-bit 3840x1608 stream's head decode
        // all 33 frames pixel-exact against ffmpeg while this arm fires 90x
        // 64x32 + 5x 32x64 -- pinned as `fixtures/reelb_rect64_intra16x4_witness.obu`
        // and gated by `a_10bit_film_frames_with_rect64_corner_tus_decode_pixel_exact`.
        // lane-wit16x4 r1 re-measured that fixture on the merged tip: this arm
        // fires 64x32=148 32x64=123 (the 90/5 above were counted out of the
        // frame-33 desync lane-frame36 r2 fixed), all 33 frames pixel-exact.
        (64, 32) | (32, 64) => None,
        _ => {
            return Err(unsupported(format!(
                "a split intra strip whose transform unit is {tx_w}x{tx_h} (no luma \
                 coefficient tables for that shape here)"
            )));
        }
    };
    // The `None` above that is NOT the square arm.
    let luma_64corner = tx_w != tx_h;
    // lane-rectsplitx r1, MEASURED: the SQUARE-unit walk below is pixel-exact
    // on a real stream (the pinned 16x8 filter-intra fixture decodes its whole
    // frame exactly, two sub-16 split strips in it), but the RECT-unit arm --
    // a 1:4 strip's depth-1 TX_16X8/TX_32X16 -- mismatched ffmpeg on the
    // 32-level 1:4 band fixture (8-bit, cq 12, seed 42). The tables and the
    // per-unit rect walk stay wired above/below; the round that finds that
    // defect deletes this refusal and nothing else.
    let luma_set = txbset_for(tx_w.min(tx_h), reduced_tx_set);
    let luma_scan = default_scan(tx_w.min(tx_h));
    let zero_tu = vec![0i32; tx_w * tx_h];
    // The strip's OWN edge availability, the fallback for a transform unit
    // whose top-right/bottom-left pixels fall outside the strip.
    let block_reach = Reach::of_rect(bw, bh, px, py, y.width, y.height, fctx);
    for tu_row in 0..bh / tx_h {
        for tu_col in 0..bw / tx_w {
            let tu_mi = (mi_r + tu_row * (tx_h / MI), mi_c + tu_col * (tx_w / MI));
            let (tu_px, tu_py) = (px + tu_col * tx_w, py + tu_row * tx_h);
            let (col_off, row_off) = (tu_col * tx_w, tu_row * tx_h);
            // lane-hgkf r1 (ported from lane-golomb 791d87a, unmerged):
            // libaom clips `av1_foreach_transformed_block_in_plane` to
            // `max_blocks_wide/high` (`mb_to_right_edge`/`mb_to_bottom_edge`,
            // i.e. `mi_cols * 4` / `mi_rows * 4`): a transform unit whose
            // TOP-LEFT sample falls outside the frame is never coded. We read
            // coefficients for those phantom units and desynced the stream at
            // the very next symbol.
            if tu_px >= y.true_width || tu_py >= y.true_height {
                continue;
            }
            // `has_top_right`/`has_bottom_left` (libaom `reconintra.c`) for a
            // transform unit INSIDE a block, which is a different rule from
            // the standalone-block table [`Reach::of`] applies -- and the
            // reason a depth-2 superblock strip came out one sample off
            // before this: `Reach::of(tx, ..)` answered as if the unit were
            // its own block at that superblock position.
            //  * top-right: if the unit's right neighbour is still inside the
            //    strip the pixels are already reconstructed (`col_off +
            //    tx_wide < plane_bw`); otherwise only the strip's own top row
            //    of units can reach past it, through the block-level answer.
            //  * bottom-left: a unit that is not in the strip's left column
            //    has none (`col_off > 0` returns 0); inside the strip
            //    (`row_off + tx_high < plane_bh`) they are the left
            //    neighbour's, already reconstructed; the last row falls back
            //    to the block-level answer.
            let tu_reach = Reach::of_tu(bw, bh, col_off, row_off, tx_w, tx_h, block_reach);
            // The unit's own window on the block's palette prediction.
            if let Some(buf) = &m.palette_y {
                let tu_buf: Vec<u16> = (0..tx_h)
                    .flat_map(|i| {
                        let base = (row_off + i) * bw + col_off;
                        buf[base..base + tx_w].iter().copied()
                    })
                    .collect();
                set_palette_pred(tu_buf, fctx);
            }
            if m.skip {
                push_intra_rect(
                    y,
                    u,
                    v,
                    0,
                    tu_px,
                    tu_py,
                    tx_w,
                    tx_h,
                    m.mode,
                    m.angle_delta_y,
                    tu_reach,
                    &zero_tu,
                    None,
                    m.filter_intra,
                    m.smooth_neighbor,
                    fctx,
                );
                neighbours.record_mi_luma_rect(tu_mi, tx_w, tx_h, &zero_tu);
            } else if luma_64corner && luma_rect.is_none() {
                // The un-split 64-level luma transform: `read_coeffs` on the
                // 32x32 CDF set [`TxbSet::Luma64`] with `default_scan(TX32)`
                // and the real rect shape passed through for
                // `av1_nz_map_ctx_offset` -- byte for byte
                // [`decode_block_rect64`]'s depth-0 read, which the key-frame
                // 64-level strip gate already proves. `tx_type` is not coded
                // (`av1_get_ext_tx_set_type` resolves EXT_TX_SET_DCTONLY once
                // the square-up size is TX_64X64), and this unit covers its
                // whole block, so `get_txb_ctx` gives txb_skip ctx 0 flat.
                if crate::envflags::env_flag!("EC_RECT64TU") {
                    eprintln!("EC_RECT64TU mi={tu_mi:?} tx={tx_w}x{tx_h} mode={}", m.mode);
                }
                hit_do! {
                    RECT64_CORNER_TU_HITS.with(|c| {
                        let mut v = c.get();
                        v[usize::from(tx_h > tx_w)] += 1;
                        c.set(v);
                    });
                }
                let tu_around = neighbours.around_mi_rect(tu_mi, tx_w, tx_h)[0];
                let mut coding = cdfs.txb(TxbSet::Luma64, fi_tx_row(m.mode, m.filter_intra));
                let (corner, tu_tx_type) = read_coeffs(
                    dec,
                    &mut coding,
                    default_scan(TX32),
                    0,
                    dc_sign_ctx(tu_around.2),
                    TxType::DctDct,
                    Some((tx_w, tx_h)),
                )?;
                let (cw, ch) = (tx_w.min(TX32), tx_h.min(TX32));
                let mut tu_grid = vec![0i32; tx_w * tx_h];
                for row in 0..ch {
                    tu_grid[row * tx_w..][..cw].copy_from_slice(&corner[row * cw..][..cw]);
                }
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
                    m.mode,
                    m.angle_delta_y,
                    tu_reach,
                    &residual,
                    None,
                    m.filter_intra,
                    m.smooth_neighbor,
                    fctx,
                );
                neighbours.record_mi_luma_rect(tu_mi, tx_w, tx_h, &tu_grid);
            } else if let Some((rect_set, rect_scan)) = luma_rect {
                // The rect-unit twin of the square arm below: same contexts,
                // same order, through the rect coefficient reader. The
                // `tx_type` CDF row of a filter-intra block is
                // `fimode_to_intradir[filter_intra_mode]`, never the block's
                // own (always DC_PRED) mode -- [`read_plane`] applies that
                // for the square arm, so this one must too.
                let tu_around = neighbours.around_mi_rect(tu_mi, tx_w, tx_h)[0];
                // libaom `get_txb_ctx`: a LUMA transform that covers its whole
                // block (`plane_bsize == txsize_to_bsize[tx_size]`, i.e. the
                // unsplit depth-0 strip) reads txb_skip ctx 0 flat -- the
                // neighbour-magnitude table is for a unit SMALLER than its
                // block. [`decode_block_rect`]'s own unsplit read passes a
                // literal 0 for the same reason; this arm applied the table
                // unconditionally and desynced every depth-0 rect strip that
                // reached it (lane-intrarect r2: ctx 5 where aomdec read 0).
                let tu_skip_ctx = if tx_w == bw && tx_h == bh {
                    0
                } else {
                    neighbours.luma_skip_ctx_rect(tu_mi, tx_w / MI, tx_h / MI)
                };
                let mut coding = cdfs.txb(rect_set, fi_tx_row(m.mode, m.filter_intra));
                let (tu_grid, tu_tx_type) = read_coeffs_rect(
                    dec,
                    &mut coding,
                    rect_scan,
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
                    m.mode,
                    m.angle_delta_y,
                    tu_reach,
                    &residual,
                    None,
                    m.filter_intra,
                    m.smooth_neighbor,
                    fctx,
                );
                neighbours.record_mi_luma_rect(tu_mi, tx_w, tx_h, &tu_grid);
            } else {
                let tu_around = neighbours.around_mi(tu_mi, tx_w)[0];
                // This unit is smaller than the block it sits in, so
                // `txb_skip_ctx` is the neighbour-magnitude table, not the
                // lone-TU 0 (spec `get_txb_ctx_general`).
                let tu_skip_ctx = neighbours.luma_skip_ctx(tu_mi, tx_w / MI);
                let tu_grid = read_plane(
                    dec,
                    cdfs,
                    luma_set,
                    luma_scan,
                    0,
                    tu_around,
                    m.mode,
                    m.mode,
                    m.angle_delta_y,
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
                    m.filter_intra,
                    Some(tu_skip_ctx),
                    m.smooth_neighbor,
                    fctx,
                )?;
                neighbours.record_mi_luma(tu_mi, tx_w, &tu_grid);
            }
        }
    }
    // CHROMA: one un-split rect transform for the whole strip.
    // lane-lossless2: except on a lossless frame, where it is a raster of 4x4
    // units -- each stamps its own neighbour context as it is read, and the
    // whole-block `record_split_luma_rect_mi` below would overwrite all of
    // them with the LAST unit's state (class `override-slot-on-one-arm`: the
    // next block then read `dc_sign_ctx` 1 where libaom reads 0). They are
    // collected here and replayed after that record.
    let mut ll_chroma_units: Vec<((usize, usize), usize, Grid, usize, usize)> = Vec::new();
    // The chroma_tiled arm below stamps each unit's coefficient context
    // immediately (so the NEXT unit reads it), then the whole-strip
    // `record_split_luma_rect_mi` rewrites every chroma cell with ONE
    // whole-strip state and the per-unit DC signs and levels are lost
    // (class `override-slot-on-one-arm`). The units are collected here and
    // replayed after that record, the way the lossless arm's
    // `ll_chroma_units` and `decode_inter_block`'s `mu_chroma_units` tail
    // already do.
    let mut tiled_chroma_units: Vec<((usize, usize), usize, Grid, usize, usize)> = Vec::new();
    let ac = m.alpha.map(|_| {
        // lane-av1-422stripanchor: the pair unit's CfL source is the PAIR's
        // luma -- the 8-px-wide region the floored 4-px chroma block covers
        // (`av1_setup_dst_planes`' floor), never the odd strip's own 4 px.
        if chroma_422_pair_wide {
            cfl_src_rect((mi_c & !1) * MI, py, 8, bh)
        } else {
            cfl_src_rect(px, py, bw, bh)
        }
    });
    let reach = if chroma_422_pair_wide {
        // The chroma units predict off the PAIR block's edges: an odd-column
        // strip's pair starts 4 luma px to the strip's left, so the strip's
        // own answer names the wrong above/right samples (the pair16 arm's
        // `pair_reach`, one partition level up).
        Reach::of_rect(8, bh, (mi_c & !1) * MI, py, y.width, y.height, fctx)
    } else {
        block_reach
    };
    let (u_grid, v_grid) = if mono(fctx) {
        // lane-av1mono: a monochrome frame codes no chroma coefficients on
        // any block (`plane < av1_num_planes == 1`), so the whole chroma
        // section -- the skipped-strip pushes, the lossless raster and the
        // un-split `read_coeffs_rect` below -- is bypassed.
        let zero = Grid::Zero(chroma_w * chroma_h);
        (zero.clone(), zero)
    } else if m.skip {
        let zero = Grid::Zero(chroma_block_w * chroma_block_h);
        if chroma_422_pair_wide || chroma_422_oob {
            // lane-av1-422blockhalf: libaom predicts an INTRA strip's chroma
            // per transform unit even when the strip is skipped
            // (`decode_token_recon_block`'s per-TU plane walk runs before the
            // skip guard), so the BLOCK_INVALID plane predicts as (4, 8)
            // units -- one whole-plane push here would ask the intra
            // predictor for a 1:8 rect it never needs.
            // lane-av1-422stripanchor: the 4-wide strip's PAIR plane does the
            // same, over the pair's own `(chroma_block_w, chroma_block_h)`
            // grid at the floored anchor `chroma_x`.
            let (uw, uh) = (4usize, 8usize);
            for plane_idx in 1..=2 {
                if let Some((ub, vb)) = &m.palette_uv {
                    let buf = if plane_idx == 1 { ub } else { vb };
                    set_palette_pred(buf.clone(), fctx);
                }
                for cu_row in 0..chroma_block_h / uh {
                    for cu_col in 0..chroma_block_w / uw {
                        push_intra_rect(
                            y,
                            u,
                            v,
                            plane_idx,
                            chroma_x + cu_col * uw,
                            cpy + cu_row * uh,
                            uw,
                            uh,
                            m.uv_predict_mode,
                            m.angle_delta_uv,
                            reach,
                            &ZERO_RESIDUAL[..uw * uh],
                            m.alpha
                                .zip(ac)
                                .map(|((au, av), ac)| (if plane_idx == 1 { au } else { av }, ac)),
                            None,
                            m.smooth_neighbor_uv,
                            fctx,
                        );
                    }
                }
            }
        } else {
            if let Some((ub, _)) = &m.palette_uv {
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
                m.uv_predict_mode,
                m.angle_delta_uv,
                reach,
                &zero,
                m.alpha.zip(ac).map(|((au, _), ac)| (au, ac)),
                None,
                m.smooth_neighbor_uv,
                fctx,
            );
            if let Some((_, vb)) = &m.palette_uv {
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
                m.uv_predict_mode,
                m.angle_delta_uv,
                reach,
                &zero,
                m.alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
                None,
                m.smooth_neighbor_uv,
                fctx,
            );
        }
        (zero.clone(), zero)
    } else if lossless(fctx) {
        // lane-lossless: the strip's chroma plane block is coded as 4x4
        // transform units too (libaom `read_tx_size` returns TX_4X4 for every
        // plane), PLANE-MAJOR inside the block -- U's units first, then V's
        // (`decode_token_recon_block`'s mu-chunk loop, measured against
        // aomdec's `EC_TRACE_COEFF` on a `-crf 0` stream: plane=1, plane=1,
        // plane=2, plane=2 for a 16x8 strip). `get_txb_ctx` adds its `+10`
        // offset (our `+3` row base) whenever the plane block is bigger than
        // the transform.
        let scan4 = default_scan(4);
        let offset = (chroma_w > 4 || chroma_h > 4).then_some(3);
        let (cw_n, ch_n) = (chroma_w / 4, chroma_h / 4);
        // lane-av1-llband: one 4x4 chroma unit's extent in LUMA pixels -- 4
        // chroma px scaled by the plane's own subsampling. The hardcoded 8
        // was the 4:2:0 unit's span; at 4:4:4 a unit is 4x4 LUMA px (one mi
        // cell), and the 8-px step made every unit past the first column/row
        // read its `txb_skip` context at the WRONG mi (two cells over) and
        // stamp a 2-cell span with one unit's state -- the polluted above-band
        // cell behind a 16x8 leaf's U bc=2 unit of the 444 lossless key frame
        // (measured: oracle `EC_DBGCTX` reads 1-cell spans at 1-mi steps;
        // ours read `above[28..29]` where the oracle read `above[26]`).
        let span_x = 4 << ss_x(fctx);
        let span_y = 4 << ss_y(fctx);
        let mut last = [Grid::Zero(16), Grid::Zero(16)];
        for plane_idx in 1..=2 {
            for cu_row in 0..ch_n {
                for cu_col in 0..cw_n {
                    // lane-av1offtile: the CHROMA twin of the luma walk's
                    // `max_blocks_wide/high` clip (decode.rs:12516, lane-hgkf
                    // r1). libaom computes those bounds per PLANE
                    // (`max_block_wide(xd, plane_bsize, plane)`,
                    // av1_common_int.h:1565 -- `mb_to_right_edge >>
                    // (3 + pd->subsampling_x)`, then `>> MI_SIZE_LOG2`), so a
                    // chroma unit whose top-left sample falls outside the
                    // CHROMA plane is never coded. This walk counted
                    // `chroma_w / 4` x `chroma_h / 4` units off the BLOCK's
                    // own extent instead.
                    //
                    // MEASURED on `mix_176x144_6.obu` (4:2:0 8-bit,
                    // --cq-level=0 --aq-mode=1 altref; the 48x16 bottom-right
                    // sliver, block mi=(32,32) px=(128,128) bw=64 bh=32):
                    // the chroma plane is 88x72, so `cw_n` must be
                    // `(88 - 64) / 4 = 6` and `ch_n` `(72 - 64) / 4 = 2` --
                    // the oracle's own `EC_ECDUMP_IN` for that block is
                    // exactly 12 units per plane (bc 0..5 x br 0..1). Ours
                    // walked 8 x 4 = 32 per plane: 20 phantom units x 2
                    // planes = the 40 extra `txb_skip` reads after SYMR
                    // 25791, all at `mi=(32,32)`, all `all_zero`, all with
                    // the bit position FROZEN at 36288 -- reads out of a
                    // tile already exhausted, which is what turned the
                    // desync 11 970 reads later into a hard
                    // `REFUSED: unsupported: AV1 tile (a Golomb tail longer
                    // than this decoder reads)`.
                    if cpx + cu_col * 4 >= u.true_width || cpy + cu_row * 4 >= u.true_height {
                        hit!(RECT_SPLIT_LOSSLESS_CHROMA_OFFTILE_HITS);
                        continue;
                    }
                    let cu_mi = (mi_r + cu_row * (span_y / MI), mi_c + cu_col * (span_x / MI));
                    // lane-av1422llintra: at ss (1, 0) one chroma column
                    // spans TWO luma mi columns, so the plain rect walk sums
                    // its whole-unit dc sign TWICE where libaom's
                    // `get_txb_ctx_general` reads `txb_w_unit` chroma cells
                    // -- one per column. `dc_sign_ctx` only sees the vote's
                    // SIGNUM, so a doubled above vote flips the row exactly
                    // when above and left cancel (a+l = 0 -> ctx 0 becomes
                    // 2a+l = +a). Measured on the pinned 4:2:2 all-intra
                    // lossless control `ll422_allintra.obu`: the V 4x4 unit
                    // at chroma (24, 8) of the BLOCK_32X8 at mi(2,8) --
                    // oracle `EC_DBGCTX above0=23 left0=13` sums to dc 0 and
                    // reads `dc_sign_cdf[0]`; ours summed the above column
                    // twice, got vote +1, and read `dc_sign_cdf[2]`, so the
                    // DC sign symbol came out inverted and the whole tile
                    // desynced from there. Identical to the guard one arm up
                    // at 12865.
                    let cu_around = if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                        neighbours.around_mi_422_chroma(cu_mi, span_x, span_y)
                    } else {
                        neighbours.around_mi_rect(cu_mi, span_x, span_y)
                    };
                    let cu_reach = tu_reach(
                        bw,
                        bh,
                        cu_col * span_x,
                        cu_row * span_y,
                        span_x,
                        block_reach,
                        px,
                        py,
                        y.width,
                        y.height,
                        fctx,
                    );
                    let (cu_x, cu_y) = (cpx + cu_col * 4, cpy + cu_row * 4);
                    if let Some((ub, vb)) = &m.palette_uv {
                        let pbuf = if plane_idx == 1 { ub } else { vb };
                        set_palette_pred(
                            palette_window(pbuf, chroma_w, cu_col * 4, cu_row * 4, 4, 4),
                            fctx,
                        );
                    }
                    let grid = read_plane(
                        dec,
                        cdfs,
                        TxbSet::Chroma4,
                        scan4,
                        plane_idx,
                        cu_around[plane_idx],
                        m.mode,
                        m.uv_predict_mode,
                        m.angle_delta_uv,
                        cu_reach,
                        y,
                        u,
                        v,
                        cu_x,
                        cu_y,
                        4,
                        4,
                        base_q_idx,
                        m.alpha
                            .zip(ac)
                            .map(|((au, av), ac)| (if plane_idx == 1 { au } else { av }, ac)),
                        None,
                        offset,
                        m.smooth_neighbor_uv,
                        fctx,
                    )?;
                    if span_x == 4 && span_y == 4 {
                        hit!(RECT_SPLIT_LOSSLESS_CHROMA444_HITS);
                    }
                    neighbours.record_mi_chroma(cu_mi, span_x, span_y, plane_idx, &grid);
                    ll_chroma_units.push((cu_mi, plane_idx, grid.clone(), span_x, span_y));
                    last[plane_idx - 1] = grid;
                }
            }
        }
        let [ug, vg] = last;
        (ug, vg)
    } else if chroma_tiled {
        let (uw, uh, set, scan): (usize, usize, TxbSet, &[u16]) =
            if chroma_422_pair_wide || chroma_422_oob {
                // lane-av1-422blockhalf / lane-av1-422stripanchor: the
                // BLOCK_INVALID-plane chroma's (4, 8) units -- the Release
                // oracle's out-of-bounds `max_txsize_rect_lookup` byte (TX_4X8,
                // see above). For the 4-wide strip the units tile the PAIR's
                // plane, stepping from the floored anchor.
                (4, 8, TxbSet::ChromaRect8x4, &SCAN_4X8)
            } else {
                match (chroma_w, chroma_h) {
                    (64, 32) | (32, 64) | (32, 32) => (32, 32, TxbSet::Chroma32, default_scan(32)),
                    (64, 16) => (32, 16, TxbSet::ChromaRect32x16, &SCAN_32X16),
                    (16, 64) => (16, 32, TxbSet::ChromaRect32x16, &SCAN_16X32),
                    // lane-av1-422: the 4:2:2 square chroma planes (height unhalved),
                    // through the square reader like every other square chroma unit.
                    (16, 16) => (16, 16, TxbSet::Chroma16, default_scan(16)),
                    (8, 8) => (8, 8, TxbSet::Chroma8, default_scan(8)),
                    _ => unreachable!("chroma_tiled"),
                }
            };
        // lane-av1-422: the witness counter, keyed to ss (1,0) so only a 4:2:2
        // frame's square chroma units count it.
        if uw == uh && ss_x(fctx) == 1 && ss_y(fctx) == 0 {
            hit!(CHROMA422_SQUARE_HITS);
        }
        if chroma_422_pair_wide {
            hit!(CHROMA422_PAIR_WIDE_HITS);
        }
        let (nw, nh) = (chroma_block_w / uw, chroma_block_h / uh);
        // lane-av1-422stripanchor: the pair's plane block is BLOCK_INVALID
        // whatever the pair size (`num_pels_log2_lookup[255]` reads 0xff in
        // the pinned Release oracle), so its txb_skip rows always carry the
        // bigger-plane +3 (the av1422q rule the pair16 arm measures).
        let bigger = chroma_422_pair_wide || chroma_w * chroma_h > uw * uh;
        // `av1_get_ext_tx_set_type`: a chroma transform whose square-up size
        // is TX_32X32 or bigger resolves to `EXT_TX_SET_DCTONLY` for intra;
        // smaller rect units (the 4:2:2 BLOCK_INVALID-plane (4, 8)s) take the
        // mode-indexed default, the same rule the unsplit arm below applies.
        let default_tx = if uw.max(uh) >= 32 {
            TxType::DctDct
        } else {
            default_intra_tx_type(m.uv_predict_mode as u8)
        };
        let mut last = [Grid::Zero(uw * uh), Grid::Zero(uw * uh)];
        for plane_idx in 1..=2 {
            for cu_row in 0..nh {
                for cu_col in 0..nw {
                    let luma_span_x = uw << ss_x(fctx);
                    let luma_span_y = uh << ss_y(fctx);
                    let cu_mi = (
                        mi_r + cu_row * (luma_span_y / MI),
                        if chroma_422_pair_wide {
                            mi_c & !1
                        } else {
                            mi_c
                        } + cu_col * (luma_span_x / MI),
                    );
                    // lane-av1-422blockhalf: the 4:2:2 (4, 8) units read their
                    // chroma context per chroma cell
                    // ([`Neighbours::around_mi_422_chroma`], the block-8
                    // rule) -- the plain rect walk samples one chroma cell
                    // twice at ss_x 1. Every pre-existing shape keeps its own
                    // walk.
                    // lane-av1422mi24: the plain rect walk's double-count
                    // hits EVERY unit shape at ss (1,0), not just the (4, 8)
                    // one -- the (8, 8) unit of a 16x8 intra-in-inter strip
                    // read its above dc votes twice and its U dc_sign one
                    // ctx row high (t422 frame 1). Route every ss (1,0)
                    // unit through the every-second-above-cell sampling;
                    // 4:2:0/4:4:4 keep the plain walk verbatim.
                    let cu_around = if chroma_422_oob || (ss_x(fctx) == 1 && ss_y(fctx) == 0) {
                        neighbours.around_mi_422_chroma(cu_mi, luma_span_x, luma_span_y)
                    } else {
                        neighbours.around_mi_rect(cu_mi, luma_span_x, luma_span_y)
                    };
                    let (cu_x, cu_y) = (chroma_x + cu_col * uw, cpy + cu_row * uh);
                    let cfl = m.alpha.map(|(au, av)| {
                        let alpha = if plane_idx == 1 { au } else { av };
                        (
                            alpha,
                            if chroma_422_pair_wide {
                                // The unit's co-located luma is the PAIR's
                                // 8-px-wide band at the floored mi column.
                                cfl_src_rect(
                                    (mi_c & !1) * MI + cu_col * luma_span_x,
                                    py + cu_row * luma_span_y,
                                    luma_span_x,
                                    luma_span_y,
                                )
                            } else {
                                cfl_src_rect(
                                    px + cu_col * luma_span_x,
                                    py + cu_row * luma_span_y,
                                    luma_span_x,
                                    luma_span_y,
                                )
                            },
                        )
                    });
                    // lane-av1-422kf: the palette colour map is consumed per
                    // unit here like every other chroma arm (`av1_visit_palette`
                    // feeds the whole plane block; window it to this unit).
                    // Without this the square chroma rows decoded a uv-palette
                    // strip's chroma as plain DC (class same-symbols-silent-
                    // prediction: the rng stayed in sync, only the prediction
                    // was dropped).
                    if let Some((ub, vb)) = &m.palette_uv {
                        let buf = if plane_idx == 1 { ub } else { vb };
                        set_palette_pred(
                            palette_window(buf, chroma_block_w, cu_col * uw, cu_row * uh, uw, uh),
                            fctx,
                        );
                    }
                    // libaom computes `has_top_right` PER TRANSFORM UNIT (defined
                    // reconintra.c:196, called from `av1_build_intra_predictors` at
                    // reconintra.c:1847 with the txb walk's own
                    // `blk_col`/`blk_row`), never per block: the
                    // `row_off == 0` arm asks `col_off + top_right_count_unit
                    // < plane_bw_unit`, so the FIRST unit of a 2-wide chroma
                    // grid reads its above-right out of the already-reconstructed
                    // block ABOVE even when the block's own right edge is the
                    // frame's. Passing this walk's block-level `reach` gave
                    // every unit the same answer, and `PlaneBuf::edges`
                    // (decode.rs:20840) truncates the above row at
                    // `own_across` when `above_right` is false -- so a
                    // directional unit whose angle is below 90
                    // (`need_right`, intra.rs:546) predicted with a 32-sample
                    // above row where libaom used 64.
                    // MEASURED on `fixtures/r512_rect2x1_1x2_444.obu`
                    // (4:4:4 8-bit 256x256, one frame): frame 0 plane U 130
                    // samples and plane V 30 samples wrong, every one of them
                    // in the right 6 columns of the `cu=(0,0)` unit at
                    // `mi=(48,48)` `px=(192,192)` -- the unit whose above-right
                    // the block reach denied. Row0 and col0 of the prediction
                    // matched the oracle exactly and the prediction SUM did
                    // not (172662 vs 171512 on U, 125978 vs 126008 on V), so
                    // the residual read is bit-identical and the divergence is
                    // prediction-side, not entropy. Gate:
                    // `a_444_rect_strip_chroma_tiled_2x1_and_1x2_units_take_their_own_above_right_reach`.
                    let cu_reach = tu_reach_rect(
                        bw,
                        bh,
                        (cu_col * uw) << ss_x(fctx),
                        (cu_row * uh) << ss_y(fctx),
                        uw << ss_x(fctx),
                        uh << ss_y(fctx),
                        reach,
                        px,
                        py,
                        y.width,
                        y.height,
                        fctx,
                    );
                    if cu_reach.above_right != reach.above_right
                        || cu_reach.below_left != reach.below_left
                    {
                        hit!(RECT_TILED_CHROMA_UNIT_REACH_HITS);
                    }
                    if nw * nh > 1 {
                        match (nw, nh) {
                            (2, 1) => hit!(RECT_TILED_CHROMA_2X1_HITS),
                            (1, 2) => hit!(RECT_TILED_CHROMA_1X2_HITS),
                            _ => hit!(RECT_TILED_CHROMA_NXN_HITS),
                        }
                    }
                    // 32x32 does not fit read_coeffs_rect's scratch (768). The
                    // square reader is the path every other 32x32 chroma unit uses.
                    let levels = if uw == uh {
                        read_plane(
                            dec,
                            cdfs,
                            set,
                            scan,
                            plane_idx,
                            cu_around[plane_idx],
                            m.mode,
                            m.uv_predict_mode,
                            m.angle_delta_uv,
                            cu_reach,
                            y,
                            u,
                            v,
                            cu_x,
                            cu_y,
                            uw,
                            uw,
                            base_q_idx,
                            cfl,
                            None,
                            bigger.then_some(3),
                            m.smooth_neighbor_uv,
                            fctx,
                        )?
                    } else {
                        let skip_ctx = usize::from(cu_around[plane_idx].0)
                            + usize::from(cu_around[plane_idx].1)
                            + usize::from(bigger) * 3;
                        let mut coding = cdfs.txb(set, m.uv_predict_mode);
                        let (levels, tx_type) = read_coeffs_rect(
                            dec,
                            &mut coding,
                            scan,
                            uw,
                            uh,
                            skip_ctx,
                            dc_sign_ctx(cu_around[plane_idx].2),
                            default_tx,
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
                        push_intra_rect(
                            y,
                            u,
                            v,
                            plane_idx,
                            cu_x,
                            cu_y,
                            uw,
                            uh,
                            m.uv_predict_mode,
                            m.angle_delta_uv,
                            cu_reach,
                            &residual,
                            cfl,
                            None,
                            m.smooth_neighbor_uv,
                            fctx,
                        );
                        levels
                    };
                    neighbours.record_mi_chroma(
                        cu_mi,
                        luma_span_x,
                        luma_span_y,
                        plane_idx,
                        &levels,
                    );
                    tiled_chroma_units.push((
                        cu_mi,
                        plane_idx,
                        levels.clone(),
                        luma_span_x,
                        luma_span_y,
                    ));
                    last[plane_idx - 1] = levels;
                }
            }
        }
        (last[0].clone(), last[1].clone())
    } else {
        let (chroma_set, chroma_scan) = chroma.expect("refused above when None");
        // `av1_get_ext_tx_set_type`: a chroma transform whose square-up size
        // is TX_32X32 or bigger resolves to `EXT_TX_SET_DCTONLY` for intra,
        // so only the 16x8/8x16 pair takes the mode-indexed default.
        let default_tx = if chroma_w.max(chroma_h) >= 32 {
            TxType::DctDct
        } else {
            default_intra_tx_type(m.uv_predict_mode as u8)
        };
        // lane-av1422mi24: at ss (1,0) the chroma above span is `bw >> 1`
        // chroma cells spread over `bw` luma mi columns -- the per-mi gather
        // counts every chroma column's whole-unit dc sign twice (the t422
        // frame-1 mi(26,60) intra-in-inter strip's U dc_sign read one ctx
        // row high). Luma keeps the per-mi gather; the two chroma planes
        // take [`Neighbours::around_mi_422_chroma`]'s every-second
        // above-cell sampling, like every other 4:2:2 chroma gather.
        let mut around = neighbours.around_mi_rect(at_mi, bw, bh);
        if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
            let c = neighbours.around_mi_422_chroma(at_mi, bw, bh);
            around[1] = c[1];
            around[2] = c[2];
        }
        let mut grids: Vec<Grid> = Vec::with_capacity(2);
        // Index loop mirrors the spec's per-position form over several offsets; an iterator would obscure it.
        #[allow(clippy::needless_range_loop)]
        for plane_idx in 1..=2 {
            let skip_ctx = usize::from(around[plane_idx].0) + usize::from(around[plane_idx].1);
            let mut coding = cdfs.txb(chroma_set, m.uv_predict_mode);
            let (levels, tx_type) = read_coeffs_rect(
                dec,
                &mut coding,
                chroma_scan,
                chroma_w,
                chroma_h,
                skip_ctx,
                dc_sign_ctx(around[plane_idx].2),
                default_tx,
                plane_idx,
            )?;
            // The same delta_q proof the unsplit rect64 path records
            // (lane-rectsplit r4: lifting the split-transform refusal moved
            // superblock strips onto this path, so only counting there would
            // leave the delta_q gate's counter silent for them).
            if bw.max(bh) == 64 && fctx.current_q_idx.with(|c| c.get()) != i32::from(base_q_idx) {
                hit!(RECT64_QIDX_DRIFT_HITS);
            }
            let residual = dequant_and_inverse_typed_wh(
                &levels,
                chroma_w,
                chroma_h,
                crate::decode::bit_depth(fctx),
                // lane-d792: the BLOCK's quantizer index, i.e. the frame's
                // own index PLUS this block's segment `SEG_LVL_ALT_Q` --
                // reading `current_q_idx` here dropped the segment feature
                // that every other dequant site applies (class
                // `parsed-then-discarded`).
                block_q_idx(fctx),
                plane_q_delta(plane_idx, fctx).0,
                plane_q_delta(plane_idx, fctx).1,
                tx_type,
                block_iqmatrix(fctx, plane_idx, chroma_w, chroma_h, tx_type),
            );
            let cfl = m
                .alpha
                .zip(ac)
                .map(|((au, av), ac)| (if plane_idx == 1 { au } else { av }, ac));
            if let Some((ub, vb)) = &m.palette_uv {
                let buf = if plane_idx == 1 { ub } else { vb };
                set_palette_pred(buf.clone(), fctx);
            }
            push_intra_rect(
                y,
                u,
                v,
                plane_idx,
                cpx,
                cpy,
                chroma_w,
                chroma_h,
                m.uv_predict_mode,
                m.angle_delta_uv,
                reach,
                &residual,
                cfl,
                None,
                m.smooth_neighbor_uv,
                fctx,
            );
            grids.push(levels);
        }
        let mut it = grids.into_iter();
        (it.next().unwrap(), it.next().unwrap())
    };
    neighbours.record_split_luma_rect_mi(
        at_mi,
        bw,
        bh,
        m.mode,
        m.uv_predict_mode,
        [&u_grid, &v_grid],
    );
    for (cu_mi, plane, unit, span_w, span_h) in &ll_chroma_units {
        neighbours.record_mi_chroma(*cu_mi, *span_w, *span_h, *plane, unit);
    }
    for (cu_mi, plane, unit, span_w, span_h) in &tiled_chroma_units {
        neighbours.record_mi_chroma(*cu_mi, *span_w, *span_h, *plane, unit);
    }
    neighbours.fill_skip_grid_rect((mi_r, mi_c), bw / MI, bh / MI, m.skip);
    neighbours.fill_lf_grid_rect(
        (mi_r, mi_c),
        bw / MI,
        bh / MI,
        tx_w as u8,
        tx_h as u8,
        0,
        fctx,
    );
    if crate::envflags::env_flag!("EC_AV1_TRACE") {
        let (rng, _) = dec.debug_state();
        eprintln!(
            "TRACE_RECT_SPLIT mi_row={mi_r} mi_col={mi_c} bw={bw} bh={bh} \
             tx={tx_w}x{tx_h} rng={rng}"
        );
    }
    Ok(())
}

/// One intra-coded HORZ/VERT strip on the INTER block path (lane-intrarect
/// r1) -- the shape [`decode_inter_block`]'s intra arm refused by name
/// because it predicts squares only. Every pixel here goes through the rect
/// machinery the key-frame paths already prove: [`decode_rect_split`], which
/// at `depth == 0` is the unsplit strip as a single transform unit and
/// beyond it the per-unit walk of [`depth_to_tx_wh`].
///
/// What is genuinely different from [`decode_block_rect`] is the mode
/// syntax: on an inter frame an intra block reads `y_mode[size_group]` and
/// `uv_mode` (libaom `read_intra_block_mode_info`, decodemv.c:1065), never
/// the key frame's `kf_y_mode` above/left pair. This mirrors the square
/// intra-in-inter arm's own reads at the rect footprint, including its
/// inherited gaps -- no `use_filter_intra` symbol (every inter gate recipe
/// pins `--enable-filter-intra=0`; lane-fiinter owns that read for both the
/// square and this arm) and a nonzero luma angle delta refused by name.
///
/// The strip's own neighbour records (luma per transform unit, chroma, skip
/// grid, deblock grid) are [`decode_rect_split`]'s; the caller adds the
/// inter-side bands and the mi map.
#[allow(clippy::too_many_arguments)]
/// The `(size_group_lookup, bsize_to_tx_size_cat)` row an INTRA block coded
/// inside an inter frame reads its `y_mode` and `tx_depth` symbols from, for
/// the rect footprints [`decode_intra_rect_in_inter`] resolves itself (the
/// 128-root halves and the 16x4/4x16 strips of a 1:4 partition take their own
/// branches before this). libaom `common_data.h:61` / `blockd.h:1347`: the 2:1
/// strips keep the group/category diagonal (BLOCK_16X8 -> 1/1, BLOCK_32X16 ->
/// 2/2, BLOCK_64X32 -> 3/3), the 1:4 ones break it (BLOCK_32X8 is group 1 but
/// category 2, BLOCK_64X16 group 2 and category 3), which is why the row is a
/// table here rather than a shift of `bw.min(bh)`.
fn intra_in_inter_size_row(bw: usize, bh: usize) -> Option<(usize, usize)> {
    Some(match (bw.min(bh), bw.max(bh)) {
        (8, 16) => (1usize, 1usize),
        (16, 32) => (2, 2),
        (32, 64) => (3, 3),
        (8, 32) => (1, 2),
        (16, 64) => (2, 3),
        _ => return None,
    })
}

/// lane-t900 r20, refusal B ("an intra-coded {bw}x{bh} block on the inter
/// block path"): the shapes that can reach that arm, enumerated against the
/// MEASURED domain of the inter block path
/// ([`INTER_BLOCK_SHAPES`], the census the gate
/// `a_block_shape_census_over_three_real_streams_leaves_the_rect_residual_refusal_unreachable`
/// fills from three real streams).
///
/// [`decode_intra_rect_in_inter`] is called from one site, under
/// `write_w != side || write_h != side`, and only after two earlier arms have
/// taken their shapes: a footprint with a side below 8 is either a 16x4/4x16
/// strip WITH a chroma-pair record (the `strip16` branch) or refused by name,
/// and a 128-px footprint returns from the `bw.max(bh) == 128` branch. What is
/// left is every rectangular census shape with `8 <= min` and `max <= 64` --
/// and each of those has a row, so the refusal names a shape no measured
/// stream can present.
#[test]
fn every_intra_in_inter_shape_the_census_lists_has_a_size_group_row() {
    // lane-av1refusalspan: this gate is the PROOF for the refusal below, so it
    // names that string exactly (placeholders included) and pins its guard
    // site. The enumeration below establishes the shape domain; on its own it
    // never said WHICH string the domain was about, so under a real function
    // boundary -- rather than a window that ran into the next test -- the row
    // read as unanchored.
    const REFUSAL: &str = "an intra-coded {bw}x{bh} block on the inter block path (no size-group/tx-category row for that shape here)";
    assert!(
        crate::test_source::module_source("decode").contains(REFUSAL),
        "this gate proves {REFUSAL:?}, but that string is no longer in decode.rs"
    );
    let mut checked = 0;
    for &(side, w, h) in &INTER_BLOCK_SHAPES {
        if (w, h) == (side, side) || w.min(h) < 8 || w.max(h) > 64 {
            continue;
        }
        assert!(
            intra_in_inter_size_row(w, h).is_some(),
            "the inter path decodes {w}x{h} blocks, and an INTRA one of that shape would hit \
             the \"no size-group/tx-category row\" refusal"
        );
        checked += 1;
    }
    assert_eq!(
        checked, 10,
        "the block-shape census changed: {checked} rect shapes reach the intra-in-inter row \
         lookup, not the 10 this claim was measured over"
    );
}

/// The `motion_mode` / `obmc` CDF row for a block footprint: libaom's
/// `motion_mode_cdf[mbmi->bsize]`, in this crate's packed row order (which
/// `cdf::MOTION_MODE` documents -- the 128-px rows come last, so they are
/// matched before the square arm's `log2` shortcut).
///
/// `None` is the refusal "a motion_mode symbol for a block shape with no CDF
/// row here"; the enumeration below is what proves no conformant block can
/// reach it.
pub(crate) fn motion_mode_cdf_row(write_w: usize, write_h: usize) -> Option<usize> {
    Some(match (write_w, write_h) {
        // lane-sb128c r5: the 128 root's own rows come FIRST -- the square arm
        // below maps 128 to row 4 (BLOCK_16X32).
        (64, 128) => 14,
        (128, 64) => 15,
        (128, 128) => 16,
        (w, h) if w == h => (w.trailing_zeros() - 3) as usize,
        (16, 32) => 4,
        (32, 16) => 5,
        (32, 64) => 6,
        (64, 32) => 7,
        (8, 32) => 8,
        (32, 8) => 9,
        (16, 64) => 10,
        (64, 16) => 11,
        // lane-inter4 r3: the 16x16-level rect inter leaves.
        (8, 16) => 12,
        (16, 8) => 13,
        _ => return None,
    })
}

/// Every `BLOCK_SIZES_ALL` footprint, as `(width, height)` in pixels: the six
/// squares, the ten 2:1 shapes and the six 1:4 ones (libaom `common_data.h`,
/// `block_size_wide`/`block_size_high`).
#[cfg(test)]
pub(super) const BLOCK_SIZES_ALL_WH: [(usize, usize); 22] = [
    (4, 4),
    (4, 8),
    (8, 4),
    (8, 8),
    (8, 16),
    (16, 8),
    (16, 16),
    (16, 32),
    (32, 16),
    (32, 32),
    (32, 64),
    (64, 32),
    (64, 64),
    (64, 128),
    (128, 64),
    (128, 128),
    (4, 16),
    (16, 4),
    (8, 32),
    (32, 8),
    (16, 64),
    (64, 16),
];

/// lane-t900 r25, enumeration: the refusal "a motion_mode symbol for a block
/// shape with no CDF row here" names a shape no conformant stream can present.
///
/// A `motion_mode`/`obmc` symbol is read only under `motion_mode_eligible`,
/// whose `write_w.min(write_h) >= 8` clause is libaom's
/// `is_motion_variation_allowed_bsize` (`blockd.h:1455`). So the reachable
/// domain of [`motion_mode_cdf_row`] is exactly the `BLOCK_SIZES_ALL`
/// footprints with `min(bw, bh) >= 8` -- 17 of the 22 -- and each of those
/// must map to a distinct row of the 17-row `Cdfs::motion_mode` table. The
/// five it excludes (4x4, 4x8, 8x4, 4x16, 16x4) are the ones the eligibility
/// clause rejects before the lookup.
#[test]
fn every_shape_that_allows_motion_variation_has_a_motion_mode_cdf_row() {
    // lane-av1refusalspan: this gate is the PROOF for the refusal below, so it
    // names it and pins its guard site.
    const REFUSAL: &str = "a motion_mode symbol for a block shape with no CDF row here";
    assert!(
        crate::refusal_inventory::pins_refusal(
            crate::test_source::module_source("decode"),
            REFUSAL
        ),
        "this gate proves {REFUSAL:?}, but that string is no longer in decode.rs"
    );
    let mut rows = std::collections::BTreeSet::new();
    let mut excluded = 0;
    for (w, h) in BLOCK_SIZES_ALL_WH {
        if w.min(h) < 8 {
            excluded += 1;
            assert!(
                w == 4 || h == 4,
                "{w}x{h} is not one of the sub-8 shapes is_motion_variation_allowed_bsize rejects"
            );
            continue;
        }
        let row = motion_mode_cdf_row(w, h).unwrap_or_else(|| {
            panic!(
                "a {w}x{h} block passes is_motion_variation_allowed_bsize and reads a motion_mode \
                 symbol, but has no CDF row -- the refusal is a real gap, not a dead arm"
            )
        });
        assert!(
            row < 17,
            "{w}x{h} maps to motion_mode row {row}, outside the 17-row table"
        );
        assert!(rows.insert(row), "{w}x{h} shares motion_mode row {row}");
    }
    assert_eq!(excluded, 5, "the sub-8 footprint set changed");
    assert_eq!(
        rows.len(),
        17,
        "17 eligible footprints must fill all 17 rows of the motion_mode table"
    );
}

/// lane-av1-422stripanchor, format-cell census for the 4-wide-strip pair port
/// ([`chroma422_pair_plane`], [`decode_rect_split`]'s chroma routing): over
/// every `BLOCK_SIZES_ALL` footprint at every subsampling cell, the pair arm
/// fires exactly when libaom's own `av1_ss_size_lookup[..][1][0]` column
/// (`common_data.c`) marks the plane block BLOCK_INVALID AND the plane would
/// be 2 px wide -- the 4-wide footprints. The tall/wide BLOCK_INVALID shapes
/// stay on the pre-existing `chroma_422_oob` (4, 8) tiling, and 4:2:0 /
/// 4:4:4 never enter either arm (the byte-identical claim, checked per cell).
#[test]
fn chroma422_pair_plane_matches_the_ss_size_lookup_cells() {
    // The `av1_ss_size_lookup[..][1][0]` column verbatim (pinned oracle,
    // `common_data.c`), keyed by footprint: `true` = BLOCK_INVALID.
    let ss10_invalid: &[(&str, bool)] = &[
        ("4x4", false),
        ("4x8", true),
        ("8x4", false),
        ("8x8", false),
        ("8x16", true),
        ("16x8", false),
        ("16x16", false),
        ("16x32", true),
        ("32x16", false),
        ("32x32", false),
        ("32x64", true),
        ("64x32", false),
        ("64x64", false),
        ("64x128", true),
        ("128x64", false),
        ("128x128", false),
        ("4x16", true),
        ("16x4", false),
        ("8x32", true),
        ("32x8", false),
        ("16x64", true),
        ("64x16", false),
    ];
    for (footprint, invalid10) in ss10_invalid {
        let (w, h) = footprint.split_once('x').unwrap();
        let (bw, bh) = (w.parse::<usize>().unwrap(), h.parse::<usize>().unwrap());
        // BLOCK_4X4 is the SQUARE path's leaf, never a strip: its ss cell is
        // valid because a (2, 4) plane area rounds UP to the 4x4 read
        // (`sub8_leaf_chroma422`'s own rule), and it never reaches
        // [`decode_rect_split`] at all.
        if (bw, bh) == (4, 4) {
            continue;
        }
        // 4:2:0 and 4:4:4: the port never fires, whatever the footprint.
        for (ss_x, ss_y) in [(false, false), (true, true)] {
            assert!(
                chroma422_pair_plane(ss_x, ss_y, 7, bw, bh).is_none(),
                "{bw}x{bh} at ss ({ss_x}, {ss_y}) must keep its own plane block"
            );
        }
        // 4:2:2: the pair arm is exactly the 2-px-wide (4-wide) slice of the
        // BLOCK_INVALID column, anchored at the floored mi column.
        let pair = chroma422_pair_plane(true, false, 7, bw, bh);
        assert_eq!(
            pair.is_some(),
            *invalid10 && bw == 4,
            "{bw}x{bh} at ss (1, 0): pair routing disagrees with ss_size_lookup"
        );
        if let Some((block_w, block_h, x, ctx_col)) = pair {
            assert_eq!((block_w, block_h), (4, bh), "{bw}x{bh} pair plane shape");
            assert_eq!(x, ((7 & !1) * MI) >> 1, "{bw}x{bh} floored write anchor");
            assert_eq!(ctx_col, 6, "{bw}x{bh} entropy cell is the floored column");
            // An even-column strip anchors at its own cell.
            let (x_even, col_even) = match chroma422_pair_plane(true, false, 6, bw, bh) {
                Some((_, _, x, c)) => (x, c),
                None => panic!("{bw}x{bh} lost its pair arm at an even mi_col"),
            };
            assert_eq!(
                (x_even, col_even),
                (12, 6),
                "even column keeps its own cell"
            );
        }
    }
    // The tall/wide BLOCK_INVALID shapes (8 px wide or more) are the
    // `chroma_422_oob` tiling's residents, not the pair arm's: both are
    // 4:2:2-only, so the two arms partition the BLOCK_INVALID column.
    for (bw, bh) in [
        (8usize, 16),
        (16, 32),
        (32, 64),
        (64, 128),
        (8, 32),
        (16, 64),
    ] {
        assert!(chroma422_pair_plane(true, false, 7, bw, bh).is_none());
    }
}

// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_intra_rect_in_inter(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    at: (usize, usize),
    bw: usize,
    bh: usize,
    skip: bool,
    allow_screen_content_tools: bool,
    // `(PARTITION_HORZ_4, is_chroma_reference)` when this strip is one of the
    // four 16x4 / 4x16 strips of a 16x16-level 1:4 partition (lane-intra16x4).
    strip16: Option<(bool, bool)>,
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    base_q_idx: u8,
    fctx: &crate::decode::FrameCtx,
) -> Result<()> {
    // `at` is in MI units (the inter path's own unit since lane-inter4);
    // `(r, c)` is the enclosing SUB cell the SUB-granular neighbour bands
    // below are still keyed by.
    let (mi_r, mi_c) = at;
    let (r, c) = (mi_r / (SUB / MI), mi_c / (SUB / MI));
    // `size_group_lookup` (libaom common_data.h) and `bsize_to_tx_size_cat`
    // (blockd.h) for the three 2:1 strip shapes: BLOCK_16X8 -> group 1 /
    // cat 1, BLOCK_32X16 -> 2 / 2, BLOCK_64X32 -> 3 / 3 (the rectsplitx
    // finding -- the category is NOT the same for every strip size).
    // A 1:4 strip breaks that diagonal (BLOCK_32X8 is group 1 but cat 2), so
    // it is refused by name rather than read from the wrong CDF row.
    // lane-intra16x4: the 16x16-level 1:4 strip. `size_group_lookup`
    // (common_data.h:61) puts BLOCK_16X4/BLOCK_4X16 in group 0 and its
    // tx-depth category is 1 -- but nothing here has to know either, because
    // the strip decodes through the very body the KEY FRAME path proves for
    // this shape ([`decode_rect4_16_strip`]), including the 4:2:0 chroma
    // pairing (`is_chroma_reference`: strips 0 and 2 read no chroma at all,
    // strips 1 and 3 code one 8x4 (4x8) unit at the PAIR's origin). The only
    // inter-frame difference is the mode-info read, which
    // [`INTRA_IN_INTER_MODE`] switches to `read_intra_block_mode_info`'s
    // `y_mode[0]` plus the `skip` this caller already read.
    if let Some((horz, has_chroma)) = strip16 {
        // lane-t900 r22: the screen-content refusal here was STALE (class
        // stale-premise): [`decode_rect4_16_strip`] takes
        // `allow_screen_content_tools` and reads the whole palette syntax
        // (mode/size/colours/colour-index map, its own `palette_ctx_and_cache_mi`
        // pair) -- the key frame proves that body. Lifted with the 10-bit
        // witness `palette_screen_witness_10bit.obu`, whose inter frames carry
        // 16x4/4x16 intra strips inside a `--tune-content=screen` frame.
        debug_assert_eq!((bw, bh), if horz { (16, 4) } else { (4, 16) });
        fctx.intra_in_inter_mode.with(|c| c.set(Some((0, skip))));
        let res = decode_rect4_16_strip(
            dec,
            cdfs,
            neighbours,
            (mi_r, mi_c),
            horz,
            has_chroma,
            // The inter strips of the same partition publish their own
            // mi-exact mode, so the key-frame caller's coarse-cell stand-in
            // has nothing to add here.
            None,
            y,
            u,
            v,
            fctx.enable_filter_intra_inter.with(std::cell::Cell::get),
            allow_screen_content_tools,
            base_q_idx,
            fctx.tx_select_inter.with(std::cell::Cell::get),
            fctx.reduced_tx_set_inter.with(std::cell::Cell::get),
            fctx,
        );
        fctx.intra_in_inter_mode.with(|c| c.set(None));
        let (mode, uv, (tx_w, tx_h)) = res?;
        // lane-intra16x4, the same publication the 2:1 intra-in-inter strip
        // makes ([`decode_intra_rect_in_inter`]'s own tail): libaom runs
        // `set_txfm_ctxs(tx_size, n4_w, n4_h, skip && is_inter, xd)` for EVERY
        // block of an inter frame, intra ones included, and
        // `txfm_partition_update` for the var-tx context. The key-frame body
        // writes neither (a key frame has no var-tx tree and its own readers
        // use the deblock grid), so the block AFTER this strip read a stale
        // band -- measured: a 128x128 8-bit stream decoded the two intra 16x4
        // strips at mi(23,16)/(23,20) exactly and then broke on the very next
        // 16x16 row.
        txfm_partition_update_rect(neighbours, (mi_r, mi_c), (tx_w, tx_h), (bw, bh));
        for i in 0..bw / MI {
            if let Some(cell) = neighbours.above_txfm.get_mut(mi_c + i) {
                *cell = tx_w as u8;
            }
        }
        for i in 0..bh / MI {
            if let Some(cell) = neighbours.left_txfm.get_mut(mi_r + i) {
                *cell = tx_h as u8;
            }
        }
        // [`decode_rect4_16`]'s own post-loop publication, per strip (the
        // coarse 16-px cell holds one entry for the whole 16x16 and the last
        // block to write it wins).
        neighbours.above_mode[c] = mode;
        neighbours.left_mode[r] = mode;
        if let Some(uvm) = uv {
            neighbours.above_uv_mode[c] = uvm;
            neighbours.left_uv_mode[r] = uvm;
        }
        neighbours.above_side[c] = bw;
        neighbours.left_side[r] = bh;
        if crate::envflags::env_flag!("EC_INTRA16X4") {
            eprintln!(
                "EC_INTRA16X4 decoded mi={mi_r},{mi_c} px={},{} horz={horz} has_chroma={has_chroma} skip={skip} mode={mode} uv={uv:?}",
                mi_c * MI,
                mi_r * MI
            );
        }
        hit_do! {
            INTRA16X4_IN_INTER_HITS.with(|h| {
                let mut v = h.get();
                v[usize::from(!horz)] += 1;
                v[2] += usize::from(has_chroma);
                h.set(v);
            });
        }
        return Ok(());
    }
    // lane-inter128intra r1: the 128 superblock's PARTITION_HORZ/VERT half
    // coded INTRA inside an inter frame. `size_group_lookup` (common_data.h:61)
    // puts BLOCK_128X64/64X128/128X128 in group 3 and
    // `max_txsize_rect_lookup` caps them at TX_64X64, i.e. the same
    // `tx_size_cat3` row [`decode_block_128rect`] already reads -- so the whole
    // block decodes through the very body the KEY FRAME path proves for this
    // shape (square 64x64 luma units, one TX_32X32 chroma unit per 64x64 mu
    // chunk on the offset-10 txb_skip rows, per-TU prediction, the mu-chunk
    // `Reach` rule and the frame-edge write clip). The only inter-frame
    // differences live inside that body: the mode-info read
    // ([`INTRA_IN_INTER_MODE`] -> `y_mode[3]` plus the `skip` this caller
    // already read), the `tx_depth` context (the real TXFM_CONTEXT bands) and
    // the `set_txfm_ctxs` publication. CfL (>32), filter intra (>32) and
    // palette (>64) do not exist at this size and are refused by name there.
    // lane-t900 r18: the opt-in is LIFTED -- the arm has a witness.
    // `intra128_in_inter_witness2.obu` (gate
    // `an_intra_coded_128_half_inside_an_inter_frame_decodes_pixel_exact`)
    // fires it 3x 128x64 + 6x 64x128 and decodes byte-exact on every plane of
    // all 15 frames, so the hits are counted out of exact frames (class
    // `counter-from-refused-stream`). What r16/r17 could not find, and what
    // that recipe exploits: libaom's RD essentially never PICKS a 128-root
    // HORZ/VERT in the interior -- the 512x512 r16 witness and a textured
    // 640x384 sweep code 128 roots as NONE or SPLIT only. The shape appears
    // where the frame EDGE forces it: a partial last superblock row has
    // `has_rows128 == false`, so the 128x64 top half is implied, and a
    // partial last column implies the 64x128 left half. Put content no
    // reference holds (a fresh smooth ramp) in exactly those two bands of an
    // inter frame and the encoder codes those forced halves INTRA.
    if bw.max(bh) == 128 {
        // lane-t900 r32: no screen-content refusal here. `av1_allow_palette`
        // rejects every footprint with a 128-pixel side (`bw > 64 || bh > 64`,
        // enumerated by `no_block_footprint_with_a_128_pixel_side_can_carry_\
        // palette_syntax`), so a 128-root half consumes no palette syntax and
        // a screen-content frame's half is syntactically identical to the
        // non-screen one this arm already decodes byte-exact.
        if coeff_trace_on() {
            // lane-t900 r16: which decode-order frame this arm fires in --
            // interleaved with the `EC_COEFF_FRAME` markers, this is what
            // proves the hit is counted out of a frame that compares exact
            // (class `counter-from-refused-stream`).
            eprintln!("EC_INTRA128 mi_row={r} mi_col={c} bw={bw} bh={bh}");
        }
        fctx.intra_in_inter_mode.with(|c| c.set(Some((3, skip))));
        let res = decode_block_128rect(
            dec,
            cdfs,
            neighbours,
            (r, c),
            bw,
            bh,
            y,
            u,
            v,
            base_q_idx,
            fctx.enable_filter_intra_inter.with(std::cell::Cell::get),
            allow_screen_content_tools,
            fctx.tx_select_inter.with(std::cell::Cell::get),
            fctx.reduced_tx_set_inter.with(std::cell::Cell::get),
            fctx,
        );
        fctx.intra_in_inter_mode.with(|c| c.set(None));
        return res;
    }
    let Some((size_group, tx_cat)) = intra_in_inter_size_row(bw, bh) else {
        if crate::envflags::env_flag!("EC_OBMCREC") {
            eprintln!(
                "OBMCREC intra-in-inter shape bw={bw} bh={bh} mi=({mi_r},{mi_c}) skip={skip}"
            );
        }
        // lane-obmcrec r1: the message used to name a 16x4/4x16 strip
        // (class refusal-names-a-correlate). lane-t900 r20: the shapes that
        // can arrive here are enumerated against the measured block-shape
        // census ([`INTER_BLOCK_SHAPES`]) by
        // [`every_intra_in_inter_shape_the_census_lists_has_a_size_group_row`]
        // -- every one of them has a row, so this arm names a shape no stream
        // has been able to present.
        return Err(unsupported(format!(
            "an intra-coded {bw}x{bh} block on the inter block path (no size-group/tx-category row for that shape here)"
        )));
    };
    // lane-t900 r21: this arm used to refuse EVERY rect strip of a
    // screen-content frame ("palette syntax is consumed for square blocks
    // only"). It is not: [`read_intra_mode_rect`] generalised the palette read
    // to rect blocks on the key-frame path, and the 1:4 arm below goes through
    // it, while the 2:1 reader inlined further down now codes the same syntax
    // itself. A real `--tune-content=screen` inter stream reached this refusal
    // on 8x16 / 16x8 strips (gate
    // `a_real_aomenc_screen_inter_sequence_codes_palette_syntax_on_rect_intra_strips`).
    // lane-intra14 r1: a 1:4 strip decodes through the very readers the KEY
    // FRAME path already proves for this shape ([`decode_block_rect4`] at the
    // 32 level, [`decode_block_rect64`] at 64 -- both already handle the 1:4
    // luma/chroma transform tables, the per-unit split walk and the strip's
    // own neighbour records). The ONLY difference on an inter frame is the
    // mode-info read, which [`INTRA_IN_INTER_MODE`] switches to
    // `read_intra_block_mode_info`'s `y_mode[size_group]` plus the `skip`
    // this caller already read. `tx_cat` above is the same category those
    // readers hardcode (cat 2 in `decode_block_rect4`, cat 3 in
    // `decode_block_rect64`), asserted here so a future shape cannot drift.
    if bw.max(bh) == 4 * bw.min(bh) {
        debug_assert_eq!(tx_cat, if bw.max(bh) == 64 { 3 } else { 2 });
        fctx.intra_in_inter_mode
            .with(|c| c.set(Some((size_group, skip))));
        let res = if bw.max(bh) == 64 {
            decode_block_rect64(
                dec,
                cdfs,
                neighbours,
                (mi_r / (SUB / MI), mi_c / (SUB / MI)),
                bw,
                bh,
                y,
                u,
                v,
                fctx.enable_filter_intra_inter.with(std::cell::Cell::get),
                allow_screen_content_tools,
                base_q_idx,
                fctx.tx_select_inter.with(std::cell::Cell::get),
                fctx.reduced_tx_set_inter.with(std::cell::Cell::get),
                fctx,
            )
        } else {
            decode_block_rect4(
                dec,
                cdfs,
                neighbours,
                (mi_r, mi_c),
                bw,
                bh,
                None,
                y,
                u,
                v,
                fctx.enable_filter_intra_inter.with(std::cell::Cell::get),
                allow_screen_content_tools,
                base_q_idx,
                fctx.tx_select_inter.with(std::cell::Cell::get),
                fctx,
            )
            .map(|_| ())
        };
        fctx.intra_in_inter_mode.with(|c| c.set(None));
        res?;
        let shape = match (bw, bh) {
            (64, 16) => 0,
            (16, 64) => 1,
            (32, 8) => 2,
            _ => 3,
        };
        hit_do! {
            INTRA_RECT4_IN_INTER_HITS.with(|h| {
                let mut v = h.get();
                v[shape] += 1;
                h.set(v);
            });
        }
        return Ok(());
    }
    crate::msac::SymbolDecoder::set_symr_cdf("y_mode");
    let mode = dec.symbol(&mut cdfs.y_mode[size_group]);
    // lane-t900 r1: this arm read six mode symbols with no ladder rung of its
    // own, so a cross-decoder bisection could only see the block's first
    // coefficient. Same names/format as [`read_tx_size`]'s rung.
    let step_trace = crate::envflags::env_flag!("EC_TRACE_MODE_STEP");
    if step_trace {
        eprintln!(
            "EC_ISTEP mi_row={mi_r} mi_col={mi_c} name=y_mode val={mode} grp={size_group} rng={}",
            dec.debug_state().0
        );
    }
    if mode >= 13 {
        return Err(unsupported(
            "an intra mode this decoder does not code (round 2)",
        ));
    }
    // lane-angleinter (merged on main for the square intra-in-inter arm):
    // the delta is read off `angle_delta_cdf[mode - V_PRED]` and feeds the
    // directional predictor; this arm refused a nonzero one before the merge.
    let angle_delta_y = if (V_PRED..=D67_PRED).contains(&mode) {
        read_angle_delta(dec, &mut cdfs.angle_delta[mode - V_PRED])
    } else {
        0
    };
    if angle_delta_y != 0 {
        hit!(INTRA_IN_INTER_ANGLE_DELTA_Y_HITS);
    }
    // `is_cfl_allowed` (spec 5.11.5) caps CFL at 32x32, so the
    // superblock-level strip reads the no-CFL alphabet -- the same split
    // [`read_intra_mode_rect`] makes for [`decode_block_rect64`].
    //
    // lane-av1mono: a monochrome strip codes no chroma mode/CFL symbol
    // (libaom `decodemv.c:933`).
    let has_chroma = !mono(fctx);
    let cfl_allowed = has_chroma && cfl_allowed_px(bw, bh, fctx);
    let uv_mode = if !has_chroma {
        DC_PRED
    } else if cfl_allowed {
        crate::msac::SymbolDecoder::set_symr_cdf("uv_mode");
        dec.symbol(&mut cdfs.uv_mode_cfl[mode])
    } else {
        crate::msac::SymbolDecoder::set_symr_cdf("uv_mode");
        dec.symbol(&mut cdfs.uv_mode_no_cfl[mode])
    };
    if step_trace {
        eprintln!(
            "EC_ISTEP mi_row={mi_r} mi_col={mi_c} name=uv_mode val={uv_mode} cfl={} rng={}",
            u8::from(cfl_allowed),
            dec.debug_state().0
        );
    }
    if (9..=12).contains(&uv_mode) {
        hit!(SMOOTH_UV_HITS);
    }
    let alpha = if cfl_allowed && uv_mode == UV_CFL_PRED {
        Some(read_cfl_alphas(dec, cdfs))
    } else {
        None
    };
    let uv_predict_mode = if uv_mode == UV_CFL_PRED {
        DC_PRED
    } else {
        uv_mode
    };
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
    // `read_palette_mode_info` (spec 5.11.13), in the position
    // `read_intra_block_mode_info` writes it: after the chroma mode and
    // before `filter_intra`. lane-t900 r21: a `--tune-content=screen` inter
    // frame writes `palette_y_mode`/`palette_uv_mode` for every block whose
    // shape [`palette_bsize_ctx_wh`] admits, intra strips included, so
    // skipping the syntax here desynced the tile at the strip's own symbol.
    // lane-t900 r22: reconstructed, not refused -- the strip's real mode
    // context and colour cache at its own mi position, and the two
    // colour-index maps decoded after `filter_intra` (`av1_visit_palette`),
    // exactly as [`read_intra_mode_rect`] does on the key-frame path.
    let (palette_ctx, palette_cache) = neighbours.palette_ctx_and_cache_mi((mi_r, mi_c));
    let palette_uv_cache = neighbours.palette_uv_cache_mi((mi_r, mi_c));
    let mut palette_y_pending: Option<(usize, [u16; 8])> = None;
    let mut palette_uv_pending: Option<(usize, [u16; 8], [u16; 8])> = None;
    if allow_screen_content_tools && let Some(bsize_ctx) = palette_bsize_ctx_wh(bw, bh) {
        hit!(INTRA_RECT_IN_INTER_SCREEN_HITS);
        let use_palette_y =
            mode == DC_PRED && dec.symbol(&mut cdfs.palette_y_mode[bsize_ctx][palette_ctx]) != 0;
        if use_palette_y {
            let n = 2 + dec.symbol(&mut cdfs.palette_y_size[bsize_ctx]);
            palette_y_pending = Some((n, read_palette_colors_y(dec, n, &palette_cache, fctx)));
        }
        // A 2:1 strip of at least 8x8 is always a chroma reference, so
        // `read_palette_mode_info`'s `xd->is_chroma_ref` guard holds here.
        // lane-av1mono: libaom's actual guard is `num_planes > 1 &&
        // uv_mode == UV_DC_PRED && xd->is_chroma_ref` (decodemv.c:602).
        let use_palette_uv = has_chroma
            && uv_mode == DC_PRED
            && dec.symbol(&mut cdfs.palette_uv_mode[usize::from(use_palette_y)]) != 0;
        if crate::envflags::env_flag!("EC_PALSYN") {
            eprintln!(
                "EC_PALSYN rect mi={mi_r},{mi_c} bw={bw} bh={bh} mode={mode} uv={uv_mode} ctx={palette_ctx} y={} uv={} rng={}",
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
    // lane-fiinter (merged on main for the square intra-in-inter arm):
    // `read_filter_intra_mode_info` is the tail of
    // `read_intra_block_mode_info`, after the palette syntax (refused whole
    // above for this shape), and `av1_filter_intra_allowed_bsize` admits the
    // 2:1 strips with both sides <= 32 -- their own CDF rows
    // ([`filter_intra_size_class_rect`]).
    let mut filter_intra = None;
    if mode == DC_PRED
        && fctx.enable_filter_intra_inter.with(std::cell::Cell::get)
        // `av1_filter_intra_allowed` excludes a palette-Y block.
        && palette_y_pending.is_none()
        && let Some(class) = filter_intra_size_class_rect(bw, bh)
        && dec.symbol(&mut cdfs.filter_intra[class]) != 0
    {
        hit!(FILTER_INTRA_HITS);
        hit!(INTRA_IN_INTER_FILTER_INTRA_HITS);
        filter_intra = Some(dec.symbol(&mut cdfs.filter_intra_mode));
    }
    // `av1_visit_palette`: Y's colour-index map then the shared chroma one.
    let palette_y = palette_y_pending.map(|(n, colors)| {
        let map = decode_color_index_map_wh(
            dec,
            cdfs,
            n,
            bw,
            bh,
            palette_onscreen(mi_r, mi_c, bw, bh, fctx),
            false,
        );
        hit!(PALETTE_HITS);
        hit!(PALETTE_RECT_HITS);
        hit!(INTRA_IN_INTER_PALETTE_Y_HITS);
        PaletteY {
            size: n,
            colors,
            map,
        }
    });
    let palette_uv = palette_uv_pending.map(|(n, u_colors, v_colors)| {
        let (cw, ch) = ((bw >> ss_x(fctx)).max(4), (bh >> ss_y(fctx)).max(4));
        let on = palette_onscreen_uv(
            (bw, bh),
            palette_onscreen(mi_r, mi_c, bw, bh, fctx),
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
    // The intra-edge filter type is the NEIGHBOUR's mode (libaom
    // `get_filt_type`), luma's off the luma neighbours and chroma's off the
    // chroma ones -- [`decode_block_rect`]'s own pair.
    // lane-t900 r4: the coarse pair is keyed on the 16x16 cell's OWN top-left
    // mi, so both 8x16 halves of a VERT split read the neighbours of the FIRST
    // half. libaom's `above_mbmi`/`left_mbmi` are this block's own
    // (`mi[-mi_stride]` / `mi[-1]`) -- take the mi-exact pair over the coarse
    // one exactly as [`decode_leaf8`] already does (class
    // context-read-from-one-cell).
    let coarse_pair = neighbours.modes_above_left(r, c);
    let (nb_above_mode, nb_left_mode) = neighbours.modes_above_left_mi(mi_r, mi_c, coarse_pair);
    if (nb_above_mode, nb_left_mode) != coarse_pair {
        hit!(EDGE_FILTER_MI_FIX_HITS);
    }
    let smooth_neighbor = is_smooth_mode(nb_above_mode) || is_smooth_mode(nb_left_mode);
    if smooth_neighbor {
        hit!(SMOOTH_LUMA_HITS);
    }
    let smooth_neighbor_uv = neighbours.smooth_uv_neighbour(mi_r, mi_c, r, c, fctx);
    // lane-av1mixloss: the lossless-segment early return (`decodeframe.c:1203`)
    // precedes `TX_MODE_SELECT`, so a lossless-segment intra-rect block inside
    // an inter frame codes no depth symbol either (`xd->lossless` is per
    // segment regardless of frame type; class sweep of the sub8 leaf's fix).
    let depth = if fctx.tx_select_inter.with(std::cell::Cell::get) && !lossless(fctx) {
        // The frame's own `TXFM_CONTEXT` bands -- [`tx_size_context_txfm_rect`].
        let ctx = tx_size_context_txfm_rect(neighbours, (mi_r, mi_c), bw.min(64), bh.min(64), fctx);
        let d = match tx_cat {
            1 => dec.symbol(&mut cdfs.tx_size_cat1[ctx]),
            2 => dec.symbol(&mut cdfs.tx_size_cat2[ctx]),
            _ => dec.symbol(&mut cdfs.tx_size_cat3[ctx]),
        };
        if step_trace {
            eprintln!(
                "EC_ISTEP mi_row={mi_r} mi_col={mi_c} name=tx_depth val={d} ctx={ctx} cat={tx_cat} rng={}",
                dec.debug_state().0
            );
        }
        d
    } else {
        0
    };
    if depth != 0 || lossless(fctx) {
        hit!(TX_DEPTH_HITS);
        // lane-intrasplit r1: a split intra strip in an inter frame walks the
        // key frame's own per-TU path ([`decode_rect_split`] below); what was
        // wrong when this refusal was written was the depth SYMBOL, not the walk.
        hit_do! {
            INTRA_RECT_IN_INTER_SPLIT_TX_HITS.with(|h| {
                let mut v = h.get();
                v[depth.min(2)] += 1;
                h.set(v);
            });
        }
    }
    // lane-intrasplit r3: print EVERY intra strip with its depth, not only the
    // split ones -- a `depth=0`-only stream is a witness gap in the SOURCE, and
    // the two are indistinguishable when the diagnostic fires on splits alone.
    if crate::envflags::env_flag!("EC_SPLITSTRIP") {
        eprintln!("EC_SPLITSTRIP mi_row={mi_r} mi_col={mi_c} bw={bw} bh={bh} depth={depth}");
    }
    let (tx_w, tx_h) = depth_to_tx_wh(bw, bh, depth, fctx);
    // lane-inter16ab r8: libaom runs `set_txfm_ctxs(tx_size, n4_w, n4_h,
    // skip && is_inter, xd)` (decodeframe.c `decode_block`) for EVERY block
    // of an inter frame, intra ones included -- with the skip term 0 it
    // memsets `above_txfm_context` to `tx_size_wide` over the block's width
    // and `left_txfm_context` to `tx_size_high` over its height. This strip
    // wrote neither, so the next block's var-tx `txfm_partition` context read
    // a stale band: on a 192x128 8-bit stream the 16x8 intra strip at
    // mi(14,0) left 8 where libaom has 16, giving ctx 13 instead of 12 at
    // mi(16,0) and desyncing the tile at that symbol.
    txfm_partition_update_rect(neighbours, (mi_r, mi_c), (tx_w, tx_h), (bw, bh));
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
        fctx.reduced_tx_set_inter.with(std::cell::Cell::get),
        fctx,
    )?;
    // `set_txfm_ctxs` (libaom `decodeframe.c:1078`, at the end of
    // `decode_token_recon_block`): an INTRA block records its TRANSFORM size in
    // both `TXFM_CONTEXT` bands -- the above row over the block's width, the
    // left column over its height. This arm wrote neither before lane-intrasplit
    // r1, so every later block in the frame read a stale `tx_size`/`txfm_split`
    // context across the strip's footprint.
    for i in 0..bw / MI {
        if let Some(cell) = neighbours.above_txfm.get_mut(mi_c + i) {
            *cell = tx_w as u8;
        }
    }
    for i in 0..bh / MI {
        if let Some(cell) = neighbours.left_txfm.get_mut(mi_r + i) {
            *cell = tx_h as u8;
        }
    }
    // The strip's own palette state into the mi-granular neighbour bands, so
    // the next block's colour cache and `palette_y_mode` context are libaom's.
    record_strip_palette(
        neighbours,
        (mi_r, mi_c),
        bw,
        bh,
        palette_y.as_ref().map_or(0, |p| p.size),
        palette_y.as_ref().map_or([0u16; 8], |p| p.colors),
        palette_uv.as_ref().map_or(0, |p| p.size),
        palette_uv.as_ref().map_or([0u16; 8], |p| p.u_colors),
    );
    let class = match bw.max(bh) {
        64 => 0,
        32 => 1,
        _ => 2,
    };
    hit_do! {
        INTRA_RECT_IN_INTER_HITS.with(|h| {
            let mut v = h.get();
            v[class] += 1;
            h.set(v);
        });
    }
    Ok(())
}

/// Reconstructs one rectangular INTRABC block, `bw`x`bh` pixels at the real
/// mi `(mi_r, mi_c)` (lane-av1-intrabc).
///
/// libaom treats an intrabc block as INTER for everything except the mode-info
/// read itself: `is_inter_block` is `is_intrabc_block(mbmi) || ref > INTRA`
/// (`blockd.h:373`), so
///
///  * `parse_decode_block` reads the transform size through the INTER var-tx
///    tree (`read_tx_size_vartx`) -- [`read_block_tx_size_rect`], whose
///    `txfm_partition` context comes off the real `TXFM_CONTEXT` bands and
///    whose leaves publish their own resolved size back;
///  * `predict_inter_block` predicts the WHOLE block once, from the CURRENT
///    frame's own pre-loop-filter reconstruction, at the coded block vector
///    with the BILINEAR kernel `read_intrabc_info` forces (`decodemv.c:913`);
///  * `decode_token_recon_block`'s inter branch then reads one residual per
///    transform unit, through the INTER coefficient sets
///    ([`read_inter_plane_rect`]/[`read_inter_plane`]) with the chroma plane
///    inheriting the colocated luma unit's coded `tx_type`.
///
/// Text order is libaom's: transform tree, prediction, luma residual, chroma
/// residual, bookkeeping. A skipped block codes no residual at all -- the
/// prediction is the reconstruction.
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_intrabc_rect(
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
    fctx: &crate::decode::FrameCtx,
) -> Result<()> {
    hit!(INTRABC_RECT_HITS);
    // [horz, vert]: see `INTRABC_RECT_CODED_HITS` / `INTRABC_RECT_SKIPPED_HITS`.
    let idx = usize::from(bh > bw);
    if skip {
        INTRABC_RECT_SKIPPED_HITS.with(|c| {
            let mut seen = c.get();
            seen[idx] += 1;
            c.set(seen);
        });
    } else {
        INTRABC_RECT_CODED_HITS.with(|c| {
            let mut seen = c.get();
            seen[idx] += 1;
            c.set(seen);
        });
    }
    let (px, py) = (mi_c * MI, mi_r * MI);
    // lane-whtshape: a LOSSLESS block's chroma plane block is
    // `bw >> ss_x` by `bh >> ss_y` at its own origin -- `av1_get_max_uv_txsize`
    // is `ss_size_lookup[bsize]` (no halving of its own), so at 4:4:4 that is
    // the block's OWN footprint, not the 4:2:0 halving the lossy arm hard-codes.
    // Measured on this lane's 4:4:4 lossless witness: with the halving, a
    // 16x8 intrabc block's chroma plane block came out 8x4 and the lossless
    // walk below coded 2 units per plane where libaom codes 8.
    //
    // lane-ibc444c: UNSCOPED now. lane-whtshape split this on `lossless` and
    // named the LOSSY 4:4:4 footprint as another lane's open cell; this is
    // that cell. Measured on `444_intrabc_rect4_witness.obu`: with the
    // halving, an 8x16 luma block's 8x16 chroma plane block was read as ONE
    // 4x8 rect unit where libaom codes ONE TX_8X16, and the U `txb_skip` of
    // the block at mi (92, 104) read all_zero=0 here against the oracle's
    // all_zero=1 -- the bit position forks there. Identical arithmetic at
    // 4:2:0, where `ss_x = ss_y = 1`, so the pinned 4:2:0 twin of the
    // witness is the identity arm of this lane's gate.
    let (cpx, cpy) = (px >> ss_x(fctx), py >> ss_y(fctx));
    // `av1_get_max_uv_txsize(bw x bh)`: 4:2:0 halves the footprint and the
    // halved shape's own `max_txsize_rect_lookup` entry IS that size for every
    // shape this helper is called at, so the uv plane block is ONE unit --
    // the same call `decode_token_recon_block`'s own chroma loop makes.
    let (cw, ch) = (bw >> ss_x(fctx), bh >> ss_y(fctx));
    // lane-av1-intrabc r2: the square stride the proven INTER path uses
    // (`side`). The residual grids, the prediction windows AND
    // `TxParams::stride` are all laid out at it -- the block's own width
    // panicked `TxParams::run` on every coded VERT strip (8x16/16x32: `h`
    // exceeds the stride).
    let side = bw.max(bh);
    let cside = cw.max(ch);
    // `av1_calc_mi_size`'s 8-px-aligned grid: a block hanging over the right
    // or bottom frame edge reads no transform-tree symbol for the units
    // outside it.
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
    if leaves.is_some() {
        hit!(INTRABC_RECT_VARTX_HITS);
    }
    if ss_x(fctx) == 0 && ss_y(fctx) == 0 {
        hit!(IBC_RECT_CHROMA_FOOTPRINT_444_HITS);
    }
    // lane-ibc444c: per-block geometry, for the chroma-tail attribution.
    if crate::envflags::env_flag!("EC_IBCBLOCK") {
        eprintln!(
            "EC_IBCBLOCK mi=({mi_r},{mi_c}) bw={bw} bh={bh} cpx={cpx} cpy={cpy} \
             cw={cw} ch={ch} cside={cside} skip={skip}"
        );
    }
    // The whole-block prediction: `predict_inter_block` runs once, before any
    // coefficient is read. The source is this frame's own reconstruction,
    // which is already written.
    // `mc::predict_with_filter` writes a DENSE window; the residual machinery
    // reads it at the square stride, so re-lay it -- the same step the inter
    // path takes for its own square prediction buffer.
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
    let mut pred_u_dense = vec![0u16; cw * ch];
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
        &mut pred_u_dense,
        fctx,
    );
    let mut pred_u = vec![0u16; cside * cside];
    for row in 0..ch {
        pred_u[row * cside..][..cw].copy_from_slice(&pred_u_dense[row * cw..][..cw]);
    }
    let mut pred_v_dense = vec![0u16; cw * ch];
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
        &mut pred_v_dense,
        fctx,
    );
    let mut pred_v = vec![0u16; cside * cside];
    for row in 0..ch {
        pred_v[row * cside..][..cw].copy_from_slice(&pred_v_dense[row * cw..][..cw]);
    }
    let reduced = fctx.reduced_tx_set_inter.with(std::cell::Cell::get);
    let luma_tx_type: TxType;
    let (luma_grid, u_grid, v_grid);
    // lane-whtshape: the LOSSLESS chroma walk's composed plane-major grids,
    // kept so their per-4x4 slices can be re-stamped over the whole-block
    // record in the tail. `None` on every other arm.
    let mut ll_chroma: Option<(Vec<i32>, Vec<i32>)> = None;
    if skip {
        // No residual syntax at all: the prediction IS the block.
        // Residual stride is `side` (and `cside`), not `bw`: reconstruct_mc_rect indexes
        // `residual[row * side .. row*side + w]` for row in 0..h, so a skipped VERT strip
        // (bh > bw) overruns a `bw * bh` slice. Bug-for-bug with the inter skip arm.
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
            &ZERO_RESIDUAL[..side * side],
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
            &ZERO_RESIDUAL[..cside * cside],
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
            &ZERO_RESIDUAL[..cside * cside],
            fctx,
        );
        if let Some(ls) = leaves.as_ref() {
            // lane-av1-loss64, merged: the same reset [`decode_intrabc_owned_rect`]'s
            // skip arm performs -- libaom `av1_reset_entropy_context`
            // (`if (mbmi->skip_txfm)`, decodeframe.c:1262 -> blockd.c:58): a
            // SKIPPED block zeroes its whole entropy-band footprint. This
            // helper serves `decode_block_rect4`'s 1:4 strips on this tree,
            // so a skipped lossless strip whose `leaves` resolved (every
            // lossless frame -- `read_block_tx_size_rect`'s lossless arm
            // returns the 4x4 grid before it looks at `skip`) left the
            // PREVIOUS occupant's luma bands standing and the NEXT block's
            // partition parse diverged from libaom's: the 320x242 lossless
            // sb128 gate read a phantom unskipped 8x32 intrabc strip at
            // mi(56,66) right after the skipped 8x16 one at mi(48,66).
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
        luma_tx_type = TxType::DctDct;
        luma_grid = Grid::Zero(side * side);
        u_grid = Grid::Zero(cside * cside);
        v_grid = Grid::Zero(cside * cside);
    } else {
        match leaves.as_ref() {
            None => {
                // One whole-block rectangular transform unit.
                // lane-av1422mi24: at ss (1,0) the chroma above span is
                // `bw >> 1` chroma cells spread over `bw` luma mi columns --
                // the per-mi gather counts every chroma column's whole-unit
                // dc sign twice (t422 frame 1, the mi(26,60) intra-in-inter
                // strip's U dc_sign read ctx 1 where the oracle's cell sum
                // is 0 -> ctx 0). Luma keeps the per-mi gather; the two
                // chroma planes take
                // [`Neighbours::around_mi_422_chroma`]'s every-second
                // above-cell sampling, like every other 4:2:2 chroma gather.
                let mut around = neighbours.around_mi_rect((mi_r, mi_c), bw, bh);
                if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                    let c = neighbours.around_mi_422_chroma((mi_r, mi_c), bw, bh);
                    around[1] = c[1];
                    around[2] = c[2];
                }
                let (grid, tx_type) = read_inter_plane_rect(
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
                luma_tx_type = tx_type;
                luma_grid = embed_grid_stride(&grid, bw, bh, side);
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
                u_grid = embed_grid_stride(&ug, cw, ch, cside);
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
                v_grid = embed_grid_stride(&vg, cw, ch, cside);
            }
            Some(leaves) => {
                // P2 (lane-av1-intrabc r2): libaom `get_transform_size` applies
                // the split override to LUMA only, per leaf -- the step the
                // proven inter var-tx path applies right after its own residual
                // loop ([`Neighbours::fill_lf_grid_leaf_luma`]). Without it the
                // leaf's own transform edges are never filtered.
                for &(row, col, tw, th) in leaves.iter() {
                    neighbours.fill_lf_grid_leaf_luma(
                        (mi_r + row, mi_c + col),
                        tw / MI,
                        th / MI,
                        tw as u8,
                        th as u8,
                    );
                }
                // One residual per transform unit, in the order the tree coded
                // them; each unit reads its own neighbour-magnitude context off
                // the units already decoded before it, and every unit adds onto
                // its own window of the block's single prediction.
                let mut first_tx_type = TxType::DctDct;
                for (idx, &(row, col, tw, th)) in leaves.iter().enumerate() {
                    let tu_mi = (mi_r + row, mi_c + col);
                    let (tu_px, tu_py) = (px + col * MI, py + row * MI);
                    let tu_pred = Pred::Inline(&pred_y, side).offset(row * MI * side + col * MI);
                    let (tu_grid, tu_tx_type) = if tw == th {
                        let scan = default_scan(tw.min(32));
                        let tu_skip_ctx = neighbours.luma_skip_ctx(tu_mi, tw / MI);
                        read_inter_plane(
                            dec,
                            cdfs,
                            inter_txbset_for(tw, reduced),
                            scan,
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
                            Some(tu_skip_ctx),
                            fctx,
                        )?
                    } else {
                        let tu_skip_ctx = neighbours.luma_skip_ctx_rect(tu_mi, tw / MI, th / MI);
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
                            Some(tu_skip_ctx),
                            fctx,
                        )?
                    };
                    if idx == 0 {
                        first_tx_type = tu_tx_type;
                    }
                    neighbours.record_mi_luma_rect(tu_mi, tw, th, &tu_grid);
                }
                // `av1_get_tx_type`'s chroma lookup lands on the colocated
                // LUMA unit, which for a split block is the top-left one.
                luma_tx_type = first_tx_type;
                // lane-av1422mi24: same 4:2:2 chroma gather rule as the
                // `None` arm above -- the per-mi rect gather double-counts
                // each chroma column's whole-unit dc sign at ss (1,0).
                let mut around = neighbours.around_mi_rect((mi_r, mi_c), bw, bh);
                if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                    let c = neighbours.around_mi_422_chroma((mi_r, mi_c), bw, bh);
                    around[1] = c[1];
                    around[2] = c[2];
                }
                if lossless(fctx) {
                    // lane-whtshape: at lossless libaom codes TX_4X4 on EVERY
                    // plane of a block -- `read_tx_size` returns `TX_4X4` for a
                    // lossless block before it looks at a tree, `read_tx_mode`
                    // already forced `ONLY_4X4` for the frame, and
                    // `get_vartx_max_txsize` returns `TX_4X4` for every plane
                    // -- so this block's chroma plane block is a
                    // `(cw/4) * (ch/4)` raster of 4x4 units, PLANE-MAJOR
                    // (`decode_token_recon_block`: every U unit, then every V
                    // unit), not the single rect unit the two
                    // `read_inter_plane_rect` calls below read. That single
                    // 8x4 unit is what reached the lossless WHT with the wrong
                    // shape (the `debug_assert_eq!((w, h), (4, 4))` this lane
                    // replaced). Same helper, same walk, as the inter 8x8 and
                    // 128 paths.
                    hit!(INTRABC_RECT_LOSSLESS_CHROMA4_HITS);
                    let mut uo = vec![0i32; cside * cside];
                    let mut vo = vec![0i32; cside * cside];
                    read_inter_chroma_lossless(
                        y,
                        u,
                        v,
                        dec,
                        cdfs,
                        neighbours,
                        Pred::Inline(&pred_u, cside),
                        Pred::Inline(&pred_v, cside),
                        (mi_r, mi_c),
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
                    // The per-unit walk stamped every unit's own coefficient
                    // context, but the whole-block `record_split_luma_rect_mi`
                    // in the tail rewrites every chroma cell with ONE composed
                    // state, which drops the per-unit DC signs and levels (class
                    // `override-slot-on-one-arm` -- the same one
                    // `decode_rect_split`'s `ll_chroma_units` replay exists
                    // for). The grids are kept here and replayed AFTER that
                    // record, from the same 4x4 slices, so the same states.
                    u_grid = embed_grid_stride(&Grid::Own(uo.clone()), cw, ch, cside);
                    v_grid = embed_grid_stride(&Grid::Own(vo.clone()), cw, ch, cside);
                    ll_chroma = Some((uo, vo));
                    luma_grid = Grid::Zero(side * side);
                } else {
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
                    u_grid = embed_grid_stride(&ug, cw, ch, cside);
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
                    v_grid = embed_grid_stride(&vg, cw, ch, cside);
                    luma_grid = Grid::Zero(side * side);
                }
            }
        }
    }
    let _ = luma_tx_type;
    // Bookkeeping. An intrabc block publishes mode `DC_PRED` everywhere the
    // next block reads a neighbour mode, its own block size in both `TXFM`
    // bands (already done by `read_block_tx_size_rect`), a zeroed palette
    // state (a stale entry would feed the next block's colour cache), and its
    // DV into the intrabc mi grid the next block's DV predictor reads.
    if leaves.is_some() {
        neighbours.record_split_luma_rect_mi(
            (mi_r, mi_c),
            bw,
            bh,
            DC_PRED,
            DC_PRED,
            [&u_grid, &v_grid],
        );
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
    // lane-whtshape: replay the LOSSLESS walk's per-4x4 chroma states over
    // the whole-block record above, so the NEXT block reads the per-unit DC
    // signs and levels libaom's plane-major walk left behind instead of one
    // composed state for the whole plane block.
    if let Some((uo, vo)) = ll_chroma {
        for (plane_idx, comp) in [(1usize, &uo), (2, &vo)] {
            for ur in 0..ch / 4 {
                for uc in 0..cw / 4 {
                    let mut unit = Vec::with_capacity(16);
                    for r in 0..4 {
                        unit.extend_from_slice(&comp[(ur * 4 + r) * cside + uc * 4..][..4]);
                    }
                    neighbours.record_mi_chroma(
                        // lane-av1lm444loss-corr: the MI STEP between chroma
                        // units is that same footprint expressed in luma mi
                        // cells, `(4 << ss) / MI` -- which is exactly what
                        // `decode_inter_block`'s reference `mu_chroma_units`
                        // construction writes (decode.rs:43688,
                        // `at.0 + cr * ((cu << ss_y(fctx)) / MI)` with
                        // `cu = 4` on a lossless frame). The `4 >> ss_y` form
                        // this replaces is 4:2:0-only arithmetic that happens
                        // to agree there: at ss (1,1) it is `4 >> 1` = 2 mi
                        // and the footprint form is `(4 << 1) / 4` = 2 mi, the
                        // SAME value, so 4:2:0 is unchanged by construction.
                        // At ss (0,0) it is `4 >> 0` = 4 mi -- four luma mi
                        // (16 luma px) per 4x4 chroma unit, where a TX_4X4
                        // chroma unit covers `4 << 0` = 4 luma px = ONE mi.
                        // The step therefore lands every replayed unit at 4x
                        // its own mi row/column on a 4:4:4 lossless frame, and
                        // the whole block's per-unit states are stamped over
                        // cells that belong to other blocks (or to nothing).
                        (
                            mi_r + ur * ((4 << ss_y(fctx)) / MI),
                            mi_c + uc * ((4 << ss_x(fctx)) / MI),
                        ),
                        4 << ss_x(fctx),
                        4 << ss_y(fctx),
                        plane_idx,
                        &Grid::Own(unit),
                    );
                }
            }
        }
    }
    neighbours.record_palette_y_rect((mi_r, mi_c), bw, bh, 0, [0u16; 8]);
    neighbours.record_palette_uv_rect((mi_r, mi_c), bw, bh, 0, [0u16; 8]);
    neighbours.fill_skip_grid_rect((mi_r, mi_c), bw / MI, bh / MI, skip);
    // lane-av1oddheightfork3: `read_block_tx_size_rect` ran above, so the
    // bands are the tree's own per-leaf publication (libaom's var-tx branch
    // never calls `set_txfm_ctxs`) -- publishing the block's own size over
    // them here is the defect this call site carried.
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
    Ok(())
}

/// lane-av1-ibcrect: reconstruction for a `use_intrabc` block on a 128-root
/// HORZ/VERT strip (`BLOCK_128X64` / `BLOCK_64X128`) -- the shape
/// `decode_block_128rect` refused by name before this port. libaom decodes
/// such a block through the INTER machinery (an intrabc block
/// `is_inter_block`): `predict_inter_block` copies the whole block from this
/// frame's own reconstruction at the integer block vector once, then
/// `decode_token_recon_block`'s inter arm walks 64x64 "mu" chunks --
/// per chunk every plane, luma units at the block's own resolved transform
/// (`read_block_tx_size_rect`, TX_64X64 roots) and chroma at
/// `av1_get_max_uv_txsize(BLOCK_128X64)` = TX_32X32, one unit per plane per
/// chunk at 4:2:0. Chroma never codes its own `tx_type`; each unit inherits
/// the coded type of the luma leaf covering its quadrant (`av1_get_tx_type`,
/// blockd.h:1291). `mode`/`uv_mode` read as `DC_PRED`/`UV_DC_PRED` back in
/// [`read_intra_mode_rect`]; no palette/filter-intra/CFL symbol exists here.
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_intrabc_128rect(
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
    fctx: &crate::decode::FrameCtx,
) -> Result<()> {
    hit!(INTRABC_128RECT_HITS);
    if crate::envflags::env_flag!("EC_AV1_TRACE") {
        let (rng, _) = dec.debug_state();
        eprintln!(
            "EC_IBC128 mi_row={mi_r} mi_col={mi_c} bw={bw} bh={bh} skip={skip} dv=({dv_row},{dv_col}) rng={rng}"
        );
    }
    let (px, py) = (mi_c * MI, mi_r * MI);
    // lane-av1-ibc128chunk: the block's chroma PLANE BLOCK is per-axis
    // `(bw >> ss_x) x (bh >> ss_y)` -- 4:2:0 halves both (this fn's
    // `mv_to_q4(cpy, dv_row, ss_y(fctx))` calls two lines below already
    // scaled the DV on the y axis by ITS OWN subsampling, so the hardcoded
    // `/2` origin and extent made the prediction window half as tall as
    // the motion vector asked for), 4:2:2 is HALF-WIDTH and FULL-HEIGHT,
    // 4:4:4 is luma-sized. At ss (1,1) every value below is the old
    // `bw / 2, bh / 2` line-for-line.
    let (cpx, cpy) = (px >> ss_x(fctx), py >> ss_y(fctx));
    let (cw, ch) = (bw >> ss_x(fctx), bh >> ss_y(fctx));
    // The square strides the proven INTER path lays its residual grids,
    // prediction windows and `TxParams` at (see [`decode_intrabc_rect`]'s r2
    // note): 128/64 here.
    let side = bw.max(bh);
    let cside = cw.max(ch);
    // `av1_calc_mi_size`'s 8-px-aligned grid: units past the frame edge code
    // nothing (`decode_reconstruct_tx`'s max_blocks_* early return).
    let mi_cols = 2 * y.true_width.div_ceil(8);
    let mi_rows = 2 * y.true_height.div_ceil(8);
    // An intrabc block is is_inter_block: the tx tree is the INTER one, and
    // [`read_block_tx_size_rect`]'s 128-root branch already carries the
    // LARGEST (no symbol, two TX_64X64 units), TX_MODE_SELECT (var-tx tree
    // rooted at TX_64X64), skip (band stamp only) and lossless (TX_4X4 grid)
    // arms (lane-sb128c r7).
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
    // Whole-block prediction, once, before any coefficient is read
    // (`predict_inter_block`); the source is this frame's own
    // reconstruction, including the writes this superblock has queued.
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
    let mut pred_u_dense = vec![0u16; cw * ch];
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
        &mut pred_u_dense,
        fctx,
    );
    let mut pred_u = vec![0u16; cside * cside];
    for row in 0..ch {
        pred_u[row * cside..][..cw].copy_from_slice(&pred_u_dense[row * cw..][..cw]);
    }
    let mut pred_v_dense = vec![0u16; cw * ch];
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
        &mut pred_v_dense,
        fctx,
    );
    let mut pred_v = vec![0u16; cside * cside];
    for row in 0..ch {
        pred_v[row * cside..][..cw].copy_from_slice(&pred_v_dense[row * cw..][..cw]);
    }
    let (luma_grid, u_grid, v_grid);
    // lane-av1-ibcrect: whether the coded arm stamped its CHROMA neighbour
    // contexts per transform unit -- the replay after this fn's end-of-block
    // record restores them (the record itself re-stamps both planes with ONE
    // composed state, class `override-slot-on-one-arm`).
    let mut mu_chroma = false;
    if skip {
        // No residual syntax at all: the prediction IS the block. Strides are
        // `side`/`cside`, not `bw` -- `reconstruct_mc_rect` indexes
        // `row * stride + w` for row in 0..h (the r5 VERT-strip overrun).
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
            &ZERO_RESIDUAL[..side * side],
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
            &ZERO_RESIDUAL[..cside * cside],
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
            &ZERO_RESIDUAL[..cside * cside],
            fctx,
        );
        if let Some(ls) = leaves.as_ref() {
            // A LOSSLESS frame's `read_block_tx_size_rect` resolves the TX_4X4
            // grid before it looks at `skip`; the skipped block still zeroes
            // its entropy-band footprint over every unit (libaom
            // `av1_reset_entropy_context` under `if (mbmi->skip_txfm)`) --
            // the same arm [`decode_intrabc_rect`]'s loss64 merge added.
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
    } else {
        let leaves = leaves
            .expect("read_block_tx_size_rect returns None only for skip below 128 at this shape");
        // libaom `get_transform_size`'s split override applies to LUMA only,
        // per leaf -- each unit's own transform edges get filtered
        // ([`decode_intrabc_rect`]'s P2, the proven inter path's step).
        for &(row, col, tw, th) in leaves.iter() {
            neighbours.fill_lf_grid_leaf_luma(
                (mi_r + row, mi_c + col),
                tw / MI,
                th / MI,
                tw as u8,
                th as u8,
            );
        }
        let reduced = fctx.reduced_tx_set_inter.with(std::cell::Cell::get);
        let mut u_units: Vec<i32> = Vec::new();
        let mut v_units: Vec<i32> = Vec::new();
        let mut leaf_tx_types: Vec<(usize, usize, usize, usize, TxType)> = Vec::new();
        // Frame-edge clip of the mu walk (`max_block_wide/high`): a unit whose
        // top-left sample is outside the frame codes nothing.
        let max_w_mi = (bw / MI).min(mi_cols.saturating_sub(mi_c));
        let max_h_mi = (bh / MI).min(mi_rows.saturating_sub(mi_r));
        // lane-av1h5: chunk-major token order at 128 (see [`mu_chunk_order`]).
        let (order, _mu_reordered) = mu_chunk_order(&leaves, side);
        for (pos, &idx) in order.iter().enumerate() {
            let (row, col, tw, th) = leaves[idx];
            debug_assert_eq!(
                tw, th,
                "a square TX_64X64 tree root only yields square leaves"
            );
            let tu_mi = (mi_r + row, mi_c + col);
            let (tu_px, tu_py) = (px + col * MI, py + row * MI);
            if tu_px >= y.true_width || tu_py >= y.true_height {
                continue;
            }
            let tu_pred = Pred::Inline(&pred_y, side).offset(row * MI * side + col * MI);
            let scan = default_scan(tw.min(32));
            let tu_skip_ctx = neighbours.luma_skip_ctx(tu_mi, tw / MI);
            let (tu_grid, tu_tx_type) = read_inter_plane(
                dec,
                cdfs,
                inter_txbset_for(tw, reduced),
                scan,
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
                Some(tu_skip_ctx),
                fctx,
            )?;
            leaf_tx_types.push((row, col, tw, th, tu_tx_type));
            neighbours.record_mi_luma_rect(tu_mi, tw, th, &tu_grid);
            // libaom `decode_token_recon_block` codes EVERY PLANE of a 64x64
            // mu chunk before the next one starts: once this chunk's leaves
            // are done, its chroma is read immediately (the inter 128 path's
            // interleave).
            let chunk = (row / 16, col / 16);
            let done = pos + 1 == order.len()
                || (leaves[order[pos + 1]].0 / 16, leaves[order[pos + 1]].1 / 16) != chunk;
            if done {
                let (cr, cc) = chunk;
                // `av1_get_max_uv_txsize(BLOCK_128X64)` = TX_32X32: one
                // TX_32X32 chroma unit per plane per chunk at 4:2:0, raster
                // within the chunk, plane-major inside the chunk.
                let cu_tx = 32usize;
                // lane-av1-ibc128chunk: a 64x64 mu chunk's chroma plane
                // block is PER-AXIS `(64 >> ss_x) x (64 >> ss_y)` -- 32x32 at
                // 4:2:0, TWO STACKED TX_32X32 at 4:2:2 (the chunk's chroma
                // plane block is 32x64 there), four at 4:4:4. The old
                // `chunk_chroma = cside * 64 / side` walked a
                // `chroma_side`-SQUARE chunk, i.e. the 4:2:0 count on both
                // axes: at 4:2:2 it read HALF the chroma units of a chunk
                // (one 32x32 at the chunk's top-left instead of two), left
                // the chunk's lower 32 chroma rows with no residual at all,
                // and stamped every unit's entropy context over a 16x16-mi
                // luma span instead of 16x8. The same defect and the same
                // per-axis repair as `decode_inter_block`'s
                // `chunk_chroma_w/chunk_chroma_h` (lane-av1-422bigblock,
                // witnessed on `422_sb128_3f.obu`).
                let chunk_chroma_w = (64usize) >> ss_x(fctx);
                let chunk_chroma_h = (64usize) >> ss_y(fctx);
                // The unit's own footprint on the LUMA mi grid, per axis:
                // `(32 << ss) px` = `(32 << ss) / MI` mi -- 16x16 at 4:2:0,
                // 16x8 at 4:2:2, 8x8 at 4:4:4. `around_mi_rect` reads and
                // `record_mi_chroma` stamp exactly the unit's own cells.
                let unit_luma_w = cu_tx << ss_x(fctx);
                let unit_luma_h = cu_tx << ss_y(fctx);
                INTRABC_128RECT_CHUNK_SHAPE.with(|c| {
                    c.set((chunk_chroma_w << 8) | chunk_chroma_h);
                });
                let chunk_w_mi = 16.min(max_w_mi.saturating_sub(cc * 16));
                let chunk_h_mi = 16.min(max_h_mi.saturating_sub(cr * 16));
                let units_x = chunk_w_mi / (unit_luma_w / MI);
                let units_y = chunk_h_mi / (unit_luma_h / MI);
                if lossless(fctx) {
                    // TX_4X4 chroma units over this chunk (lane-lossless2's
                    // walk, shared with the inter 128 path).
                    read_inter_chroma_lossless(
                        y,
                        u,
                        v,
                        dec,
                        cdfs,
                        neighbours,
                        Pred::Inline(&pred_u, cside),
                        Pred::Inline(&pred_v, cside),
                        (mi_r, mi_c),
                        (cpx, cpy),
                        (cc * chunk_chroma_w, cr * chunk_chroma_h),
                        (chunk_chroma_w, chunk_chroma_h),
                        (cw, ch),
                        cside,
                        0,
                        base_q_idx,
                        &mut u_units,
                        &mut v_units,
                        fctx,
                    )?;
                    // lane-av1-ibcrect: read_inter_chroma_lossless stamped
                    // every TX_4X4 unit's own context (`record_mi_chroma`
                    // inside the walk) -- the end-of-block record below must
                    // not overwrite them with the composed grid (the inter
                    // 128 path sets the same flag on this arm).
                    mu_chroma = true;
                    continue;
                }
                let cu_scan = default_scan(cu_tx);
                for plane_idx in 1..3 {
                    let src = if plane_idx == 1 {
                        Pred::Inline(&pred_u, cside)
                    } else {
                        Pred::Inline(&pred_v, cside)
                    };
                    for ur in 0..units_y {
                        for uc in 0..units_x {
                            let unit_mi = (
                                mi_r + cr * 16 + ur * (unit_luma_h / MI),
                                mi_c + cc * 16 + uc * (unit_luma_w / MI),
                            );
                            // Per unit, AFTER the earlier units' stamps: this
                            // unit's coefficient context must see them.
                            //
                            // lane-av1muchunk422sym: same 4:2:2 pair rule as
                            // `decode_inter_block`'s mu-chunk walk -- at ss
                            // (1, 0) one CHROMA column spans two luma mi
                            // columns, so the plain rect gather doubles the
                            // above dc vote and flips `dc_sign_ctx` whenever
                            // above and left cancel.
                            let cu_around = if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                                neighbours.around_mi_422_chroma(unit_mi, unit_luma_w, unit_luma_h)
                            } else {
                                neighbours.around_mi_rect(unit_mi, unit_luma_w, unit_luma_h)
                            };
                            if crate::envflags::env_flag!("EC_ECDUMP") {
                                // Index loop mirrors the spec's per-position form over several offsets; an iterator would obscure it.
                                #[allow(clippy::needless_range_loop)]
                                for p in 1..3 {
                                    let ab: Vec<String> = (0..unit_luma_w / MI)
                                        .map(|k| {
                                            format!(
                                                "{:?}/{}",
                                                neighbours.above[unit_mi.1 + k][p].dc,
                                                neighbours.above[unit_mi.1 + k][p].level
                                            )
                                        })
                                        .collect();
                                    let lf: Vec<String> = (0..unit_luma_h / MI)
                                        .map(|k| {
                                            format!(
                                                "{:?}/{}",
                                                neighbours.left[unit_mi.0 + k][p].dc,
                                                neighbours.left[unit_mi.0 + k][p].level
                                            )
                                        })
                                        .collect();
                                    eprintln!(
                                        "EC_IBC128_BANDS mi=({mi_r},{mi_c}) unit=({cr},{cc}) plane={p} ctx=({},{},{}) above=[{}] left=[{}]",
                                        cu_around[p].0,
                                        cu_around[p].1,
                                        cu_around[p].2,
                                        ab.join(","),
                                        lf.join(",")
                                    );
                                }
                            }
                            let (cu_x, cu_y) = (
                                cpx + cc * chunk_chroma_w + uc * cu_tx,
                                cpy + cr * chunk_chroma_h + ur * cu_tx,
                            );
                            // `av1_get_tx_type` (blockd.h:1291): an inter
                            // block's chroma unit never codes a tx_type; it
                            // inherits the coded type of the luma leaf
                            // covering its own quadrant (all of this chunk's
                            // leaves are read by now).
                            let unit_rel_mi = (unit_mi.0 - mi_r, unit_mi.1 - mi_c);
                            let covering = covering_leaf_tx_type(&leaf_tx_types, unit_rel_mi);
                            // The prediction window is the block's own square
                            // `cside` buffer with the unit's true per-axis
                            // position inside it.
                            let cu_pred = src.offset(
                                (cr * chunk_chroma_h + ur * cu_tx) * cside
                                    + cc * chunk_chroma_w
                                    + uc * cu_tx,
                            );
                            let cu_grid = read_inter_plane(
                                dec,
                                cdfs,
                                TxbSet::Chroma32,
                                cu_scan,
                                plane_idx,
                                cu_around[plane_idx],
                                0,
                                y,
                                u,
                                v,
                                cu_x,
                                cu_y,
                                cu_tx,
                                base_q_idx,
                                cu_pred,
                                Some(covering.unwrap_or(TxType::DctDct)),
                                Some(3),
                                fctx,
                            )?
                            .0;
                            // Immediately, so the NEXT unit reads this one's
                            // coefficient context.
                            neighbours.record_mi_chroma(
                                unit_mi,
                                unit_luma_w,
                                unit_luma_h,
                                plane_idx,
                                &cu_grid,
                            );
                            if crate::envflags::env_flag!("EC_ECDUMP") {
                                let st = crate::decode::neighbour_state(&cu_grid);
                                eprintln!(
                                    "EC_IBC128_STAMP mi=({mi_r},{mi_c}) unit=({cr},{cc}) plane={plane_idx} state=({:?},{})",
                                    st.dc, st.level
                                );
                            }
                            hit!(CHROMA_SPLIT_TX_HITS);
                            INTRABC_128RECT_CHROMA_CHUNK_HITS.with(|c| c.set(c.get() + 1));
                            // lane-av1-ibcrect: per-unit chroma context -- the
                            // end-of-block record's composed-grid re-stamp
                            // must not stand (replayed below).
                            mu_chroma = true;
                            let dst = mu_units(
                                if plane_idx == 1 {
                                    &mut u_units
                                } else {
                                    &mut v_units
                                },
                                cside * cside,
                            );
                            for rr in 0..cu_tx {
                                let start = (cr * chunk_chroma_h + ur * cu_tx + rr) * cside
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
        // The block-level luma bands were recorded per leaf above; the
        // composed chroma grids carry every unit in place.
        luma_grid = Grid::Zero(side * side);
        u_grid = if u_units.is_empty() {
            Grid::Zero(cside * cside)
        } else {
            Grid::Own(std::mem::take(&mut u_units))
        };
        v_grid = if v_units.is_empty() {
            Grid::Zero(cside * cside)
        } else {
            Grid::Own(std::mem::take(&mut v_units))
        };
    }
    // Bookkeeping (see [`decode_intrabc_rect`]): DC_PRED everywhere a
    // neighbour reads a mode, zeroed palette state, the skip grid, the
    // TXFM_CONTEXT bands (`read_block_tx_size_rect` already stamped them),
    // and the DV into the intrabc mi grid.
    if skip {
        neighbours.record_rect_mi(
            (mi_r, mi_c),
            bw,
            bh,
            DC_PRED,
            DC_PRED,
            &[&luma_grid[..], &u_grid[..], &v_grid[..]],
        );
    } else {
        neighbours.record_split_luma_rect_mi(
            (mi_r, mi_c),
            bw,
            bh,
            DC_PRED,
            DC_PRED,
            [&u_grid[..], &v_grid[..]],
        );
    }
    // lane-av1-ibcrect: the coded arm's chroma contexts were stamped PER
    // TRANSFORM UNIT (libaom `av1_set_entropy_contexts` inside
    // `decode_reconstruct_tx`'s mu-chunk loop), but the record above re-stamps
    // both planes with ONE `neighbour_state(composed_grid)` over the whole
    // span -- level = the SUM of every unit's levels and dc = the top-left
    // unit's sign (lane-vert46 r1 measured the same smearing at the 64x128
    // inter block: the next block read a stale `dc_sign_ctx` one row low).
    // Measured on this shape: the 128x64 block at mi(64,32) of the
    // `ibcrect-hunt/E2` sb128 screen stream left chunk-1 U's level 1 smeared
    // into chunk-0's cells and the block below read `txb_skip` off the sum.
    // Replay every coded unit's own state after the record, in mu-chunk order
    // so a later unit still wins over an earlier one on the shared seam cells
    // (the composed construction `mu_chroma_units` uses, transcribed here).
    if mu_chroma {
        let cu = if lossless(fctx) { 4usize } else { 32usize };
        // The unit's own footprint on the LUMA mi grid (the chroma context
        // bands are luma-mi indexed, [`Self::record_mi_chroma`]'s
        // convention), PER AXIS: `cu << ss` px, i.e. 8 px per TX_4x4 unit and
        // 64 px per TX_32X32 one at 4:2:0 -- 4 px / 32 px at 4:2:2.
        let unit_luma_w = cu << ss_x(fctx);
        let unit_luma_h = cu << ss_y(fctx);
        // A 64x64 mu chunk's chroma plane block is PER-AXIS
        // `(64 >> ss_x) x (64 >> ss_y)` -- 32x32 at 4:2:0 (ONE TX_32X32 per
        // chunk), 32x64 at 4:2:2 (TWO STACKED), 64x64 at 4:4:4 (FOUR);
        // `av1_get_max_uv_txsize` caps the unit at 32 on each axis.
        let chunk_chroma_w = (64usize) >> ss_x(fctx);
        let chunk_chroma_h = (64usize) >> ss_y(fctx);
        if lossless(fctx) {
            // lane-av1-ibc128chunk, PRE-EXISTING and NOT this lane's: on a
            // LOSSLESS frame `cu` is 4 and the composed plane grid is a raster
            // of TX_4x4 units, not a `cu`-per-chunk tiling -- so `rows`/`cols`
            // below (which count mu CHUNKS at the 4:2:0 chunk extent) do not
            // enumerate it, and this arm re-stamps only the 4x4 at each
            // chunk's top-left with a 2x2-mi span. The twin
            // (`decode_inter_block`'s `mu_chroma_units`, lane-av1-llinter)
            // instead raster-walks the WHOLE plane block at `cu` steps, and
            // that is the correct shape here too. It is left as-is on purpose:
            // it is a 4:2:0 defect, not a subsampling square-cut, and fixing
            // it would change 4:2:0 LOSSLESS output -- which this lane's
            // byte-identity requirement forbids doing on an unverified
            // reconstruction. Measured unreachability: this arm is entered
            // 0 times by the whole `lossless` and `intrabc` gate battery
            // (temporary env-gated counter in the arm). Unblock: one
            // lossless sb128 4:2:0 stream coding a NON-SKIP 128-root
            // intrabc strip (`--lossless=1 --sb-size=128
            // --min-partition-size=64 --enable-rect-partitions=1
            // --enable-intrabc=1` over near-match screen content).
            let (rows, cols) = ((ch / chunk_chroma_h).max(1), (cw / chunk_chroma_w).max(1));
            for cr in 0..rows {
                for cc in 0..cols {
                    let unit_mi = (
                        mi_r + cr * (unit_luma_h / MI),
                        mi_c + cc * (unit_luma_w / MI),
                    );
                    for (plane_idx, grid) in [(1usize, &u_grid), (2usize, &v_grid)] {
                        let mut unit = Vec::with_capacity(cu * cu);
                        for rr in 0..cu {
                            let start = (cr * chunk_chroma_h + rr) * cside + cc * chunk_chroma_w;
                            unit.extend_from_slice(&grid[start..start + cu]);
                        }
                        neighbours.record_mi_chroma(
                            unit_mi,
                            unit_luma_w,
                            unit_luma_h,
                            plane_idx,
                            &unit,
                        );
                    }
                }
            }
        } else {
            // The chunk count on each axis, then EVERY unit inside each
            // chunk at its own mi origin, its own `(cu << ss)`-px entropy-cell
            // span, and its own quadrant of the composed plane grid -- the
            // twin's loop (decode_inter_block's `mu_chroma_units`,
            // lane-av1-444 c8). lane-av1-ibc128chunk: the loop used to count
            // mu CHUNKS in `rows`/`cols` and then step the mi origin and read
            // the grid PER UNIT, which only agrees when a chunk holds exactly
            // one unit. At 4:2:0 (`chunk_chroma == cu`) it is
            // line-identical to the old walk; at 4:2:2 and 4:4:4 a chunk holds
            // 2 or 4, so the old body reached only the first of the stacked
            // units and the rest of the chunk kept the composed-grid smearing
            // this replay exists to undo. A unit the frame edge clipped codes
            // nothing and carries no composed samples beyond the clip, so it
            // replays as the zero state exactly like libaom's uncoded unit.
            let (ur, uc) = (chunk_chroma_h / cu, chunk_chroma_w / cu);
            let (rows, cols) = ((ch / chunk_chroma_h).max(1), (cw / chunk_chroma_w).max(1));
            for cr in 0..rows {
                for cc in 0..cols {
                    for kr in 0..ur {
                        for kc in 0..uc {
                            let unit_mi = (
                                mi_r + (cr * 64 + kr * unit_luma_h) / MI,
                                mi_c + (cc * 64 + kc * unit_luma_w) / MI,
                            );
                            let (cy, cx) =
                                (cr * chunk_chroma_h + kr * cu, cc * chunk_chroma_w + kc * cu);
                            for (plane_idx, grid) in [(1usize, &u_grid), (2usize, &v_grid)] {
                                let mut unit = Vec::with_capacity(cu * cu);
                                for rr in 0..cu {
                                    let start = (cy + rr) * cside + cx;
                                    unit.extend_from_slice(&grid[start..start + cu]);
                                }
                                neighbours.record_mi_chroma(
                                    unit_mi,
                                    unit_luma_w,
                                    unit_luma_h,
                                    plane_idx,
                                    &unit,
                                );
                            }
                        }
                    }
                }
            }
        }
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
    Ok(())
}

/// Decodes one true `bw`x`bh` `PARTITION_HORZ`/`PARTITION_VERT` intra strip
/// (lane-intradisp r1, spec `decode_block` restricted to a 32x32 quadrant's
/// two rect children). Only the `skip` case is supported: `skip`/`y_mode`/
/// `uv_mode`/`filter_intra`/`tx_depth` are read exactly per spec either way
/// (so a skip strip elsewhere in the tile stays in sync), but this decoder
/// has no rectangular-transform coefficient reader, and a chroma plane here
/// is genuinely `TX_16X8`/`TX_8X16` (`av1_get_max_uv_txsize`, spec 5.11.16)
/// with no depth-based square-tiling escape the way luma's own resolved
/// `tx_depth` has (`sub_tx_size_map[TX_32X16]` == `TX_16X16`, square) --
/// so a coded (non-skip) strip is refused by name instead of guess-decoded.
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_block_rect(
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
        true,
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
        return decode_intrabc_rect(
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
            fctx,
        );
    }
    let smooth_neighbor = is_smooth_mode(nb_above_mode) || is_smooth_mode(nb_left_mode);
    if smooth_neighbor {
        hit!(SMOOTH_LUMA_HITS);
    }
    // lane-chroma r3: chroma's own edge-filter-strength neighbour check
    // (spec `get_intra_edge_filter_type`) reads the CHROMA neighbour's
    // `uv_mode`, not the luma one.
    let smooth_neighbor_uv =
        neighbours.smooth_uv_neighbour(r * (SUB / MI), c * (SUB / MI), r, c, fctx);
    // lane-rectsplit r1: `predict_filter_intra` now takes the block's own
    // `bw`x`bh` (`av1_filter_intra_predictor_c` walks its 4x2 patches over a
    // rectangle just as happily), so a `use_filter_intra` strip at this level
    // predicts instead of refusing. The below-16x16 leaf arm
    // (`decode_leaf_rect`) keeps the refusal: it has no gate of its own.
    if filter_intra.is_some() {
        hit!(FILTER_INTRA_RECT_HITS);
    }
    let uv_predict_mode = if uv_mode == UV_CFL_PRED {
        DC_PRED
    } else {
        uv_mode
    };
    let (mi_r, mi_c) = (r * (SUB / MI), c * (SUB / MI));
    // The tx-depth symbol EXISTS only under TX_MODE_SELECT. Both square paths
    // gate their `read_tx_size` on `tx_select` for exactly this reason; this
    // one did not, so with `--enable-tx-size-search=0` -- which several gate
    // recipes use -- the decoder consumed a symbol the encoder never wrote and
    // desynced the tile from that point on. That is why both strips of the
    // pinned HORZ quadrant were wrong from their very first pixel, and why our
    // range after the read equalled the oracle's range for the whole block.
    // lane-av1mixloss: the lossless-segment early return
    // (`decodeframe.c:1203`) precedes `TX_MODE_SELECT`, so a lossless-segment
    // rect block codes no depth symbol here either (class sweep of the sub8
    // leaf's fix).
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
    // lane-av1-intrabc r3: this strip's own `set_txfm_ctxs` when the frame
    // reads the `TXFM_CONTEXT` bands -- see [`publish_txfm_bands_if_in_inter`].
    publish_txfm_bands_if_in_inter(
        neighbours,
        (mi_r, mi_c),
        depth_to_tx_wh(bw, bh, depth, fctx),
        (bw, bh),
        fctx,
    );
    if depth != 0 || lossless(fctx) {
        hit!(TX_DEPTH_HITS);
        // lane-rectsplit r1: a split transform is predicted and reconstructed
        // per transform unit, each unit taking its edges from the previous
        // unit's reconstruction inside this same strip (spec 5.11.36) --
        // [`decode_rect_split`], the unit named by [`depth_to_tx_wh`].
        let (tx_w, tx_h) = depth_to_tx_wh(bw, bh, depth, fctx);
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
        // This early return skips this fn's own tail, so the palette
        // neighbour state has to be stamped HERE too: without it a split-tx
        // strip left the above/left palette arrays holding a DEAD block's
        // colours, and the next block's colour cache
        // ([`Neighbours::palette_uv_cache`]) then hit a cached entry with the
        // wrong VALUE -- same bit count, so the entropy stream stayed in sync
        // and only those pixels were wrong (lane-palette2 r10).
        let (py_size, py_colors) = palette_y
            .as_ref()
            .map_or((0, [0u16; 8]), |p| (p.size, p.colors));
        neighbours.record_palette_y_rect((mi_r, mi_c), bw, bh, py_size, py_colors);
        let (puv_size, puv_colors) = palette_uv
            .as_ref()
            .map_or((0, [0u16; 8]), |p| (p.size, p.u_colors));
        neighbours.record_palette_uv_rect((mi_r, mi_c), bw, bh, puv_size, puv_colors);
        hit!(RECT_PARTITION_HITS);
        return Ok(());
    }
    let (tx_w, tx_h) = (bw, bh);
    let (cpx, cpy) = (px >> ss_x(fctx), py >> ss_y(fctx));
    let (chroma_w, chroma_h) = (bw >> ss_x(fctx), bh >> ss_y(fctx));
    let reach = Reach::of_rect(bw, bh, px, py, y.width, y.height, fctx);
    if crate::envflags::env_flag!("EC_AV1_TRACE") {
        let (rng, _) = dec.debug_state();
        eprintln!(
            "TRACE_RECT_IMODE mi_row={mi_r} mi_col={mi_c} mode={mode} uv_mode={uv_mode} \
             skip={} tx={depth} rng={rng}",
            skip as i32
        );
    }
    // As [`decode_block`]'s own [`PALETTE_PRED`] wiring: a palette-Y/UV
    // block's prediction is the reconstructed colour-index map, set fresh
    // right before each `reconstruct_rect` call below (lane-palette2 r4).
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
        // lane-rectwire r2: real coefficients. `get_txsize_entropy_ctx`
        // reduces both size pairs to their square-up CDF sets
        // (`TxbSet::LumaRect32x16`/`ChromaRect16x8`, see those variants' own
        // doc comments); the scan order and 2D context tables are the real
        // rect ones (`SCAN_32X16`/etc, `base_ctx_rect`/`br_ctx_rect`).
        let luma_scan: &[u16] = if bw > bh { &SCAN_32X16 } else { &SCAN_16X32 };
        // Chroma tx follows the plane block, not a hardcoded half. At 4:2:0 a
        // 32x16 strip's chroma is 16x8 (ChromaRect16x8); at 4:4:4 it is 32x16
        // (ChromaRect32x16, DCT-only — ADST is undefined on a 32-point axis).
        let (chroma_set, chroma_scan): (TxbSet, &[u16]) = match (chroma_w, chroma_h) {
            (32, 16) => (TxbSet::ChromaRect32x16, &SCAN_32X16),
            (16, 32) => (TxbSet::ChromaRect32x16, &SCAN_16X32),
            (16, 8) => (TxbSet::ChromaRect16x8, &SCAN_16X8),
            (8, 16) => (TxbSet::ChromaRect16x8, &SCAN_8X16),
            // lane-av1-422: a 4:2:2 strip halves only the width. A 32x16
            // luma strip's chroma is a 16x16 SQUARE (`TxbSet::Chroma16`, the
            // set any 16x16 chroma plane reads); a 16x32 strip's chroma is
            // 8x32, whose entropy set is the 16x16 one and whose DCT-only
            // default follows the >=32 rule below -- the same pairing the
            // (8,32) row of [`decode_rect_split`]'s chroma table proves. At
            // 4:2:0 and 4:4:4 a strip's chroma is itself rect, so neither row
            // is reachable there.
            (16, 16) => (TxbSet::Chroma16, default_scan(16)),
            (8, 32) => (TxbSet::Chroma16, &SCAN_8X32),
            _ => {
                return Err(unsupported(
                    "a rectangular chroma transform whose size has no coefficient table",
                ));
            }
        };
        let chroma_default_tx = if chroma_w.max(chroma_h) >= 32 {
            TxType::DctDct
        } else {
            default_intra_tx_type(uv_predict_mode as u8)
        };
        // lane-rectwire r3: real coefficients desync a pixel-exact gate
        // (rectwire-flake-1.obu, seed 55) -- both 32x16 strips of frame 0's
        // mi(8,8) HORZ quadrant come out ~100% wrong from their very first
        // pixel (not a subtle context-table off-by-one), while the sibling
        // non-rect quadrants of the same frame match. Range-ladder bisection
        // against a real aomdec (EC_TRACE_COEFF/EC_TRACE_MODE) was started
        // but the intra key-frame path has no traced symbol between the
        // partition read and the first coefficient block (mode/skip/tx_depth
        // reads are all untraced in libaom's decodeframe.c intra path, unlike
        // the inter EC_TRACE_MODE this round added), so a same-length range
        // comparison wasn't reachable within budget -- see
        // lanes/rectwire-r3.report.md for what was ruled out (base_ctx_rect's
        // 5-neighbour offsets, u/v_skip_ctx's above+left formula, and
        // dc_sign_ctx's around-index all cross-checked byte-for-byte against
        // libaom and match). lane-rectwire r4: range-ladder against the
        // now-instrumented oracle (EC_TRACE_MODE's intra EC_IMODE/EC_IMODE_VAL)
        // narrowed the divergence past skip/y_mode/uv_mode (values AND range
        // both confirmed in sync -- rng=58692 on both sides right after
        // uv_mode, with `enable_filter_intra=0` on this frame so neither side
        // reads a filter_intra symbol) to the `tx_size_cat2` depth read
        // immediately after: ours moves rng 58692 -> 43570, which cannot be
        // reconciled with oracle's own EC_IMODE_VAL (rng=58692, taken *after*
        // its own tx_size read) unless oracle's tx symbol is a near-certainty
        // CDF that barely narrows the range -- i.e. this decoder's
        // tx-depth ctx or `cdfs.tx_size_cat2` CDF selection for
        // a 32x16/16x32 strip is the first diverging symbol, not the
        // coefficient-context math r3 already cleared. Not confirmed byte-
        // exact (would need a decodemv.c EC_TRACE_MODE patch specifically on
        // `read_tx_size`'s own rng, not attempted this round -- budget), so
        // r5 CONFIRMED the suspect and it was simpler than the CDF row: the
        // tx-depth symbol exists only under TX_MODE_SELECT, and this path read
        // it unconditionally where both square paths gate on `tx_select`. With
        // --enable-tx-size-search=0 the encoder writes no such symbol, so the
        // decoder consumed one that was never there.
        {
            // lane-av1422warp r20: this 32x16/16x32 strip reader is the SIBLING of
            // `decode_leaf_rect`, and it was the one left ungated: at ss (1,0) the
            // strip's chroma is a square (side/2, side) block whose above cells
            // spread over `side` LUMA mi columns, so the per-mi rect gather
            // double-counts every chroma column's whole-unit dc sign. Measured on
            // the pinned 4:2:2 rotzoom stream (a1.obu frame 0): the chroma-U 4x4
            // transform of the 32x16 strip at mi(68,16) reads its DC sign from
            // `dc_sign_ctx` 1 where libaom reads row 0 -- the first read of
            // 45260 whose pre-state, bit position and symbol all match and only
            // the CDF row differs, and the first divergence in the EC_SYMR
            // sequence (all 45259 reads before it pair exactly). Luma keeps the
            // per-mi gather; 4:2:0 and 4:4:4 keep the plain rect walk verbatim.
            let mut around = neighbours.around_rect(at, bw, bh);
            if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                let c2 =
                    neighbours.around_mi_422_chroma((at.0 * (SUB / MI), at.1 * (SUB / MI)), bw, bh);
                around[1] = c2[1];
                around[2] = c2[2];
            }
            let mut luma_coding = cdfs.txb(TxbSet::LumaRect32x16, fi_tx_row(mode, filter_intra));
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
            if crate::envflags::env_flag!("EC_AV1_TRACE") {
                let (rng, _) = dec.debug_state();
                eprintln!("TRACE_RECT_COEFF plane=0 mi_row={mi_r} mi_col={mi_c} rng={rng}");
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
            let ac = alpha.map(|_| cfl_src_rect(px, py, bw, bh));
            if let Some((ub, _)) = &palette_uv_bufs {
                set_palette_pred(ub.clone(), fctx);
            }
            let u_default_tx = chroma_default_tx;
            let u_skip_ctx = usize::from(around[1].0) + usize::from(around[1].1);
            let mut u_coding = cdfs.txb(chroma_set, uv_predict_mode);
            let (u_levels, u_tx_type) = read_chroma_coeffs_rect(
                dec,
                &mut u_coding,
                chroma_scan,
                chroma_w,
                chroma_h,
                u_skip_ctx,
                dc_sign_ctx(around[1].2),
                u_default_tx,
                fctx,
                1,
            )?;
            if crate::envflags::env_flag!("EC_AV1_TRACE") {
                let (rng, _) = dec.debug_state();
                eprintln!("TRACE_RECT_COEFF plane=1 mi_row={mi_r} mi_col={mi_c} rng={rng}");
            }
            let u_residual = dequant_and_inverse_typed_wh(
                &u_levels,
                chroma_w,
                chroma_h,
                crate::decode::bit_depth(fctx),
                block_q_idx(fctx),
                plane_q_delta(1, fctx).0,
                plane_q_delta(1, fctx).1,
                u_tx_type,
                block_iqmatrix(fctx, 1, chroma_w, chroma_h, u_tx_type),
            );
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
                &u_residual,
                alpha.zip(ac).map(|((au, _), ac)| (au, ac)),
                None,
                smooth_neighbor_uv,
                fctx,
            );
            if let Some((_, vb)) = &palette_uv_bufs {
                set_palette_pred(vb.clone(), fctx);
            }
            let v_default_tx = chroma_default_tx;
            let v_skip_ctx = usize::from(around[2].0) + usize::from(around[2].1);
            let mut v_coding = cdfs.txb(chroma_set, uv_predict_mode);
            let (v_levels, v_tx_type) = read_chroma_coeffs_rect(
                dec,
                &mut v_coding,
                chroma_scan,
                chroma_w,
                chroma_h,
                v_skip_ctx,
                dc_sign_ctx(around[2].2),
                v_default_tx,
                fctx,
                2,
            )?;
            if crate::envflags::env_flag!("EC_AV1_TRACE") {
                let (rng, _) = dec.debug_state();
                eprintln!("TRACE_RECT_COEFF plane=2 mi_row={mi_r} mi_col={mi_c} rng={rng}");
            }
            let v_residual = dequant_and_inverse_typed_wh(
                &v_levels,
                chroma_w,
                chroma_h,
                crate::decode::bit_depth(fctx),
                block_q_idx(fctx),
                plane_q_delta(2, fctx).0,
                plane_q_delta(2, fctx).1,
                v_tx_type,
                block_iqmatrix(fctx, 2, chroma_w, chroma_h, v_tx_type),
            );
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
                &v_residual,
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
                &[&luma_levels[..], &u_levels[..], &v_levels[..]],
            );
            hit!(RECT_COEFF_HITS);
        }
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
    hit!(RECT_PARTITION_HITS);
    if crate::envflags::env_flag!("EC_AV1_TRACE") {
        eprintln!("TRACE_RECT32_END mi_row={mi_r} mi_col={mi_c} bw={bw} bh={bh}");
    }
    Ok(())
}

/// Decodes one `bw`x`bh` rect strip of a plain 16x16-level `PARTITION_HORZ`/
/// `PARTITION_VERT` (lane-rect16 r2), sibling to [`decode_block_rect`] but
/// addressed by REAL mi coordinates rather than derived from an [`SUB`]-grid
/// `outer_at`, the same relationship [`decode_leaf8`] has to `decode_block`:
/// the second strip of a HORZ/VERT split sits offset by half the 16x16
/// parent, which `decode_block_rect`'s `at`-derived `px = c * SUB` can't
/// name. `prev_leaf` mirrors `decode_leaf8`'s own convention -- when the
/// previous strip is directly above (same column, `mi_h` rows up) or
/// directly left (same row, `mi_w` cols over), its mode overrides the coarse
/// per-16x16-cell `above_mode`/`left_mode` slot for THIS strip's own mode
/// context read, and bookkeeping goes through the `_mi`/`_rect` neighbour
/// writers (`record_mi_rect`, `fill_skip_grid_rect`, `fill_lf_grid_rect`) at
/// this strip's real position, never the coarse `record_rect` (which the
/// caller uses once, after both strips, exactly like the `VERT_B` arm next
/// to it). The non-skip case (lane-rectx) reads real `TX_16X8`/`TX_8X16`
/// luma coefficients (`TxbSet::LumaRect16x8`) and their `TX_8X4`/`TX_4X8`
/// chroma half (`TxbSet::ChromaRect8x4`) at 4:2:0 -- an 8x8 *square* chroma
/// plane (`TxbSet::Chroma8` + the default scan) at 4:2:2 (lane-av1-422e:
/// half-width, unhalved height), the two size classes one level
/// under [`decode_block_rect`]'s own 32x16/16x32 strip.
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_leaf_rect(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    outer_at: (usize, usize),
    leaf_mi: (usize, usize),
    bw: usize,
    bh: usize,
    prev_leaf: Option<((usize, usize), usize)>,
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    enable_filter_intra: bool,
    allow_screen_content_tools: bool,
    base_q_idx: u8,
    tx_select: bool,
    reduced_tx_set: bool,
    fctx: &crate::decode::FrameCtx,
) -> Result<usize> {
    let _ = base_q_idx;
    if crate::envflags::env_flag!("EC_AV1_RECTX_TRACE") {
        eprintln!(
            "decode_leaf_rect: leaf_mi={leaf_mi:?} bw={bw} bh={bh} outer_at={outer_at:?} rng={}",
            dec.debug_state().0
        );
    }
    let (r, c) = outer_at;
    let mut above_mode = neighbours.above_mode[c];
    let mut left_mode = neighbours.left_mode[r];
    let (mi_w, mi_h) = (bw / MI, bh / MI);
    if let Some(((pr, pc), pmode)) = prev_leaf {
        if pc == leaf_mi.1 && leaf_mi.0 == pr + mi_h {
            above_mode = pmode;
        } else if pr == leaf_mi.0 && leaf_mi.1 == pc + mi_w {
            left_mode = pmode;
        }
    }
    // Same mi-exact override [`decode_leaf8`] takes: one coarse 16x16 slot
    // cannot hold the modes a split (or a pair of strips) leaves behind, so
    // when the map holds the block at exactly this strip's above/left mi it
    // is the neighbour libaom's `above_mi`/`left_mi` reads.
    // lane-rectsplitx r2 (same class as the 1:4 fix): mi-exact AND
    // availability-correct -- four 16x8 strips share one coarse 16x16
    // `left_mode` cell too, so the fallback must not stand in for an absent
    // tile-edge neighbour.
    let (above_mode, left_mode) =
        neighbours.modes_above_left_mi(leaf_mi.0, leaf_mi.1, (above_mode, left_mode));
    let smooth_neighbor = is_smooth_mode(above_mode) || is_smooth_mode(left_mode);
    if smooth_neighbor {
        hit!(SMOOTH_LUMA_HITS);
    }
    // The palette-Y mode CDF row is picked by the neighbours' palette use
    // (`palette_ctx_and_cache`), so this sub-16x16 leaf must pass its own real
    // ctx/cache -- reading `palette_y_mode` off row 0 when a neighbour did use
    // a palette narrows the wrong interval and desyncs the tile even when the
    // decoded bit is the same. The cell is the 16px-SUB cell containing the
    // leaf (the palette neighbour arrays have no finer granularity).
    // lane-kf900 r6: the leaf's REAL mi position -- the second leaf of a 16x8
    // pair sits 8 px into the 16px cell, and both the cache's superblock-top-row
    // exclusion and the neighbour palette lookup itself are mi-granular
    // (see [`Neighbours::palette_ctx_and_cache_mi`]).
    let pal_at = leaf_mi;
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
        neighbours.skip_txfm_ctx(leaf_mi.0, leaf_mi.1),
        allow_screen_content_tools,
        Some((palette_ctx, &palette_cache)),
        &palette_uv_cache,
        leaf_mi.0,
        leaf_mi.1,
        fctx,
    )?;

    if let Some((dv_row, dv_col)) = intrabc {
        decode_intrabc_rect(
            dec,
            cdfs,
            neighbours,
            leaf_mi,
            (bw, bh),
            skip,
            (dv_row, dv_col),
            y,
            u,
            v,
            base_q_idx,
            fctx,
        )?;
        return Ok(DC_PRED);
    }
    // lane-kf900 r4: palette prediction on this shape, the same colour-index
    // map -> base colours buffer [`decode_block_rect`] hands each
    // `reconstruct_rect` through [`set_palette_pred`].
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
    if crate::envflags::env_flag!("EC_AV1_RECTX_TRACE") {
        eprintln!("  skip={skip} mode={mode} angle_delta_y={angle_delta_y} uv_mode={uv_mode}");
    }
    if filter_intra.is_some() {
        // lane-fistrip r1: `predict_filter_intra` already walks its 4x2
        // patches over a true `bw`x`bh` block (lane-rectsplit r1), so an
        // 8x16/16x8 strip predicts through the same call `reconstruct_rect`
        // below already makes; this refusal was square-only prose.
        hit!(FILTER_INTRA_RECT_HITS);
        hit!(FILTER_INTRA_RECT_SUB16_HITS);
    }
    let uv_predict_mode = if uv_mode == UV_CFL_PRED {
        DC_PRED
    } else {
        uv_mode
    };
    // lane-cfl r1, same defect as [`decode_leaf_8x8`]: chroma's intra-edge
    // filter type is the NEIGHBOUR's `uv_mode` (libaom `get_filt_type`), not
    // a constant `false`.
    let smooth_neighbor_uv = neighbours.smooth_uv_neighbour(leaf_mi.0, leaf_mi.1, r, c, fctx);
    let depth = if tx_select && !lossless(fctx) {
        // lane-t900 r2 (sibling sweep): the in-inter `TXFM_CONTEXT` band read
        // every other rect reader now does -- a no-op on a key frame.
        // lane-av1mixloss: libaom `read_tx_size`'s first line
        // (`decodeframe.c:1203`, `if (xd->lossless[segment_id]) return
        // TX_4X4;`) precedes the `TX_MODE_SELECT` test, so a lossless-segment
        // rect leaf codes NO depth symbol (`depth_to_tx_wh` clamps to 4x4).
        // The mixed frame's 16x8 leaf at mi(10,0) forked exactly here: ours
        // read `tx_size_cat1` (rng 55700->49918) where the oracle's next
        // read was the leaf's own txb_skip.
        let ctx = tx_size_context_txfm_rect(neighbours, leaf_mi, bw, bh, fctx);
        // lane-rectsplitx r1: `bsize_to_tx_size_cat` (libaom `blockd.h`,
        // `bsize_to_tx_size_depth_table`) is 2 for BLOCK_16X8/BLOCK_8X16, so
        // the category is 1 -- NOT the 2 the 32x16 strips read. Same symbol
        // count (3), different CDF row: a wrong-table read of a real symbol,
        // the class that cost lane-rectsplit r1 its 64x32 depth.
        // lane-rect1d r1: the CATEGORY of this leaf is 1, not 2. libaom's
        // `bsize_to_tx_size_cat` reads `bsize_to_tx_size_depth_table` and
        // subtracts one: BLOCK_16X8/8X16 (and the 1:4 BLOCK_16X4/4X16 this
        // function also decodes) sit at depth 2, so they read
        // `tx_size_cdf[1]` -- the same table a 16x16 square block reads --
        // while `tx_size_cat2` belongs to the 32-level shapes
        // ([`decode_block_rect`]). Both tables carry three symbols, so the
        // wrong one decodes a plausible depth from the wrong probabilities:
        // the range diverges silently at the first such leaf and every later
        // symbol in the tile is read from the wrong place (class
        // `wrong-alphabet-same-value`, here same alphabet / wrong row).
        let d = dec.symbol(&mut cdfs.tx_size_cat1[ctx]);
        if crate::envflags::env_flag!("EC_TRACE_MODE_STEP") {
            eprintln!(
                "EC_ISTEP mi_row={} mi_col={} name=tx_depth val={d} ctx={ctx} cat=1 rng={}",
                leaf_mi.0,
                leaf_mi.1,
                dec.debug_state().0
            );
        }
        d
    } else {
        0
    };
    // lane-av1-intrabc r3: this leaf's own `set_txfm_ctxs` when the frame reads
    // the `TXFM_CONTEXT` bands -- see [`publish_txfm_bands_if_in_inter`].
    publish_txfm_bands_if_in_inter(
        neighbours,
        leaf_mi,
        depth_to_tx_wh(bw, bh, depth, fctx),
        (bw, bh),
        fctx,
    );
    if depth != 0 || lossless(fctx) {
        // The same per-transform-unit walk the bigger strips take
        // (`depth_to_tx_wh`: TX_16X8 -> TX_8X8 at depth 1, -> TX_4X4 at
        // depth 2), addressed at this leaf's own mi position.
        hit!(TX_DEPTH_HITS);
        hit!(SUB16_SPLIT_HITS);
        let (tx_w, tx_h) = depth_to_tx_wh(bw, bh, depth, fctx);
        let modes = RectStripModes {
            skip,
            mode,
            angle_delta_y,
            uv_predict_mode,
            angle_delta_uv,
            alpha,
            filter_intra,
            smooth_neighbor,
            // lane-rectsplitx r3: the split arm hard-coded `false` here while
            // the unsplit arm below passes the real neighbour answer -- chroma
            // of a tx-split 16x8/8x16 leaf lost its intra-edge filter type.
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
            leaf_mi,
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
        return Ok(mode);
    }
    let (px, py) = (leaf_mi.1 * MI, leaf_mi.0 * MI);
    let reach = Reach::of_rect(bw, bh, px, py, y.width, y.height, fctx);
    let (cpx, cpy) = (px >> ss_x(fctx), py >> ss_y(fctx));
    let (chroma_w, chroma_h) = (bw >> ss_x(fctx), bh >> ss_y(fctx));
    let (luma_levels, u_levels, v_levels);
    if skip {
        luma_levels = Grid::Zero(bw * bh);
        u_levels = Grid::Zero(chroma_w * chroma_h);
        v_levels = Grid::Zero(chroma_w * chroma_h);
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
    } else {
        // lane-rectx: a genuine 16x8/8x16 luma transform (`TxbSet::LumaRect16x8`,
        // real `EOB_PT_128_LUMA` alphabet + the same mode-indexed `tx_type`
        // symbol the square `Luma16` set reads -- `get_ext_tx_set_type`'s
        // `use_reduced_set` branch returns `EXT_TX_SET_DTT4_IDTX` at
        // `tx_size_sqr_up == TX_16X16` regardless of the true, non-square
        // `tx_size`) and its chroma half (8x4/4x8, `TxbSet::ChromaRect8x4`).
        // lane-av1-422e: a 4:2:2 strip halves only the width, so the chroma
        // half of a 16x8/8x16 luma leaf is an 8x8 SQUARE (`TxbSet::Chroma8` +
        // the default scan -- the pairing any 8x8 chroma plane reads). 4:2:0
        // halves both axes (8x4/4x8 rect, `TxbSet::ChromaRect8x4`); 4:4:4
        // leaves the full 16x8/8x16 rect (`TxbSet::ChromaRect16x8`).
        let chroma_422 = ss_x(fctx) == 1 && ss_y(fctx) == 0;
        let (luma_scan, chroma_scan): (&[u16], &[u16]) = if ss_x(fctx) == 0 {
            if bw == 16 {
                (&SCAN_16X8, &SCAN_16X8)
            } else {
                (&SCAN_8X16, &SCAN_8X16)
            }
        } else if chroma_422 {
            if bw == 16 {
                (&SCAN_16X8, default_scan(8))
            } else {
                (&SCAN_8X16, default_scan(8))
            }
        } else if bw == 16 {
            (&SCAN_16X8, &SCAN_8X4)
        } else {
            (&SCAN_8X16, &SCAN_4X8)
        };
        let chroma_rect_set = if ss_x(fctx) == 0 {
            TxbSet::ChromaRect16x8
        } else if chroma_422 {
            TxbSet::Chroma8
        } else {
            TxbSet::ChromaRect8x4
        };
        // lane-av1422warp: at ss (1,0) this leaf's chroma is a SQUARE
        // (side/2, side) block, so `side` chroma 4-px cells above it spread
        // over `side` LUMA mi columns -- one chroma column per PAIR of luma
        // mi cells, and both carry the covering unit's whole-unit dc sign.
        // The per-mi rect gather therefore double-counts every chroma
        // column's sign. `around_mi_422_chroma`'s every-second above-cell
        // sampling is libaom's `txb_w_unit` sum, and every other 4:2:2 chroma
        // gather in this file already routes through it (see the gate at this
        // arm's siblings); this one did not, so a 4:2:2 rect leaf voted over
        // luma unit counts. Measured on the pinned 4:2:2 rotzoom stream
        // (a1.obu frame 0): the 16x8 leaf at mi(48,28) resolved its V plane's
        // dc sign from vote 0 (`dc_sign_cdf[0]`) where the oracle resolves a
        // negative vote (`dc_sign_cdf[1]`), which is the first bit-position
        // divergence in the frame (the bit offset is a clean constant through
        // coefficient unit 1418 and turns 6 bits at 1419). Luma keeps the
        // per-mi gather; 4:2:0 and 4:4:4 keep the plain rect walk verbatim.
        let mut around = neighbours.around_mi_rect(leaf_mi, bw, bh);
        if chroma_422 {
            let c = neighbours.around_mi_422_chroma(leaf_mi, bw, bh);
            around[1] = c[1];
            around[2] = c[2];
        }
        let luma_set = if reduced_tx_set {
            TxbSet::LumaRect16x8
        } else {
            TxbSet::LumaRect16x8Set1
        };
        let mut luma_coding = cdfs.txb(luma_set, fi_tx_row(mode, filter_intra));
        let (l_levels, luma_tx_type) = read_coeffs_rect(
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
        if crate::envflags::env_flag!("EC_AV1_RECTX_TRACE") {
            let nz: Vec<(usize, i32)> = l_levels
                .iter()
                .copied()
                .enumerate()
                .filter(|(_, v)| *v != 0)
                .collect();
            eprintln!("  luma_tx_type={luma_tx_type:?} nz_levels={nz:?}");
        }
        luma_levels = l_levels;
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
        let u_default_tx = default_intra_tx_type(uv_predict_mode as u8);
        let u_skip_ctx = usize::from(around[1].0) + usize::from(around[1].1);
        let mut u_coding = cdfs.txb(chroma_rect_set, uv_predict_mode);
        let (u_l, u_tx_type) = read_chroma_coeffs_rect(
            dec,
            &mut u_coding,
            chroma_scan,
            chroma_w,
            chroma_h,
            u_skip_ctx,
            dc_sign_ctx(around[1].2),
            u_default_tx,
            fctx,
            1,
        )?;
        u_levels = u_l;
        let u_residual = dequant_and_inverse_typed_wh(
            &u_levels,
            chroma_w,
            chroma_h,
            crate::decode::bit_depth(fctx),
            block_q_idx(fctx),
            plane_q_delta(1, fctx).0,
            plane_q_delta(1, fctx).1,
            u_tx_type,
            block_iqmatrix(fctx, 1, chroma_w, chroma_h, u_tx_type),
        );
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
            &u_residual,
            alpha.zip(ac).map(|((au, _), ac)| (au, ac)),
            None,
            smooth_neighbor_uv,
            fctx,
        );
        let v_default_tx = default_intra_tx_type(uv_predict_mode as u8);
        let v_skip_ctx = usize::from(around[2].0) + usize::from(around[2].1);
        let mut v_coding = cdfs.txb(chroma_rect_set, uv_predict_mode);
        let (v_l, v_tx_type) = read_chroma_coeffs_rect(
            dec,
            &mut v_coding,
            chroma_scan,
            chroma_w,
            chroma_h,
            v_skip_ctx,
            dc_sign_ctx(around[2].2),
            v_default_tx,
            fctx,
            2,
        )?;
        v_levels = v_l;
        let v_residual = dequant_and_inverse_typed_wh(
            &v_levels,
            chroma_w,
            chroma_h,
            crate::decode::bit_depth(fctx),
            block_q_idx(fctx),
            plane_q_delta(2, fctx).0,
            plane_q_delta(2, fctx).1,
            v_tx_type,
            block_iqmatrix(fctx, 2, chroma_w, chroma_h, v_tx_type),
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
            &v_residual,
            alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
            None,
            smooth_neighbor_uv,
            fctx,
        );
        hit!(RECT_LEAF_COEFF_HITS);
    }
    neighbours.record_mi_rect(
        leaf_mi,
        bw,
        bh,
        &[&luma_levels[..], &u_levels[..], &v_levels[..]],
    );
    neighbours.fill_skip_grid_rect(leaf_mi, mi_w, mi_h, skip);
    neighbours.fill_lf_grid_rect(leaf_mi, mi_w, mi_h, bw as u8, bh as u8, 0, fctx);
    // The leaf's own palette state (`0` clears, as this site always did
    // while a palette leaf refused -- lane-palette2 r8), written on the SPLIT
    // AXIS only until the pair's closing leaf: the coarse cell holds one
    // entry for both leaves, and leaf 0 zeroing the LEFT one threw away the
    // real left neighbour leaf 1 reads (lane-kf900 r4).
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
    // A rect leaf spans mi_w x mi_h, so its mode reaches several columns and
    // rows -- record the whole span, not one cell.
    neighbours.record_mode_mi(leaf_mi.0, leaf_mi.1, mi_w, mi_h, mode);
    neighbours.record_uv_mode_mi(leaf_mi.0, leaf_mi.1, mi_w, mi_h, uv_predict_mode);
    Ok(mode)
}

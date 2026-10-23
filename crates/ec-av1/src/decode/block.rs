//! Block decoder: intra mode and angle readers, CFL, the plane reader and decode_block. //! av1split: moved out of `decode/mod.rs`; the items are unchanged apart from visibility. (D17)

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn read_intra_mode(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    above_mode: usize,
    left_mode: usize,
    cfl: bool,
    side: usize,
    enable_filter_intra: bool,
    skip_ctx: usize,
    allow_screen_content_tools: bool,
    allow_intrabc: bool,
    // `Some((palette_mode_ctx, color_cache))` at the call sites this lane's
    // scope actually reconstructs a palette-Y block for (`av1_get_palette_mode_ctx`
    // and `av1_get_palette_cache`'s neighbour lookup, [`Neighbours::palette_ctx_and_cache`]);
    // `None` at the excluded call sites (the 8x8-leaf path), which still read
    // the `palette_y_mode` symbol at ctx 0 -- matching the bits a real
    // decoder reads either way -- but refuse by name the moment it fires,
    // exactly the old behaviour.
    palette: Option<(usize, &[u16])>,
    // The U-channel colour cache for a chroma palette read ([`Neighbours::palette_uv_cache`]),
    // `&[]` at the excluded call sites -- same corner-cut as `palette` above,
    // no `mode_ctx` needed (`palette_uv_mode_ctx` is this block's own just-read
    // `use_palette_y`, not a neighbour lookup).
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
)> {
    let trace = crate::envflags::env_flag!("EC_AV1_TRACE");
    // lane-tiny r2: EC_ISTEP-format trace (same "name=... val=... rng=..."
    // shape the oracle's own `ec_read_intra_frame_mode_info_impl` prints
    // under `EC_TRACE_MODE_STEP`) so a range ladder can be diffed line for
    // line against the instrumented aomdec, not just `tell()`.
    crate::msac::SymbolDecoder::set_symr_mi(mi_r as i64, mi_c as i64);
    let ec_istep = crate::envflags::env_flag!("EC_TRACE_MODE_STEP");
    if ec_istep {
        // Mirrors the oracle's own `EC_IMODE mi_row=.. mi_col=.. rng=..` line
        // (decodemv.c:818), printed at the same point: before the first
        // mode-info symbol of the block, so a cross-decoder range ladder can
        // be keyed on the BLOCK, not just on the symbol sequence.
        eprintln!(
            "EC_IMODE mi_row={mi_r} mi_col={mi_c} fn=sq side={side} rng={}",
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
    // lane-sbrect10 r1: `is_cfl_allowed`'s <=32 bound, enforced at the single
    // point every square caller routes through (see `read_intra_mode_rect`).
    let cfl = if cfl && side > 32 {
        hit!(NOCFL_UV_MODE_HITS);
        false
    } else {
        cfl
    };
    // spec 5.11.6 `intra_frame_mode_info`: `intra_segment_id` is read BEFORE
    // `skip` when `SegIdPreSkip`, and after it otherwise.
    let (seg_w_mi, seg_h_mi) = (side / 4, side / 4);
    if seg_id_pre_skip(fctx) {
        intra_segment_id(dec, cdfs, mi_r, mi_c, seg_w_mi, seg_h_mi, false, fctx);
    }
    let skip = dec.symbol(&mut cdfs.skip[skip_ctx]) != 0;
    if trace {
        eprintln!(
            "TRACE skip value={} rng={}",
            skip as i32,
            dec.debug_state().0
        );
    }
    istep!("skip", skip as i32);
    if !seg_id_pre_skip(fctx) {
        intra_segment_id(dec, cdfs, mi_r, mi_c, seg_w_mi, seg_h_mi, skip, fctx);
    }
    // spec order (see the comment below on `read_intrabc_info`): `skip`,
    // `segment_id`, `cdef`, `delta_q` -- `cdef` lands right here.
    maybe_read_cdef_idx(dec, mi_r, mi_c, (side / MI, side / MI), skip, fctx);
    istep!("cdef", 0);
    // lane-sb128b r1: "this leaf IS the superblock" is `side == sb_size`, not
    // a hardcoded 64 -- a `PARTITION_NONE` 128x128 root is the whole
    // superblock and skips `delta_q` exactly like a 64 one under a 64 root.
    let whole_sb = side == sb_mi_cur(fctx) as usize * 4;
    maybe_read_delta_q(dec, cdfs, mi_r, mi_c, whole_sb, skip, fctx);
    maybe_read_delta_lf(dec, cdfs, mi_r, mi_c, whole_sb, skip, fctx);
    istep!("dq", 0);
    // `read_intrabc_info` (spec 5.11.13, libaom decodemv.c:693, called at
    // :811 right after `skip_txfm`/`segment_id`/`cdef`/`delta_q` and before
    // `mbmi->mode` is ever read): a `use_intrabc` symbol only present on an
    // intra frame whose header set `allow_intrabc`. Actual intra block copy
    // needs `assign_dv`'s own motion-vector-prediction machinery (`av1_find_mv_refs`,
    // `av1_find_ref_dv`) this decoder does not carry -- lane-screen scope
    // draws the line at consuming this one flag symbol and refusing by name
    // when it fires, so the arithmetic decoder still resyncs for a stream
    // that merely allows (but never uses) intrabc.
    if allow_intrabc {
        let use_intrabc = dec.symbol(&mut cdfs.intrabc) != 0;
        if crate::envflags::env_flag!("EC_TRACE_MODE_STEP") {
            eprintln!(
                "EC_ISTEP mi_row={mi_r} mi_col={mi_c} name=intrabc val={} rng={}",
                u8::from(use_intrabc),
                dec.debug_state().0
            );
        }
        if trace {
            eprintln!("TRACE use_intrabc value={}", use_intrabc as i32);
        }
        if use_intrabc {
            let dv = read_intrabc_dv(dec, cdfs, mi_r, mi_c, (side / MI, side / MI), fctx);
            if trace {
                eprintln!("TRACE intrabc dv={dv:?}");
            }
            fctx.intrabc_dv.with(|c| c.set(Some(dv)));
            hit!(INTRABC_HITS);
            // spec 5.11.13 / libaom `read_intra_frame_mode_info` returns the
            // moment `is_intrabc_block`: no y/uv mode, angle delta, palette,
            // CFL or filter-intra syntax follows. `YMode`/`UVMode` are forced
            // `DC_PRED` (which is also what the neighbour mode contexts and
            // the loop filter then see).
            return Ok((skip, DC_PRED, 0, DC_PRED, 0, None, None, None, None));
        }
    }
    let above_ctx = INTRA_MODE_CTX[above_mode];
    let left_ctx = INTRA_MODE_CTX[left_mode];
    let mode = dec.symbol(&mut cdfs.kf_y_mode[above_ctx][left_ctx]);
    if trace {
        eprintln!(
            "TRACE y_mode ctx=({above_ctx},{left_ctx}) value={mode} rng={}",
            dec.debug_state().0
        );
    }
    istep!("mode", mode as i32);
    let angle_delta_y = if (V_PRED..=D67_PRED).contains(&mode) {
        let delta = read_angle_delta(dec, &mut cdfs.angle_delta[mode - V_PRED]);
        if trace {
            eprintln!(
                "TRACE angle_delta value={}",
                delta + ANGLE_DELTA_ZERO as i32
            );
        }
        delta
    } else {
        0
    };
    istep!("angle_y", angle_delta_y);
    // lane-av1mono: a monochrome block codes no chroma mode/CFL/palette
    // symbol at all (libaom `decodemv.c:933`'s `!monochrome && is_chroma_ref`
    // guard). Fold `NumPlanes == 1` into the same `has_chroma` this reader
    // gates its chroma symbols on.
    let has_chroma = !mono(fctx);
    let cfl = cfl && has_chroma && cfl_allowed_px(side, side, fctx);
    let uv_mode = if !has_chroma {
        DC_PRED
    } else if cfl {
        crate::msac::SymbolDecoder::set_symr_cdf("uv_mode");
        dec.symbol(&mut cdfs.uv_mode_cfl[mode])
    } else {
        crate::msac::SymbolDecoder::set_symr_cdf("uv_mode");
        dec.symbol(&mut cdfs.uv_mode_no_cfl[mode])
    };
    if trace {
        eprintln!(
            "TRACE uv_mode cfl={cfl} y_mode={mode} value={uv_mode} rng={}",
            dec.debug_state().0
        );
    }
    istep!("uv_mode", uv_mode as i32);
    let alpha = if cfl && uv_mode == UV_CFL_PRED {
        Some(read_cfl_alphas(dec, cdfs))
    } else {
        None
    };
    // `SMOOTH_PRED..PAETH_PRED` (9..=12) chroma is a separate round-2 gap
    // from this lane's directional chroma: a corner block with neither
    // `above` nor `left` (both `Edges::build`'s no-neighbour fallback,
    // 127/129) fed `SMOOTH_PRED` produces the wrong pixel there (worst delta
    // 78 against ffmpeg, traced 2026-08-27) -- an existing, un-lane-touched
    // bug in that fallback this lane's own scope does not cover. Keep it
    // refused here rather than ship a silently-wrong decode.
    if (9..=12).contains(&uv_mode) {
        hit!(SMOOTH_UV_HITS);
    }
    // `get_uv_mode` (spec 9.3): `UV_CFL_PRED` predicts as `DC_PRED` for the
    // angle-delta question -- libaom's own `read_intra_frame_mode_info` reads
    // `angle_delta_uv` off `get_uv_mode(uv_mode)`, never off `uv_mode` raw,
    // so a CFL block (uv_mode==13, outside `V_PRED..=D67_PRED`) already takes
    // the `else` branch below and never reads one either way.
    let angle_delta_uv = if (V_PRED..=D67_PRED).contains(&uv_mode) {
        hit!(DIRECTIONAL_UV_HITS);
        read_angle_delta(dec, &mut cdfs.angle_delta[uv_mode - V_PRED])
    } else {
        0
    };
    istep!("angle_uv", angle_delta_uv);
    if angle_delta_uv != 0 {
        hit!(UV_ANGLE_DELTA_HITS);
    }
    // `read_palette_mode_info` (spec 5.11.13, libaom decodemv.c:567, called
    // at :840 right after `xd->cfl.store_y` and before `read_filter_intra_mode_info`):
    // gated by `av1_allow_palette` (blockd.h -- size bound only,
    // [`palette_bsize_ctx`]). `palette_mode_ctx` comes from the caller
    // (`palette` param, `None` refuses unconditionally at ctx 0 -- the old
    // behaviour, bit-identical since a hardcoded ctx-0 read and a real one
    // read the same CDF row whenever every neighbour really is non-palette).
    // `palette_uv_mode_ctx` is this block's own just-decided Y palette use,
    // per `read_palette_mode_info`'s own `pmi->palette_size[0] > 0` (not a
    // neighbour lookup at all), so it is exact even at the excluded call
    // sites once Y decodes real syntax there too.
    //
    // lane-palette r7: libaom's `decode_color_map_tokens` is NOT called
    // inline from `read_palette_mode_info` -- it runs later, from
    // `parse_decode_block` (`decodeframe.c:1135`, `av1_visit_palette`),
    // after the WHOLE mode-info read for the block, including UV palette
    // mode_info (`read_palette_mode_info`'s own second half, right below)
    // and `read_filter_intra_mode_info`. So this branch now only reads
    // mode/size/colours for both planes (deferring the two colour-index-map
    // decodes to after `filter_intra`, below) -- r6's range trace pinned
    // this exact ordering bug (`compare-range-not-tell`: ranges matched
    // bit-for-bit through the Y colours, diverged only at the old inline
    // `decode_color_index_map` call).
    let mut palette_y_pending: Option<(usize, [u16; 8])> = None;
    let mut palette_uv_pending: Option<(usize, [u16; 8], [u16; 8])> = None;
    if allow_screen_content_tools && let Some(bsize_ctx) = palette_bsize_ctx(side) {
        let (mode_ctx, cache) = palette.unwrap_or((0, &[]));
        if mode == DC_PRED && trace {
            let (rng, _) = dec.debug_state();
            eprintln!(
                "TRACE pre_palette_y_mode bsize_ctx={bsize_ctx} mode_ctx={mode_ctx} rng={rng}"
            );
        }
        let use_palette_y =
            mode == DC_PRED && dec.symbol(&mut cdfs.palette_y_mode[bsize_ctx][mode_ctx]) != 0;
        if mode == DC_PRED && trace {
            let (rng, _) = dec.debug_state();
            eprintln!(
                "TRACE palette_y_mode value={} rng={rng}",
                use_palette_y as i32
            );
        }
        if use_palette_y {
            let n = 2 + dec.symbol(&mut cdfs.palette_y_size[bsize_ctx]);
            if trace {
                let (rng, _) = dec.debug_state();
                eprintln!("TRACE palette_y_size n={n} rng={rng}");
            }
            let colors = read_palette_colors_y(dec, n, cache, fctx);
            if trace {
                let (rng, _) = dec.debug_state();
                eprintln!("TRACE palette_y_colors colors={:?} rng={rng}", &colors[..n]);
            }
            palette_y_pending = Some((n, colors));
        }
        // `read_palette_mode_info`'s UV half (decodemv.c:588): read
        // unconditionally at this bit position whenever `uv_mode ==
        // UV_DC_PRED`, regardless of whether Y just fired -- this decoder's
        // scope always reconstructs chroma alongside luma (`decode_block`
        // always calls `u.reconstruct`/`v.reconstruct`), so `is_chroma_ref`
        // is always true here and does not need threading in.
        //
        // lane-av1mono: libaom's actual guard is `num_planes > 1 &&
        // uv_mode == UV_DC_PRED && xd->is_chroma_ref` (decodemv.c:602), so a
        // monochrome frame reads NO `palette_uv_mode`/`palette_uv_size`
        // symbol even at this chroma-owning square block.
        let palette_uv_mode_ctx = usize::from(use_palette_y);
        if has_chroma
            && uv_mode == DC_PRED
            && dec.symbol(&mut cdfs.palette_uv_mode[palette_uv_mode_ctx]) != 0
        {
            let n = 2 + dec.symbol(&mut cdfs.palette_uv_size[bsize_ctx]);
            let (u_colors, v_colors) = read_palette_colors_uv(dec, n, palette_uv_cache, fctx);
            palette_uv_pending = Some((n, u_colors, v_colors));
        }
    }
    // `read_filter_intra_mode_info` (spec 5.11.14, libaom decodemv.c): a
    // `use_filter_intra` symbol this decoder never read at all until
    // lane-av1real r10 -- silently skipping it left every bit past a
    // DC_PRED block at <=32x32 read one symbol short of libaom for the rest
    // of the tile (the pinned CfL stream's actual bug: partition/skip/mode
    // matched bit-exact, then `eob_pt` read 5 instead of 6, because this
    // symbol's bits were never consumed).
    let mut filter_intra = None;
    // lane-tiles r12: `av1_filter_intra_allowed` (blockd.h) also requires
    // `mbmi->palette_mode_info.palette_size[0] == 0` -- libaom writes NO
    // `use_filter_intra` symbol for a Y-palette block, and reading one here
    // desynced every screen-content stream from the first palette block on
    // (it surfaced as a bogus "partition below 8x8" refusal further down the
    // tile).
    if mode == DC_PRED
        && enable_filter_intra
        && palette_y_pending.is_none()
        && let Some(class) = filter_intra_size_class(side)
    {
        let use_filter_intra = dec.symbol(&mut cdfs.filter_intra[class]) != 0;
        if trace {
            eprintln!(
                "TRACE use_filter_intra side={side} rng={} value={}",
                dec.debug_state().0,
                use_filter_intra as i32
            );
        }
        istep!("use_filter_intra", use_filter_intra as i32);
        if use_filter_intra {
            hit!(FILTER_INTRA_HITS);
            let fi_mode = dec.symbol(&mut cdfs.filter_intra_mode);
            if trace {
                eprintln!("TRACE filter_intra_mode value={fi_mode}");
            }
            istep!("filter_intra_mode", fi_mode as i32);
            filter_intra = Some(fi_mode);
        }
    }
    // `av1_visit_palette` (decoder.c:234): plane 0 (Y) then plane 1 (chroma,
    // shared U/V map) -- the same order libaom's own loop visits them in.
    let palette_y = palette_y_pending.map(|(n, colors)| {
        let map = decode_color_index_map(
            dec,
            cdfs,
            n,
            side,
            palette_onscreen(mi_r, mi_c, side, side, fctx),
            false,
        );
        hit!(PALETTE_HITS);
        if trace {
            eprintln!("TRACE palette_y size={n} colors={:?}", &colors[..n]);
        }
        PaletteY {
            size: n,
            colors,
            map,
        }
    });
    let palette_uv = palette_uv_pending.map(|(n, u_colors, v_colors)| {
        let on = palette_onscreen_uv(
            (side, side),
            palette_onscreen(mi_r, mi_c, side, side, fctx),
            (side >> ss_x(fctx), side >> ss_y(fctx)),
            (ss_x(fctx), ss_y(fctx)),
        );
        // lane-av1-422d: the chroma map's extent is the PLANE BLOCK
        // `(side >> ss_x) x (side >> ss_y)` -- 4x8 at ss (1,0), not the
        // square call's `(side >> ss_x)`-squared (4x4), which left a
        // 16-entry map for a 32-sample unit and panicked the reconstruct.
        let map = decode_color_index_map_wh(
            dec,
            cdfs,
            n,
            side >> ss_x(fctx),
            side >> ss_y(fctx),
            on,
            true,
        );
        hit!(PALETTE_UV_HITS);
        if trace {
            eprintln!(
                "TRACE palette_uv size={n} u_colors={:?} v_colors={:?}",
                &u_colors[..n],
                &v_colors[..n]
            );
        }
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
    ))
}

/// `UV_CFL_PRED` (spec 6.10.19's chroma intra mode enum): the fourteenth
/// entry of [`crate::cdf::UV_MODE_CFL`], one past the thirteen ordinary
/// intra modes.
pub(super) const UV_CFL_PRED: usize = 13;

/// `read_cfl_alphas` (libaom decodemv.c): the joint sign symbol (8-ary, spec
/// 5.11.45's `cfl_alpha_signs`), then each plane's alpha magnitude symbol
/// (16-ary, `cfl_alpha_u`/`cfl_alpha_v`) where its sign is nonzero -- context
/// indices `CFL_CONTEXT_U`/`CFL_CONTEXT_V` (libaom cfl.h) derived from the
/// joint sign the same way. Returns each plane's final signed `alpha_q3`
/// (`cfl_idx_to_alpha`, cfl.c): magnitude `idx + 1`, sign-zero collapsing to 0.
pub(super) fn read_cfl_alphas(dec: &mut SymbolDecoder, cdfs: &mut Cdfs) -> (i32, i32) {
    hit!(CFL_BLOCK_HITS);
    let joint_sign = dec.symbol(&mut cdfs.cfl_sign) as i32;
    let sign_u = ((joint_sign + 1) * 11) >> 5;
    let sign_v = (joint_sign + 1) - 3 * sign_u;
    let alpha_u = if sign_u == 0 {
        0
    } else {
        let ctx_u = (joint_sign + 1 - 3) as usize;
        let mag = dec.symbol(&mut cdfs.cfl_alpha[ctx_u]) as i32 + 1;
        if sign_u == 2 { mag } else { -mag }
    };
    let alpha_v = if sign_v == 0 {
        0
    } else {
        let ctx_v = (sign_v * 3 + sign_u - 3) as usize;
        let mag = dec.symbol(&mut cdfs.cfl_alpha[ctx_v]) as i32 + 1;
        if sign_v == 2 { mag } else { -mag }
    };
    (alpha_u, alpha_v)
}

/// `cfl_luma_subsampling_420_lbd_c` + `subtract_average_c` (libaom cfl.c):
/// the reconstructed luma samples under a `side`-square block, subsampled
/// 4:2:0 to Q3 (`(a+b+c+d) << 1`, each 2x2 group), then block-average
/// subtracted (`round_offset = num_pel/2`, right-shifted by `log2(num_pel)`)
/// to give the AC values [`cfl_scaled`] scales by alpha.
pub(super) fn cfl_ac_q3(y: &PlaneBuf<'_>, px: usize, py: usize, side: usize) -> Vec<i32> {
    cfl_ac_q3_at(px, py, side, side, |lx, ly| {
        i32::from(y.data[ly * y.width + lx])
    })
}

/// The one body [`cfl_ac_q3`] and [`cfl_ac_q3_rect`] share, reading its luma
/// samples through `sample(x, y)` instead of a [`PlaneBuf`] -- so the encoder
/// (lane-av1cfl) builds a CfL candidate's AC signal from its own `u8`
/// reconstruction through THIS function rather than a second transcription of
/// `cfl_luma_subsampling_420_lbd_c` + `subtract_average_c`.
/// CfL AC at the frame's subsampling. 4:2:0 keeps the 2x2 average; 4:4:4
/// copies each luma sample shifted to Q3 (`cfl_luma_subsampling_444_lbd`).
pub(super) fn cfl_ac_ss(
    y: &PlaneBuf<'_>,
    px: usize,
    py: usize,
    bw: usize,
    bh: usize,
    ss_x: usize,
    ss_y: usize,
) -> Vec<i32> {
    if ss_x == 0 && ss_y == 0 {
        hit!(CFL_AC_444_HITS);
        let mut ac = vec![0i32; bw * bh];
        let mut sum = 0i32;
        for row in 0..bh {
            for col in 0..bw {
                let q3 = i32::from(y.data[(py + row) * y.width + px + col]) << 3;
                ac[row * bw + col] = q3;
                sum += q3;
            }
        }
        let num_pel = (bw * bh) as i32;
        let avg = (sum + num_pel / 2) >> num_pel.trailing_zeros();
        ac.iter_mut().for_each(|v| *v -= avg);
        ac
    } else if ss_x == 1 && ss_y == 0 {
        // lane-av1-422: `cfl_luma_subsampling_422_lbd_c` -- each chroma sample
        // averages the HORIZONTAL luma pair above it, at full chroma height
        // (`(a + b) >> 1` in Q3 is `(a + b) << 2`). A rect luma strip's chroma
        // is square here, and the 4:2:0 rect/square helpers below would halve
        // the height it must keep.
        hit!(CFL_AC_422_HITS);
        let (cw, ch) = (bw >> 1, bh);
        let mut ac = vec![0i32; cw * ch];
        let mut sum = 0i32;
        for row in 0..ch {
            for col in 0..cw {
                let (lx, ly) = (px + col * 2, py + row);
                let q3 = (i32::from(y.data[ly * y.width + lx])
                    + i32::from(y.data[ly * y.width + lx + 1]))
                    << 2;
                ac[row * cw + col] = q3;
                sum += q3;
            }
        }
        let num_pel = (cw * ch) as i32;
        let avg = (sum + num_pel / 2) >> num_pel.trailing_zeros();
        ac.iter_mut().for_each(|v| *v -= avg);
        ac
    } else if bw == bh {
        hit!(CFL_AC_420_SQ_HITS);
        cfl_ac_q3(y, px, py, bw)
    } else {
        hit!(CFL_AC_420_RECT_HITS);
        cfl_ac_q3_rect(y, px, py, bw, bh)
    }
}

pub(crate) fn cfl_ac_q3_at(
    px: usize,
    py: usize,
    bw: usize,
    bh: usize,
    sample: impl Fn(usize, usize) -> i32,
) -> Vec<i32> {
    hit!(CFL_AC_Q3_AT_HITS);
    if crate::envflags::env_flag!("EC_HALVSWEEP") {
        eprintln!("EC_HALV cfl_ac_q3_at px={px} py={py} bw={bw} bh={bh}");
    }
    let (cw, ch) = (bw / 2, bh / 2);
    let mut ac = vec![0i32; cw * ch];
    let mut sum = 0i32;
    for cy in 0..ch {
        for cx in 0..cw {
            let (lx, ly) = (px + cx * 2, py + cy * 2);
            let q3 =
                (sample(lx, ly) + sample(lx + 1, ly) + sample(lx, ly + 1) + sample(lx + 1, ly + 1))
                    << 1;
            ac[cy * cw + cx] = q3;
            sum += q3;
        }
    }
    let num_pel = (cw * ch) as i32;
    let avg = (sum + num_pel / 2) >> num_pel.trailing_zeros();
    ac.iter_mut().for_each(|v| *v -= avg);
    ac
}

/// `get_scaled_luma_q0` (libaom cfl.h): `alpha_q3 * ac_q3`, rounded from Q6
/// back to whole samples with `ROUND_POWER_OF_TWO_SIGNED` (round-to-nearest,
/// ties away from zero on the shifted-out sign).
pub(crate) fn cfl_scaled(alpha_q3: i32, ac_q3: i32) -> i32 {
    let v = alpha_q3 * ac_q3;
    if v >= 0 {
        (v + 32) >> 6
    } else {
        -((-v + 32) >> 6)
    }
}

/// One plane's reconstruction buffer, `width * height` samples, and the
/// prediction/residual reads and writes into it — the decode-side twin of
/// [`crate::encode`]'s private `Plane`, whose `edges` this mirrors exactly
/// (own-edge and reach clamps both), passing what it gathers straight to
/// [`crate::intra::predict`] instead of hand-rolling `DC_PRED` alone.
/// lane-tiny r4: raw padded-plane dump for filter-stage bisection, written
/// only when `var` names a path. Same byte shape as `EC_AV1_PREFILT_DUMP`.
/// lane-hgkf r2: 16-bit cropped stage dump, the high-bitdepth twin of
/// [`dump_stage`] (whose `as u8` truncation hides |delta| <= 2 defects).
/// lane-unwritten-dep: this doc previously said the dump "matches aomdec's own
/// `ec_dump16` rung byte for byte (crop dims, u16 LE)", which was true only of
/// the 4:2:0 crop it then used. The crop is now PER-AXIS, which is what the
/// oracle's rung 1 actually does -- it writes `y_crop_width` x `y_crop_height`,
/// `uv_crop_width` x `uv_crop_height`, `uv_crop_width` x `uv_crop_height` (the
/// libaom `YV12_BUFFER_CONFIG` fields, each already per-axis). Byte shape per
/// subsampling, `fw x fh` luma then both chroma planes, u16 LE, so:
///
/// ```text
/// ss_x ss_y  format   luma        chroma (each)
///   0    0   4:4:4    fw x fh     fw   x fh      fw*fh*3 samples
///   1    1   4:2:0    fw x fh     fw/2 x fh/2    fw*fh*3/2 samples
///   1    0   4:2:2    fw x fh     fw/2 x fh      fw*fh*2 samples
///   0    1   4:4:0    fw x fh     fw   x fh/2    fw*fh*3/2 samples
/// ```
///
/// (`round_ss` is the rounding the decoder's own plane extents use; at ss = 1 it
/// is `div_ceil(2)`, so every 4:2:0 dump is byte-for-byte unchanged by this.)
/// lane-mc64 r1: the file name is keyed on [`dump_stage_idx`], a per-var
/// decode-order counter -- both dumps used to write `.f0` unconditionally, so
/// every frame overwrote the previous one and only the LAST frame survived,
/// which silently defeats any per-frame filter-stage bisection (ledger, class
/// stale-output-faked-measurement). A sibling accessor `stage_dump_idx()`, which
/// derived the same number from `PREFILT_PICTURE_IDX`, had no caller and was
/// deleted: `PREFILT_PICTURE_IDX` is bumped after this point, not before it, so
/// the counter that names these files is the per-var map.
// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
pub(super) fn dump_stage16(
    var: &str,
    y: &PlaneBuf<'_>,
    u: &PlaneBuf<'_>,
    v: &PlaneBuf<'_>,
    fw: usize,
    fh: usize,
    ss_x: usize,
    ss_y: usize,
) {
    // lane-sbrect10 r2 / lane-cdef r1 (one shared helper): index the dump by
    // decode-order picture like EC_AV1_PREFILT_DUMP already does -- a fixed
    // `.f0` name kept only the LAST frame, so a mid-sequence divergence could
    // not be read at all.
    //
    // lane-unwritten-dep: the chroma extent is PER-AXIS subsampling, not
    // `div_ceil(2)` on both. The hardcoded pair was a 4:2:0 assumption and it
    // was load-bearing: on a 64x64 4:2:2 frame (ss_x = 1, ss_y = 0) it wrote
    // 32x32 chroma per plane where the frame carries 32x64, so HALF of each
    // chroma plane appeared in NO stage dump at all. A stage bisect that reads
    // "identical pre-deblock, differs post-deblock" off these dumps is then
    // measuring the dump's blind spot, not the pipeline's: the samples it could
    // not see are exactly the region where the 4:2:2 witness's output diverges
    // (chroma rows 32..63 -- the old `fh.div_ceil(2)` crop wrote 32 of 64 rows,
    // so everything from row 32 up was invisible; 2048 samples). `round_ss` is
    // the same rounding the
    // decoder's own plane extents use, and it matches the oracle's
    // `uv_crop_width` / `uv_crop_height` per axis.
    //
    // ONE file per frame: `dump_stage_idx` advances on every call, so creating
    // the file inside the plane loop would write three files per frame and
    // shift every later frame's index.
    //
    // lane-av1dumploud: this used to be `let _ = f.write_all(&bytes)` per row, so
    // a full filesystem left a SHORT file that no reader could tell from a
    // complete one. `LoudDump` is given the exact expected length this function
    // knows -- `(fw*fh + 2*cw*ch) * 2`, u16 LE -- and checks it against the file
    // on disk; a shortfall aborts the decode instead of leaving a plausible
    // artefact behind.
    if let Ok(path) = crate::envflags::var(var) {
        let cw = round_ss(fw, ss_x);
        let ch = round_ss(fh, ss_y);
        let expected = (fw * fh + 2 * cw * ch) * 2;
        let mut d = crate::dumpio::LoudDump::create(
            var,
            format!("{path}.f{}", dump_stage_idx(var)),
            expected,
        );
        for (p, w, h) in [(y, fw, fh), (u, cw, ch), (v, cw, ch)] {
            for row in 0..h {
                let mut bytes = Vec::with_capacity(w * 2);
                for &s in &p.data[row * p.width..][..w] {
                    bytes.extend_from_slice(&s.to_le_bytes());
                }
                d.write(&bytes);
            }
        }
        d.finish();
    }
}

/// Decode-order index of the next [`dump_stage`]/[`dump_stage16`] file for a
/// given env var -- lane-cdef r1: both used to write `.f0` unconditionally, so
/// every frame overwrote the previous one and only the LAST frame survived,
/// which silently defeats any per-frame filter-stage bisection.
fn dump_stage_idx(var: &str) -> usize {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static IDX: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    let mut m = IDX
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    let e = m.entry(var.to_string()).or_insert(0);
    let i = *e;
    *e += 1;
    i
}

pub(super) fn dump_stage(var: &str, y: &PlaneBuf<'_>, u: &PlaneBuf<'_>, v: &PlaneBuf<'_>) {
    // lane-av1dumploud: loud on a short write (see [`crate::dumpio::LoudDump`]).
    // The expected length is the three planes' own data lengths -- the writer
    // iterates exactly `p.data`, so this is the same extent, not a re-derivation
    // that could disagree with it.
    if let Ok(path) = crate::envflags::var(var) {
        let narrow = |p: &PlaneBuf<'_>| -> Vec<u8> { p.data.iter().map(|&s| s as u8).collect() };
        crate::dumpio::write_planes(
            var,
            format!("{path}.f{}", dump_stage_idx(var)),
            &[&narrow(y), &narrow(u), &narrow(v)],
        );
    }
}

/// `EC_AV1_PREFILT_WIDE_DUMP=<prefix>` -> `<prefix>.f<N>`, the pre-loop-filter
/// reconstruction cropped to each plane's mi-ALIGNED true extent, byte for
/// byte the shape aomdec's own rung 7 writes (`y_width`/`y_height`,
/// `uv_width`/`uv_height`), so the two can be diffed directly.
///
/// lane-av1444edge: this used to be an inline block in the KEY-frame tile path
/// only, with a FUNCTION-LOCAL `static IDX2` as its counter. Both facts were
/// measurement traps, and the second one is why the first went unnoticed:
/// the inter-frame path (`decode_inter_frame_tile_with_cdfs`) had no wide
/// prefilter dump at all, so every 4:4:4 lossy divergence localized to an
/// inter frame had no pre-filter rung to compare against and got attributed to
/// reconstruction when the oracle could not see it. The `static` also restarted
/// per monomorphisation, so every frame after the first overwrote `.f0` -- a
/// per-frame bisection silently compared the LAST decoded frame. The counter
/// is now the shared per-var [`dump_stage_idx`] map, and the call also sits on
/// the inter path (skipped when the filter pipeline is active, which has
/// already deblocked by the time the tile tail runs -- same condition
/// `capture_filter_replay` guards itself with).
pub(super) fn dump_prefilter_wide(y: &PlaneBuf<'_>, u: &PlaneBuf<'_>, v: &PlaneBuf<'_>) {
    let Ok(path) = crate::envflags::var("EC_AV1_PREFILT_WIDE_DUMP") else {
        return;
    };
    // lane-av1dumploud: loud on a short write. Expected length is
    // `sum(true_width * true_height)` over the three planes -- the crop the
    // writer itself builds, so the check cannot disagree with the bytes.
    let idx = dump_stage_idx("EC_AV1_PREFILT_WIDE_DUMP");
    let crop = |plane: &PlaneBuf<'_>| -> Vec<u8> {
        let mut out = Vec::with_capacity(plane.true_width * plane.true_height);
        for row in 0..plane.true_height {
            out.extend(
                plane.data[row * plane.width..][..plane.true_width]
                    .iter()
                    .map(|&s| s as u8),
            );
        }
        out
    };
    let (cy, cu, cv) = (crop(y), crop(u), crop(v));
    crate::dumpio::write_planes(
        "EC_AV1_PREFILT_WIDE_DUMP",
        format!("{path}.f{idx}"),
        &[&cy, &cu, &cv],
    );
}

/// Unwritten samples [`census_unwritten`] has counted since the last
/// [`take_unwritten_samples`] -- process-global (not thread-local) because the
/// gate reads it after the decode, from a different thread than the one that
/// scanned the plane.
pub(super) static UNWRITTEN_SAMPLES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Turns [`PLANE_SENTINEL`] filling on for the CALLING thread, independently
/// of `EC_AV1_PLANE_SENTINEL`. A gate cannot set a process-constant env var
/// (see [`crate::envflags`]), and the census is the only honest way to ask
/// "did the tile walk write every sample it was handed" -- a wrong-sample diff
/// cannot tell an unwritten sample from a mis-predicted one.
pub fn set_plane_sentinel(on: bool) {
    PLANE_SENTINEL_ON.with(|c| c.set(on));
}

/// Reads and clears [`UNWRITTEN_SAMPLES`]: the number of samples the decode
/// just handed to the tile walk never wrote, summed over every frame and
/// plane. Zero is the only acceptable answer for a decoder whose output must
/// not depend on its allocator.
pub fn take_unwritten_samples() -> usize {
    UNWRITTEN_SAMPLES.swap(0, std::sync::atomic::Ordering::SeqCst)
}

/// Samples in the PADDED TAIL -- every sample of each plane outside its own
/// `true_width` x `true_height` extent -- that still holds [`plane_poison`] at
/// the pre-deblock point. [`census_unwritten`] scans that region too, because
/// "past the true extent nothing is ever read" was an ASSUMPTION the tail's
/// census was built to avoid checking; this counter is the measurement.
pub(super) static PAD_UNWRITTEN_SAMPLES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Reads and clears [`PAD_UNWRITTEN_SAMPLES`].
pub fn take_pad_unwritten_samples() -> usize {
    PAD_UNWRITTEN_SAMPLES.swap(0, std::sync::atomic::Ordering::SeqCst)
}

/// The largest value reconstruction can produce, `(1 << 12) - 1`: the highest
/// bit depth this decoder carries is 12. A sentinel at or below this could be
/// a LEGAL sample, which would make the whole census vacuous -- every
/// unwritten sample would look like an ordinary one.
pub(super) const MAX_LEGAL_SAMPLE: u16 = 4095;

/// The value [`fresh_plane`] fills with when the sentinel is on.
///
/// [`PLANE_SENTINEL`] unless `EC_AV1_PLANE_SENTINEL` names a different one in
/// hex. Two different poisons are what turns the sentinel from "did a hole
/// survive into the output" into "did the output depend on the buffer's
/// initial content": a hole whose value reaches the picture changes the hash
/// when the poison changes, and one that does not cannot have influenced
/// anything. A value at or below [`MAX_LEGAL_SAMPLE`] is REFUSED (falls back
/// to [`PLANE_SENTINEL`]) because the census's soundness rests on the sentinel
/// being impossible. `EC_AV1_PLANE_SENTINEL=1` -- the spelling every existing
/// caller and gate uses, and `env_flag!`'s mere presence test -- keeps
/// [`PLANE_SENTINEL`].
/// `pub(crate)` for lane-av1graincensus: `film_grain::uninit_plane` fills its
/// destination with the SAME poison `fresh_plane` does, and
/// [`census_unwritten_grained`] counts the same value. The fill and the census
/// must agree on both the value and the predicate, or a "0" means nothing.
pub(crate) fn plane_poison() -> u16 {
    static V: std::sync::LazyLock<u16> = std::sync::LazyLock::new(|| {
        let Ok(raw) = crate::envflags::var("EC_AV1_PLANE_SENTINEL") else {
            return PLANE_SENTINEL;
        };
        let t = raw.trim();
        if t.is_empty() || t == "1" {
            return PLANE_SENTINEL;
        }
        let hex = t
            .strip_prefix("0x")
            .or_else(|| t.strip_prefix("0X"))
            .unwrap_or(t);
        match u16::from_str_radix(hex, 16) {
            Ok(v) if v > MAX_LEGAL_SAMPLE => v,
            _ => PLANE_SENTINEL,
        }
    });
    *V
}

thread_local! {
    /// The [`set_plane_sentinel`] override; `EC_AV1_PLANE_SENTINEL` is the
    /// process-wide default.
    static PLANE_SENTINEL_ON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether freshly allocated planes are [`PLANE_SENTINEL`]-filled.
/// `pub(crate)` for the same reason as [`plane_poison`].
pub(crate) fn plane_sentinel_on() -> bool {
    crate::envflags::env_flag!("EC_AV1_PLANE_SENTINEL")
        || PLANE_SENTINEL_ON.with(std::cell::Cell::get)
}

/// lane-av1unwritten: whether a subsize has a chroma PLANE BLOCK at this
/// frame's subsampling -- libaom `av1_ss_size_lookup[bsize][ss_x][ss_y] !=
/// BLOCK_INVALID` (`common_data.c`), which `decode_partition` checks on every
/// subsize before descending into it:
///
/// ```c
///   if (get_plane_block_size(subsize, pd_u->subsampling_x, pd_u->subsampling_y) ==
///       BLOCK_INVALID)
///     aom_internal_error(xd->error_info, AOM_CODEC_CORRUPT_FRAME,
///                        "Block size %dx%d invalid with this subsampling mode", ...);
/// ```
/// (`decodeframe.c:1449-1458`).
///
/// So at 4:2:2 a 4x8, 8x16, 16x32, 32x64, 64x128, 4x16, 8x32 or 16x64
/// subsize is NOT DECODABLE: libaom rejects the frame outright and never
/// reaches a transform size for it. This decoder walked those subsizes anyway,
/// covering a chroma plane block whose shape did not match the luma one.
///
/// Two independent facts justify refusing rather than decoding, and NEITHER is
/// this lane's pixel measurement:
///
/// 1. libaom REFUSES the shape. `aomdec` on the pinned
///    `440_request_is_422.obu` (a 4:2:2 sequence header over a 4:2:0 key
///    frame's tile) prints `Failed to decode frame 1: Corrupt frame detected`
///    and `Block size 8x16 invalid with this subsampling mode`.
/// 2. libaom's own ENCODER never emits it: `partition_search.c:3383-3389`
///    gates `partition_rect_allowed` on this same table, so no conformant
///    4:2:2 stream can contain the shape and the refusal cannot reject legal
///    content.
///
/// What led here was the observation that this decoder's output for such a
/// witness DEPENDS ON UNWRITTEN PLANE CONTENT. Measured on lane/av1unwritten's
/// branch point `8d6998d7` by two independent builds (`EC_AV1_FINAL_DUMP=<dir>/x`,
/// the directory created FIRST -- the dump writes nothing if it does not exist):
/// five runs with uninitialised planes gave TWO different output hashes, five runs
/// with `EC_AV1_PLANE_SENTINEL=1` gave ONE hash, and the sentinel output differs
/// from the plain one. At 8-bit the sentinel survives in the dump as the single
/// byte `0xAD`, not the pair `DE AD` -- `PLANE_SENTINEL`'s u16 narrows in
/// [`crate::stream`]'s u8 write path, so counting a two-byte `0xDEAD` in an
/// 8-bit dump cannot find it.
///
/// That hash observation is HISTORICAL and its hash half did NOT reproduce: on
/// the same `8d6998d7`, five plain runs of `440_request_is_422` gave ONE hash,
/// `a761118c8dd5c8d0` (`lanes/unwritten-dep.report.md` §10, which also states in
/// §13 that this pre-deblock census still has no committed reader on `main`).
/// Ambient allocator content is machine-dependent, so a stable value on one host
/// and a varying one on another would be the same defect; the hash spread is
/// cited as measured-on-`8d6998d7`, not as a standing property.
///
/// What the census localised: the lane's own pre-deblock census
/// ([`census_unwritten`]) found 112 unwritten samples per chroma plane on the
/// witness, 224 total -- on that lane's tree, with a lane-only instrument. That
/// count is a lane-tree measurement and is cited as such.
///
/// Which witness behaviour is CURRENT: `main` REFUSES this witness. The pin
/// `440_request_is_422.obu` is exercised by
/// `stream::tests::a_422_header_over_a_420_tile_refuses_the_subsize_libaom_calls_corrupt`,
/// which asserts the decode ERRORS with libaom's own "invalid with this
/// subsampling mode" wording, so `440_request_is_422` is not sweepable on the
/// merge tree (`lanes/unwritten-dep.report.md` §10). The run-to-run hashes above
/// are what the tree did BEFORE that refusal and cannot be re-measured on
/// `main`. Facts (1) and (2) above -- libaom refusing the shape, and libaom's own
/// encoder never emitting it -- are what make the refusal correct; the
/// measurement history is why it was looked for.
///
/// `av1_ss_size_lookup`'s `BLOCK_INVALID` cells, transcribed verbatim:
/// `(common_data.c:17-41`, rows in `BLOCK_SIZES_ALL` order, indexed
/// `[subsampling_x][subsampling_y])`. Every entry below is an (luma width,
/// luma height) whose plane block does not EXIST at that subsampling pair, so
/// libaom refuses any subsize of that shape
/// (`av1/decoder/decodeframe.c:1449-1458`, `AOM_CODEC_CORRUPT_FRAME`,
/// "Block size %dx%d invalid with this subsampling mode").
///
/// There is no other `BLOCK_INVALID` cell in the table: at (0,0) 4:4:4 and at
/// (1,1) 4:2:0 every row is valid, and the sixteen shapes below are the only
/// ones that are invalid at (1,0) 4:2:2 or (0,1) 4:4:0. The table test
/// [`chroma_plane_block_codable_matches_the_libaom_table`] re-transcribes all
/// 22 rows and checks this list cell for cell -- a diff against the C source,
/// not a restatement of it.
///
/// ```text
/// BLOCK_4X8    {4X8, 4X4}      {INVALID, 4X4}      <- INVALID @ (1,0)
/// BLOCK_8X4    {8X4, INVALID}  {4X4, 4X4}          <- INVALID @ (0,1)
/// BLOCK_8X16   {8X16, 8X8}     {INVALID, 4X8}      <- INVALID @ (1,0)
/// BLOCK_16X8   {16X8, INVALID} {8X8, 8X4}          <- INVALID @ (0,1)
/// BLOCK_16X32  {16X32, 16X16}  {INVALID, 8X16}     <- INVALID @ (1,0)
/// BLOCK_32X16  {32X16, INVALID}{16X16, 16X8}       <- INVALID @ (0,1)
/// BLOCK_32X64  {32X64, 32X32}  {INVALID, 16X32}    <- INVALID @ (1,0)
/// BLOCK_64X32  {64X32, INVALID}{32X32, 32X16}      <- INVALID @ (0,1)
/// BLOCK_64X128 {64X128,64X64}  {INVALID, 32X64}    <- INVALID @ (1,0)
/// BLOCK_128X64 {128X64,INVALID}{64X64, 64X32}      <- INVALID @ (0,1)
/// BLOCK_4X16   {4X16, 4X8}     {INVALID, 4X8}      <- INVALID @ (1,0)
/// BLOCK_16X4   {16X4, 8X4}     {8X4, 8X4}                         <- valid @ (1,0)
/// BLOCK_8X32   {8X32, 8X16}    {INVALID, 4X16}     <- INVALID @ (1,0)
/// BLOCK_32X8   {32X8, 16X8}    {32X8, 16X4}                      <- valid @ (0,1)
/// BLOCK_16X64  {16X64,16X32}   {INVALID, 8X32}     <- INVALID @ (1,0)
/// BLOCK_64X16  {64X16,32X16}   {32X16, 32X8}                     <- valid @ (0,1)
/// ```
///
/// The two "valid" rows are here on purpose: an earlier version of this
/// predicate hardcoded BLOCK_16X4@(1,0) and BLOCK_32X8@(0,1) as invalid, which
/// REFUSED a legal 4:2:2 shape -- one aomenc itself emits, since its own
/// `partition_search.c:3383-3389` gates `partition_rect_allowed[HORZ]` with
/// exactly this table.
pub(super) const PLANE_BLOCK_INVALID_422: &[(usize, usize)] = &[
    (4, 8),
    (8, 16),
    (16, 32),
    (32, 64),
    (64, 128),
    (4, 16),
    (8, 32),
    (16, 64),
];

/// The same list at 4:4:0 `(0, 1)` -- a shape no `color_config` can request
/// (libaom asserts `subsampling_y == 0` when `subsampling_x == 0`), kept so
/// the table test can cover every cell rather than only the reachable ones.
pub(super) const PLANE_BLOCK_INVALID_440: &[(usize, usize)] = &[
    (8, 4),
    (16, 8),
    (32, 16),
    (64, 32),
    (128, 64),
    (16, 4),
    (32, 8),
    (64, 16),
];

/// Whether `av1_ss_size_lookup[(bw, bh)][sx][sy]` is not `BLOCK_INVALID` -- i.e.
/// whether libaom lets a subsize of this luma shape have a chroma plane block
/// at this subsampling at all. `pub(crate)` so a fixture gate in `stream.rs`
/// can assert WHICH shape its pin carries, rather than only that the decode
/// refused -- a refusal gate that cannot name the shape is one decode-site
/// change away from measuring the wrong thing.
pub(crate) fn chroma_plane_block_codable(bw: usize, bh: usize, sx: usize, sy: usize) -> bool {
    match (sx, sy) {
        (1, 0) => !PLANE_BLOCK_INVALID_422.contains(&(bw, bh)),
        (0, 1) => !PLANE_BLOCK_INVALID_440.contains(&(bw, bh)),
        _ => true,
    }
}

/// libaom `get_partition_subsize` (spec 6.10.3, `common_data.c`'s
/// `subsize_lookup`) reduced to its DIMENSIONS -- all the `av1_ss_size_lookup`
/// check needs, and, by construction, never the PARENT block's own shape
/// (that is the r2 mistake in a new place: refusing `16x4` at 4:2:2 by testing
/// the 16x8 parent). `PARTITION_HORZ_A`/`_B` and `PARTITION_VERT_A`/`_B` are
/// the four-way arms, whose subsizes are the strips they visit.
pub(super) fn partition_subsize_dims(bsize: (usize, usize), part: usize) -> (usize, usize) {
    let (w, h) = bsize;
    match part {
        PARTITION_SPLIT => (w / 2, h / 2),
        PARTITION_HORZ | PARTITION_HORZ_A | PARTITION_HORZ_B => (w, h / 2),
        PARTITION_VERT | PARTITION_VERT_A | PARTITION_VERT_B => (w / 2, h),
        PARTITION_HORZ_4 => (w, h / 4),
        PARTITION_VERT_4 => (w / 4, h),
        _ => (w, h),
    }
}

/// [`chroma_plane_block_codable`] applied to the subsize a partition SYMBOL
/// selected, exactly as `decode_partition` does at `decodeframe.c:1449-1458`
/// before descending into it. Returns libaom's named refusal for the caller to
/// propagate.
#[track_caller]
pub(super) fn refuse_invalid_subsize(
    bsize: (usize, usize),
    part: usize,
    fctx: &crate::decode::FrameCtx,
) -> Result<()> {
    // lane-av1unwritten r6: the coverage artefact. A guard that sits INSIDE a
    // `PARTITION_HORZ_A..=PARTITION_VERT_B` arm can only ever observe 4..=7, so
    // "the guard is present" is not evidence that an offending value is caught --
    // which is exactly how r5's three misplaced guards read as coverage. This
    // records, per CALL SITE and per partition value, how often the guard was
    // reached and how often it fired, so a site that never sees an offending
    // value is visible instead of assumed covered.
    if crate::envflags::env_flag!("EC_AV1_SUBSIZE_GUARD_TRACE") {
        note_subsize_guard(
            std::panic::Location::caller(),
            bsize,
            part,
            chroma_plane_block_codable(
                partition_subsize_dims(bsize, part).0,
                partition_subsize_dims(bsize, part).1,
                ss_x(fctx),
                ss_y(fctx),
            ),
        );
    }
    let (bw, bh) = partition_subsize_dims(bsize, part);
    if chroma_plane_block_codable(bw, bh, ss_x(fctx), ss_y(fctx)) {
        Ok(())
    } else {
        Err(refuse_invalid_plane_block(bw, bh, fctx))
    }
}

/// lane-av1unwritten r6: the OTHER half of the coverage artefact. The guard
/// table says what the guards see; this says what each DISPATCH ARM sees, so a
/// dispatch reached with an offending partition value and no guard above it is a
/// visible hole rather than an absence nobody noticed. `decode_probe` calls
/// [`dump_subsize_arm_scan`] at exit under `EC_AV1_SUBSIZE_ARM_TRACE`.
pub(super) static SUBSIZE_ARM: std::sync::LazyLock<
    std::sync::Mutex<std::collections::BTreeMap<(&'static str, usize), usize>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(Default::default()));

#[track_caller]
pub(crate) fn note_subsize_arm(part: usize, codable: bool) {
    if !crate::envflags::env_flag!("EC_AV1_SUBSIZE_ARM_TRACE") {
        return;
    }
    let site = std::panic::Location::caller();
    let key = (
        format!("{}:{}", site.file(), site.line()).leak() as &'static str,
        part * 2 + usize::from(codable),
    );
    if let Ok(mut m) = SUBSIZE_ARM.lock() {
        *m.entry(key).or_insert(0) += 1;
    }
}

/// Prints and clears the [`note_subsize_arm`] table.
pub fn dump_subsize_arm_scan() {
    let Ok(mut m) = SUBSIZE_ARM.lock() else {
        return;
    };
    for ((site, k), n) in m.iter() {
        eprintln!(
            "SUBSIZE_ARM site={site} part={} codable={} reached={n}",
            k / 2,
            if k % 2 == 1 { "true" } else { "false" },
        );
    }
    m.clear();
}

/// Per-site, per-value counters for [`refuse_invalid_subsize`], plus the dump
/// that makes the reach/guard split readable. `EC_AV1_SUBSIZE_GUARD_TRACE`
/// records; `dump_subsize_guard_scan` prints (and clears) the table.
pub(super) static SUBSIZE_GUARD: std::sync::LazyLock<
    std::sync::Mutex<std::collections::BTreeMap<(&'static str, usize, usize, bool), usize>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(Default::default()));

fn note_subsize_guard(
    site: &'static std::panic::Location<'static>,
    bsize: (usize, usize),
    part: usize,
    codable: bool,
) {
    let key = (
        format!("{}:{}", site.file(), site.line()).leak() as &'static str,
        bsize.0 * 256 + bsize.1,
        part,
        codable,
    );
    if let Ok(mut m) = SUBSIZE_GUARD.lock() {
        *m.entry(key).or_insert(0) += 1;
    }
}

/// Prints and clears the [`refuse_invalid_subsize`] table: one line per
/// (site, block, partition value) with the times reached and whether the guard
/// fired. `decode_probe` calls this at exit.
pub fn dump_subsize_guard_scan() {
    let Ok(mut m) = SUBSIZE_GUARD.lock() else {
        return;
    };
    for ((site, bsize, part, codable), n) in m.iter() {
        eprintln!(
            "SUBSIZE_GUARD site={site} bsize={}x{} part={part} codable={codable} \
             reached={n}{}",
            bsize / 256,
            bsize % 256,
            if *codable { "" } else { " FIRED" },
        );
    }
    m.clear();
}

/// [`chroma_plane_block_codable`] as libaom's refusal: a returned error naming
/// the shape and the subsampling mode, never an `assert!` -- a partition symbol
/// is stream data, so a hostile file can reach it.
pub(super) fn refuse_invalid_plane_block(
    _bw: usize,
    _bh: usize,
    _fctx: &crate::decode::FrameCtx,
) -> Error {
    unsupported(
        "a block size 4x8, 8x16 or 16x4 (or 8x4 at 4:4:0) has no chroma plane block at this \
         frame's subsampling mode (libaom: \"Block size %dx%d invalid with this subsampling \
         mode\", av1/decoder/decodeframe.c:1456, refusing by the same rule)",
    )
}

/// The value [`fresh_plane`] fills with under `EC_AV1_PLANE_SENTINEL`: no
/// legal sample can equal it (reconstruction clamps to `(1 << bit_depth) - 1`,
/// at most 4095), so a decode that hashes identically under the flag proves
/// the output does not depend on the plane buffer's initial content.
pub(super) const PLANE_SENTINEL: u16 = 0xDEAD;

/// lane-unwritten-dep: the census count at the LAST point of the pipeline, on the
/// exact bytes `EC_AV1_FINAL_DUMP` writes.
///
/// WHY A SECOND COUNT, and what it is actually for -- the justification this
/// comment originally carried was WRONG and is corrected here, because a reader
/// would otherwise re-derive the wrong conclusion. An earlier revision of this
/// lane claimed the pre-deblock plane was byte-identical plain-vs-sentinel and
/// that "deblock creates the dependence". That reading came from
/// [`dump_stage16`], which cropped chroma as `(fw/2, fh/2)` -- a 4:2:0
/// assumption. On a 64x64 4:2:2 frame (ss_x=1, ss_y=0) that wrote 32x32 chroma
/// where the frame carries 32x64, so 2048 chroma samples appeared in NO stage
/// dump, and they are exactly the region where the difference lives.
///
/// With per-axis extents the corrected measurement on `440_request_is_422` is:
/// the PRE-DEBLOCK dump already DIFFERS by 2158 bytes plain-vs-sentinel, and an
/// exact u16 whole-plane census finds 224 unwritten samples (U 112, V 112,
/// rows 36..63, whole 4x4 groups) -- the same number
/// `lanes/av1unwritten.report.md` reports. **The hole is present before any
/// filter runs, and the pre-deblock census sees it.** This comment previously
/// said the opposite; that was the instrument's blind spot read as a property of
/// the pipeline.
///
/// So what this second count is FOR, stated honestly: it is not there because the
/// pre-deblock census misses holes. It is there because it is the measurement
/// taken where the OUTPUT is taken -- the bytes stored into the reference slots
/// and handed to the caller. A hole can be created after reconstruction (a
/// filter stage, a superres step, a second pass) and be invisible to a scan taken
/// before it; this one cannot. It is a check on the whole span from the tile walk
/// to the caller's frame, not a claim that the pre-deblock scan is insufficient.
pub(super) static FINAL_UNWRITTEN_SAMPLES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// lane-unwritten-dep: reads and clears [`FINAL_UNWRITTEN_SAMPLES`] -- unwritten
/// samples still present in the frame as handed to the caller, summed over every
/// frame and plane. Process-global, like the pre-deblock census's counter, so a
/// gate can read it after the decode from a different thread than the one that
/// scanned.
pub fn take_final_unwritten_samples() -> usize {
    FINAL_UNWRITTEN_SAMPLES.swap(0, std::sync::atomic::Ordering::SeqCst)
}

/// lane-av1graincensus: samples still holding the poison after film grain, over
/// the picture [`crate::film_grain::apply_grain`] RETURNS -- i.e. the picture the
/// caller receives on a grained frame.
pub(super) static GRAINED_UNWRITTEN_SAMPLES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Reads and clears [`GRAINED_UNWRITTEN_SAMPLES`].
pub fn take_grained_unwritten_samples() -> usize {
    GRAINED_UNWRITTEN_SAMPLES.swap(0, std::sync::atomic::Ordering::SeqCst)
}

/// A frame plane's own backing store, `n` samples, zero-filled.
///
/// The `true_width`/`true_height`/`width`/`height` arguments are kept for the
/// call sites' shape and the sentinel's poison fill; the zero fill itself reads
/// none of them. The former `EC_AV1_PLANE_ALLOC=uninit|padzero|extzero` arms
/// (uninitialised allocation for memcheck census) are gone: they needed
/// `set_len` over uninitialised memory, which the crate no longer contains. The
/// shipped default was already the zeroed arm (`lanes/av1uballoc.report.md` §1);
/// the poison sentinel (`EC_AV1_PLANE_SENTINEL`) still catches any read of the
/// initial content.
pub(super) fn fresh_plane(
    n: usize,
    _true_width: usize,
    _true_height: usize,
    _width: usize,
    _height: usize,
) -> Vec<u16> {
    if plane_sentinel_on() {
        return vec![plane_poison(); n];
    }
    vec![0u16; n]
}

#[derive(Clone)]
pub(super) struct PlaneBuf<'a> {
    pub(super) data: std::borrow::Cow<'a, [u16]>,
    pub(super) width: usize,
    pub(super) height: usize,
    /// The frame's true, decodable extent in this plane's own units — past
    /// this, samples are the padded coding surface's invented tail, never a
    /// real decoder's edge or reach reads.
    pub(super) true_width: usize,
    pub(super) true_height: usize,
    /// This plane's own tile's top-left pixel origin (spec: intra prediction
    /// never reaches across a tile boundary even though an earlier-decoded
    /// tile's real pixels sit right there in the shared buffer) — set via
    /// [`Self::set_tile_origin`] before each tile's own superblock walk,
    /// `0` for the single-tile case. lane-tiles r2: only the left/top bound
    /// is threaded (the two-tile column-split stage 1 fixture's tile always
    /// ends at the frame's own right/bottom edge); a non-last tile's own
    /// right/bottom reach bound is still open, tracked in the report.
    pub(super) tile_x0: usize,
    pub(super) tile_y0: usize,
    /// lane-tiles r5: this plane's own tile's right/bottom pixel bound
    /// (exclusive) -- a non-last tile column/row's own reach must stop at
    /// its own SB-aligned tile boundary. `width`/`height` (this plane's
    /// full padded extent) for the single-tile/last-tile case, matching the
    /// old unclipped behaviour exactly.
    pub(super) tile_x1: usize,
    pub(super) tile_y1: usize,
    /// Which plane this buffer IS (0 luma, 1 U, 2 V) -- TEMPORARY probe.
    pub(super) plane: u8,
}

/// One entry per `MV_REFERENCE_FRAME` (index 0/1 unused -- `NONE`/
/// `INTRA_FRAME`/`LAST_FRAME` all resolve elsewhere), `Some` when this
/// frame's own `ref_frame_idx` names a DPB slot that has a picture in it
/// (lane-av1refs: the same "empty slot" case `GOLDEN_FRAME` already
/// refused by name, now generic across `LAST2`/`LAST3`/`GOLDEN`/`BWDREF`/
/// `ALTREF2`/`ALTREF`).
// lane-av1pixprobe: env-gated one-sample write probe -- `EC_AV1_PIXPROBE=x,y`
// prints one PIXWRITE line per reconstruction store landing on that sample,
// naming the store site, the value it replaces and the one it leaves. Re-anchored
// from `lane-av1-ibcwrite` @ 25f5c729 onto the main-shape sites (the two mc stores
// now live in `reconstruct_mc_rect`).
pub(super) static PIXPROBE_ON: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(|| std::env::var_os("EC_AV1_PIXPROBE").is_some());

pub(super) static PIXPROBE: std::sync::LazyLock<Option<(usize, usize)>> =
    std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_PIXPROBE").ok().and_then(|v| {
            let (x, y) = v.split_once(',')?;
            Some((
                x.trim().parse::<usize>().ok()?,
                y.trim().parse::<usize>().ok()?,
            ))
        })
    });

#[inline]
// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
pub(super) fn pix_write(
    site: &str,
    bx: usize,
    by: usize,
    bside: usize,
    x: usize,
    y: usize,
    prev: u16,
    val: u16,
) {
    if *PIXPROBE_ON && Some((x, y)) == *PIXPROBE {
        eprintln!(
            "PIXWRITE {site} block=({bx},{by})+{bside} px=({x},{y}) prev={prev} val={val} at {}",
            std::panic::Location::caller()
        );
    }
}

impl PlaneBuf<'_> {
    /// Sets this plane's own tile pixel origin ([`Self::tile_x0`]/
    /// [`Self::tile_y0`]) before that tile's own superblock walk. `x0`/`y0`
    /// are already in this plane's own units (luma pixels for `y`, chroma
    /// pixels — halved — for `u`/`v`).
    pub(super) fn set_tile_origin(&mut self, x0: usize, y0: usize, x1: usize, y1: usize) {
        self.tile_x0 = x0;
        self.tile_y0 = y0;
        self.tile_x1 = x1;
        self.tile_y1 = y1;
    }

    pub(super) fn edges(
        &self,
        x: usize,
        y: usize,
        side: usize,
        reach: Reach,
    ) -> (Option<Vec<u16>>, Option<Vec<u16>>, Option<u16>) {
        let own_across = x + side.min(self.true_width.saturating_sub(x));
        let across = if reach.above_right {
            own_across + side.min(self.true_width.saturating_sub(own_across))
        } else {
            own_across
        }
        .min(self.width)
        .min(self.tile_x1);
        let own_down = y + side.min(self.true_height.saturating_sub(y));
        let down = if reach.below_left {
            own_down + side.min(self.true_height.saturating_sub(own_down))
        } else {
            own_down
        }
        .min(self.height)
        .min(self.tile_y1);
        let above = (y > self.tile_y0 && across > x)
            .then(|| self.data[(y - 1) * self.width + x..][..across - x].to_vec());
        let left = (x > self.tile_x0 && down > y).then(|| {
            (y..down)
                .map(|row| self.data[row * self.width + x - 1])
                .collect::<Vec<_>>()
        });
        let corner =
            (x > self.tile_x0 && y > self.tile_y0).then(|| self.data[(y - 1) * self.width + x - 1]);
        (above, left, corner)
    }

    /// [`Self::edges`] for a true `bw`x`bh` block (lane-intradisp r1):
    /// the above/left reach each extend by their OWN axis (`bw` across,
    /// `bh` down), not one shared `side`.
    pub(super) fn edges_rect(
        &self,
        x: usize,
        y: usize,
        bw: usize,
        bh: usize,
        reach: Reach,
    ) -> (Option<Vec<u16>>, Option<Vec<u16>>, Option<u16>) {
        let own_across = x + bw.min(self.true_width.saturating_sub(x));
        let across = if reach.above_right {
            own_across + bw.min(self.true_width.saturating_sub(own_across))
        } else {
            own_across
        }
        .min(self.width)
        .min(self.tile_x1);
        let own_down = y + bh.min(self.true_height.saturating_sub(y));
        let down = if reach.below_left {
            own_down + bh.min(self.true_height.saturating_sub(own_down))
        } else {
            own_down
        }
        .min(self.height)
        .min(self.tile_y1);
        let above = (y > self.tile_y0 && across > x)
            .then(|| self.data[(y - 1) * self.width + x..][..across - x].to_vec());
        let left = (x > self.tile_x0 && down > y).then(|| {
            (y..down)
                .map(|row| self.data[row * self.width + x - 1])
                .collect::<Vec<_>>()
        });
        let corner =
            (x > self.tile_x0 && y > self.tile_y0).then(|| self.data[(y - 1) * self.width + x - 1]);
        if matches!((bw, bh), (16, 8) | (8, 16)) {
            hit!(RECT_STRIP_PRED_HITS);
            if across > own_across || down > own_down {
                hit!(RECT_STRIP_REACH_HITS);
            }
        }
        (above, left, corner)
    }

    /// [`Self::reconstruct`] for a true `bw`x`bh` block (lane-intradisp r1),
    /// via [`Self::edges_rect`] and [`crate::intra::predict`]'s own
    /// already-rect-capable `(bw, bh)` pair.
    #[allow(clippy::too_many_arguments)]
    #[track_caller]
    pub(super) fn reconstruct_rect(
        &mut self,
        x: usize,
        y: usize,
        bw: usize,
        bh: usize,
        mode: usize,
        angle_delta: i32,
        reach: Reach,
        residual: &[i32],
        cfl: Option<(i32, &[i32])>,
        filter_intra: Option<usize>,
        smooth_neighbor: bool,
        // lane-wave1 step (i): the palette/intrabc prediction override, taken
        // off `FrameCtx::palette_pred` by the PUSH site (parse order) rather
        // than by this replay.
        pred: Option<Vec<u16>>,
        fctx: &crate::decode::FrameCtx,
    ) {
        // As [`Self::reconstruct`]'s own [`PALETTE_PRED`] override
        // (lane-palette2 r4): a palette block's prediction is the
        // reconstructed colour-index map, needing no edge pixels at all.
        // A override that is not `side x side` is not this block's (the palette
        // map of another block); predict from the edges instead of indexing past it.
        let prediction = if let Some(buf) = pred.filter(|buf| buf.len() == bw * bh) {
            buf
        } else {
            let (above, left, corner) = self.edges_rect(x, y, bw, bh, reach);
            if crate::envflags::env_flag!("EC_DEBUG_EDGES") {
                eprintln!(
                    "EDGES x={x} y={y} bw={bw} bh={bh} mode={mode} above={:?} left_len={:?} left0={:?} corner={corner:?} tx0={} ty0={} truew={} trueh={}",
                    above.as_ref().map(|a| (a.len(), a[0])),
                    left.as_ref().map(|l| l.len()),
                    left.as_ref().map(|l| l[0]),
                    self.tile_x0,
                    self.tile_y0,
                    self.true_width,
                    self.true_height
                );
            }
            let mut prediction = vec![0u16; bw * bh];
            if let Some(fi_mode) = filter_intra {
                crate::intra::predict_filter_intra(
                    fi_mode,
                    above.as_deref(),
                    left.as_deref(),
                    corner,
                    bw,
                    bh,
                    &mut prediction,
                    fctx,
                );
            } else {
                let enable_edge_filter = fctx.enable_edge_filter.with(std::cell::Cell::get);
                predict(
                    mode as u8,
                    angle_delta,
                    above.as_deref(),
                    left.as_deref(),
                    corner,
                    bw,
                    bh,
                    enable_edge_filter,
                    smooth_neighbor,
                    &mut prediction,
                    fctx,
                );
            }
            prediction
        };
        // lane-hgkf r2: per-TU prediction trace, the twin of aomdec's
        // EC_PRED/EC_PREDND rungs (x/y are this plane's pixel origin).
        if crate::envflags::env_flag!("EC_PRED") {
            let sum: i64 = prediction.iter().map(|&v| i64::from(v)).sum();
            let row0: Vec<u16> = prediction[..bw.min(8)].to_vec();
            let col0: Vec<u16> = (0..bh.min(8)).map(|r| prediction[r * bw]).collect();
            eprintln!(
                "@{} OUR_PRED x={x} y={y} plane={} bw={bw} bh={bh} mode={mode} ad={angle_delta} ft={} sum={sum} row0={row0:?} col0={col0:?}",
                std::panic::Location::caller(),
                recon_plane(),
                i32::from(smooth_neighbor)
            );
        }
        let residual = dense_residual(residual, bw * bh);
        if crate::envflags::env_flag!("EC_DBG192") && y == 168 && (x == 224 || x == 232) {
            let res0: Vec<i32> = residual[..bw.min(8)].to_vec();
            let pred0: Vec<u16> = prediction[..bw.min(8)].to_vec();
            eprintln!(
                "DBG192 rect x={x} y={y} bw={bw} bh={bh} mode={mode} pred0={pred0:?} res0={res0:?}"
            );
        }
        for row in 0..bh {
            // Same bottom/right frame-edge clip as [`Self::reconstruct`]'s
            // (lane-sb128c r8) -- a 64x128 block at the right edge of a frame
            // whose padded width is not superblock aligned hangs past it.
            if y + row >= self.height {
                break;
            }
            for col in 0..bw {
                if x + col >= self.width {
                    break;
                }
                let idx = row * bw + col;
                let mut base = i32::from(prediction[idx]);
                if let Some((alpha_q3, ac_q3)) = cfl {
                    // The AC buffer is sized for the CfL source block; a transform
                    // block larger than it has no AC sample here, so it adds nothing.
                    let ac = ac_q3.get(idx).copied().unwrap_or(0);
                    base = (base + cfl_scaled(alpha_q3, ac)).clamp(0, sample_max(fctx));
                }
                let sample = (base + residual[idx]).clamp(0, sample_max(fctx)) as u16;
                pix_write(
                    "reconstruct_rect",
                    x,
                    y,
                    bw,
                    x + col,
                    y + row,
                    self.data[(y + row) * self.width + x + col],
                    sample,
                );
                self.data.to_mut()[(y + row) * self.width + x + col] = sample;
            }
        }
        // lane-d792: the RECONSTRUCTION rung beside `EC_PRED`'s prediction one
        // -- a block whose prediction matches the oracle's but whose written
        // samples do not is either a residual defect or a later overwrite, and
        // only this print separates the two.
        if crate::envflags::env_flag!("EC_PRED") {
            let row0: Vec<u16> = (0..bw.min(8))
                .map(|c| self.data[y * self.width + x + c])
                .collect();
            let col0: Vec<u16> = (0..bh.min(8))
                .map(|r| self.data[(y + r) * self.width + x])
                .collect();
            eprintln!("OUR_RECON x={x} y={y} bw={bw} bh={bh} row0={row0:?} col0={col0:?}");
        }
    }

    /// Predicts under `mode` then adds `residual` (side*side, raster),
    /// writing the clamped reconstruction back into the plane at `(x, y)`.
    /// `cfl`, when `Some((alpha_q3, ac_q3))` (a `UV_CFL_PRED` chroma block),
    /// nudges the prediction by [`cfl_scaled`]'s per-sample amount before the
    /// residual is added — `av1_cfl_predict_block`'s own clip-then-add-residual
    /// order (cfl.c).
    #[track_caller]
    // Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn reconstruct(
        &mut self,
        x: usize,
        y: usize,
        side: usize,
        mode: usize,
        angle_delta: i32,
        reach: Reach,
        residual: &[i32],
        cfl: Option<(i32, &[i32])>,
        filter_intra: Option<usize>,
        // `smooth_neighbor` (spec `get_intra_edge_filter_type` /
        // libaom `get_filt_type`): true when the block's above OR left
        // neighbour's mode is `SMOOTH_PRED..=SMOOTH_H_PRED` (never `PAETH`).
        // Callers pass their own plane's neighbour-mode check
        // ([`is_smooth_mode`]) — chroma's is still hardcoded `false` by its
        // callers until a smooth/paeth `uv_mode` can reach this call at all
        // (still refused in `read_intra_mode`/`read_intra_mode_rect`), which
        // keeps `false` exact for chroma without this fn needing to know why.
        smooth_neighbor: bool,
        // lane-wave1 step (i): the palette/intrabc prediction override, taken
        // off `FrameCtx::palette_pred` by the PUSH site (parse order) rather
        // than by this replay.
        pred: Option<Vec<u16>>,
        fctx: &crate::decode::FrameCtx,
    ) {
        // A palette block's own [`PALETTE_PRED`] override (lane-palette r2)
        // takes priority over `predict()`/`predict_filter_intra` entirely --
        // palette prediction needs no edge pixels at all.
        // A override that is not `side x side` is not this block's (the palette
        // map of another block); predict from the edges instead of indexing past it.
        let prediction = if let Some(buf) = pred.filter(|buf| buf.len() == side * side) {
            buf
        } else {
            let (above, left, corner) = self.edges(x, y, side, reach);
            if crate::envflags::env_flag!("EC_DEBUG_EDGES") {
                eprintln!(
                    "EDGES_SQ x={x} y={y} side={side} mode={mode} above={:?} left={:?} corner={corner:?}",
                    above.as_deref(),
                    left.as_deref(),
                );
            }
            let mut prediction = vec![0u16; side * side];
            if let Some(fi_mode) = filter_intra {
                crate::intra::predict_filter_intra(
                    fi_mode,
                    above.as_deref(),
                    left.as_deref(),
                    corner,
                    side,
                    side,
                    &mut prediction,
                    fctx,
                );
            } else {
                // `enable_intra_edge_filter` is the sequence header's own flag
                // (`ENABLE_EDGE_FILTER`, set once per `decode_key_frame_tile_with_cdfs`
                // call, never per-block) -- threading it through every
                // `read_plane`/`decode_block`/`decode_leaf8` call would touch two
                // dozen sites for a value that never changes mid-decode, so it
                // rides a thread-local instead (this decoder is single-threaded
                // per stream).
                let enable_edge_filter = fctx.enable_edge_filter.with(std::cell::Cell::get);
                predict(
                    mode as u8,
                    angle_delta,
                    above.as_deref(),
                    left.as_deref(),
                    corner,
                    side,
                    side,
                    enable_edge_filter,
                    smooth_neighbor,
                    &mut prediction,
                    fctx,
                );
            }
            prediction
        };
        // lane-hgkf r2: per-TU prediction trace, the twin of aomdec's
        // EC_PRED/EC_PREDND rungs (x/y are this plane's pixel origin).
        if crate::envflags::env_flag!("EC_PRED") {
            let sum: i64 = prediction.iter().map(|&v| i64::from(v)).sum();
            let row0: Vec<u16> = prediction[..side.min(8)].to_vec();
            let col0: Vec<u16> = (0..side.min(8)).map(|r| prediction[r * side]).collect();
            eprintln!(
                "@{} OUR_PRED x={x} y={y} plane={} side={side} side={side} mode={mode} ad={angle_delta} ft={} sum={sum} row0={row0:?} col0={col0:?}",
                std::panic::Location::caller(),
                recon_plane(),
                i32::from(smooth_neighbor)
            );
        }
        let residual = dense_residual(residual, side * side);
        if crate::envflags::env_flag!("EC_DBG192") && y == 168 && (x == 224 || x == 232) {
            let res0: Vec<i32> = residual[..side.min(8)].to_vec();
            let pred0: Vec<u16> = prediction[..side.min(8)].to_vec();
            eprintln!(
                "DBG192 sq x={x} y={y} side={side} mode={mode} cfl={} pred0={pred0:?} res0={res0:?}",
                cfl.is_some()
            );
        }
        for row in 0..side {
            // lane-sb128c r8: libaom's frame buffer is superblock
            // aligned; ours is padded to 32 (`block_grid`), so a 64x64
            // transform unit of a 128x64 block at the BOTTOM frame edge
            // can hang past the last padded row (1920x792: a unit at
            // y=768 wants 832 rows of an 800-row buffer, which panicked
            // the film's key frames). Those samples are outside the
            // frame: never output, and no block below ever references
            // them.
            if y + row >= self.height {
                break;
            }
            for col in 0..side {
                if x + col >= self.width {
                    break;
                }
                let idx = row * side + col;
                let mut base = i32::from(prediction[idx]);
                if let Some((alpha_q3, ac_q3)) = cfl {
                    // The AC buffer is sized for the CfL source block; a transform
                    // block larger than it has no AC sample here, so it adds nothing.
                    let ac = ac_q3.get(idx).copied().unwrap_or(0);
                    base = (base + cfl_scaled(alpha_q3, ac)).clamp(0, sample_max(fctx));
                }
                let sample = (base + residual[idx]).clamp(0, sample_max(fctx)) as u16;
                pix_write(
                    "reconstruct",
                    x,
                    y,
                    side,
                    x + col,
                    y + row,
                    self.data[(y + row) * self.width + x + col],
                    sample,
                );
                self.data.to_mut()[(y + row) * self.width + x + col] = sample;
            }
        }
    }
}

/// Reads one plane's transform block, reconstructs it into `plane` at
/// `(x, y)`/`side`, and records what it leaves behind in `neighbours`.
#[allow(clippy::too_many_arguments)]
pub(super) fn read_plane(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    set: TxbSet,
    scan: &[u16],
    plane_idx: usize,
    around: (bool, bool, i32),
    // The block's own luma mode — [`crate::tile::write_block_planes`] passes
    // it for every plane's `txb` lookup (it is what an intra transform type's
    // CDF is indexed by, spec 9.3), never the plane's own predicted mode.
    tx_mode: usize,
    predict_mode: usize,
    angle_delta: i32,
    reach: Reach,
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    x: usize,
    cy: usize,
    side: usize,
    tx_side: usize,
    base_q_idx: u8,
    cfl: Option<(i32, CflSrc)>,
    filter_intra: Option<usize>,
    // Only meaningful for `plane_idx == 0`: `Some` when this transform unit is
    // smaller than its own block (spec `get_txb_ctx_general`'s
    // `plane_bsize != tx_size` branch), carrying the already-looked-up
    // `SKIP_CONTEXTS` value; `None` everywhere else (a lone luma TU, or any
    // chroma plane), where `txb_skip_ctx` is 0 or the usual OR-of-neighbours
    // formula below.
    luma_skip_ctx: Option<usize>,
    // See [`PlaneBuf::reconstruct`]'s own doc: caller's neighbour-smooth check.
    smooth_neighbor: bool,
    fctx: &crate::decode::FrameCtx,
) -> Result<Grid> {
    // lane-av1mono: a monochrome frame has `NumPlanes == 1`, so libaom's
    // coefficient loop (`for (plane = 0; plane < av1_num_planes(cm); ++plane)`)
    // never reaches a chroma plane -- no u/v coefficient symbol is coded and
    // no chroma write is recorded. Return the all-zero grid the caller's
    // neighbour bookkeeping reads, without consuming a bit.
    if plane_idx > 0 && mono(fctx) {
        return Ok(Grid::Zero(side * side));
    }
    // lane-av1readcensus: the units read from here on are this plane's.
    set_census_plane(plane_idx);
    let skip_ctx = if plane_idx == 0 {
        luma_skip_ctx.unwrap_or(0)
    } else {
        // libaom `get_txb_ctx`: a chroma unit's context is
        // `combine_entropy_contexts(above, left)` plus 7, or plus 10 when the
        // plane block is bigger than the transform. Our chroma tables hold the
        // offset-7 rows first, so the caller adds 3 for the bigger-block case
        // (every chroma unit of a plane block above TX_4X4 -- lane-sb128b r3,
        // and lane-av1h5 measured the 4:4:4 lossless 128 root's TX_4X4 chroma
        // units reading the offset-10 rows too, against aomdec's `EC_DBGCTX`).
        usize::from(around.0) + usize::from(around.1) + luma_skip_ctx.unwrap_or(0)
    };
    // spec 5.11.47/libaom `av1_read_tx_type`: a filter-intra luma block's
    // `intra_ext_tx_cdf` row is indexed by `fimode_to_intradir[filter_intra_mode]`,
    // not by the block's ordinary `mode` (which is always DC_PRED whenever
    // filter-intra is on) -- caller passes the raw filter-intra mode 0..4 in
    // `filter_intra`, only meaningful for `plane_idx == 0` (chroma has no
    // filter-intra in AV1).
    // lane-av1444chr: per-TU mode trace with the PLANE named. `PlaneBuf`'s own
    // `OUR_PRED` rung cannot carry it (reconstruction does not know its
    // plane), and a rung whose plane has to be guessed from the values has
    // already produced wrong claims twice this session -- `read_coeffs_rect`
    // labelling its unit `plane=0`, and a sweep reading a second-luma-unit
    // phantom out of that label (class `rung-mislabels-plane`). Diagnostic
    // only: no gate asserts decode behaviour off this line.
    if crate::envflags::env_flag!("EC_TRACE_MODE") {
        eprintln!(
            "OUR_MODE plane={plane_idx} x={x} y={cy} side={side} mode={predict_mode} \
             ad={angle_delta} fi={filter_intra:?} reach={reach:?}"
        );
    }
    let tx_mode = if plane_idx == 0 {
        fi_tx_row(tx_mode, filter_intra)
    } else {
        tx_mode
    };
    let mut coding = cdfs.txb(set, tx_mode);
    // libaom `read_tx_type` (`av1/decoder/decodetxb.c`): "No need to read
    // transform type for lossless mode(qindex==0)" -- the symbol is not coded
    // at all, and `av1_get_tx_type` (`blockd.h`) returns DCT_DCT for every
    // plane of a lossless block (the residual then goes through the WHT, so
    // the type only picks the scan, which is the default one).
    if lossless(fctx) {
        coding.tx_type = None;
    }
    // A chroma plane's `tx_type` is *never* its own coded symbol
    // (`coding.tx_type` is always `None` for every `TxbSet::Chroma*`), so it is
    // resolved, not read. Three cases, in libaom's own order
    // (`av1_get_tx_type` / `av1_get_ext_tx_set_type`, `blockd.h`):
    //
    // * **intra** chroma: `intra_mode_to_tx_type(mbmi, PLANE_TYPE_UV)` -- the
    //   plane's own predicted mode (`default_intra_tx_type`) -- then clamped by
    //   `av1_get_ext_tx_set_type`. At 32 points and up that set is
    //   `EXT_TX_SET_DCTONLY` (the `is_inter ? EXT_TX_SET_DCT_IDTX :
    //   EXT_TX_SET_DCTONLY` line), which forces `DCT_DCT` no matter what the
    //   mode-indexed table said. `TxbSet::Chroma8`/`Chroma16` never hit this --
    //   every value `default_intra_tx_type` can produce is already a member of
    //   both `TX_SET_INTRA_1`/`_2`, so nothing narrows there -- only
    //   `Chroma32` and up do.
    // * **inter** chroma, i.e. [`INTRABC_CHROMA_TX`]: the same
    //   `xd->tx_type_map` cell the luma unit stamped (`blk_row <<=
    //   pd->subsampling_y`, `blk_col <<= pd->subsampling_x`), reduced by the
    //   INTER transform set. At **32 points** that set is
    //   `EXT_TX_SET_DCT_IDTX`, NOT `EXT_TX_SET_DCTONLY` -- so a 32-point
    //   inter chroma unit inherits the luma's coded `IDTX`, and the identity
    //   transform (a plain copy) is what libaom actually runs.
    //   [`reduce_inherited_chroma_tx_type`] is that reduction verbatim.
    // * 64 points and up is `EXT_TX_SET_DCTONLY` for both, which the same
    //   reduction already yields.
    //
    // The `side >= 32` -> `DCT_DCT` clamp this replaces was written for the
    // intra case (and read as libaom's clamp at `tx_size_sqr_up >= TX_32X32`),
    // but libaom's cap is `txsize_sqr_up_map[tx_size] > TX_32X32` -- 64 and up
    // -- so it also swallowed a 32-point INTER chroma unit's inherited `IDTX`
    // and ran `DCT_DCT` where libaom ran the identity transform.
    // Sub-32 units keep the raw inherited type exactly as before.
    let default_tx_type = if plane_idx == 0 {
        TxType::DctDct
    } else if let Some(inherited) = fctx.intrabc_chroma_tx.with(std::cell::Cell::get) {
        if side >= 32 {
            reduce_inherited_chroma_tx_type(inherited, side, side, fctx)
        } else {
            inherited
        }
    } else if side >= 32 {
        TxType::DctDct
    } else {
        default_intra_tx_type(predict_mode as u8)
    };
    // libaom `av1_get_tx_type` (`blockd.h`): a lossless block's type is
    // DCT_DCT on EVERY plane, so chroma never inherits or mode-derives one.
    let default_tx_type = if lossless(fctx) {
        TxType::DctDct
    } else {
        default_tx_type
    };
    let (grid, tx_type) = read_coeffs(
        dec,
        &mut coding,
        scan,
        skip_ctx,
        dc_sign_ctx(around.2),
        default_tx_type,
        None,
    )?;
    note_chroma_class1(plane_idx, tx_type);
    if plane_idx == 0 {
        fctx.luma_tx_type.with(|c| c.set(tx_type));
    }
    // A 64x64 luma block's transform covers the whole 64x64 area, but only its
    // top-left 32x32 of frequencies are coded (spec 5.11.40); the rest of the
    // dequantized grid stays zero, which `inverse_transform_2d`'s own `< 32`
    // guard also assumes.
    // `grid` is `tx_side x tx_side` (the coded corner); `dequant_and_inverse`
    // wants the true `side x side` transform, with dqDenom (spec 7.12.3)
    // keyed by that true size, not the coded corner's -- so the corner
    // goes into the top-left of a `side`-sized grid, not the reverse.
    let levels = extend_corner(grid, tx_side, tx_side, side, side);
    let (dc_delta, ac_delta) = plane_q_delta(plane_idx, fctx);
    let tx = TxParams {
        plane: plane_idx,
        w: side,
        h: side,
        bit_depth: bit_depth(fctx),
        q_idx: block_q_idx(fctx),
        dc_delta,
        ac_delta,
        tx_type,
        qm: block_iqmatrix(fctx, plane_idx, side, side, tx_type),
        stride: 0,
        lossless: lossless(fctx),
    };
    if crate::envflags::env_flag!("EC_AV1_TRACE") {
        let residual = tx.run(&levels);
        eprintln!(
            "TRACE dequant plane={plane_idx} base_q_idx={base_q_idx} tx_type={tx_type:?} side={side} levels={:?} residual={residual:?}",
            &*levels,
        );
    }
    push_intra_tx(
        y,
        u,
        v,
        plane_idx,
        x,
        cy,
        side,
        side,
        false,
        predict_mode,
        angle_delta,
        reach,
        tx,
        &levels,
        cfl,
        filter_intra,
        smooth_neighbor,
        fctx,
    );
    // `record`'s neighbour state only reads whether *the coded grid* (not the
    // residual) has nonzero entries, so the levels this function already
    // built are what it wants back -- both arms of the old `tx_side == side`
    // branch returned exactly that, one of them through a full `to_vec` copy
    // of the grid per intra transform unit (lane-coefbuf).
    Ok(levels)
}

/// [`push_intra`] when the unit is square, [`push_intra_rect`] when it is a
/// rect (lane-av1-422f): a 4:2:2 square-block chroma unit is the plane
/// block's own rect shape, and the `rect` flag selects
/// [`PlaneBuf::reconstruct_rect`]'s two-dimensional loop.
#[allow(clippy::too_many_arguments)]
pub(super) fn push_intra_unit(
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    plane: usize,
    x: usize,
    cy: usize,
    w: usize,
    h: usize,
    mode: usize,
    angle_delta: i32,
    reach: Reach,
    residual: &[i32],
    cfl: Option<(i32, CflSrc)>,
    filter_intra: Option<usize>,
    smooth_neighbor: bool,
    fctx: &crate::decode::FrameCtx,
) {
    if w == h {
        push_intra(
            y,
            u,
            v,
            plane,
            x,
            cy,
            w,
            mode,
            angle_delta,
            reach,
            residual,
            cfl,
            filter_intra,
            smooth_neighbor,
            fctx,
        );
    } else {
        push_intra_rect(
            y,
            u,
            v,
            plane,
            x,
            cy,
            w,
            h,
            mode,
            angle_delta,
            reach,
            residual,
            cfl,
            filter_intra,
            smooth_neighbor,
            fctx,
        );
    }
}

/// ONE RECT chroma transform unit of a square block's uv plane
/// (lane-av1-422f) -- a 4:2:2 frame's plane block is `(side/2, side)` and
/// `av1_get_max_uv_txsize` fills it with a single rect unit below side 64
/// (TX_4X8 / TX_8X16 / TX_16X32), which the square-shaped [`read_plane`]
/// cannot express. Same shape as [`leaf8_chroma422_unit`]'s proven read: the
/// [`TxbSet`] whose entropy class the unit squares up to, the unit-shaped
/// scan, `txb_skip_ctx` = above + left in the offset-7 rows (the unit covers
/// the plane block exactly, so no `+3` bigger-plane term -- that one lives in
/// the square multi-unit walks' `chroma_ctx_offset`), and the plane's
/// predicted mode supplies the mode-derived default `tx_type` unless an armed
/// [`INTRABC_CHROMA_TX`] slot carries the intrabc luma's own -- except at an
/// entropy class of 32x32 and up, where the spec forces `DCT_DCT`
/// ([`read_plane`]'s `side >= 32` clamp).
#[allow(clippy::too_many_arguments)]
pub(super) fn read_rect_chroma_unit(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    set: TxbSet,
    scan: &[u16],
    w: usize,
    h: usize,
    plane_idx: usize,
    around: (bool, bool, i32),
    uv_predict_mode: usize,
    angle_delta_uv: i32,
    reach: Reach,
    // Unread, like [`read_plane`]'s own: the borrow proves the caller holds
    // the plane exclusively while the unit's coefficients are parsed.
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    x: usize,
    cy: usize,
    // Signature parity with [`read_plane`]; the dequant reads its q index
    // off `block_q_idx(fctx)` like every other reader here.
    _base_q_idx: u8,
    // lane-av1-422p: `get_txb_ctx_general`'s chroma branch
    // (txb_common.h) reads `txb_skip_ctx = above + left + ctx_offset`,
    // where `ctx_offset` is 10 when the plane block is bigger than the
    // transform and 7 otherwise. The port stores the offset-7 rows at
    // `txb_skip_chroma_8` rows 0..3 and the offset-10 rows at rows 3..6
    // (lane-av1-422b), so a unit SMALLER than its plane block passes
    // `Some(3)` to reach the offset-10 rows. Every unit that IS the whole
    // plane block stays `None` -- 4:2:2 plane blocks never shrink to a
    // rect unit except in the chunk arms.
    chroma_skip_offset: Option<usize>,
    cfl: Option<(i32, CflSrc)>,
    smooth_neighbor_uv: bool,
    fctx: &crate::decode::FrameCtx,
) -> Result<Grid> {
    if ss_x(fctx) == 1 && ss_y(fctx) == 0 && w != h {
        hit!(CHROMA422_RECT_HITS);
    }
    // The same three cases as [`read_plane`], for the same reason: at 32 points
    // an INTER chroma unit's transform set is `EXT_TX_SET_DCT_IDTX` (the luma's
    // coded `IDTX` survives) where the intra set is `EXT_TX_SET_DCTONLY`, and
    // an armed [`INTRABC_CHROMA_TX`] slot means the block IS an inter block.
    let default_tx = if let Some(inherited) = fctx.intrabc_chroma_tx.with(std::cell::Cell::get) {
        if w.max(h) >= 32 {
            reduce_inherited_chroma_tx_type(inherited, w, h, fctx)
        } else {
            inherited
        }
    } else if w.max(h) >= 32 {
        TxType::DctDct
    } else {
        default_intra_tx_type(uv_predict_mode as u8)
    };
    let mut coding = cdfs.txb(set, uv_predict_mode);
    let (levels, tx_type) = read_coeffs_rect(
        dec,
        &mut coding,
        scan,
        w,
        h,
        usize::from(around.0) + usize::from(around.1) + chroma_skip_offset.unwrap_or(0),
        dc_sign_ctx(around.2),
        default_tx,
        plane_idx,
    )?;
    let residual = dequant_and_inverse_typed_wh(
        &levels,
        w,
        h,
        crate::decode::bit_depth(fctx),
        block_q_idx(fctx),
        plane_q_delta(plane_idx, fctx).0,
        plane_q_delta(plane_idx, fctx).1,
        tx_type,
        block_iqmatrix(fctx, plane_idx, w, h, tx_type),
    );
    push_intra_rect(
        y,
        u,
        v,
        plane_idx,
        x,
        cy,
        w,
        h,
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

/// Decodes one whole-block coded superblock/quadrant/leaf: its mode, then its
/// three planes, reconstructing them into `y`/`u`/`v` and updating
/// `neighbours`.
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_block(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    at: (usize, usize),
    side: usize,
    luma_set: TxbSet,
    chroma_set: TxbSet,
    luma_tx: usize,
    chroma_tx: usize,
    scans: (&[u16], &[u16]),
    cfl: bool,
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    base_q_idx: u8,
    enable_filter_intra: bool,
    allow_screen_content_tools: bool,
    allow_intrabc: bool,
    scan32: &[u16],
    scan16: &[u16],
    scan8: &[u16],
    scan4: &[u16],
    tx_select: bool,
    reduced_tx_set: bool,
    fctx: &crate::decode::FrameCtx,
) -> Result<()> {
    let (r, c) = at;
    let (px, py) = (c * SUB, r * SUB);
    let (palette_ctx, palette_cache) = neighbours.palette_ctx_and_cache(at);
    let palette_uv_cache = neighbours.palette_uv_cache(at);
    let (nb_above_mode, nb_left_mode) = neighbours.modes_above_left(r, c);
    let at_tile_left = c == neighbours.tile_col0_mi / (SUB / MI);
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
        nb_above_mode,
        nb_left_mode,
        cfl,
        side,
        enable_filter_intra,
        neighbours.skip_txfm_ctx(r * (SUB / MI), c * (SUB / MI)),
        allow_screen_content_tools,
        allow_intrabc,
        Some((palette_ctx, &palette_cache)),
        &palette_uv_cache,
        r * (SUB / MI),
        c * (SUB / MI),
        fctx,
    )?;
    if at_tile_left && palette_y.is_some() {
        hit!(PALETTE_TILE_LEFT_HITS);
    }
    // lane-lossless: a lossless BLOCK's transform is TX_4X4 on every plane
    // (libaom `read_tx_size`, `decodeframe.c`), so the caller's per-size
    // tables/scans are replaced by the 4x4 ones and the block always takes the
    // multi-transform-unit path below. lane-av1mixloss: this rewrite used to
    // sit at the TOP of the function -- before `read_intra_mode`, which is
    // where the block's own `segment_id` is read -- so a mixed frame resolved
    // every block's geometry against the PREVIOUS block's segment (a lossless
    // neighbour forced 4x4 units on a lossy block; a lossy one handed the
    // lossless TxParams an 8x8 grid). libaom's order is the opposite one:
    // `decode_mbmi_block` (segment id included) runs before any tx-size rule.
    // lane-av1mixmerge: the 4:4:4 chroma-size arm from main's copy of this
    // rewrite (which sat above `read_intra_mode`) rides along here -- same
    // per-frame `ss_x`/`ss_y` condition, now resolved against the block's own
    // segment like the lossless arm above it.
    let (luma_set, chroma_set, luma_tx, chroma_tx, chroma_tx_h, scans) = if lossless(fctx) {
        (TxbSet::Luma4, TxbSet::Chroma4, 4, 4, 4, (scan4, scan4))
    } else if ss_x(fctx) == 0 && ss_y(fctx) == 0 {
        let tx = if side >= 64 { 32 } else { side };
        let (set, scan): (TxbSet, &[u16]) = match tx {
            32 => (TxbSet::Chroma32, scan32),
            16 => (TxbSet::Chroma16, scan16),
            8 => (TxbSet::Chroma8, scan8),
            _ => (TxbSet::Chroma4, scan4),
        };
        (luma_set, set, luma_tx, tx, tx, (scans.0, scan))
    } else if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
        // lane-av1-422f: the uv plane block is the (side/2, side) rect
        // (`ss_size_lookup[..][1][0]`), and `av1_get_max_uv_txsize` is
        // `av1_get_adjusted_tx_size(max_txsize_rect_lookup[plane_bsize])` --
        // the RECT that fills the plane block, NOT the square transform of
        // the smaller side (the oracle reads ONE TX_8X16 for a 16x16 block's
        // 8x16 plane where the old model read two 8x8 units): 4 -> TX_4X4
        // (the 2x4 plane rounds up to BLOCK_4X4), 8 -> TX_4X8, 16 ->
        // TX_8X16, 32 -> TX_16X32 (one unit per plane, the whole plane
        // block), 64 -> TX_32X64 adjusts to TX_32X32 and 128 -> TX_64X64
        // adjusts to TX_32X32 (square units, a 1x2 / 2x4 grid).
        // `chroma_tx` is the unit WIDTH, `chroma_tx_h` its HEIGHT; the
        // chroma walks below read one (chroma_tx, chroma_tx_h) unit per grid
        // cell and key their rect reads on `chroma_tx != chroma_tx_h`.
        let (tx_w, tx_h) = match side {
            4 => (4, 4),
            8 => (4, 8),
            16 => (8, 16),
            32 => (16, 32),
            // 64 and 128: TX_32X64 / TX_64X64 adjust down to TX_32X32.
            _ => (32, 32),
        };
        let (set, scan): (TxbSet, &[u16]) = match side {
            4 => (TxbSet::Chroma4, scan4),
            8 => (TxbSet::ChromaRect8x4, &SCAN_4X8[..]),
            16 => (TxbSet::ChromaRect16x8, &SCAN_8X16[..]),
            32 => (TxbSet::ChromaRect32x16, &SCAN_16X32[..]),
            _ => (TxbSet::Chroma32, scan32),
        };
        (luma_set, set, luma_tx, tx_w, tx_h, (scans.0, scan))
    } else {
        (luma_set, chroma_set, luma_tx, chroma_tx, chroma_tx, scans)
    };
    let intrabc_dv = fctx.intrabc_dv.with(|c| c.take());
    // `predict` panics outside `DC_PRED..=PAETH_PRED` (0..=12); `UV_CFL_PRED`
    // (13) predicts as `DC_PRED` with the [`cfl_scaled`] nudge carrying the
    // actual chroma-from-luma correlation, same as this decoder always did.
    let uv_predict_mode = if uv_mode == UV_CFL_PRED {
        DC_PRED
    } else {
        uv_mode
    };
    let smooth_neighbor = is_smooth_mode(nb_above_mode) || is_smooth_mode(nb_left_mode);
    if smooth_neighbor {
        hit!(SMOOTH_LUMA_HITS);
    }
    let smooth_neighbor_uv =
        neighbours.smooth_uv_neighbour(r * (SUB / MI), c * (SUB / MI), r, c, fctx);
    if crate::envflags::env_flag!("EC_AV1_TRACE") {
        eprintln!(
            "TRACE block px={px} py={py} side={side} mode={mode} uv_mode={uv_mode} \
             angle_delta_uv={angle_delta_uv}"
        );
    }
    let (mi_r, mi_c) = (r * (SUB / MI), c * (SUB / MI));
    // `TxMode::Select`'s `tx_depth` (spec 5.11.16): read for every intra
    // block, skipped or not. `logical_tx` is what the loop filter's edge
    // lookup and the next block's own `get_tx_size_context` want (the *nominal*
    // transform size -- 64 even for the 64x64 corner-scanned case, spec
    // 5.11.40); `coeff_tx_side` is the actual coded coefficient grid's side,
    // which only differs from it at that one corner case.
    // lane-t900 r32: spec 5.11.16 `read_block_tx_size` gates the `tx_depth`
    // symbol on `allowSelect = !skip || !is_inter`, and libaom's
    // `is_inter_block` counts an INTRABC block (`blockd.h:373`). So a SKIPPED
    // intrabc block reads NO tx-size symbol at all -- it takes
    // `maxRectTxSize` -- while every ordinary skipped intra block does read
    // one. Reading it anyway consumed a symbol libaom never wrote and desynced
    // the tile from that block on (class `equal-range-means-unread`).
    // lane-t900 r32: and an UNSKIPPED one reads the inter var-tx partition
    // tree instead (`read_var_tx_size`), the same syntax the inter block path
    // has always read -- libaom's `parse_decode_block` dispatches on
    // `is_inter_block`, so both branches of `read_block_tx_size` treat an
    // intrabc block as inter.
    let intrabc_skip_tx = intrabc_dv.is_some() && skip;
    // libaom `parse_decode_block`'s var-tx tree condition carries
    // `!xd->lossless[mbmi->segment_id]` (decodeframe.c:1237): an intrabc
    // block inside a LOSSLESS segment of a mixed frame reads NO
    // `txfm_partition` tree -- it falls to `read_tx_size`, whose first line
    // hands back TX_4X4. Invisible while every lossless frame was also
    // `CodedLossless` (`tx_mode` forced `Only4x4`, tree unreachable), which
    // is why the mixed frame is the first stream to reach it.
    let intrabc_vartx = intrabc_dv.is_some() && tx_select && !skip && !lossless(fctx);
    let intrabc_leaves = if intrabc_vartx {
        let max_tx = side.min(64);
        // `max_block_wide`/`max_block_high`: the frame's own mi grid
        // (`av1_calc_mi_size`, 8-px aligned), so a block hanging over the
        // right/bottom edge reads no split symbol for the units outside it.
        let mi_cols = 2 * y.true_width.div_ceil(8);
        let mi_rows = 2 * y.true_height.div_ceil(8);
        let side_mi = side / MI;
        let max_w_mi = side_mi.min(mi_cols.saturating_sub(mi_c));
        let max_h_mi = side_mi.min(mi_rows.saturating_sub(mi_r));
        let mut leaves = Vec::new();
        for row in (0..side_mi).step_by(max_tx / MI) {
            for col in (0..side_mi).step_by(max_tx / MI) {
                read_var_tx_size(
                    dec,
                    cdfs,
                    neighbours,
                    (mi_r, mi_c),
                    side,
                    (max_w_mi, max_h_mi),
                    (max_tx, max_tx),
                    0,
                    row,
                    col,
                    &mut leaves,
                );
            }
        }
        hit!(INTRABC_VARTX_HITS);
        // lane-t900 r33 census: the t900 recipes resolved every tree UNIFORM.
        // lane-av1-ibcvtx: a tree that resolved to mixed leaf sizes is no
        // longer refused -- the luma reconstruct below walks the tree's own
        // leaf list per leaf (libaom `decode_token_recon_block`, whose token
        // loop reconstructs whatever `read_tx_size_vartx` resolved, per
        // transform unit), so mixed sizes reconstruct at their own stride.
        // The counter marks the shape so a gate can prove a witness actually
        // carries a mixed tree and not merely another uniform one.
        let uniform = leaves.first().is_some_and(|f| {
            leaves
                .iter()
                .all(|l| (l.2, l.3) == (f.2, f.3) && l.2 == l.3)
        });
        if !uniform {
            hit!(INTRABC_VARTX_MIXED_LEAVES_HITS);
            if crate::envflags::env_flag!("EC_AV1_IBCVTX_DEBUG") {
                eprintln!(
                    "IBCVTX mixed intrabc tree mi=({mi_r},{mi_c}) side={side} \
                     dv={intrabc_dv:?} leaves={leaves:?}"
                );
            }
        }
        Some(leaves)
    } else {
        None
    };
    let (logical_tx, coeff_tx_side) = if let Some(tx) = intrabc_leaves.as_ref().map(|l| l[0].2) {
        (tx, if tx == 64 { 32 } else { tx })
    } else if tx_select && !intrabc_skip_tx {
        let resolved = read_tx_size(dec, cdfs, neighbours, (mi_r, mi_c), side, fctx);
        (resolved, if resolved == 64 { 32 } else { resolved })
    } else {
        // lane-sb128b r1: `max_txsize_rect_lookup[BLOCK_128X128]` is
        // TX_64X64, so a 128x128 block is always FOUR luma transform units
        // even with `tx_select` off -- the TU loop below, never the
        // single-unit path.
        (lossless_tx(side.min(64), fctx), luma_tx)
    };
    if tx_select && logical_tx < side && uv_mode != DC_PRED {
        hit!(SQ_CHROMA_TX_HITS);
    }
    let luma_set = if tx_select {
        txbset_for(logical_tx, reduced_tx_set)
    } else {
        luma_set
    };
    let luma_set = if intrabc_dv.is_some() {
        inter_txbset_for(logical_tx, reduced_tx_set)
    } else {
        luma_set
    };
    let luma_scan = scan_for(coeff_tx_side, scan32, scan16, scan8, scan4);
    let reach = Reach::of(side, px, py, y.width, y.height, fctx);
    let (cpx, cpy) = (px >> ss_x(fctx), py >> ss_y(fctx));
    let chroma_side = side >> ss_x(fctx);
    // lane-av1-422b: chroma extent per axis, `side` at ss_y 0 (4:2:2/4:4:4),
    // `side / 2` at 4:2:0.
    let chroma_height = side >> ss_y(fctx);
    // spec 7.11.3.4 with `use_intrabc`: the reference is the *current*
    // frame's own pre-loop-filter reconstruction (all loop filters are off
    // whenever `allow_intrabc`, frame.rs:194/210/252/272) at the full-pel
    // block vector, with the BILINEAR kernel libaom forces for intrabc --
    // luma is a straight copy, chroma lands half-pel whenever the luma DV is
    // odd, which is exactly what the bilinear taps then interpolate.
    let intrabc_bufs = intrabc_dv.map(|(dv_row, dv_col)| {
        let mut yb = vec![0u16; side * side];
        mc::predict_with_filter(
            &y.data,
            y.width,
            y.true_width,
            y.true_height,
            mv_to_q4(px, dv_col, 0),
            mv_to_q4(py, dv_row, 0),
            side,
            side,
            mc::InterpFilterKind::Bilinear,
            &mut yb,
            fctx,
        );
        let mut ub = vec![0u16; chroma_side * chroma_height];
        mc::predict_with_filter(
            &u.data,
            u.width,
            u.true_width,
            u.true_height,
            mv_to_q4(cpx, dv_col, ss_x(fctx)),
            mv_to_q4(cpy, dv_row, ss_y(fctx)),
            chroma_side,
            chroma_height,
            mc::InterpFilterKind::Bilinear,
            &mut ub,
            fctx,
        );
        let mut vb = vec![0u16; chroma_side * chroma_height];
        mc::predict_with_filter(
            &v.data,
            v.width,
            v.true_width,
            v.true_height,
            mv_to_q4(cpx, dv_col, ss_x(fctx)),
            mv_to_q4(cpy, dv_row, ss_y(fctx)),
            chroma_side,
            chroma_height,
            mc::InterpFilterKind::Bilinear,
            &mut vb,
            fctx,
        );
        (yb, ub, vb)
    });
    // A palette-Y block's own prediction is the reconstructed colour-index
    // map, not `predict()`'s edge-based one -- [`PALETTE_PRED`] carries it
    // to [`PlaneBuf::reconstruct`]'s next call rather than threading a
    // parameter through this function's dozen `y.reconstruct`/`read_plane`
    // sites. A split luma transform (`tx_select && logical_tx != side`)
    // would need the map sliced per-TU -- untested against a real
    // `--enable-palette=1` stream this round, so refuse by name rather than
    // guess at the slicing (charter's own named-acceptable fallback).
    let palette_y_buf: Option<Vec<u16>> = palette_y
        .as_ref()
        .map(|py| py.map.iter().map(|&idx| py.colors[idx as usize]).collect());
    // lane-t900 r32: the intrabc luma prediction rides the very same
    // whole-block override slot, so the multi-TU loop below windows it per
    // transform unit exactly like a palette map -- setting it once up front
    // handed the whole `side`x`side` buffer to the FIRST unit and left every
    // later one predicting off block edges (the var-tx tree above is what
    // first makes `logical_tx < side` reachable for an intrabc block).
    let palette_y_buf = match &intrabc_bufs {
        Some((yb, _, _)) => Some(yb.clone()),
        None => palette_y_buf,
    };
    // With one luma transform unit the whole `side`x`side` map is the
    // prediction; with several (`logical_tx != side`) each unit takes its own
    // `logical_tx`-sized window of that same map, set fresh inside the TU loop
    // below (lane-palette2 r7) -- setting the full-block buffer here would be
    // taken by the FIRST unit and indexed at the wrong stride.
    // lane-svt1: a SKIPPED block predicts the whole `side`x`side` in ONE call
    // (the `else` arm below -- a palette/intrabc block never takes the per-unit
    // `split_tx_skip` walk), so its map must be armed here even when the
    // transform is split. Gating the arm on `logical_tx == side` alone left a
    // 16x16 palette block with `tx_depth == 1` and `skip == 1` predicting DC
    // off its own edges: right symbols, wrong pixels (SVT-AV1 preset-8 screen
    // key frame; class override-slot-on-one-arm). Neither prediction is
    // edge-derived, so the whole-block buffer at the block's own stride is
    // exactly what that single call wants.
    // Counted on the SHAPE, not on the arm below, so a gate can tell a stream
    // that reaches this block apart from one that never does (class
    // gate-blind-to-feature).
    if palette_y_buf.is_some() && skip && logical_tx < side {
        hit!(SKIP_SPLIT_TX_OVERRIDE_HITS);
    }
    if let Some(buf) = &palette_y_buf
        && (logical_tx == side || skip)
    {
        set_palette_pred(buf.clone(), fctx);
    }
    // A UV-palette block's own chroma prediction override (lane-palette2 r1),
    // same [`PALETTE_PRED`] slot as Y above -- set fresh right before each
    // plane's own `reconstruct`/`read_plane` call below (never both planes at
    // once), since Y's own set-then-take already emptied the slot by the time
    // either of these run.
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
    // The chroma half of the intrabc prediction rides the same per-plane
    // override slot the UV-palette buffers do (set fresh before each plane's
    // own `reconstruct`/`read_plane` call below).
    let palette_uv_bufs = match &intrabc_bufs {
        Some((_, ub, vb)) => Some((ub.clone(), vb.clone())),
        None => palette_uv_bufs,
    };
    if skip {
        // A skipped block codes no residual syntax at all (spec 5.11.34):
        // straight prediction, on every plane.
        //
        // lane-hgkf r2: an intra block's `tx_depth` is read whether or not it
        // is skipped (only an INTER skip forces the largest transform), and
        // spec 7.11.2 / libaom `predict_and_reconstruct_intra_block` predicts
        // ONE TRANSFORM UNIT AT A TIME -- each unit's edges are the units
        // already written left/above of it inside this same block. Predicting
        // the whole `side`x`side` at once gives a different (uniformly off by
        // ~1) answer, which is exactly the residue this lane was chasing.
        // The palette/intrabc override path keeps the single-call shape (its
        // per-TU windowing is the non-skip loop's own `palette_y_buf` slice,
        // untested for skip and unreachable in these streams).
        // lane-sb128b r2: a 128x128 block's transform is TX_64X64 even with
        // `tx_select` off (`max_txsize_rect_lookup[BLOCK_128X128]`), so the
        // per-unit prediction is keyed on the transform being smaller than
        // the block, not on the tx-depth syntax. The counter keeps lane-hgkf's
        // narrower meaning (a READ split tx depth).
        let split_tx_skip = logical_tx < side && palette_y_buf.is_none() && intrabc_bufs.is_none();
        if split_tx_skip {
            if tx_select {
                hit!(SKIP_SPLIT_TX_HITS);
            }
            let n_axis = side / logical_tx;
            for tu_row in 0..n_axis {
                for tu_col in 0..n_axis {
                    let tu_px = px + tu_col * logical_tx;
                    let tu_py = py + tu_row * logical_tx;
                    if tu_px >= y.true_width || tu_py >= y.true_height {
                        continue;
                    }
                    let tu_reach = tu_reach(
                        side,
                        side,
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
                        tu_reach,
                        &ZERO_RESIDUAL[..logical_tx * logical_tx],
                        None,
                        filter_intra,
                        smooth_neighbor,
                        fctx,
                    );
                }
            }
        } else {
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
        // lane-av1-422b: the chroma unit grid is `cn_cols` x `cn_rows` --
        // square at 4:2:0/4:4:4, at 4:2:2 `(side/2)/tx` units wide by
        // `side/tx` tall (the plane block is (side/2, side)). A unit's LUMA
        // anchor follows the per-axis shifts: `chroma_tx << ss_x` wide,
        // `chroma_tx_h << ss_y` tall (lane-av1-422f: the unit itself is the
        // (chroma_tx, chroma_tx_h) rect, so a 4:2:2 frame below side 64
        // reads ONE unit per plane, the whole plane block).
        let cn_cols = (chroma_side / chroma_tx).max(1);
        let cn_rows = (chroma_height / chroma_tx_h).max(1);
        let luma_span_x = chroma_tx << ss_x(fctx);
        let luma_span_y = chroma_tx_h << ss_y(fctx);
        for cu_row in 0..cn_rows {
            for cu_col in 0..cn_cols {
                let cu_r = if cn_rows == 1 && cn_cols == 1 {
                    reach
                } else {
                    tu_reach_rect(
                        side,
                        side,
                        cu_col * luma_span_x,
                        cu_row * luma_span_y,
                        luma_span_x,
                        luma_span_y,
                        reach,
                        px,
                        py,
                        y.width,
                        y.height,
                        fctx,
                    )
                };
                let (cu_x, cu_y) = (cpx + cu_col * chroma_tx, cpy + cu_row * chroma_tx_h);
                // lane-av1chromadc: the chroma override is a whole
                // `chroma_side x chroma_height` buffer and
                // `PlaneBuf::reconstruct` uses the buffer it takes as the
                // UNIT's own `chroma_tx x chroma_tx_h` prediction, so a
                // multi-unit plane block must hand each unit its own window of
                // it. Handing the whole buffer (the pre-fix shape) made every
                // unit predict from the top-left one: at 4:2:0 the chroma plane
                // block is a single 32x32 unit, so the whole buffer IS that
                // unit's window and the bug is invisible -- which is why every
                // 4:2:0 gate stayed green -- and at 4:4:4 a 64x64 block's 64x64
                // plane block is a 2x2 grid whose right and bottom columns were
                // a copy of its top-left unit.
                // MEASURED on `fixtures/r512.obu` (4:4:4 8-bit 512x512, three
                // frames): frame 0's skipped 64x64 intra-BC block at
                // `mi=(112,80)` `px=(320,448)` reconstructed its `cu=(1,0)`
                // unit at `px=(352,448)` as a flat 241 where the oracle has
                // 202 -- byte-identical to its `cu=(0,0)` prediction, sum
                // 882176 and all -- and the coded 64x64 to its right then read
                // 202/175 against the oracle's 165/166. Gate:
                // `a_444_skipped_64x64_square_block_windows_its_chroma_override_per_unit`.
                let chroma_window = |buf: &[u16]| -> Vec<u16> {
                    if cn_cols == 1 && cn_rows == 1 {
                        return buf.to_vec();
                    }
                    let mut window = Vec::with_capacity(chroma_tx * chroma_tx_h);
                    for row in 0..chroma_tx_h {
                        let at = (cu_row * chroma_tx_h + row) * chroma_side + cu_col * chroma_tx;
                        window.extend_from_slice(&buf[at..at + chroma_tx]);
                    }
                    window
                };
                if let Some((ub, _)) = &palette_uv_bufs {
                    if cn_cols * cn_rows > 1 {
                        hit!(SKIP_CHROMA_OVERRIDE_WINDOW_HITS);
                    }
                    set_palette_pred(chroma_window(ub), fctx);
                }
                push_intra_unit(
                    y,
                    u,
                    v,
                    1,
                    cu_x,
                    cu_y,
                    chroma_tx,
                    chroma_tx_h,
                    uv_predict_mode,
                    angle_delta_uv,
                    cu_r,
                    &ZERO_RESIDUAL[..chroma_tx * chroma_tx_h],
                    alpha.zip(ac).map(|((au, _), ac)| (au, ac)),
                    None,
                    smooth_neighbor_uv,
                    fctx,
                );
                if let Some((_, vb)) = &palette_uv_bufs {
                    set_palette_pred(chroma_window(vb), fctx);
                }
                push_intra_unit(
                    y,
                    u,
                    v,
                    2,
                    cu_x,
                    cu_y,
                    chroma_tx,
                    chroma_tx_h,
                    uv_predict_mode,
                    angle_delta_uv,
                    cu_r,
                    &ZERO_RESIDUAL[..chroma_tx * chroma_tx_h],
                    alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
                    None,
                    smooth_neighbor_uv,
                    fctx,
                );
            }
        }
        let luma_grid = vec![0i32; side * side];
        // lane-av1muchroma site 1: the skip arm's chroma grids are the
        // block's PLANE BLOCK per axis (`chroma_side` x `chroma_height`),
        // not a square. `chroma_side` here is `side >> ss_x` -- the key-frame
        // path's own binding, which at 4:2:2 (ss (1,0)) is `side / 2`, so the
        // old `chroma_side * chroma_side` allocated HALF the block's chroma:
        // a 64x64 block's plane block is 32x64 = 2048 samples and the square
        // held 1024, a 2x under-read for any per-row consumer. No current
        // consumer indexes it -- `record_rect` -> `record_mi_rect` reduces
        // each plane through `neighbour_state`, which sums the slice and
        // reads `grid[0]` only, and every sample is zero on a skip block --
        // so the shape is latent, not live (mutation-verified: sizing this
        // per-axis leaves every pinned 4:2:2 gate green, shrinking sites
        // 11/12 to per-axis reds two). Fixed here so the allocation cannot
        // go short if a per-row consumer ever lands. At 4:2:0 the two are
        // the same number and at 4:4:4 `chroma_side == chroma_height ==
        // side`, so only 4:2:2 changes.
        let u_grid = vec![0i32; chroma_side * chroma_height];
        let v_grid = vec![0i32; chroma_side * chroma_height];
        // The measured arm, not a proxy: how many samples this block's
        // chroma bookkeeping grid is SHORT of its own plane block. The
        // square form this lane removed is `chroma_side * chroma_side`,
        // which at 4:2:2 is half the plane block; restoring it makes this
        // non-zero on every 4:2:2 skip block, and the witness gate asserts
        // the total stayed 0.
        let plane_block = chroma_side * chroma_height;
        let short = plane_block.saturating_sub(u_grid.len());
        hit_do!(SKIP_ARM_CHROMA_SHORT_SAMPLES.with(|c| c.set(c.get() + short)));
        if chroma_side != chroma_height {
            hit!(SKIP_ARM_CHROMA_RECT_SHAPES);
        }
        neighbours.record(
            at,
            side,
            mode,
            uv_predict_mode,
            &[&luma_grid[..], &u_grid[..], &v_grid[..]],
        );
    } else if logical_tx == side {
        // lane-sb128b r1: the test used to be `!tx_select || logical_tx == side`.
        // Without `tx_select` `logical_tx` IS `side` at every size <= 64, so
        // that arm was redundant -- and wrong at 128, where
        // `max_txsize_rect_lookup` caps the block's transform at 64 and the
        // block is four units whether or not `tx_select` is on.
        // A single luma transform unit -- the old path, unchanged (including
        // the 64x64 corner case, `logical_tx == side == 64` with
        // `coeff_tx_side == 32`).
        let around = neighbours.around(at, side);
        let luma_grid = read_plane(
            dec,
            cdfs,
            luma_set,
            luma_scan,
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
            coeff_tx_side,
            base_q_idx,
            None,
            filter_intra,
            None,
            smooth_neighbor,
            fctx,
        )?;
        let ac = alpha.map(|_| cfl_src(px, py, side));
        // Armed for exactly this block's two chroma reads (see
        // [`INTRABC_CHROMA_TX`]); the luma call above just set
        // [`LUMA_TX_TYPE`] to the type it coded.
        if intrabc_dv.is_some() {
            fctx.intrabc_chroma_tx
                .with(|c| c.set(Some(fctx.luma_tx_type.with(std::cell::Cell::get))));
        }
        // lane-av1-444: this arm is one luma TU (`logical_tx == side`), including
        // the 64x64 case whose coefficient grid is 32. At 4:4:4 the chroma plane
        // is the same size as luma and `av1_get_max_uv_txsize` caps it at
        // TX_32X32, so a 64x64 block codes four chroma units, plane-major
        // (every U, then every V) — the same loop the multi-TU arm runs.
        // At 4:2:0 `chroma_side == chroma_tx`, so it stays one read.
        // lane-av1-422b: 4:2:2 never collapses to one read above a 4x4 luma
        // block -- the unit grid is `(side/2)/tx` wide by `side/tx` tall.
        // lane-av1-422f: `chroma_tx` is the unit WIDTH and `chroma_tx_h` its
        // HEIGHT, so below side 64 a 4:2:2 frame's grid is 1x1 and the one
        // unit is the plane block's own rect.
        let cn_cols = (chroma_side / chroma_tx).max(1);
        let cn_rows = (chroma_height / chroma_tx_h).max(1);
        let luma_span_x = chroma_tx << ss_x(fctx);
        let luma_span_y = chroma_tx_h << ss_y(fctx);
        let (u_grid, v_grid, chroma_units) = if cn_rows == 1 && cn_cols == 1 {
            if chroma_tx == chroma_tx_h {
                if let Some((ub, _)) = &palette_uv_bufs {
                    set_palette_pred(ub.clone(), fctx);
                }
                let u_grid = read_plane(
                    dec,
                    cdfs,
                    chroma_set,
                    scans.1,
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
                    chroma_side,
                    chroma_tx,
                    base_q_idx,
                    alpha.zip(ac).map(|((au, _), ac)| (au, ac)),
                    None,
                    None,
                    smooth_neighbor_uv,
                    fctx,
                )?;
                if crate::envflags::env_flag!("EC_AV1_TRACE") {
                    eprintln!(
                        "TRACE u-write cpx={cpx} cpy={cpy} chroma_side={chroma_side} row0={:?}",
                        &u.data[cpy * u.width + cpx..][..chroma_side]
                    );
                }
                if let Some((_, vb)) = &palette_uv_bufs {
                    set_palette_pred(vb.clone(), fctx);
                }
                let v_grid = read_plane(
                    dec,
                    cdfs,
                    chroma_set,
                    scans.1,
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
                    chroma_side,
                    chroma_tx,
                    base_q_idx,
                    alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
                    None,
                    None,
                    smooth_neighbor_uv,
                    fctx,
                )?;
                (u_grid, v_grid, Vec::new())
            } else {
                // lane-av1-422f: the one unit is a RECT filling the plane
                // block -- the square-shaped [`read_plane`] cannot express
                // it, so both planes go through [`read_rect_chroma_unit`]
                // (U, then V, the same order the square arm reads). The
                // chroma context reads per chroma 4-px cell (see
                // [`Neighbours::around_mi_422_chroma`]).
                let chroma_around = neighbours.around_mi_422_chroma((mi_r, mi_c), side, side);
                if let Some((ub, _)) = &palette_uv_bufs {
                    set_palette_pred(ub.clone(), fctx);
                }
                let u_grid = read_rect_chroma_unit(
                    dec,
                    cdfs,
                    chroma_set,
                    scans.1,
                    chroma_tx,
                    chroma_tx_h,
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
                )?;
                if let Some((_, vb)) = &palette_uv_bufs {
                    set_palette_pred(vb.clone(), fctx);
                }
                let v_grid = read_rect_chroma_unit(
                    dec,
                    cdfs,
                    chroma_set,
                    scans.1,
                    chroma_tx,
                    chroma_tx_h,
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
                )?;
                (u_grid, v_grid, Vec::new())
            }
        } else {
            let chroma_ctx_offset = Some(3);
            let mut u_pass: Vec<Grid> = Vec::new();
            let mut units: Vec<((usize, usize), usize, usize, Grid, Grid)> = Vec::new();
            let mut last_grids = [Grid::Zero(0), Grid::Zero(0)];
            for plane_pass in 1..=2usize {
                let mut pass_idx = 0usize;
                for cu_row in 0..cn_rows {
                    for cu_col in 0..cn_cols {
                        let cu_mi = (
                            mi_r + cu_row * (luma_span_y / MI),
                            mi_c + cu_col * (luma_span_x / MI),
                        );
                        let cu_around = if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                            // lane-av1-422f: chroma-only loop; per-chroma-cell
                            // above votes (see [`Neighbours::around_mi_422_chroma`]).
                            neighbours.around_mi_422_chroma(cu_mi, luma_span_x, luma_span_y)
                        } else {
                            neighbours.around_mi_rect(cu_mi, luma_span_x, luma_span_y)
                        };
                        let cu_reach = tu_reach_rect(
                            side,
                            side,
                            cu_col * luma_span_x,
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
                        let (cu_x, cu_y) = (cpx + cu_col * chroma_tx, cpy + cu_row * chroma_tx_h);
                        if let Some((ub, vb)) = &palette_uv_bufs {
                            let pbuf = if plane_pass == 1 { ub } else { vb };
                            set_palette_pred(
                                palette_window(
                                    pbuf,
                                    chroma_side,
                                    cu_col * chroma_tx,
                                    cu_row * chroma_tx_h,
                                    chroma_tx,
                                    chroma_tx_h,
                                ),
                                fctx,
                            );
                        }
                        let cu_grid = read_plane(
                            dec,
                            cdfs,
                            chroma_set,
                            scans.1,
                            plane_pass,
                            cu_around[plane_pass],
                            mode,
                            uv_predict_mode,
                            angle_delta_uv,
                            cu_reach,
                            y,
                            u,
                            v,
                            cu_x,
                            cu_y,
                            chroma_tx,
                            chroma_tx,
                            base_q_idx,
                            alpha
                                .zip(ac)
                                .map(|((au, av), ac)| (if plane_pass == 1 { au } else { av }, ac)),
                            None,
                            chroma_ctx_offset,
                            smooth_neighbor_uv,
                            fctx,
                        )?;
                        hit!(CHROMA_SPLIT_TX_HITS);
                        if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                            hit!(CHROMA422_SQUARE_HITS);
                        }
                        // Immediately, so the next unit of this block reads this
                        // one's coefficient context. `record` below stamps one
                        // chroma state across the whole block; the replay after
                        // it puts the per-unit state back.
                        neighbours.record_mi_chroma(
                            cu_mi,
                            luma_span_x,
                            luma_span_y,
                            plane_pass,
                            &cu_grid,
                        );
                        if plane_pass == 1 {
                            u_pass.push(cu_grid);
                        } else {
                            let u_grid = std::mem::replace(&mut u_pass[pass_idx], Grid::Zero(0));
                            units.push((
                                cu_mi,
                                luma_span_x,
                                luma_span_y,
                                u_grid.clone(),
                                cu_grid.clone(),
                            ));
                            last_grids = [u_grid, cu_grid];
                        }
                        pass_idx += 1;
                    }
                }
            }
            (last_grids[0].clone(), last_grids[1].clone(), units)
        };
        fctx.intrabc_chroma_tx.with(|c| c.set(None));
        neighbours.record(
            at,
            side,
            mode,
            uv_predict_mode,
            &[&luma_grid[..], &u_grid[..], &v_grid[..]],
        );
        for (cu_mi, sw, sh, u_unit, v_unit) in &chroma_units {
            neighbours.record_mi_chroma(*cu_mi, *sw, *sh, 1, u_unit);
            neighbours.record_mi_chroma(*cu_mi, *sw, *sh, 2, v_unit);
        }
    } else {
        // Several luma transform units, raster order (spec
        // `transform_block`'s loop): the block's resolved transform is
        // genuinely smaller than its own side, so each tile of it predicts
        // and codes independently, its own context read fresh off whatever
        // the earlier tiles in this same block already wrote -- the same
        // per-TU pattern [`decode_leaf8`] already runs for an 8x8 leaf.
        let n_axis = side / logical_tx;
        // lane-av1-422f: unit width/height per axis, like walk 2 above --
        // a 4:2:2 frame below side 64 reads ONE rect unit per plane per
        // chunk, the whole plane block.
        let cn_cols = (chroma_side / chroma_tx).max(1);
        let cn_rows = (chroma_height / chroma_tx_h).max(1);
        let luma_span_x = chroma_tx << ss_x(fctx);
        let luma_span_y = chroma_tx_h << ss_y(fctx);
        // libaom's transform-block loop is over 64x64 luma "mu" chunks
        // (`decode_token_recon_block`: `mu_blocks_wide/high` =
        // `mi_size_wide[BLOCK_64X64]`), and it codes EVERY PLANE of a chunk
        // before moving to the next one. Below 128 a block is a single chunk,
        // so this is byte-for-byte the all-luma-then-all-chroma order that has
        // always run; at 128 the four quadrants interleave Y,U,V per quadrant
        // and reading all four luma units first desyncs the tile at the second
        // one (lane-sb128b r3, measured against aomdec's `EC_TRACE_COEFF`).
        let nchunk = (side / 64).max(1);
        // `cn > 1` means the uv plane block is larger than the uv transform,
        // which moves libaom's chroma `txb_skip_ctx` offset from 7 to 10.
        let chroma_ctx_offset = if cn_rows * cn_cols > 1 { Some(3) } else { None };
        let tpc = (n_axis / nchunk).max(1);
        let cpc_rows = (cn_rows / nchunk).max(1);
        let cpc_cols = (cn_cols / nchunk).max(1);
        // The block's luma transform units, raster order inside each mu
        // chunk. lane-av1-ibcvtx: an intrabc block's units are the var-tx
        // tree's own leaf list -- mixed sizes included, exactly what libaom's
        // `decode_token_recon_block` token loop reconstructs (one
        // `av1_read_coeffs_txb` + inverse transform per resolved unit) --
        // while every other block's list is the uniform `logical_tx` grid
        // this loop has always walked, generated here in the same order.
        let mut chunk_tus: Vec<Vec<(usize, usize, usize)>> = vec![Vec::new(); nchunk * nchunk];
        if let Some(leaves) = &intrabc_leaves {
            for &(lr, lc, tw, _th) in leaves {
                let (off_y, off_x) = (lr * MI, lc * MI);
                chunk_tus[(off_y / 64) * nchunk + (off_x / 64)].push((off_y, off_x, tw));
            }
        } else {
            for tu_row in 0..n_axis {
                for tu_col in 0..n_axis {
                    chunk_tus[(tu_row / tpc) * nchunk + (tu_col / tpc)].push((
                        tu_row * logical_tx,
                        tu_col * logical_tx,
                        logical_tx,
                    ));
                }
            }
        }
        let mut last_grids = [Grid::Zero(0), Grid::Zero(0)];
        let mut units: Vec<((usize, usize), usize, usize, Grid, Grid)> = Vec::new();
        for ch_row in 0..nchunk {
            for ch_col in 0..nchunk {
                // lane-av1-ibcvtx: the chunk's chroma units inherit the CHROMA tx
                // type of the co-located luma position (`av1_get_tx_type`,
                // blockd.h:1278: a non-Y plane of an inter-classified block -- and
                // intrabc is one -- reads `tx_type_map`, i.e. the coded type of the
                // luma unit covering it). Inside a mu chunk that is the chunk's
                // FIRST luma unit in raster order (the chunk origin is always the
                // first leaf's own origin). Captured right after that unit's read;
                // the single-TU arm below does the same for a whole-block unit.
                let mut first_leaf_tx: Option<TxType> = None;
                for &(tu_off_y, tu_off_x, tu_tx) in &chunk_tus[ch_row * nchunk + ch_col] {
                    let tu_mi = (mi_r + tu_off_y / MI, mi_c + tu_off_x / MI);
                    let tu_px = px + tu_off_x;
                    let tu_py = py + tu_off_y;
                    // Same libaom clip as `decode_rect_split`'s loop above; a
                    // square block only hangs off the frame when the frame's mi
                    // dimensions are odd at the block's own level.
                    if tu_px >= y.true_width || tu_py >= y.true_height {
                        continue;
                    }
                    let tu_around = neighbours.around_mi(tu_mi, tu_tx)[0];
                    // A transform unit inside a block is NOT a block at that
                    // position (lane-palette2 r12): `Reach::of` granted the
                    // top-right 8x8 unit of a 16x16 D203 block bottom-left
                    // pixels libaom refuses outright (`col_off > 0`), which is
                    // the diagonal wedge seed 46 reconstructed wrong.
                    let tu_reach = tu_reach(
                        side, side, tu_off_x, tu_off_y, tu_tx, reach, px, py, y.width, y.height,
                        fctx,
                    );
                    // This transform unit's own bsize (`tu_tx`) is smaller
                    // than the block it sits in (`side`), so `txb_skip_ctx` is
                    // the neighbour-magnitude table, not the lone-TU 0 (spec
                    // `get_txb_ctx_general`'s `plane_bsize != tx_size` branch).
                    let tu_skip_ctx = neighbours.luma_skip_ctx(tu_mi, tu_tx / MI);
                    // An intrabc unit's CDF set and scan follow the unit's OWN
                    // resolved size (`get_txsize_entropy_ctx` /
                    // `av1_get_scan` per `av1_read_coeffs_txb` call); the
                    // uniform grid's precomputed pair already matches because
                    // `tu_tx == logical_tx` there.
                    let (tu_set, tu_scan) = if intrabc_dv.is_some() {
                        (
                            inter_txbset_for(tu_tx, reduced_tx_set),
                            scan_for(
                                if tu_tx == 64 { 32 } else { tu_tx },
                                scan32,
                                scan16,
                                scan8,
                                scan4,
                            ),
                        )
                    } else {
                        (luma_set, luma_scan)
                    };
                    if let Some(buf) = &palette_y_buf {
                        hit!(PALETTE_SPLIT_TX_HITS);
                        let mut window = vec![0u16; tu_tx * tu_tx];
                        for row in 0..tu_tx {
                            let src = (tu_off_y + row) * side + tu_off_x;
                            window[row * tu_tx..][..tu_tx].copy_from_slice(&buf[src..][..tu_tx]);
                        }
                        set_palette_pred(window, fctx);
                    }
                    let tu_grid = read_plane(
                        dec,
                        cdfs,
                        tu_set,
                        tu_scan,
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
                        tu_tx,
                        // The unit's own COEFFICIENT grid, half its side at
                        // TX_64X64 (a 64-point transform codes nothing past 32);
                        // identical to `tu_tx` at every smaller size
                        // (lane-sb128b r1).
                        if tu_tx == 64 { 32 } else { tu_tx },
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
                    neighbours.record_mi_luma(tu_mi, tu_tx, &tu_grid);
                    if first_leaf_tx.is_none() {
                        first_leaf_tx = Some(fctx.luma_tx_type.with(std::cell::Cell::get));
                    }
                }
                // CfL is only allowed up to 32x32, i.e. only at `nchunk == 1`, where
                // this still runs after every luma unit of the block.
                let ac = alpha.map(|_| cfl_src(px, py, side));
                // lane-av1-ibcvtx: an intrabc block's chroma rides the luma's coded
                // tx type (see `first_leaf_tx` above) -- the same arm the single-TU
                // branch sets before its own chroma reads.
                if intrabc_dv.is_some()
                    && let Some(t) = first_leaf_tx
                {
                    fctx.intrabc_chroma_tx.with(|c| c.set(Some(t)));
                }
                // The block's uv plane block is `chroma_side` across but its uv
                // transform is `chroma_tx` (`av1_get_max_uv_txsize` =
                // `av1_get_adjusted_tx_size(max_txsize_rect_lookup[uv_plane_bsize])`):
                // identical at every size up to 64x64, but a 128x128 block's 64x64 uv
                // plane block adjusts TX_64X64 down to TX_32X32, so each plane is
                // FOUR units (lane-sb128b r2) -- one per mu chunk.
                // libaom `decode_token_recon_block` codes a mu chunk PLANE-MAJOR
                // (`for (plane..) for (blk_row..) for (blk_col..)`): every U unit of
                // the chunk, then every V unit. At one unit per plane per chunk --
                // every non-lossless case this decoder reaches -- that is the same
                // symbol order as the interleaved pair this loop used to read; a
                // lossless frame's 4x4 chroma units are the case where it differs
                // (lane-lossless2).
                let mut u_pass: Vec<Grid> = Vec::new();
                for plane_pass in 1..=2usize {
                    let mut pass_idx = 0usize;
                    for cu_row in ch_row * cpc_rows..(ch_row + 1) * cpc_rows {
                        for cu_col in ch_col * cpc_cols..(ch_col + 1) * cpc_cols {
                            // libaom `decode_reconstruct_tx`'s frame-edge clip
                            // (decodeframe.c:299, `blk_row >= max_blocks_high || blk_col
                            // >= max_blocks_wide` returns before ANY symbol is read): a
                            // transform unit whose 4x4 origin sits outside the frame's mi
                            // grid is not coded at all. The luma loop above carries the
                            // same clip; its absence here read `txb_skip` (+coeffs) for
                            // chroma units libaom never wrote -- the lossless 64x64 block
                            // at mi(48,0) of a 320x240 frame overhangs the bottom edge by
                            // 4 mi, so its 32x32 chroma codes SIX rows of 4x4 units, not
                            // eight (lane-av1-loss64; 48 chroma units per plane in
                            // aomdec's EC_COEFF census vs the 64 this loop read).
                            // `max_block_wide/high` (av1_common_int.h:1565): the plane
                            // block's pixel span (4:2:0 halves the luma span, clamped to
                            // 4 by `set_plane_n4`) plus the negative overhang
                            // (`mb_to_*_edge` = `2 * overhang_mi` chroma px), in 4x4
                            // units.
                            let span_mi = side / MI;
                            let over_w_mi = (2 * y.true_width.div_ceil(8)) as i32
                                - mi_c as i32
                                - span_mi as i32;
                            let over_h_mi = (2 * y.true_height.div_ceil(8)) as i32
                                - mi_r as i32
                                - span_mi as i32;
                            // `max_block_wide/high` in chroma pixels: the plane block's
                            // own span (`side` at 4:4:4, `side / 2` at 4:2:0 -- the old
                            // hardcoded `span_mi * 2` clipped three quarters of a 4:4:4
                            // block's coded units and desynced the tile) plus the
                            // negative overhang (`mb_to_*_edge >> (3 + ss)`), whose
                            // chroma-px scale is `MI >> ss` per mi.
                            let plane_px = (((span_mi * MI) >> ss_x(fctx)).max(4)) as i32;
                            let over_w_px = over_w_mi.min(0) * (MI >> ss_x(fctx)) as i32;
                            let over_h_px = over_h_mi.min(0) * (MI >> ss_y(fctx)) as i32;
                            let max_units_w = ((plane_px + over_w_px) >> 2).max(0) as usize;
                            // lane-av1-422f: the shared halved formula in the `else` counts
                            // the plane span in 4-px units through the 2-px-per-mi
                            // subsampled height, which a 4:2:2 plane does not have (ss_y
                            // 0) -- its span is the full luma mi count, so a keyed
                            // formula clips the same units libaom's `max_block_high`
                            // does. 4:2:0 and 4:4:4 keep the proven shared rows (at 4:2:0
                            // `MI >> ss_y` is 2, the same `2 * over_h_mi.min(0)` the
                            // pre-ss form spelled).
                            let max_units_h = if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                                (span_mi as i32 + over_h_mi.min(0)).max(0) as usize
                            } else {
                                ((plane_px + over_h_px) >> 2).max(0) as usize
                            };
                            let (unit_w, unit_h) = (chroma_tx / 4, chroma_tx_h / 4);
                            if cu_col * unit_w >= max_units_w || cu_row * unit_h >= max_units_h {
                                hit!(CHROMA_EDGE_TU_CLIP_HITS);
                                continue;
                            }
                            // The chroma context arrays are indexed in LUMA mi, so this
                            // unit's anchor is its own luma quadrant (lane-av1-422b: the
                            // quadrant spans `chroma_tx << ss_x` luma px wide by
                            // `chroma_tx_h << ss_y` tall -- at 4:2:2 below side 64 the
                            // one unit's quadrant is the whole block).
                            let cu_mi = (
                                mi_r + cu_row * (luma_span_y / MI),
                                mi_c + cu_col * (luma_span_x / MI),
                            );
                            let cu_around = if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                                // lane-av1-422f: chroma-only loop; per-chroma-cell above
                                // votes (see [`Neighbours::around_mi_422_chroma`]).
                                neighbours.around_mi_422_chroma(cu_mi, luma_span_x, luma_span_y)
                            } else {
                                neighbours.around_mi_rect(cu_mi, luma_span_x, luma_span_y)
                            };
                            let cu_reach = if cn_rows == 1 && cn_cols == 1 {
                                reach
                            } else {
                                tu_reach_rect(
                                    side,
                                    side,
                                    cu_col * luma_span_x,
                                    cu_row * luma_span_y,
                                    luma_span_x,
                                    luma_span_y,
                                    reach,
                                    px,
                                    py,
                                    y.width,
                                    y.height,
                                    fctx,
                                )
                            };
                            let (cu_x, cu_y) =
                                (cpx + cu_col * chroma_tx, cpy + cu_row * chroma_tx_h);
                            if let Some((ub, vb)) = &palette_uv_bufs {
                                let pbuf = if plane_pass == 1 { ub } else { vb };
                                set_palette_pred(
                                    palette_window(
                                        pbuf,
                                        chroma_side,
                                        cu_col * chroma_tx,
                                        cu_row * chroma_tx_h,
                                        chroma_tx,
                                        chroma_tx_h,
                                    ),
                                    fctx,
                                );
                            }
                            let cu_grid = if chroma_tx == chroma_tx_h {
                                read_plane(
                                    dec,
                                    cdfs,
                                    chroma_set,
                                    scans.1,
                                    plane_pass,
                                    cu_around[plane_pass],
                                    mode,
                                    uv_predict_mode,
                                    angle_delta_uv,
                                    cu_reach,
                                    y,
                                    u,
                                    v,
                                    cu_x,
                                    cu_y,
                                    chroma_tx,
                                    chroma_tx,
                                    base_q_idx,
                                    alpha.zip(ac).map(|((au, av), ac)| {
                                        (if plane_pass == 1 { au } else { av }, ac)
                                    }),
                                    None,
                                    chroma_ctx_offset,
                                    smooth_neighbor_uv,
                                    fctx,
                                )?
                            } else {
                                // lane-av1-422f: the whole plane block as one rect unit;
                                // `chroma_ctx_offset` is `None` there (the unit covers
                                // the plane block, so the offset-7 skip rows are right).
                                read_rect_chroma_unit(
                                    dec,
                                    cdfs,
                                    chroma_set,
                                    scans.1,
                                    chroma_tx,
                                    chroma_tx_h,
                                    plane_pass,
                                    cu_around[plane_pass],
                                    uv_predict_mode,
                                    angle_delta_uv,
                                    cu_reach,
                                    y,
                                    u,
                                    v,
                                    cu_x,
                                    cu_y,
                                    base_q_idx,
                                    None,
                                    alpha.zip(ac).map(|((au, av), ac)| {
                                        (if plane_pass == 1 { au } else { av }, ac)
                                    }),
                                    smooth_neighbor_uv,
                                    fctx,
                                )?
                            };
                            if cn_rows * cn_cols > 1 {
                                hit!(CHROMA_SPLIT_TX_HITS);
                                if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                                    hit!(CHROMA422_SQUARE_HITS);
                                }
                                // Immediately, so the NEXT unit of this same block reads
                                // this one's coefficient context (libaom's chroma
                                // `txb_skip_ctx` for the second quadrant is 11, i.e. base
                                // 1 from this left neighbour -- measured against aomdec).
                                // The same writes are replayed after `record_split_luma`
                                // below, which would otherwise clobber them whole-block.
                                neighbours.record_mi_chroma(
                                    cu_mi,
                                    luma_span_x,
                                    luma_span_y,
                                    plane_pass,
                                    &cu_grid,
                                );
                            }
                            if plane_pass == 1 {
                                u_pass.push(cu_grid);
                            } else {
                                let u_grid =
                                    std::mem::replace(&mut u_pass[pass_idx], Grid::Zero(0));
                                if cn_rows * cn_cols > 1 {
                                    units.push((
                                        cu_mi,
                                        luma_span_x,
                                        luma_span_y,
                                        u_grid.clone(),
                                        cu_grid.clone(),
                                    ));
                                }
                                last_grids = [u_grid, cu_grid];
                            }
                            pass_idx += 1;
                        }
                    }
                }
            }
            if intrabc_dv.is_some() {
                fctx.intrabc_chroma_tx.with(|c| c.set(None));
            }
        }
        // Mode/side bookkeeping plus, at `cn == 1`, the whole-plane chroma
        // context write; at `cn > 1` the per-unit writes above are what the
        // next block reads, so they run after this (last writer wins).
        neighbours.record_split_luma(
            at,
            side,
            mode,
            uv_predict_mode,
            [&last_grids[0], &last_grids[1]],
        );
        for (cu_mi, sw, sh, u_grid, v_grid) in &units {
            neighbours.record_mi_chroma(*cu_mi, *sw, *sh, 1, u_grid);
            neighbours.record_mi_chroma(*cu_mi, *sw, *sh, 2, v_grid);
        }
    }
    // Unconditional, `size == 0` for a non-palette block: clears any stale
    // neighbour palette state the same way [`Neighbours::record`] always
    // overwrites `above_mode`/`left_mode` regardless of this block's own mode
    // (the split-luma-TU branch above always refuses before reaching here
    // when `palette_y` is `Some`, so it is never live for that branch).
    let (py_size, py_colors) = palette_y
        .as_ref()
        .map_or((0, [0u16; 8]), |p| (p.size, p.colors));
    neighbours.record_palette_y(at, side, py_size, py_colors);
    let (puv_size, puv_colors) = palette_uv
        .as_ref()
        .map_or((0, [0u16; 8]), |p| (p.size, p.u_colors));
    neighbours.record_palette_uv(at, side, puv_size, puv_colors);
    neighbours.fill_skip_grid((r * (SUB / MI), c * (SUB / MI)), side / MI, skip);
    if intrabc_leaves.is_some() {
        // lane-av1422a2a4: the var-tx tree already published the
        // TXFM_CONTEXT bands per leaf ([`read_var_tx_size`] →
        // [`txfm_partition_update_rect`]); [`Self::fill_lf_grid_rect`]'s
        // whole-footprint publish would overwrite them with the block's own
        // `logical_tx` -- the same class [`Self::fill_lf_grid_rect_after_vartx`]
        // exists for (lane-av1oddheightfork3). Only the deblock grid is
        // still wanted here, so publish nothing.
        neighbours.fill_lf_grid_rect_after_vartx(
            (r * (SUB / MI), c * (SUB / MI)),
            side / MI,
            side / MI,
            logical_tx as u8,
            logical_tx as u8,
            0,
            fctx,
        );
    } else {
        neighbours.fill_lf_grid(
            (r * (SUB / MI), c * (SUB / MI)),
            side / MI,
            logical_tx as u8,
            0,
            fctx,
        );
    }
    // lane-t900 r32 / lane-av1txbands: libaom runs `set_txfm_ctxs(mbmi->tx_size,
    // n4_w, n4_h, skip_txfm && is_inter_block(mbmi), xd)` at the end of EVERY
    // `decode_block`. An intra-only frame's blocks also publish their TRANSFORM
    // size from [`Neighbours::fill_lf_grid_rect`], but this grid cannot express
    // libaom's `skip && is_inter` term -- the one thing that makes a SKIPPED
    // intrabc block publish its BLOCK size instead, intrabc being the only
    // "inter" block an intra-only frame codes (`blockd.h:373`). So this call
    // stays the authority wherever one can appear; with `allow_intrabc` unset
    // it writes exactly what the grid already did.
    //
    // lane-av1422a2a4: `parse_decode_block`'s tx branch (decodeframe.c:1227-1236)
    // is an EITHER/OR. When the var-tx branch ran, `read_tx_size_vartx` already
    // published per-leaf band sizes and libaom NEVER calls `set_txfm_ctxs` --
    // the blanket write below is the `else` branch's publish only. Writing it
    // after a var-tx tree clobbers the per-leaf band values with
    // `leaves[0]`'s transform: measured on the pinned A4 witness (256x224 4:2:2,
    // cq 30), the 32x32 intrabc block at mi(32,8) resolved a mixed tree
    // (8x8 leaves at (0,0)+(0,4), 16x16 leaves at (4,0)+(4,4)), leaving
    // `left_txfm[32..35] == 16`; the blanket write reset those cells to 8, so
    // the NEXT tree's sub-read at (32,16) took `txfm_partition_ctx` 10 where
    // libaom takes 9 (oracle operands: above=16, left=16) -- that frame's first
    // divergent read, surfacing ~30 partition symbols later as the
    // "block size 4x8 ... invalid" refusal (an E6/E7-class desync symptom, not
    // an invalid shape). Gate the blanket write on the tree NOT having run.
    if allow_intrabc && !(intrabc_leaves.is_some() && intrabc_leaves.as_ref().unwrap().len() > 1) {
        set_txfm_ctxs(
            neighbours,
            (mi_r, mi_c),
            logical_tx,
            side / MI,
            side / MI,
            skip && intrabc_dv.is_some(),
        );
    }
    record_intrabc_mi(mi_r, mi_c, side / MI, intrabc_dv, neighbours, fctx);
    Ok(())
}

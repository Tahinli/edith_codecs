//! Inter block decoder: `decode_inter_block`, the per-block inter path. Moved out of `decode/mod.rs` (av1split D8b); its intra else-arm is in `inter_intra_arm.rs` (D8a). The items are unchanged apart from visibility.

use super::*;

// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_inter_block(
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
    refpix: &RefPix<'_>,
    // lane-av1refs: every non-`LAST_FRAME` reference this frame header's own
    // `ref_frame_idx` names a live DPB slot for, indexed `[ref_frame]`
    // (`LAST2_FRAME`=2 .. `ALTREF_FRAME`=7; index 0/1 unused, `LAST_FRAME`
    // stays on `ref_y`/`u`/`v` above) -- `None` at a live index means the
    // slot's still empty (an error-resilient stream, or too early in the
    // GOP), matching `GOLDEN_FRAME`'s existing empty-slot refusal below.
    sign_bias_table: &SignBiasTable,
    // lane-gm r2: this frame header's own `global_motion` table (spec
    // 5.9.24), indexed `[ref_frame - LAST_FRAME]` same as `sign_bias_table`
    // -- feeds `warp::gm_get_motion_vector` for `GLOBALMV`/`GLOBAL_GLOBALMV`
    // assignment and the mv-stack's neighbour substitution.
    global_motion: &[ec_av1_syntax::WarpParams; 7],
    base_q_idx: u8,
    luma_set_intra: TxbSet,
    luma_set_inter: TxbSet,
    chroma_set: TxbSet,
    luma_tx: usize,
    chroma_tx: usize,
    scan_luma: &[u16],
    scan_chroma: &[u16],
    // lane-inter4 r3: superseded by `size_group_wh(write_w, write_h)` at the
    // one site that read it (`y_mode`) -- a rect strip's group comes from its
    // true footprint, not the caller's square `side`. Kept in the signature
    // rather than editing twenty call sites.
    _size_group: usize,
    allow_high_precision_mv: bool,
    force_integer_mv: bool,
    interp_fixed: Option<mc::InterpFilterKind>,
    enable_dual_filter: bool,
    tpl_frame: Option<&TplFrameArgs>,
    // lane-av1comp: this frame header's own `reference_select` bit -- `false`
    // means every block is `SINGLE_REFERENCE` by construction and `comp_mode`
    // is never read (spec 5.11.25); `true` reads `comp_mode` per block and
    // refuses (rather than silently misdecoding) the ones that pick
    // `COMPOUND_REFERENCE`, since two-reference motion compensation is not
    // wired yet.
    reference_select: bool,
    // lane-av1comp: `seq_params`' own masked-compound/distance-weighted-
    // compound enable bits, gating `comp_group_idx`/`compound_idx`
    // (spec 5.11.25) -- always present (unlike `tpl_frame`), since the
    // compound blend weights need this frame's own order hint regardless
    // of `use_ref_frame_mvs`.
    enable_masked_compound: bool,
    // lane-sb128 r4: this sequence header's own `enable_interintra_compound`
    // bit (spec 5.11.24's `interintra` read, libaom `decodemv.c`
    // read_inter_block_mode_info ~1490-1510) -- read right after `assign_mv`
    // and before `read_motion_mode` on the single-ref path below, gated by
    // `is_interintra_allowed` (single-ref, `BLOCK_8X8..=BLOCK_32X32`, and
    // every mode this single-ref branch can produce is already in
    // `SINGLE_INTER_MODE_START..END`); `interintra == 1` is a named refusal
    // (inter+intra blended prediction unimplemented) rather than a
    // mis-decode.
    enable_interintra_compound: bool,
    enable_jnt_comp: bool,
    order_hint_bits: u32,
    order_hint: u32,
    ref_order_hints: [u32; 7],
    // lane-av1comp round 14: this frame header's own `skip_mode_present` bit
    // and `skip_mode_frame` reference pair (spec 5.9.22, `frame.rs`'s
    // `read_skip_mode_params`, already 1-based `MV_REFERENCE_FRAME`) --
    // `skip_mode` (spec 5.11.23 `read_skip_mode`) is read *before* `skip`
    // when this is `true` (every block this decoder reaches is >= 8x8, so
    // `is_comp_ref_allowed` never gates it further); a `true` result forces
    // `skip_txfm`, `is_inter`, `NEAREST_NEARESTMV` of this pair with no mode/
    // DRL symbol read, and a plain average blend with no `comp_group_idx`/
    // `compound_idx` read (libaom `decodemv.c` 1604-1619, 1382-1384, 1509).
    skip_mode_present: bool,
    skip_mode_frame: [u8; 2],
    // lane-motionmode round 1: this frame header's own `is_motion_mode_switchable`/
    // `allow_warped_motion` bits (spec 5.11.24's `read_motion_mode`).
    switchable_motion_mode: bool,
    allow_warped_motion: bool,
    // lane-warp r5: set by the HORZ_B arm -- the 32x16 top strip is decoded
    // as a square 32x32 block, which over-reads a residual unless coded
    // `skip`; a non-skip strip must refuse cleanly instead of desyncing.
    reject_residual: bool,
    // lane-partitions r1: the strip's own true width/height in pixels --
    // `side` keeps governing every syntax/CDF/prediction-buffer decision
    // (matching `HORZ_B`'s already-accepted square-context corner-cut), but
    // the final pixel write and this block's own neighbour-context stamp
    // (`record*`/`fill_*_grid`) use `write_w`/`write_h` instead, so a
    // PARTITION_HORZ/VERT strip only claims its own true footprint. `side`
    // for every existing (square) caller.
    write_w: usize,
    write_h: usize,
    // lane-screen r2: this frame header's own `allow_screen_content_tools`
    // bit, threaded to the intra-sub-block branch below -- libaom's
    // `read_intra_block_mode_info` (decodemv.c:1065-1107, the inter-frame
    // counterpart of `read_intra_frame_mode_info`) reads `y_mode_cdf`
    // (never `kf_y_mode_cdf`) and has NO `use_intrabc` call at all (that
    // symbol is intra-frame-only, spec `av1_read_mode_info` dispatches on
    // `frame_is_intra_only`), but it still calls `read_palette_mode_info`
    // under the same `av1_allow_palette` gate as the key-frame path.
    allow_screen_content_tools: bool,
    // lane-superres r9: this frame's own coded luma width (spec 7.11.3.3) --
    // compared against each reference's stored picture width to decide
    // whether a scaled-MC path (`mc::predict_scaled`) or the ordinary
    // stride-1 one (`mc::predict_with_filters`) applies per reference, and to
    // refuse (rather than mis-decode) the warp/OBMC/interintra/compound
    // combinations that don't implement scaled MC yet.
    frame_size: (usize, usize),
    fctx: &crate::decode::FrameCtx,
) -> Result<()> {
    // Round 27: the per-read trace is the only instrument that reaches the inter
    // path (the coefficient rungs stop well before the frame-3 divergence),
    // so publish the phase and the block's mi for every read it makes.
    crate::msac::SymbolDecoder::set_symr_phase("inter");
    crate::msac::SymbolDecoder::set_symr_mi(at.0 as i64, at.1 as i64);
    // lane-inter4 r1: `at` is in MI units (4 px) so a 32-level 1:4 strip can
    // name an 8-px offset; `(r, c)` stays the enclosing 16-px cell for the
    // few 16-px-granular uses below.
    let (r, c) = (at.0 / (SUB / MI), at.1 / (SUB / MI));
    // lane-inter8 r2: the inter side bands are mi-granular; this block's own
    // above/left neighbour is the mi cell at its top-left corner (libaom
    // `above_mbmi` = mi(mi_row - 1, mi_col)), so a SUB-aligned block reads
    // exactly what it read before.
    let (rmi, cmi) = at;
    let (px, py) = (at.1 * MI, at.0 * MI);
    census_inter_block_shape(side, write_w, write_h);
    // lane-inter16ab r2: a 16x4 / 4x16 inter strip's chroma is the PAIR's
    // (8x4 / 4x8 at the even strip's origin) and is coded only by the odd
    // strip (`is_chroma_reference`); every other block keeps its own.
    let strip_chroma = fctx.inter_strip_chroma.with(std::cell::Cell::take);
    fctx.inter_last_mc.with(|c| c.set(None));
    let has_chroma = strip_chroma.is_none_or(|s| s.has_chroma);
    let (cpx, cpy) = match strip_chroma {
        Some(s) if s.has_chroma => (
            (s.pair_mi.1 * MI) >> ss_x(fctx),
            (s.pair_mi.0 * MI) >> ss_y(fctx),
        ),
        _ => (px >> ss_x(fctx), py >> ss_y(fctx)),
    };
    // lane-av1-422j: the chroma prediction/residual buffers are
    // `chroma_side`-square and every consumer indexes them
    // `row * chroma_side + col` over the WRITE rect (`write_chroma_w` x
    // `write_chroma_h`). At ss (1,0) that rect is `side/2 x side` (an 8x16
    // strip's 4x16 chroma), so a `side >> ss_x` square is shorter than
    // `row * chroma_side + write_chroma_w` on rows `>= side/2` -- frame 1's
    // skipped VERT_B strip read `prediction[64..68]` out of the 64-sample
    // buffer (reconstruct_mc_rect panic, decode.rs:29094). The enclosing
    // square of the chroma rect is the max: unchanged at 4:2:0 (side/2) and
    // 4:4:4 (side), so only 4:2:2 changes shape.
    let chroma_side = (side >> ss_x(fctx)).max(side >> ss_y(fctx));
    let (write_chroma_w, write_chroma_h) = match strip_chroma {
        Some(s) if s.has_chroma => {
            // 4:2:0: the PAIR's 8x4 / 4x8. 4:4:4: the strip's own 16x4 /
            // 4x16 (lane-av1-444; no pair at ss 0/0).
            //
            // lane-av1422lpf site 4: the VERT arm's third shape. A
            // `PARTITION_VERT_4` strip is 4x16 luma and pairs on COLUMNS
            // while `ss_x == 1` (`is_chroma_reference` on `mi_col` parity,
            // av1_common_int.h:1454), so the pair is 8x16 luma -- and 4:2:2
            // subsamples X ONLY (`subsampling_x = 1, subsampling_y = 0`,
            // spec 5.5.2's `color_config`), which leaves the pair's chroma
            // plane block at 8>>1 x 16>>0 = **4x16**, twice as tall as the
            // 4:2:0 pair's 4x8. The old `else` handed 4:2:2 the 4:2:0 shape,
            // halving `write_chroma_h` for the WHOLE of `decode_inter_block`
            // at ss (1,0) -- `chroma_stride`/`chroma_buf_h` at 40017 and so
            // every prediction buffer, every grid and every mu-chunk replay
            // below. Measured on `W_intrabc.obu` frame 1: a 4x16 vert strip
            // got `chroma_stride = 4`, `chroma_buf_h = 8`, a 32-sample grid,
            // and the lossless TX_4x4 replay's last unit row read index 64 of
            // it (`range end index 36 out of range for slice of length 32`
            // at the composed-grid read).
            //
            // The HORZ arm needs no third shape at 4:2:2 and gets none: a
            // HORZ_4 strip pairs on ROWS only while `ss_y == 1`, and 4:2:2
            // has `ss_y == 0`, so each 16x4 strip is its own chroma reference
            // and its own 8x4 chroma block -- the number `(8, 4)` already
            // returns. At 4:2:0 (`ss_x == ss_y == 1`) the pair halves on both
            // axes and at 4:4:4 there is no pair at all, so both of those
            // arms are untouched by this line.
            if ss_x(fctx) == 0 && ss_y(fctx) == 0 {
                if s.horz { (16, 4) } else { (4, 16) }
            } else if s.horz {
                (8, 4)
            } else if ss_y(fctx) == 0 {
                (4, 16)
            } else {
                (4, 8)
            }
        }
        _ => (write_w >> ss_x(fctx), write_h >> ss_y(fctx)),
    };
    // lane-av1-422blockhalf: the chroma PREDICTION buffers take the PLANE
    // BLOCK's own per-axis shape at 4:2:2 -- (side/2, side) -- instead of the
    // enclosing square `chroma_side` the pre-port model indexed, and the
    // masked-compound blend reads its luma mask through the plane's real
    // subsampling shifts (aom_dsp/blend-a64-mask.c's four branches; the
    // stop-note this lane owns). Every consumer below strides with
    // `chroma_stride`. At 4:2:0 and 4:4:4 both values reduce to the square
    // `chroma_side` verbatim, so only 4:2:2 changes shape.
    let chroma_422 = ss_x(fctx) == 1 && ss_y(fctx) == 0;
    let (chroma_stride, chroma_buf_h) = if chroma_422 {
        (write_chroma_w, write_chroma_h)
    } else {
        (chroma_side, chroma_side)
    };
    let (chroma_set, chroma_tx, scan_chroma) =
        if ss_x(fctx) == 0 && ss_y(fctx) == 0 && strip_chroma.is_none() {
            let tx = if side >= 64 { 32 } else { side };
            let set = match tx {
                32 => TxbSet::Chroma32,
                16 => TxbSet::Chroma16,
                8 => TxbSet::Chroma8,
                _ => TxbSet::Chroma4,
            };
            (set, tx, scan_luma)
        } else {
            (chroma_set, chroma_tx, scan_chroma)
        };
    // lane-midcut r3 (same rule as lane-t900 r5's `warp_plane_allowed`, one
    // copy kept at the merge): `av1_init_warp_params` (libaom reconinter.c)
    // bails per PLANE at `block_width < 8 || block_height < 8` -- on the
    // block's own dims, never on the square prediction buffer this reader
    // allocates.
    if crate::envflags::env_flag!("EC_MC_TRACE") {
        eprintln!("EC_IB px={px} py={py} side={side} w={write_w} h={write_h}");
    }

    if crate::envflags::env_flag!("EC_AV1_TELL") {
        eprintln!(
            "TELL mi_row={} mi_col={} label=block_entry side={side} tell={} range={}",
            rmi,
            cmi,
            dec.debug_bitpos(),
            dec.debug_state().0
        );
    }
    let skip_mode_ctx =
        usize::from(neighbours.above_skip_mode[cmi]) + usize::from(neighbours.left_skip_mode[rmi]);
    // lane-comppin r3: skip_mode desync isolation -- dump ctx + the pre-read
    // MSAC tell so it can be diffed against an equivalent hook in libaom's
    // own `read_skip_mode`/`av1_get_skip_mode_context`.
    if crate::envflags::env_flag!("EC_AV1_SKIPMODE_DUMP") {
        eprintln!(
            "EC_SKIPMODE r={r} c={c} ctx={skip_mode_ctx} skip_mode_present={skip_mode_present} tell_before={}",
            dec.debug_bitpos()
        );
    }
    // spec 5.11.5 `inter_frame_mode_info`: `inter_segment_id(1)` before
    // `skip_mode`/`skip`, `inter_segment_id(0)` after them.
    let (seg_mi_r, seg_mi_c) = (rmi, cmi);
    let (seg_w_mi, seg_h_mi) = (write_w / 4, write_h / 4);
    inter_segment_id(
        dec, cdfs, seg_mi_r, seg_mi_c, seg_w_mi, seg_h_mi, false, true, fctx,
    );
    // libaom `read_skip_mode` (decodemv.c): `if (!is_comp_ref_allowed(bsize))
    // return 0` -- a 16x4 / 4x16 strip (min(bw, bh) == 4, blockd.h:65) reads
    // no `skip_mode` symbol at all.
    let comp_allowed = write_w.min(write_h) >= 8;
    let skip_mode =
        skip_mode_present && comp_allowed && dec.symbol(&mut cdfs.skip_mode[skip_mode_ctx]) == 1;
    if crate::envflags::env_flag!("EC_AV1_SKIPMODE_DUMP") {
        eprintln!(
            "EC_SKIPMODE r={r} c={c} result={skip_mode} tell_after={}",
            dec.debug_bitpos()
        );
    }
    if skip_mode {
        hit!(SKIP_MODE_HITS);
    }

    let skip_ctx = usize::from(neighbours.above_skip[cmi]) + usize::from(neighbours.left_skip[rmi]);
    let skip = skip_mode || dec.symbol(&mut cdfs.skip[skip_ctx]) == 1;
    inter_segment_id(
        dec, cdfs, seg_mi_r, seg_mi_c, seg_w_mi, seg_h_mi, skip, false, fctx,
    );
    maybe_read_cdef_idx(dec, rmi, cmi, (write_w / MI, write_h / MI), skip, fctx);
    // lane-inter4 r1: spec 5.11.6's `bSize == sbSize` -- a 64x32/64x16 strip
    // is NOT the whole superblock, so its `skip` never suppresses the read.
    // lane-sb128c r7: `sbSize` is 128 under `--sb-size=128`, so the whole-block
    // test is against the sequence's own superblock side, not a hardcoded 64.
    let sb_px = sb_mi_cur(fctx) as usize * 4;
    let whole_sb = side == sb_px && write_w == sb_px && write_h == sb_px;
    maybe_read_delta_q(dec, cdfs, rmi, cmi, whole_sb, skip, fctx);
    maybe_read_delta_lf(dec, cdfs, rmi, cmi, whole_sb, skip, fctx);
    let (has_above, has_left) = (rmi > neighbours.tile_row0_mi, cmi > neighbours.tile_col0_mi);
    let (above_inter, left_inter) = (neighbours.above_inter[cmi], neighbours.left_inter[rmi]);
    let ii_ctx = intra_inter_ctx(has_above, has_left, above_inter, left_inter);
    let is_inter = skip_mode || dec.symbol(&mut cdfs.intra_inter[ii_ctx]) == 1;
    // lane-sb128c r1: this round wires the INTER half of a 128x128 block (four
    // TX_64X64 luma units and four TX_32X32 chroma units interleaved per mu
    // chunk). An INTRA block at that size inside an inter frame takes the
    // intra tail below, which still codes one whole-block chroma unit and one
    // `read_tx_size` at 128 -- refuse by name instead of mis-decoding it.
    if crate::envflags::env_flag!("EC_TRACE_MODE") {
        eprintln!(
            "EC_MODE mi_row={} mi_col={} rng={} is_inter={} skip={} side={side}",
            rmi,
            cmi,
            dec.debug_state().0,
            u8::from(is_inter),
            u8::from(skip)
        );
    }
    // lane-warp r5: see `reject_residual` -- a square-block decode of a
    // HORZ_B strip is symbol-exact only for `skip` (no residual, no
    // motion_mode/warp symbol). lane-intrarect r1 MOVED this gate from just
    // after the `skip` read to here, past `is_inter` (class
    // [[refusal-short-circuits-its-own-code]]: at the old place it refused
    // 24/24 attempts of this lane's own gate before the intra arm below
    // could run). An INTRA strip with its true `write_w`x`write_h` footprint
    // now codes its residual rectangularly ([`decode_intra_rect_in_inter`]);
    // an INTER one, and a strip decoded with square dimensions anyway, still
    // cannot.
    // merge b51719f+lane-intrarect: main's `rect_inter_residual_supported`
    // term stays -- lane-inter4's 16x8/8x16 inter leaves DO code a
    // rectangular residual, and the lane's condition (written before that
    // landed) refused every one of them.
    if reject_residual
        && !skip
        && !rect_inter_residual_supported(write_w, write_h)
        && (is_inter || (write_w == side && write_h == side))
    {
        if crate::envflags::env_flag!("EC_RECTRES") {
            eprintln!(
                "EC_RECTRES w={write_w} h={write_h} side={side} skip={} is_inter={} mi_row={rmi} mi_col={cmi}",
                u8::from(skip),
                u8::from(is_inter)
            );
        }
        return Err(unsupported(
            "a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding",
        ));
    }
    if crate::envflags::env_flag!("EC_AV1_TELL") {
        eprintln!(
            "TELL mi_row={} mi_col={} label=post_is_inter skip_mode={skip_mode} skip={skip} is_inter={is_inter} tell={} range={}",
            rmi,
            cmi,
            dec.debug_bitpos(),
            dec.debug_state().0
        );
    }

    let mode_for_tx;
    let uv_predict_mode;
    let (luma_grid, u_grid, v_grid);
    // The rect_tu arms' chroma helper returns its TRUE per-unit coefficient
    // context stamps (see `read_inter_rect_chroma`); they are appended to
    // `mu_chroma_units` below so the block tail replays them after the
    // whole-block record. `mu_chroma`'s own square-mu derivation cannot
    // express a non-square strip's units.
    let mut rect_chroma_units: Vec<((usize, usize), usize, Grid, usize, usize)> = Vec::new();
    let block_filter;
    let ref_frame_for_lf;
    let globalmv_for_lf;
    // lane-av1comp: `Some((comp_group_idx, compound_idx))` only for a real
    // `COMPOUND_REFERENCE` block -- applied to `neighbours` *after* the
    // common `record_inter` call below (which would otherwise stamp the
    // single-ref ALTREF-special-case default over it).
    let mut compound_ctx: Option<(i8, u8, u8)> = None;
    // lane-txselect: `read_block_tx_size`'s leaf transform units (spec
    // 5.11.17), `Some` only when this block's var-tx tree resolved to more
    // than one luma transform -- read back by the residual loop, the
    // coefficient-context write-back and the loop-filter grid fill below.
    let vartx_leaves: Option<Vec<(usize, usize, usize, usize)>>;
    // lane-t900 r22: this block's own palette state, stamped into the
    // mi-granular neighbour bands at the tail below for EVERY block --
    // libaom's `MB_MODE_INFO.palette_size` is 0 on an inter block, and
    // `av1_get_palette_cache`/`av1_get_palette_mode_ctx` read those cells, so
    // leaving a previous palette block's colours standing here would feed the
    // next intra block a cache libaom never builds.
    let mut palette_record: (usize, [u16; 8], usize, [u16; 8]) = (0, [0u16; 8], 0, [0u16; 8]);
    let at_mi = at;
    // lane-vert46 r1: set when this block's chroma was coded as one 32x32 unit
    // per 64x64 "mu" chunk (any side above 64), so the end-of-block record
    // below must NOT re-stamp the chroma neighbour context from the assembled
    // whole-block grid.
    let mut mu_chroma = false;
    if is_inter {
        let above_nbr = has_above.then(|| NeighbourRef {
            is_inter: above_inter,
            ref0: if above_inter {
                neighbours.above_ref[cmi]
            } else {
                0
            },
            ref1: neighbours.above_ref1[cmi],
            uni: neighbours.above_ref1[cmi]
                .is_some_and(|r1| is_uni_comp_ref(neighbours.above_ref[cmi], r1)),
        });
        let left_nbr = has_left.then(|| NeighbourRef {
            is_inter: left_inter,
            ref0: if left_inter {
                neighbours.left_ref[rmi]
            } else {
                0
            },
            ref1: neighbours.left_ref1[rmi],
            uni: neighbours.left_ref1[rmi]
                .is_some_and(|r1| is_uni_comp_ref(neighbours.left_ref[rmi], r1)),
        });
        if crate::envflags::env_flag!("EC_AV1_COMPIDX_DUMP") {
            eprintln!(
                "EC_PRECOMP r={r} c={c} skip_mode={skip_mode} skip={skip} reference_select={reference_select} tell={}",
                dec.debug_bitpos()
            );
        }
        let is_compound = skip_mode
            || (reference_select
                    // `is_comp_ref_allowed` (blockd.h:65) gates the
                    // `comp_mode` read exactly as it gates `skip_mode`.
                    && comp_allowed
                    && read_comp_mode(dec, cdfs, above_nbr, left_nbr));
        if is_compound {
            let (ref0, ref1) = if skip_mode {
                (skip_mode_frame[0] as i8, skip_mode_frame[1] as i8)
            } else {
                read_compound_ref_frames(
                    dec,
                    cdfs,
                    above_nbr,
                    left_nbr,
                    neighbours.above_ref[cmi],
                    neighbours.left_ref[rmi],
                )
            };
            let (mi_row, mi_col) = (rmi, cmi);
            // lane-rect r2: true footprint, mirroring the single-ref path below.
            let bw4 = write_w / 4;
            let bh4 = write_h / 4;
            // spec 7.10.2.8, doubled per side (`CompoundTplArgs`): same
            // `use_ref_frame_mvs` gating as the single-ref `tpl` build
            // below, just one `get_relative_dist` per reference.
            let comp_tpl = tpl_frame.map(|t| crate::mvstack::CompoundTplArgs {
                field: t.field,
                cur_offset_0: crate::motion_field::get_relative_dist(
                    t.order_hint_bits,
                    t.order_hint,
                    t.ref_order_hints[(ref0 - LAST_FRAME) as usize],
                ),
                cur_offset_1: crate::motion_field::get_relative_dist(
                    t.order_hint_bits,
                    t.order_hint,
                    t.ref_order_hints[(ref1 - LAST_FRAME) as usize],
                ),
                allow_high_precision_mv,
                force_integer_mv,
            });
            let gm_table = build_gm_mv_table(
                global_motion,
                mi_row,
                mi_col,
                bw4,
                bh4,
                allow_high_precision_mv,
                force_integer_mv,
            );
            let comp_stack = crate::mvstack::find_mv_stack_compound(
                grid,
                mi_row,
                mi_col,
                bw4,
                bh4,
                (ref0, ref1),
                mi_cols as usize,
                mi_rows as usize,
                sign_bias_table,
                &gm_table,
                comp_tpl,
            );
            let compound_mode = if skip_mode {
                0 // NEAREST_NEARESTMV, forced -- no mode symbol read
            } else {
                read_inter_compound_mode(dec, cdfs, comp_stack.new_mv_ctx, comp_stack.ref_mv_ctx)
            };
            let (mv0, mv1) = assign_compound_mv(
                dec,
                cdfs,
                &comp_stack,
                compound_mode,
                allow_high_precision_mv,
                force_integer_mv,
                (
                    gm_table[(ref0 - LAST_FRAME) as usize],
                    gm_table[(ref1 - LAST_FRAME) as usize],
                ),
                fctx,
            );
            if crate::envflags::env_flag!("EC_TRACE_MODE") {
                eprintln!(
                    "EC_MODE_VAL mi_row={mi_row} mi_col={mi_col} mode={} ref0={ref0} ref1={ref1} mv0=({},{}) stack={} rng={}",
                    compound_mode + 17,
                    mv0.0,
                    mv0.1,
                    comp_stack.entries.len(),
                    dec.debug_state().0
                );
                for (i, e) in comp_stack.entries.iter().enumerate() {
                    eprintln!(
                        "EC_STACK mi_row={mi_row} mi_col={mi_col} i={i} this=({},{}) comp=({},{}) w={}",
                        e.mv0.0, e.mv0.1, e.mv1.0, e.mv1.1, e.weight
                    );
                }
            }
            let is_globalmv = compound_mode == 6; // GLOBAL_GLOBALMV
            // be non-TRANSLATION (IDENTITY counts) for the compound block's
            // interp-filter read to be suppressed.
            let gm_nontrans_models = is_globalmv
                && global_motion[(ref0 - LAST_FRAME) as usize].model
                    != ec_av1_syntax::WarpModel::Translation
                && global_motion[(ref1 - LAST_FRAME) as usize].model
                    != ec_av1_syntax::WarpModel::Translation;
            // lane-t900 r9: `is_nontrans_global_motion` bails at
            // `AOMMIN(mi_size_wide, mi_size_high) < 2` before it looks at any
            // model, so a 4-px-sided block still reads its interp filter.
            let gm_nontrans = gm_nontrans_models && bw4.min(bh4) >= 2;
            if gm_nontrans_models && !gm_nontrans {
                hit!(GM_NONTRANS_SMALL_SIDE_HITS);
            }
            // lane-defer8: both slots' pictures are resolved inside the
            // prediction closure; the parse only needs their SIZES.
            // lane-cwarp r1: `is_global_mv_block` (blockd.h:421-429) is
            // PER REFERENCE SLOT -- mode GLOBAL_GLOBALMV, block >= 8x8, and
            // THAT slot's own gm model > TRANSLATION. `bw4`/`bh4` come from
            // the write extent below, so recompute the size bound from
            // `side` here (the block's own size, what libaom's `bsize`
            // is).
            let is_global_mv0 = is_globalmv
                && side >= 8
                && global_motion[(ref0 - LAST_FRAME) as usize].model as u8 > 1;
            let is_global_mv1 = is_globalmv
                && side >= 8
                && global_motion[(ref1 - LAST_FRAME) as usize].model as u8 > 1;
            // lane-av1422warp r33 (reviewer P3): the top-half census is
            // placed HERE, after `is_global_mv0`/`is_global_mv1`, so it
            // counts the same blocks libaom's `is_global_mv_block` does --
            // mode GLOBAL_GLOBALMV AND block >= 8x8 AND that slot's own gm
            // model > TRANSLATION. An earlier placement tested the model term
            // alone, which counts blocks that are not global-mv blocks at
            // all, so its number was an upper bound dressed up as an
            // engagement figure.
            if mi_row < mi_rows as usize / 2 {
                if is_globalmv {
                    TOP_HALF_COMPOUND_HITS.with(|c| c.set(c.get() + 1));
                }
                if is_global_mv0 || is_global_mv1 {
                    TOP_HALF_WARP_HITS.with(|c| c.set(c.get() + 1));
                }
            }
            // libaom `av1_init_warp_params` + `allow_warp`'s
            // `global_warp_allowed` branch, run once per reference for a
            // compound block exactly as for a single-ref one (reconinter.c
            // `build_inter_predictors_8x8_and_bigger` loops over `ref` and
            // calls `av1_init_warp_params` inside the loop) -- WARPED_CAUSAL
            // never reaches here (spec 5.11.27 / libaom `read_motion_mode`:
            // `has_second_ref` returns SIMPLE_TRANSLATION), so the local-warp
            // arm of `allow_warp` is dead on this path and only the global
            // one applies. `force_integer_mv` short-circuits
            // `av1_init_warp_params` before `allow_warp`.
            let compound_warp = |ref_frame: i8, eligible: bool| {
                if eligible
                    && !force_integer_mv
                    && !global_motion[(ref_frame - LAST_FRAME) as usize].invalid
                {
                    crate::warp::global_warp_params(
                        global_motion[(ref_frame - LAST_FRAME) as usize].params,
                    )
                } else {
                    None
                }
            };
            // lane-t900 r28 (class branch-dropped-as-unreachable, swept out
            // of the scaled-ref lift): libaom `allow_warp`
            // (av1/common/reconinter.c:41) opens with `if (av1_is_scaled(sf))
            // return 0;` PER SLOT, so a compound tap whose own reference is
            // scaled (superres) is predicted translationally while its
            // sibling may still warp.
            let ref_unscaled = |rf: i8| {
                refpix
                    .dims(rf)
                    .is_some_and(|d| scale_of(d, frame_size).is_unscaled())
            };
            let warp0 = compound_warp(ref0, is_global_mv0).filter(|_| ref_unscaled(ref0));
            let warp1 = compound_warp(ref1, is_global_mv1).filter(|_| ref_unscaled(ref1));
            if warp0.is_some() || warp1.is_some() {
                hit!(COMPOUND_WARP_HITS);
                // lane-av112bitw: the 12-bit refusal that stood here is
                // LIFTED with the compound-warp witness: the rotating-
                // mandelbrot 12-bit recipe refused exactly here pre-lift and
                // now decodes byte-exact vs ffmpeg through the same bumped
                // `warp::warp_round_0` shifts
                // (`a_real_compound_global_warp_12bit_stream_decodes_pixel_exact`).
            }
            // spec `get_ref_filter_type`: matches when EITHER of the
            // neighbour's two references equals this block's own ref0 --
            // `above_ref1`/`left_ref1` is the neighbour's real second
            // reference (`record_compound_ctx`) when it was itself a
            // compound block; checking `above_ref`/`left_ref` alone drops a
            // real filter match down to the "no neighbour" sentinel
            // whenever the shared reference sits in the neighbour's SECOND
            // slot (lane-av1idx r2).
            let above_filter_ctx =
                if neighbours.above_ref[cmi] == ref0 || neighbours.above_ref1[cmi] == Some(ref0) {
                    neighbours.above_filter[cmi]
                } else {
                    [3, 3]
                };
            let left_filter_ctx =
                if neighbours.left_ref[rmi] == ref0 || neighbours.left_ref1[rmi] == Some(ref0) {
                    neighbours.left_filter[rmi]
                } else {
                    [3, 3]
                };
            globalmv_for_lf = is_globalmv;
            ref_frame_for_lf = ref0;
            mode_for_tx = 0;
            uv_predict_mode = DC_PRED;
            // lane-rect r2: `grid` (mvstack.rs's own MiGrid, distinct from
            // `neighbours`) must be stamped with the block's true footprint
            // too -- a square `bw4`x`bw4` stamp here claims a rect strip's
            // NEXT strip's own rows/cols before it decodes, corrupting
            // every later mvstack scan that reads this cell back.
            grid.fill_rect(
                mi_row,
                mi_col,
                bh4,
                bw4,
                MiInfo {
                    is_inter: true,
                    ref_frame: ref0,
                    ref_frame1: ref1,
                    mv1: mv16(mv1),
                    mv: mv16(mv0),
                    is_new_mv: matches!(compound_mode, 2 | 3 | 4 | 5 | 7),
                    size: bw4 as u8,
                    size_h: bh4 as u8,
                    // libaom `is_global_mv_block`: per-ref-slot, mode
                    // GLOBAL_GLOBALMV, block >= 8x8, and THAT slot's
                    // own gm model > TRANSLATION (IDENTITY excluded
                    // too -- the two-predicate trap, distinct from
                    // `gm_nontrans` above which allows IDENTITY).
                    is_global_mv0,
                    is_global_mv1,
                },
            );
            // spec 5.11.25's `comp_group_idx`/`compound_idx`: only read when
            // masked compound / distance-weighted compound are actually
            // enabled for this stream -- `is_any_masked_compound_used_here`
            // is `true` for every block size this decoder ever reaches
            // (`min(side,side) >= 8`, `COMPOUND_DIFFWTD` always eligible
            // there), so the gate reduces to the sequence header bit; kept
            // as an explicit call so a future smaller block size is not
            // silently wrong (libaom `reconinter.h::is_any_masked_compound_used`).
            let group_ctx = get_comp_group_idx_context(neighbours, (rmi, cmi), side);
            let comp_group_idx = if !skip_mode
                && enable_masked_compound
                && is_any_masked_compound_used_here(write_w, write_h)
            {
                crate::msac::SymbolDecoder::set_symr_cdf("comp_group_idx");
                dec.symbol(&mut cdfs.comp_group_idx[group_ctx])
            } else {
                0
            };
            if crate::envflags::env_flag!("EC_TRACE_MODE") {
                eprintln!(
                    "EC_CGI mi_row={rmi} mi_col={cmi} ctx={group_ctx} val={comp_group_idx} rng={}",
                    dec.debug_state().0
                );
            }
            // lane-maskcomp r2 / lane-wedge r3: `Some(mask_type)` for a
            // `COMPOUND_DIFFWTD` block, `Some(mask)` (wedge codebook lookup)
            // for a `COMPOUND_WEDGE` block -- exactly one of the two is set
            // when `comp_group_idx == 1`.
            let mut diffwtd_mask_type: Option<u8> = None;
            let mut wedge_mask: Option<&'static [u8]> = None;
            if comp_group_idx == 1 {
                // lane-maskcomp r1: `read_compound_type` (spec 5.11.25,
                // libaom `decodemv.c` 1634-1656). `is_interinter_compound_used
                // (COMPOUND_WEDGE, bsize)` (`av1_is_wedge_used`,
                // `av1_wedge_params_lookup`) is true for every square bsize
                // this leaf reaches (8x8/16x16/32x32 all have
                // `wedge_types > 0`), so `compound_type` always reads a real
                // symbol here -- no alphabet collapse to worry about at this
                // block-size family.
                // lane-mcomp64 r1: `av1_is_wedge_used` is false at 64x64
                // (`av1_wedge_params_lookup[BLOCK_64X64].wedge_types == 0`),
                // so libaom's `read_compound_type` (decodemv.c 1634-1656)
                // writes NO `compound_type` symbol there and INFERS
                // `COMPOUND_DIFFWTD` -- only the 1-bit `mask_type` literal
                // follows. `None` here is exactly that inferred arm.
                // lane-inter16ab r3: the row is this block's TRUE
                // `BLOCK_SIZES_ALL` index, not the enclosing square `side`'s
                // -- a 16x8 strip reads `compound_type_cdf[BLOCK_16X8]`.
                let wedge_bsize = bsize_all_index(write_w, write_h)
                    .filter(|_| wedge_used_bsize(write_w, write_h));
                let compound_type = match wedge_bsize {
                    Some(b) => dec.symbol(&mut cdfs.compound_type[b]),
                    None => 1,
                };
                if let Some(wedge_bsize) = wedge_bsize.filter(|_| compound_type == 0) {
                    // COMPOUND_WEDGE: lane-wedge r3, codebook checksum-
                    // verified vs independent C dump (wedge.rs).
                    let wedge_index = dec.symbol(&mut cdfs.wedge_idx[wedge_bsize]);
                    let wedge_sign = dec.literal(1);
                    hit!(WEDGE_HITS);
                    if let Some(k) = rect_wedge_slot(write_w, write_h) {
                        hit_do! {
                            RECT_WEDGE_HITS.with(|c| {
                                let mut a = c.get();
                                a[k] += 1;
                                c.set(a);
                            });
                        }
                    }
                    // lane-inter16ab r5: the codebook row is this block's TRUE
                    // footprint (`av1_wedge_params_lookup[bsize]`: hgtw for
                    // h>w, hltw for h<w, plus that bsize's own signflip row).
                    // Masks are padded to stride `max(bw,bh) == side`, so the
                    // square blend below indexes them unchanged and the
                    // padding never reaches the frame (only `write_w x
                    // write_h` is written out).
                    wedge_mask = Some(
                        crate::wedge::wedge_masks()
                            .codebook(write_w, write_h)
                            .mask(wedge_sign as usize, wedge_index),
                    );
                } else {
                    // COMPOUND_DIFFWTD: `mask_type`, MAX_DIFFWTD_MASK_BITS==1.
                    let mask_type = dec.literal(1);
                    diffwtd_mask_type = Some(mask_type as u8);
                    if wedge_bsize.is_none() {
                        hit!(DIFFWTD_INFERRED_HITS);
                    }
                }
                hit!(MASKED_COMPOUND_HITS);
                if write_w != write_h {
                    hit!(RECT_MASKED_COMPOUND_HITS);
                }
            }
            // corner-cut (lane-av1comp r16/r17, lane-av1blend r1): r16/r17
            // called this a `predict_compound_intermediate`/`combine_compound`
            // reconstruction defect. r1 FALSIFIED that hypothesis outright:
            // unmasking and running the real gates
            // (a_real_aomenc_stream_with_reference_select_reads_comp_mode_correctly,
            // a_real_aomenc_stream_with_compound_references_decodes_pixel_exact)
            // shows the plain-average blend is bit-exact *most of the time*
            // -- `combine_compound`'s two-shift-in-one round2 was proven
            // algebraically identical to libaom's separate
            // `>>DIST_PRECISION_BITS`-then-`ROUND_POWER_OF_TWO` sequence
            // (av1_dist_wtd_convolve_2d_c, convolve.c:402-415) for every
            // input, and a whole-pel zero-MV compound block (mv0=mv1=(0,0),
            // the common case a lag-in-frames GOP's hidden altref frames
            // hit) reproduces aomdec's own pre-deblock dump byte-for-byte
            // for several consecutive frames.
            //
            // The real defect a live gate run isolated (seed 49,
            // `fixtures/av1blend-r1-mismatch.obu`): decode-order frame 0
            // (keyframe) and frame 1 (first hidden altref, `ref1 ==
            // ALTREF_FRAME`) match aomdec's own pre-deblock dump byte for
            // byte; decode-order frame 2 -- same shape, same zero MV, only
            // difference `ref1 == BWDREF_FRAME` instead of `ALTREF_FRAME` --
            // is the *first* frame to diverge (881/6144 luma+chroma bytes).
            // Every later frame up through decode-order 9 also mismatches,
            // *including* frame 8, whose four blocks are back to
            // `ref1 == ALTREF_FRAME` only -- ruling out "BWDREF_FRAME's MC
            // math is wrong" (that block's own math is proven exact) in
            // favour of "the picture sitting in some DPB slot is already
            // wrong by the time frame 2 reads it, and the corruption
            // propagates through refresh_frame_flags into every later
            // reference regardless of which named ref a later block picks".
            // Frames 10 through 18 (once the GOP's lineage stops tracing
            // back through the bad slot) are bit-exact again. This points at
            // reference-slot bookkeeping (`ref_slots`/`refresh_frame_flags`
            // in `stream.rs`'s `decode_stream`, or `ref_order_hints`/DPB
            // indexing feeding `dist_wtd_comp_weight_assign`) around
            // BWDREF_FRAME specifically, not at the MC/blend arithmetic in
            // mc.rs. Not isolated further within lane-av1blend's budget --
            // refuse by name rather than ship a blend that is right only
            // sometimes. Ceiling: r13/r14's MC plumbing above this line
            // stays -- only the plain-average reconstruct is masked off
            // again. Upgrade: instrument `ref_slots`/`refresh_frame_flags`
            // per decode-order frame (which slot each `Frame` OBU refreshes,
            // and which slot each `ref_frame_idx[BWDREF_FRAME - LAST_FRAME]`
            // reads) against aomdec's own `RefCntBuffer` bookkeeping on
            // `fixtures/av1blend-r1-mismatch.obu`, starting at decode-order
            // frame 2.
            //
            // r2 (fresh fixture, seed 45, same recipe -- the original r1
            // fixture is gitignored and could not be regenerated byte-exact):
            // static review of `stream.rs`'s `ref_slots`/`refresh_frame_flags`/
            // `cdf_slots` bookkeeping and the `show_existing_frame` refresh
            // arm (spec 7.21: only refreshes on a shown KEY frame, which the
            // parser already enforces at `frame.rs:791/815` -- `refresh_frame_flags`
            // is forced 0 on every non-key `show_existing_frame`) turned up
            // nothing wrong; `other_refs`/`ref_planes` slot indexing for
            // `BWDREF_FRAME` is spec-correct too. A live unmasked run
            // isolated a *different, smaller* shape than r1's: decode-order
            // frame 3 (a `ref1 == BWDREF_FRAME`, zero-MV, simple-average
            // compound block) is the first to diverge from ffmpeg (worst
            // delta 1, luma only, clustered at 32-pixel block edges) even
            // though BOTH its compound inputs -- the key frame and
            // decode-order frame 2's own picture (`ref1 == ALTREF_FRAME`,
            // itself proven bit-exact: it is later re-shown via
            // `show_existing_frame` and matches ffmpeg exactly) -- are
            // independently pixel-exact, and the blend arithmetic is the
            // same proven-exact formula. That rules out "wrong slot"/"stale
            // picture" for this fixture: the inputs are right, so the defect
            // is in decode-order frame 3's *own* per-block decode (entropy
            // desync from compound-CDF adaptation carried across a slot, or
            // the compound mode/MV read itself) -- not in `ref_slots`/
            // `refresh_frame_flags`. Not isolated further within budget.
            // Upgrade: bisect decode-order frame 3's tile decode against a
            // synced `EC_TOK`/`EC_PART` trace from the instrumented aomdec
            // build (`/tmp/libaom-src/build/decoder-debug/aomdec`) on
            // `fixtures/av1blend-r1-mismatch.obu` (seed 45 now, not seed 49)
            // -- the pre-deblock dump frame-index alignment between this
            // crate and that aomdec build is NOT 1:1 on this fixture (23
            // dumps here vs 24 there), so calibrate that first.
            // r3 (this round): pre-deblock buffers were bisected byte-for-byte
            // against an instrumented aomdec build (frame-index alignment
            // recalibrated via content-hash matching rather than the raw
            // dump count -- the keyframe never calls a pre-deblock dump at
            // all, since only `decode_inter_frame_tile` does, which is the
            // whole "23 vs 24" gap, not a real bug).
            // Decode-order frame 3 (`fixtures/av1blend-r1-mismatch.obu`,
            // seed 45) is still the first divergence: PRE-deblock differs
            // from aomdec's own pre-deblock dump by up to 1, on ~20% of
            // luma pixels, clustered where its two compound inputs' pixel
            // values disagree by more than a couple of levels -- both
            // inputs (the keyframe and decode-order frame 2's own picture)
            // are independently proven bit-exact (pre- and post-deblock)
            // against aomdec, and `decodemv`'s own EC_TRACE (mode/mv/skip)
            // matches this decoder's block loop exactly for every block in
            // this frame. Solving `aomval = (key*w0 + bwd*w1 + 8) >> 4` for
            // every mismatching pixel against `key`/`bwd` (both known-exact)
            // picks out `w0=9, w1=7` for the majority (525/846) with zero
            // matches for the plain average this decoder actually emits or
            // the reversed `w0=7, w1=9` -- i.e. `compound_idx` is being
            // decoded as `1` (simple average, spec `COMPOUND_AVERAGE`) when
            // the real bitstream encoded `0` (`COMPOUND_DISTWTD`,
            // `dist_wtd_comp_weight_assign`'s own weights for this frame's
            // order-hint distances 2/3 land on exactly `(9, 7)` -- this
            // decoder's own `dist_wtd_comp_weight_assign` was independently
            // re-verified against `QUANT_DIST_WEIGHT`/`QUANT_DIST_LOOKUP_TABLE`
            // by hand for these distances and gives the same `(9, 7)`).
            // `get_comp_index_context` was checked line-for-line against
            // libaom's `pred_common.h` (the `fwd`/`bck` distance halves, the
            // `above_mi`/`left_mi` `has_second_ref`/`ALTREF_FRAME` fallback,
            // the `3 * offset` term) and matches; `Default_Compound_Idx_Cdf`
            // (`cdf.rs`'s `COMPOUND_IDX`) matches `entropymode.c`'s
            // `default_compound_idx_cdfs` value-for-value; the read is
            // gated on `!skip_mode && enable_jnt_comp`, matching libaom's
            // `has_second_ref(mbmi) && !mbmi->skip_mode` outer guard plus
            // `order_hint_info.enable_dist_wtd_comp` inner guard. The
            // remaining 321/846 mismatched pixels do NOT fit `(9, 7)`
            // either -- some blocks in this frame plausibly decode a
            // genuinely different (correct, context-dependent) compound_idx
            // than block (0,0)'s own -- so this is not simply "always wrong
            // the same way": table/context/gate are all individually
            // verified correct, yet the decoded VALUE at this specific
            // symbol is wrong at least once in this frame. Not isolated
            // further within budget. Upgrade: instrument `dec.symbol`'s own
            // range/bit state (`rng`/`tell`, matching aomdec's own EC_PART
            // trace fields) immediately before and after the `compound_idx`
            // read specifically, not just the surrounding mode/mv/skip
            // trace which does match -- the desync (if any) is local to
            // this one read, not a wider stream desync.
            //
            // r5 (this round): the `resolve_interp_filter` call used to sit
            // right here, ahead of `comp_group_idx`/`compound_idx` -- spec
            // 5.11.19/libaom `decodemv.c:1575` reads `interp_filter` AFTER
            // `read_compound_type`, not before, so that was a genuine
            // ordering bug for a Switchable-filter frame (moved below, past
            // the `compound_ctx = Some(...)` line). It does NOT explain this
            // fixture's own mismatch, though: `av1blend-r1-mismatch.obu`'s
            // header carries a *fixed* interpolation filter
            // (`interp_fixed = Some(Regular)`, confirmed live via a
            // one-off dump), so `resolve_interp_filter` never read a symbol
            // in either position -- zero bits moved, decoded values
            // unchanged before and after the reorder. Kept anyway: it is a
            // real spec-order fix for the next Switchable-filter compound
            // stream, independently correct regardless of this fixture.
            //
            // Also ruled out this round: comparing this crate's own
            // `SymbolDecoder::debug_bitpos()` against aomdec's
            // `aom_reader_tell()` at the block boundary is NOT a valid
            // desync probe -- they are different quantities (raw consumed
            // input bits vs. an entropy-cost estimate that advances without
            // a literal byte fetch), so equal/unequal bitpos proves nothing
            // either way. Do not repeat that comparison; instead trace
            // `range`/`value` (this crate) against libaom's own internal
            // `dif`/`rng` (needs new instrumentation in both) immediately
            // after each read *upstream* of this one in the same block:
            // `read_comp_mode`, `read_compound_ref_frames` (comp_ref_type/
            // uni_comp_ref/comp_ref/comp_bwdref), `read_inter_compound_mode`,
            // and the DRL loop. All of those independently decoded the SAME
            // values this crate and aomdec both show (ref0=1, ref1=5, mode
            // NEAREST_NEARESTMV, mv=(0,0)) -- but an upstream *context*
            // miscomputation can still decode the identical symbol value off
            // a slightly different CDF, narrowing `range` by a different
            // amount than aomdec without changing the decoded value itself,
            // and only surface as a wrong decode a few reads later exactly
            // like this. `get_comp_index_context` and the `COMPOUND_IDX`
            // table were independently re-verified again this round and are
            // still correct; the four candidates above were not re-audited
            // this round. Refusing here (rather than shipping a value-exact-
            // most-of-the-time blend) until one of them is found wrong.
            // r6/r7: found and fixed one real bug (mvstack.rs ref_ctx family
            // undercounted compound neighbours in the single_ref_p*/comp_ref*/
            // comp_bwdref* context reads -- kept, real fix). Unmasking on
            // that fix alone and running the wide gate sweep (many seeds,
            // `a_real_aomenc_stream_with_compound_references_decodes_pixel_exact`/
            // `a_real_aomenc_stream_with_reference_select_reads_comp_mode_correctly`,
            // cq-level=45) still mismatches roughly half the time, at
            // different frames each run for the SAME seed -- aomenc's own
            // cpu-used=0 RD search is run-to-run nondeterministic (noted
            // above), so this is a second, still-unfound compound_idx defect,
            // not the one r6/r7 fixed. r6's own residual-mismatch note (321/
            // 846 pixels not fitting the (9,7)-vs-plain-average hypothesis
            // either) already flagged more than one bug here. Re-masking:
            // do not ship a blend that is right only sometimes.
            // lane-av1idx r1: fixed one real bug this round
            // (`switchable_interp_ctx` never folded in libaom's
            // `INTER_FILTER_COMP_OFFSET` for a compound block's own
            // interp_filter context -- kept). Pinned
            // `fixtures/av1idx-refsel-pin.obu` (seed 61, reference_select
            // gate) still mismatches after that fix (decode-order frame 8
            // first, small deltas, worst 1-2, propagating through later
            // frames via DPB reference) -- a second, still-unfound bug.
            // Re-masking: see that fixture + `scratch_isolate_pinned_mismatch`
            // (`EC_AV1_PIN=fixtures/av1idx-refsel-pin.obu EC_AV1_PIN_N=20`)
            // for the next round's starting point.
            // lane-maskcomp r2: `compound_idx` is only read when
            // `comp_group_idx == 0` (libaom `decodemv.c:1606`) -- a masked
            // (wedge/diffwtd) block never also carries a distance-weighted
            // split. This gate was missing before r2 (dead code: the masked
            // path always refused earlier, so it never executed with
            // `comp_group_idx == 1` in practice).
            let (fwd_offset, bck_offset, compound_idx) =
                if comp_group_idx == 0 && !skip_mode && enable_jnt_comp {
                    let idx_ctx = get_comp_index_context(
                        neighbours,
                        (rmi, cmi),
                        side,
                        order_hint_bits,
                        order_hint,
                        ref_order_hints[(ref0 - LAST_FRAME) as usize],
                        ref_order_hints[(ref1 - LAST_FRAME) as usize],
                    );
                    crate::msac::SymbolDecoder::set_symr_cdf("compound_idx");
                    let idx = dec.symbol(&mut cdfs.compound_idx[idx_ctx]);
                    if crate::envflags::env_flag!("EC_TRACE_MODE") {
                        eprintln!(
                            "EC_CIDX mi_row={rmi} mi_col={cmi} ctx={idx_ctx} val={idx} rng={}",
                            dec.debug_state().0
                        );
                    }
                    if idx == 1 {
                        (8, 8, 1u8)
                    } else {
                        // lane-av1distwtd: `idx == 0` is the
                        // distance-weighted arm -- the only branch whose
                        // weights are NOT the constant (8, 8) split.
                        hit!(DIST_WTD_COMP_HITS);
                        let (fwd, bck) = crate::compound::dist_wtd_comp_weight_assign(
                            order_hint_bits,
                            order_hint,
                            ref_order_hints[(ref0 - LAST_FRAME) as usize],
                            ref_order_hints[(ref1 - LAST_FRAME) as usize],
                        );
                        (fwd, bck, 0u8)
                    }
                } else {
                    (8, 8, 1u8)
                };
            compound_ctx = Some((ref1, comp_group_idx as u8, compound_idx));
            if crate::envflags::env_flag!("EC_AV1_COMPIDX_DUMP") {
                eprintln!(
                    "EC_COMPIDX mi_row={mi_row} mi_col={mi_col} bsize={side} mode={compound_mode} mv0=({},{}) mv1=({},{}) ref0={ref0} ref1={ref1} comp_group_idx={comp_group_idx} compound_idx={compound_idx} tell={}",
                    mv0.0,
                    mv0.1,
                    mv1.0,
                    mv1.1,
                    dec.debug_bitpos()
                );
            }

            // spec 5.11.19/libaom `decodemv.c` 1575: `read_mb_interp_filter`
            // comes AFTER `read_compound_type` (the `comp_group_idx`/
            // `compound_idx` reads just above), not before -- moved here
            // (lane-av1blend r5) from its old position ahead of those reads,
            // which stole their bits for a Switchable-filter compound block
            // and desynced every symbol read after it in the tile.
            // `av1_is_interp_needed` is also 0 for `skip_mode` blocks: the
            // filter is derived (Regular), never read -- same
            // conditional-read contract as the WARPED_CAUSAL suppression on
            // the single-ref path below.
            let (h_filter, v_filter, resolved_filter) = resolve_interp_filter(
                dec,
                cdfs,
                interp_fixed,
                enable_dual_filter,
                gm_nontrans || skip_mode,
                above_filter_ctx,
                left_filter_ctx,
                true,
            );
            block_filter = resolved_filter;

            // lane-scaledref r1: each compound tap scales INDEPENDENTLY off
            // its own stored reference's luma width (spec 7.11.3.3 derives
            // x_scale_fp per reference; libaom `av1_setup_pre_planes` builds
            // one `scale_factors` per `ref_frame`), so the two taps can carry
            // different ratios in the same block. `REF_NO_SCALE` reduces
            // `predict_compound_intermediate`'s walk to the ordinary
            // stride-1 one, so the unscaled case is unchanged.
            let scale0 = scale_of(ref_dims(ref0, refpix)?, frame_size);
            let scale1 = scale_of(ref_dims(ref1, refpix)?, frame_size);
            if !scale0.is_unscaled() || !scale1.is_unscaled() {
                hit!(SCALED_COMPOUND_HITS);
            }
            // lane-scaledref r2: the MIXED case -- one tap scaled, the other
            // not -- is the one an all-frames-scaled recipe never produces,
            // and the one a single shared scale factor would get wrong.
            if (scale0.is_unscaled()) != (scale1.is_unscaled()) {
                hit!(MIXED_SCALE_COMPOUND_HITS);
            }

            // lane-wave4 (the compound arm's half): this block's whole
            // prediction -- both references, the wedge/diffwtd/distance
            // blend, every plane -- as a closure, run here (see the
            // single-reference arm below). A compound block reads
            // neither `motion_mode` nor `interintra`, so nothing but the
            // blend itself lives in here.
            let build = move |_y: &mut PlaneBuf<'static>,
                              _u: &mut PlaneBuf<'static>,
                              _v: &mut PlaneBuf<'static>,
                              refpix: &RefPix<'_>,
                              fctx: &crate::decode::FrameCtx,
                              out: &mut [Vec<u16>; 3]| {
                let (Some((py0, pu0, pv0)), Some((py1, pu1, pv1))) = (
                    ref_planes_deferred(ref0, refpix),
                    ref_planes_deferred(ref1, refpix),
                ) else {
                    return;
                };
                let mut inter0_y = vec![0i32; side * side];
                mc::predict_compound_intermediate_kern(
                    &py0.data,
                    py0.width,
                    py0.true_width,
                    py0.true_height,
                    mv_to_q4(px, mv0.1, 0),
                    mv_to_q4(py, mv0.0, 0),
                    scale0,
                    side,
                    side,
                    write_w,
                    write_h,
                    h_filter,
                    v_filter,
                    &mut inter0_y,
                    fctx,
                );
                // lane-cwarp r1: this reference's own GLOBAL warp replaces the
                // translational tap (libaom `av1_warp_plane` with
                // `conv_params->is_compound`); the blend below is unchanged.
                if let Some(wp) = warp0
                    .as_ref()
                    .filter(|_| warp_plane_allowed(write_w, write_h))
                {
                    crate::warp::warp_affine_compound(
                        wp,
                        &py0.data,
                        py0.true_width as i32,
                        py0.true_height as i32,
                        py0.width as i32,
                        &mut inter0_y,
                        px as i32,
                        py as i32,
                        side as i32,
                        side as i32,
                        side as i32,
                        0,
                        0,
                        fctx,
                    );
                }
                let mut inter1_y = vec![0i32; side * side];
                mc::predict_compound_intermediate_kern(
                    &py1.data,
                    py1.width,
                    py1.true_width,
                    py1.true_height,
                    mv_to_q4(px, mv1.1, 0),
                    mv_to_q4(py, mv1.0, 0),
                    scale1,
                    side,
                    side,
                    write_w,
                    write_h,
                    h_filter,
                    v_filter,
                    &mut inter1_y,
                    fctx,
                );
                // lane-cwarp r1: this reference's own GLOBAL warp replaces the
                // translational tap (libaom `av1_warp_plane` with
                // `conv_params->is_compound`); the blend below is unchanged.
                if let Some(wp) = warp1
                    .as_ref()
                    .filter(|_| warp_plane_allowed(write_w, write_h))
                {
                    crate::warp::warp_affine_compound(
                        wp,
                        &py1.data,
                        py1.true_width as i32,
                        py1.true_height as i32,
                        py1.width as i32,
                        &mut inter1_y,
                        px as i32,
                        py as i32,
                        side as i32,
                        side as i32,
                        side as i32,
                        0,
                        0,
                        fctx,
                    );
                }
                let mut pred_y = refill(std::mem::take(&mut out[0]), side * side);
                let mut diffwtd_mask_y;
                // lane-wedge r3: `mask_y` is the DIFFWTD buffer just computed OR
                // the wedge codebook lookup (comp_group_idx==1's two mutually
                // exclusive branches) -- same [`mc::blend_masked_compound`] call
                // either way, only the mask source differs.
                let mask_y: Option<&[u8]> = if let Some(mask_type) = diffwtd_mask_type {
                    diffwtd_mask_y = vec![0u8; side * side];
                    hit!(DIFFWTD_HITS);
                    mc::diffwtd_mask(
                        &inter0_y,
                        &inter1_y,
                        mask_type == 1,
                        &mut diffwtd_mask_y,
                        fctx,
                    );
                    Some(diffwtd_mask_y.as_slice())
                } else {
                    wedge_mask
                };
                if let Some(mask_y) = mask_y {
                    mc::blend_masked_compound(
                        &inter0_y,
                        &inter1_y,
                        mask_y,
                        side,
                        side,
                        side,
                        0,
                        0,
                        &mut pred_y,
                        fctx,
                    );
                } else {
                    mc::combine_compound(
                        &inter0_y,
                        &inter1_y,
                        fwd_offset,
                        bck_offset,
                        &mut pred_y,
                        fctx,
                    );
                }

                let mut inter0_u = vec![0i32; chroma_stride * chroma_buf_h];
                mc::predict_compound_intermediate_kern(
                    &pu0.data,
                    pu0.width,
                    pu0.true_width,
                    pu0.true_height,
                    mv_to_q4(cpx, mv0.1, ss_x(fctx)),
                    mv_to_q4(cpy, mv0.0, ss_y(fctx)),
                    scale0,
                    chroma_stride,
                    chroma_buf_h,
                    write_chroma_w,
                    write_chroma_h,
                    h_filter,
                    v_filter,
                    &mut inter0_u,
                    fctx,
                );
                // `av1_init_warp_params` bails when the PLANE's block is
                // narrower/shorter than 8 (`block_width < 8`), so a 4x4 chroma
                // block of an 8x8 luma block stays translational.
                if let Some(wp) = &warp0
                    && warp_plane_allowed(write_chroma_w, write_chroma_h)
                {
                    crate::warp::warp_affine_compound(
                        wp,
                        &pu0.data,
                        pu0.true_width as i32,
                        pu0.true_height as i32,
                        pu0.width as i32,
                        &mut inter0_u,
                        cpx as i32,
                        cpy as i32,
                        chroma_stride as i32,
                        chroma_buf_h as i32,
                        chroma_stride as i32,
                        ss_x(fctx) as i32,
                        ss_y(fctx) as i32,
                        fctx,
                    );
                }
                let mut inter1_u = vec![0i32; chroma_stride * chroma_buf_h];
                mc::predict_compound_intermediate_kern(
                    &pu1.data,
                    pu1.width,
                    pu1.true_width,
                    pu1.true_height,
                    mv_to_q4(cpx, mv1.1, ss_x(fctx)),
                    mv_to_q4(cpy, mv1.0, ss_y(fctx)),
                    scale1,
                    chroma_stride,
                    chroma_buf_h,
                    write_chroma_w,
                    write_chroma_h,
                    h_filter,
                    v_filter,
                    &mut inter1_u,
                    fctx,
                );
                // `av1_init_warp_params` bails when the PLANE's block is
                // narrower/shorter than 8 (`block_width < 8`), so a 4x4 chroma
                // block of an 8x8 luma block stays translational.
                if let Some(wp) = &warp1
                    && warp_plane_allowed(write_chroma_w, write_chroma_h)
                {
                    crate::warp::warp_affine_compound(
                        wp,
                        &pu1.data,
                        pu1.true_width as i32,
                        pu1.true_height as i32,
                        pu1.width as i32,
                        &mut inter1_u,
                        cpx as i32,
                        cpy as i32,
                        chroma_stride as i32,
                        chroma_buf_h as i32,
                        chroma_stride as i32,
                        ss_x(fctx) as i32,
                        ss_y(fctx) as i32,
                        fctx,
                    );
                }
                let mut pred_u = refill(std::mem::take(&mut out[1]), chroma_stride * chroma_buf_h);
                if let Some(mask_y) = mask_y {
                    // lane-av1-422blockhalf: the blend walks this plane's own
                    // (chroma_stride x chroma_buf_h) rect through the plane's
                    // real subsampling shifts -- the mask read is aom_dsp/
                    // blend-a64-mask.c's `(1, 0)` branch at 4:2:2, the direct
                    // luma sample at 4:2:0/4:4:4.
                    mc::blend_masked_compound(
                        &inter0_u,
                        &inter1_u,
                        mask_y,
                        side,
                        chroma_stride,
                        chroma_buf_h,
                        ss_x(fctx),
                        ss_y(fctx),
                        &mut pred_u,
                        fctx,
                    );
                } else {
                    mc::combine_compound(
                        &inter0_u,
                        &inter1_u,
                        fwd_offset,
                        bck_offset,
                        &mut pred_u,
                        fctx,
                    );
                }

                let mut inter0_v = vec![0i32; chroma_stride * chroma_buf_h];
                mc::predict_compound_intermediate_kern(
                    &pv0.data,
                    pv0.width,
                    pv0.true_width,
                    pv0.true_height,
                    mv_to_q4(cpx, mv0.1, ss_x(fctx)),
                    mv_to_q4(cpy, mv0.0, ss_y(fctx)),
                    scale0,
                    chroma_stride,
                    chroma_buf_h,
                    write_chroma_w,
                    write_chroma_h,
                    h_filter,
                    v_filter,
                    &mut inter0_v,
                    fctx,
                );
                // `av1_init_warp_params` bails when the PLANE's block is
                // narrower/shorter than 8 (`block_width < 8`), so a 4x4 chroma
                // block of an 8x8 luma block stays translational.
                if let Some(wp) = &warp0
                    && warp_plane_allowed(write_chroma_w, write_chroma_h)
                {
                    crate::warp::warp_affine_compound(
                        wp,
                        &pv0.data,
                        pv0.true_width as i32,
                        pv0.true_height as i32,
                        pv0.width as i32,
                        &mut inter0_v,
                        cpx as i32,
                        cpy as i32,
                        chroma_stride as i32,
                        chroma_buf_h as i32,
                        chroma_stride as i32,
                        ss_x(fctx) as i32,
                        ss_y(fctx) as i32,
                        fctx,
                    );
                }
                let mut inter1_v = vec![0i32; chroma_stride * chroma_buf_h];
                mc::predict_compound_intermediate_kern(
                    &pv1.data,
                    pv1.width,
                    pv1.true_width,
                    pv1.true_height,
                    mv_to_q4(cpx, mv1.1, ss_x(fctx)),
                    mv_to_q4(cpy, mv1.0, ss_y(fctx)),
                    scale1,
                    chroma_stride,
                    chroma_buf_h,
                    write_chroma_w,
                    write_chroma_h,
                    h_filter,
                    v_filter,
                    &mut inter1_v,
                    fctx,
                );
                // `av1_init_warp_params` bails when the PLANE's block is
                // narrower/shorter than 8 (`block_width < 8`), so a 4x4 chroma
                // block of an 8x8 luma block stays translational.
                if let Some(wp) = &warp1
                    && warp_plane_allowed(write_chroma_w, write_chroma_h)
                {
                    crate::warp::warp_affine_compound(
                        wp,
                        &pv1.data,
                        pv1.true_width as i32,
                        pv1.true_height as i32,
                        pv1.width as i32,
                        &mut inter1_v,
                        cpx as i32,
                        cpy as i32,
                        chroma_stride as i32,
                        chroma_buf_h as i32,
                        chroma_stride as i32,
                        ss_x(fctx) as i32,
                        ss_y(fctx) as i32,
                        fctx,
                    );
                }
                let mut pred_v = refill(std::mem::take(&mut out[2]), chroma_stride * chroma_buf_h);
                if let Some(mask_y) = mask_y {
                    // lane-av1-422blockhalf: the blend walks this plane's own
                    // (chroma_stride x chroma_buf_h) rect through the plane's
                    // real subsampling shifts -- the mask read is aom_dsp/
                    // blend-a64-mask.c's `(1, 0)` branch at 4:2:2, the direct
                    // luma sample at 4:2:0/4:4:4.
                    mc::blend_masked_compound(
                        &inter0_v,
                        &inter1_v,
                        mask_y,
                        side,
                        chroma_stride,
                        chroma_buf_h,
                        ss_x(fctx),
                        ss_y(fctx),
                        &mut pred_v,
                        fctx,
                    );
                } else {
                    mc::combine_compound(
                        &inter0_v,
                        &inter1_v,
                        fwd_offset,
                        bck_offset,
                        &mut pred_v,
                        fctx,
                    );
                }

                // lane-inter4 r2: a rectangular strip's transform tree starts from
                // its own rect `max_txsize_rect_lookup` entry, not a square one.
                *out = [pred_y, pred_u, pred_v];
            };
            let (pred_y, pred_u, pred_v): (Vec<u16>, Vec<u16>, Vec<u16>);
            let (sy, su, sv) = if pred_deferred(refpix) {
                push_pred(build);
                (Pred::Cur { plane: 0, stride: side, off: 0 }, Pred::Cur { plane: 1, stride: chroma_stride, off: 0 }, Pred::Cur { plane: 2, stride: chroma_stride, off: 0 })
            } else {
                let mut built: [Vec<u16>; 3] = Default::default();
                build(y, u, v, refpix, fctx, &mut built);
                let [by, bu, bv] = built;
                (pred_y, pred_u, pred_v) = (by, bu, bv);
                (
                    Pred::Inline(&pred_y, side),
                    Pred::Inline(&pred_u, chroma_stride),
                    Pred::Inline(&pred_v, chroma_stride),
                )
            };

            let rect_tu = (write_w, write_h) != (side, side);
            vartx_leaves = if rect_tu {
                read_block_tx_size_rect(
                    dec,
                    cdfs,
                    neighbours,
                    at_mi,
                    (write_w, write_h),
                    (mi_cols as usize, mi_rows as usize),
                    skip,
                    fctx,
                )?
            } else {
                read_block_tx_size(
                    dec,
                    cdfs,
                    neighbours,
                    at_mi,
                    side,
                    (mi_cols as usize, mi_rows as usize),
                    true,
                    skip,
                    fctx,
                )?
                .1
            };
            if skip {
                push_mc_rect(
                    y,
                    u,
                    v,
                    0,
                    px,
                    py,
                    side,
                    write_w,
                    write_h,
                    sy,
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
                    chroma_stride,
                    write_chroma_w,
                    write_chroma_h,
                    su,
                    &ZERO_RESIDUAL[..chroma_stride * chroma_buf_h],
                    fctx,
                );
                push_mc_rect(
                    y,
                    u,
                    v,
                    2,
                    cpx,
                    cpy,
                    chroma_stride,
                    write_chroma_w,
                    write_chroma_h,
                    sv,
                    &ZERO_RESIDUAL[..chroma_stride * chroma_buf_h],
                    fctx,
                );
                luma_grid = Grid::Zero(side * side);
                u_grid = Grid::Zero(chroma_stride * chroma_buf_h);
                v_grid = Grid::Zero(chroma_stride * chroma_buf_h);
            } else {
                // lane-inter4 r2: a rect strip's coefficient context spans its
                // OWN width above and height left -- reading `side` cells both
                // ways indexes past the tile's last mi row for a bottom strip.
                let around = if rect_tu {
                    // lane-av1422mi24: at ss (1,0) the rect strip's chroma
                    // above span is `write_w >> 1` chroma cells spread over
                    // `write_w` luma mi columns -- the per-mi gather counts
                    // every chroma column's whole-unit dc sign twice (the
                    // t422 frame-1 mi(26,60) strip's U dc_sign read one row
                    // high). Luma keeps the per-mi extents; chroma takes
                    // [`Neighbours::around_mi_422_chroma`]'s every-second-
                    // above-cell sampling.
                    let mut a = neighbours.around_mi_rect(at_mi, write_w, write_h);
                    if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                        let c = neighbours.around_mi_422_chroma(at_mi, write_w, write_h);
                        a[1] = c[1];
                        a[2] = c[2];
                    }
                    a
                } else if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                    // lane-av1422mi24: at ss (1,0) a square block's chroma
                    // plane block is (side/2, side) -- `side` chroma 4-px
                    // above cells spread over `side` luma mi columns, one
                    // chroma column per PAIR of luma mi cells. The plain
                    // per-mi gather samples every second chroma column, and
                    // its dc-vote sum lands one sign row off (t422 frame 1,
                    // mi(24,56) U dc_sign: ours read dc_sign_cdf[0] where
                    // the oracle read row 1). Luma keeps the per-mi gather
                    // verbatim; only the two chroma planes take
                    // [`Neighbours::around_mi_422_chroma`]'s every-second-
                    // above-cell sampling (the same measured rule the strip
                    // and chunk arms already run).
                    let mut a = neighbours.around_mi(at, side);
                    let c = neighbours.around_mi_422_chroma(at, write_w, write_h);
                    a[1] = c[1];
                    a[2] = c[2];
                    a
                } else {
                    neighbours.around_mi(at, side)
                };
                let luma_tx_type;
                // lane-sb128c r1: a 128x128 inter block's chroma is read
                // INSIDE the luma leaf loop (each 64x64 mu chunk's own units:
                // one per chunk at 4:2:0, FOUR TX_32X32 at ss 0/0); these
                // collect them for the block-level neighbour write-back.
                let mut u_units: Vec<i32> = Vec::new();
                let mut v_units: Vec<i32> = Vec::new();
                // lane-av1-444-128: every luma leaf's coded tx_type, so the
                // chunk tail's chroma units can each inherit the type of the
                // leaf covering their own quadrant (`av1_get_tx_type`,
                // blockd.h:1291, scales the CHROMA 4x4 position back to luma).
                let mut leaf_tx_types: Vec<(usize, usize, usize, usize, TxType)> = Vec::new();
                if let Some(leaves) = vartx_leaves.as_ref() {
                    // spec 5.11.17's transform tree, leaf by leaf in the order
                    // `read_var_tx_size` coded it: each unit reads its own
                    // coefficients against the neighbour magnitudes the
                    // earlier units already wrote, and reconstructs over its
                    // own slice of the block's motion-compensated predictor.
                    let reduced = fctx.reduced_tx_set_inter.with(std::cell::Cell::get);
                    let mut first_tx_type = TxType::DctDct;
                    // lane-av1h5: libaom's `decode_token_recon_block` walks
                    // 64x64 "mu" chunks with the PLANE loop inside, so at 128
                    // the token order is CHUNK-major; the transform tree hands
                    // us a BLOCK-wide raster whose chunk members are not
                    // contiguous. Below 128 a block IS one chunk.
                    let (order, _mu_reordered) = mu_chunk_order(leaves, side);
                    for (pos, &idx) in order.iter().enumerate() {
                        let (row, col, tw, th) = leaves[idx];
                        let tu_mi = (at_mi.0 + row, at_mi.1 + col);
                        let (tu_px, tu_py) = (px + col * MI, py + row * MI);
                        let (tu_grid, tu_tx_type) = if tw == th {
                            let tx_px = tw;
                            let tu_pred = sy.offset(row * MI * side + col * MI);
                            let scan = default_scan(tx_px.min(32));
                            let tu_skip_ctx = neighbours.luma_skip_ctx(tu_mi, tx_px / MI);
                            read_inter_plane(
                                dec,
                                cdfs,
                                inter_txbset_for(tx_px, reduced),
                                scan,
                                0,
                                neighbours.around_mi(tu_mi, tx_px)[0],
                                mode_for_tx,
                                y,
                                u,
                                v,
                                tu_px,
                                tu_py,
                                tx_px,
                                base_q_idx,
                                tu_pred,
                                None,
                                Some(tu_skip_ctx),
                                fctx,
                            )?
                        } else {
                            // lane-r14 r3: a 1:4 strip's `sub_tx_size_map`
                            // sub-transform is RECTANGULAR (TX_64X16 ->
                            // TX_32X16, TX_16X64 -> TX_16X32), so this leaf is
                            // read with the rect coefficient machinery, at its
                            // own `txb_skip_ctx`/neighbour spans -- the unit is
                            // smaller than its block, so `get_txb_ctx_general`'s
                            // neighbour table applies, not the lone-TU 0.
                            vartx_rect_leaf_hit(tw, th);
                            let tu_skip_ctx =
                                neighbours.luma_skip_ctx_rect(tu_mi, tw / MI, th / MI);
                            let start = row * MI * side + col * MI;
                            read_inter_plane_rect(
                                dec,
                                cdfs,
                                rect_inter_luma_set(tw, th, fctx)?,
                                (tw, th),
                                side,
                                0,
                                neighbours.around_mi_rect(tu_mi, tw, th)[0],
                                mode_for_tx,
                                y,
                                u,
                                v,
                                tu_px,
                                tu_py,
                                sy.offset(start),
                                None,
                                Some(tu_skip_ctx),
                                fctx,
                            )?
                        };
                        if idx == 0 {
                            first_tx_type = tu_tx_type;
                        }
                        // lane-av1444chr: EVERY luma leaf's coded type, at
                        // every block size (see the single-reference twin
                        // below) -- the chroma units resolve their own
                        // quadrant's leaf, and a leaf that coded NOTHING is
                        // `DctDct` exactly as libaom's `decodetxb.c:199-203`
                        // stamps `DCT_DCT` into that leaf's map cell.
                        leaf_tx_types.push((row, col, tw, th, tu_tx_type));
                        neighbours.record_mi_luma_rect(tu_mi, tw, th, &tu_grid);
                        // lane-sb128c r1: libaom's `decode_token_recon_block`
                        // walks 64x64 "mu" chunks (`mu_blocks_wide/high =
                        // mi_size_wide[BLOCK_64X64]`) and codes EVERY PLANE of
                        // a chunk before the next one, so a 128x128 block
                        // interleaves Y,U,V per quadrant -- the same order the
                        // intra 128 path reads (lane-sb128b r3, measured
                        // against aomdec's `EC_TRACE_COEFF`). Below 128 a block
                        // is a single chunk and this never fires.
                        if side > 64 {
                            let chunk = (row / 16, col / 16);
                            let done = pos + 1 == order.len()
                                || (leaves[order[pos + 1]].0 / 16, leaves[order[pos + 1]].1 / 16)
                                    != chunk;
                            if done {
                                let (cr, cc) = chunk;
                                // `av1_get_max_uv_txsize(BLOCK_128X128)` =
                                // `av1_get_adjusted_tx_size(TX_64X64)` =
                                // TX_32X32. The mu walk
                                // (`decode_token_recon_block`, decodeframe.c)
                                // then enumerates, per plane, EVERY TX_32X32
                                // unit of the chunk's chroma in raster order:
                                // at ss 0/0 the chunk's chroma is 64x64 and
                                // that is FOUR units; at 4:2:0 it is 32x32
                                // and the loop degenerates to the one unit
                                // this arm has always coded. `chroma_side`
                                // already carries the subsampling.
                                let cu_tx = 32usize;
                                // lane-av1-422bigblock: a 64x64 mu chunk's
                                // chroma plane block is PER-AXIS
                                // (64 >> ss_x) x (64 >> ss_y) -- 32x64 at
                                // ss (1,0), i.e. TWO stacked TX_32X32 units
                                // per plane (`av1_get_max_uv_txsize` caps the
                                // unit at 32 on each axis). The old square-cut
                                // model walked `chroma_side`-square chunks:
                                // twice the units the oracle codes at 4:2:2,
                                // prediction windows indexed at the square
                                // `chroma_side` instead of the prediction
                                // buffer's own `chroma_stride`, and chunk
                                // columns at 64.. of a 64-wide chroma plane.
                                let chunk_chroma_w = (64usize) >> ss_x(fctx);
                                let chunk_chroma_h = (64usize) >> ss_y(fctx);
                                let units_w = chunk_chroma_w / cu_tx;
                                let units_h = chunk_chroma_h / cu_tx;
                                // The unit's own footprint on the LUMA mi
                                // grid, per axis (32 << ss) px: 64x64 (16x16
                                // mi) at 4:2:0, 32x32 (8x8 mi) at 4:4:4,
                                // 64x32 (16x8 mi) at 4:2:2. The context
                                // gather and `record_mi_chroma`'s stamp span
                                // exactly the unit's own cells -- `get_txb_ctx`
                                // reads the 8 context cells of the TX_32X32
                                // itself, and `av1_set_entropy_contexts` memsets
                                // the same 8, so the unit over never sees this
                                // unit's state and the unit below always does.
                                // At ss (1, 0) those 8 are CHROMA cells, so the
                                // gather routes through the 4:2:2 pair rule
                                // (lane-av1muchunk422sym, see the walk below).
                                let unit_luma_w = cu_tx << ss_x(fctx);
                                let unit_luma_h = cu_tx << ss_y(fctx);
                                let cu_scan = default_scan(cu_tx);
                                if lossless(fctx) {
                                    // lane-lossless2: TX_4X4 units over this
                                    // chunk's chroma, not one TX_32X32 unit.
                                    read_inter_chroma_lossless(
                                        y,
                                        u,
                                        v,
                                        dec,
                                        cdfs,
                                        neighbours,
                                        su,
                                        sv,
                                        at_mi,
                                        (cpx, cpy),
                                        (cc * chunk_chroma_w, cr * chunk_chroma_h),
                                        (chunk_chroma_w, chunk_chroma_h),
                                        (write_chroma_w, write_chroma_h),
                                        // lane-av1422lpf site 5: the stride
                                        // the walk indexes `su`/`sv` at is the
                                        // PREDICTION BUFFER's own
                                        // `chroma_stride` -- the same number
                                        // `su`/`sv` were built at, and the one
                                        // the composed `u_units`/`v_units`
                                        // grid is sized and written at (site
                                        // 1's `stride * blk_h`). `chroma_side`
                                        // is the ENCLOSING SQUARE
                                        // (`max(side >> ss_x, side >> ss_y)`)
                                        // and at 4:2:2 it is the LUMA side: a
                                        // 128x128 block's chroma plane block is
                                        // 64x128, so the walk addressed
                                        // `oy * 128 + ox` on a 64-wide, 8192-
                                        // sample buffer and the last unit row
                                        // ran off the end
                                        // (`offset 8192 .. 8580 of 8192`,
                                        // X_intrabc_tiled frame 1, at
                                        // `push_mc_rect_tx`).
                                        //
                                        // At 4:2:0 `chroma_stride ==
                                        // chroma_side == side >> 1` and at
                                        // 4:4:4 both are `side`, so this
                                        // argument is the same number on every
                                        // shape this decoder admits except
                                        // 4:2:2.
                                        chroma_stride,
                                        mode_for_tx,
                                        base_q_idx,
                                        &mut u_units,
                                        &mut v_units,
                                        fctx,
                                    )?;
                                    mu_chroma = true;
                                    continue;
                                }
                                for plane_idx in 1..3 {
                                    let src = if plane_idx == 1 { su } else { sv };
                                    // Plane-major per chunk (libaom's plane
                                    // loop is outermost inside the mu walk);
                                    // within a plane the units walk raster.
                                    for ur in 0..units_h {
                                        for uc in 0..units_w {
                                            let unit_mi = (
                                                at_mi.0 + cr * 16 + ur * (unit_luma_h / MI),
                                                at_mi.1 + cc * 16 + uc * (unit_luma_w / MI),
                                            );
                                            // Per unit, AFTER the earlier
                                            // units' stamps: this unit's
                                            // coefficient context must see
                                            // them.
                                            //
                                            // lane-av1muchunk422sym: at ss
                                            // (1, 0) one CHROMA column spans
                                            // two luma mi columns, so the plain
                                            // rect gather sums this unit's
                                            // above dc sign TWICE where
                                            // libaom's `get_txb_ctx_general`
                                            // reads `txb_w_unit` CHROMA cells --
                                            // one per column. `dc_sign_ctx`
                                            // only sees the vote's signum, so
                                            // the doubled vote flips the row
                                            // exactly when above and left
                                            // cancel; `around_mi_422_chroma`
                                            // samples every second above cell,
                                            // which IS libaom's sum. Same guard
                                            // as the intra twin (13169) and the
                                            // block-8 arm (19810).
                                            let cu_around = if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                                                neighbours.around_mi_422_chroma(
                                                    unit_mi,
                                                    unit_luma_w,
                                                    unit_luma_h,
                                                )
                                            } else {
                                                neighbours.around_mi_rect(
                                                    unit_mi,
                                                    unit_luma_w,
                                                    unit_luma_h,
                                                )
                                            };
                                            let (cu_x, cu_y) = (
                                                cpx + cc * chunk_chroma_w + uc * cu_tx,
                                                cpy + cr * chunk_chroma_h + ur * cu_tx,
                                            );
                                            // `av1_get_tx_type` (blockd.h:
                                            // 1291): an inter block's chroma
                                            // unit never codes a tx_type; it
                                            // inherits the coded type of the
                                            // luma leaf covering its own
                                            // quadrant (all of this chunk's
                                            // leaves are read by now).
                                            let unit_rel_mi =
                                                (unit_mi.0 - at_mi.0, unit_mi.1 - at_mi.1);
                                            let covering =
                                                covering_leaf_tx_type(&leaf_tx_types, unit_rel_mi);
                                            // The prediction buffer is
                                            // `chroma_stride`-strided (the
                                            // plane block's own width at
                                            // 4:2:2), not the square
                                            // `chroma_side`.
                                            let cu_pred = src.offset(
                                                (cr * chunk_chroma_h + ur * cu_tx) * chroma_stride
                                                    + cc * chunk_chroma_w
                                                    + uc * cu_tx,
                                            );
                                            let cu_grid = read_inter_plane(
                                                dec,
                                                cdfs,
                                                chroma_set,
                                                cu_scan,
                                                plane_idx,
                                                cu_around[plane_idx],
                                                mode_for_tx,
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
                                            // Immediately, so the NEXT unit
                                            // reads this one's coefficient
                                            // context.
                                            neighbours.record_mi_chroma(
                                                unit_mi,
                                                unit_luma_w,
                                                unit_luma_h,
                                                plane_idx,
                                                &cu_grid,
                                            );
                                            hit!(CHROMA_SPLIT_TX_HITS);
                                            mu_chroma = true;
                                            // lane-av1muchunk422b: the allocation of this
                                            // mu-chunk walk, counted per TX_32X32 UNIT.
                                            // The compound and single-reference arms share
                                            // this source text, so `is_compound` routes the
                                            // bump: `MU_CHUNK_COMPOUND_UNITS` names site 2
                                            // and the single-reference twin (site 5) stays
                                            // out of it.
                                            if is_compound {
                                                hit!(MU_CHUNK_COMPOUND_UNITS);
                                            }
                                            let dst = mu_units(
                                                if plane_idx == 1 {
                                                    &mut u_units
                                                } else {
                                                    &mut v_units
                                                },
                                                chroma_side * chroma_side,
                                            );
                                            for rr in 0..cu_tx {
                                                // The ASSEMBLED grid stays at
                                                // the square `chroma_side`
                                                // stride the block tail's
                                                // `mu_chroma_units` re-stamp
                                                // indexes; the unit's true
                                                // per-axis position is what
                                                // changes.
                                                let start = (cr * chunk_chroma_h + ur * cu_tx + rr)
                                                    * chroma_side
                                                    + cc * chunk_chroma_w
                                                    + uc * cu_tx;
                                                dst[start..start + cu_tx].copy_from_slice(
                                                    &cu_grid[rr * cu_tx..][..cu_tx],
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // The block-level `luma_tx_type` stays the first
                    // (top-left) luma unit's coded type; the PER-UNIT chroma
                    // inheritance in the chunk tail above resolves each unit's
                    // own covering leaf instead (`av1_get_tx_type`,
                    // blockd.h:1291).
                    luma_tx_type = first_tx_type;
                    luma_grid = Grid::Zero(side * side);
                } else if rect_tu {
                    // The whole-block rectangular transform unit
                    // (`txfm_partition` said 0 at depth 0).
                    let (grid, tx_type) = read_inter_plane_rect(
                        dec,
                        cdfs,
                        rect_inter_luma_set(write_w, write_h, fctx)?,
                        (write_w, write_h),
                        side,
                        0,
                        around[0],
                        mode_for_tx,
                        y,
                        u,
                        v,
                        px,
                        py,
                        sy,
                        None,
                        None,
                        fctx,
                    )?;
                    luma_tx_type = tx_type;
                    luma_grid = Grid::Own(embed_rect_grid(&grid, write_w, write_h, side));
                } else {
                    {
                        let coded = read_inter_plane(
                            dec,
                            cdfs,
                            luma_set_inter,
                            scan_luma,
                            0,
                            around[0],
                            mode_for_tx,
                            y,
                            u,
                            v,
                            px,
                            py,
                            side,
                            base_q_idx,
                            sy,
                            None,
                            None,
                            fctx,
                        )?;
                        luma_grid = coded.0;
                        luma_tx_type = coded.1;
                    }
                }
                if side > 64 {
                    // Already read inside the mu-chunk loop above -- at the 128
                    // root a rect (128x64/64x128) block's chroma is per mu
                    // chunk too, so this test comes BEFORE the `rect_tu` one.
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
                } else if lossless(fctx) {
                    // lane-lossless2: TX_4X4 chroma units, plane-major
                    // (libaom `read_tx_size` + `decode_token_recon_block`).
                    let (mut uo, mut vo) = (Vec::new(), Vec::new());
                    read_inter_chroma_lossless(
                        y,
                        u,
                        v,
                        dec,
                        cdfs,
                        neighbours,
                        su,
                        sv,
                        at_mi,
                        (cpx, cpy),
                        (0, 0),
                        (write_chroma_w, write_chroma_h),
                        (write_chroma_w, write_chroma_h),
                        chroma_stride,
                        mode_for_tx,
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
                    // Every unit stamped its own neighbour context above; the
                    // whole-block record must not overwrite them with the
                    // assembled grid (class `override-slot-on-one-arm`).
                    mu_chroma = true;
                } else if rect_tu || (write_chroma_w, write_chroma_h) != (chroma_side, chroma_side)
                {
                    // One chroma read for the whole strip at 4:2:0, two
                    // 32-capped units at 4:4:4 -- see the helper.
                    // lane-av1-422j: a 4:2:2 square block's chroma plane is the
                    // (side/2 x side) rect (a 32x32 block's 16x32), never
                    // `chroma_side`-square -- the square read below would pair
                    // a `chroma_side`-square unit with the 4:2:0 unit scan
                    // (one-coefficient-per-position assert in
                    // inverse_transform_2d_typed_wh). The rect reader codes
                    // the exact `av1_get_max_uv_txsize` unit (TX_16X32,
                    // `rect_inter_chroma_set`'s tables).
                    (u_grid, v_grid, rect_chroma_units) = read_inter_rect_chroma(
                        y,
                        u,
                        v,
                        dec,
                        cdfs,
                        neighbours,
                        around,
                        su,
                        sv,
                        at_mi,
                        (cpx, cpy),
                        (write_chroma_w, write_chroma_h),
                        chroma_stride,
                        mode_for_tx,
                        &leaf_tx_types,
                        luma_tx_type,
                        fctx,
                    )?;
                } else if ss_x(fctx) == 0 && ss_y(fctx) == 0 && chroma_side > chroma_tx {
                    // lane-av1-444: at ss 0/0 this block's chroma plane is
                    // `chroma_side` square while `av1_get_max_uv_txsize` caps
                    // the units at TX_32X32 -- each plane codes FOUR TX_32X32
                    // units, plane-major inside the block's one 64x64 mu chunk
                    // (libaom `decode_token_recon_block`'s plane loop). Each
                    // unit's `txb_skip_ctx` carries the +10
                    // `get_txb_ctx_general` offset (plane block larger than the
                    // transform); our `txb_skip_chroma_32` holds those rows at
                    // +3, so `luma_skip_ctx: Some(3)` (lane-sb128b r3's
                    // convention, as the 128 mu path above).
                    let cu_tx = chroma_tx;
                    let mu_units_n = chroma_side / cu_tx;
                    let mut u_acc: Vec<i32> = Vec::new();
                    let mut v_acc: Vec<i32> = Vec::new();
                    for plane_idx in 1..3 {
                        let (src, acc) = if plane_idx == 1 {
                            (su, &mut u_acc)
                        } else {
                            (sv, &mut v_acc)
                        };
                        for cr in 0..mu_units_n {
                            for cc in 0..mu_units_n {
                                let cu_mi = (rmi + cr * (cu_tx / MI), cmi + cc * (cu_tx / MI));
                                let cu_around = neighbours.around_mi(cu_mi, cu_tx);
                                // Per-quadrant inheritance, as in the
                                // single-reference twin: the covering luma
                                // leaf's OWN coded type, `DctDct` where that
                                // leaf coded nothing.
                                let cu_rel_mi = (cr * (cu_tx / MI), cc * (cu_tx / MI));
                                let cu_tx_type = covering_leaf_tx_type(&leaf_tx_types, cu_rel_mi)
                                    .unwrap_or(luma_tx_type);
                                if covering_leaf_tx_type(&leaf_tx_types, cu_rel_mi).is_some() {
                                    hit!(CHROMA_QUAD_LEAF_TX_HITS);
                                    if cu_tx_type != luma_tx_type {
                                        hit!(CHROMA_QUAD_LEAF_TX_DIFF_HITS);
                                    }
                                }
                                let cu_grid = read_inter_plane(
                                    dec,
                                    cdfs,
                                    chroma_set,
                                    scan_chroma,
                                    plane_idx,
                                    cu_around[plane_idx],
                                    mode_for_tx,
                                    y,
                                    u,
                                    v,
                                    cpx + cc * cu_tx,
                                    cpy + cr * cu_tx,
                                    cu_tx,
                                    base_q_idx,
                                    src.offset(cr * cu_tx * chroma_side + cc * cu_tx),
                                    Some(cu_tx_type),
                                    Some(3),
                                    fctx,
                                )?
                                .0;
                                // Immediately, so the next unit's context sees
                                // this one's coefficients.
                                neighbours
                                    .record_mi_chroma(cu_mi, cu_tx, cu_tx, plane_idx, &cu_grid);
                                hit!(CHROMA_SPLIT_TX_HITS);
                                // lane-av1-444 c8: the unit's levels must ALSO
                                // land in the block grid -- the block-tail mu
                                // re-stamp slices the per-unit states back out
                                // of it. This arm used to hand it a ZERO grid,
                                // so the re-stamp (and `record_rect_mi`'s
                                // whole-block write under it) painted the whole
                                // block's chroma context cells 0 AFTER the
                                // correct per-unit stamps above: the next block
                                // read `txb_skip_ctx` base 0 where aom gathers
                                // 1 (f52 mi(52,16) U all_zero, ctx 0 vs 8).
                                let dst = mu_units(acc, chroma_side * chroma_side);
                                for rr in 0..cu_tx {
                                    let start = (cr * cu_tx + rr) * chroma_side + cc * cu_tx;
                                    dst[start..start + cu_tx]
                                        .copy_from_slice(&cu_grid[rr * cu_tx..][..cu_tx]);
                                }
                            }
                        }
                    }
                    mu_chroma = true;
                    u_grid = Grid::Own(u_acc);
                    v_grid = Grid::Own(v_acc);
                } else {
                    u_grid = read_inter_plane(
                        dec,
                        cdfs,
                        chroma_set,
                        scan_chroma,
                        1,
                        around[1],
                        mode_for_tx,
                        y,
                        u,
                        v,
                        cpx,
                        cpy,
                        chroma_side,
                        base_q_idx,
                        su,
                        Some(luma_tx_type),
                        None,
                        fctx,
                    )?
                    .0;
                    v_grid = read_inter_plane(
                        dec,
                        cdfs,
                        chroma_set,
                        scan_chroma,
                        2,
                        around[2],
                        mode_for_tx,
                        y,
                        u,
                        v,
                        cpx,
                        cpy,
                        chroma_side,
                        base_q_idx,
                        sv,
                        Some(luma_tx_type),
                        None,
                        fctx,
                    )?
                    .0;
                }
            }
        } else {
            let ref_frame = read_single_ref(
                dec,
                cdfs,
                neighbours.above_ref[cmi],
                neighbours.above_ref1[cmi],
                neighbours.left_ref[rmi],
                neighbours.left_ref1[rmi],
            );
            ref_frame_for_lf = ref_frame;
            if crate::envflags::env_flag!("EC_AV1_TELL") {
                let (mi_row, mi_col) = (rmi, cmi);
                eprintln!(
                    "TELL mi_row={mi_row} mi_col={mi_col} label=post_ref_frame ref={ref_frame} tell={} range={}",
                    dec.debug_bitpos(),
                    dec.debug_state().0
                );
            }
            // round 4-9 (lane-av1golden..lane-av1golden7): GOLDEN_FRAME's own
            // stacked defects (lf_level ref/mode-delta forwarding, the
            // switchable_interp ref-frame ctx gate, and a film-grain synthesis
            // gap that looked like a GOLDEN MC bug by survivor bias -- see the
            // ledger) are all closed; GOLDEN_FRAME decodes live via `other_refs`
            // below same as every other non-`LAST_FRAME` reference. lane-av1refs
            // widens this from GOLDEN alone to every reference `read_single_ref`
            // can name.
            // lane-defer8: SIZE here (spec 7.11.3.3's scale factor), pixels
            // inside the prediction closure -- the parse thread of a threaded
            // decode must never wait on a reference picture.
            let (ref_width, ref_height) = ref_dims(ref_frame, refpix)?;

            let (mi_row, mi_col) = (rmi, cmi);
            // lane-rect r2: the mv stack must be queried with the block's
            // REAL extent (libaom `av1_get_mv_refs` gets bw4/bh4 from
            // xd->width/xd->height, not a square guess) -- `write_w`/
            // `write_h` already name the true HORZ/VERT/HORZ_B strip
            // footprint (r1's plumbing), so derive bw4/bh4 from those
            // instead of the old `side`-square (HORZ_B-only) heuristic.
            let bw4 = write_w / 4;
            let bh4 = write_h / 4;
            // spec 7.10.2.8: only reached when the frame header set
            // `use_ref_frame_mvs` (`tpl_frame` is `Some` then, `None`
            // otherwise) -- `cur_offset_0` is this query block's own resolved
            // `ref_frame`'s distance from the current frame (spec 7.9.3),
            // recomputed per block since different blocks pick different refs.
            let tpl = tpl_frame.map(|t| crate::mvstack::TplArgs {
                field: t.field,
                cur_offset_0: crate::motion_field::get_relative_dist(
                    t.order_hint_bits,
                    t.order_hint,
                    t.ref_order_hints[(ref_frame - LAST_FRAME) as usize],
                ),
                allow_high_precision_mv,
                force_integer_mv,
            });
            let gm_table = build_gm_mv_table(
                global_motion,
                mi_row,
                mi_col,
                bw4,
                bh4,
                allow_high_precision_mv,
                force_integer_mv,
            );
            let stack = find_mv_stack_with_sign_bias(
                grid,
                mi_row,
                mi_col,
                bw4,
                bh4,
                ref_frame,
                mi_cols as usize,
                mi_rows as usize,
                sign_bias_table,
                &gm_table,
                tpl,
            );
            let not_new = dec.symbol(&mut cdfs.new_mv[stack.new_mv_ctx]) == 1;
            if !not_new {
                crate::census::mode(0);
            }
            let mut is_globalmv = false;
            // lane-interbis r1: which single-ref mode this block resolved to,
            // in the oracle's own numbering (NEARESTMV=13 .. NEWMV=16), for
            // the `EC_MODE_VAL` value ladder below.
            let mut dbg_mode = 16u32;
            let (mv, is_new_mv) = if !not_new {
                // NEWMV (spec 5.11.24's `read_drl_idx`, `RefMvIdx` starting at 0):
                // read at most two `drl_mode` bits, one per stack entry past the
                // first, stopping at the first `0`. The chosen index selects
                // which stack entry `read_mv`'s base predictor comes from
                // (spec 7.10.2.10 `assign_mv`'s `PredMv = RefStackMv[RefMvIdx]`)
                // -- entry 0 (`stack.pred_mv`) only when the loop never advances.
                let mut idx = 0usize;
                while idx < 2 && stack.entries.len() > idx + 1 {
                    if dec.symbol(&mut cdfs.drl_mode[stack.drl_ctx[idx]]) == 0 {
                        break;
                    }
                    idx += 1;
                }
                let base_mv = stack.entries.get(idx).map_or(stack.pred_mv, |e| e.mv);
                (
                    read_mv(
                        dec,
                        &mut cdfs.mv_comp,
                        &mut cdfs.mv_joint,
                        base_mv,
                        allow_high_precision_mv,
                        force_integer_mv,
                        fctx,
                    ),
                    true,
                )
            } else {
                let not_zero = dec.symbol(&mut cdfs.zero_mv[stack.zero_mv_ctx]) == 1;
                if !not_zero {
                    crate::census::mode(1);
                }
                is_globalmv = !not_zero;
                let mv = if !not_zero {
                    // GLOBALMV: `gm_get_motion_vector` (spec 7.10.2.1), already
                    // computed at this block's own position in `gm_table`.
                    dbg_mode = 15;
                    gm_table[(ref_frame - LAST_FRAME) as usize]
                } else {
                    let nearest = dec.symbol(&mut cdfs.ref_mv[stack.ref_mv_ctx]) == 0;
                    crate::census::mode(if nearest { 2 } else { 3 });
                    dbg_mode = if nearest { 13 } else { 14 };
                    if nearest {
                        stack.nearest_mv
                    } else {
                        // NEARMV (spec 5.11.24's `read_drl_idx`, `RefMvIdx` starting
                        // at 1): read at most two more `drl_mode` bits, one per stack
                        // entry past the second, stopping at the first `0`.
                        // `drl_ctx[idx]` is the context between `entries[idx]` and
                        // `entries[idx + 1]` (`MvStack::drl_ctx`'s own doc), which is
                        // exactly the pair this index is choosing between.
                        let mut idx = 1usize;
                        while idx < 3 && stack.entries.len() > idx + 1 {
                            if dec.symbol(&mut cdfs.drl_mode[stack.drl_ctx[idx]]) == 0 {
                                break;
                            }
                            idx += 1;
                        }
                        stack.entries.get(idx).map_or(stack.near_mv, |e| e.mv)
                    }
                };
                (mv, false)
            };
            if crate::msac::symtrace::dir().is_some() {
                crate::msac::symtrace::note(&format!(
                    "  MODE mi=({mi_row},{mi_col}) dbg={dbg_mode} mv={mv:?}"
                ));
            }
            if crate::envflags::env_flag!("EC_TRACE_MODE") {
                for (i, e) in stack.entries.iter().enumerate() {
                    eprintln!(
                        "EC_STACK mi_row={mi_row} mi_col={mi_col} ref={ref_frame} i={i} this=({},{}) comp=(0,0) w={}",
                        e.mv.0, e.mv.1, e.weight
                    );
                }
                eprintln!(
                    "EC_MODE_VAL mi_row={mi_row} mi_col={mi_col} mode={dbg_mode} ref0={ref_frame} ref1=-1 mv0=({},{}) stack={} rng={}",
                    mv.0,
                    mv.1,
                    stack.entries.len(),
                    dec.debug_state().0
                );
            }
            // lane-gm r2: the two libaom predicates single-ref GLOBALMV
            // blocks need -- `is_global_mv_block` (blockd.h:421-429, mode
            // GLOBAL* AND model > TRANSLATION AND min(bw,bh) >= 8px, gating
            // `motion_mode_eligible`/`MiInfo::is_global_mv0` below) and
            // `is_nontrans_global_motion` (reconinter.h:420-425, model !=
            // TRANSLATION -- IDENTITY counts here -- gating the interp
            // filter suppress below). Do NOT unify these.
            let gm_model = global_motion[(ref_frame - LAST_FRAME) as usize].model;
            let min_bw_bh4 = bw4.min(bh4);
            let is_global_mv_block = is_globalmv && gm_model as u8 > 1 && min_bw_bh4 >= 2;
            let gm_nontrans =
                is_globalmv && gm_model != ec_av1_syntax::WarpModel::Translation && min_bw_bh4 >= 2;
            if crate::envflags::env_flag!("EC_AV1_TELL") {
                eprintln!(
                    "TELL mi_row={mi_row} mi_col={mi_col} label=post_assign_mv tell={} range={}",
                    dec.debug_bitpos(),
                    dec.debug_state().0
                );
            }
            // lane-sb128 r4: `interintra` (spec 5.11.24; libaom
            // `decodemv.c` read_inter_block_mode_info ~1490-1510), placed
            // right after `assign_mv` and before `read_motion_mode` below --
            // libaom's own sequential order. `is_interintra_allowed_bsize`
            // (`BLOCK_8X8..=BLOCK_32X32`) is the only real gate on this
            // single-ref path: `is_interintra_allowed_ref` always holds here
            // (single ref, `ref_frame[1]` not yet `INTRA_FRAME`) and
            // `is_interintra_allowed_mode` always holds (every mode this
            // branch produces -- NEARESTMV/NEARMV/GLOBALMV/NEWMV -- is
            // `SINGLE_INTER_MODE_START..END` by construction).
            // `size_group_lookup`: BLOCK_16X16 -> 2, BLOCK_32X32 -> 3.
            let mut interintra_mode: Option<u8> = None;
            let mut wedge_mask: Option<(&'static [u8], usize)> = None;
            // lane-inter4 r1: `is_interintra_allowed_bsize` is
            // `BLOCK_8X8..=BLOCK_32X32` in libaom's enum ORDER, which excludes
            // every 1:4 shape (BLOCK_8X32 = 18, BLOCK_32X8 = 19) even though
            // both dimensions are in range -- reading this symbol on a 1:4
            // strip is a symbol the reference never wrote.
            let interintra_shape =
                write_w.max(write_h) <= 32 && write_w.max(write_h) / write_w.min(write_h) <= 2;
            if enable_interintra_compound
                && !skip_mode
                && (side == 16 || side == 32)
                && interintra_shape
            {
                // lane-inter4 r3: `size_group_lookup` reads the block's
                // TRUE footprint -- a 16x8 strip is group 1, a 32x16 strip
                // group 2, not their square `side`'s 2/3.
                let bsize_group = size_group_wh(write_w, write_h);
                crate::msac::SymbolDecoder::set_symr_cdf("interintra");
                let interintra = dec.symbol(&mut cdfs.interintra[bsize_group]) == 1;
                if interintra {
                    // lane-interintra r1 (decodemv.c 1540-1555): interintra_mode,
                    // then -- gated on block size ALONE, never the
                    // `enable_interintra_wedge` seq bit -- the wedge flag
                    // (`av1_is_wedge_used(bsize)` holds for 16x16/32x32).
                    let ii = dec.symbol(&mut cdfs.interintra_mode[bsize_group]) as u8;
                    // lane-inter16ab r3 (same class as the compound
                    // `compound_type` row below): the row is the block's TRUE
                    // `BLOCK_SIZES_ALL` index -- a 16x8 / 32x16 interintra
                    // strip is BLOCK_16X8 / BLOCK_32X16, not its square
                    // `side`'s BLOCK_16X16 / BLOCK_32X32.
                    let wedge_bsize = bsize_all_index(write_w, write_h)
                        .expect("interintra shape is a BLOCK_SIZES_ALL size");
                    let wedge = dec.symbol(&mut cdfs.wedge_interintra[wedge_bsize]) == 1;
                    if wedge {
                        // lane-wii r2 (spec 5.11.25): `wedge_index` is an
                        // ADAPTING CDF symbol over the same `wedge_bsize` row;
                        // NO sign symbol follows -- libaom fixes
                        // INTERINTRA_WEDGE_SIGN 0 (blockd.h).
                        let wedge_index = dec.symbol(&mut cdfs.wedge_idx[wedge_bsize]);
                        hit!(WII_HITS);
                        if let Some(k) = rect_wedge_slot(write_w, write_h) {
                            hit_do! {
                                RECT_WII_HITS.with(|c| {
                                    let mut a = c.get();
                                    a[k] += 1;
                                    c.set(a);
                                });
                            }
                        }
                        // lane-inter16ab r5: true-footprint codebook row,
                        // mask padded to stride `max(bw,bh) == side` -- see
                        // the compound site's own comment.
                        wedge_mask = Some((
                            crate::wedge::wedge_masks()
                                .codebook(write_w, write_h)
                                .mask(0, wedge_index),
                            side,
                        ));
                    } else {
                        hit!(INTERINTRA_HITS);
                    }
                    interintra_mode = Some(ii);
                    // The inter-intra predictor reads this block's neighbours as already
                    // reconstructed: a deferred wavefront must finish first.
                    super::wave::finish_tile(fctx, refpix, [&mut *y, &mut *u, &mut *v]);
                }
            }
            if crate::envflags::env_flag!("EC_AV1_TELL") {
                eprintln!(
                    "TELL mi_row={mi_row} mi_col={mi_col} label=post_interintra tell={} range={}",
                    dec.debug_bitpos(),
                    dec.debug_state().0
                );
            }
            // lane-motionmode round 1: `read_motion_mode` (spec 5.11.24;
            // libaom `decodemv.c` read_inter_block_mode_info, ~1520-1528),
            // placed right after MV assignment and before the switchable
            // interp filter read below (`read_mb_interp_filter`, decodemv.c
            // ~1600) -- libaom's own sequential order. `motion_mode_allowed`
            // (`reconinter.h`) reduces, for every block this single-ref path
            // reaches, to "does an inter neighbour border the above row or
            // left column": `is_motion_variation_allowed_bsize` always holds
            // (`side` is never below 16px on this path), `ref_frame[1] !=
            // INTRA_FRAME` always holds (no interintra reader here),
            // `is_motion_variation_allowed_compound` always holds (this is
            // the single-ref branch), and `is_global_mv_block` never
            // triggers (this decoder's global motion is always `IDENTITY`,
            // so its warp `type` is never `> TRANSLATION`). A stream with
            // `allow_warped_motion=1` is refused right at an eligible block
            // rather than guessed at: `av1_findSamples`/`num_proj_ref` is
            // not ported, so this decoder cannot tell a real
            // `WARPED_CAUSAL`-eligible block (3-symbol `motion_mode_cdf`)
            // from an `OBMC_CAUSAL`-only one (2-symbol `obmc_cdf`) whenever
            // the header allows warp -- reading the wrong alphabet desyncs
            // the tile.
            // An interintra block has `ref_frame[1] == INTRA_FRAME`, which
            // fails libaom's `is_motion_variation_allowed_compound`: the
            // motion_mode symbol is NOT read (SIMPLE_TRANSLATION implied).
            let motion_mode_eligible = switchable_motion_mode
                && !skip_mode
                // `is_motion_variation_allowed_bsize` (blockd.h:1455):
                // min(bw, bh) >= 8, false for a 16x4 / 4x16 strip.
                && write_w.min(write_h) >= 8
                && interintra_mode.is_none()
                && has_overlappable_neighbour(
                    grid, mi_row, mi_col, bw4, bh4, mi_cols as usize, mi_rows as usize,
                )
                // libaom `motion_mode_allowed`: a GLOBALMV block whose model
                // is non-IDENTITY-non-TRANSLATION under free (non-integer)
                // mv reads no motion_mode/obmc symbol at all -- implicit
                // SIMPLE_TRANSLATION.
                && !(is_global_mv_block && !force_integer_mv);
            // lane-t900 r9: the eligibility terms themselves, on the same rung
            // as aomdec's `EC_ISTEP2 name=motion_mode` -- an eligibility
            // mismatch is otherwise invisible (a skipped symbol prints
            // nothing).
            if crate::envflags::env_flag!("EC_TRACE_MODE_STEP") {
                eprintln!(
                    "EC_MM mi_row={mi_row} mi_col={mi_col} w={write_w} h={write_h} elig={} \
                     ab={} lf={} sw={} skipmode={} ii={} gmb={} rng={}",
                    motion_mode_eligible as u8,
                    overlappable_above(grid, mi_row, mi_col, bw4, mi_cols as usize, 1).len(),
                    overlappable_left(grid, mi_row, mi_col, bh4, mi_rows as usize, 1).len(),
                    switchable_motion_mode as u8,
                    skip_mode as u8,
                    interintra_mode.is_some() as u8,
                    is_global_mv_block as u8,
                    dec.debug_state().0,
                );
            }
            let mut obmc_selected = false;
            // lane-warp round 2: `Some` when motion_mode == WARPED_CAUSAL
            // *and* the local warp estimate is valid (`!wm_params.invalid`,
            // `av1_find_projection`) -- an invalid estimate falls back to
            // this block's own translational mv, same as any other block
            // (spec `allow_warp`/`reconinter.c` -- global warp is never the
            // fallback here since this decoder's global motion is always
            // IDENTITY).
            let mut warp_params: Option<crate::warp::WarpParams> = None;
            // Tracks the SYMBOL value, not projection validity: libaom's
            // `av1_is_interp_needed` suppresses the interp-filter read for
            // `motion_mode == WARPED_CAUSAL` even when the projection later
            // falls back to translation. Its third suppressor,
            // `is_nontrans_global_motion`, matches our unconditional
            // `is_globalmv` only because this decoder codes global motion as
            // IDENTITY (non-TRANSLATION) and never reads switchable filters
            // below 8x8 -- porting TRANSLATION global motion must add the
            // wmtype check here.
            let mut warped_selected = false;
            if motion_mode_eligible {
                // `default_obmc_cdf`'s own index: square bsize 8/16/32/64 ->
                // 0/1/2/3; lane-rect r2: a rect strip reads its OWN bsize row
                // (libaom `motion_mode_cdf[mbmi->bsize]`: BLOCK_16X32 -> 4,
                // BLOCK_32X16 -> 5 in our packed table) -- rect-flake-1's
                // strip motion_mode read diverged on the square row.
                // lane-inter4 r1: every rect shape this decoder can now
                // reach reads its OWN libaom bsize row (the packed order is
                // documented on `cdf::MOTION_MODE`); a wrong row is a wrong
                // interval narrowing, not a wrong value (class
                // wrong-alphabet-same-value).
                let bsize_idx = motion_mode_cdf_row(write_w, write_h).ok_or_else(|| {
                    unsupported("a motion_mode symbol for a block shape with no CDF row here")
                })?;
                // `motion_mode_allowed` reads the 3-symbol `motion_mode_cdf`
                // instead of the 2-symbol `obmc_cdf` exactly when
                // `num_proj_ref >= 1` under `allow_warped_motion`.
                // lane-scaledref r2 + lane-fimv r1: libaom
                // `motion_mode_allowed` (`blockd.h:1484`) requires BOTH
                // `!av1_is_scaled(block_ref_scale_factors[0])` and
                // `!xd->cur_frame_force_integer_mv` for WARPED_CAUSAL --
                // under a scaled reference (superres) OR on a frame whose
                // header set `force_integer_mv` (screen content) the block
                // reads the 2-symbol `obmc_cdf`, never the 3-symbol
                // `motion_mode_cdf`. Reading the wrong alphabet here narrows
                // the arithmetic coder by the wrong amount and predicts the
                // rest of the tile off a diverged state: silently wrong
                // pixels, not a desync error. `force_integer_mv` is the
                // per-frame value (spec 5.9.2: coded per frame only when
                // `seq_force_integer_mv == SELECT_INTEGER_MV`, threaded from
                // `FrameHeader::force_integer_mv`).
                // libaom `av1_is_scaled` is `x_scale_fp != REF_NO_SCALE ||
                // y_scale_fp != REF_NO_SCALE`, which `RefScale::is_unscaled`
                // reads on both axes.
                let ref_is_scaled = !scale_of((ref_width, ref_height), frame_size).is_unscaled();
                let proj_ok = num_proj_ref(
                    grid,
                    mi_row,
                    mi_col,
                    bw4,
                    bh4,
                    mi_cols as usize,
                    mi_rows as usize,
                    ref_frame,
                    fctx,
                ) >= 1;
                let warp_eligible =
                    allow_warped_motion && !ref_is_scaled && !force_integer_mv && proj_ok;
                if allow_warped_motion && proj_ok && ref_is_scaled {
                    hit!(SCALED_WARP_SUPPRESSED_HITS);
                }
                if allow_warped_motion && proj_ok && !ref_is_scaled && force_integer_mv {
                    hit!(FIMV_ALPHABET_HITS);
                }
                if warp_eligible {
                    crate::msac::SymbolDecoder::set_symr_cdf("motion_mode");
                    let mode = dec.symbol(&mut cdfs.motion_mode[bsize_idx]);
                    match mode {
                        0 => {}
                        1 => {
                            obmc_selected = true;
                            hit!(OBMC_HITS);
                        }
                        _ => {
                            warped_selected = true;
                            hit!(WARP_SELECTED_HITS);
                            let mut samples = find_samples(
                                grid,
                                mi_row,
                                mi_col,
                                bw4,
                                bh4,
                                mi_cols as usize,
                                mi_rows as usize,
                                ref_frame,
                                fctx,
                            );
                            if crate::envflags::env_flag!("EC_WARP_DEBUG") {
                                eprintln!(
                                    "EC_WARP_DEBUG findSamples mi_row={mi_row} mi_col={mi_col} bsize={side} num_proj_ref={}",
                                    samples.len()
                                );
                                for (i, s) in samples.iter().enumerate() {
                                    eprintln!(
                                        "EC_WARP_DEBUG sample[{i}] pts={:?} pts_inref={:?}",
                                        s.pts1, s.pts2
                                    );
                                }
                            }
                            if samples.len() > 1 {
                                crate::warp::select_samples(
                                    mv,
                                    &mut samples,
                                    side as i32,
                                    side as i32,
                                );
                            }
                            warp_params = crate::warp::find_projection(
                                &samples,
                                // lane-rect r2: the block's TRUE dims -- a
                                // 32x16 strip's model center is not a
                                // 32x32's (aom av1_find_projection bw/bh).
                                write_w as i32,
                                write_h as i32,
                                mv.1,
                                mv.0,
                                mi_row as i32,
                                mi_col as i32,
                            );
                            if crate::envflags::env_flag!("EC_WARP_DEBUG") {
                                eprintln!(
                                    "EC_WARP_DEBUG projection mi_row={mi_row} mi_col={mi_col} num_proj_ref(final)={} mv=({},{}) params={:?}",
                                    samples.len(),
                                    mv.0,
                                    mv.1,
                                    warp_params
                                );
                                for (i, s) in samples.iter().enumerate() {
                                    eprintln!(
                                        "EC_WARP_DEBUG sample_used[{i}] pts={:?} pts_inref={:?}",
                                        s.pts1, s.pts2
                                    );
                                }
                            }
                        }
                    }
                } else {
                    obmc_selected = dec.symbol(&mut cdfs.obmc[bsize_idx]) == 1;
                    if obmc_selected {
                        hit!(OBMC_HITS);
                    }
                }
                // lane-t900 r9: which ALPHABET this block read (3-symbol
                // motion_mode vs 2-symbol obmc) and the range after -- the
                // two give the same value with a different narrowing (class
                // wrong-alphabet-same-value).
                if crate::envflags::env_flag!("EC_TRACE_MODE_STEP") {
                    eprintln!(
                        "EC_MM2 mi_row={mi_row} mi_col={mi_col} row={bsize_idx} warp_elig={} \
                         scaled={} fimv={} obmc={} warped={} rng={}",
                        warp_eligible as u8,
                        ref_is_scaled as u8,
                        force_integer_mv as u8,
                        obmc_selected as u8,
                        warped_selected as u8,
                        dec.debug_state().0,
                    );
                }
            }
            // lane-gm r4: `allow_warp`'s `global_warp_allowed` branch
            // (`reconinter.c:33-55`), gated INDEPENDENTLY of `motion_mode` --
            // libaom's `av1_init_warp_params` runs for every inter
            // predictor build, not just a `WARPED_CAUSAL`-selected one. Only
            // reachable when `warp_params` is still `None` here: a block
            // that read (and got) local `WARPED_CAUSAL` above already has
            // `local_warp_allowed && !wm_params.invalid`, which `allow_warp`
            // checks FIRST and short-circuits on -- the global branch is an
            // `else if`, never layered on top. `is_global_mv_block` already
            // encodes `allow_warp`'s size bound (`min(bw4,bh4) >= 2`, i.e.
            // >=8px both dims, matching `av1_init_warp_params`'s
            // `block_height < 8 || block_width < 8` early return) and, via
            // `motion_mode_eligible`'s own suppression above, the
            // `force_integer_mv` case never reads a motion_mode symbol at
            // all -- checked again here directly since this branch fires
            // independent of `motion_mode_eligible`.
            let gm_ref = &global_motion[(ref_frame - LAST_FRAME) as usize];
            if warp_params.is_none() && is_global_mv_block && !force_integer_mv && !gm_ref.invalid {
                warp_params = crate::warp::global_warp_params(gm_ref.params);
                if warp_params.is_some() && gm_ref.model == ec_av1_syntax::WarpModel::Affine {
                    hit!(AFFINE_GM_HITS);
                }
                // lane-av1gwarp12: the ROTZOOM sibling -- the only
                // >TRANSLATION model vanilla aomenc can emit for a single-ref
                // global-motion block (encoder search pinned to ROTZOOM).
                if warp_params.is_some() && gm_ref.model == ec_av1_syntax::WarpModel::Rotzoom {
                    hit!(ROTZOOM_GM_WARP_HITS);
                }
            }
            // lane-av112bitw: a local `WARPED_CAUSAL` projection (set above)
            // or a global warp model both land here with `warp_params` set --
            // the 12-bit refusal that used to stand here is LIFTED: warp.rs's
            // rounding is parameterised on the bumped `conv_params->round_0`
            // (`warp::warp_round_0`, 5 at 12 bits) and the six-frame 12-bit
            // warp stream decodes byte-exact vs ffmpeg
            // (`a_12bit_warped_motion_stream_decodes_pixel_exact`).
            if crate::envflags::env_flag!("EC_AV1_TELL") {
                eprintln!(
                    "TELL mi_row={mi_row} mi_col={mi_col} label=post_motion_mode eligible={} tell={} range={}",
                    motion_mode_eligible as u8,
                    dec.debug_bitpos(),
                    dec.debug_state().0
                );
            }
            // spec `get_ref_filter_type`: a neighbour's own filter only feeds
            // this block's switchable_interp context when that neighbour coded
            // the SAME reference frame this block is about to read -- a
            // different-ref neighbour reads as "no neighbour" ([3, 3], the
            // sentinel `Neighbours::new` already seeds unset slots with),
            // exactly like an intra neighbour. Without this gate, a GOLDEN_FRAME
            // block next to a LAST_FRAME one inherits that neighbour's filter
            // choice into its own context, corrupting the symbol it reads.
            // lane-av1idx r2: also match on the neighbour's SECOND reference
            // (`above_ref1`/`left_ref1`, real for a compound neighbour) --
            // see the 16x16 leaf's own comment above for the spec citation.
            let above_filter_ctx = if neighbours.above_ref[cmi] == ref_frame
                || neighbours.above_ref1[cmi] == Some(ref_frame)
            {
                neighbours.above_filter[cmi]
            } else {
                [3, 3]
            };
            let left_filter_ctx = if neighbours.left_ref[rmi] == ref_frame
                || neighbours.left_ref1[rmi] == Some(ref_frame)
            {
                neighbours.left_filter[rmi]
            } else {
                [3, 3]
            };
            let (h_filter, v_filter, resolved_filter) = resolve_interp_filter(
                dec,
                cdfs,
                interp_fixed,
                enable_dual_filter,
                gm_nontrans || warped_selected,
                above_filter_ctx,
                left_filter_ctx,
                false,
            );
            block_filter = resolved_filter;
            globalmv_for_lf = is_globalmv;
            if crate::envflags::env_flag!("EC_AV1_TELL") {
                eprintln!(
                    "TELL mi_row={mi_row} mi_col={mi_col} label=post_interp_filter tell={} range={}",
                    dec.debug_bitpos(),
                    dec.debug_state().0
                );
            }
            if crate::envflags::env_flag!("EC_AV1_TRACE") {
                eprintln!(
                    "EC_TRACE mi_row={mi_row} mi_col={mi_col} skip={} is_inter=1 mv=({},{}) is_new_mv={is_new_mv} bsize={side} ref={ref_frame} filter={:?} motion_mode_eligible={} obmc_selected={} warped={} tell={}",
                    skip as u8,
                    mv.0,
                    mv.1,
                    block_filter,
                    motion_mode_eligible as u8,
                    obmc_selected as u8,
                    warp_params.is_some() as u8,
                    dec.debug_bitpos()
                );
            }
            // lane-rect r2: see the compound path's matching comment above --
            // `grid` must be stamped with the block's true bh4/bw4 span, not
            // a square `bw4`x`bw4` guess.
            grid.fill_rect(
                mi_row,
                mi_col,
                bh4,
                bw4,
                MiInfo {
                    is_inter: true,
                    ref_frame,
                    // An interintra block records ref_frame[1] ==
                    // INTRA_FRAME (0): warp-sample gathering
                    // (libaom av1_findSamples, mvref_common.c:1155)
                    // requires ref_frame[1] == NONE_FRAME, so such a
                    // neighbour must not donate samples or count in
                    // num_proj_ref.
                    ref_frame1: if interintra_mode.is_some() {
                        0
                    } else {
                        NO_REF1
                    },
                    mv1: (0, 0),
                    mv: mv16(mv),
                    is_new_mv,
                    size: bw4 as u8,
                    size_h: bh4 as u8,
                    is_global_mv0: is_global_mv_block,
                    is_global_mv1: false,
                },
            );
            mode_for_tx = 0;
            uv_predict_mode = DC_PRED;

            // lane-superres r9: spec 7.11.3.3's x_scale_fp comes from LUMA
            // widths only (r8's derivation) and applies unchanged to the
            // chroma calls below (their x_q4 is already in the chroma
            // plane's own pixel units). `mc::predict_scaled` has no warp/OBMC
            // /interintra counterpart -- those three combinations are refused
            // by name instead of silently sampling a scaled reference wrong
            // (warp_params/obmc_selected/interintra_mode are all resolved by
            // this point, every symbol for this block already read).
            let luma_scale = scale_of((ref_width, ref_height), frame_size);
            if !luma_scale.is_unscaled() {
                // lane-scaledref r1: libaom `allow_warp`
                // (av1/common/reconinter.c:41) suppresses BOTH local and
                // global warp under a scaled reference and predicts
                // translationally instead -- the fallback below. lane-t900
                // r28 LIFTED the refusal that sat here: the mismatch r1
                // measured was the 16x4/4x16 strip's sub-8x8 chroma piece,
                // which this file used to SKIP under a scaled reference
                // (fixed in the same commit), not the warp fallback.
                if warp_params.is_some() {
                    hit!(SCALED_WARP_FALLBACK_HITS);
                }
                if obmc_selected {
                    hit!(SCALED_OBMC_HITS);
                }
                if interintra_mode.is_some() {
                    hit!(SCALED_INTERINTRA_HITS);
                }
            }

            // lane-inter16ab r2: `is_sub8x8_inter`'s neighbour walk reads the
            // PREVIOUS strip's `mv`/`ref_frame`/`interp_filters`
            // (reconinter_template.inc:75-82); record them for the next 1:4
            // strip, which is the only reader.
            fctx.inter_last_mc
                .with(|c| c.set(Some((ref_frame, mv, h_filter, v_filter))));
            // lane-defer8: the sub-8x8 chroma piece's own reference resolved to
            // a SIZE here and to pixels inside the closure, like the block's own.
            let sub8 = match strip_chroma
                .filter(|s| s.has_chroma)
                .and_then(|s| s.prev.map(|p| (s.horz, p)))
            {
                Some((horz, (prev_ref, prev_mv, prev_h, prev_v))) => {
                    let prev_dims = ref_dims(prev_ref, refpix)?;
                    hit!(INTER16_SUB8_PIECE_HITS);
                    Some((
                        horz,
                        prev_ref,
                        prev_mv,
                        prev_h,
                        prev_v,
                        scale_of(prev_dims, frame_size),
                    ))
                }
                None => None,
            };
            // The whole-block prediction (motion compensation + warp + the
            // interintra blend) runs here, inline. OBMC and the sub-8x8 chroma
            // piece are built here too.
            // lane-defer8: OBMC's neighbour walk reads the mode-info grid and
            // the neighbour bands, both of which the parse keeps rewriting --
            // so the walk happens here and the blend inside the closure.
            let obmc = if obmc_selected {
                Some(obmc_plan(
                    grid,
                    &neighbours.above_filter,
                    &neighbours.left_filter,
                    mi_row,
                    mi_col,
                    bw4,
                    bh4,
                    mi_rows as usize,
                    mi_cols as usize,
                    side,
                    write_w,
                    write_h,
                    ss_x(fctx),
                    ss_y(fctx),
                    chroma_stride,
                    px,
                    py,
                    cpx,
                    cpy,
                    refpix,
                    interp_fixed,
                    frame_size,
                )?)
            } else {
                None
            };
            let build = move |y: &mut PlaneBuf<'static>,
                              u: &mut PlaneBuf<'static>,
                              v: &mut PlaneBuf<'static>,
                              refpix: &RefPix<'_>,
                              fctx: &crate::decode::FrameCtx,
                              out: &mut [Vec<u16>; 3]| {
                let Some((py_ref, pu_ref, pv_ref)) = ref_planes_deferred(ref_frame, refpix) else {
                    return;
                };
                let mut pred_y = refill(std::mem::take(&mut out[0]), side * side);
                let mut pred_u = refill(std::mem::take(&mut out[1]), chroma_stride * chroma_buf_h);
                let mut pred_v = refill(std::mem::take(&mut out[2]), chroma_stride * chroma_buf_h);
                if luma_scale.is_unscaled() {
                    mc::predict_with_filters_kern(
                        &py_ref.data,
                        py_ref.width,
                        py_ref.true_width,
                        py_ref.true_height,
                        mv_to_q4(px, mv.1, 0),
                        mv_to_q4(py, mv.0, 0),
                        side,
                        side,
                        write_w,
                        write_h,
                        h_filter,
                        v_filter,
                        &mut pred_y,
                        fctx,
                    );
                    mc::predict_with_filters_kern(
                        &pu_ref.data,
                        pu_ref.width,
                        pu_ref.true_width,
                        pu_ref.true_height,
                        mv_to_q4(cpx, mv.1, ss_x(fctx)),
                        mv_to_q4(cpy, mv.0, ss_y(fctx)),
                        chroma_stride,
                        chroma_buf_h,
                        write_chroma_w,
                        write_chroma_h,
                        h_filter,
                        v_filter,
                        &mut pred_u,
                        fctx,
                    );
                    mc::predict_with_filters_kern(
                        &pv_ref.data,
                        pv_ref.width,
                        pv_ref.true_width,
                        pv_ref.true_height,
                        mv_to_q4(cpx, mv.1, ss_x(fctx)),
                        mv_to_q4(cpy, mv.0, ss_y(fctx)),
                        chroma_stride,
                        chroma_buf_h,
                        write_chroma_w,
                        write_chroma_h,
                        h_filter,
                        v_filter,
                        &mut pred_v,
                        fctx,
                    );
                } else {
                    mc::predict_scaled_kern(
                        &py_ref.data,
                        py_ref.width,
                        py_ref.true_width,
                        py_ref.true_height,
                        mv_to_q4(px, mv.1, 0),
                        mv_to_q4(py, mv.0, 0),
                        luma_scale,
                        side,
                        side,
                        write_w,
                        write_h,
                        h_filter,
                        v_filter,
                        &mut pred_y,
                        fctx,
                    );
                    mc::predict_scaled_kern(
                        &pu_ref.data,
                        pu_ref.width,
                        pu_ref.true_width,
                        pu_ref.true_height,
                        mv_to_q4(cpx, mv.1, ss_x(fctx)),
                        mv_to_q4(cpy, mv.0, ss_y(fctx)),
                        luma_scale,
                        chroma_stride,
                        chroma_buf_h,
                        write_chroma_w,
                        write_chroma_h,
                        h_filter,
                        v_filter,
                        &mut pred_u,
                        fctx,
                    );
                    mc::predict_scaled_kern(
                        &pv_ref.data,
                        pv_ref.width,
                        pv_ref.true_width,
                        pv_ref.true_height,
                        mv_to_q4(cpx, mv.1, ss_x(fctx)),
                        mv_to_q4(cpy, mv.0, ss_y(fctx)),
                        luma_scale,
                        chroma_stride,
                        chroma_buf_h,
                        write_chroma_w,
                        write_chroma_h,
                        h_filter,
                        v_filter,
                        &mut pred_v,
                        fctx,
                    );
                }

                // lane-scaledref r1: libaom `allow_warp` (av1/common/reconinter.c:41)
                // opens with `if (av1_is_scaled(sf)) return 0;` -- BOTH local
                // (`wm_params`) and global warp fall back to the translational
                // prediction above when the reference is scaled, even though the
                // motion_mode symbol was still read and warp_params still
                // resolved. Suppress the warp, keep every symbol read.
                if let Some(params) = warp_params.as_ref().filter(|_| luma_scale.is_unscaled()) {
                    // `av1_init_warp_params` is per PLANE: the luma plane's
                    // own block is `write_w`x`write_h` (a PARTITION_HORZ/VERT
                    // strip is not `side`x`side`), so a strip under 8 px in
                    // either direction keeps the translational prediction too.
                    if warp_plane_allowed(write_w, write_h) {
                        crate::warp::warp_affine(
                            params,
                            &py_ref.data,
                            py_ref.true_width as i32,
                            py_ref.true_height as i32,
                            py_ref.width as i32,
                            &mut pred_y,
                            px as i32,
                            py as i32,
                            side as i32,
                            side as i32,
                            side as i32,
                            0,
                            0,
                            fctx,
                        );
                    }
                    // libaom `av1_init_warp_params` (reconinter.c): warp is
                    // per PLANE and bails out at `block_width < 8 ||
                    // block_height < 8`, so a plane whose own block is under
                    // 8x8 keeps the translational prediction built above --
                    // in 420 that is every chroma plane whose own width or
                    // height is under 8 -- `write_chroma_w`/`write_chroma_h`,
                    // NOT the square `chroma_side` (lane-t900 r5: an 8x16 luma
                    // block has `chroma_side == 8` but a 4x8 chroma block, so
                    // the square side warped chroma libaom predicts
                    // translationally; lane-gmaffine r4 for the 8x8 leaf).
                    if warp_plane_allowed(write_chroma_w, write_chroma_h) {
                        crate::warp::warp_affine(
                            params,
                            &pu_ref.data,
                            pu_ref.true_width as i32,
                            pu_ref.true_height as i32,
                            pu_ref.width as i32,
                            &mut pred_u,
                            cpx as i32,
                            cpy as i32,
                            chroma_stride as i32,
                            chroma_buf_h as i32,
                            chroma_stride as i32,
                            ss_x(fctx) as i32,
                            ss_y(fctx) as i32,
                            fctx,
                        );
                        crate::warp::warp_affine(
                            params,
                            &pv_ref.data,
                            pv_ref.true_width as i32,
                            pv_ref.true_height as i32,
                            pv_ref.width as i32,
                            &mut pred_v,
                            cpx as i32,
                            cpy as i32,
                            chroma_stride as i32,
                            chroma_buf_h as i32,
                            chroma_stride as i32,
                            ss_x(fctx) as i32,
                            ss_y(fctx) as i32,
                            fctx,
                        );
                    }
                }
                // Mutually exclusive with warp/OBMC (motion_mode is not read for
                // an interintra block): blend the intra predictor over the MC
                // result, all planes (reconinter.c av1_build_interintra_predictor
                // plane loop).
                if let Some(ii) = interintra_mode {
                    if write_w != write_h {
                        hit!(INTERINTRA_RECT_HITS);
                    }
                    interintra_blend(
                        y,
                        px,
                        py,
                        write_w,
                        write_h,
                        side,
                        0,
                        0,
                        ii,
                        wedge_mask,
                        &mut pred_y,
                        fctx,
                    );
                    interintra_blend(
                        u,
                        cpx,
                        cpy,
                        write_chroma_w,
                        write_chroma_h,
                        chroma_stride,
                        ss_x(fctx),
                        ss_y(fctx),
                        ii,
                        wedge_mask,
                        &mut pred_u,
                        fctx,
                    );
                    interintra_blend(
                        v,
                        cpx,
                        cpy,
                        write_chroma_w,
                        write_chroma_h,
                        chroma_stride,
                        ss_x(fctx),
                        ss_y(fctx),
                        ii,
                        wedge_mask,
                        &mut pred_v,
                        fctx,
                    );
                }
                if let Some(plan) = &obmc {
                    obmc_run(plan, refpix, &mut pred_y, &mut pred_u, &mut pred_v, fctx);
                }
                // lane-inter16ab r2: libaom `build_inter_predictors_sub8x8`
                // (reconinter_template.inc:87-160). A 4:2:0 chroma block under a
                // 16x4 (4x16) luma pair covers TWO luma blocks, so it is built in
                // `b4_w x b4_h` = 8x2 (2x8) pieces, each from its own strip's
                // mv/ref/filters. The bottom (right) piece is this strip's own mv
                // over its own rows, which the whole-block prediction above
                // already wrote there (MC is position-invariant for a fixed mv),
                // so only the FIRST piece has to be rebuilt -- and only when
                // `is_sub8x8_inter` holds. lane-t900 r28: it scales off the
                // PREVIOUS strip's own reference, not this block's.
                if let Some((horz, prev_ref, prev_mv, prev_h, prev_v, prev_scale)) = sub8 {
                    let Some((_, prev_u, prev_v_plane)) = ref_planes_deferred(prev_ref, refpix)
                    else {
                        return;
                    };
                    let (piece_w, piece_h) = if horz {
                        (8usize, 2usize)
                    } else {
                        (2usize, 8usize)
                    };
                    for (src, dst) in [(prev_u, &mut pred_u), (prev_v_plane, &mut pred_v)] {
                        let mut piece = vec![0u16; piece_w * piece_h];
                        mc::predict_maybe_scaled(
                            &src.data,
                            src.width,
                            src.true_width,
                            src.true_height,
                            mv_to_q4(cpx, prev_mv.1, ss_x(fctx)),
                            mv_to_q4(cpy, prev_mv.0, ss_y(fctx)),
                            prev_scale,
                            piece_w,
                            piece_h,
                            prev_h,
                            prev_v,
                            &mut piece,
                            fctx,
                        );
                        for row in 0..piece_h {
                            dst[row * chroma_stride..row * chroma_stride + piece_w]
                                .copy_from_slice(&piece[row * piece_w..(row + 1) * piece_w]);
                        }
                    }
                }
                *out = [pred_y, pred_u, pred_v];
            };
            // OBMC and interintra are mutually exclusive (`motion_mode` is
            // never read for an interintra block), so blending interintra
            // inside `build` and OBMC after it is still the coded order.
            debug_assert!(!(obmc_selected && interintra_mode.is_some()));
            // lane-defer8: OBMC and the sub-8x8 piece now live inside `build`,
            // so nothing about this block keeps its prediction on this thread.
            let (pred_y, pred_u, pred_v): (Vec<u16>, Vec<u16>, Vec<u16>);
            let (sy, su, sv) = if pred_deferred(refpix) {
                push_pred(build);
                (Pred::Cur { plane: 0, stride: side, off: 0 }, Pred::Cur { plane: 1, stride: chroma_stride, off: 0 }, Pred::Cur { plane: 2, stride: chroma_stride, off: 0 })
            } else {
                let mut built: [Vec<u16>; 3] = Default::default();
                build(y, u, v, refpix, fctx, &mut built);
                let [by, bu, bv] = built;
                (pred_y, pred_u, pred_v) = (by, bu, bv);

                // lane-inter16ab r2: libaom `build_inter_predictors_sub8x8`
                // (reconinter_template.inc:87-160). A 4:2:0 chroma block under a
                // 16x4 (4x16) luma pair covers TWO luma blocks, so it is built in
                // `b4_w x b4_h` = 8x2 (2x8) pieces, each from its own strip's
                // mv/ref/filters. The bottom (right) piece is this strip's own mv
                // over its own rows, which the whole-block prediction above
                // already wrote there (MC is position-invariant for a fixed mv),
                // so only the FIRST piece has to be rebuilt -- and only when
                // `is_sub8x8_inter` holds, i.e. the previous strip was itself a
                // single-reference inter block (`prev`), else libaom falls
                // through to the ordinary whole-block predictor.
                (
                    Pred::Inline(&pred_y, side),
                    Pred::Inline(&pred_u, chroma_stride),
                    Pred::Inline(&pred_v, chroma_stride),
                )
            };

            // lane-inter4 r2: a rectangular strip's transform tree starts from
            // its own rect `max_txsize_rect_lookup` entry, not a square one.
            let rect_tu = (write_w, write_h) != (side, side);
            vartx_leaves = if rect_tu {
                read_block_tx_size_rect(
                    dec,
                    cdfs,
                    neighbours,
                    at_mi,
                    (write_w, write_h),
                    (mi_cols as usize, mi_rows as usize),
                    skip,
                    fctx,
                )?
            } else {
                read_block_tx_size(
                    dec,
                    cdfs,
                    neighbours,
                    at_mi,
                    side,
                    (mi_cols as usize, mi_rows as usize),
                    true,
                    skip,
                    fctx,
                )?
                .1
            };
            if skip {
                push_mc_rect(
                    y,
                    u,
                    v,
                    0,
                    px,
                    py,
                    side,
                    write_w,
                    write_h,
                    sy,
                    &ZERO_RESIDUAL[..side * side],
                    fctx,
                );
                if has_chroma {
                    push_mc_rect(
                        y,
                        u,
                        v,
                        1,
                        cpx,
                        cpy,
                        chroma_stride,
                        write_chroma_w,
                        write_chroma_h,
                        su,
                        &ZERO_RESIDUAL[..chroma_stride * chroma_buf_h],
                        fctx,
                    );
                    push_mc_rect(
                        y,
                        u,
                        v,
                        2,
                        cpx,
                        cpy,
                        chroma_stride,
                        write_chroma_w,
                        write_chroma_h,
                        sv,
                        &ZERO_RESIDUAL[..chroma_stride * chroma_buf_h],
                        fctx,
                    );
                }
                luma_grid = Grid::Zero(side * side);
                u_grid = Grid::Zero(chroma_stride * chroma_buf_h);
                v_grid = Grid::Zero(chroma_stride * chroma_buf_h);
            } else {
                // lane-inter4 r2: a rect strip's coefficient context spans its
                // OWN width above and height left -- reading `side` cells both
                // ways indexes past the tile's last mi row for a bottom strip.
                let around = if rect_tu {
                    // lane-av1422mi24: same 4:2:2 rect-strip chroma gather
                    // rule as the single-TU path above -- luma per-mi,
                    // chroma every-second above cell.
                    let mut a = neighbours.around_mi_rect(at_mi, write_w, write_h);
                    if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                        let c = neighbours.around_mi_422_chroma(at_mi, write_w, write_h);
                        a[1] = c[1];
                        a[2] = c[2];
                    }
                    a
                } else if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                    // lane-av1422mi24: same 4:2:2 square-block chroma gather
                    // rule as the single-TU path above -- luma per-mi, chroma
                    // every-second above cell.
                    let mut a = neighbours.around_mi(at, side);
                    let c = neighbours.around_mi_422_chroma(at, write_w, write_h);
                    a[1] = c[1];
                    a[2] = c[2];
                    a
                } else {
                    neighbours.around_mi(at, side)
                };
                // lane-inter16ab r2: the pair's chroma transform sits at the
                // EVEN strip's mi origin and spans both strips, so its
                // above/left coefficient context is the pair's, not this
                // strip's row.
                // lane-av1422ctxgather: at 4:2:2 (ss 1,0) a HORZ strip is
                // its own chroma reference (`is_chroma_reference`'s
                // `!(subsampling_y)` row clause) -- pair_mi is the strip
                // itself and the context reads the strip's own 8x4, ONE
                // luma-mi row tall, with the 4:2:2 above sampling
                // ([`Neighbours::around_mi_422_chroma`]) -- and the same
                // reroute catches 4:2:2 VERT: its (8, 16) pair span's
                // above side covers one chroma column per two luma mi
                // columns, the identical double-count.
                //
                // lane-av1444rect (H1, MEASURED): the (16, 8) / (8, 16) pair
                // extent was reached at ss (0,0) too, where no pair exists.
                // At 4:4:4 `is_chroma_reference` (av1_common_int.h:1454)
                // reduces BOTH parity clauses to `!subsampling_y` /
                // `!subsampling_x`, so every block is its own chroma
                // reference: a HORZ_4 16x4 strip codes its OWN 16x4 chroma
                // and the setter above already gave it `pair_mi == at_mi`.
                // Its coefficient context is therefore the strip's own,
                // which is exactly the `around` gathered one screen above
                // as `around_mi_rect(at_mi, write_w, write_h)`. The pair
                // gather read one EXTRA luma mi row of LEFT chroma context
                // (`ph = 8` covers left[8] and left[9], a 4-px strip only
                // left[8]), so the U unit's `get_txb_ctx` saw a neighbour
                // libaom does not have and forked the arithmetic coder: the
                // 6441-byte 4:4:4 lossy cq-20 stream, frame 1, mi(8,16)
                // read our `txb_skip_ctx` 1 (0 above + 1 left) where the
                // oracle read offset-7 row 7 (ctx_base 0).
                let around_c = match strip_chroma {
                    // lane-av1444rect: the arm `is_chroma_reference` makes the
                    // ONLY correct choice at ss_x 0 -- no pair exists, the
                    // extent is the strip's own. Counted so a 4:4:4 1:4-strip
                    // gate can prove it exercised this route (a 4:2:0 stream
                    // of the same content reads 0).
                    Some(_) if ss_x(fctx) == 0 => {
                        hit!(RECT4_INTER_OWN_CHROMA444_HITS);
                        around
                    }
                    Some(s) if s.has_chroma => {
                        let (pw, ph) = if s.horz {
                            if ss_y(fctx) == 0 { (16, 4) } else { (16, 8) }
                        } else {
                            (8, 16)
                        };
                        if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                            neighbours.around_mi_422_chroma(s.pair_mi, pw, ph)
                        } else {
                            neighbours.around_mi_rect(s.pair_mi, pw, ph)
                        }
                    }
                    _ => around,
                };
                let luma_tx_type;
                // lane-sb128c r1: a 128x128 inter block's chroma is read
                // INSIDE the luma leaf loop (each 64x64 mu chunk's own units:
                // one per chunk at 4:2:0, FOUR TX_32X32 at ss 0/0); these
                // collect them for the block-level neighbour write-back.
                let mut u_units: Vec<i32> = Vec::new();
                let mut v_units: Vec<i32> = Vec::new();
                // lane-av1-444-128: every luma leaf's coded tx_type, so the
                // chunk tail's chroma units can each inherit the type of the
                // leaf covering their own quadrant (`av1_get_tx_type`,
                // blockd.h:1291, scales the CHROMA 4x4 position back to luma).
                let mut leaf_tx_types: Vec<(usize, usize, usize, usize, TxType)> = Vec::new();
                if let Some(leaves) = vartx_leaves.as_ref() {
                    // spec 5.11.17's transform tree, leaf by leaf in the order
                    // `read_var_tx_size` coded it: each unit reads its own
                    // coefficients against the neighbour magnitudes the
                    // earlier units already wrote, and reconstructs over its
                    // own slice of the block's motion-compensated predictor.
                    let reduced = fctx.reduced_tx_set_inter.with(std::cell::Cell::get);
                    let mut first_tx_type = TxType::DctDct;
                    // lane-av1h5: libaom's `decode_token_recon_block` walks the
                    // block in 64x64 "mu" CHUNKS and codes EVERY PLANE of a
                    // chunk before the next one starts, so at 128 the token
                    // order is CHUNK-major. The transform tree hands us a
                    // BLOCK-wide raster (32 mi across here), whose chunk
                    // members are not contiguous: `leaves` 0..15 are chunk
                    // (0,0) row 0, but chunk (0,0) rows 1..15 are leaves
                    // 32..47, 64..79, ... Walking it as given closed the
                    // "chunk" every 16 leaves and coded each chunk's chroma
                    // sixteen times -- 64 chroma walks per 128 root where
                    // libaom codes 4 (measured against aomdec's `EC_DBGCTX`:
                    // 24576 chroma units per inter frame vs 1536). Below 128 a
                    // block IS one chunk and the two orders coincide.
                    let (order, _mu_reordered) = mu_chunk_order(leaves, side);
                    for (pos, &idx) in order.iter().enumerate() {
                        let (row, col, tw, th) = leaves[idx];
                        let tu_mi = (at_mi.0 + row, at_mi.1 + col);
                        let (tu_px, tu_py) = (px + col * MI, py + row * MI);
                        let (tu_grid, tu_tx_type) = if tw == th {
                            let tx_px = tw;
                            let tu_pred = sy.offset(row * MI * side + col * MI);
                            let scan = default_scan(tx_px.min(32));
                            let tu_skip_ctx = neighbours.luma_skip_ctx(tu_mi, tx_px / MI);
                            read_inter_plane(
                                dec,
                                cdfs,
                                inter_txbset_for(tx_px, reduced),
                                scan,
                                0,
                                neighbours.around_mi(tu_mi, tx_px)[0],
                                mode_for_tx,
                                y,
                                u,
                                v,
                                tu_px,
                                tu_py,
                                tx_px,
                                base_q_idx,
                                tu_pred,
                                None,
                                Some(tu_skip_ctx),
                                fctx,
                            )?
                        } else {
                            // lane-r14 r3: a 1:4 strip's `sub_tx_size_map`
                            // sub-transform is RECTANGULAR (TX_64X16 ->
                            // TX_32X16, TX_16X64 -> TX_16X32), so this leaf is
                            // read with the rect coefficient machinery, at its
                            // own `txb_skip_ctx`/neighbour spans -- the unit is
                            // smaller than its block, so `get_txb_ctx_general`'s
                            // neighbour table applies, not the lone-TU 0.
                            vartx_rect_leaf_hit(tw, th);
                            let tu_skip_ctx =
                                neighbours.luma_skip_ctx_rect(tu_mi, tw / MI, th / MI);
                            let start = row * MI * side + col * MI;
                            read_inter_plane_rect(
                                dec,
                                cdfs,
                                rect_inter_luma_set(tw, th, fctx)?,
                                (tw, th),
                                side,
                                0,
                                neighbours.around_mi_rect(tu_mi, tw, th)[0],
                                mode_for_tx,
                                y,
                                u,
                                v,
                                tu_px,
                                tu_py,
                                sy.offset(start),
                                None,
                                Some(tu_skip_ctx),
                                fctx,
                            )?
                        };
                        if idx == 0 {
                            first_tx_type = tu_tx_type;
                        }
                        // lane-av1444chr: EVERY luma leaf's coded type, at
                        // every block size -- the chroma units below resolve
                        // their own quadrant's leaf, and at 64x64 that is
                        // this very loop's leaves (`av1_get_tx_type`,
                        // blockd.h:1287-1297, scales the CHROMA 4x4 position
                        // back to luma). It used to be collected only for
                        // `side > 64`, so the 64x64 four-unit arm below had
                        // nothing to resolve from and handed all four units
                        // the block-level (top-left leaf) type. A leaf that
                        // coded NOTHING is `DctDct` here exactly as libaom's
                        // `decodetxb.c:199-203` stamps `DCT_DCT` into that
                        // leaf's map cell on the `all_zero` path.
                        leaf_tx_types.push((row, col, tw, th, tu_tx_type));
                        neighbours.record_mi_luma_rect(tu_mi, tw, th, &tu_grid);
                        // lane-sb128c r1: libaom's `decode_token_recon_block`
                        // walks 64x64 "mu" chunks (`mu_blocks_wide/high =
                        // mi_size_wide[BLOCK_64X64]`) and codes EVERY PLANE of
                        // a chunk before the next one, so a 128x128 block
                        // interleaves Y,U,V per quadrant -- the same order the
                        // intra 128 path reads (lane-sb128b r3, measured
                        // against aomdec's `EC_TRACE_COEFF`). Below 128 a block
                        // is a single chunk and this never fires.
                        if side > 64 {
                            let chunk = (row / 16, col / 16);
                            let done = pos + 1 == order.len()
                                || (leaves[order[pos + 1]].0 / 16, leaves[order[pos + 1]].1 / 16)
                                    != chunk;
                            if done {
                                let (cr, cc) = chunk;
                                // `av1_get_max_uv_txsize(BLOCK_128X128)` =
                                // `av1_get_adjusted_tx_size(TX_64X64)` =
                                // TX_32X32. The mu walk
                                // (`decode_token_recon_block`, decodeframe.c)
                                // then enumerates, per plane, EVERY TX_32X32
                                // unit of the chunk's chroma in raster order:
                                // at ss 0/0 the chunk's chroma is 64x64 and
                                // that is FOUR units; at 4:2:0 it is 32x32
                                // and the loop degenerates to the one unit
                                // this arm has always coded. `chroma_side`
                                // already carries the subsampling.
                                let cu_tx = 32usize;
                                // lane-av1-422bigblock: a 64x64 mu chunk's
                                // chroma plane block is PER-AXIS
                                // (64 >> ss_x) x (64 >> ss_y) -- 32x64 at
                                // ss (1,0), i.e. TWO stacked TX_32X32 units
                                // per plane (`av1_get_max_uv_txsize` caps the
                                // unit at 32 on each axis). The old square-cut
                                // model walked `chroma_side`-square chunks:
                                // twice the units the oracle codes at 4:2:2,
                                // prediction windows indexed at the square
                                // `chroma_side` instead of the prediction
                                // buffer's own `chroma_stride`, and chunk
                                // columns at 64.. of a 64-wide chroma plane.
                                let chunk_chroma_w = (64usize) >> ss_x(fctx);
                                let chunk_chroma_h = (64usize) >> ss_y(fctx);
                                let units_w = chunk_chroma_w / cu_tx;
                                let units_h = chunk_chroma_h / cu_tx;
                                // The unit's own footprint on the LUMA mi
                                // grid, per axis (32 << ss) px: 64x64 (16x16
                                // mi) at 4:2:0, 32x32 (8x8 mi) at 4:4:4,
                                // 64x32 (16x8 mi) at 4:2:2. The context
                                // gather and `record_mi_chroma`'s stamp span
                                // exactly the unit's own cells -- `get_txb_ctx`
                                // reads the 8 context cells of the TX_32X32
                                // itself, and `av1_set_entropy_contexts` memsets
                                // the same 8, so the unit over never sees this
                                // unit's state and the unit below always does.
                                // At ss (1, 0) those 8 are CHROMA cells, so the
                                // gather routes through the 4:2:2 pair rule
                                // (lane-av1muchunk422sym, see the walk below).
                                let unit_luma_w = cu_tx << ss_x(fctx);
                                let unit_luma_h = cu_tx << ss_y(fctx);
                                let cu_scan = default_scan(cu_tx);
                                if lossless(fctx) {
                                    // lane-lossless2: TX_4X4 units over this
                                    // chunk's chroma, not one TX_32X32 unit.
                                    read_inter_chroma_lossless(
                                        y,
                                        u,
                                        v,
                                        dec,
                                        cdfs,
                                        neighbours,
                                        su,
                                        sv,
                                        at_mi,
                                        (cpx, cpy),
                                        (cc * chunk_chroma_w, cr * chunk_chroma_h),
                                        (chunk_chroma_w, chunk_chroma_h),
                                        (write_chroma_w, write_chroma_h),
                                        // lane-av1422lpf site 5 (the compound
                                        // twin of the single-reference fix at
                                        // 41572): the prediction buffer's own
                                        // `chroma_stride`, never the enclosing
                                        // square `chroma_side`. Identical
                                        // number at 4:2:0 and 4:4:4.
                                        chroma_stride,
                                        mode_for_tx,
                                        base_q_idx,
                                        &mut u_units,
                                        &mut v_units,
                                        fctx,
                                    )?;
                                    mu_chroma = true;
                                    continue;
                                }
                                for plane_idx in 1..3 {
                                    let src = if plane_idx == 1 { su } else { sv };
                                    // Plane-major per chunk (libaom's plane
                                    // loop is outermost inside the mu walk);
                                    // within a plane the units walk raster.
                                    for ur in 0..units_h {
                                        for uc in 0..units_w {
                                            let unit_mi = (
                                                at_mi.0 + cr * 16 + ur * (unit_luma_h / MI),
                                                at_mi.1 + cc * 16 + uc * (unit_luma_w / MI),
                                            );
                                            // Per unit, AFTER the earlier
                                            // units' stamps: this unit's
                                            // coefficient context must see
                                            // them.
                                            //
                                            // lane-av1muchunk422sym: at ss
                                            // (1, 0) one CHROMA column spans
                                            // two luma mi columns, so the plain
                                            // rect gather sums this unit's
                                            // above dc sign TWICE where
                                            // libaom's `get_txb_ctx_general`
                                            // reads `txb_w_unit` CHROMA cells --
                                            // one per column. `dc_sign_ctx`
                                            // only sees the vote's signum, so
                                            // the doubled vote flips the row
                                            // exactly when above and left
                                            // cancel; `around_mi_422_chroma`
                                            // samples every second above cell,
                                            // which IS libaom's sum. Same guard
                                            // as the intra twin (13169) and the
                                            // block-8 arm (19810).
                                            let cu_around = if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                                                neighbours.around_mi_422_chroma(
                                                    unit_mi,
                                                    unit_luma_w,
                                                    unit_luma_h,
                                                )
                                            } else {
                                                neighbours.around_mi_rect(
                                                    unit_mi,
                                                    unit_luma_w,
                                                    unit_luma_h,
                                                )
                                            };
                                            let (cu_x, cu_y) = (
                                                cpx + cc * chunk_chroma_w + uc * cu_tx,
                                                cpy + cr * chunk_chroma_h + ur * cu_tx,
                                            );
                                            // `av1_get_tx_type` (blockd.h:
                                            // 1291): an inter block's chroma
                                            // unit never codes a tx_type; it
                                            // inherits the coded type of the
                                            // luma leaf covering its own
                                            // quadrant (all of this chunk's
                                            // leaves are read by now).
                                            let unit_rel_mi =
                                                (unit_mi.0 - at_mi.0, unit_mi.1 - at_mi.1);
                                            let covering =
                                                covering_leaf_tx_type(&leaf_tx_types, unit_rel_mi);
                                            // The prediction buffer is
                                            // `chroma_stride`-strided (the
                                            // plane block's own width at
                                            // 4:2:2), not the square
                                            // `chroma_side`.
                                            let cu_pred = src.offset(
                                                (cr * chunk_chroma_h + ur * cu_tx) * chroma_stride
                                                    + cc * chunk_chroma_w
                                                    + uc * cu_tx,
                                            );
                                            let cu_grid = read_inter_plane(
                                                dec,
                                                cdfs,
                                                chroma_set,
                                                cu_scan,
                                                plane_idx,
                                                cu_around[plane_idx],
                                                mode_for_tx,
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
                                            // Immediately, so the NEXT unit
                                            // reads this one's coefficient
                                            // context.
                                            neighbours.record_mi_chroma(
                                                unit_mi,
                                                unit_luma_w,
                                                unit_luma_h,
                                                plane_idx,
                                                &cu_grid,
                                            );
                                            hit!(CHROMA_SPLIT_TX_HITS);
                                            mu_chroma = true;
                                            // lane-av1muchunk422b: the allocation of this
                                            // mu-chunk walk, counted per TX_32X32 UNIT.
                                            // The compound and single-reference arms share
                                            // this source text, so `is_compound` routes the
                                            // bump: `MU_CHUNK_COMPOUND_UNITS` names site 2
                                            // and the single-reference twin (site 5) stays
                                            // out of it.
                                            if is_compound {
                                                hit!(MU_CHUNK_COMPOUND_UNITS);
                                            }
                                            let dst = mu_units(
                                                if plane_idx == 1 {
                                                    &mut u_units
                                                } else {
                                                    &mut v_units
                                                },
                                                chroma_side * chroma_side,
                                            );
                                            for rr in 0..cu_tx {
                                                // The ASSEMBLED grid stays at
                                                // the square `chroma_side`
                                                // stride the block tail's
                                                // `mu_chroma_units` re-stamp
                                                // indexes; the unit's true
                                                // per-axis position is what
                                                // changes.
                                                let start = (cr * chunk_chroma_h + ur * cu_tx + rr)
                                                    * chroma_side
                                                    + cc * chunk_chroma_w
                                                    + uc * cu_tx;
                                                dst[start..start + cu_tx].copy_from_slice(
                                                    &cu_grid[rr * cu_tx..][..cu_tx],
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // The block-level `luma_tx_type` stays the first
                    // (top-left) luma unit's coded type; the PER-UNIT chroma
                    // inheritance in the chunk tail above resolves each unit's
                    // own covering leaf instead (`av1_get_tx_type`,
                    // blockd.h:1291).
                    luma_tx_type = first_tx_type;
                    luma_grid = Grid::Zero(side * side);
                } else if rect_tu {
                    // The whole-block rectangular transform unit
                    // (`txfm_partition` said 0 at depth 0).
                    let (grid, tx_type) = read_inter_plane_rect(
                        dec,
                        cdfs,
                        rect_inter_luma_set(write_w, write_h, fctx)?,
                        (write_w, write_h),
                        side,
                        0,
                        around[0],
                        mode_for_tx,
                        y,
                        u,
                        v,
                        px,
                        py,
                        sy,
                        None,
                        None,
                        fctx,
                    )?;
                    luma_tx_type = tx_type;
                    luma_grid = Grid::Own(embed_rect_grid(&grid, write_w, write_h, side));
                } else {
                    {
                        let coded = read_inter_plane(
                            dec,
                            cdfs,
                            luma_set_inter,
                            scan_luma,
                            0,
                            around[0],
                            mode_for_tx,
                            y,
                            u,
                            v,
                            px,
                            py,
                            side,
                            base_q_idx,
                            sy,
                            None,
                            None,
                            fctx,
                        )?;
                        luma_grid = coded.0;
                        luma_tx_type = coded.1;
                    }
                }
                if !has_chroma {
                    // `is_chroma_reference` is false for this strip: libaom's
                    // plane loop (decodeframe.c
                    // `predict_and_reconstruct_intra_block`/
                    // `reconstruct_inter_block`'s `for (plane...)` guarded by
                    // `xd->is_chroma_ref`) codes NO chroma transform here, so
                    // neither symbol nor context exists.
                    u_grid = Grid::Zero(chroma_stride * chroma_buf_h);
                    v_grid = Grid::Zero(chroma_stride * chroma_buf_h);
                } else if side > 64 {
                    // Already read inside the mu-chunk loop above -- at the 128
                    // root a rect (128x64/64x128) block's chroma is per mu
                    // chunk too, so this test comes BEFORE the `rect_tu` one.
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
                } else if lossless(fctx) {
                    // lane-lossless2: TX_4X4 chroma units, plane-major
                    // (libaom `read_tx_size` + `decode_token_recon_block`).
                    let (mut uo, mut vo) = (Vec::new(), Vec::new());
                    read_inter_chroma_lossless(
                        y,
                        u,
                        v,
                        dec,
                        cdfs,
                        neighbours,
                        su,
                        sv,
                        at_mi,
                        (cpx, cpy),
                        (0, 0),
                        (write_chroma_w, write_chroma_h),
                        (write_chroma_w, write_chroma_h),
                        chroma_stride,
                        mode_for_tx,
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
                    // Every unit stamped its own neighbour context above; the
                    // whole-block record must not overwrite them with the
                    // assembled grid (class `override-slot-on-one-arm`).
                    mu_chroma = true;
                } else if rect_tu || (write_chroma_w, write_chroma_h) != (chroma_side, chroma_side)
                {
                    // One chroma read for the whole strip at 4:2:0, two
                    // 32-capped units at 4:4:4 -- see the helper.
                    // lane-av1-422j: same rule as the compound arm above -- a
                    // 4:2:2 square block's chroma plane is the
                    // (side/2 x side) rect, so the rect reader takes it
                    // instead of pairing a `chroma_side`-square unit with the
                    // 4:2:0 scan.
                    (u_grid, v_grid, rect_chroma_units) = read_inter_rect_chroma(
                        y,
                        u,
                        v,
                        dec,
                        cdfs,
                        neighbours,
                        around_c,
                        su,
                        sv,
                        at_mi,
                        (cpx, cpy),
                        (write_chroma_w, write_chroma_h),
                        chroma_stride,
                        mode_for_tx,
                        &leaf_tx_types,
                        luma_tx_type,
                        fctx,
                    )?;
                } else if ss_x(fctx) == 0 && ss_y(fctx) == 0 && chroma_side > chroma_tx {
                    // lane-av1-444: at ss 0/0 this block's chroma plane is
                    // `chroma_side` square while `av1_get_max_uv_txsize` caps
                    // the units at TX_32X32 -- each plane codes FOUR TX_32X32
                    // units, plane-major inside the block's one 64x64 mu chunk
                    // (libaom `decode_token_recon_block`'s plane loop). Each
                    // unit's `txb_skip_ctx` carries the +10
                    // `get_txb_ctx_general` offset (plane block larger than the
                    // transform); our `txb_skip_chroma_32` holds those rows at
                    // +3, so `luma_skip_ctx: Some(3)` (lane-sb128b r3's
                    // convention, as the 128 mu path above).
                    let cu_tx = chroma_tx;
                    let mu_units_n = chroma_side / cu_tx;
                    let mut u_acc: Vec<i32> = Vec::new();
                    let mut v_acc: Vec<i32> = Vec::new();
                    for plane_idx in 1..3 {
                        let (src, acc) = if plane_idx == 1 {
                            (su, &mut u_acc)
                        } else {
                            (sv, &mut v_acc)
                        };
                        for cr in 0..mu_units_n {
                            for cc in 0..mu_units_n {
                                let cu_mi = (rmi + cr * (cu_tx / MI), cmi + cc * (cu_tx / MI));
                                let cu_around = neighbours.around_mi(cu_mi, cu_tx);
                                // `av1_get_tx_type` (blockd.h:1287-1297): an
                                // inter block's chroma unit never codes a
                                // `tx_type`; it reads the map cell at its OWN
                                // position scaled back to luma
                                // (`blk_row << ss_y`, `blk_col << ss_x`),
                                // which is the coded type of the luma leaf
                                // covering this unit's top-left mi cell --
                                // `DctDct` when that leaf coded nothing
                                // (`decodetxb.c:199-203`). The block-level
                                // `luma_tx_type` is only the TOP-LEFT leaf's
                                // type, so handing it to all four units
                                // mistyped every unit sitting over a skipped
                                // or differently-typed luma quadrant.
                                let cu_rel_mi = (cr * (cu_tx / MI), cc * (cu_tx / MI));
                                let cu_tx_type = covering_leaf_tx_type(&leaf_tx_types, cu_rel_mi)
                                    .unwrap_or(luma_tx_type);
                                if covering_leaf_tx_type(&leaf_tx_types, cu_rel_mi).is_some() {
                                    hit!(CHROMA_QUAD_LEAF_TX_HITS);
                                    hit!(CHROMA_QUAD_LEAF_TX_SINGLEREF_HITS);
                                    // lane-av1f9singleland: this arm's own
                                    // non-vacuity number -- the shared
                                    // DIFFERS counter below cannot say which
                                    // arm changed an answer.
                                    if cu_tx_type != luma_tx_type {
                                        hit!(CHROMA_QUAD_LEAF_TX_DIFF_HITS);
                                        hit!(CHROMA_QUAD_LEAF_TX_SINGLEREF_DIFF_HITS);
                                    }
                                }
                                let cu_grid = read_inter_plane(
                                    dec,
                                    cdfs,
                                    chroma_set,
                                    scan_chroma,
                                    plane_idx,
                                    cu_around[plane_idx],
                                    mode_for_tx,
                                    y,
                                    u,
                                    v,
                                    cpx + cc * cu_tx,
                                    cpy + cr * cu_tx,
                                    cu_tx,
                                    base_q_idx,
                                    src.offset(cr * cu_tx * chroma_side + cc * cu_tx),
                                    Some(cu_tx_type),
                                    Some(3),
                                    fctx,
                                )?
                                .0;
                                // Immediately, so the next unit's context sees
                                // this one's coefficients.
                                neighbours
                                    .record_mi_chroma(cu_mi, cu_tx, cu_tx, plane_idx, &cu_grid);
                                hit!(CHROMA_SPLIT_TX_HITS);
                                // lane-av1-444 c8: the unit's levels must ALSO
                                // land in the block grid -- the block-tail mu
                                // re-stamp slices the per-unit states back out
                                // of it. This arm used to hand it a ZERO grid,
                                // so the re-stamp (and `record_rect_mi`'s
                                // whole-block write under it) painted the whole
                                // block's chroma context cells 0 AFTER the
                                // correct per-unit stamps above: the next block
                                // read `txb_skip_ctx` base 0 where aom gathers
                                // 1 (f52 mi(52,16) U all_zero, ctx 0 vs 8).
                                let dst = mu_units(acc, chroma_side * chroma_side);
                                for rr in 0..cu_tx {
                                    let start = (cr * cu_tx + rr) * chroma_side + cc * cu_tx;
                                    dst[start..start + cu_tx]
                                        .copy_from_slice(&cu_grid[rr * cu_tx..][..cu_tx]);
                                }
                            }
                        }
                    }
                    mu_chroma = true;
                    u_grid = Grid::Own(u_acc);
                    v_grid = Grid::Own(v_acc);
                } else {
                    u_grid = read_inter_plane(
                        dec,
                        cdfs,
                        chroma_set,
                        scan_chroma,
                        1,
                        around[1],
                        mode_for_tx,
                        y,
                        u,
                        v,
                        cpx,
                        cpy,
                        chroma_side,
                        base_q_idx,
                        su,
                        Some(luma_tx_type),
                        None,
                        fctx,
                    )?
                    .0;
                    v_grid = read_inter_plane(
                        dec,
                        cdfs,
                        chroma_set,
                        scan_chroma,
                        2,
                        around[2],
                        mode_for_tx,
                        y,
                        u,
                        v,
                        cpx,
                        cpy,
                        chroma_side,
                        base_q_idx,
                        sv,
                        Some(luma_tx_type),
                        None,
                        fctx,
                    )?
                    .0;
                }
            }
        }
    } else {
        let Some(arm) = decode_inter_block_intra_arm(
            dec,
            cdfs,
            neighbours,
            grid,
            at,
            side,
            mi_cols,
            mi_rows,
            y,
            u,
            v,
            base_q_idx,
            luma_set_intra,
            luma_tx,
            scan_luma,
            write_w,
            write_h,
            allow_screen_content_tools,
            fctx,
            at_mi,
            px,
            py,
            r,
            c,
            rmi,
            cmi,
            cpx,
            cpy,
            chroma_side,
            chroma_stride,
            chroma_buf_h,
            chroma_set,
            chroma_tx,
            scan_chroma,
            strip_chroma,
            skip,
            skip_mode,
            mu_chroma,
        )?
        else {
            return Ok(());
        };
        (
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
        ) = arm;
    }
    // lane-comppin r9: end-of-block checkpoint -- everything between
    // post_assign_mv and here (interintra, motion_mode, interp_filter, then
    // every plane's coeffs) is unproven; a range mismatch here vs aomdec's
    // equivalent post-content point (added at the matching call site)
    // narrows the ladder to this block's own tail instead of the next
    // block's partition read.
    if crate::envflags::env_flag!("EC_AV1_TELL") {
        eprintln!(
            "TELL mi_row={} mi_col={} label=block_end tell={} range={}",
            rmi,
            cmi,
            dec.debug_bitpos(),
            dec.debug_state().0
        );
    }
    // lane-inter16ab r2: the pair's chroma coefficient context spans BOTH
    // strips (mirroring the intra `decode_rect4_16` path), and the even
    // strip's own record wrote empty chroma state over those cells -- this
    // rewrite is what libaom's single `av1_set_contexts` call for the 8x4
    // (4x8) chroma unit leaves behind.
    // lane-av1-444interband: 4:2:0 ONLY. At ss 0/0 every strip IS its own
    // chroma reference (`is_chroma_reference`, no mi parity test), its own
    // `record_rect_mi` above already stamps exactly the strip's 16x4 (4x16)
    // chroma span -- libaom `av1_set_entropy_contexts`, or
    // `av1_reset_entropy_context` for a skipped strip (decodeframe.c:1180)
    // -- and the pair-form 16x8 (8x16) span anchored at `pair_mi` (== the
    // strip's own mi at ss 0/0) smeared the strip's state over one extra
    // row/col of cells: a skipped strip wrote zero over `left[r+1][1..2]`,
    // erasing the 16x16-intra block to its left's U/V stamps, and the strip
    // below read its chroma `txb_skip` off the wrong CDF row (t6 frame 1,
    // U unit (2,8): our base 0 vs aom's base 1, `left[2][1]` 0 vs 23 --
    // `INTER16_CHROMA_PAIR_HITS` fired 31x on a stream that has no pairs).
    let pair_chroma = strip_chroma
        .filter(|s| s.has_chroma)
        .filter(|_| ss_x(fctx) == 1 && ss_y(fctx) == 1)
        .map(|s| (s, neighbour_state(&u_grid), neighbour_state(&v_grid)));
    // lane-vert46 r1: libaom stamps the chroma entropy context PER TRANSFORM
    // UNIT (`av1_set_entropy_contexts` inside `decode_reconstruct_tx`'s mu-chunk
    // loop). Above 64 a block's chroma is two or four 32x32 units, so the
    // whole-block re-stamp in the record below overwrote every unit's own DC
    // sign and level with the TOP-LEFT unit's -- the next block then read a
    // stale `dc_sign_ctx` (192x256 --sb-size=128 stream: the 64x128 block at
    // the partial right superblock read row 1 where libaom reads row 2). The
    // units are collected here and re-stamped after that record, in mu-chunk
    // order so a later unit still wins over an earlier one.
    let mut mu_chroma_units: Vec<((usize, usize), usize, Grid, usize, usize)> = if mu_chroma {
        // lane-lossless2: the unit is TX_32X32 (one per 64x64 mu chunk)
        // normally and TX_4X4 on a lossless frame, where the whole chroma
        // plane block is a raster of them.
        let cu = if lossless(fctx) { 4usize } else { 32usize };
        let mut units = Vec::new();
        if lossless(fctx) {
            // lane-av1-llinter: a TX_4X4 chroma unit covers `4 << ss` LUMA
            // pixels (8, x2-per-unit, at 4:2:0; 4, x1, at 4:4:4) -- the
            // composed plane grid then holds a full cu-row per chroma row and
            // each unit's own origin sits `(cu << ss) / MI` mi cells out.
            // The hardcoded 4:2:0 /2-and-x2 math stamped half the plane with
            // double spans at 4:4:4 and corrupted the next block's
            // `txb_skip_ctx` (dodge2 frame 1, U unit mi (4,16) read row 11
            // where the oracle reads row 10).
            let (rows, cols) = (
                (write_h >> ss_y(fctx)).div_ceil(cu),
                (write_w >> ss_x(fctx)).div_ceil(cu),
            );
            for cr in 0..rows {
                for cc in 0..cols {
                    let cu_mi = (
                        at.0 + cr * ((cu << ss_y(fctx)) / MI),
                        at.1 + cc * ((cu << ss_x(fctx)) / MI),
                    );
                    for (plane, grid) in [(1usize, &u_grid), (2usize, &v_grid)] {
                        let mut unit = Vec::with_capacity(cu * cu);
                        for rr in 0..cu {
                            // lane-av1422lpf site 2: the composed grid this
                            // replays is the block's CHROMA PLANE BLOCK at
                            // `chroma_stride` -- the same per-axis stride
                            // site 1 sized `u_out`/`v_out` at and the same
                            // `read_inter_chroma_lossless` wrote them at
                            // (`src.offset(..)` + `(oy + row) * stride + ox`).
                            // `chroma_side` is the ENCLOSING SQUARE
                            // (`max(side >> ss_x, side >> ss_y)`), which is
                            // the plane block's own width only when
                            // `ss_x == ss_y`: at 4:2:2 it is the full luma
                            // side, twice a strip block's plane-block width,
                            // and the last unit row ran off the end
                            // (`range end index 68 out of range for slice of
                            // length 64`, W_intrabc frame 1).
                            //
                            // At 4:2:0 (`chroma_stride == chroma_side ==
                            // side >> 1`) and 4:4:4 (`side`) the two are the
                            // same number and this line is unchanged.
                            let start = (cr * cu + rr) * chroma_stride + cc * cu;
                            unit.extend_from_slice(&grid[start..start + cu]);
                        }
                        units.push((
                            cu_mi,
                            plane,
                            Grid::Own(unit),
                            cu << ss_x(fctx),
                            cu << ss_y(fctx),
                        ));
                    }
                }
            }
        } else {
            // lane-av1-444 c8: a 64x64 mu chunk's chroma plane block is
            // (64 >> ss) square -- ONE TX_32X32 unit per chunk at 4:2:0 but
            // FOUR at ss 0/0 (`av1_get_max_uv_txsize` caps the unit at 32
            // either way). Enumerate every unit at its own mi origin, its
            // own (32 << ss)-px entropy-cell span, and its own 32x32
            // quadrant of the assembled plane grid; stamping one quadrant's
            // state over the whole chunk desynced the next block's
            // `txb_skip_ctx` (f52 mi(52,16) U read base 0, aom 1).
            let (ur, uc) = (
                ((64usize) >> ss_y(fctx)) / cu,
                ((64usize) >> ss_x(fctx)) / cu,
            );
            let (rows, cols) = ((write_h / 64).max(1), (write_w / 64).max(1));
            for cr in 0..rows {
                for cc in 0..cols {
                    for kr in 0..ur {
                        for kc in 0..uc {
                            let cu_mi = (
                                at.0 + (cr * 64 + kr * (cu << ss_y(fctx))) / MI,
                                at.1 + (cc * 64 + kc * (cu << ss_x(fctx))) / MI,
                            );
                            let cy = cr * ((64usize) >> ss_y(fctx)) + kr * cu;
                            let cx = cc * ((64usize) >> ss_x(fctx)) + kc * cu;
                            for (plane, grid) in [(1usize, &u_grid), (2usize, &v_grid)] {
                                let mut unit = Vec::with_capacity(cu * cu);
                                for rr in 0..cu {
                                    let start = (cy + rr) * chroma_side + cx;
                                    unit.extend_from_slice(&grid[start..start + cu]);
                                }
                                units.push((
                                    cu_mi,
                                    plane,
                                    Grid::Own(unit),
                                    cu << ss_x(fctx),
                                    cu << ss_y(fctx),
                                ));
                            }
                        }
                    }
                }
            }
        }
        units
    } else {
        Vec::new()
    };
    // The rect_tu strips' true units (`read_inter_rect_chroma`'s return):
    // replayed by the same loop below, after the whole-block record.
    mu_chroma_units.extend(rect_chroma_units);
    // libaom `av1_reset_entropy_context` / `av1_set_entropy_contexts`:
    // `nplanes = 1 + (num_planes - 1) * xd->is_chroma_ref` -- a block that is
    // NOT the chroma reference of its 4:2:0 pair codes no chroma transform at
    // all, so it leaves its neighbours' CHROMA coefficient contexts exactly as
    // it found them. `record_rect_mi`/`record_split_luma_rect_mi` below write
    // all three planes unconditionally, and for such a strip the chroma grids
    // are all-zero (see the `!has_chroma` arm above) -- that wiped the real
    // left-neighbour chroma state, so the PAIR's own `all_zero` then read
    // `txb_skip_ctx` one row low (lane-t900 r3, 10-bit 1920x792 stream,
    // mi(40,220): base 1 where libaom gathers 2). Snapshot and restore is
    // equivalent to not writing them, without threading a flag through both
    // recorders.
    // lane-t900 r22: every block stamps its palette state (all-zero for an
    // inter block, real for a palette one) into the mi-granular neighbour
    // bands, so the next intra block's `av1_get_palette_mode_ctx` and colour
    // cache see exactly what libaom's mode-info grid holds.
    record_strip_palette(
        neighbours,
        at,
        write_w,
        write_h,
        palette_record.0,
        palette_record.1,
        palette_record.2,
        palette_record.3,
    );
    let saved_chroma_ctx = (!has_chroma).then(|| {
        hit!(NON_CHROMA_REF_CTX_SKIP_HITS);
        let (w_mi, h_mi) = (write_w / MI, write_h / MI);
        let left: Vec<[Neighbour; 2]> = (0..h_mi)
            .filter_map(|k| neighbours.left.get(at.0 + k).map(|s| [s[1], s[2]]))
            .collect();
        let above: Vec<[Neighbour; 2]> = (0..w_mi)
            .filter_map(|k| neighbours.above.get(at.1 + k).map(|s| [s[1], s[2]]))
            .collect();
        (left, above)
    });
    if vartx_leaves.is_some() {
        // lane-t900 r17 root cause (class early-return-skips-tail): the
        // "plane 0 is already correct per transform unit" premise below holds
        // only when the per-TU luma loop RAN. A SKIP inter block takes the
        // `if skip` arm above, which reconstructs straight from the predictor
        // and never enters that loop -- yet `read_block_tx_size` still hands
        // back four TX_64X64 leaves for a 128 root under `TX_MODE_LARGEST`
        // (and for any block under it), so `vartx_leaves` is `Some` and the
        // luma coefficient contexts were left at the PREVIOUS block's values.
        // libaom calls `av1_reset_entropy_context` for every skipped block,
        // zeroing all three planes over its own footprint; `luma_grid` is the
        // all-zero grid on this arm, so publishing it is exactly that reset.
        // Witnessed on a 10-bit 512x512 --sb-size=128 stream: the 128x128
        // block at mi(32,64) of decode-order frame 1 read `txb_skip_ctx` 2
        // where aomdec reads 1, because the skipped 128x128 block at
        // mi(32,32) never cleared `left[32..48]`.
        if skip {
            hit!(SKIP_VARTX_LUMA_RESET_HITS);
            neighbours.record_mi_luma_rect(at, write_w, write_h, &luma_grid);
        }
        // Plane 0 is already correct per transform unit
        // ([`Neighbours::record_mi_luma`] above); this writes everything else
        // [`Neighbours::record_rect`] would.
        //
        // lane-sbab r1: this used to pass `side, side` on the premise that "a
        // var-tx block is always square". That stopped being true the moment
        // the SB-level rect strips landed (lane-inter4): a 64x32 / 32x64
        // strip keeps `side = SB` for syntax but is NOT square, and stamping
        // its neighbour bands 64x64 wrote `above_side_mi = 64` where libaom's
        // `update_partition_context` writes `partition_context_lookup[
        // BLOCK_32X64]`. The next superblock row then read the partition
        // context one value low (ctx 2 where aomdec says 3) and desynced.
        // The true footprint is `write_w`/`write_h`, exactly as the
        // non-var-tx arm below uses.
        // Plane 0 is already correct per transform unit
        // ([`Neighbours::record_mi_luma`] above); this writes everything else
        // [`Neighbours::record_rect`] would. lane-vert46 r1: the block's OWN
        // width/height, not `side` -- var-tx is no longer square-only (the 128
        // root's HORZ/VERT arm decodes 128x64 / 64x128 var-tx blocks), and
        // `side` there stamped `above_side`/`left_side` = 128, so the NEXT
        // superblock's 128-root partition context read 16 where libaom reads 19.
        neighbours.record_split_luma_rect_mi(
            at,
            write_w,
            write_h,
            mode_for_tx,
            uv_predict_mode,
            [&u_grid, &v_grid],
        );
    } else {
        neighbours.record_rect_mi(
            at,
            write_w,
            write_h,
            mode_for_tx,
            uv_predict_mode,
            &[&luma_grid[..], &u_grid[..], &v_grid[..]],
        );
    }
    if let Some((left, above)) = saved_chroma_ctx {
        for (k, v) in left.into_iter().enumerate() {
            if let Some(slot) = neighbours.left.get_mut(at.0 + k) {
                slot[1] = v[0];
                slot[2] = v[1];
            }
        }
        for (k, v) in above.into_iter().enumerate() {
            if let Some(slot) = neighbours.above.get_mut(at.1 + k) {
                slot[1] = v[0];
                slot[2] = v[1];
            }
        }
    }
    if let Some((s, u_state, v_state)) = pair_chroma {
        // lane-av1422ctxgather: at 4:2:2 (ss 1,0) a horz strip is its own
        // chroma reference -- no pair merge ever happened, so the write
        // covers the strip's own 8x4: ONE luma-mi left row (libaom's
        // `av1_set_contexts` for the TX_8X4 at ss_y 0 stamps
        // `tx_size_high_unit` = 1 cell). The 4:2:0 pair's two-row (16, 8)
        // span would overwrite the NEXT row's leaf-stamped level state
        // (measured: t422 frame 1, the strip at mi(8,4) zeroed
        // left[9][1] under the mi(8,2) 8x8's level 5 before the mi(9,4)
        // strip's own all_zero read it one ctx row low).
        let (pw, ph) = if s.horz {
            if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                (16usize, 4usize)
            } else {
                (16usize, 8usize)
            }
        } else {
            (8, 16)
        };
        let round_up_even = |n: usize| n.div_ceil(2) * 2;
        let (bound_h, bound_w) = (
            round_up_even(neighbours.mi_rows),
            round_up_even(neighbours.mi_cols),
        );
        for cell in 0..(ph / MI) {
            if s.pair_mi.0 + cell < bound_h
                && let Some(slot) = neighbours.left.get_mut(s.pair_mi.0 + cell)
            {
                slot[1] = u_state;
                slot[2] = v_state;
            }
        }
        for cell in 0..(pw / MI) {
            if s.pair_mi.1 + cell < bound_w
                && let Some(slot) = neighbours.above.get_mut(s.pair_mi.1 + cell)
            {
                slot[1] = u_state;
                slot[2] = v_state;
            }
        }
        neighbours.record_uv_mode_mi(s.pair_mi.0, s.pair_mi.1, pw / MI, ph / MI, uv_predict_mode);
        hit!(INTER16_CHROMA_PAIR_HITS);
    }
    // lane-av1-444 c8: each unit stamps its OWN span (32 << ss px at
    // TX_32X32, 8 px lossless) -- a 64x64 mu chunk at ss 0/0 holds four
    // TX_32X32 units, so the old whole-chunk 64-px span smeared one unit's
    // state over its three neighbours' cells too.
    for (cu_mi, plane, unit, span_w, span_h) in &mu_chroma_units {
        neighbours.record_mi_chroma(*cu_mi, *span_w, *span_h, *plane, unit);
    }
    neighbours.record_inter_rect_mi(
        at,
        write_w / MI,
        write_h / MI,
        skip,
        is_inter,
        ref_frame_for_lf,
        block_filter,
        skip_mode,
    );
    if let Some((ref1, group_idx, idx)) = compound_ctx {
        neighbours.record_compound_ctx_rect_mi(
            at,
            write_w / MI,
            write_h / MI,
            ref1,
            group_idx,
            idx,
        );
    }
    neighbours.fill_skip_grid_rect((rmi, cmi), write_w / MI, write_h / MI, skip);
    neighbours.fill_lf_grid_rect(
        (rmi, cmi),
        write_w / MI,
        write_h / MI,
        // lane-rect r2: the strip's true TX_32X16/TX_16X32 dims -- the
        // single-scalar corner-cut deblocked the strip seam with the wrong
        // edge length (1-px chroma drift on rect-flake-1 f17+).
        write_w as u8,
        write_h as u8,
        // ref_grid's packed convention (lf_level): sign carries GLOBALMV,
        // whose mode_lf_lut row is 0, not the 1 every other inter mode gets.
        if globalmv_for_lf {
            -ref_frame_for_lf
        } else {
            ref_frame_for_lf
        },
        fctx,
    );
    // lane-txselect: a var-tx block's deblock edges follow its own transform
    // units, not the block -- for LUMA only (lane-golomb r6): the chroma
    // grids keep the block's own `av1_get_max_uv_txsize`, which is what
    // libaom's `get_transform_size` reads for `plane != AOM_PLANE_Y`.
    // lane-sb128c r4: libaom `set_one_param_for_line_luma` suppresses every
    // edge that is a TU edge but not a PU edge when
    // `curr_skipped = mbmi->skip_txfm && is_inter_block(mbmi)`, so a SKIP
    // inter block's internal transform edges are never filtered even though
    // `get_transform_size` still reports TX_64X64 for it. The only block that
    // reaches here skipped is the 128 root (`read_block_tx_size` hands back
    // four TX_64X64 leaves for `side > 64` regardless of `skip`); leaving its
    // block-level 128 publication in place is exactly that suppression.
    // lane-sb128c r6: narrowed to the 128 root. Below it `read_block_tx_size`
    // hands a skipped inter block a single leaf equal to the block, so the
    // per-leaf write is the block-level one and this branch is a no-op either
    // way; only the 128 root gets four TX_64X64 leaves regardless of `skip`.
    let skip_inter_128 = skip && is_inter && write_w.max(write_h) > 64;
    if skip_inter_128 {
        neighbours.suppress_internal_lf_edges((rmi, cmi), write_w / MI, write_h / MI, fctx);
    }
    if let Some(leaves) = (!skip_inter_128).then_some(()).and(vartx_leaves.as_ref()) {
        for &(row, col, tw, th) in leaves {
            neighbours.fill_lf_grid_leaf_luma(
                (at_mi.0 + row, at_mi.1 + col),
                tw / MI,
                th / MI,
                tw as u8,
                th as u8,
            );
        }
    }
    Ok(())
}

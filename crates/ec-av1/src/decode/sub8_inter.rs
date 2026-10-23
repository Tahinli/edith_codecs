//! Sub-8x8 inter and intra leaf decoders (split4 and rect2 paths). Moved out of `decode/mod.rs` (av1split D7); the items are unchanged apart from `pub(super)` visibility.

use super::*;

/// Decodes one 8x8 group that an inter frame's `partition_w8` symbol split
/// into four `BLOCK_4X4` inter blocks (lane-intersub8: `PARTITION_SPLIT`
/// below 8x8 -- traced live against the oracle `aomdec`'s own `EC_PART_VAL`,
/// which reads `bsize=3 value=3` on real inter frames, so the refusal this
/// replaces was NOT our own desync).
///
/// The inter counterpart of [`decode_leaf_split4`], and a `BLOCK_4X4`-only
/// cut of [`decode_inter_block8`]: at `min(bw, bh) < 8` libaom switches off
/// half the inter syntax, and every one of those is a symbol NOT read here
/// (`decodemv.c` / `blockd.h`, oracle tree):
/// * `skip_mode`: `read_skip_mode` returns 0 without a symbol when
///   `!is_comp_ref_allowed(bsize)` (`AOMMIN(bw, bh) >= 8`, blockd.h).
/// * `comp_mode`: `read_block_reference_mode` returns `SINGLE_REFERENCE` on
///   the same predicate, so a sub-8x8 block is always single-reference.
/// * `interintra`: `is_interintra_allowed_bsize` starts at `BLOCK_8X8`.
/// * `motion_mode` / `obmc`: `motion_mode_allowed` returns
///   `SIMPLE_TRANSLATION` for `block_size_wide < 8 || block_size_high < 8`,
///   and `is_global_mv_block`'s own size term fails, so no warp either.
/// * tx size: `block_signals_txsize(bsize)` is `bsize > BLOCK_4X4`
///   (blockd.h:1027), so a `BLOCK_4X4` reads neither `tx_depth` nor a
///   `txfm_partition` bit even in `TX_MODE_SELECT` -- the transform is
///   `max_txsize_rect_lookup[BLOCK_4X4]` = `TX_4X4`
///   (`decodeframe.c:1164-1177`).
///
/// Chroma is coded ONCE for the whole 8x8 group, on the last sub-block
/// (`is_chroma_reference`: `(mi_row & 1) && (mi_col & 1)` in 4:2:0), as one
/// `TX_4X4` unit per plane -- same rule and same neighbour-context tail as
/// [`decode_leaf_split4`]. Its PREDICTION is the sub8x8 one
/// (`dec_build_inter_predictors`'s `is_sub8x8` branch, `decodeframe.c`; spec
/// 7.11.3.1's `candRow`/`candCol` loop): four 2x2 chroma pieces, each built
/// from its own luma sub-block's reference, motion vector and interpolation
/// filters, never from the last sub-block's mv alone.
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_inter_sub8_split4(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    grid: &mut MiGrid,
    group_mi: (usize, usize),
    mi_cols: u32,
    mi_rows: u32,
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    refpix: &RefPix<'_>,
    sign_bias_table: &SignBiasTable,
    global_motion: &[ec_av1_syntax::WarpParams; 7],
    base_q_idx: u8,
    scan4: &[u16],
    allow_high_precision_mv: bool,
    force_integer_mv: bool,
    interp_fixed: Option<mc::InterpFilterKind>,
    enable_dual_filter: bool,
    tpl_frame: Option<&TplFrameArgs>,
    reduced_tx_set: bool,
    frame_size: (usize, usize),
    fctx: &crate::decode::FrameCtx,
) -> Result<()> {
    const LAST_FRAME: i8 = 1;
    const B4: usize = 4;
    hit!(SUB8_INTER_SPLIT_HITS);
    let (gr, gc) = group_mi;
    let (gpx, gpy) = (gc * MI, gr * MI);
    let (cpx, cpy) = (gpx >> ss_x(fctx), gpy >> ss_y(fctx));
    // Per-sub-block state the group's chroma prediction needs (spec
    // 7.11.3.1's candRow/candCol loop reads each sub-block's OWN mv/ref/
    // filters), indexed `dr * 2 + dc`.
    let mut piece: [Option<(i8, (i32, i32), mc::InterpFilterKind, mc::InterpFilterKind)>; 4] =
        [None; 4];
    let mut first_tx_type = TxType::DctDct;
    let mut sub0_tx_type = TxType::DctDct;
    // This loop iteration's own sub-block luma `tx_type` (libaom `mbmi->tx_type`
    // after its `read_tx_type`): the per-piece 4:4:4 chroma units inherit it
    // (`av1_get_tx_type`, blockd.h -- inter chroma never codes its own symbol).
    let mut piece_tx_type = TxType::DctDct;
    let mut last_skip = true;
    // Set when the chroma-reference (last) sub-block is INTRA: it coded the
    // group's one chroma unit itself.
    let mut intra_chroma = false;
    // 4:4:4 (lane-av1-444): `is_chroma_reference` is unconditional at
    // subsampling 0/0, so EVERY 4x4 sub-block codes its own 4x4 chroma unit
    // right after its own luma (aomdec EC_COEFF on `av1-profile1-444.ivf`
    // frame 1: plane 0,1,2 per leaf in leaf order) -- there is no shared
    // group unit and no 2x2-piece group prediction.
    let chroma_444 = ss_x(fctx) == 0 && ss_y(fctx) == 0;
    // 4:2:2 (lane-av1-422sub8rect): `is_chroma_reference` for a BLOCK_4X4
    // piece at ss (1, 0) is `(mi_col & 0x01) || !(bw & 0x01) || !ss_x`
    // == ODD mi_col (av1_common_int.h:1459) -- pieces 1 and 3 of the group
    // are their plane's own chroma references and each codes its own
    // TX_4X4 unit right after its luma, exactly the key-frame split-group
    // behaviour the pinned t422 fixture's frame 0 traces (groups (18,54)
    // and (30,42): U+V read on BOTH odd-column leaves) and frame 1's
    // inter group at mi(12,2) repeats (U+V TX_4X4 ctx 8 on leaf (12,3)
    // alone -- the bottom-right leaf was skipped, so it codes nothing).
    // The 4:2:0 shared group unit never exists at 4:2:2.
    let chroma_422 = ss_x(fctx) == 1 && ss_y(fctx) == 0;
    for i in 0..4usize {
        let (dr, dc) = (i / 2, i % 2);
        let lmi = (gr + dr, gc + dc);
        if lmi.0 >= mi_rows as usize || lmi.1 >= mi_cols as usize {
            continue;
        }
        let (rmi, cmi) = lmi;
        let (px, py) = (cmi * MI, rmi * MI);
        inter_segment_id(dec, cdfs, rmi, cmi, 1, 1, false, true, fctx);
        // No `skip_mode` symbol: `is_comp_ref_allowed(BLOCK_4X4)` is false.
        let skip_ctx =
            usize::from(neighbours.above_skip[cmi]) + usize::from(neighbours.left_skip[rmi]);
        let skip = dec.symbol(&mut cdfs.skip[skip_ctx]) == 1;
        let ec_skip_post = if std::env::var_os("EC_DBG_SUB8").is_some() {
            Some(dec.debug_state().0)
        } else {
            None
        };
        inter_segment_id(dec, cdfs, rmi, cmi, 1, 1, skip, false, fctx);
        maybe_read_cdef_idx(dec, rmi, cmi, (2, 2), skip, fctx);
        maybe_read_delta_q(dec, cdfs, rmi, cmi, false, skip, fctx);
        maybe_read_delta_lf(dec, cdfs, rmi, cmi, false, skip, fctx);
        let (has_above, has_left) = (rmi > neighbours.tile_row0_mi, cmi > neighbours.tile_col0_mi);
        let ii_ctx = intra_inter_ctx(
            has_above,
            has_left,
            neighbours.above_inter[cmi],
            neighbours.left_inter[rmi],
        );
        let is_inter = dec.symbol(&mut cdfs.intra_inter[ii_ctx]) == 1;
        if std::env::var_os("EC_DBG_SUB8").is_some() {
            eprintln!(
                "EC_DBG piece mi=({rmi},{cmi}) skip={skip} skipctx={skip_ctx} skip_post={:?} is_inter={is_inter} iictx={ii_ctx} post={}",
                ec_skip_post,
                dec.debug_state().0
            );
        }
        if !is_inter {
            if crate::envflags::env_flag!("EC_SUB8INTRA") {
                eprintln!(
                    "EC_SUB8INTRA shape=4x4 sub={i} has_chroma={} skip={skip} mi={rmi},{cmi} tx_select={}",
                    i == 3,
                    fctx.tx_select_inter.with(std::cell::Cell::get),
                );
            }
            // lane-sub8x4 r2: the refusal and its `EC_SUB8INTRA_DECODE` bypass
            // are gone -- `a_real_aomenc_inter_sequence_with_intra_sub8x8_
            // leaves_decodes_pixel_exact` (stream.rs) decodes 12 real aomenc
            // streams carrying this arm pixel-exact vs ffmpeg.
            hit_do! {
                SUB8_INTRA_RECT_HITS.with(|h| {
                    let mut st = h.get();
                    st[4] += 1;
                    h.set(st);
                });
            }
            // lane-sub8intra: the `BLOCK_4X4` twin of the HORZ/VERT arm --
            // same [`decode_intra_sub8_leaf`], no tx-depth symbol, chroma on
            // the chroma-reference sub-blocks (`is_chroma_reference`: the
            // LAST one under 4:2:0, BOTH odd-column ones at 4:2:2, every
            // one at 4:4:4).
            decode_intra_sub8_leaf(
                dec,
                cdfs,
                neighbours,
                grid,
                group_mi,
                lmi,
                (B4, B4),
                chroma_444 || (cmi & 1 == 1 && chroma_422) || i == 3,
                skip,
                scan4,
                y,
                u,
                v,
                base_q_idx,
                fctx.enable_filter_intra_inter.with(std::cell::Cell::get),
                fctx.tx_select_inter.with(std::cell::Cell::get),
                reduced_tx_set,
                fctx,
            )?;
            if i == 3 {
                intra_chroma = true;
            }
            continue;
        }
        // Same print point as the oracle's `EC_MODE` (entry of
        // `read_inter_block_mode_info`, i.e. after skip / is_inter), so the
        // two range ladders line up element for element.
        if crate::envflags::env_flag!("EC_TRACE_MODE") {
            eprintln!(
                "EC_MODE mi_row={rmi} mi_col={cmi} rng={}",
                dec.debug_state().0
            );
        }
        let ref_frame = read_single_ref(
            dec,
            cdfs,
            neighbours.above_ref[cmi],
            neighbours.above_ref1[cmi],
            neighbours.left_ref[rmi],
            neighbours.left_ref1[rmi],
        );
        // lane-sub8group: this piece's reference SIZE at parse, its pictures
        // inside the prediction closure below.
        let (ref_width, ref_height) = ref_dims(ref_frame, refpix)?;
        // lane-t900 r28: refusal lifted -- this sub-block's own reference may
        // be scaled (superres); spec 7.11.3.3's walk is per reference, and
        // each of the group's pieces (luma and its 2x2 chroma) carries its
        // OWN factor because each can name a different `ref_frame`.
        let luma_scale = scale_of((ref_width, ref_height), frame_size);
        if !luma_scale.is_unscaled() {
            hit!(SCALED_SUB8_HITS);
        }
        let gm_table = build_gm_mv_table(
            global_motion,
            rmi,
            cmi,
            1,
            1,
            allow_high_precision_mv,
            force_integer_mv,
        );
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
        let stack = crate::mvstack::find_mv_stack_with_sign_bias(
            grid,
            rmi,
            cmi,
            1,
            1,
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
        let (mv, is_new_mv) = if !not_new {
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
                gm_table[(ref_frame - LAST_FRAME) as usize]
            } else {
                let nearest = dec.symbol(&mut cdfs.ref_mv[stack.ref_mv_ctx]) == 0;
                crate::census::mode(if nearest { 2 } else { 3 });
                if nearest {
                    stack.nearest_mv
                } else {
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
        // Same print point as the oracle's `EC_MODE_MV` (right after
        // `assign_mv`), the second rung of the sub-8x8 range ladder.
        if crate::envflags::env_flag!("EC_TRACE_MODE") {
            eprintln!(
                "EC_MODE_MV mi_row={rmi} mi_col={cmi} mv0=({},{}) rng={}",
                mv.0,
                mv.1,
                dec.debug_state().0
            );
        }
        // No `interintra` and no `motion_mode`/`obmc` symbol below 8x8 (see
        // this function's own doc); `read_mb_interp_filter` still runs.
        let gm_ref = &global_motion[(ref_frame - LAST_FRAME) as usize];
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
        if crate::envflags::env_flag!("EC_AV1_IFDBG") {
            eprintln!(
                "IFDBG4 mi=({rmi},{cmi}) ref={ref_frame} above_ref={} above_ref1={:?} \
                 above_filt={:?} left_ref={} left_ref1={:?} left_filt={:?}",
                neighbours.above_ref[cmi],
                neighbours.above_ref1[cmi],
                neighbours.above_filter[cmi],
                neighbours.left_ref[rmi],
                neighbours.left_ref1[rmi],
                neighbours.left_filter[rmi],
            );
        }
        // lane-t900 r9: this reader's sub-blocks have a 4-px side, and libaom
        // `is_nontrans_global_motion` returns 0 for
        // `AOMMIN(mi_size_wide, mi_size_high) < 2` BEFORE testing any model --
        // so a GLOBALMV sub-block here always reads its interp filter.
        if is_globalmv && gm_ref.model != ec_av1_syntax::WarpModel::Translation {
            hit!(GM_NONTRANS_SMALL_SIDE_HITS);
        }
        let gm_nontrans = false;
        let (h_filter, v_filter, resolved_filter) = resolve_interp_filter(
            dec,
            cdfs,
            interp_fixed,
            enable_dual_filter,
            gm_nontrans,
            above_filter_ctx,
            left_filter_ctx,
            false,
        );
        if crate::envflags::env_flag!("EC_TRACE_MODE") {
            eprintln!(
                "EC_MODE_VAL4 mi_row={rmi} mi_col={cmi} newmv={is_new_mv} globalmv={is_globalmv} \
                 ref0={ref_frame} mv0=({},{}) stack={} rng={}",
                mv.0,
                mv.1,
                stack.entries.len(),
                dec.debug_state().0
            );
        }
        grid.set(
            rmi,
            cmi,
            MiInfo {
                is_inter: true,
                ref_frame,
                ref_frame1: NO_REF1,
                mv1: (0, 0),
                mv: mv16(mv),
                is_new_mv,
                size: 1,
                size_h: 1,
                is_global_mv0: false,
                is_global_mv1: false,
            },
        );
        piece[i] = Some((ref_frame, mv, h_filter, v_filter));
        // lane-sub8group: this piece's prediction as a closure, run at the push
        // site, exactly like the 8x8 leaf's.
        let build = move |_y: &mut PlaneBuf<'static>,
                          _u: &mut PlaneBuf<'static>,
                          _v: &mut PlaneBuf<'static>,
                          refpix: &RefPix<'_>,
                          fctx: &crate::decode::FrameCtx,
                          out: &mut [Vec<u16>; 3]| {
            let Some((sref_y, _, _)) = ref_planes_deferred(ref_frame, refpix) else {
                return;
            };
            let mut pred_y = refill(std::mem::take(&mut out[0]), B4 * B4);
            mc::predict_maybe_scaled(
                &sref_y.data,
                sref_y.width,
                sref_y.true_width,
                sref_y.true_height,
                mv_to_q4(px, mv.1, 0),
                mv_to_q4(py, mv.0, 0),
                luma_scale,
                B4,
                B4,
                h_filter,
                v_filter,
                &mut pred_y,
                fctx,
            );
            out[0] = pred_y;
        };
        let pred_y: Vec<u16>;
        let sy = if pred_deferred(refpix) {
            push_pred(build);
            Pred::Cur { plane: 0, stride: B4, off: 0 }
        } else {
            let mut built: [Vec<u16>; 3] = Default::default();
            build(y, u, v, refpix, fctx, &mut built);
            let [by, _, _] = built;
            pred_y = by;
            Pred::Inline(&pred_y, B4)
        };
        // `BLOCK_4X4` never signals a transform size, so the luma unit is the
        // whole block and `get_txb_ctx` takes its `plane_bsize ==
        // txsize_to_bsize[tx_size]` branch (`txb_skip_ctx = 0`, which
        // `read_inter_plane`'s `plane_idx == 0` arm already assumes).
        if skip {
            push_mc(y, u, v, 0, px, py, B4, sy, &ZERO_RESIDUAL[..B4 * B4], fctx);
            neighbours.record_mi_luma_rect(lmi, B4, B4, &ZERO_RESIDUAL[..B4 * B4]);
        } else {
            let around = neighbours.around_mi(lmi, B4);
            let (luma_grid, tx_type) = read_inter_plane(
                dec,
                cdfs,
                if reduced_tx_set {
                    TxbSet::Luma4Inter
                } else {
                    TxbSet::Luma4InterSet1
                },
                scan4,
                0,
                around[0],
                0,
                y,
                u,
                v,
                px,
                py,
                B4,
                base_q_idx,
                sy,
                None,
                None,
                fctx,
            )?;
            piece_tx_type = tx_type;
            if i == 0 {
                sub0_tx_type = tx_type;
            }
            if i == 3 {
                // `av1_get_tx_type`'s chroma tail scales `blk_row`/`blk_col`
                // back by the subsampling, but it indexes `xd->tx_type_map`,
                // which `set_mi_offsets` (av1_common_int.h:1679) re-points to
                // the CURRENT sub-block's own mi -- so the group's one chroma
                // unit inherits the CHROMA-REFERENCE (last) sub-block's
                // tx_type, never the group's first. lane-t900 r7: reading it
                // off sub-block 0 left the entropy stream in sync (no symbol
                // is coded for chroma) and inverted one 4x4 chroma unit on the
                // wrong basis.
                first_tx_type = tx_type;
            }
            neighbours.record_mi_luma_rect(lmi, B4, B4, &luma_grid);
        }
        // lane-av1-444: `is_chroma_reference` is unconditional at ss 0/0
        // (av1_common_int.h:1459 -- `!subsampling_x`/`!subsampling_y` force
        // both disjuncts true), so at 4:4:4 this 4x4 sub-block codes its own
        // 4x4 U and V units right after its luma, plane order 0,1,2 per leaf
        // (aomdec EC_COEFF on `av1-profile1-444.ivf` frame 1). Chroma MC is
        // the piece's own mv and filters at the piece's own pixel position
        // (ss 0: the chroma planes share luma's grid), and each unit's
        // tx_type is its own piece's luma type -- an inter block's chroma
        // plane never codes its own tx_type symbol but inherits the
        // colocated luma transform's coded type verbatim (`av1_get_tx_type`,
        // blockd.h), which decides the eob CDF's class-1 row
        // (`eob_multi_ctx`, decodetxb.c:214).
        if chroma_444 {
            let (pc_ref, pc_mv, pc_hf, pc_vf) = piece[i].expect("inter piece recorded above");
            let build_c = move |_y: &mut PlaneBuf<'static>,
                                _u: &mut PlaneBuf<'static>,
                                _v: &mut PlaneBuf<'static>,
                                refpix: &RefPix<'_>,
                                fctx: &crate::decode::FrameCtx,
                                out: &mut [Vec<u16>; 3]| {
                let Some((pc_ref_y, sref_u, sref_v)) = ref_planes_deferred(pc_ref, refpix) else {
                    return;
                };
                let pc_scale = scale_of((pc_ref_y.true_width, pc_ref_y.true_height), frame_size);
                let mut pred_u = refill(std::mem::take(&mut out[1]), B4 * B4);
                let mut pred_v = refill(std::mem::take(&mut out[2]), B4 * B4);
                for (plane, dst) in [(sref_u, &mut pred_u), (sref_v, &mut pred_v)] {
                    let mut buf = vec![0u16; B4 * B4];
                    mc::predict_maybe_scaled(
                        &plane.data,
                        plane.width,
                        plane.true_width,
                        plane.true_height,
                        mv_to_q4(px, pc_mv.1, ss_x(fctx)),
                        mv_to_q4(py, pc_mv.0, ss_x(fctx)),
                        pc_scale,
                        B4,
                        B4,
                        pc_hf,
                        pc_vf,
                        &mut buf,
                        fctx,
                    );
                    dst.copy_from_slice(&buf);
                }
                out[1] = pred_u;
                out[2] = pred_v;
            };
            let (pred_cu, pred_cv): (Vec<u16>, Vec<u16>);
            let (su, sv) = if pred_deferred(refpix) {
                push_pred(build_c);
                (Pred::Cur { plane: 1, stride: B4, off: 0 }, Pred::Cur { plane: 2, stride: B4, off: 0 })
            } else {
                let mut built: [Vec<u16>; 3] = Default::default();
                build_c(y, u, v, refpix, fctx, &mut built);
                let [_, bu, bv] = built;
                (pred_cu, pred_cv) = (bu, bv);
                (Pred::Inline(&pred_cu, B4), Pred::Inline(&pred_cv, B4))
            };
            if skip {
                push_mc(y, u, v, 1, px, py, B4, su, &ZERO_RESIDUAL[..B4 * B4], fctx);
                push_mc(y, u, v, 2, px, py, B4, sv, &ZERO_RESIDUAL[..B4 * B4], fctx);
                neighbours.record_mi_chroma(lmi, B4, B4, 1, &ZERO_RESIDUAL[..B4 * B4]);
                neighbours.record_mi_chroma(lmi, B4, B4, 2, &ZERO_RESIDUAL[..B4 * B4]);
            } else {
                let ca = neighbours.around_mi(lmi, B4);
                let (ug, _) = read_inter_plane(
                    dec,
                    cdfs,
                    TxbSet::Chroma4,
                    scan4,
                    1,
                    ca[1],
                    0,
                    y,
                    u,
                    v,
                    px,
                    py,
                    B4,
                    base_q_idx,
                    su,
                    Some(piece_tx_type),
                    None,
                    fctx,
                )?;
                let (vg, _) = read_inter_plane(
                    dec,
                    cdfs,
                    TxbSet::Chroma4,
                    scan4,
                    2,
                    ca[2],
                    0,
                    y,
                    u,
                    v,
                    px,
                    py,
                    B4,
                    base_q_idx,
                    sv,
                    Some(piece_tx_type),
                    None,
                    fctx,
                )?;
                neighbours.record_mi_chroma(lmi, B4, B4, 1, &ug);
                neighbours.record_mi_chroma(lmi, B4, B4, 2, &vg);
            }
        // lane-av1-422sub8rect: the 4:2:2 per-piece arm. `is_chroma_reference`
        // for a BLOCK_4X4 at ss (1, 0) is the odd mi_col (`av1_common_int.h`
        // :1459), so THIS piece (right column of the group) is a chroma
        // reference and codes its own U and V TX_4X4 units right after its
        // luma -- measured on the pinned t422 fixture: frame 0 key-frame
        // groups (18,54)/(30,42) read U+V on both odd-column leaves, and
        // frame 1's inter group at mi(12,2) reads U+V TX_4X4 ctx 8 on leaf
        // (12,3) alone (its bottom-right twin was skipped).
        //
        // The unit anchors at the FLOORED chroma cell
        // (`av1_setup_dst_planes`' `(mi_col & !1) * MI >> ss_x`, the
        // lane-av1-422kf2 rule), so the 4-px-wide square covers the group's
        // whole chroma width, and the prediction walks
        // `build_inter_predictors_sub8x8` (reconinter_template.inc): the
        // left 2x4 half from the LEFT piece's own ref/mv/filters, the right
        // 2x4 half from this piece's -- unless the left piece is intra (or
        // off-frame), which makes `is_sub8x8_inter` false and predicts the
        // whole 4x4 from THIS piece's own mv. A skipped reference piece
        // still predicts (spec 7.11.3: `predict_inter_block_visit` runs
        // before the `skip_txfm` guard) but codes no coefficients.
        } else if chroma_422 && (cmi & 1) == 1 {
            let (pc_ref, pc_mv, pc_hf, pc_vf) = piece[i].expect("inter piece recorded above");
            let unit_x = ((cmi & !1) * MI) >> ss_x(fctx);
            let unit_y = py >> ss_y(fctx);
            // `is_sub8x8_inter`'s left-neighbour probe (`mi[-1]`): the piece
            // one mi to the LEFT, when it exists and is inter.
            let piece_left = piece[i - 1];
            let build_c = move |_y: &mut PlaneBuf<'static>,
                                _u: &mut PlaneBuf<'static>,
                                _v: &mut PlaneBuf<'static>,
                                refpix: &RefPix<'_>,
                                fctx: &crate::decode::FrameCtx,
                                out: &mut [Vec<u16>; 3]| {
                let Some((pc_ref_y, sref_u, sref_v)) = ref_planes_deferred(pc_ref, refpix) else {
                    return;
                };
                let pc_scale = scale_of((pc_ref_y.true_width, pc_ref_y.true_height), frame_size);
                // The left piece's own reference planes + mv/filters, when it
                // is an inter piece (`is_sub8x8_inter`'s `mi[-1]` probe).
                let left = piece_left
                    .as_ref()
                    .map(|&(l_ref, l_mv, l_hf, l_vf)| (l_ref, l_mv, l_hf, l_vf))
                    .and_then(|(l_ref, l_mv, l_hf, l_vf)| {
                        ref_planes_deferred(l_ref, refpix).map(|planes| {
                            let (lp_y, _, _) = planes;
                            let l_scale = scale_of((lp_y.true_width, lp_y.true_height), frame_size);
                            (l_mv, l_hf, l_vf, planes, l_scale)
                        })
                    });
                let mut pred_u = refill(std::mem::take(&mut out[1]), B4 * B4);
                let mut pred_v = refill(std::mem::take(&mut out[2]), B4 * B4);
                for (pi, (plane, dst)) in [(sref_u, &mut pred_u), (sref_v, &mut pred_v)]
                    .into_iter()
                    .enumerate()
                {
                    let mut buf = vec![0u16; B4 * B4];
                    // Right 2x4 half: this piece's own ref/mv/filters.
                    let mut half = vec![0u16; 2 * 4];
                    mc::predict_maybe_scaled(
                        &plane.data,
                        plane.width,
                        plane.true_width,
                        plane.true_height,
                        mv_to_q4(unit_x + 2, pc_mv.1, ss_x(fctx)),
                        mv_to_q4(unit_y, pc_mv.0, ss_y(fctx)),
                        pc_scale,
                        2,
                        4,
                        pc_hf,
                        pc_vf,
                        &mut half,
                        fctx,
                    );
                    for row in 0..4 {
                        buf[row * B4 + 2..row * B4 + 4]
                            .copy_from_slice(&half[row * 2..row * 2 + 2]);
                    }
                    // Left 2x4 half: the LEFT piece's own mv/filters
                    // (sub8x8-inter); a mixed group (left piece intra or
                    // off-frame) predicts it from THIS piece too, which
                    // reduces to the whole-4x4-from-own rule.
                    let (l_mv, l_hf, l_vf, l_planes, l_scale) = match left {
                        Some((l_mv, l_hf, l_vf, l_planes, l_scale)) => {
                            (l_mv, l_hf, l_vf, l_planes, l_scale)
                        }
                        None => (pc_mv, pc_hf, pc_vf, (pc_ref_y, sref_u, sref_v), pc_scale),
                    };
                    // lane-av1422sub8pix: the left half predicts from the
                    // LEFT piece's own plane of its reference -- U for the U
                    // iteration, V for the V one. The old `l_ref_u` grab
                    // fed the V plane's left half from the reference's U
                    // plane (V cols 44..45 of the t422 frame-1 groups read
                    // U samples; U was exact, V wasn't).
                    let l_ref = if pi == 0 {
                        let (_, u, _) = l_planes;
                        u
                    } else {
                        let (_, _, v) = l_planes;
                        v
                    };
                    mc::predict_maybe_scaled(
                        &l_ref.data,
                        l_ref.width,
                        l_ref.true_width,
                        l_ref.true_height,
                        mv_to_q4(unit_x, l_mv.1, ss_x(fctx)),
                        mv_to_q4(unit_y, l_mv.0, ss_y(fctx)),
                        l_scale,
                        2,
                        4,
                        l_hf,
                        l_vf,
                        &mut half,
                        fctx,
                    );
                    for row in 0..4 {
                        buf[row * B4..row * B4 + 2].copy_from_slice(&half[row * 2..row * 2 + 2]);
                    }
                    dst.copy_from_slice(&buf);
                }
                out[1] = pred_u;
                out[2] = pred_v;
            };
            let (pred_cu, pred_cv): (Vec<u16>, Vec<u16>);
            let (su, sv) = if pred_deferred(refpix) {
                push_pred(build_c);
                (Pred::Cur { plane: 1, stride: B4, off: 0 }, Pred::Cur { plane: 2, stride: B4, off: 0 })
            } else {
                let mut built: [Vec<u16>; 3] = Default::default();
                build_c(y, u, v, refpix, fctx, &mut built);
                let [_, bu, bv] = built;
                (pred_cu, pred_cv) = (bu, bv);
                (Pred::Inline(&pred_cu, B4), Pred::Inline(&pred_cv, B4))
            };
            // The unit's entropy context: one chroma cell wide, floored --
            // [`Neighbours::around_mi_422_chroma`] at the unit's own 8x4
            // luma span (the [`sub8_leaf_chroma422`] rule).
            let ctx_mi = (rmi, cmi & !1);
            let ca = neighbours.around_mi_422_chroma(ctx_mi, 8, 4);
            if skip {
                push_mc(
                    y,
                    u,
                    v,
                    1,
                    unit_x,
                    unit_y,
                    B4,
                    su,
                    &ZERO_RESIDUAL[..B4 * B4],
                    fctx,
                );
                push_mc(
                    y,
                    u,
                    v,
                    2,
                    unit_x,
                    unit_y,
                    B4,
                    sv,
                    &ZERO_RESIDUAL[..B4 * B4],
                    fctx,
                );
                neighbours.record_mi_chroma(ctx_mi, 8, 4, 1, &ZERO_RESIDUAL[..B4 * B4]);
                neighbours.record_mi_chroma(ctx_mi, 8, 4, 2, &ZERO_RESIDUAL[..B4 * B4]);
            } else {
                hit!(CHROMA422_INTER_SUB8_HITS);
                let (ug, _) = read_inter_plane(
                    dec,
                    cdfs,
                    TxbSet::Chroma4,
                    scan4,
                    1,
                    ca[1],
                    0,
                    y,
                    u,
                    v,
                    unit_x,
                    unit_y,
                    B4,
                    base_q_idx,
                    su,
                    Some(piece_tx_type),
                    None,
                    fctx,
                )?;
                let (vg, _) = read_inter_plane(
                    dec,
                    cdfs,
                    TxbSet::Chroma4,
                    scan4,
                    2,
                    ca[2],
                    0,
                    y,
                    u,
                    v,
                    unit_x,
                    unit_y,
                    B4,
                    base_q_idx,
                    sv,
                    Some(piece_tx_type),
                    None,
                    fctx,
                )?;
                neighbours.record_mi_chroma(ctx_mi, 8, 4, 1, &ug);
                neighbours.record_mi_chroma(ctx_mi, 8, 4, 2, &vg);
            }
        }
        neighbours.record_mode_mi(rmi, cmi, 1, 1, DC_PRED);
        neighbours.record_inter_rect_mi(lmi, 1, 1, skip, true, ref_frame, resolved_filter, false);
        // lane-inter16ab r8 (same-shape sweep of the intra-strip band gap):
        // a BLOCK_4X4 codes no `txfm_partition` symbol, but libaom still runs
        // `set_txfm_ctxs(TX_4X4, 1, 1, skip && is_inter, xd)` for it -- both
        // arms give 4 -- so the sub-block must publish 4 to the next block's
        // var-tx context instead of leaving whatever the previous block left.
        set_txfm_ctxs(neighbours, lmi, B4, 1, 1, false);
        neighbours.fill_skip_grid_rect(lmi, 1, 1, skip);
        neighbours.fill_lf_grid_rect(
            lmi,
            1,
            1,
            B4 as u8,
            B4 as u8,
            ref_frame.max(LAST_FRAME),
            fctx,
        );
        last_skip = skip;
    }
    // lane-av1-444: when the chroma-reference sub-block was INTRA it coded
    // and reconstructed the group's one chroma unit itself.
    // lane-av1-444 (f19): at ss 0/0 the group unit does not exist -- every
    // piece coded its own U/V inside the loop above, and every piece also
    // wrote its own `uv_mode` neighbour state: an inter piece through
    // `record_inter_rect_mi`'s is_inter DC stamp (libaom leaves `uv_mode`
    // unset on an inter mbmi, `is_smooth` reads 0), an INTRA piece through
    // [`decode_intra_sub8_leaf`]'s chroma_444 arm (libaom's per-sub-block
    // `mbmi->uv_mode` is real there -- frame 19 of the fixture reads
    // uv 12/11/9/12 on the four pieces of the (42,54) group, and the next
    // block's `get_intra_edge_filter_type` must see that SMOOTH_H left
    // neighbour, not the DC_PRED this stamp used to paint over it). So the
    // group-level stamp runs only where the 4:2:0 group unit is real.
    //
    // 4:2:0 "real" means the group unit was painted by the INTER arm below:
    // when the chroma-reference sub-block was INTRA, that leaf's own tail
    // ([`decode_intra_sub8_leaf`]) already recorded the group cell with its
    // real `uv_predict_mode` -- possibly a SMOOTH mode the next block's
    // chroma edge filter must see. The 444 delta's unconditional
    // `!chroma_444` stamp ran after it and painted DC over the mi-exact
    // cell, so every later chroma block read `intra_edge_filter_type` 0
    // where libaom (and the pre-delta tree) read 1 -- 188 chroma ops of the
    // gm_small_side witness flipped their smooth flag and 10-bit film
    // frames mismatched ffmpeg by +-1..7 (class
    // `override-slot-on-one-arm`, this lane's own 10-bit film gate
    // regression at 1ad7e0fc; port of lane-av1-cfl's 7c6dad53).
    if !chroma_444 && !chroma_422 && !intra_chroma {
        neighbours.record_uv_mode_mi(gr, gc, 2, 2, DC_PRED);
    }
    // lane-t900 r23 (class [[new-map-ignores-tile-edge]]: a neighbour map
    // needs EVERY writer): a sub-8x8 group published no palette state, so
    // once an 8x8 leaf of the same row can BE a palette block, the group
    // left the row's band holding that earlier block's size and the next
    // intra block read `av1_get_palette_mode_ctx` one too high (measured:
    // mi(30,30) of decode-order frame 1, ours ctx 1 where libaom gathers
    // 0). No sub-8x8 piece can itself be a palette (`av1_allow_palette`
    // needs `bsize >= BLOCK_8X8`), so the group always clears.
    record_strip_palette(neighbours, (gr, gc), 8, 8, 0, [0u16; 8], 0, [0u16; 8]);
    if !chroma_444 && !chroma_422 && !intra_chroma {
        // Chroma, once for the whole group, on the last sub-block's own `skip`
        // flag -- prediction from all four sub-blocks (2x2 pieces), residual as
        // one TX_4X4 unit per plane. lane-av1-422sub8rect: 4:2:2 never takes
        // this arm -- the shared group unit does not exist at ss (1, 0); each
        // odd-column piece coded its own chroma inside the loop above.
        // `is_sub8x8_inter` (reconinter_template.inc:54) is false as soon as ANY
        // mi of the group is intra: the group's 4x4 chroma is then predicted WHOLE
        // from the chroma-reference (last) sub-block's own mv, not in 2x2 pieces.
        let mixed = piece.iter().any(Option::is_none);
        if mixed {
            hit_do! {
                SUB8_INTRA_RECT_HITS.with(|h| {
                    let mut st = h.get();
                    st[3] += 1;
                    h.set(st);
                });
            }
        }
        // lane-sub8group: the group's chroma pieces, built where they are
        // written -- the pieces' refs/mvs/filters travel by value.
        let build = move |_y: &mut PlaneBuf<'static>,
                          _u: &mut PlaneBuf<'static>,
                          _v: &mut PlaneBuf<'static>,
                          refpix: &RefPix<'_>,
                          fctx: &crate::decode::FrameCtx,
                          out: &mut [Vec<u16>; 3]| {
            let mut pred_u = refill(std::mem::take(&mut out[1]), B4 * B4);
            let mut pred_v = refill(std::mem::take(&mut out[2]), B4 * B4);
            // Index loop mirrors the spec's per-position form over several offsets; an iterator would obscure it.
            #[allow(clippy::needless_range_loop)]
            for i in 0..4usize {
                let (dr, dc) = (i / 2, i % 2);
                if mixed && i != 3 {
                    continue;
                }
                let Some((ref_frame, mv, h_filter, v_filter)) = piece[i] else {
                    continue;
                };
                let Some((piece_ref_y, sref_u, sref_v)) = ref_planes_deferred(ref_frame, refpix)
                else {
                    continue;
                };
                let piece_scale = scale_of(
                    (piece_ref_y.true_width, piece_ref_y.true_height),
                    frame_size,
                );
                let (ox, oy) = if mixed {
                    (cpx, cpy)
                } else {
                    (cpx + dc * 2, cpy + dr * 2)
                };
                let (pw, ph) = if mixed { (B4, B4) } else { (2, 2) };
                let (row0, col0) = if mixed { (0, 0) } else { (dr * 2, dc * 2) };
                for (plane, dst) in [(sref_u, &mut pred_u), (sref_v, &mut pred_v)] {
                    let mut buf = vec![0u16; pw * ph];
                    mc::predict_maybe_scaled(
                        &plane.data,
                        plane.width,
                        plane.true_width,
                        plane.true_height,
                        mv_to_q4(ox, mv.1, ss_x(fctx)),
                        mv_to_q4(oy, mv.0, ss_x(fctx)),
                        piece_scale,
                        pw,
                        ph,
                        h_filter,
                        v_filter,
                        &mut buf,
                        fctx,
                    );
                    for row in 0..ph {
                        for col in 0..pw {
                            dst[(row0 + row) * B4 + col0 + col] = buf[row * pw + col];
                        }
                    }
                }
            }
            out[1] = pred_u;
            out[2] = pred_v;
        };
        let (pred_u, pred_v): (Vec<u16>, Vec<u16>);
        let (su, sv) = if pred_deferred(refpix) {
            push_pred(build);
            (Pred::Cur { plane: 1, stride: B4, off: 0 }, Pred::Cur { plane: 2, stride: B4, off: 0 })
        } else {
            let mut built: [Vec<u16>; 3] = Default::default();
            build(y, u, v, refpix, fctx, &mut built);
            let [_, bu, bv] = built;
            (pred_u, pred_v) = (bu, bv);
            (Pred::Inline(&pred_u, B4), Pred::Inline(&pred_v, B4))
        };
        let (u_grid, v_grid) = if last_skip {
            push_mc(
                y,
                u,
                v,
                1,
                cpx,
                cpy,
                B4,
                su,
                &ZERO_RESIDUAL[..B4 * B4],
                fctx,
            );
            push_mc(
                y,
                u,
                v,
                2,
                cpx,
                cpy,
                B4,
                sv,
                &ZERO_RESIDUAL[..B4 * B4],
                fctx,
            );
            (Grid::Zero(B4 * B4), Grid::Zero(B4 * B4))
        } else {
            let around = neighbours.around_mi(group_mi, 8);
            let ug = read_inter_plane(
                dec,
                cdfs,
                TxbSet::Chroma4,
                scan4,
                1,
                around[1],
                0,
                y,
                u,
                v,
                cpx,
                cpy,
                B4,
                base_q_idx,
                su,
                Some(first_tx_type),
                None,
                fctx,
            )?
            .0;
            let vg = read_inter_plane(
                dec,
                cdfs,
                TxbSet::Chroma4,
                scan4,
                2,
                around[2],
                0,
                y,
                u,
                v,
                cpx,
                cpy,
                B4,
                base_q_idx,
                sv,
                Some(first_tx_type),
                None,
                fctx,
            )?
            .0;
            let coded = ug.iter().chain(vg.iter()).any(|&c| c != 0);
            note_sub8_chroma_tx(sub0_tx_type, first_tx_type, coded);
            (ug, vg)
        };
        let round_up_even = |n: usize| n.div_ceil(2) * 2;
        let bound_h = round_up_even(neighbours.mi_rows);
        let bound_w = round_up_even(neighbours.mi_cols);
        let u_state = neighbour_state(&u_grid);
        let v_state = neighbour_state(&v_grid);
        for cell in 0..2usize {
            if cell < 2usize.min(bound_h.saturating_sub(gr)) {
                neighbours.left[gr + cell][1] = u_state;
                neighbours.left[gr + cell][2] = v_state;
            }
            if cell < 2usize.min(bound_w.saturating_sub(gc)) {
                neighbours.above[gc + cell][1] = u_state;
                neighbours.above[gc + cell][2] = v_state;
            }
        }
    }
    // Same two neighbour tails [`decode_leaf_split4`] runs: the partition
    // context of a `BLOCK_4X4` subsize (`partition_context_lookup` bit 0 = 1,
    // reached by `side_mi = 4` here) and the group's chroma coefficient state.
    for cell in 0..2usize {
        neighbours.left_side_mi[gr + cell] = 4;
        neighbours.above_side_mi[gc + cell] = 4;
    }
    Ok(())
}

/// One INTRA 8x4/4x8 leaf inside an INTER frame's sub-8x8 HORZ/VERT partition
/// (lane-sub8intra) -- the shape [`decode_inter_sub8_rect2`] refused by name.
/// Mode syntax is `read_intra_block_mode_info`'s (decodemv.c:1065), not the key
/// frame's above/left pair: `y_mode[size_group_lookup[BLOCK_8X4] == 0]`, NO
/// angle delta (`av1_use_angle_delta` needs `bsize >= BLOCK_8X8`), `uv_mode` +
/// CfL alphas only on the chroma-reference sub-block (`is_chroma_reference`:
/// the SECOND one under 4:2:0), then `filter_intra` off BLOCK_8X4/BLOCK_4X8's
/// own class row (`av1_filter_intra_allowed_bsize` is `wide <= 32 && high <=
/// 32`, so this shape is allowed). Everything below the mode read -- the tx
/// depth symbol, the whole-leaf `TX_8X4`/`TX_4X8` or split `TX_4X4` walk, the
/// 4x4 chroma unit at the group's origin -- is [`decode_leaf_rect8`]'s body for
/// the same shape; the INTER-frame differences are the tx-depth context
/// (`above_txfm`/`left_txfm`),
/// `set_txfm_ctxs` publication, and the inter-side bands + mi map the caller's
/// inter leaves also write (`is_inter = 0`, ref `INTRA_FRAME`, no interp
/// filter).
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_intra_sub8_leaf(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    grid: &mut MiGrid,
    group_mi: (usize, usize),
    lmi: (usize, usize),
    // `(bw, bh)`: 8x4 / 4x8 under HORZ/VERT, 4x4 under SPLIT.
    (bw, bh): (usize, usize),
    has_chroma: bool,
    skip: bool,
    scan4: &[u16],
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    base_q_idx: u8,
    enable_filter_intra: bool,
    tx_select: bool,
    reduced_tx_set: bool,
    fctx: &crate::decode::FrameCtx,
) -> Result<usize> {
    // lane-av1mono: a monochrome leaf codes no chroma mode (libaom
    // `decodemv.c:933`).
    let has_chroma = has_chroma && !mono(fctx);
    let vert = bw < bh;
    let (w_mi, h_mi) = (bw / MI, bh / MI);
    let (gr, gc) = group_mi;
    let (r, c) = (gr / (SUB / MI), gc / (SUB / MI));
    let (above_mode, left_mode) = neighbours.modes_above_left(r, c);
    let leaf_above = neighbours.mode_above_mi(lmi.0, lmi.1).unwrap_or(above_mode);
    let leaf_left = neighbours.mode_left_mi(lmi.0, lmi.1).unwrap_or(left_mode);
    let step = crate::envflags::env_flag!("EC_TRACE_MODE_STEP");
    // `size_group_lookup[BLOCK_8X4] == size_group_lookup[BLOCK_4X8] == 0`
    // (libaom common_data.h:61).
    crate::msac::SymbolDecoder::set_symr_cdf("y_mode");
    let mode = dec.symbol(&mut cdfs.y_mode[0]);
    if step {
        eprintln!(
            "EC_ISTEP mi_row={} mi_col={} name=mode val={mode} rng={}",
            lmi.0,
            lmi.1,
            dec.debug_state().0
        );
    }
    // No angle delta below 8x8 (`av1_use_angle_delta`), for luma or chroma.
    let angle_delta_y = 0;
    let (uv_mode, alpha) = if has_chroma {
        // `is_cfl_allowed` (blockd.h): away from lossless it is
        // `wide <= 32 && high <= 32` -- true for this shape; at lossless the
        // CHROMA plane block must be a single TX_4X4, so at 4:4:4 only the
        // 4x4 leaf keeps the 14-symbol CFL alphabet while an 8x4/4x8 leaf
        // reads the 13-symbol `uv_mode_no_cfl` one (the t6 frame-1 8x4 leaf
        // desynced at its uv_mode symbol reading the CFL alphabet,
        // lane-av1llintercdf). At 4:2:0 a sub-8 shape's chroma block is
        // always 4x4-or-smaller, so the predicate stays true and this read
        // is unchanged.
        let uv_mode = if cfl_allowed_px(bw, bh, fctx) {
            crate::msac::SymbolDecoder::set_symr_cdf("uv_mode");
            dec.symbol(&mut cdfs.uv_mode_cfl[mode])
        } else {
            crate::msac::SymbolDecoder::set_symr_cdf("uv_mode");
            dec.symbol(&mut cdfs.uv_mode_no_cfl[mode])
        };
        if step {
            eprintln!(
                "EC_ISTEP mi_row={} mi_col={} name=uv_mode val={uv_mode} rng={}",
                lmi.0,
                lmi.1,
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
        (uv_mode, alpha)
    } else {
        (DC_PRED, None)
    };
    let mut filter_intra = None;
    if mode == DC_PRED && enable_filter_intra {
        let fi_class = if bw == bh {
            filter_intra_size_class(bw).expect("BLOCK_4X4 has its own filter_intra row")
        } else {
            filter_intra_size_class_rect(bw, bh)
                .expect("BLOCK_4X8/BLOCK_8X4 are inside av1_filter_intra_allowed_bsize")
        };
        let use_fi = dec.symbol(&mut cdfs.filter_intra[fi_class]);
        if step {
            eprintln!(
                "EC_ISTEP mi_row={} mi_col={} name=use_filter_intra val={use_fi} rng={}",
                lmi.0,
                lmi.1,
                dec.debug_state().0
            );
        }
        if use_fi != 0 {
            hit!(FILTER_INTRA_HITS);
            hit!(FILTER_INTRA_RECT_HITS);
            hit!(FILTER_INTRA_RECT_SUB8_HITS);
            let fi = dec.symbol(&mut cdfs.filter_intra_mode);
            if step {
                eprintln!(
                    "EC_ISTEP mi_row={} mi_col={} name=filter_intra_mode val={fi} rng={}",
                    lmi.0,
                    lmi.1,
                    dec.debug_state().0
                );
            }
            filter_intra = Some(fi);
        }
    }
    // `read_block_tx_size`' non-var-tx arm: an INTRA block of an inter frame
    // reads `read_tx_size(allow_select = 1)` -- `bsize_to_tx_size_cat
    // (BLOCK_4X8) == 0` (blockd.h:1344, depth table entry 1), the same
    // 2-symbol category [`decode_leaf_rect8`] reads -- with the var-tx
    // context bands, which on an inter frame are live.
    // `block_signals_txsize(BLOCK_4X4)` is false (blockd.h:1027): the 4x4 leaf
    // codes no depth symbol at all, its transform is its own size.
    // lane-av1mix: the lossless early return (`decodeframe.c:1203`) sits
    // before the `TX_MODE_SELECT` test, so a lossless-segment rect leaf codes
    // no depth symbol either (sweep of the class fixed at the key-frame pair
    // path).
    let depth = if tx_select && bw != bh && !lossless(fctx) {
        let ctx = tx_size_context_txfm_rect(neighbours, lmi, bw, bh, fctx);
        let d = dec.symbol(&mut cdfs.tx_size_cat0[ctx]);
        if step {
            eprintln!(
                "EC_ISTEP mi_row={} mi_col={} name=tx_depth val={d} ctx={ctx} cat=0 rng={}",
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
        hit!(RECT8_SPLIT_TX_HITS);
    }
    let split = depth != 0 || (lossless(fctx) && bw != bh);
    let (px, py) = (lmi.1 * MI, lmi.0 * MI);
    let reach = if bw == bh {
        Reach::of(bw, px, py, y.width, y.height, fctx)
    } else {
        Reach::of_rect(bw, bh, px, py, y.width, y.height, fctx)
    };
    let smooth_neighbor = is_smooth_mode(leaf_above) || is_smooth_mode(leaf_left);
    if smooth_neighbor {
        hit!(SMOOTH_LUMA_HITS);
    }
    neighbours.record_mode_mi(lmi.0, lmi.1, w_mi, h_mi, mode);
    let scan: &[u16] = if vert { &SCAN_4X8 } else { &SCAN_8X4 };
    if split {
        // `sub_tx_size_map[TX_4X8] == TX_4X4`: two 4x4 units along the long
        // axis, each predicted off the previous one's reconstruction.
        for tu in 0..2usize {
            let tu_mi = if vert {
                (lmi.0 + tu, lmi.1)
            } else {
                (lmi.0, lmi.1 + tu)
            };
            let (tu_px, tu_py) = (tu_mi.1 * MI, tu_mi.0 * MI);
            let tu_reach = tu_reach(
                bw,
                bh,
                tu_px - px,
                tu_py - py,
                4,
                reach,
                px,
                py,
                y.width,
                y.height,
                fctx,
            );
            if skip {
                let zeros = vec![0i32; 16];
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
                    &zeros,
                    None,
                    filter_intra,
                    smooth_neighbor,
                    fctx,
                );
                neighbours.record_mi_luma(tu_mi, 4, &zeros);
                continue;
            }
            let tu_around = neighbours.around_mi(tu_mi, 4)[0];
            let tu_skip_ctx = neighbours.luma_skip_ctx(tu_mi, 1);
            let tu_grid = read_plane(
                dec,
                cdfs,
                if reduced_tx_set {
                    TxbSet::Luma4
                } else {
                    TxbSet::Luma4Set1
                },
                scan4,
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
                TX4,
                base_q_idx,
                None,
                filter_intra,
                Some(tu_skip_ctx),
                smooth_neighbor,
                fctx,
            )?;
            neighbours.record_mi_luma(tu_mi, 4, &tu_grid);
        }
    } else if bw == bh {
        // A `BLOCK_4X4` leaf's luma TU *is* the whole block, so `get_txb_ctx`
        // fixes `txb_skip_ctx` at 0 (`luma_skip_ctx: None`) -- the same square
        // reader [`decode_leaf_split4`] uses.
        if skip {
            push_intra(
                y,
                u,
                v,
                0,
                px,
                py,
                4,
                mode,
                angle_delta_y,
                reach,
                &ZERO_RESIDUAL[..16],
                None,
                filter_intra,
                smooth_neighbor,
                fctx,
            );
            neighbours.record_mi_luma(lmi, 4, &ZERO_RESIDUAL[..16]);
        } else {
            let tu_around = neighbours.around_mi(lmi, 4)[0];
            let tu_grid = read_plane(
                dec,
                cdfs,
                if reduced_tx_set {
                    TxbSet::Luma4
                } else {
                    TxbSet::Luma4Set1
                },
                scan4,
                0,
                tu_around,
                mode,
                mode,
                angle_delta_y,
                reach,
                y,
                u,
                v,
                px,
                py,
                4,
                TX4,
                base_q_idx,
                None,
                filter_intra,
                None,
                smooth_neighbor,
                fctx,
            )?;
            neighbours.record_mi_luma(lmi, 4, &tu_grid);
        }
    } else {
        let grid_levels = if skip {
            let zeros = Grid::Zero(bw * bh);
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
            // The leaf's luma TU *is* the leaf, so `get_txb_ctx` fixes
            // `txb_skip_ctx` at 0.
            let around = neighbours.around_mi_rect(lmi, bw, bh)[0];
            let set = if reduced_tx_set {
                TxbSet::LumaRect4x8
            } else {
                TxbSet::LumaRect4x8Set1
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
                // lane-d792 sweep: the block's segment `SEG_LVL_ALT_Q`, as at
                // every other dequant site (class `parsed-then-discarded`).
                block_q_idx(fctx),
                plane_q_delta(0, fctx).0,
                plane_q_delta(0, fctx).1,
                tx_type,
                block_iqmatrix(fctx, 0, bw, bh, tx_type),
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
                &residual,
                None,
                filter_intra,
                smooth_neighbor,
                fctx,
            );
            levels
        };
        let state = neighbour_state(&grid_levels);
        for cell in 0..h_mi {
            if lmi.0 + cell < neighbours.mi_rows {
                neighbours.left[lmi.0 + cell][0] = state;
            }
        }
        for cell in 0..w_mi {
            if lmi.1 + cell < neighbours.mi_cols {
                neighbours.above[lmi.1 + cell][0] = state;
            }
        }
    }
    let (tx_w, tx_h) = if split { (4, 4) } else { (bw, bh) };
    neighbours.fill_skip_grid_rect(lmi, w_mi, h_mi, skip);
    neighbours.fill_lf_grid_rect(lmi, w_mi, h_mi, tx_w as u8, tx_h as u8, 0, fctx);
    // `set_txfm_ctxs(tx_size, n4_w, n4_h, skip && is_inter == 0, xd)`: an
    // intra block of an inter frame publishes its TRANSFORM dims, never its
    // block dims (blockd.h). Missing this left the next block reading the
    // previous block's band (the lane-intra16x4 finding for the 16x4 strip).
    for i in 0..w_mi {
        if let Some(cell) = neighbours.above_txfm.get_mut(lmi.1 + i) {
            *cell = tx_w as u8;
        }
    }
    for i in 0..h_mi {
        if let Some(cell) = neighbours.left_txfm.get_mut(lmi.0 + i) {
            *cell = tx_h as u8;
        }
    }
    neighbours.above_mode[c] = mode;
    neighbours.left_mode[r] = mode;
    // The inter-side bands and the mi map, exactly as the square
    // intra-in-inter block writes them: `is_inter = 0`, `INTRA_FRAME` ref, no
    // interpolation filter, no skip mode.
    neighbours.record_inter_rect_mi(lmi, w_mi, h_mi, skip, false, 0, [3, 3], false);
    grid.fill_rect(
        lmi.0,
        lmi.1,
        h_mi,
        w_mi,
        MiInfo {
            is_inter: false,
            ref_frame: -1,
            ref_frame1: NO_REF1,
            mv1: (0, 0),
            mv: (0, 0),
            is_new_mv: false,
            size: w_mi as u8,
            size_h: h_mi as u8,
            is_global_mv0: false,
            is_global_mv1: false,
        },
    );
    hit_do! {
        SUB8_INTRA_RECT_HITS.with(|h| {
            let mut s = h.get();
            s[usize::from(vert)] += 1;
            s[2] += usize::from(has_chroma);
            h.set(s);
        });
    }
    // lane-t900 r23 (class [[new-map-ignores-tile-edge]]: a neighbour map
    // needs EVERY writer): a sub-8x8 group published no palette state, so
    // once an 8x8 leaf of the same row can BE a palette block, the group
    // left the row's band holding that earlier block's size and the next
    // intra block read `av1_get_palette_mode_ctx` one too high (measured:
    // mi(30,30) of decode-order frame 1, ours ctx 1 where libaom gathers
    // 0). No sub-8x8 piece can itself be a palette (`av1_allow_palette`
    // needs `bsize >= BLOCK_8X8`), so the group always clears.
    //
    // lane-av1mono: this publish is NOT chroma work and must run BEFORE the
    // `!has_chroma` return below. libaom's mi grid holds
    // `palette_size[0] == 0` in every cell of such a group whatever the
    // plane count, and on a monochrome frame EVERY sub-8 leaf passes
    // `has_chroma == false`, so a publish placed after the return never ran
    // at all: the band kept an earlier palette block's size, the next block
    // read `palette_y_mode` off CDF row 1 where libaom reads row 0, decoded
    // a palette libaom never coded, and the tile desynced one block later
    // (measured: gray testsrc2 320x240, decode-order frame 33, mi(42,50),
    // ours ctx 2 where libaom gathers 1).
    record_strip_palette(neighbours, (gr, gc), 8, 8, 0, [0u16; 8], 0, [0u16; 8]);
    if !has_chroma {
        // lane-av1-422sub8rect: 4:2:2 has no shared group unit whose tail
        // would stamp the mi map (`is_chroma_reference` runs per piece), so
        // a non-referenced piece publishes its own cell -- libaom leaves
        // `uv_mode` unset on it, `is_smooth` reads 0, i.e. DC_PRED.
        if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
            neighbours.record_uv_mode_mi(lmi.0, lmi.1, 1, 1, DC_PRED);
        }
        return Ok(mode);
    }
    // lane-av1-422sub8rect: at 4:2:2 this (odd-column) piece is its plane's
    // OWN chroma reference -- the key-frame split group's per-leaf TX_4X4
    // model, floored anchor and per-chroma-cell context included
    // ([`sub8_leaf_chroma422`], measured on the pinned t422 fixture frame 0:
    // groups (18,54)/(30,42) code U+V on both odd-column leaves), never a
    // shared group unit. Inter frames code no intrabc, so `intrabc_dv` is
    // `None` here.
    if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
        sub8_leaf_chroma422(
            dec,
            cdfs,
            neighbours,
            lmi,
            uv_mode,
            0,
            alpha,
            skip,
            None,
            y,
            u,
            v,
            scan4,
            base_q_idx,
            neighbours.smooth_uv_neighbour(lmi.0, lmi.1, r, c, fctx),
            (bw, bh),
            fctx,
        )?;
        return Ok(mode);
    }
    // The group's single 4x4 chroma unit, intra-predicted from THIS
    // (chroma-reference) sub-block's own `uv_mode` -- `is_sub8x8_inter`
    // (reconinter_template.inc:54) returns false as soon as any mi of the
    // group is intra, and the decoder reconstructs chroma with the
    // chroma-reference block's own type. Same body as [`decode_leaf_rect8`]'s
    // post-loop chroma.
    //
    // lane-av1-444: at ss 0/0 EVERY piece is its own chroma reference
    // (`is_chroma_reference`, av1_common_int.h:1459), so the unit anchors at
    // the piece's own position with the piece's own shape -- a 4x4 unit under
    // SPLIT, the leaf's own 8x4/4x8 unit under HORZ/VERT (`ChromaRect8x4`,
    // tx_type from the uv mode via `Intra_Mode_To_Tx_Type`, which decides the
    // eob CDF's class-1 row, decodetxb.c:214).
    let chroma_444 = ss_x(fctx) == 0 && ss_y(fctx) == 0;
    let (gpx, gpy) = (gc * MI, gr * MI);
    let (cpx, cpy) = (gpx >> ss_x(fctx), gpy >> ss_y(fctx));
    let (unit_x, unit_y) = if chroma_444 { (px, py) } else { (cpx, cpy) };
    let group_reach = Reach::of(8, gpx, gpy, y.width, y.height, fctx);
    let reach_c = if chroma_444 && bw != bh {
        Reach::of_rect(bw, bh, px, py, y.width, y.height, fctx)
    } else if chroma_444 {
        Reach::of(bw, px, py, y.width, y.height, fctx)
    } else {
        group_reach
    };
    let (band_r, band_c) = if chroma_444 {
        (lmi.0 / (SUB / MI), lmi.1 / (SUB / MI))
    } else {
        (r, c)
    };
    let smooth_neighbor_uv = neighbours.smooth_uv_neighbour(
        if chroma_444 { lmi.0 } else { gr },
        if chroma_444 { lmi.1 } else { gc },
        band_r,
        band_c,
        fctx,
    );
    let uv_predict_mode = if uv_mode == UV_CFL_PRED {
        DC_PRED
    } else {
        uv_mode
    };
    let angle_delta_uv = 0;
    neighbours.above_uv_mode[band_c] = uv_predict_mode;
    neighbours.left_uv_mode[band_r] = uv_predict_mode;
    if chroma_444 {
        neighbours.record_uv_mode_mi(lmi.0, lmi.1, w_mi, h_mi, uv_predict_mode);
    } else {
        neighbours.record_uv_mode_mi(gr, gc, 2, 2, uv_predict_mode);
    }

    let ac = alpha.map(|_| {
        if chroma_444 {
            cfl_src_rect(px, py, bw, bh)
        } else {
            cfl_src(gpx, gpy, 8)
        }
    });
    let mut chroma_stamped_per_unit = false;
    let (u_grid, v_grid): (Grid, Grid) = if skip {
        if alpha.is_some() {
            hit!(CHROMA_SKIP_CFL_HITS);
            hit!(SKIP_CFL_HITS);
        }
        if chroma_444 && bw != bh {
            let zeros = Grid::Zero(bw * bh);
            push_intra_rect(
                y,
                u,
                v,
                1,
                unit_x,
                unit_y,
                bw,
                bh,
                uv_predict_mode,
                angle_delta_uv,
                reach_c,
                &zeros,
                alpha.zip(ac).map(|((au, _), ac)| (au, ac)),
                None,
                smooth_neighbor_uv,
                fctx,
            );
            push_intra_rect(
                y,
                u,
                v,
                2,
                unit_x,
                unit_y,
                bw,
                bh,
                uv_predict_mode,
                angle_delta_uv,
                reach_c,
                &zeros,
                alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
                None,
                smooth_neighbor_uv,
                fctx,
            );
            (zeros, Grid::Zero(bw * bh))
        } else {
            push_intra(
                y,
                u,
                v,
                1,
                unit_x,
                unit_y,
                4,
                uv_predict_mode,
                angle_delta_uv,
                reach_c,
                &ZERO_RESIDUAL[..16],
                alpha.zip(ac).map(|((au, _), ac)| (au, ac)),
                None,
                smooth_neighbor_uv,
                fctx,
            );
            push_intra(
                y,
                u,
                v,
                2,
                unit_x,
                unit_y,
                4,
                uv_predict_mode,
                angle_delta_uv,
                reach_c,
                &ZERO_RESIDUAL[..16],
                alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
                None,
                smooth_neighbor_uv,
                fctx,
            );
            (Grid::Zero(16), Grid::Zero(16))
        }
    } else if chroma_444 && bw != bh {
        if lossless(fctx) {
            // lane-av1sub8rect444: lossless codes TX_4X4 on EVERY plane
            // (`av1_get_tx_size`, blockd.h:1383), so this leaf's 8x4/4x8
            // chroma plane block is a 1x2/2x1 raster of TX_4X4 units per
            // plane, PLANE-MAJOR -- the same per-unit walk
            // [`read_intra_chroma_lossless`] runs for whole blocks and this
            // leaf's own luma `split` runs above, not one TX_8X4/TX_4X8
            // rect unit. The rect read was wrong twice over: its unit count
            // left the second unit's coefficients in the bitstream, and its
            // skip ctx fixed at above+left missed `get_txb_ctx`'s
            // plane-block-bigger-than-transform term (libaom ctx 10+above
            // +left, our tables' +3), where the oracle read ctx 11≡4.
            let (mut uo, mut vo) = (Vec::new(), Vec::new());
            for plane_idx in 1..3 {
                for tu in 0..2usize {
                    let (ox, oy) = if vert { (0, tu * 4) } else { (tu * 4, 0) };
                    // libaom's `max_blocks_wide/high` clip: a unit whose
                    // origin is past the frame edge is not coded at all
                    // (`decode_token_recon_block`). At 4:4:4 the unit's
                    // chroma origin is the leaf's own (px, py).
                    if px + ox >= u.true_width || py + oy >= u.true_height {
                        continue;
                    }
                    // At 4:4:4 a TX_4X4 chroma unit IS one mi cell.
                    let tu_mi = (lmi.0 + oy / MI, lmi.1 + ox / MI);
                    let tu_around = neighbours.around_mi(tu_mi, MI);
                    let tu_r =
                        tu_reach(bw, bh, ox, oy, 4, reach_c, px, py, y.width, y.height, fctx);
                    // The plane block (8x4/4x8) is bigger than the TX_4X4
                    // transform, so `get_txb_ctx`'s chroma ctx_offset is 10,
                    // not 7 -- our chroma tables carry the offset-10 rows at
                    // +3, the same `Some(3)` [`read_intra_chroma_lossless`]
                    // derives from `blk_w > 4 || blk_h > 4`.
                    let tu_grid = read_plane(
                        dec,
                        cdfs,
                        TxbSet::Chroma4,
                        scan4,
                        plane_idx,
                        tu_around[plane_idx],
                        mode,
                        uv_predict_mode,
                        angle_delta_uv,
                        tu_r,
                        y,
                        u,
                        v,
                        px + ox,
                        py + oy,
                        4,
                        TX4,
                        base_q_idx,
                        None,
                        None,
                        Some(3),
                        smooth_neighbor_uv,
                        fctx,
                    )?;
                    // The unit's own coefficient context, stamped per unit
                    // (`av1_set_contexts`); the tail's whole-leaf chroma
                    // stamp below is skipped for this shape.
                    neighbours.record_mi_chroma(tu_mi, 4, 4, plane_idx, &tu_grid);
                    hit!(CHROMA_SPLIT_TX_HITS);
                    let dst = mu_units(if plane_idx == 1 { &mut uo } else { &mut vo }, bw * bh);
                    for row in 0..4 {
                        let start = (oy + row) * bw + ox;
                        dst[start..start + 4].copy_from_slice(&tu_grid[row * 4..][..4]);
                    }
                }
            }
            chroma_stamped_per_unit = true;
            hit!(LLSUB8RECT444_CHROMA_WALK_HITS);
            (
                if uo.is_empty() {
                    Grid::Zero(bw * bh)
                } else {
                    Grid::Own(uo)
                },
                if vo.is_empty() {
                    Grid::Zero(bw * bh)
                } else {
                    Grid::Own(vo)
                },
            )
        } else {
            // The leaf's own TX_8X4/TX_4X8 chroma unit: it IS the chroma plane
            // block, so `get_txb_ctx` fixes the skip context at above+left.
            // lane-av1422gather3: 4:4:4-only -- this `ca` arm sits inside the
            // `chroma_444` guard (ss 0/0), so the 4:2:2 pair rule does not
            // apply here. Probed at ss (1,0) over all 21 committed 4:2:2
            // cells: 0 hits.
            let ca = neighbours.around_mi_rect(lmi, bw, bh);
            let scan_c: &[u16] = if vert { &SCAN_4X8 } else { &SCAN_8X4 };
            let uv_tx = default_intra_tx_type(uv_predict_mode as u8);
            let mut read_chroma = |plane: usize,
                                   around: (bool, bool, i32),
                                   alpha_uv: Option<(i32, CflSrc)>|
             -> Result<Grid> {
                let mut coding = cdfs.txb(TxbSet::ChromaRect8x4, 0);
                let skip_ctx = usize::from(around.0) + usize::from(around.1);
                let (levels, tx_type) = read_coeffs_rect(
                    dec,
                    &mut coding,
                    scan_c,
                    bw,
                    bh,
                    skip_ctx,
                    dc_sign_ctx(around.2),
                    uv_tx,
                    plane,
                )?;
                let residual = dequant_and_inverse_typed_wh(
                    &levels,
                    bw,
                    bh,
                    crate::decode::bit_depth(fctx),
                    block_q_idx(fctx),
                    plane_q_delta(plane, fctx).0,
                    plane_q_delta(plane, fctx).1,
                    tx_type,
                    block_iqmatrix(fctx, plane, bw, bh, tx_type),
                );
                push_intra_rect(
                    y,
                    u,
                    v,
                    plane,
                    unit_x,
                    unit_y,
                    bw,
                    bh,
                    uv_predict_mode,
                    angle_delta_uv,
                    reach_c,
                    &residual,
                    alpha_uv,
                    None,
                    smooth_neighbor_uv,
                    fctx,
                );
                Ok(levels)
            };
            let ug = read_chroma(1, ca[1], alpha.zip(ac).map(|((au, _), ac)| (au, ac)))?;
            let vg = read_chroma(2, ca[2], alpha.zip(ac).map(|((_, av), ac)| (av, ac)))?;
            (ug, vg)
        }
    } else {
        let chroma_around = neighbours.around_mi(
            if chroma_444 { lmi } else { group_mi },
            if chroma_444 { 4 } else { 8 },
        );
        let ug = read_plane(
            dec,
            cdfs,
            TxbSet::Chroma4,
            scan4,
            1,
            chroma_around[1],
            uv_mode,
            uv_predict_mode,
            angle_delta_uv,
            reach_c,
            y,
            u,
            v,
            unit_x,
            unit_y,
            4,
            TX4,
            base_q_idx,
            alpha.zip(ac).map(|((au, _), ac)| (au, ac)),
            None,
            None,
            smooth_neighbor_uv,
            fctx,
        )?;
        let vg = read_plane(
            dec,
            cdfs,
            TxbSet::Chroma4,
            scan4,
            2,
            chroma_around[2],
            uv_mode,
            uv_predict_mode,
            angle_delta_uv,
            reach_c,
            y,
            u,
            v,
            unit_x,
            unit_y,
            4,
            TX4,
            base_q_idx,
            alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
            None,
            None,
            smooth_neighbor_uv,
            fctx,
        )?;
        (ug, vg)
    };
    // The 4:4:4 lossless walk above already stamped these bands PER UNIT
    // (raster-order last writer per cell, exactly libaom's per-entry last
    // update); the whole-leaf aggregate would clobber the two units' distinct
    // states with one, so it runs only for the one-unit shapes (the same rule
    // [`decode_leaf8`]'s intrabc arm and the inter 8x8 leaf obey). The walk
    // must skip the 4:2:0 group-tail below too: it spans the GROUP's two mi
    // rows/cols with one composed state, which would overwrite the walk's
    // own stamps with the whole-leaf aggregate.
    if chroma_444 {
        if !chroma_stamped_per_unit {
            neighbours.record_mi_chroma(lmi, bw, bh, 1, &u_grid);
            neighbours.record_mi_chroma(lmi, bw, bh, 2, &v_grid);
        }
    } else {
        let round_up_even = |n: usize| n.div_ceil(2) * 2;
        let bound_h = round_up_even(neighbours.mi_rows);
        let bound_w = round_up_even(neighbours.mi_cols);
        let u_state = neighbour_state(&u_grid);
        let v_state = neighbour_state(&v_grid);
        for cell in 0..2usize {
            if cell < 2usize.min(bound_h.saturating_sub(gr)) {
                neighbours.left[gr + cell][1] = u_state;
                neighbours.left[gr + cell][2] = v_state;
            }
            if cell < 2usize.min(bound_w.saturating_sub(gc)) {
                neighbours.above[gc + cell][1] = u_state;
                neighbours.above[gc + cell][2] = v_state;
            }
        }
    }
    Ok(mode)
}

/// Decodes a `PARTITION_HORZ`/`PARTITION_VERT` of one 8x8 INTER block into two
/// `BLOCK_8X4`/`BLOCK_4X8` inter leaves (lane-intersub8 r3) -- the inter twin of
/// [`decode_leaf_rect8`] and the rectangular sibling of
/// [`decode_inter_sub8_split4`], whose element order it follows exactly.
/// Per sub-block, in libaom `decodemv.c` `read_inter_frame_mode_info` order:
///
/// * `segment_id`, `skip`, `cdef`/`delta_q`/`delta_lf`, `intra_inter`;
/// * NO `skip_mode` symbol: `is_comp_ref_allowed(BLOCK_8X4)` is false
///   (`av1_is_comp_ref_allowed` needs both sides >= 8), so
///   `read_skip_mode` returns 0 without reading and `comp_mode` is never read
///   either (`read_ref_frames`' `is_comp_ref_allowed` guard) -- single
///   reference only;
/// * NO `interintra` and NO `motion_mode`/OBMC/warp symbol: every one of those
///   gates needs `block_size_wide >= 8 && block_size_high >= 8`
///   (`is_interintra_allowed_bsize`, `motion_mode_allowed`), and one side is 4;
/// * `read_mb_interp_filter` DOES run for both sub-blocks.
///
/// The shape's own differences from the four-way split:
///
/// * `block_signals_txsize(BLOCK_8X4)` is true (`bsize > BLOCK_4X4`,
///   blockd.h:1027), so a non-skipped sub-block reads ONE `txfm_partition`
///   symbol under `TX_MODE_SELECT` at `max_txsize_rect_lookup[BLOCK_8X4] ==
///   TX_8X4` ([`read_var_tx_size`]); split resolves to two `TX_4X4` units
///   (`sub_tx_size_map[TX_8X4] == TX_4X4`, libaom's early return), no split to
///   the whole-leaf `TX_8X4`;
/// * the luma coefficient set is [`TxbSet::LumaRect8x4Inter`] (the 8x8
///   coefficient class with a 32-position `eob_pt` and the INTER `TX_4X4`
///   `tx_type` row) for the whole-leaf unit, and the square
///   [`TxbSet::Luma4Inter`] for a split `TX_4X4` unit;
/// * chroma is coded ONCE, on the sub-block that is the chroma reference:
///   `is_chroma_reference` (`av1_common_int.h`) is
///   `((mi_row & 1) || !(bh4 & 1) || !ss_y) && ((mi_col & 1) || !(bw4 & 1) ||
///   !ss_x)`, i.e. under 4:2:0 the BOTTOM 8x4 (`bh4 == 1`, odd) and the RIGHT
///   4x8 (`bw4 == 1`) -- always the second sub-block here;
/// * that chroma unit's prediction is the sub8x8 one (`reconinter.c`
///   `build_inter_predictors_sub8x8`: `row_start = -(block_size_high == 4 &&
///   ss_y)`, `col_start = -(block_size_wide == 4 && ss_x)`), two 4x2 (HORZ) or
///   2x4 (VERT) chroma pieces, each from its OWN sub-block's reference, mv and
///   interpolation filters.
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_inter_sub8_rect2(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    grid: &mut MiGrid,
    group_mi: (usize, usize),
    vert: bool,
    mi_cols: u32,
    mi_rows: u32,
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    refpix: &RefPix<'_>,
    sign_bias_table: &SignBiasTable,
    global_motion: &[ec_av1_syntax::WarpParams; 7],
    base_q_idx: u8,
    scan4: &[u16],
    allow_high_precision_mv: bool,
    force_integer_mv: bool,
    interp_fixed: Option<mc::InterpFilterKind>,
    enable_dual_filter: bool,
    tpl_frame: Option<&TplFrameArgs>,
    reduced_tx_set: bool,
    frame_size: (usize, usize),
    fctx: &crate::decode::FrameCtx,
) -> Result<()> {
    const LAST_FRAME: i8 = 1;
    // The 8x8 group's own square stride: both leaves' prediction buffers are
    // laid out at it, as every other rect inter unit here is.
    const SIDE: usize = 8;
    let (bw, bh) = if vert {
        (4usize, 8usize)
    } else {
        (8usize, 4usize)
    };
    let (w_mi, h_mi) = (bw / MI, bh / MI);
    hit_do! {
        SUB8_INTER_RECT_HITS.with(|c| {
            let mut h = c.get();
            h[usize::from(vert)] += 1;
            c.set(h);
        });
    }
    let (gr, gc) = group_mi;
    let (gpx, gpy) = (gc * MI, gr * MI);
    let (cpx, cpy) = (gpx >> ss_x(fctx), gpy >> ss_y(fctx));
    // Per-sub-block state the group's one chroma unit predicts from.
    let mut piece: [Option<(i8, (i32, i32), mc::InterpFilterKind, mc::InterpFilterKind)>; 2] =
        [None; 2];
    let mut first_tx_type = TxType::DctDct;
    let mut sub0_tx_type = TxType::DctDct;
    // lane-av1-444: `is_chroma_reference` is unconditional at subsampling 0/0
    // (av1_common_int.h:1459), so every 8x4/4x8 piece is its own chroma
    // reference and codes its own 8x4/4x8 U/V units right after its luma --
    // there is no shared group unit (aomdec EC_COEFF on
    // `av1-profile1-444.ivf` decode-order frame 1, mi (3,8)).
    let chroma_444 = ss_x(fctx) == 0 && ss_y(fctx) == 0;
    // lane-av1-422blockhalf: at ss (1,0) the chroma reference is PER PIECE
    // too, keyed by `is_chroma_reference`'s own terms: an 8x4 HORZ piece is
    // 2 mi wide (`!(bw & 1)`), so BOTH pieces of the group are references;
    // a 4x8 VERT piece is 1 mi wide, so only the ODD-column piece is
    // (`mi_col & 1`) -- measured on the pinned t422 fixture frame 1: the
    // horz group at mi (8,10)/(9,10) reads U+V TX_4X4 after EACH piece's
    // luma (rng 37134/37852 then 63752/50952), and the vert group at
    // (3,10)/(3,11) reads U+V on the odd-column piece (3,11) alone. The old
    // `i == 1` rule coded the group's chroma on the wrong piece (and, for
    // horz, half of the units) and desynced frame 1 at (8,10)'s uv_mode.
    let chroma_422 = ss_x(fctx) == 1 && ss_y(fctx) == 0;
    // 4:4:4: every piece. 4:2:2: both horz pieces, the odd-column vert piece.
    // 4:2:0 keeps its existing second-piece model untouched.
    let piece_is_chroma_ref = |i: usize| -> bool {
        if ss_x(fctx) == 0 && ss_y(fctx) == 0 {
            true
        } else if chroma_422 {
            !vert || ((gc + i) & 1) == 1
        } else {
            i == 1
        }
    };
    // This piece's first transform's luma `tx_type` (libaom `mbmi->tx_type`):
    // an inter block's chroma plane never codes its own symbol but inherits
    // the colocated luma type (`av1_get_tx_type`, blockd.h:1291).
    let mut piece_tx_type = TxType::DctDct;
    let mut last_skip = true;
    // Set when the chroma-reference sub-block is INTRA: it coded the group's
    // one chroma unit itself.
    let mut intra_chroma = false;
    for i in 0..2usize {
        let lmi = if vert { (gr, gc + i) } else { (gr + i, gc) };
        if lmi.0 >= mi_rows as usize || lmi.1 >= mi_cols as usize {
            continue;
        }
        // libaom's `xd->above_mbmi`/`xd->left_mbmi` are the neighbour block as
        // DECODED, so leaf 1 sees leaf 0's own `bsize` -- the group tail below
        // publishes the side bands only once both leaves are done, which left
        // leaf 1 reading whatever block preceded the GROUP. `get_tx_size_context`
        // (pred_common.h:342) takes `block_size_wide[above_mbmi->bsize]` from it,
        // so an intra 8x4 leaf 1 read `tx_size_cat0` row 1 where aomdec reads
        // row 2 (class tx-grid-published-block-side).
        if i == 1 {
            if vert {
                for cell in 0..h_mi {
                    neighbours.left_side_mi[gr + cell] = bh as u8;
                }
                neighbours.above_side_mi[gc] = bw as u8;
            } else {
                for cell in 0..w_mi {
                    neighbours.above_side_mi[gc + cell] = bw as u8;
                }
                neighbours.left_side_mi[gr] = bh as u8;
            }
        }
        let (rmi, cmi) = lmi;
        let (px, py) = (cmi * MI, rmi * MI);
        inter_segment_id(dec, cdfs, rmi, cmi, w_mi, h_mi, false, true, fctx);
        let skip_ctx =
            usize::from(neighbours.above_skip[cmi]) + usize::from(neighbours.left_skip[rmi]);
        let skip = dec.symbol(&mut cdfs.skip[skip_ctx]) == 1;
        inter_segment_id(dec, cdfs, rmi, cmi, w_mi, h_mi, skip, false, fctx);
        maybe_read_cdef_idx(dec, rmi, cmi, (2, 2), skip, fctx);
        maybe_read_delta_q(dec, cdfs, rmi, cmi, false, skip, fctx);
        maybe_read_delta_lf(dec, cdfs, rmi, cmi, false, skip, fctx);
        let (has_above, has_left) = (rmi > neighbours.tile_row0_mi, cmi > neighbours.tile_col0_mi);
        let ii_ctx = intra_inter_ctx(
            has_above,
            has_left,
            neighbours.above_inter[cmi],
            neighbours.left_inter[rmi],
        );
        let is_inter = dec.symbol(&mut cdfs.intra_inter[ii_ctx]) == 1;
        if !is_inter {
            if crate::envflags::env_flag!("EC_SUB8INTRA") {
                eprintln!(
                    "EC_SUB8INTRA shape={}x{} sub={i} has_chroma={} skip={skip} mi={rmi},{cmi} tx_select={}",
                    bw,
                    bh,
                    piece_is_chroma_ref(i),
                    fctx.tx_select_inter.with(std::cell::Cell::get),
                );
            }
            // lane-sub8x4 r2: refusal and bypass lifted -- see the split arm's
            // note above for the gate that proves this leaf.
            // Its chroma rule is `is_sub8x8_inter`'s: when the
            // chroma-reference (second) sub-block is the intra one, the
            // group's 4x4 chroma is coded and predicted INTRA there and the
            // inter chroma tail below is skipped entirely.
            decode_intra_sub8_leaf(
                dec,
                cdfs,
                neighbours,
                grid,
                group_mi,
                lmi,
                (bw, bh),
                piece_is_chroma_ref(i),
                skip,
                scan4,
                y,
                u,
                v,
                base_q_idx,
                fctx.enable_filter_intra_inter.with(std::cell::Cell::get),
                fctx.tx_select_inter.with(std::cell::Cell::get),
                reduced_tx_set,
                fctx,
            )?;
            if piece_is_chroma_ref(i) {
                intra_chroma = true;
            }
            continue;
        }
        if crate::envflags::env_flag!("EC_TRACE_MODE") {
            eprintln!(
                "EC_MODE mi_row={rmi} mi_col={cmi} rng={}",
                dec.debug_state().0
            );
        }
        let ref_frame = read_single_ref(
            dec,
            cdfs,
            neighbours.above_ref[cmi],
            neighbours.above_ref1[cmi],
            neighbours.left_ref[rmi],
            neighbours.left_ref1[rmi],
        );
        // lane-sub8group: this piece's reference SIZE at parse, its pictures
        // inside the prediction closure below.
        let (ref_width, ref_height) = ref_dims(ref_frame, refpix)?;
        // lane-t900 r28: refusal lifted -- see the split arm above.
        let luma_scale = scale_of((ref_width, ref_height), frame_size);
        if !luma_scale.is_unscaled() {
            hit!(SCALED_SUB8_HITS);
        }
        let gm_table = build_gm_mv_table(
            global_motion,
            rmi,
            cmi,
            w_mi,
            h_mi,
            allow_high_precision_mv,
            force_integer_mv,
        );
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
        let stack = crate::mvstack::find_mv_stack_with_sign_bias(
            grid,
            rmi,
            cmi,
            w_mi,
            h_mi,
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
        let (mv, is_new_mv) = if !not_new {
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
                gm_table[(ref_frame - LAST_FRAME) as usize]
            } else {
                let nearest = dec.symbol(&mut cdfs.ref_mv[stack.ref_mv_ctx]) == 0;
                crate::census::mode(if nearest { 2 } else { 3 });
                if nearest {
                    stack.nearest_mv
                } else {
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
        if crate::envflags::env_flag!("EC_TRACE_MODE") {
            eprintln!(
                "EC_MODE_MV mi_row={rmi} mi_col={cmi} mv0=({},{}) rng={}",
                mv.0,
                mv.1,
                dec.debug_state().0
            );
        }
        let gm_ref = &global_motion[(ref_frame - LAST_FRAME) as usize];
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
        // lane-t900 r9: this reader's sub-blocks have a 4-px side, and libaom
        // `is_nontrans_global_motion` returns 0 for
        // `AOMMIN(mi_size_wide, mi_size_high) < 2` BEFORE testing any model --
        // so a GLOBALMV sub-block here always reads its interp filter.
        if is_globalmv && gm_ref.model != ec_av1_syntax::WarpModel::Translation {
            hit!(GM_NONTRANS_SMALL_SIDE_HITS);
        }
        let gm_nontrans = false;
        let (h_filter, v_filter, resolved_filter) = resolve_interp_filter(
            dec,
            cdfs,
            interp_fixed,
            enable_dual_filter,
            gm_nontrans,
            above_filter_ctx,
            left_filter_ctx,
            false,
        );
        // `read_block_tx_size` (libaom `decodeframe.c`): unlike a `BLOCK_4X4`,
        // this shape signals its transform size. A non-skipped inter block
        // walks the var-tx tree from `max_txsize_rect_lookup[BLOCK_8X4]`; a
        // skipped one codes nothing and records its BLOCK size in both txfm
        // contexts (`set_txfm_ctxs`' `skip_inter` arm).
        let mut leaves: Vec<(usize, usize, usize, usize)> = Vec::new();
        // lane-av1mixllfork: the same libaom ordering fix as the intrabc rect
        // leaf's (`parse_decode_block`, decodeframe.c:1237): the per-segment
        // `lossless` flag is a CONJUNCT of the var-tx condition, not its
        // `else` arm, so a lossless SEGMENT of a mixed-lossless inter frame
        // reads no `tx_size_cat1` symbol here and codes TX_4X4 per plane.
        if fctx.tx_select_inter.with(std::cell::Cell::get) && !skip && !lossless(fctx) {
            let max_w_mi = w_mi.min((mi_cols as usize).saturating_sub(cmi));
            let max_h_mi = h_mi.min((mi_rows as usize).saturating_sub(rmi));
            read_var_tx_size(
                dec,
                cdfs,
                neighbours,
                lmi,
                SIDE,
                (max_w_mi, max_h_mi),
                (bw, bh),
                0,
                0,
                0,
                &mut leaves,
            );
        } else if lossless(fctx) && !skip {
            // COUNTED ONLY WHEN THE SUPPRESSION IS THE CONJUNCT'S WORKING: the
            // else-if also fires for a leaf whose `tx_select_inter` is off,
            // where the tree would not have run either way. The refutation pass
            // measured 33 hits on a build with the conjunct REMOVED, which is
            // exactly that population -- so an unconditional hit proves only
            // "a lossless sub-8 leaf was walked", not "the conjunct suppressed
            // a tree". Guard it on the same term the conjunct uses.
            if fctx.tx_select_inter.with(std::cell::Cell::get) {
                hit!(SUB8_LOSSLESS_NO_VARTX);
            }
            // lane-lossless2: `read_block_tx_size` returns TX_4X4 before it
            // ever looks at `max_txsize_rect_lookup`, so an 8x4/4x8 leaf is
            // TWO 4x4 units, not one rect one.
            for row in 0..bh / MI {
                for col in 0..bw / MI {
                    leaves.push((row, col, MI, MI));
                }
            }
            set_txfm_ctxs(neighbours, lmi, MI, w_mi, h_mi, skip);
        } else {
            leaves.push((0, 0, bw, bh));
            set_txfm_ctxs(neighbours, lmi, bw.max(bh), w_mi, h_mi, skip);
        }
        if crate::envflags::env_flag!("EC_TRACE_MODE") {
            eprintln!(
                "EC_MODE_VALR mi_row={rmi} mi_col={cmi} newmv={is_new_mv} globalmv={is_globalmv} \
                 ref0={ref_frame} mv0=({},{}) stack={} leaves={} rng={}",
                mv.0,
                mv.1,
                stack.entries.len(),
                leaves.len(),
                dec.debug_state().0
            );
        }
        grid_stamp_rect(grid, rmi, cmi, w_mi, h_mi, ref_frame, mv, is_new_mv);
        piece[i] = Some((ref_frame, mv, h_filter, v_filter));
        // Prediction over the leaf's true footprint, laid out at the group's
        // square stride (what `reconstruct_mc_rect` and `read_inter_plane_rect`
        // index with).
        // lane-sub8group: this piece's prediction as a closure, run at the push
        // site, exactly like the 8x8 leaf's.
        let build = move |_y: &mut PlaneBuf<'static>,
                          _u: &mut PlaneBuf<'static>,
                          _v: &mut PlaneBuf<'static>,
                          refpix: &RefPix<'_>,
                          fctx: &crate::decode::FrameCtx,
                          out: &mut [Vec<u16>; 3]| {
            let Some((sref_y, _, _)) = ref_planes_deferred(ref_frame, refpix) else {
                return;
            };
            let mut pred_y = refill(std::mem::take(&mut out[0]), SIDE * SIDE);
            let mut dense = vec![0u16; bw * bh];
            mc::predict_maybe_scaled(
                &sref_y.data,
                sref_y.width,
                sref_y.true_width,
                sref_y.true_height,
                mv_to_q4(px, mv.1, 0),
                mv_to_q4(py, mv.0, 0),
                luma_scale,
                bw,
                bh,
                h_filter,
                v_filter,
                &mut dense,
                fctx,
            );
            for row in 0..bh {
                pred_y[row * SIDE..][..bw].copy_from_slice(&dense[row * bw..][..bw]);
            }
            out[0] = pred_y;
        };
        let pred_y: Vec<u16>;
        let sy = if pred_deferred(refpix) {
            push_pred(build);
            Pred::Cur { plane: 0, stride: SIDE, off: 0 }
        } else {
            let mut built: [Vec<u16>; 3] = Default::default();
            build(y, u, v, refpix, fctx, &mut built);
            let [by, _, _] = built;
            pred_y = by;
            Pred::Inline(&pred_y, SIDE)
        };
        if skip {
            push_mc_rect(
                y,
                u,
                v,
                0,
                px,
                py,
                SIDE,
                bw,
                bh,
                sy,
                &ZERO_RESIDUAL[..SIDE * SIDE],
                fctx,
            );
            neighbours.record_mi_luma_rect(lmi, bw, bh, &ZERO_RESIDUAL[..bw * bh]);
            neighbours.fill_lf_grid_rect(
                lmi,
                w_mi,
                h_mi,
                bw as u8,
                bh as u8,
                ref_frame.max(LAST_FRAME),
                fctx,
            );
        } else {
            let (mut last_tw, mut last_th) = (bw, bh);
            for (idx, &(row, col, tw, th)) in leaves.iter().enumerate() {
                let tu_mi = (rmi + row, cmi + col);
                let (tu_px, tu_py) = (px + col * MI, py + row * MI);
                // This unit's offset into the block prediction (lane-sub8group).
                let start = row * MI * SIDE + col * MI;
                let (tu_grid, tu_tx_type) = if tw == th {
                    // A split `TX_4X4` unit: square reader, its own
                    // `get_txb_ctx_general` neighbour context (the unit is
                    // smaller than its plane block, so the lone-TU 0 does not
                    // apply).
                    // lane-sub8group: the unit's window of the block
                    // prediction, at the block's stride.
                    let tu_skip_ctx = neighbours.luma_skip_ctx_rect(tu_mi, tw / MI, th / MI);
                    read_inter_plane(
                        dec,
                        cdfs,
                        if reduced_tx_set {
                            TxbSet::Luma4Inter
                        } else {
                            TxbSet::Luma4InterSet1
                        },
                        scan4,
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
                        sy.offset(start),
                        None,
                        Some(tu_skip_ctx),
                        fctx,
                    )?
                } else {
                    // The whole leaf as one `TX_8X4`/`TX_4X8` unit: it IS the
                    // plane block, so `get_txb_ctx_general` fixes `txb_skip_ctx`
                    // at 0 (`luma_skip_ctx: None`).
                    read_inter_plane_rect(
                        dec,
                        cdfs,
                        if reduced_tx_set {
                            TxbSet::LumaRect8x4Inter
                        } else {
                            TxbSet::LumaRect8x4InterSet1
                        },
                        (tw, th),
                        SIDE,
                        0,
                        neighbours.around_mi_rect(tu_mi, tw, th)[0],
                        0,
                        y,
                        u,
                        v,
                        tu_px,
                        tu_py,
                        sy.offset(start),
                        None,
                        None,
                        fctx,
                    )?
                };
                if idx == 0 {
                    piece_tx_type = tu_tx_type;
                }
                if idx == 0 && i == 0 {
                    sub0_tx_type = tu_tx_type;
                }
                if idx == 0 && i == 1 {
                    // `av1_get_tx_type` indexes `xd->tx_type_map`, which
                    // `set_mi_offsets` (av1_common_int.h:1679) re-points to the
                    // CURRENT sub-block: the group's one chroma unit inherits
                    // the CHROMA-REFERENCE (second) sub-block's first transform
                    // unit, never the first sub-block's (lane-t900 r7).
                    first_tx_type = tu_tx_type;
                }
                neighbours.record_mi_luma_rect(tu_mi, tw, th, &tu_grid);
                (last_tw, last_th) = (tw, th);
            }
            neighbours.fill_lf_grid_rect(
                lmi,
                w_mi,
                h_mi,
                last_tw as u8,
                last_th as u8,
                ref_frame.max(LAST_FRAME),
                fctx,
            );
        }
        // lane-av1-444: at ss 0/0 this piece codes its OWN 8x4/4x8 U and V
        // units right after its luma, plane order 0,1,2 (libaom decodes each
        // piece as its own block: `decode_token_recon_block`'s plane-major
        // loop over `av1_get_max_uv_txsize(BLOCK_8X4, 0, 0) == TX_8X4`). The
        // unit's prediction is the piece's own mv/filters at the piece's own
        // pixel position (ss 0: the chroma planes share luma's grid), and its
        // tx_type is this piece's first luma transform's coded type.
        if chroma_444 {
            let build_c = move |_y: &mut PlaneBuf<'static>,
                                _u: &mut PlaneBuf<'static>,
                                _v: &mut PlaneBuf<'static>,
                                refpix: &RefPix<'_>,
                                fctx: &crate::decode::FrameCtx,
                                out: &mut [Vec<u16>; 3]| {
                let Some((_, sref_u, sref_v)) = ref_planes_deferred(ref_frame, refpix) else {
                    return;
                };
                let piece_scale = scale_of((ref_width, ref_height), frame_size);
                let mut pred_u = refill(std::mem::take(&mut out[1]), SIDE * SIDE);
                let mut pred_v = refill(std::mem::take(&mut out[2]), SIDE * SIDE);
                for (plane, dst) in [(sref_u, &mut pred_u), (sref_v, &mut pred_v)] {
                    let mut dense = vec![0u16; bw * bh];
                    mc::predict_maybe_scaled(
                        &plane.data,
                        plane.width,
                        plane.true_width,
                        plane.true_height,
                        mv_to_q4(px, mv.1, ss_x(fctx)),
                        mv_to_q4(py, mv.0, ss_x(fctx)),
                        piece_scale,
                        bw,
                        bh,
                        h_filter,
                        v_filter,
                        &mut dense,
                        fctx,
                    );
                    for row in 0..bh {
                        dst[row * SIDE..][..bw].copy_from_slice(&dense[row * bw..][..bw]);
                    }
                }
                out[1] = pred_u;
                out[2] = pred_v;
            };
            let (pred_cu, pred_cv): (Vec<u16>, Vec<u16>);
            let (su, sv) = if pred_deferred(refpix) {
                push_pred(build_c);
                (Pred::Cur { plane: 1, stride: SIDE, off: 0 }, Pred::Cur { plane: 2, stride: SIDE, off: 0 })
            } else {
                let mut built: [Vec<u16>; 3] = Default::default();
                build_c(y, u, v, refpix, fctx, &mut built);
                let [_, bu, bv] = built;
                (pred_cu, pred_cv) = (bu, bv);
                (Pred::Inline(&pred_cu, SIDE), Pred::Inline(&pred_cv, SIDE))
            };
            if skip {
                push_mc_rect(
                    y,
                    u,
                    v,
                    1,
                    px,
                    py,
                    SIDE,
                    bw,
                    bh,
                    su,
                    &ZERO_RESIDUAL[..SIDE * SIDE],
                    fctx,
                );
                push_mc_rect(
                    y,
                    u,
                    v,
                    2,
                    px,
                    py,
                    SIDE,
                    bw,
                    bh,
                    sv,
                    &ZERO_RESIDUAL[..SIDE * SIDE],
                    fctx,
                );
                neighbours.record_mi_chroma(lmi, bw, bh, 1, &ZERO_RESIDUAL[..bw * bh]);
                neighbours.record_mi_chroma(lmi, bw, bh, 2, &ZERO_RESIDUAL[..bw * bh]);
            } else if lossless(fctx) && !mono(fctx) {
                // lane-av1-llsub8b: lossless codes TX_4X4 on EVERY plane
                // (`av1_get_tx_size`, blockd.h:1383), so this 4:4:4 piece's
                // (8,4)/(4,8) chroma plane block is a 2x1/1x2 raster of
                // TX_4X4 units, not the ONE TX_8X4 rect unit the lossy arm
                // below reads -- that single rect is what tripped
                // `TxParams::run`'s WHT assert ((8,4) != (4,4),
                // decode.rs:2566). The walk stamps each unit's own
                // coefficient context, so -- unlike the lossy arm -- no
                // whole-block `record_mi_chroma` runs after it: the walk's
                // per-unit stamps ARE the block's chroma contexts (the same
                // rule `leaf8_inter_chroma_lossless`'s caller obeys).
                read_inter_chroma_lossless(
                    y,
                    u,
                    v,
                    dec,
                    cdfs,
                    neighbours,
                    su,
                    sv,
                    (rmi, cmi),
                    (px, py),
                    (0, 0),
                    (bw, bh),
                    (bw, bh),
                    SIDE,
                    0,
                    base_q_idx,
                    &mut Vec::new(),
                    &mut Vec::new(),
                    fctx,
                )?;
            } else {
                // lane-av1422gather3: 4:4:4-only -- this `ca` arm sits inside
                // the `chroma_444` guard (ss 0/0); the 4:2:2 per-piece arm
                // below already reads through `around_mi_422_chroma`. Probed
                // at ss (1,0) over all 21 committed 4:2:2 cells: 0 hits.
                let ca = neighbours.around_mi_rect(lmi, bw, bh);
                let (ug, _) = read_inter_plane_rect(
                    dec,
                    cdfs,
                    TxbSet::ChromaRect8x4,
                    (bw, bh),
                    SIDE,
                    1,
                    ca[1],
                    0,
                    y,
                    u,
                    v,
                    px,
                    py,
                    su,
                    Some(piece_tx_type),
                    None,
                    fctx,
                )?;
                let (vg, _) = read_inter_plane_rect(
                    dec,
                    cdfs,
                    TxbSet::ChromaRect8x4,
                    (bw, bh),
                    SIDE,
                    2,
                    ca[2],
                    0,
                    y,
                    u,
                    v,
                    px,
                    py,
                    sv,
                    Some(piece_tx_type),
                    None,
                    fctx,
                )?;
                neighbours.record_mi_chroma(lmi, bw, bh, 1, &ug);
                neighbours.record_mi_chroma(lmi, bw, bh, 2, &vg);
            }
        // lane-av1-422blockhalf: the 4:2:2 per-piece arm. The chroma
        // reference piece codes its own U and V TX_4X4 units right after its
        // luma -- both pieces of a horz group, the odd-column piece of a
        // vert one (see `piece_is_chroma_ref` above; measured on the pinned
        // t422 fixture frame 1, horz group (8,10)/(9,10) and vert group
        // (3,10)/(3,11)). The unit anchors at the FLOORED chroma cell
        // (`av1_setup_dst_planes`' `(mi_col & !1) * MI >> ss_x`, the
        // lane-av1-422kf2 rule) and reads through
        // [`Neighbours::around_mi_422_chroma`]'s per-chroma-cell context.
        // A horz piece IS 4x4 chroma (8 luma wide, ss_y 0): its unit
        // predicts whole from its own mv/filters. A vert piece is 2 chroma
        // px wide, so its 4-px unit predicts in halves --
        // `build_inter_predictors_sub8x8`'s rule: the left 2x4 from the
        // piece to the LEFT (the group's own left piece, or the decoded
        // neighbour when the group starts on an odd column), the right 2x4
        // from this piece. A skipped reference piece still predicts (spec
        // 7.11.3) but codes no coefficients.
        } else if chroma_422 && piece_is_chroma_ref(i) {
            const B4: usize = 4;
            let (pc_ref, pc_mv, pc_hf, pc_vf) = piece[i].expect("inter piece recorded above");
            let unit_x = ((lmi.1 & !1) * MI) >> ss_x(fctx);
            let unit_y = py;
            // The left 2x4 half's source: the in-group left piece when this
            // piece is the group's second, else the decoded neighbour one mi
            // to the left (an odd-column group start). Its interp filters
            // are not on the mi grid, so an outside neighbour borrows this
            // piece's own.
            let left_piece = if i > 0 {
                piece[i - 1]
            } else if lmi.1 > 0 {
                grid.get(rmi, lmi.1 - 1).filter(|mi| mi.is_inter).map(|mi| {
                    (
                        mi.ref_frame,
                        (i32::from(mi.mv.0), i32::from(mi.mv.1)),
                        pc_hf,
                        pc_vf,
                    )
                })
            } else {
                None
            };
            let build_c = move |_y: &mut PlaneBuf<'static>,
                                _u: &mut PlaneBuf<'static>,
                                _v: &mut PlaneBuf<'static>,
                                refpix: &RefPix<'_>,
                                fctx: &crate::decode::FrameCtx,
                                out: &mut [Vec<u16>; 3]| {
                let Some((pc_ref_y, sref_u, sref_v)) = ref_planes_deferred(pc_ref, refpix) else {
                    return;
                };
                let pc_scale = scale_of((pc_ref_y.true_width, pc_ref_y.true_height), frame_size);
                let mut pred_u = refill(std::mem::take(&mut out[1]), B4 * B4);
                let mut pred_v = refill(std::mem::take(&mut out[2]), B4 * B4);
                for (plane, dst) in [(sref_u, &mut pred_u), (sref_v, &mut pred_v)] {
                    let mut buf = vec![0u16; B4 * B4];
                    if vert {
                        // Right 2x4 half: this piece's own ref/mv/filters.
                        let mut half = vec![0u16; 2 * 4];
                        mc::predict_maybe_scaled(
                            &plane.data,
                            plane.width,
                            plane.true_width,
                            plane.true_height,
                            mv_to_q4(unit_x + 2, pc_mv.1, ss_x(fctx)),
                            mv_to_q4(unit_y, pc_mv.0, ss_y(fctx)),
                            pc_scale,
                            2,
                            4,
                            pc_hf,
                            pc_vf,
                            &mut half,
                            fctx,
                        );
                        for row in 0..4 {
                            buf[row * B4 + 2..row * B4 + 4]
                                .copy_from_slice(&half[row * 2..row * 2 + 2]);
                        }
                        // Left 2x4 half: the piece to the left (an
                        // intra/off-frame neighbour reduces to whole-from-own,
                        // `is_sub8x8_inter`'s rule).
                        let (l_ref, l_mv, l_hf, l_vf) = match left_piece {
                            Some((l_ref, l_mv, l_hf, l_vf)) => (l_ref, l_mv, l_hf, l_vf),
                            None => (pc_ref, pc_mv, pc_hf, pc_vf),
                        };
                        let Some((l_y, l_u, _)) = ref_planes_deferred(l_ref, refpix) else {
                            continue;
                        };
                        mc::predict_maybe_scaled(
                            &l_u.data,
                            l_u.width,
                            l_u.true_width,
                            l_u.true_height,
                            mv_to_q4(unit_x, l_mv.1, ss_x(fctx)),
                            mv_to_q4(unit_y, l_mv.0, ss_y(fctx)),
                            scale_of((l_y.true_width, l_y.true_height), frame_size),
                            2,
                            4,
                            l_hf,
                            l_vf,
                            &mut half,
                            fctx,
                        );
                        for row in 0..4 {
                            buf[row * B4..row * B4 + 2]
                                .copy_from_slice(&half[row * 2..row * 2 + 2]);
                        }
                    } else {
                        mc::predict_maybe_scaled(
                            &plane.data,
                            plane.width,
                            plane.true_width,
                            plane.true_height,
                            mv_to_q4(unit_x, pc_mv.1, ss_x(fctx)),
                            mv_to_q4(unit_y, pc_mv.0, ss_y(fctx)),
                            pc_scale,
                            B4,
                            B4,
                            pc_hf,
                            pc_vf,
                            &mut buf,
                            fctx,
                        );
                    }
                    dst.copy_from_slice(&buf);
                }
                out[1] = pred_u;
                out[2] = pred_v;
            };
            let (pred_cu, pred_cv): (Vec<u16>, Vec<u16>);
            let (su, sv) = if pred_deferred(refpix) {
                push_pred(build_c);
                (Pred::Cur { plane: 1, stride: B4, off: 0 }, Pred::Cur { plane: 2, stride: B4, off: 0 })
            } else {
                let mut built: [Vec<u16>; 3] = Default::default();
                build_c(y, u, v, refpix, fctx, &mut built);
                let [_, bu, bv] = built;
                (pred_cu, pred_cv) = (bu, bv);
                (Pred::Inline(&pred_cu, B4), Pred::Inline(&pred_cv, B4))
            };
            let ctx_mi = (lmi.0, lmi.1 & !1);
            let ca = neighbours.around_mi_422_chroma(ctx_mi, 8, 4);
            if skip {
                push_mc(
                    y,
                    u,
                    v,
                    1,
                    unit_x,
                    unit_y,
                    B4,
                    su,
                    &ZERO_RESIDUAL[..B4 * B4],
                    fctx,
                );
                push_mc(
                    y,
                    u,
                    v,
                    2,
                    unit_x,
                    unit_y,
                    B4,
                    sv,
                    &ZERO_RESIDUAL[..B4 * B4],
                    fctx,
                );
                neighbours.record_mi_chroma(ctx_mi, 8, 4, 1, &ZERO_RESIDUAL[..B4 * B4]);
                neighbours.record_mi_chroma(ctx_mi, 8, 4, 2, &ZERO_RESIDUAL[..B4 * B4]);
            } else {
                hit!(CHROMA422_INTER_SUB8_HITS);
                let (ug, _) = read_inter_plane(
                    dec,
                    cdfs,
                    TxbSet::Chroma4,
                    scan4,
                    1,
                    ca[1],
                    0,
                    y,
                    u,
                    v,
                    unit_x,
                    unit_y,
                    B4,
                    base_q_idx,
                    su,
                    Some(piece_tx_type),
                    None,
                    fctx,
                )?;
                let (vg, _) = read_inter_plane(
                    dec,
                    cdfs,
                    TxbSet::Chroma4,
                    scan4,
                    2,
                    ca[2],
                    0,
                    y,
                    u,
                    v,
                    unit_x,
                    unit_y,
                    B4,
                    base_q_idx,
                    sv,
                    Some(piece_tx_type),
                    None,
                    fctx,
                )?;
                neighbours.record_mi_chroma(ctx_mi, 8, 4, 1, &ug);
                neighbours.record_mi_chroma(ctx_mi, 8, 4, 2, &vg);
            }
        }
        neighbours.record_mode_mi(rmi, cmi, w_mi, h_mi, DC_PRED);
        neighbours.record_inter_rect_mi(
            lmi,
            w_mi,
            h_mi,
            skip,
            true,
            ref_frame,
            resolved_filter,
            false,
        );
        neighbours.fill_skip_grid_rect(lmi, w_mi, h_mi, skip);
        last_skip = skip;
    }
    // lane-av1-444: at ss 0/0 the group unit does not exist -- each piece
    // coded its own U/V inside the loop above, and each piece wrote its own
    // uv neighbour state (inter pieces through `record_inter_rect_mi`'s
    // is_inter DC stamp, intra pieces through their own per-piece record).
    // The group-level DC stamp below stays 4:2:0-only: at ss 0/0 it painted
    // over an intra piece's real `uv_mode` and the next block's
    // `get_intra_edge_filter_type` lost the smooth neighbour (same class as
    // [`decode_inter_sub8_split4`]'s tail, frame 19 of the fixture).
    // 4:2:0 needs the same second half as the split twin: the stamp must run
    // only when the group unit was painted by the INTER arm below. When the
    // chroma-reference sub-block was INTRA, its own
    // [`decode_intra_sub8_leaf`] tail recorded the group cell with the real
    // `uv_predict_mode`, and this DC stamp ran after it and flattened the
    // mi-exact cell (class `override-slot-on-one-arm`, this lane's own
    // 10-bit film gate regression at 1ad7e0fc; port of lane-av1-cfl's
    // 7c6dad53). lane-av1-422blockhalf: 4:2:2 never takes it either -- the
    // group unit does not exist there; every chroma-reference piece coded
    // (and recorded) its own unit inside the loop above.
    if !chroma_444 && !chroma_422 && !intra_chroma {
        neighbours.record_uv_mode_mi(gr, gc, 2, 2, DC_PRED);
    }
    // lane-t900 r23 (class [[new-map-ignores-tile-edge]]: a neighbour map
    // needs EVERY writer): a sub-8x8 group published no palette state, so
    // once an 8x8 leaf of the same row can BE a palette block, the group
    // left the row's band holding that earlier block's size and the next
    // intra block read `av1_get_palette_mode_ctx` one too high (measured:
    // mi(30,30) of decode-order frame 1, ours ctx 1 where libaom gathers
    // 0). No sub-8x8 piece can itself be a palette (`av1_allow_palette`
    // needs `bsize >= BLOCK_8X8`), so the group always clears.
    record_strip_palette(neighbours, (gr, gc), 8, 8, 0, [0u16; 8], 0, [0u16; 8]);
    // lane-sub8intra: when the chroma-reference sub-block was INTRA it coded
    // and reconstructed the group's one chroma unit itself.
    // lane-av1-422blockhalf: 4:2:2 never runs this tail -- the shared group
    // unit does not exist at ss (1,0); each chroma-reference piece coded its
    // own U/V TX_4X4 inside the loop above, exactly like the 4:4:4 arm.
    if !chroma_444 && !chroma_422 && !intra_chroma {
        // The group's single 4x4 chroma unit per plane, on the chroma-reference
        // (second) sub-block's own skip flag.
        const B4: usize = 4;
        // `is_sub8x8_inter` (reconinter_template.inc:54) is false as soon as ANY
        // mi of the group is intra, so a group whose chroma reference is inter but
        // whose sibling is INTRA does NOT get the two-piece sub8x8 prediction: the
        // ordinary 8x8-and-bigger path predicts the WHOLE 4x4 chroma unit from the
        // chroma-reference block's own mv, at the group's chroma origin
        // (`row_start`/`col_start` shift `pre_x`/`pre_y` there either way).
        let mixed = piece[0].is_none();
        if mixed {
            hit_do! {
                SUB8_INTRA_RECT_HITS.with(|h| {
                    let mut s = h.get();
                    s[3] += 1;
                    h.set(s);
                });
            }
        }
        let (piece_w, piece_h) = match (mixed, vert) {
            (true, _) => (B4, B4),
            (false, true) => (2usize, 4usize),
            (false, false) => (4usize, 2usize),
        };
        // lane-sub8group: the group's chroma pieces, built where they are
        // written -- the pieces' refs/mvs/filters travel by value.
        let build = move |_y: &mut PlaneBuf<'static>,
                          _u: &mut PlaneBuf<'static>,
                          _v: &mut PlaneBuf<'static>,
                          refpix: &RefPix<'_>,
                          fctx: &crate::decode::FrameCtx,
                          out: &mut [Vec<u16>; 3]| {
            let mut pred_u = refill(std::mem::take(&mut out[1]), B4 * B4);
            let mut pred_v = refill(std::mem::take(&mut out[2]), B4 * B4);
            // Index loop mirrors the spec's per-position form over several offsets; an iterator would obscure it.
            #[allow(clippy::needless_range_loop)]
            for i in 0..2usize {
                let Some((ref_frame, mv, h_filter, v_filter)) = piece[i] else {
                    continue;
                };
                let Some((piece_ref_y, sref_u, sref_v)) = ref_planes_deferred(ref_frame, refpix)
                else {
                    continue;
                };
                let piece_scale = scale_of(
                    (piece_ref_y.true_width, piece_ref_y.true_height),
                    frame_size,
                );
                let (ox, oy) = if mixed {
                    (cpx, cpy)
                } else if vert {
                    (cpx + i * piece_w, cpy)
                } else {
                    (cpx, cpy + i * piece_h)
                };
                let (dc, dr) = if mixed {
                    (0, 0)
                } else if vert {
                    (i * piece_w, 0)
                } else {
                    (0, i * piece_h)
                };
                for (plane, dst) in [(sref_u, &mut pred_u), (sref_v, &mut pred_v)] {
                    let mut buf = vec![0u16; piece_w * piece_h];
                    mc::predict_maybe_scaled(
                        &plane.data,
                        plane.width,
                        plane.true_width,
                        plane.true_height,
                        mv_to_q4(ox, mv.1, ss_x(fctx)),
                        mv_to_q4(oy, mv.0, ss_x(fctx)),
                        piece_scale,
                        piece_w,
                        piece_h,
                        h_filter,
                        v_filter,
                        &mut buf,
                        fctx,
                    );
                    for row in 0..piece_h {
                        for col in 0..piece_w {
                            dst[(dr + row) * B4 + dc + col] = buf[row * piece_w + col];
                        }
                    }
                }
            }
            out[1] = pred_u;
            out[2] = pred_v;
        };
        let (pred_u, pred_v): (Vec<u16>, Vec<u16>);
        let (su, sv) = if pred_deferred(refpix) {
            push_pred(build);
            (Pred::Cur { plane: 1, stride: B4, off: 0 }, Pred::Cur { plane: 2, stride: B4, off: 0 })
        } else {
            let mut built: [Vec<u16>; 3] = Default::default();
            build(y, u, v, refpix, fctx, &mut built);
            let [_, bu, bv] = built;
            (pred_u, pred_v) = (bu, bv);
            (Pred::Inline(&pred_u, B4), Pred::Inline(&pred_v, B4))
        };
        let (u_grid, v_grid) = if last_skip {
            push_mc(
                y,
                u,
                v,
                1,
                cpx,
                cpy,
                B4,
                su,
                &ZERO_RESIDUAL[..B4 * B4],
                fctx,
            );
            push_mc(
                y,
                u,
                v,
                2,
                cpx,
                cpy,
                B4,
                sv,
                &ZERO_RESIDUAL[..B4 * B4],
                fctx,
            );
            (Grid::Zero(B4 * B4), Grid::Zero(B4 * B4))
        } else {
            let around = neighbours.around_mi(group_mi, 8);
            let ug = read_inter_plane(
                dec,
                cdfs,
                TxbSet::Chroma4,
                scan4,
                1,
                around[1],
                0,
                y,
                u,
                v,
                cpx,
                cpy,
                B4,
                base_q_idx,
                su,
                Some(first_tx_type),
                None,
                fctx,
            )?
            .0;
            let vg = read_inter_plane(
                dec,
                cdfs,
                TxbSet::Chroma4,
                scan4,
                2,
                around[2],
                0,
                y,
                u,
                v,
                cpx,
                cpy,
                B4,
                base_q_idx,
                sv,
                Some(first_tx_type),
                None,
                fctx,
            )?
            .0;
            let coded = ug.iter().chain(vg.iter()).any(|&c| c != 0);
            note_sub8_chroma_tx(sub0_tx_type, first_tx_type, coded);
            (ug, vg)
        };
        let round_up_even = |n: usize| n.div_ceil(2) * 2;
        let bound_h = round_up_even(neighbours.mi_rows);
        let bound_w = round_up_even(neighbours.mi_cols);
        let u_state = neighbour_state(&u_grid);
        let v_state = neighbour_state(&v_grid);
        for cell in 0..2usize {
            if cell < 2usize.min(bound_h.saturating_sub(gr)) {
                neighbours.left[gr + cell][1] = u_state;
                neighbours.left[gr + cell][2] = v_state;
            }
            if cell < 2usize.min(bound_w.saturating_sub(gc)) {
                neighbours.above[gc + cell][1] = u_state;
                neighbours.above[gc + cell][2] = v_state;
            }
        }
    }
    // `update_partition_context` writes `partition_context_lookup[subsize]`
    // over the whole 8x8 span -- BLOCK_4X8's width above and height left, the
    // same values [`decode_leaf_rect8`] writes for the intra shape.
    for cell in 0..2usize {
        neighbours.above_side_mi[gc + cell] = bw as u8;
        neighbours.left_side_mi[gr + cell] = bh as u8;
    }
    Ok(())
}

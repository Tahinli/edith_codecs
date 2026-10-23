//! Sub-8x8 chroma-format leaf decoders and the split4 / rect8 leaves. //! av1split: moved out of `decode/mod.rs`; the items are unchanged apart from visibility. (D15)

use super::*;

/// 4:4:4 sub-8x8 key-frame leaf chroma (lane-av1-444). At subsampling 0/0
/// `is_chroma_reference` (av1_common_int.h:1459) is unconditional, so EVERY
/// 4x4 / 4x8 / 8x4 leaf codes its own full-resolution chroma unit right
/// after its own luma (`read_intra_mode_sub8` already read this leaf's own
/// `uv_mode`), not the one shared 4x4 unit [`decode_leaf_split4`] /
/// [`decode_leaf_rect8`]'s group tails decode at 4:2:0. Mirrors those tails
/// arm for arm at leaf scale: a skipped leaf keeps its CfL contribution, an
/// intrabc leaf predicts a frame copy at its DV with the luma `tx_type`
/// inherited through [`INTRABC_CHROMA_TX`] (the caller arms it right after
/// the luma read) -- and a SKIPPED intrabc leaf is still the DV copy, never
/// an intra prediction (lane-av1-ibcskip2: libaom's
/// `av1_build_inter_predictors_sb` runs for an intrabc block regardless of
/// `skip`). The chroma position, reach and entropy-context span are
/// the leaf's own (ss 0/0); the 4x8/8x4 unit reads the same
/// `ChromaRect8x4` tables [`decode_rect_split`] proves for that shape.
/// Records the leaf's own `uv_mode` map entry and chroma above/left state,
/// and returns the (u, v) grids for the caller's coarse slots.
#[allow(clippy::too_many_arguments)]
pub(super) fn sub8_leaf_chroma444(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    lmi: (usize, usize),
    bw: usize,
    bh: usize,
    uv_mode: usize,
    alpha: Option<(i32, i32)>,
    skip: bool,
    intrabc_dv: Option<(i32, i32)>,
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    scan4: &[u16],
    base_q_idx: u8,
    smooth_neighbor_uv: bool,
    fctx: &crate::decode::FrameCtx,
) -> Result<(Grid, Grid, usize, usize)> {
    let uv_predict_mode = if uv_mode == UV_CFL_PRED {
        DC_PRED
    } else {
        uv_mode
    };
    let (w_mi, h_mi) = (bw / MI, bh / MI);
    neighbours.record_uv_mode_mi(lmi.0, lmi.1, w_mi, h_mi, uv_predict_mode);
    let (px, py) = (lmi.1 * MI, lmi.0 * MI);
    let square = (bw, bh) == (4, 4);
    // `Reach::of_rect` has no 4x4 table (`rect_reach_tables` starts at 4x8);
    // the square unit takes the leaf's own `Reach::of`, as its luma does.
    let reach = if square {
        Reach::of(4, px, py, y.width, y.height, fctx)
    } else {
        Reach::of_rect(bw, bh, px, py, y.width, y.height, fctx)
    };
    let grids = if let Some(dv) = intrabc_dv.filter(|_| skip) {
        // lane-av1-ibcskip2: a SKIPPED intrabc leaf still predicts the frame
        // copy at its DV -- libaom's `av1_build_inter_predictors_sb` runs for
        // an intrabc block regardless of `skip` (only the residual read is
        // dropped), so the reconstruction is the bare DV copy. The intra push
        // in the next arm is the NON-intrabc route; predicting DC there
        // decoded flat cells where this fixture's oracle copies frame bytes
        // (class skipped-intrabc-predicts-intra,
        // lanes/av1ibcskip2.report.md). Same armed-palette-slot route the
        // leaf's own luma skip arm uses (`decode_leaf_split4` /
        // `decode_leaf_rect8`); the luma DV copy reads current samples.
        let mut ub = vec![0u16; bw * bh];
        mc::predict_with_filter(
            &u.data,
            u.width,
            u.true_width,
            u.true_height,
            mv_to_q4(px, dv.1, 0),
            mv_to_q4(py, dv.0, 0),
            bw,
            bh,
            mc::InterpFilterKind::Bilinear,
            &mut ub,
            fctx,
        );
        let mut vb = vec![0u16; bw * bh];
        mc::predict_with_filter(
            &v.data,
            v.width,
            v.true_width,
            v.true_height,
            mv_to_q4(px, dv.1, 0),
            mv_to_q4(py, dv.0, 0),
            bw,
            bh,
            mc::InterpFilterKind::Bilinear,
            &mut vb,
            fctx,
        );
        hit!(SKIPPED_INTRABC_DV_COPY_HITS);
        let zeros = &ZERO_RESIDUAL[..bw * bh];
        if square {
            set_palette_pred(ub, fctx);
            push_intra(
                y,
                u,
                v,
                1,
                px,
                py,
                4,
                DC_PRED,
                0,
                reach,
                zeros,
                None,
                None,
                smooth_neighbor_uv,
                fctx,
            );
            set_palette_pred(vb, fctx);
            push_intra(
                y,
                u,
                v,
                2,
                px,
                py,
                4,
                DC_PRED,
                0,
                reach,
                zeros,
                None,
                None,
                smooth_neighbor_uv,
                fctx,
            );
        } else {
            set_palette_pred(ub, fctx);
            push_intra_rect(
                y,
                u,
                v,
                1,
                px,
                py,
                bw,
                bh,
                DC_PRED,
                0,
                reach,
                zeros,
                None,
                None,
                smooth_neighbor_uv,
                fctx,
            );
            set_palette_pred(vb, fctx);
            push_intra_rect(
                y,
                u,
                v,
                2,
                px,
                py,
                bw,
                bh,
                DC_PRED,
                0,
                reach,
                zeros,
                None,
                None,
                smooth_neighbor_uv,
                fctx,
            );
        }
        // The same arm-without-clear route the 4:2:0 tails count: a skipped
        // leaf reads no coefficient, so the inherited chroma `tx_type` is
        // dropped unconsumed.
        hit!(SKIPPED_INTRABC_CHROMA_ARM_HITS);
        fctx.intrabc_chroma_tx.with(|c| c.set(None));
        (Grid::Zero(bw * bh), Grid::Zero(bw * bh), 4, 4)
    } else if skip {
        let ac = alpha.map(|_| cfl_src_rect(px, py, bw, bh));
        if square {
            push_intra(
                y,
                u,
                v,
                1,
                px,
                py,
                4,
                uv_predict_mode,
                0,
                reach,
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
                px,
                py,
                4,
                uv_predict_mode,
                0,
                reach,
                &ZERO_RESIDUAL[..16],
                alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
                None,
                smooth_neighbor_uv,
                fctx,
            );
        } else {
            push_intra_rect(
                y,
                u,
                v,
                1,
                px,
                py,
                bw,
                bh,
                uv_predict_mode,
                0,
                reach,
                &ZERO_RESIDUAL[..bw * bh],
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
                px,
                py,
                bw,
                bh,
                uv_predict_mode,
                0,
                reach,
                &ZERO_RESIDUAL[..bw * bh],
                alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
                None,
                smooth_neighbor_uv,
                fctx,
            );
        }
        // The same arm-without-clear route the 4:2:0 tails count: a skipped
        // leaf reads no coefficient, so an armed intrabc slot is dropped.
        if fctx.intrabc_chroma_tx.with(std::cell::Cell::get).is_some() {
            hit!(SKIPPED_INTRABC_CHROMA_ARM_HITS);
            fctx.intrabc_chroma_tx.with(|c| c.set(None));
        }
        (Grid::Zero(bw * bh), Grid::Zero(bw * bh), 4, 4)
    } else if let Some(dv) = intrabc_dv {
        // Frame copy at the DV; the residual still reads the ordinary chroma
        // tables with the inherited luma `tx_type` (see the 4:2:0 tails).
        let mut ub = vec![0u16; bw * bh];
        mc::predict_with_filter(
            &u.data,
            u.width,
            u.true_width,
            u.true_height,
            mv_to_q4(px, dv.1, 0),
            mv_to_q4(py, dv.0, 0),
            bw,
            bh,
            mc::InterpFilterKind::Bilinear,
            &mut ub,
            fctx,
        );
        let mut vb = vec![0u16; bw * bh];
        mc::predict_with_filter(
            &v.data,
            v.width,
            v.true_width,
            v.true_height,
            mv_to_q4(px, dv.1, 0),
            mv_to_q4(py, dv.0, 0),
            bw,
            bh,
            mc::InterpFilterKind::Bilinear,
            &mut vb,
            fctx,
        );
        let around = neighbours.around_mi_rect(lmi, bw, bh);
        let pair = if square {
            set_palette_pred(ub, fctx);
            let ug = read_plane(
                dec,
                cdfs,
                TxbSet::Chroma4,
                scan4,
                1,
                around[1],
                DC_PRED,
                DC_PRED,
                0,
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
                None,
                None,
                smooth_neighbor_uv,
                fctx,
            )?;
            set_palette_pred(vb, fctx);
            let vg = read_plane(
                dec,
                cdfs,
                TxbSet::Chroma4,
                scan4,
                2,
                around[2],
                DC_PRED,
                DC_PRED,
                0,
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
                None,
                None,
                smooth_neighbor_uv,
                fctx,
            )?;
            (ug, vg, 4, 4)
        } else {
            // lane-av1-ibc444rect: the unskipped lossless chroma walk this
            // lane fixes (one rect unit -> per-4x4 Chroma4 units).
            if lossless(fctx) {
                hit!(INTRABC_RECT8_LOSSLESS_CHROMA_HITS);
            }
            if lossless(fctx) {
                // lane-av1-ibc444rect: at lossless `read_tx_size` forces
                // TX_4X4 on EVERY plane (`av1_get_tx_size`'s lossless
                // first line), so this rect leaf's chroma is (bw/4)*(bh/4)
                // TX_4X4 units at their own 4x4 origins -- not the single
                // `ChromaRect8x4` rect unit the non-lossless arm below reads
                // (`av1_get_max_uv_txsize(BLOCK_8X4)`'s rect, unreachable at
                // lossless). Same walk the leaf's own non-intrabc lossless
                // arm runs (lane-av1-llsub8's fix), plane-major per libaom
                // `decode_token_recon_block`, with the intra predictor
                // swapped for the CURRENT frame at the DV; CFL is never
                // allowed at lossless and no alpha rides an intrabc leaf.
                let mut out_u = Grid::Own(vec![0i32; bw * bh]);
                let mut out_v = Grid::Own(vec![0i32; bw * bh]);
                for plane_idx in [1usize, 2] {
                    for ur in 0..bh / 4usize {
                        for uc in 0..bw / 4usize {
                            let (umi_r, umi_c) = (lmi.0 + ur, lmi.1 + uc);
                            let (upx, upy) = (px + uc * 4, py + ur * 4);
                            let mut pb = vec![0u16; 16];
                            let buf: &PlaneBuf<'static> = if plane_idx == 1 { &*u } else { &*v };
                            mc::predict_with_filter(
                                &buf.data,
                                buf.width,
                                buf.true_width,
                                buf.true_height,
                                mv_to_q4(upx, dv.1, 0),
                                mv_to_q4(upy, dv.0, 0),
                                4,
                                4,
                                mc::InterpFilterKind::Bilinear,
                                &mut pb,
                                fctx,
                            );
                            set_palette_pred(pb, fctx);
                            let unit_around = neighbours.around_mi_rect((umi_r, umi_c), 4, 4);
                            // lane-av1loss444kf: libaom asks
                            // `has_top_right`/`has_bottom_left` with the
                            // LEAF's bsize, this unit's `(col_off, row_off)`
                            // and `tx_size` -- not as a standalone BLOCK_4X4
                            // at the unit's own position. A 4x8 leaf's FIRST
                            // unit has its bottom-left pixels inside the
                            // block's own second row (already reconstructed),
                            // which `has_bottom_left`'s
                            // `row_off + count < plane_bh_unit` early-out
                            // answers TRUE; the 4x4 table answered false, so
                            // `PlaneBuf::edges` replicated the last left
                            // sample and the unit predicted from a fabricated
                            // edge (the 4:4:4 lossless key frame's 49-sample
                            // chroma damage at (196,200)-(204,200)). The
                            // 4:2:2 twin of this arm was fixed the same way
                            // (lane-av1-422kf2); this is its 4:4:4 twin.
                            let unit_reach = Reach::of_tu(bw, bh, uc * 4, ur * 4, 4, 4, reach);
                            if !square
                                && unit_reach != Reach::of(4, upx, upy, y.width, y.height, fctx)
                            {
                                hit!(SUB8_CHROMA444_LOSSLESS_TU_REACH_HITS);
                            }
                            let grid = read_plane(
                                dec,
                                cdfs,
                                TxbSet::Chroma4,
                                scan4,
                                plane_idx,
                                unit_around[plane_idx],
                                DC_PRED,
                                DC_PRED,
                                0,
                                unit_reach,
                                y,
                                u,
                                v,
                                upx,
                                upy,
                                4,
                                TX4,
                                base_q_idx,
                                None,
                                None,
                                Some(3),
                                smooth_neighbor_uv,
                                fctx,
                            )?;
                            let cell = grid_unit_state(&grid, 4, 0, 0, 4, 4);
                            if umi_r < neighbours.mi_rows {
                                neighbours.left[umi_r][plane_idx] = cell;
                            }
                            if umi_c < neighbours.mi_cols {
                                neighbours.above[umi_c][plane_idx] = cell;
                            }
                            let out = if plane_idx == 1 {
                                &mut out_u
                            } else {
                                &mut out_v
                            };
                            if let Grid::Own(v) = out {
                                for r in 0..4usize {
                                    v[(ur * 4 + r) * bw + uc * 4..][..4]
                                        .copy_from_slice(&grid[r * 4..][..4]);
                                }
                            }
                        }
                    }
                }
                (out_u, out_v, 4, 4)
            } else {
                let scan: &[u16] = if bw > bh { &SCAN_8X4 } else { &SCAN_4X8 };
                let default_tx = fctx
                    .intrabc_chroma_tx
                    .with(std::cell::Cell::get)
                    .unwrap_or(TxType::DctDct);
                let mut out = (Grid::Zero(bw * bh), Grid::Zero(bw * bh), bw, bh);
                for (plane_idx, buf) in [(1usize, ub), (2usize, vb)] {
                    let skip_ctx =
                        usize::from(around[plane_idx].0) + usize::from(around[plane_idx].1);
                    let mut coding = cdfs.txb(TxbSet::ChromaRect8x4, DC_PRED);
                    let (levels, tx_type) = read_coeffs_rect(
                        dec,
                        &mut coding,
                        scan,
                        bw,
                        bh,
                        skip_ctx,
                        dc_sign_ctx(around[plane_idx].2),
                        default_tx,
                        plane_idx,
                    )?;
                    let residual = dequant_and_inverse_typed_wh(
                        &levels,
                        bw,
                        bh,
                        crate::decode::bit_depth(fctx),
                        block_q_idx(fctx),
                        plane_q_delta(plane_idx, fctx).0,
                        plane_q_delta(plane_idx, fctx).1,
                        tx_type,
                        block_iqmatrix(fctx, plane_idx, bw, bh, tx_type),
                    );
                    set_palette_pred(buf, fctx);
                    push_intra_rect(
                        y,
                        u,
                        v,
                        plane_idx,
                        px,
                        py,
                        bw,
                        bh,
                        DC_PRED,
                        0,
                        reach,
                        &residual,
                        None,
                        None,
                        smooth_neighbor_uv,
                        fctx,
                    );
                    if plane_idx == 1 {
                        out.0 = levels;
                    } else {
                        out.1 = levels;
                    }
                }
                out
            }
        };
        fctx.intrabc_chroma_tx.with(|c| c.set(None));
        pair
    } else {
        let ac = alpha.map(|_| cfl_src_rect(px, py, bw, bh));
        let around = neighbours.around_mi_rect(lmi, bw, bh);
        if square {
            let ug = read_plane(
                dec,
                cdfs,
                TxbSet::Chroma4,
                scan4,
                1,
                around[1],
                uv_mode,
                uv_predict_mode,
                0,
                reach,
                y,
                u,
                v,
                px,
                py,
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
                around[2],
                uv_mode,
                uv_predict_mode,
                0,
                reach,
                y,
                u,
                v,
                px,
                py,
                4,
                TX4,
                base_q_idx,
                alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
                None,
                None,
                smooth_neighbor_uv,
                fctx,
            )?;
            (ug, vg, 4, 4)
        } else if lossless(fctx) {
            // lane-av1-llsub8: `read_tx_size` answers TX_4X4 for EVERY plane of
            // a lossless block, so this rect leaf's chroma is (bw/4)*(bh/4)
            // TX_4X4 units at their own 4x4 origins -- not the single
            // TX_8X4/TX_4X8 rect unit the non-lossless arm below reads (that
            // one is `av1_get_max_uv_txsize(BLOCK_8X4)`'s rect transform, which
            // lossless never reaches). Measured on `ll444.obu`'s HORZ leaf at
            // mi (8,16): the oracle codes U units at bc 0,1 with
            // `get_txb_ctx`'s +10 rows (`txb_skip_cdf[0][10..12]`,
            // plane pels 32 > tx pels 16) where the rect-unit read consumed
            // `txb_skip_cdf[1][0]` -- the tile desynced at that U unit's
            // all_zero symbol. Walk is plane-major like libaom
            // `decode_token_recon_block`'s plane/row/col loop; each unit adds
            // 3 to the chroma context (Chroma4's `big` rows, the same fold
            // [`read_plane`]'s 128x128 caller uses); CFL is never allowed at
            // lossless, so no alpha rides along.
            let mut out_u = Grid::Own(vec![0i32; bw * bh]);
            let mut out_v = Grid::Own(vec![0i32; bw * bh]);
            for plane_idx in [1usize, 2] {
                for ur in 0..bh / 4usize {
                    for uc in 0..bw / 4usize {
                        let (umi_r, umi_c) = (lmi.0 + ur, lmi.1 + uc);
                        let (upx, upy) = (px + uc * 4, py + ur * 4);
                        let unit_around = neighbours.around_mi_rect((umi_r, umi_c), 4, 4);
                        // lane-av1loss444kf: the block-relative answer, not
                        // a standalone BLOCK_4X4 lookup at the unit's own
                        // position -- see the intrabc twin above for the
                        // libaom citation and the measured damage.
                        let unit_reach = Reach::of_tu(bw, bh, uc * 4, ur * 4, 4, 4, reach);
                        if !square && unit_reach != Reach::of(4, upx, upy, y.width, y.height, fctx)
                        {
                            hit!(SUB8_CHROMA444_LOSSLESS_TU_REACH_HITS);
                        }
                        let grid = read_plane(
                            dec,
                            cdfs,
                            TxbSet::Chroma4,
                            scan4,
                            plane_idx,
                            unit_around[plane_idx],
                            uv_mode,
                            uv_predict_mode,
                            0,
                            unit_reach,
                            y,
                            u,
                            v,
                            upx,
                            upy,
                            4,
                            TX4,
                            base_q_idx,
                            None,
                            None,
                            Some(3),
                            smooth_neighbor_uv,
                            fctx,
                        )?;
                        let cell = grid_unit_state(&grid, 4, 0, 0, 4, 4);
                        if umi_r < neighbours.mi_rows {
                            neighbours.left[umi_r][plane_idx] = cell;
                        }
                        if umi_c < neighbours.mi_cols {
                            neighbours.above[umi_c][plane_idx] = cell;
                        }
                        let out = if plane_idx == 1 {
                            &mut out_u
                        } else {
                            &mut out_v
                        };
                        if let Grid::Own(v) = out {
                            for r in 0..4usize {
                                v[(ur * 4 + r) * bw + uc * 4..][..4]
                                    .copy_from_slice(&grid[r * 4..][..4]);
                            }
                        }
                    }
                }
            }
            (out_u, out_v, 4, 4)
        } else {
            // `av1_get_max_uv_txsize(BLOCK_4X8/8X4)` at ss 0/0 is the rect
            // transform itself; its `tx_type` is mode-derived with the
            // <32 set (`decode_rect_split`'s own rule for this shape).
            let scan: &[u16] = if bw > bh { &SCAN_8X4 } else { &SCAN_4X8 };
            let default_tx = default_intra_tx_type(uv_predict_mode as u8);
            let mut out = (Grid::Zero(bw * bh), Grid::Zero(bw * bh), bw, bh);
            // Index loop mirrors the spec's per-position form over several offsets; an iterator would obscure it.
            #[allow(clippy::needless_range_loop)]
            for plane_idx in 1..=2usize {
                let skip_ctx = usize::from(around[plane_idx].0) + usize::from(around[plane_idx].1);
                let mut coding = cdfs.txb(TxbSet::ChromaRect8x4, uv_predict_mode);
                let (levels, tx_type) = read_coeffs_rect(
                    dec,
                    &mut coding,
                    scan,
                    bw,
                    bh,
                    skip_ctx,
                    dc_sign_ctx(around[plane_idx].2),
                    default_tx,
                    plane_idx,
                )?;
                let residual = dequant_and_inverse_typed_wh(
                    &levels,
                    bw,
                    bh,
                    crate::decode::bit_depth(fctx),
                    block_q_idx(fctx),
                    plane_q_delta(plane_idx, fctx).0,
                    plane_q_delta(plane_idx, fctx).1,
                    tx_type,
                    block_iqmatrix(fctx, plane_idx, bw, bh, tx_type),
                );
                let cfl = alpha
                    .zip(ac)
                    .map(|((au, av), ac)| (if plane_idx == 1 { au } else { av }, ac));
                push_intra_rect(
                    y,
                    u,
                    v,
                    plane_idx,
                    px,
                    py,
                    bw,
                    bh,
                    uv_predict_mode,
                    0,
                    reach,
                    &residual,
                    cfl,
                    None,
                    smooth_neighbor_uv,
                    fctx,
                );
                if plane_idx == 1 {
                    out.0 = levels;
                } else {
                    out.1 = levels;
                }
            }
            out
        }
    };
    // libaom `av1_set_entropy_contexts` stamps above/left PER TRANSFORM UNIT:
    // the leaf's own chroma span, not the 8x8 group's. A rect leaf's lossless
    // units (4x4) each carry their own state -- a 4:2:0-style whole-leaf smear
    // gave one unit's state to its neighbour's cells (lane-av1-llsub8).
    let (u_grid, v_grid, unit_w, unit_h) = grids;
    // One unit covers `unit_w/MI` mi columns and `unit_h/MI` mi rows (a 4x4
    // unit: one cell each; the non-lossless rect unit: the whole leaf).
    let (uw_cells, uh_cells) = ((unit_w / MI).max(1), (unit_h / MI).max(1));
    for ur in 0..h_mi / uh_cells {
        for uc in 0..w_mi / uw_cells {
            let (x0, y0) = (uc * unit_w, ur * unit_h);
            let us = grid_unit_state(&u_grid, bw, x0, y0, unit_w, unit_h);
            let vs = grid_unit_state(&v_grid, bw, x0, y0, unit_w, unit_h);
            for cell in 0..uh_cells {
                if lmi.0 + ur * uh_cells + cell < neighbours.mi_rows {
                    neighbours.left[lmi.0 + ur * uh_cells + cell][1] = us;
                    neighbours.left[lmi.0 + ur * uh_cells + cell][2] = vs;
                }
            }
            for cell in 0..uw_cells {
                if lmi.1 + uc * uw_cells + cell < neighbours.mi_cols {
                    neighbours.above[lmi.1 + uc * uw_cells + cell][1] = us;
                    neighbours.above[lmi.1 + uc * uw_cells + cell][2] = vs;
                }
            }
        }
    }
    Ok((u_grid, v_grid, unit_w, unit_h))
}

/// [`neighbour_state`] over one `(unit_w x unit_h)` transform unit at offset
/// `(x0, y0)` inside a `bw`-wide assembled chroma grid -- the per-unit stamp
/// the rect arms need when a leaf carries more than one unit.
fn grid_unit_state(
    grid: &Grid,
    bw: usize,
    x0: usize,
    y0: usize,
    unit_w: usize,
    unit_h: usize,
) -> Neighbour {
    let mut cul = 0u32;
    let mut dc = None;
    for r in 0..unit_h {
        for c in 0..unit_w {
            let v = grid[(y0 + r) * bw + x0 + c];
            cul += v.unsigned_abs();
            if r == 0 && c == 0 && v != 0 {
                dc = Some(v < 0);
            }
        }
    }
    if cul == 0 {
        return Neighbour::default();
    }
    Neighbour {
        level: cul.min(7) as u8,
        dc,
    }
}

/// [`sub8_leaf_chroma444`] for 4:2:2 (lane-av1-422g): an even-mi-column 4x4
/// leaf is its plane's own chroma reference (`is_chroma_reference`'s
/// `!(mi_col & ss_x)` at ss_x 1 -- every even-column leaf, not 4:2:0's single
/// bottom-right one) and codes ONE TX_4X4 chroma unit per plane right after
/// its luma: `ss_size_lookup[BLOCK_4X4][1][0]` is BLOCK_4X4, so the (2, 4)
/// plane area rounds up to the square 4x4 read. The unit sits at the chroma
/// position (`px >> ss_x`), and its 4 chroma-px width spans TWO luma mi
/// columns, so the context reads go through
/// [`Neighbours::around_mi_422_chroma`] and the entropy stamp covers both
/// columns with the unit's whole-unit state.
#[allow(clippy::too_many_arguments)]
pub(super) fn sub8_leaf_chroma422(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    lmi: (usize, usize),
    uv_mode: usize,
    angle_delta_uv: i32,
    alpha: Option<(i32, i32)>,
    skip: bool,
    intrabc_dv: Option<(i32, i32)>,
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    scan4: &[u16],
    base_q_idx: u8,
    smooth_neighbor_uv: bool,
    // The owning luma leaf's own shape (`(4, 4)` for a split leaf, `(8, 4)` /
    // `(4, 8)` for a rect pair's strip): libaom's `has_top_right` /
    // `has_bottom_left` answer for this chroma TX_4X4 unit comes from the
    // LEAF's own rect table row at the leaf's mi (`bsize` is the leaf, the
    // unit covers it whole), never the standalone square-4x4 table the old
    // `Reach::of(4, ..)` lookup used -- that said 1 where `has_tr_8x4` says 0
    // at mi(27,44) (the oracle's own EC_PRED prints `n_tr=0` there), so the
    // D45 unit read undecoded pixels to its upper right.
    leaf_shape: (usize, usize),
    fctx: &crate::decode::FrameCtx,
) -> Result<(Grid, Grid)> {
    let uv_predict_mode = if uv_mode == UV_CFL_PRED {
        DC_PRED
    } else {
        uv_mode
    };
    // lane-av1-422kf2: libaom writes one MB_MODE_INFO per mi cell the LEAF
    // covers, and `chroma_left_mbmi`/`chroma_above_mbmi` read whichever of
    // those cells the next block's snap lands on -- an 8x4 strip's SM_V
    // uv_mode must therefore be visible at BOTH of its mi columns, or the
    // block to its right (mi (30,50): left neighbour cell (30,49)) missed the
    // smooth neighbour and skipped libaom's strength-1 intra edge filter
    // (class context-read-from-one-cell, the 4:2:2 sub-8 twin of the
    // lane-kf900 snap fix).
    neighbours.record_uv_mode_mi(
        lmi.0,
        lmi.1,
        (leaf_shape.0 / MI).max(1),
        (leaf_shape.1 / MI).max(1),
        uv_predict_mode,
    );
    let (px, py) = (lmi.1 * MI, lmi.0 * MI);
    // lane-av1-422kf2: the unit's WRITE/PREDICTION anchor is the same floored
    // chroma cell as the entropy anchor below -- libaom positions the sub-8
    // chroma plane block at `av1_setup_dst_planes`' mi-exact floor
    // (`(mi_col & !1) * MI >> ss_x`), NOT at the odd leaf's own chroma x:
    // the oracle's mi(18,55) TX_4X4 unit reconstructed chroma px 108..111
    // (a pure V-copy of the row above at 108..111), while the leaf's own
    // chroma x is 110 -- anchoring there left px 108..109 unwritten (zeros)
    // and stepped px 112..113 with the unit's right columns.
    let (cpx, cpy) = (((lmi.1 & !1) * MI) >> ss_x(fctx), py >> ss_y(fctx));
    // lane-av1-422h: the entropy-context anchor is the unit's FLOOR chroma
    // cell (`pd->above_entropy_context + (mi_col >> ss_x)`): an ODD-column
    // leaf's 4-px unit starts at a chroma px that is NOT cell aligned, and
    // both its ctx read and its ctx write live in the cell containing that
    // start -- one luma mi column to the LEFT of the leaf's own. Even-column
    // (cell-aligned) leaves keep their own column. The state write spans the
    // anchor's two luma mi columns, so the second (odd) leaf of a split group
    // overwrites exactly the cell the first one wrote, as libaom's
    // `av1_set_entropy_contexts` per-TU writes do.
    let ctx_mi = (lmi.0, lmi.1 & !1);
    // lane-av1-422kf2: the square unit takes the leaf's OWN reach, but at the
    // CHROMA plane's granularity, not the luma leaf's. libaom passes the leaf
    // through `scale_chroma_bsize(bsize, ss_x, ss_y)` first
    // (`reconintra.c:1823`, definition `:1649`) and only then calls
    // `has_top_right`/`has_bottom_left` with it, so a rect strip's answer
    // comes from `has_tr_8x4`/`has_tr_4x8` at the strip's mi, not from a
    // 4x4-block lookup. At 4:2:2 (`ss (1,0)`) that remap is NOT the identity:
    // a BLOCK_4X4 luma leaf is answered as BLOCK_8X4, TWO mi wide and one mi
    // tall.
    //
    // MEASURED, `probe/ll422_allintra.obu` (320x240, 4:2:2 lossless
    // all-intra) against the instrumented `aomdec`: the FIRST wrong chroma
    // prediction in decode order is unit 2818, plane U, chroma (96,32) --
    // the leaf at mi(8,49). The oracle's own `EC_PRED` prints `plane=1
    // row_off=0 col_off=0 txw=4 txh=4 mode=7 p_angle=203 have_top=1
    // have_left=1 n_top=4 n_left=4 n_tr=-1 n_bl=4 bsize=2`, and `bsize=2`
    // is BLOCK_8X4 -- the remap, not the BLOCK_4X4 leaf libaom decoded. That
    // `n_bl=4` is the "leftmost column of superblock" arm: `blk_col_in_sb =
    // (mi_col & (sb_mi-1)) >> bw_in_mi_log2 = (49 & 15) >> 1 = 0`
    // (`reconintra.c:421`), where the one-mi-wide luma BLOCK_4X4 reads column
    // 1, and `blk_start_row_off + row_off + 1 < sb_height_unit` is
    // `8 + 0 + 1 < 16 >> 0` (`reconintra.c:429-435`). Squared on the luma
    // leaf, `Reach::of(4, ..)` instead read `has_bl_4x4[byte 32] bit 1`
    // (`84 = 0b0101_0100`, bit 1 clear) and answered FALSE, so
    // `PlaneBuf::edges` fed the predictor a 4-sample left column where
    // libaom fed 8. `build_directional_and_filter_intra_predictors`
    // (`reconintra.c:1150-1158`) copies `n_left_px` into `left_col[0..3]` and
    // only `if (n_bottomleft_px > 0)` the next `n_bottomleft_px`, so with the
    // denial `av1_dr_prediction_z3_c` reads its base term off a replicated
    // tail -- interior-only, with row0/col0 and the coefficients untouched.
    // 4:2:0 and 4:4:4 never reach this arm: the reader is 4:2:2-only (its
    // sibling `sub8_leaf_chroma444` owns those), and the remap is the
    // identity for a leaf of 8 or more at every subsampling.
    let (mi_bw, mi_bh) =
        crate::encode::chroma_mi_shape(leaf_shape.0, leaf_shape.1, ss_x(fctx), ss_y(fctx));
    // libaom's two edge tests are TILE bounds over the TRANSFORM's plane
    // extent and its `have_top`/`have_left` flags are `y0 > 0` / `x0 > 0`
    // (`reconintra.c:1804-1806`, `:1815-1817`); the tile end is the frame end
    // for every leaf this reader owns, and the unit is one TX_4X4 chroma
    // transform wide and tall (`4 << ss` luma px per axis).
    // lane-av1unwritten: at ss (1,0) a BLOCK_4X8 chroma reference's plane
    // block is 4x8 -- `get_plane_block_size(BLOCK_4X8, 1, 0)` is BLOCK_4X8
    // (`common_data.c`'s `av1_ss_size_lookup[BLOCK_4X8][1][0]` is
    // BLOCK_INVALID, but `blockd.h:1372` takes the per-axis MIN of the two
    // lookups) -- and `av1_get_max_uv_txsize` (blockd.h:1372-1379) resolves
    // it through `max_txsize_rect_lookup[BLOCK_4X8]` == TX_4X8
    // (`common_data.h:126`). So away from lossless the WHOLE 4x8 plane block
    // is ONE rect transform unit, exactly the one [`leaf8_chroma422_unit`]
    // already reads for a BLOCK_8X8 leaf. Emitting the single square TX_4X4
    // this arm used to wrote left the block's bottom four chroma rows
    // UNWRITTEN: on the pinned `440_request_is_422.obu` (a 4:2:2 header over
    // a 4:2:0 tile, whose legal 8x8 `PARTITION_HORZ` groups make this shape
    // legal syntax) the sentinel census found 112 unwritten samples per
    // chroma plane, one 4x4 block per group, and the decode's output hash
    // CHANGED RUN TO RUN because the unwritten samples were uninitialised
    // memory (class `writer-gap-reaches-output`).
    //
    // At LOSSLESS the single-unit reading is wrong the other way:
    // `av1_get_tx_size`'s first line returns TX_4X4 on every plane
    // (blockd.h:1383), so the 4x8 plane block is walked as TWO stacked 4x4
    // units -- which is exactly what the two chroma-reference leaves of the
    // `decode_leaf_split4` route below already emit, so the square arms stay
    // for lossless.
    let tx_w = 4 << ss_x(fctx);
    let tx_h = 4 << ss_y(fctx);
    let reach = Reach::of_tu_chroma(
        mi_bw,
        mi_bh,
        lmi.0,
        lmi.1,
        crate::encode::reach_sb_mi(fctx),
        0,
        0,
        tx_w,
        tx_h,
        ss_x(fctx),
        ss_y(fctx),
        // `tx_w`/`tx_h` are already in luma samples (`4 << ss`), so the frame
        // extent is compared with them directly. Shifting them by `ss` again
        // doubled the step at 4:2:2 (`ss_x == 1`): a 4-wide chroma unit at
        // luma x 336 of a 352-wide frame read `336 + 16 < 352` and lost its
        // above-right, while libaom's `right_available`
        // (`mi_col + ((col_off + txw) << ss_x) < mi_col_end`) and the spec's
        // BlockDecoded test both keep it (`336 + 8 < 352`).
        py > 0 && px + tx_w < y.width,
        px > 0 && py + tx_h < y.height,
    );
    let chroma_around = neighbours.around_mi_422_chroma(ctx_mi, 8, 4);
    // lane-av1422sub8pix: the CfL AC signal spans the UNIT's luma footprint,
    // anchored at the same FLOORED luma x the write/prediction anchor below
    // uses -- `px` (the odd leaf's own luma x) steps half the span into the
    // RIGHT neighbour group (t422 frame-1 leaf (1,23): the unit reconstructs
    // chroma px 44..47 from luma 88..95, while a `px`-anchored source
    // averaged luma 92..99 and every CFL unit of the group mismatched).
    let ac = alpha.map(|_| cfl_src_rect((lmi.1 & !1) * MI, py, 8, 4));
    let grids: (Grid, Grid) = if let Some(dv) = intrabc_dv.filter(|_| skip) {
        // lane-av1422ctattr: a SKIPPED intrabc chroma leaf still predicts the
        // frame copy at its DV. libaom's `decode_token_recon_block` takes the
        // INTER arm for an intrabc block (`is_inter_block` counts it,
        // `blockd.h:373`) and `predict_inter_block_visit` runs
        // `dec_build_inter_predictor` REGARDLESS of `skip_txfm` -- only the
        // residual read is dropped. Measured on the 4:2:2 seed of
        // `R422_320x242.obu` / `Q_odd320x242.obu` (chroma U(116,160)):
        //   oracle  EC_DP mi_row=40 mi_col=58 bsize=2 inter=1 skip_txfm=1 is_intrabc=1
        //           EC_ISTEP mi_row=40 mi_col=58 name=intrabc val=1 rng=39264
        //           EC_DV   mi_row=40 mi_col=58 dv_col=0 dv_row=-1024 rng=40116
        //   ours    the SAME symbol and the SAME DV (parse in sync), then
        //           OUR_PRED x=116 y=160 plane=1 mode=0 row0=[182,182,182,182]
        // i.e. this `if skip` arm was taken and the `else if let Some(dv)`
        // frame copy below never ran. 202 = (808+2)/4 is not a `dc_top`: there
        // is NO DC variant at all on the oracle side, because
        // `av1_predict_intra_block` is never called for an intrabc block.
        // `sub8_leaf_chroma444` already orders its arms this way
        // (`intrabc_dv.filter(|_| skip)` first, lane-av1ibcskip2); this arm is
        // that one, at 4:2:2's square 4x4 chroma unit.
        let mut ub = vec![0u16; 16];
        mc::predict_with_filter(
            &u.data,
            u.width,
            u.true_width,
            u.true_height,
            mv_to_q4(cpx, dv.1, ss_x(fctx)),
            mv_to_q4(cpy, dv.0, ss_y(fctx)),
            4,
            4,
            mc::InterpFilterKind::Bilinear,
            &mut ub,
            fctx,
        );
        hit!(SKIPPED_INTRABC_DV_COPY_HITS);
        let zeros = &ZERO_RESIDUAL[..16];
        set_palette_pred(ub, fctx);
        push_intra(
            y,
            u,
            v,
            1,
            cpx,
            cpy,
            4,
            DC_PRED,
            0,
            reach,
            zeros,
            None,
            None,
            smooth_neighbor_uv,
            fctx,
        );
        let mut vb = vec![0u16; 16];
        mc::predict_with_filter(
            &v.data,
            v.width,
            v.true_width,
            v.true_height,
            mv_to_q4(cpx, dv.1, ss_x(fctx)),
            mv_to_q4(cpy, dv.0, ss_y(fctx)),
            4,
            4,
            mc::InterpFilterKind::Bilinear,
            &mut vb,
            fctx,
        );
        set_palette_pred(vb, fctx);
        push_intra(
            y,
            u,
            v,
            2,
            cpx,
            cpy,
            4,
            DC_PRED,
            0,
            reach,
            zeros,
            None,
            None,
            smooth_neighbor_uv,
            fctx,
        );
        // The same arm-without-clear route the 4:2:0 tails and
        // [`sub8_leaf_chroma444`] count: a skipped leaf reads no coefficient,
        // so the inherited chroma `tx_type` is dropped unconsumed.
        if fctx.intrabc_chroma_tx.with(std::cell::Cell::get).is_some() {
            hit!(SKIPPED_INTRABC_CHROMA_ARM_HITS);
            fctx.intrabc_chroma_tx.with(|c| c.set(None));
        }
        (Grid::Zero(16), Grid::Zero(16))
    } else if skip {
        // A skipped chroma-reference leaf still predicts (the CfL contribution
        // survives a skip); no coefficient symbol exists.
        if alpha.is_some() {
            hit!(CHROMA_SKIP_CFL_HITS);
            hit!(SKIP_CFL_HITS);
        }
        push_intra(
            y,
            u,
            v,
            1,
            cpx,
            cpy,
            4,
            uv_predict_mode,
            angle_delta_uv,
            reach,
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
            cpx,
            cpy,
            4,
            uv_predict_mode,
            angle_delta_uv,
            reach,
            &ZERO_RESIDUAL[..16],
            alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
            None,
            smooth_neighbor_uv,
            fctx,
        );
        // The same arm-without-clear route the 4:2:0 tails count: a skipped
        // leaf reads no coefficient, so an armed intrabc slot is dropped.
        if fctx.intrabc_chroma_tx.with(std::cell::Cell::get).is_some() {
            hit!(SKIPPED_INTRABC_CHROMA_ARM_HITS);
            fctx.intrabc_chroma_tx.with(|c| c.set(None));
        }
        (Grid::Zero(16), Grid::Zero(16))
    } else if let Some(dv) = intrabc_dv {
        hit!(CHROMA422_SUB8_HITS);
        // Frame copy at the DV; the residual still reads the ordinary chroma
        // tables with the inherited luma `tx_type` (see the 4:2:0 tails).
        let mut ub = vec![0u16; 16];
        mc::predict_with_filter(
            &u.data,
            u.width,
            u.true_width,
            u.true_height,
            mv_to_q4(cpx, dv.1, ss_x(fctx)),
            mv_to_q4(cpy, dv.0, ss_y(fctx)),
            4,
            4,
            mc::InterpFilterKind::Bilinear,
            &mut ub,
            fctx,
        );
        let mut vb = vec![0u16; 16];
        mc::predict_with_filter(
            &v.data,
            v.width,
            v.true_width,
            v.true_height,
            mv_to_q4(cpx, dv.1, ss_x(fctx)),
            mv_to_q4(cpy, dv.0, ss_y(fctx)),
            4,
            4,
            mc::InterpFilterKind::Bilinear,
            &mut vb,
            fctx,
        );
        set_palette_pred(ub, fctx);
        let ug = read_plane(
            dec,
            cdfs,
            TxbSet::Chroma4,
            scan4,
            1,
            chroma_around[1],
            DC_PRED,
            DC_PRED,
            0,
            reach,
            y,
            u,
            v,
            cpx,
            cpy,
            4,
            TX4,
            base_q_idx,
            None,
            None,
            None,
            smooth_neighbor_uv,
            fctx,
        )?;
        set_palette_pred(vb, fctx);
        let vg = read_plane(
            dec,
            cdfs,
            TxbSet::Chroma4,
            scan4,
            2,
            chroma_around[2],
            DC_PRED,
            DC_PRED,
            0,
            reach,
            y,
            u,
            v,
            cpx,
            cpy,
            4,
            TX4,
            base_q_idx,
            None,
            None,
            None,
            smooth_neighbor_uv,
            fctx,
        )?;
        fctx.intrabc_chroma_tx.with(|c| c.set(None));
        (ug, vg)
    } else {
        hit!(CHROMA422_SUB8_HITS);
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
            reach,
            y,
            u,
            v,
            cpx,
            cpy,
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
            reach,
            y,
            u,
            v,
            cpx,
            cpy,
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
    // libaom `av1_set_entropy_contexts` stamps above/left PER TRANSFORM UNIT:
    // the unit's own chroma span -- 1 mi row tall, TWO luma mi columns wide
    // (one chroma column) -- not the 8x8 group's.
    let (u_grid, v_grid) = grids;
    let u_state = neighbour_state(&u_grid);
    let v_state = neighbour_state(&v_grid);
    if lmi.0 < neighbours.mi_rows {
        neighbours.left[ctx_mi.0][1] = u_state;
        neighbours.left[ctx_mi.0][2] = v_state;
    }
    for cell in 0..2 {
        if ctx_mi.1 + cell < neighbours.mi_cols {
            neighbours.above[ctx_mi.1 + cell][1] = u_state;
            neighbours.above[ctx_mi.1 + cell][2] = v_state;
        }
    }
    Ok((u_grid, v_grid))
}

/// Decodes a `PARTITION_SPLIT` of one 8x8 block into four `BLOCK_4X4` leaves
/// (spec `decode_partition`'s recursion bottom), sharing [`decode_leaf8`]'s
/// own signature so its caller's `prev_leaf`/`above_mode`/`left_mode`
/// bookkeeping needs no change. Each leaf reads its own luma mode info,
/// neighboured off the *previously decoded leaf in this same group* on
/// whichever axis is adjacent (mirroring [`decode_leaf8`]'s own `prev_leaf`
/// pattern, one level deeper); chroma is read and reconstructed exactly
/// once, on the last (bottom-right) leaf, at the whole 8x8 group's own 4x4
/// chroma unit -- [`read_intra_mode_sub8`]'s doc has the `is_chroma_reference`
/// derivation.
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_leaf_split4(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    outer_at: (usize, usize),
    leaf_mi: (usize, usize),
    scan4: &[u16],
    prev_leaf: Option<((usize, usize), usize)>,
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    base_q_idx: u8,
    enable_filter_intra: bool,
    allow_intrabc: bool,
    reduced_tx_set: bool,
    // Returns `(below_mode, right_mode)`: the mode a neighbour BELOW this 8x8
    // group sees (its bottom-left 4x4, libaom reads mi(row-1, col)) and the
    // one a neighbour to its RIGHT sees (its top-right 4x4, mi(row, col-1))., fctx: &crate::decode::FrameCtx
    fctx: &crate::decode::FrameCtx,
) -> Result<(usize, usize)> {
    hit!(SUB8_SPLIT_HITS);
    let (r, c) = outer_at;
    let mut above_mode = neighbours.above_mode[c];
    let mut left_mode = neighbours.left_mode[r];
    if let Some(((pr, pc), pmode)) = prev_leaf {
        if pc == leaf_mi.1 && leaf_mi.0 == pr + 2 {
            above_mode = pmode;
        } else if pr == leaf_mi.0 && leaf_mi.1 == pc + 2 {
            left_mode = pmode;
        }
    }
    // See `decode_leaf8`'s own `smooth_neighbor_uv`: the chroma edge-filter
    // type is the chroma neighbours' `uv_mode`, mi-exact where recorded.
    let smooth_neighbor_uv = neighbours.smooth_uv_neighbour(leaf_mi.0, leaf_mi.1, r, c, fctx);
    let mut leaf_modes = [0usize; 4];
    let mut leaf_skips = [false; 4];
    let mut last_uv: Option<(usize, i32, Option<(i32, i32)>)> = None;
    // lane-av1txr: the chroma-reference (i==3) leaf's block vector when that
    // leaf is an intrabc block -- its prediction/reconstruction below is a
    // frame copy at this DV, not an intra one.
    let mut last_intrabc: Option<(i32, i32)> = None;
    // 4:4:4 (lane-av1-444): `is_chroma_reference` (av1_common_int.h:1459) is
    // unconditional at subsampling 0/0, so EVERY 4x4 leaf reads its own
    // `uv_mode` and codes its own full-resolution 4x4 chroma unit right
    // after its luma -- there is no shared group unit at all.
    let chroma_444 = ss_x(fctx) == 0 && ss_y(fctx) == 0;
    // lane-av1-422g: at ss (1,0) every even-mi-column leaf is its plane's own
    // chroma reference (`is_chroma_reference`'s `!(mi_col & ss_x)`) and codes
    // one TX_4X4 chroma unit per plane; odd-column leaves read no chroma.
    let chroma_422 = ss_x(fctx) == 1 && ss_y(fctx) == 0;
    // The last leaf's own `uv_predict_mode`, for the coarse above/left slots
    // the 4:2:0 tail fills from the group's single unit.
    let mut last_uv444 = DC_PRED;
    let mut last_uv422 = DC_PRED;
    for i in 0..4usize {
        let (dr, dc) = (i / 2, i % 2);
        let lmi = (leaf_mi.0 + dr, leaf_mi.1 + dc);
        // Neighbour modes at real mi granularity: inside the group the
        // sibling leaf, outside it the split-leaf map when the adjacent mi
        // was itself a 4x4 leaf, else the coarse 16x16 slot (see
        // `sub8_mode_col`'s doc).
        let leaf_above = if dr == 0 {
            neighbours.mode_above_mi(lmi.0, lmi.1).unwrap_or(above_mode)
        } else {
            leaf_modes[i - 2]
        };
        let leaf_left = if dc == 0 {
            neighbours.mode_left_mi(lmi.0, lmi.1).unwrap_or(left_mode)
        } else {
            leaf_modes[i - 1]
        };
        let has_chroma = if chroma_444 {
            true
        } else if chroma_422 {
            // lane-av1-422h: `is_chroma_reference`'s column clause at ss_x 1 is
            // `(mi_col & 1)` for a BLOCK_4X4 leaf -- the ODD-column leaf is the
            // chroma reference (oracle, mi(30,42) split group: uv_mode read at
            // mi(30,43) and mi(31,43) only). The even-column rule this
            // replaces came from 422f's pre-partition-trace misreading.
            (lmi.1 & 1) == 1
        } else {
            i == 3
        };
        let skip_ctx = neighbours.skip_txfm_ctx(lmi.0, lmi.1);
        let (skip, mode, angle_delta_y, chroma, filter_intra, intrabc) = read_intra_mode_sub8(
            dec,
            cdfs,
            leaf_above,
            leaf_left,
            has_chroma,
            enable_filter_intra,
            filter_intra_size_class(4).expect("BLOCK_4X4 has its own filter_intra row"),
            skip_ctx,
            allow_intrabc,
            lmi.0,
            lmi.1,
            1,
            1,
            fctx,
        )?;
        leaf_modes[i] = mode;
        leaf_skips[i] = skip;
        neighbours.record_mode_mi(lmi.0, lmi.1, 1, 1, mode);
        // lane-av1txr: libaom `parse_decode_block`'s `set_txfm_ctxs` for this
        // `BLOCK_4X4` leaf -- EVERY block publishes its transform size into
        // the TXFM context bands, and an intrabc block's later `txfm_split`
        // context is read off them (`txfm_partition_context`). A 4x4 never
        // signals a tx size, so it is always TX_4X4 (`set_txfm_ctxs`'s
        // `skip && is_inter` branch writes the block's own 4 pixels -- the
        // same value).
        set_txfm_ctxs(neighbours, lmi, 4, 1, 1, false);
        let smooth_neighbor = is_smooth_mode(leaf_above) || is_smooth_mode(leaf_left);
        if smooth_neighbor {
            hit!(SMOOTH_LUMA_HITS);
        }
        let (px, py) = (lmi.1 * MI, lmi.0 * MI);
        let reach = Reach::of(4, px, py, y.width, y.height, fctx);
        if let Some(dv) = intrabc {
            // lane-av1txr: an intrabc `BLOCK_4X4` leaf -- prediction is the
            // CURRENT frame at the block vector (`predict_inter_block`'s
            // intrabc arm, spec 7.11.3.4), residual the INTER coefficient set
            // (libaom reads `tx_type` off `inter_ext_tx_cdf` for
            // `is_inter_block`, which counts intrabc). Published into
            // `intrabc_dv` so the group's chroma arm (below, when the
            // chroma-reference leaf is the intrabc one) and
            // [`record_intrabc_mi`] see it.
            let mut yb = vec![0u16; 16];
            mc::predict_with_filter(
                &y.data,
                y.width,
                y.true_width,
                y.true_height,
                mv_to_q4(px, dv.1, 0),
                mv_to_q4(py, dv.0, 0),
                4,
                4,
                mc::InterpFilterKind::Bilinear,
                &mut yb,
                fctx,
            );
            set_palette_pred(yb.clone(), fctx);
            if skip {
                push_intra(
                    y,
                    u,
                    v,
                    0,
                    px,
                    py,
                    4,
                    DC_PRED,
                    0,
                    reach,
                    &ZERO_RESIDUAL[..16],
                    None,
                    None,
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
                        TxbSet::Luma4Inter
                    } else {
                        TxbSet::Luma4InterSet1
                    },
                    scan4,
                    0,
                    tu_around,
                    DC_PRED,
                    DC_PRED,
                    0,
                    reach,
                    y,
                    u,
                    v,
                    px,
                    py,
                    4,
                    4,
                    base_q_idx,
                    None,
                    None,
                    None,
                    smooth_neighbor,
                    fctx,
                )?;
                neighbours.record_mi_luma(lmi, 4, &tu_grid);
            }
            neighbours.fill_skip_grid(lmi, 1, skip);
            neighbours.fill_lf_grid(lmi, 1, 4, 0, fctx);
            record_intrabc_mi(lmi.0, lmi.1, 1, Some(dv), neighbours, fctx);
            if has_chroma {
                // An intrabc block is an INTER block for `av1_get_tx_type`, so
                // the chroma inherits the LUMA's coded transform type
                // (`xd->tx_type_map` at the co-located luma position) -- the
                // luma `read_plane` above just set [`LUMA_TX_TYPE`]. Armed for
                // exactly this leaf's two chroma reads.
                fctx.intrabc_chroma_tx
                    .with(|c| c.set(Some(fctx.luma_tx_type.with(std::cell::Cell::get))));
                if chroma_444 {
                    // lane-av1-444: this leaf codes its OWN 4x4 chroma unit
                    // right here (frame copy at the DV); no group tail.
                    sub8_leaf_chroma444(
                        dec,
                        cdfs,
                        neighbours,
                        lmi,
                        4,
                        4,
                        DC_PRED,
                        None,
                        skip,
                        Some(dv),
                        y,
                        u,
                        v,
                        scan4,
                        base_q_idx,
                        neighbours.smooth_uv_neighbour(lmi.0, lmi.1, r, c, fctx),
                        fctx,
                    )?;
                    last_uv444 = DC_PRED;
                } else if chroma_422 {
                    sub8_leaf_chroma422(
                        dec,
                        cdfs,
                        neighbours,
                        lmi,
                        DC_PRED,
                        0,
                        None,
                        skip,
                        Some(dv),
                        y,
                        u,
                        v,
                        scan4,
                        base_q_idx,
                        neighbours.smooth_uv_neighbour(lmi.0, lmi.1, r, c, fctx),
                        (4, 4),
                        fctx,
                    )?;
                    last_uv422 = DC_PRED;
                } else {
                    last_uv = Some((DC_PRED, 0, None));
                    // lane-av1txbands: the group's chroma is the CHROMA-REFERENCE
                    // leaf's own block -- libaom codes one chroma unit under the
                    // last sub-block only (`is_chroma_reference`), so its prediction
                    // is that leaf's `uv_mode` (CfL alpha and all) unless THAT leaf
                    // is the intrabc one. Arming this from a non-chroma-reference
                    // leaf's DV made `warped.obu`'s 8x8 group at mi(40,62) copy
                    // frame bytes at mi(41,62)'s vector where aomdec predicts
                    // `uv_mode=UV_CFL_PRED`: luma stayed byte-exact and only chroma
                    // moved (class chroma-keyed-at-a-non-chroma-leaf). Mirrors
                    // [`decode_leaf_rect8`]'s own placement.
                    last_intrabc = Some(dv);
                }
            }
            continue;
        }
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
            // A `BLOCK_4X4` leaf's luma TU *is* the whole block, so libaom's
            // `get_txb_ctx` takes its `plane_bsize == txsize_to_bsize[tx_size]`
            // branch: `txb_skip_ctx = 0`, never the neighbour-magnitude table
            // (`txb_common.h:400`). Reading the table here decoded `all_zero`
            // off the wrong CDF on the very first leaf and desynced the tile.
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
                4,
                base_q_idx,
                None,
                filter_intra,
                None,
                smooth_neighbor,
                fctx,
            )?;
            neighbours.record_mi_luma(lmi, 4, &tu_grid);
        }
        neighbours.fill_skip_grid(lmi, 1, skip);
        neighbours.fill_lf_grid(lmi, 1, 4, 0, fctx);
        if has_chroma {
            if chroma_444 {
                // lane-av1-444: the leaf's own 4x4 chroma unit, coded right
                // after its luma (no shared group unit at ss 0/0).
                let (uv_mode, _, alpha) = chroma.expect("4:4:4 reads uv_mode on every leaf");
                sub8_leaf_chroma444(
                    dec,
                    cdfs,
                    neighbours,
                    lmi,
                    4,
                    4,
                    uv_mode,
                    alpha,
                    skip,
                    None,
                    y,
                    u,
                    v,
                    scan4,
                    base_q_idx,
                    neighbours.smooth_uv_neighbour(lmi.0, lmi.1, r, c, fctx),
                    fctx,
                )?;
                last_uv444 = if uv_mode == UV_CFL_PRED {
                    DC_PRED
                } else {
                    uv_mode
                };
            } else if chroma_422 {
                let (uv_mode, angle_delta_uv, alpha) =
                    chroma.expect("4:2:2 reads uv_mode on even-column leaves");
                sub8_leaf_chroma422(
                    dec,
                    cdfs,
                    neighbours,
                    lmi,
                    uv_mode,
                    angle_delta_uv,
                    alpha,
                    skip,
                    None,
                    y,
                    u,
                    v,
                    scan4,
                    base_q_idx,
                    neighbours.smooth_uv_neighbour(lmi.0, lmi.1, r, c, fctx),
                    (4, 4),
                    fctx,
                )?;
                last_uv422 = if uv_mode == UV_CFL_PRED {
                    DC_PRED
                } else {
                    uv_mode
                };
            } else {
                last_uv = chroma;
            }
        }
    }
    // lane-av1mono: as [`decode_leaf_rect8`] -- a monochrome frame has no
    // chroma to reconstruct and no uv_mode was read.
    let (u_grid, v_grid): (Grid, Grid) = if mono(fctx) {
        (Grid::Zero(16), Grid::Zero(16))
    } else if chroma_444 {
        // lane-av1-444: every leaf coded and reconstructed its own chroma in
        // the loop above ([`sub8_leaf_chroma444`] stamped the mi-exact maps
        // and per-leaf entropy state); only the coarse slots remain, filled
        // from the bottom-right leaf like the 4:2:0 tail's single unit.
        neighbours.above_uv_mode[c] = last_uv444;
        neighbours.left_uv_mode[r] = last_uv444;
        (Grid::Zero(16), Grid::Zero(16))
    } else if chroma_422 {
        // lane-av1-422g: each even-column leaf coded and reconstructed its
        // own TX_4X4 chroma unit in the loop above (sub8_leaf_chroma422
        // stamped the per-leaf entropy state); only the coarse slots remain,
        // filled from the group's last chroma-reference leaf (bottom-left).
        neighbours.above_uv_mode[c] = last_uv422;
        neighbours.left_uv_mode[r] = last_uv422;
        (Grid::Zero(16), Grid::Zero(16))
    } else {
        let (gpx, gpy) = (leaf_mi.1 * MI, leaf_mi.0 * MI);
        let (cpx, cpy) = (gpx >> ss_x(fctx), gpy >> ss_y(fctx));
        let group_reach = Reach::of(8, gpx, gpy, y.width, y.height, fctx);
        let (uv_mode, angle_delta_uv, alpha) =
            last_uv.expect("i==3 always sets has_chroma, so chroma is always Some");
        let uv_predict_mode = if uv_mode == UV_CFL_PRED {
            DC_PRED
        } else {
            uv_mode
        };
        neighbours.above_uv_mode[c] = uv_predict_mode;
        neighbours.left_uv_mode[r] = uv_predict_mode;
        neighbours.record_uv_mode_mi(leaf_mi.0, leaf_mi.1, 2, 2, uv_predict_mode);
        let (u_grid, v_grid): (Grid, Grid) = if leaf_skips[3] {
            // A skipped chroma block still gets its CfL contribution: libaom's
            // `predict_and_reconstruct_intra_block` calls `cfl_predict_block`
            // from `av1_predict_intra_block_facade` (av1/common/reconintra.c:1719)
            // whenever `mbmi->uv_mode == UV_CFL_PRED`, before any residual is
            // added -- `skip_txfm` only drops the residual, never the alpha.
            // lane-reelckf and lane-kf1200 fixed THIS SAME arm independently; the
            // merge keeps one fix and feeds both lanes' counters so neither of
            // their gates goes vacuous.
            let ac = alpha.map(|_| cfl_src(gpx, gpy, 8));
            if alpha.is_some() {
                hit!(CHROMA_SKIP_CFL_HITS);
                hit!(SKIP_CFL_HITS);
            }
            push_intra(
                y,
                u,
                v,
                1,
                cpx,
                cpy,
                4,
                uv_predict_mode,
                angle_delta_uv,
                group_reach,
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
                cpx,
                cpy,
                4,
                uv_predict_mode,
                angle_delta_uv,
                group_reach,
                &ZERO_RESIDUAL[..16],
                alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
                None,
                smooth_neighbor_uv,
                fctx,
            );
            // lane-av1txbands: a SKIPPED chroma-reference leaf reads no chroma
            // coefficient, so it never consults the inherited luma transform type
            // -- but an intrabc chroma-reference leaf did arm
            // [`INTRABC_CHROMA_TX`] above (`has_chroma`), and the two coefficient
            // branches below are the only ones that disarm it. Left armed, the slot
            // picks the NEXT block's chroma scan and inverse transform (class
            // arm-without-clear-on-a-skip-route). Counted so a gate can tell a
            // stream that reaches this route from one that never does.
            if fctx.intrabc_chroma_tx.with(std::cell::Cell::get).is_some() {
                hit!(SKIPPED_INTRABC_CHROMA_ARM_HITS);
                fctx.intrabc_chroma_tx.with(|c| c.set(None));
            }
            (Grid::Zero(16), Grid::Zero(16))
        } else if let Some(dv) = last_intrabc {
            // lane-av1txr: the chroma-reference leaf was an intrabc block, so the
            // group's one chroma unit is a frame copy at that DV too (its residual
            // still reads the ordinary `Chroma4` coefficient tables -- libaom
            // codes a chroma `tx_type` nowhere).
            let mut ub = vec![0u16; 16];
            mc::predict_with_filter(
                &u.data,
                u.width,
                u.true_width,
                u.true_height,
                mv_to_q4(cpx, dv.1, ss_x(fctx)),
                mv_to_q4(cpy, dv.0, ss_y(fctx)),
                4,
                4,
                mc::InterpFilterKind::Bilinear,
                &mut ub,
                fctx,
            );
            let mut vb = vec![0u16; 16];
            mc::predict_with_filter(
                &v.data,
                v.width,
                v.true_width,
                v.true_height,
                mv_to_q4(cpx, dv.1, ss_x(fctx)),
                mv_to_q4(cpy, dv.0, ss_y(fctx)),
                4,
                4,
                mc::InterpFilterKind::Bilinear,
                &mut vb,
                fctx,
            );
            let chroma_around = neighbours.around_mi(leaf_mi, 8);
            set_palette_pred(ub, fctx);
            let ug = read_plane(
                dec,
                cdfs,
                TxbSet::Chroma4,
                scan4,
                1,
                chroma_around[1],
                DC_PRED,
                DC_PRED,
                0,
                group_reach,
                y,
                u,
                v,
                cpx,
                cpy,
                4,
                TX4,
                base_q_idx,
                None,
                None,
                None,
                smooth_neighbor_uv,
                fctx,
            )?;
            set_palette_pred(vb, fctx);
            let vg = read_plane(
                dec,
                cdfs,
                TxbSet::Chroma4,
                scan4,
                2,
                chroma_around[2],
                DC_PRED,
                DC_PRED,
                0,
                group_reach,
                y,
                u,
                v,
                cpx,
                cpy,
                4,
                TX4,
                base_q_idx,
                None,
                None,
                None,
                smooth_neighbor_uv,
                fctx,
            )?;
            fctx.intrabc_chroma_tx.with(|c| c.set(None));
            (ug, vg)
        } else {
            let ac = alpha.map(|_| cfl_src(gpx, gpy, 8));
            let chroma_around = neighbours.around_mi(leaf_mi, 8);
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
                group_reach,
                y,
                u,
                v,
                cpx,
                cpy,
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
                group_reach,
                y,
                u,
                v,
                cpx,
                cpy,
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
        (u_grid, v_grid)
    };
    // Chroma neighbour bookkeeping for the whole 8x8 group -- mirrors
    // [`decode_leaf8`]'s own tx4-chroma section (`record_split_luma`'s
    // rescaling is wrong for an already-mi-unit position, so this inlines
    // its tail directly at `leaf_mi`, side_mi=2), EXCEPT for the partition
    // context value itself. Unlike `decode_leaf8`'s TX4 branch (still one
    // `BLOCK_8X8`, `PARTITION_NONE`, transform depth only), this group is
    // genuinely four `BLOCK_4X4` blocks (`PARTITION_SPLIT` at the 8x8
    // level, libaom `decode_partition`'s `bsize2`/`subsize` recursion
    // bottom). libaom's `update_ext_partition_context` writes
    // `partition_context_lookup[subsize]` at the 8x8 slot -- `{30,30}` for
    // `BLOCK_8X8` (bit0=0, what `decode_leaf8` already reproduces via
    // side_mi=8) but `{31,31}` for `BLOCK_4X4` (bit0=1) here, and
    // `partition_ctx_mi`'s `left_side_mi[mi_r]*2 <= side` needs
    // `left_side_mi <= 4` to read that bit as 1. Leaving this at 8 (this
    // round's bug) makes the very next sibling 8x8's `partition_w8` read
    // with the wrong context and desync immediately.
    let side_mi = 2;
    for cell in 0..side_mi {
        neighbours.left_side_mi[leaf_mi.0 + cell] = 4;
        neighbours.above_side_mi[leaf_mi.1 + cell] = 4;
    }
    // lane-av1-444: at 4:4:4 [`sub8_leaf_chroma444`] already stamped each
    // leaf's own span as it decoded; these placeholder grids carry no state.
    // lane-av1-422g: the 4:2:2 per-leaf units stamp the same way.
    if !chroma_444 && !chroma_422 {
        let round_up_even = |n: usize| n.div_ceil(2) * 2;
        let bound_h = round_up_even(neighbours.mi_rows);
        let bound_w = round_up_even(neighbours.mi_cols);
        let u_state = neighbour_state(&u_grid);
        let v_state = neighbour_state(&v_grid);
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
    // lane-dkey: a sub-8x8 leaf can never use palette (`av1_allow_palette`
    // needs BLOCK_8X8 and up), and libaom's mi grid says so -- its
    // `palette_size` is 0 in every one of these mi cells, so the NEXT block's
    // `av1_get_palette_mode_ctx` and colour cache read 0 here. This decoder
    // only ever WROTE palette state from the >=8x8 readers, so an 8x8 palette
    // block above a 4x4 group left its size/colours standing in the mi-granular
    // bands: the block below the group read `palette_y_mode` off CDF row 1
    // where libaom reads row 0 (class [[cdf-row-held-constant]]), decoded a
    // palette libaom never coded, and then skipped the `use_filter_intra`
    // symbol that a palette block has none of -- one symbol short, tile
    // desynced. Publishing the group's own zeros is the clear.
    neighbours.record_palette_y_rect(leaf_mi, 8, 8, 0, [0u16; 8]);
    neighbours.record_palette_uv_rect(leaf_mi, 8, 8, 0, [0u16; 8]);
    Ok((leaf_modes[2], leaf_modes[1]))
}

/// Decodes a `PARTITION_VERT`/`PARTITION_HORZ` of one 8x8 block into two
/// `BLOCK_4X8`/`BLOCK_8X4` leaves (lane-tx4x8), the last partition shape
/// below 8x8 this decoder refused. Mirrors [`decode_leaf_split4`] element for
/// element -- same mode reads ([`read_intra_mode_sub8`]: no angle delta, no
/// tx-depth symbol below 8x8), same "chroma once, on the last sub-block, at
/// the whole 8x8 group's 4x4 unit" rule (`is_chroma_reference` is false for
/// every sub-block one mi wide or high but the last) -- differing only where
/// the shape does:
///
/// * the transform is a true `TX_4X8`/`TX_8X4`
///   ([`dequant_and_inverse_typed_wh`], whose `abs(rect_type) == 1` scale and
///   `av1_inv_txfm_shift_ls` row already cover this size pair);
/// * the CDF set is [`TxbSet::LumaRect4x8`] (`get_txsize_entropy_ctx` squares
///   up to `TX_8X8`) with a 32-position `eob_pt` and the `TX_4X4` `tx_type`
///   row -- see that variant's doc comment;
/// * the scan is [`SCAN_4X8`]/[`SCAN_8X4`], or their `mrow`/`mcol` siblings
///   ([`class_scan_table_wh`]) when the coded `tx_type` is `V_DCT`/`H_DCT`;
/// * `Reach` comes from libaom's own `has_tr_4x8`/`has_bl_8x4` rows
///   ([`Reach::of_rect`]);
/// * `update_partition_context` writes `partition_context_lookup[BLOCK_4X8]`
///   = `{31, 30}` (width 4 above, height 8 left), not the `{31, 31}` a
///   `BLOCK_4X4` split writes.
///
/// Filter intra on one of these leaves is decoded (lane-fi8 r1): the flag is
/// read from `BLOCK_4X8`/`BLOCK_8X4`'s own `cdf::FILTER_INTRA` row (classes
/// 8/9, NOT `BLOCK_4X4`'s), the predictor is [`crate::intra::predict_filter_intra`]
/// over the leaf's true `bw`x`bh` -- per 4x4 transform unit, with that unit's
/// own edge, when `tx_depth` splits it -- and the coded `tx_type` row is
/// [`fi_tx_row`]'s `fimode_to_intradir`, not the block's raw `DC_PRED`.
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_leaf_rect8(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    outer_at: (usize, usize),
    leaf_mi: (usize, usize),
    vert: bool,
    scan4: &[u16],
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    base_q_idx: u8,
    enable_filter_intra: bool,
    allow_intrabc: bool,
    tx_select: bool,
    reduced_tx_set: bool,
    fctx: &crate::decode::FrameCtx,
) -> Result<(usize, usize)> {
    let (r, c) = outer_at;
    // lane-modectx r1: availability-correct fallback pair (see
    // `modes_above_left`), not the raw coarse bands.
    let (above_mode, left_mode) = neighbours.modes_above_left(r, c);
    let smooth_neighbor_uv = neighbours.smooth_uv_neighbour(leaf_mi.0, leaf_mi.1, r, c, fctx);
    let (bw, bh) = if vert { (4, 8) } else { (8, 4) };
    let (w_mi, h_mi) = (bw / MI, bh / MI);
    let scan: &[u16] = if vert { &SCAN_4X8 } else { &SCAN_8X4 };
    let mut leaf_modes = [0usize; 2];
    let mut leaf_skips = [false; 2];
    let mut last_uv: Option<(usize, i32, Option<(i32, i32)>)> = None;
    // lane-av1txr: as [`decode_leaf_split4`]'s own `last_intrabc`.
    let mut last_intrabc: Option<(i32, i32)> = None;
    // 4:4:4 (lane-av1-444): as [`decode_leaf_split4`] -- at subsampling 0/0
    // EVERY 4x8/8x4 leaf reads its own `uv_mode` and codes its own
    // full-resolution rect chroma unit right after its luma.
    let chroma_444 = ss_x(fctx) == 0 && ss_y(fctx) == 0;
    let mut last_uv444 = DC_PRED;
    // lane-av1-422h: at ss (1,0) `is_chroma_reference`
    // (av1_common_int.h:1459) resolves per strip shape --
    // `((mi_row & 1) || !(bh_mi & 1) || !ss_y) && ((mi_col & 1) || !(bw_mi & 1) || !ss_x)`:
    // a HORZ pair is two BLOCK_8X4 strips at the group's even column, so
    // BOTH strips are chroma references (`!(bw & 1)`); a VERT pair is two
    // BLOCK_4X8 strips, and the column clause keeps only the ODD-column
    // strip (i == 1, the same leaf the 4:2:0 rule reads chroma on).
    let chroma_422 = ss_x(fctx) == 1 && ss_y(fctx) == 0;
    let mut last_uv422 = DC_PRED;
    for i in 0..2usize {
        let lmi = if vert {
            (leaf_mi.0, leaf_mi.1 + i)
        } else {
            (leaf_mi.0 + i, leaf_mi.1)
        };
        // Inside the pair the sibling is the neighbour on the split axis;
        // outside it the mi-granular map, else the coarse 16x16 slot.
        let leaf_above = if !vert && i == 1 {
            leaf_modes[0]
        } else {
            neighbours.mode_above_mi(lmi.0, lmi.1).unwrap_or(above_mode)
        };
        let leaf_left = if vert && i == 1 {
            leaf_modes[0]
        } else {
            neighbours.mode_left_mi(lmi.0, lmi.1).unwrap_or(left_mode)
        };
        let has_chroma = if chroma_444 {
            true
        } else if chroma_422 {
            !vert || i == 1
        } else {
            i == 1
        };
        let skip_ctx = neighbours.skip_txfm_ctx(lmi.0, lmi.1);
        let (skip, mode, angle_delta_y, chroma, filter_intra, intrabc) = read_intra_mode_sub8(
            dec,
            cdfs,
            leaf_above,
            leaf_left,
            has_chroma,
            enable_filter_intra,
            filter_intra_size_class_rect(bw, bh)
                .expect("BLOCK_4X8/BLOCK_8X4 are inside av1_filter_intra_allowed_bsize"),
            skip_ctx,
            allow_intrabc,
            lmi.0,
            lmi.1,
            bw / 4,
            bh / 4,
            fctx,
        )?;
        leaf_modes[i] = mode;
        leaf_skips[i] = skip;
        neighbours.record_mode_mi(lmi.0, lmi.1, w_mi, h_mi, mode);
        let smooth_neighbor = is_smooth_mode(leaf_above) || is_smooth_mode(leaf_left);
        if let Some(dv) = intrabc {
            // lane-av1-ibc444rect: the unskipped lossless luma walk this lane
            // fixes (one rect unit -> per-4x4 inter units).
            if !skip && lossless(fctx) {
                hit!(INTRABC_RECT8_LOSSLESS_LUMA_HITS);
            }
            // lane-av1txr: an intrabc `BLOCK_4X8`/`BLOCK_8X4` leaf. The
            // prediction is a frame copy at the DV and the residual reads the
            // INTER `LumaRect8x4` set -- but with `TxMode::Select` the leaf
            // reads the INTER var-tx tree, exactly like every other intrabc
            // block: libaom's `block_signals_txsize` is `bsize > BLOCK_4X4`
            // (`blockd.h:1027`), TRUE at `BLOCK_4X8`/`BLOCK_8X4`, so
            // `parse_decode_block` sends an unskipped `inter_block_tx` block
            // (intrabc counts, `is_inter_block`) of a signalling bsize into
            // `read_tx_size_vartx` over `max_txsize_rect_lookup[BLOCK_4X8] ==
            // TX_4X8`; only `BLOCK_4X4` reads no tx symbol at all. Measured on
            // `warped.obu` mi (37,62): reading NO symbol here left the
            // arithmetic decoder one symbol short and desynced the very next
            // block's `skip` (ours 60320 vs aomdec 48630, class
            // `symbol-consumption-gap`).
            let (px, py) = (lmi.1 * MI, lmi.0 * MI);
            let reach = Reach::of_rect(bw, bh, px, py, y.width, y.height, fctx);
            let mut luma_leaves: Vec<(usize, usize, usize, usize)> = Vec::new();
            // The non-var-tx band publish, deferred to AFTER the unit loop:
            // libaom's `set_txfm_ctxs` runs at `parse_decode_block`'s tail, so
            // a unit's own `txb_skip_ctx` read must see the PRE-block bands.
            let mut publish = Some((bw, bh));
            // lane-av1mixllfork: libaom `parse_decode_block` (decodeframe.c:1237)
            // gates the var-tx tree on `!xd->lossless[mbmi->segment_id]` as a
            // CONJUNCT of the tx_mode-select condition, not as the `else` arm --
            // `read_tx_size` (`decodeframe.c:1208`) answers TX_4X4 on the same
            // flag. A frame whose segments disagree (per-segment mixed
            // lossless: `--cq-level=0` with AQ) therefore reads NO tree symbol
            // on its lossless blocks. Ordering the test the other way round
            // reads a `tx_size_cat1` symbol the oracle never reads and forks
            // the very next block's `txb_skip`.
            if tx_select && !skip && !lossless(fctx) {
                // `max_block_wide`/`max_block_high`: the frame's own mi grid,
                // so a leaf hanging over the right/bottom edge reads no split
                // symbol for the units outside it (as in `decode_block`).
                let mi_cols = 2 * y.true_width.div_ceil(8);
                let mi_rows = 2 * y.true_height.div_ceil(8);
                let max_w_mi = w_mi.min(mi_cols.saturating_sub(lmi.1));
                let max_h_mi = h_mi.min(mi_rows.saturating_sub(lmi.0));
                read_var_tx_size(
                    dec,
                    cdfs,
                    neighbours,
                    lmi,
                    bw.max(bh),
                    (max_w_mi, max_h_mi),
                    (bw, bh),
                    0,
                    0,
                    0,
                    &mut luma_leaves,
                );
                hit!(RECT_INTRABC_VARTX_HITS);
                publish = None;
            } else if lossless(fctx) && !skip {
                // lane-av1-ibc444rect: `read_tx_size`'s lossless first line
                // (`decodeframe.c:1203`) answers TX_4X4 for EVERY plane before
                // any tree, and a lossless frame's `TxMode` is `ONLY_4X4`
                // (spec 6.8.20) so no var-tx tree is read either -- the leaf's
                // coefficients are (bw/4)*(bh/4) inter TX_4X4 units at their
                // own origins, plane-major, NOT the single rect transform the
                // non-lossless arm reads (that one is
                // `av1_get_max_uv_txsize(BLOCK_8X4)`'s rect, unreachable at
                // lossless). Same unit structure the non-intrabc lossless
                // split below walks; each unit predicts from the CURRENT
                // frame at the DV. Measured on the lane's `e4` fixture
                // (`ll444_ibc_rect.obu`): the single-rect read desynced the
                // tile and tripped the lossless WHT assert `(4, 8) != (4, 4)`
                // at `TxParams::run`.
                for ur in 0..bh / 4usize {
                    for uc in 0..bw / 4usize {
                        luma_leaves.push((ur, uc, 4, 4));
                    }
                }
                publish = Some((4, 4));
            } else {
                luma_leaves.push((0, 0, bw, bh));
                // The non-var-tx arm: a SKIPPED intrabc leaf (or a frame
                // outside `TxMode::Select`) publishes its own rect transform
                // (`set_txfm_ctxs`'s `skip && is_inter` branch writes the
                // block's own pixels -- the same size here).
            }
            // The transform type an intrabc block's CHROMA inherits is the
            // `tx_type_map` entry at the chroma unit's co-located luma
            // position (`av1_get_tx_type`, `blockd.h:1278`), i.e. the FIRST
            // luma unit the tree resolved.
            let single = luma_leaves.len() == 1;
            let mut chroma_tx = TxType::DctDct;
            let mut grid = Grid::Zero(bw * bh);
            for (li, &(tu_row, tu_col, tw, th)) in luma_leaves.iter().enumerate() {
                let tu_mi = (lmi.0 + tu_row, lmi.1 + tu_col);
                let (tu_px, tu_py) = (tu_mi.1 * MI, tu_mi.0 * MI);
                let tu_reach = if single {
                    reach
                } else {
                    tu_reach(
                        bw,
                        bh,
                        tu_px - px,
                        tu_py - py,
                        tw,
                        reach,
                        px,
                        py,
                        y.width,
                        y.height,
                        fctx,
                    )
                };
                let mut yb = vec![0u16; tw * th];
                mc::predict_with_filter(
                    &y.data,
                    y.width,
                    y.true_width,
                    y.true_height,
                    mv_to_q4(tu_px, dv.1, 0),
                    mv_to_q4(tu_py, dv.0, 0),
                    tw,
                    th,
                    mc::InterpFilterKind::Bilinear,
                    &mut yb,
                    fctx,
                );
                set_palette_pred(yb, fctx);
                let tu_grid = if skip {
                    let zeros = Grid::Zero(tw * th);
                    if (tw, th) == (4, 4) {
                        push_intra(
                            y,
                            u,
                            v,
                            0,
                            tu_px,
                            tu_py,
                            4,
                            DC_PRED,
                            0,
                            tu_reach,
                            &zeros,
                            None,
                            None,
                            smooth_neighbor,
                            fctx,
                        );
                    } else {
                        push_intra_rect(
                            y,
                            u,
                            v,
                            0,
                            tu_px,
                            tu_py,
                            tw,
                            th,
                            DC_PRED,
                            0,
                            tu_reach,
                            &zeros,
                            None,
                            None,
                            smooth_neighbor,
                            fctx,
                        );
                    }
                    zeros
                } else {
                    // A unit smaller than its own block (the var-tx split)
                    // reads its `txb_skip_ctx` off the neighbours; the
                    // whole-leaf unit's `plane_bsize == txsize_to_bsize` fixes
                    // it at 0.
                    let (set, tu_scan, around, skip_ctx) = if (tw, th) == (4, 4) {
                        (
                            inter_txbset_for(4, reduced_tx_set),
                            scan4,
                            neighbours.around_mi(tu_mi, 4)[0],
                            neighbours.luma_skip_ctx(tu_mi, 1),
                        )
                    } else {
                        (
                            if reduced_tx_set {
                                TxbSet::LumaRect8x4Inter
                            } else {
                                TxbSet::LumaRect8x4InterSet1
                            },
                            scan,
                            neighbours.around_mi_rect(tu_mi, tw, th)[0],
                            0,
                        )
                    };
                    let mut coding = cdfs.txb(set, DC_PRED);
                    // lane-av1-ibc444rect: `av1_read_tx_type` returns before any
                    // symbol for a lossless block (qindex == 0, decodemv.c:681)
                    // on EVERY plane and size -- the lossless units this arm now
                    // walks must not read the set's tx_type either (`read_plane`
                    // does the same for its own call sites). Measured on the
                    // lane's `ll444_ibc_rect.obu`: with the symbol read, the
                    // first unit's ladder diverged right after a matched
                    // `all_zero` (ours +4096 rng on the unused tx_type).
                    if lossless(fctx) {
                        coding.tx_type = None;
                    }
                    let (levels, tx_type) = read_coeffs_rect(
                        dec,
                        &mut coding,
                        tu_scan,
                        tw,
                        th,
                        skip_ctx,
                        dc_sign_ctx(around.2),
                        TxType::DctDct,
                        0,
                    )?;
                    if li == 0 {
                        chroma_tx = tx_type;
                        fctx.luma_tx_type.with(|c| c.set(tx_type));
                    }
                    // lane-av1-ibc444rect: the residual goes through [`TxParams`]
                    // with the frame's lossless flag, exactly like
                    // [`read_plane`]'s own dequant -- at lossless `TxParams::run`
                    // is the Walsh-Hadamard inverse on every plane, and the
                    // `dequant_and_inverse_typed_wh` shortcut the lossy var-tx
                    // callers use has no lossless flag (it would run the DCT
                    // inverse over the WHT-ordered levels). Measured on the lane
                    // fixture: the DCT route left ±1..30 sample errors on every
                    // walked leaf's luma.
                    let tx = TxParams {
                        plane: 0,
                        w: tw,
                        h: th,
                        bit_depth: crate::decode::bit_depth(fctx),
                        q_idx: block_q_idx(fctx),
                        dc_delta: plane_q_delta(0, fctx).0,
                        ac_delta: plane_q_delta(0, fctx).1,
                        tx_type,
                        qm: block_iqmatrix(fctx, 0, tw, th, tx_type),
                        stride: 0,
                        lossless: lossless(fctx),
                    };
                    let residual = tx.run(&levels);
                    if (tw, th) == (4, 4) {
                        push_intra(
                            y,
                            u,
                            v,
                            0,
                            tu_px,
                            tu_py,
                            4,
                            DC_PRED,
                            0,
                            tu_reach,
                            &residual,
                            None,
                            None,
                            smooth_neighbor,
                            fctx,
                        );
                    } else {
                        push_intra_rect(
                            y,
                            u,
                            v,
                            0,
                            tu_px,
                            tu_py,
                            tw,
                            th,
                            DC_PRED,
                            0,
                            tu_reach,
                            &residual,
                            None,
                            None,
                            smooth_neighbor,
                            fctx,
                        );
                    }
                    levels
                };
                if single {
                    grid = tu_grid;
                } else {
                    // Per-unit neighbour state, the same shape the intra rect
                    // leaf's own split path runs (`decode_leaf8`'s depth-1
                    // loop): a transform unit inside a block is not a block at
                    // that position.
                    neighbours.record_mi_luma(tu_mi, 4, &tu_grid);
                }
            }
            // The deferred non-var-tx band publish (see `publish` above): the
            // units' `txb_skip_ctx` reads ran against the pre-block bands.
            if let Some((pub_w, pub_h)) = publish {
                txfm_partition_update_rect(neighbours, lmi, (pub_w, pub_h), (bw, bh));
            }
            if single {
                let state = neighbour_state(&grid);
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
            neighbours.fill_skip_grid_rect(lmi, w_mi, h_mi, skip);
            neighbours.fill_lf_grid_rect(
                lmi,
                w_mi,
                h_mi,
                if single { bw as u8 } else { 4 },
                if single { bh as u8 } else { 4 },
                0,
                fctx,
            );
            // `record_intrabc_mi` publishes a square `n4`x`n4` footprint; a
            // 4x8/8x4 leaf needs its true `w_mi`x`h_mi` (lane-av1txr).
            record_intrabc_mi_rect(lmi.0, lmi.1, w_mi, h_mi, Some(dv), neighbours, fctx);
            if has_chroma {
                // See [`decode_leaf_split4`]: an intrabc block's chroma
                // inherits the luma transform type.
                fctx.intrabc_chroma_tx.with(|c| c.set(Some(chroma_tx)));
                if chroma_444 {
                    // lane-av1-444: this leaf codes its OWN rect chroma unit
                    // right here (frame copy at the DV); no group tail.
                    sub8_leaf_chroma444(
                        dec,
                        cdfs,
                        neighbours,
                        lmi,
                        bw,
                        bh,
                        DC_PRED,
                        None,
                        skip,
                        Some(dv),
                        y,
                        u,
                        v,
                        scan4,
                        base_q_idx,
                        neighbours.smooth_uv_neighbour(lmi.0, lmi.1, r, c, fctx),
                        fctx,
                    )?;
                    last_uv444 = DC_PRED;
                } else if chroma_422 {
                    // lane-av1-422h: same at ss (1,0) -- the strip's own square
                    // TX_4X4 chroma unit at its halved position.
                    sub8_leaf_chroma422(
                        dec,
                        cdfs,
                        neighbours,
                        lmi,
                        DC_PRED,
                        0,
                        None,
                        skip,
                        Some(dv),
                        y,
                        u,
                        v,
                        scan4,
                        base_q_idx,
                        neighbours.smooth_uv_neighbour(lmi.0, lmi.1, r, c, fctx),
                        (bw, bh),
                        fctx,
                    )?;
                    last_uv422 = DC_PRED;
                } else {
                    last_uv = Some((DC_PRED, 0, None));
                    last_intrabc = Some(dv);
                }
            }
            continue;
        }
        if filter_intra.is_some() {
            // lane-fi8 r1: `predict_filter_intra` walks its 4x2 patches over
            // a true `bw`x`bh` block, so a 4x8/8x4 leaf predicts through the
            // same `reconstruct_rect`/`reconstruct` calls below -- and under
            // a split transform, per 4x4 unit with that unit's own edge,
            // which is exactly what the depth-1 loop already does.
            hit!(FILTER_INTRA_RECT_HITS);
            hit!(FILTER_INTRA_RECT_SUB8_HITS);
        }
        // `TxMode::Select`'s `tx_depth` (spec 5.11.16) exists at a 4x8/8x4
        // leaf too, and is read for every intra block, skipped or not. Its
        // category is libaom's `bsize_to_tx_size_cat(BLOCK_4X8) == 0` --
        // `sub_tx_size_map[TX_4X8] == TX_4X4` is a single step, so it is the
        // same 2-symbol `tx_size_cat0` an 8x8 uses, not the 3-symbol
        // `tx_size_cat2` the 32x16 strips read -- with the rect context
        // (`get_tx_size_context`'s `max_tx_wide`/`max_tx_high` = bw/bh).
        // Missing this read consumed one symbol fewer than the encoder wrote
        // and desynced the tile at the very first rect leaf (class
        // `symbol-consumption-gap`).
        // lane-av1mix: libaom `read_tx_size`'s FIRST line
        // (`decodeframe.c:1203`, `if (xd->lossless[...]) return TX_4X4`)
        // fires before the `TX_MODE_SELECT` test, so a lossless-segment leaf
        // codes NO depth symbol even in `TxMode::Select` -- the mixed-lossless
        // frame's sub8 leaves forked exactly here (ours read the symbol at
        // (12477,50864) bit 3011, the oracle's all_zero used it).
        let depth = if tx_select && !lossless(fctx) {
            // lane-av1txbands: the `TXFM_CONTEXT` band read every other rect
            // reader already does.
            let ctx = tx_size_context_txfm_rect(neighbours, lmi, bw, bh, fctx);
            dec.symbol(&mut cdfs.tx_size_cat0[ctx])
        } else {
            0
        };
        if depth != 0 {
            hit!(TX_DEPTH_HITS);
            hit!(RECT8_SPLIT_TX_HITS);
        }
        if smooth_neighbor {
            hit!(SMOOTH_LUMA_HITS);
        }
        let (px, py) = (lmi.1 * MI, lmi.0 * MI);
        let reach = Reach::of_rect(bw, bh, px, py, y.width, y.height, fctx);
        // `sub_tx_size_map[TX_4X8] == TX_4X4`: depth 1 splits the leaf into
        // two 4x4 transform units along its long axis, each predicted from
        // the previous unit's reconstruction and reading its own `txb_skip`
        // context -- `decode_leaf8`'s own depth-1 loop, two units instead of
        // four. The per-TU path writes its own neighbour state, so the
        // block-level write below is skipped for it.
        let split = depth != 0 || lossless(fctx);
        // lane-av1txr: libaom `parse_decode_block`'s `set_txfm_ctxs` for this
        // leaf -- the size the `tx_depth` symbol resolved (depth 1 -> TX_4X4,
        // else the leaf's own rect transform), published over the leaf's own
        // mi span. An intrabc block's `txfm_partition_context` reads these
        // bands, and the key-frame sub-8x8 leaves had no write at all:
        // `warped.obu` mi (37,62) read a stale 8 where aomdec read 4 (ctx 18
        // vs 19) and the tile desynced (class `context-band-not-published`).
        let (ctx_tx_w, ctx_tx_h) = if split { (4, 4) } else { (bw, bh) };
        txfm_partition_update_rect(neighbours, lmi, (ctx_tx_w, ctx_tx_h), (bw, bh));
        if split {
            let mut done = 0usize;
            for tu in 0..2usize {
                let tu_mi = if vert {
                    (lmi.0 + tu, lmi.1)
                } else {
                    (lmi.0, lmi.1 + tu)
                };
                let (tu_px, tu_py) = (tu_mi.1 * MI, tu_mi.0 * MI);
                let tu_around = neighbours.around_mi(tu_mi, 4)[0];
                // Same per-TU rule as every other split-transform site
                // (lane-palette2 r12): a transform unit inside a block is not
                // a block at that position, so its reach comes from the
                // block's own row, offset by the unit's position.
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
                let tu_skip_ctx = neighbours.luma_skip_ctx(tu_mi, 1);
                if skip {
                    // A skipped leaf still predicts per transform unit: the
                    // second 4x4 takes its above/left edge from the first
                    // one's reconstruction, which is NOT what a single 4x8
                    // prediction of the same mode produces.
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
                if tu_grid.iter().any(|&l| l != 0) {
                    done += 1;
                }
                neighbours.record_mi_luma(tu_mi, 4, &tu_grid);
            }
            if done > 0 {
                if vert {
                    hit!(TX4X8_CODED_HITS);
                } else {
                    hit!(TX8X4_CODED_HITS);
                }
            }
            neighbours.fill_skip_grid_rect(lmi, w_mi, h_mi, skip);
            neighbours.fill_lf_grid_rect(lmi, w_mi, h_mi, 4, 4, 0, fctx);
            if has_chroma {
                if chroma_444 {
                    // lane-av1-444: the leaf's own rect chroma unit after its
                    // split luma (no shared group unit at ss 0/0).
                    let (uv_mode, _, alpha) = chroma.expect("4:4:4 reads uv_mode on every leaf");
                    sub8_leaf_chroma444(
                        dec,
                        cdfs,
                        neighbours,
                        lmi,
                        bw,
                        bh,
                        uv_mode,
                        alpha,
                        skip,
                        None,
                        y,
                        u,
                        v,
                        scan4,
                        base_q_idx,
                        neighbours.smooth_uv_neighbour(lmi.0, lmi.1, r, c, fctx),
                        fctx,
                    )?;
                    last_uv444 = if uv_mode == UV_CFL_PRED {
                        DC_PRED
                    } else {
                        uv_mode
                    };
                } else if chroma_422 {
                    // lane-av1-422h: the strip's own square TX_4X4 chroma unit
                    // per plane right after its split luma (one per ref strip).
                    let (uv_mode, angle_delta_uv, alpha) =
                        chroma.expect("4:2:2 chroma references read uv_mode");
                    sub8_leaf_chroma422(
                        dec,
                        cdfs,
                        neighbours,
                        lmi,
                        uv_mode,
                        angle_delta_uv,
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
                    last_uv422 = if uv_mode == UV_CFL_PRED {
                        DC_PRED
                    } else {
                        uv_mode
                    };
                } else {
                    last_uv = chroma;
                }
            }
            continue;
        }
        let grid = if skip {
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
            // `plane_bsize == txsize_to_bsize[tx_size]` here too (the leaf's
            // luma TU *is* the leaf), so `get_txb_ctx` fixes `txb_skip_ctx`
            // at 0 -- see `decode_leaf_split4`'s own note.
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
            if levels.iter().any(|&l| l != 0) {
                if vert {
                    hit!(TX4X8_CODED_HITS);
                } else {
                    hit!(TX8X4_CODED_HITS);
                }
            }
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
        // Plane 0's per-TU coefficient context over this leaf's own mi span
        // (`record_mi_luma`'s rect twin -- the chroma planes are written once
        // for the whole 8x8 group below).
        let state = neighbour_state(&grid);
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
        neighbours.fill_skip_grid_rect(lmi, w_mi, h_mi, skip);
        let (tx_w, tx_h) = if split { (4, 4) } else { (bw as u8, bh as u8) };
        neighbours.fill_lf_grid_rect(lmi, w_mi, h_mi, tx_w, tx_h, 0, fctx);
        if has_chroma {
            if chroma_444 {
                // lane-av1-444: the leaf's own rect chroma unit after its
                // luma (no shared group unit at ss 0/0).
                let (uv_mode, _, alpha) = chroma.expect("4:4:4 reads uv_mode on every leaf");
                sub8_leaf_chroma444(
                    dec,
                    cdfs,
                    neighbours,
                    lmi,
                    bw,
                    bh,
                    uv_mode,
                    alpha,
                    skip,
                    None,
                    y,
                    u,
                    v,
                    scan4,
                    base_q_idx,
                    neighbours.smooth_uv_neighbour(lmi.0, lmi.1, r, c, fctx),
                    fctx,
                )?;
                last_uv444 = if uv_mode == UV_CFL_PRED {
                    DC_PRED
                } else {
                    uv_mode
                };
            } else if chroma_422 {
                // lane-av1-422h: the strip's own square TX_4X4 chroma unit
                // per plane right after its luma -- libaom codes each
                // BLOCK_8X4 strip's chroma under the strip itself (both
                // strips of a HORZ pair are chroma references), not one
                // shared group unit.
                let (uv_mode, angle_delta_uv, alpha) =
                    chroma.expect("4:2:2 chroma references read uv_mode");
                sub8_leaf_chroma422(
                    dec,
                    cdfs,
                    neighbours,
                    lmi,
                    uv_mode,
                    angle_delta_uv,
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
                last_uv422 = if uv_mode == UV_CFL_PRED {
                    DC_PRED
                } else {
                    uv_mode
                };
            } else {
                last_uv = chroma;
            }
        }
    }
    // lane-av1mono: a monochrome frame codes no chroma at all -- no
    // `uv_mode` was read ([`read_intra_mode_sub8`] folds the plane count in),
    // so there is no chroma to reconstruct and no chroma coefficient to read.
    let (u_grid, v_grid): (Grid, Grid) = if mono(fctx) {
        (Grid::Zero(16), Grid::Zero(16))
    } else if chroma_444 {
        // lane-av1-444: every leaf coded and reconstructed its own chroma in
        // the loop above; only the coarse slots remain (last leaf's mode).
        neighbours.above_uv_mode[c] = last_uv444;
        neighbours.left_uv_mode[r] = last_uv444;
        (Grid::Zero(16), Grid::Zero(16))
    } else if chroma_422 {
        // lane-av1-422h: every chroma-reference strip coded and reconstructed
        // its own chroma unit in the loop above (and stamped its own entropy
        // span), so the 4:2:0 shared group unit below must not run. Only the
        // coarse 16x16 slots remain: libaom's `above_uv_mode`/`left_uv_mode`
        // end up holding the LAST chroma reference's mode (decode order).
        neighbours.above_uv_mode[c] = last_uv422;
        neighbours.left_uv_mode[r] = last_uv422;
        (Grid::Zero(16), Grid::Zero(16))
    } else {
        let (gpx, gpy) = (leaf_mi.1 * MI, leaf_mi.0 * MI);
        let (cpx, cpy) = (gpx >> ss_x(fctx), gpy >> ss_y(fctx));
        let group_reach = Reach::of(8, gpx, gpy, y.width, y.height, fctx);
        let (uv_mode, angle_delta_uv, alpha) =
            last_uv.expect("i==1 always sets has_chroma, so chroma is always Some");
        let uv_predict_mode = if uv_mode == UV_CFL_PRED {
            DC_PRED
        } else {
            uv_mode
        };
        neighbours.above_uv_mode[c] = uv_predict_mode;
        neighbours.left_uv_mode[r] = uv_predict_mode;
        neighbours.record_uv_mode_mi(leaf_mi.0, leaf_mi.1, 2, 2, uv_predict_mode);
        let (u_grid, v_grid): (Grid, Grid) = if leaf_skips[1] && last_intrabc.is_none() {
            // A skipped chroma block still gets its CfL contribution: libaom's
            // `predict_and_reconstruct_intra_block` calls `cfl_predict_block`
            // from `av1_predict_intra_block_facade` (av1/common/reconintra.c:1719)
            // whenever `mbmi->uv_mode == UV_CFL_PRED`, before any residual is
            // added -- `skip_txfm` only drops the residual, never the alpha.
            // lane-reelckf and lane-kf1200 fixed THIS SAME arm independently; the
            // merge keeps one fix and feeds both lanes' counters so neither of
            // their gates goes vacuous.
            let ac = alpha.map(|_| cfl_src(gpx, gpy, 8));
            if alpha.is_some() {
                hit!(CHROMA_SKIP_CFL_HITS);
                hit!(SKIP_CFL_HITS);
            }
            push_intra(
                y,
                u,
                v,
                1,
                cpx,
                cpy,
                4,
                uv_predict_mode,
                angle_delta_uv,
                group_reach,
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
                cpx,
                cpy,
                4,
                uv_predict_mode,
                angle_delta_uv,
                group_reach,
                &ZERO_RESIDUAL[..16],
                alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
                None,
                smooth_neighbor_uv,
                fctx,
            );
            // lane-av1txbands: a SKIPPED chroma-reference leaf reads no chroma
            // coefficient, so it never consults the inherited luma transform type
            // -- but an intrabc chroma-reference leaf did arm
            // [`INTRABC_CHROMA_TX`] above (`has_chroma`), and the two coefficient
            // branches below are the only ones that disarm it. Left armed, the slot
            // picks the NEXT block's chroma scan and inverse transform (class
            // arm-without-clear-on-a-skip-route). Counted so a gate can tell a
            // stream that reaches this route from one that never does.
            if fctx.intrabc_chroma_tx.with(std::cell::Cell::get).is_some() {
                hit!(SKIPPED_INTRABC_CHROMA_ARM_HITS);
                fctx.intrabc_chroma_tx.with(|c| c.set(None));
            }
            (Grid::Zero(16), Grid::Zero(16))
        } else if let Some(dv) = last_intrabc {
            // A 4:2:0 `BLOCK_4X8`/`BLOCK_8X4` leaf maps to a clamped chroma
            // `BLOCK_4X4` (`get_plane_block_size`, blockd.h:1184; `set_plane_n4`,
            // av1_common_int.h:1344). The chroma reference is still the whole
            // 8x8 group's 4x4 plane block at `(cpx, cpy)`, so prediction uses the
            // group's destination origin, not the odd-mi leaf's nominal 2x4/4x2
            // extent. The old route treated a skipped intrabc leaf as skipped
            // intra and predicted this block with DC, first differing at
            // mi(46,52), U(104,92), while consuming identical symbols.
            // `decode_reconstruct_tx` reads no chroma coefficients for a skipped
            // intrabc block, but an unskipped one reads the ordinary Chroma4
            // tables; chroma inherits no coded tx_type of its own.
            let mut ub = vec![0u16; 16];
            mc::predict_with_filter(
                &u.data,
                u.width,
                u.true_width,
                u.true_height,
                mv_to_q4(cpx, dv.1, ss_x(fctx)),
                mv_to_q4(cpy, dv.0, ss_y(fctx)),
                4,
                4,
                mc::InterpFilterKind::Bilinear,
                &mut ub,
                fctx,
            );
            let mut vb = vec![0u16; 16];
            mc::predict_with_filter(
                &v.data,
                v.width,
                v.true_width,
                v.true_height,
                mv_to_q4(cpx, dv.1, ss_x(fctx)),
                mv_to_q4(cpy, dv.0, ss_y(fctx)),
                4,
                4,
                mc::InterpFilterKind::Bilinear,
                &mut vb,
                fctx,
            );
            if leaf_skips[1] {
                // `skip_txfm` still reconstructs an intrabc prediction, but
                // libaom's `decode_reconstruct_tx` reads NO chroma coefficients.
                // The ordinary skipped-intra arm above is therefore not valid for
                // an intrabc leaf: use the copied, clamped 4x4 prediction directly.
                push_mc_rect(
                    y,
                    u,
                    v,
                    1,
                    cpx,
                    cpy,
                    4,
                    4,
                    4,
                    Pred::Inline(&ub, 4),
                    &ZERO_RESIDUAL[..16],
                    fctx,
                );
                push_mc_rect(
                    y,
                    u,
                    v,
                    2,
                    cpx,
                    cpy,
                    4,
                    4,
                    4,
                    Pred::Inline(&vb, 4),
                    &ZERO_RESIDUAL[..16],
                    fctx,
                );
                hit!(SKIPPED_INTRABC_CHROMA_ARM_HITS);
                fctx.intrabc_chroma_tx.with(|c| c.set(None));
                (Grid::Zero(16), Grid::Zero(16))
            } else {
                let chroma_around = neighbours.around_mi(leaf_mi, 8);
                set_palette_pred(ub, fctx);
                let ug = read_plane(
                    dec,
                    cdfs,
                    TxbSet::Chroma4,
                    scan4,
                    1,
                    chroma_around[1],
                    DC_PRED,
                    DC_PRED,
                    0,
                    group_reach,
                    y,
                    u,
                    v,
                    cpx,
                    cpy,
                    4,
                    TX4,
                    base_q_idx,
                    None,
                    None,
                    None,
                    smooth_neighbor_uv,
                    fctx,
                )?;
                set_palette_pred(vb, fctx);
                let vg = read_plane(
                    dec,
                    cdfs,
                    TxbSet::Chroma4,
                    scan4,
                    2,
                    chroma_around[2],
                    DC_PRED,
                    DC_PRED,
                    0,
                    group_reach,
                    y,
                    u,
                    v,
                    cpx,
                    cpy,
                    4,
                    TX4,
                    base_q_idx,
                    None,
                    None,
                    None,
                    smooth_neighbor_uv,
                    fctx,
                )?;
                fctx.intrabc_chroma_tx.with(|c| c.set(None));
                (ug, vg)
            }
        } else {
            let ac = alpha.map(|_| cfl_src(gpx, gpy, 8));
            let chroma_around = neighbours.around_mi(leaf_mi, 8);
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
                group_reach,
                y,
                u,
                v,
                cpx,
                cpy,
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
                group_reach,
                y,
                u,
                v,
                cpx,
                cpy,
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
        (u_grid, v_grid)
    };
    // `update_partition_context` at the 8x8 level writes
    // `partition_context_lookup[subsize]` over the whole 8x8 span: BLOCK_4X8
    // is `{31, 30}` (bit0 set on the above half only), BLOCK_8X4 `{30, 31}`
    // -- the mirror of `decode_leaf_split4`'s `{31, 31}`, which
    // `partition_ctx_mi` reads back off these side values.
    for cell in 0..2 {
        neighbours.above_side_mi[leaf_mi.1 + cell] = bw as u8;
        neighbours.left_side_mi[leaf_mi.0 + cell] = bh as u8;
    }
    // lane-av1-444: at 4:4:4 [`sub8_leaf_chroma444`] already stamped each
    // leaf's own span as it decoded; these placeholder grids carry no state.
    // lane-av1-422h: same at ss (1,0) -- [`sub8_leaf_chroma422`] stamped each
    // chroma-reference strip's own span in decode order.
    if !(chroma_444 || chroma_422) {
        let round_up_even = |n: usize| n.div_ceil(2) * 2;
        let bound_h = round_up_even(neighbours.mi_rows);
        let bound_w = round_up_even(neighbours.mi_cols);
        let u_state = neighbour_state(&u_grid);
        let v_state = neighbour_state(&v_grid);
        for cell in 0..2usize {
            if cell < 2usize.min(bound_h.saturating_sub(leaf_mi.0)) {
                neighbours.left[leaf_mi.0 + cell][1] = u_state;
                neighbours.left[leaf_mi.0 + cell][2] = v_state;
            }
            if cell < 2usize.min(bound_w.saturating_sub(leaf_mi.1)) {
                neighbours.above[leaf_mi.1 + cell][1] = u_state;
                neighbours.above[leaf_mi.1 + cell][2] = v_state;
            }
        }
    }
    // lane-dkey (same clear as `decode_leaf_split4`): a sub-8x8 leaf can never use palette (`av1_allow_palette`
    // needs BLOCK_8X8 and up), and libaom's mi grid says so -- its
    // `palette_size` is 0 in every one of these mi cells, so the NEXT block's
    // `av1_get_palette_mode_ctx` and colour cache read 0 here. This decoder
    // only ever WROTE palette state from the >=8x8 readers, so an 8x8 palette
    // block above a 4x4 group left its size/colours standing in the mi-granular
    // bands: the block below the group read `palette_y_mode` off CDF row 1
    // where libaom reads row 0 (class [[cdf-row-held-constant]]), decoded a
    // palette libaom never coded, and then skipped the `use_filter_intra`
    // symbol that a palette block has none of -- one symbol short, tile
    // desynced. Publishing the group's own zeros is the clear.
    neighbours.record_palette_y_rect(leaf_mi, 8, 8, 0, [0u16; 8]);
    neighbours.record_palette_uv_rect(leaf_mi, 8, 8, 0, [0u16; 8]);
    // `(below_mode, right_mode)`: the sub-block a neighbour below sees is the
    // bottom one (the second under HORZ, the left one under VERT), the one a
    // neighbour to the right sees is the top-right (the second under VERT).
    Ok(if vert {
        (leaf_modes[0], leaf_modes[1])
    } else {
        (leaf_modes[1], leaf_modes[0])
    })
}

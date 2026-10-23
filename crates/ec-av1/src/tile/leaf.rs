use super::*;

/// Writes one 16x16 leaf a straddling 32x32 quadrant's [`Quadrant::Split`]
/// splits into: intra (spec `inter_frame_mode_info`'s intra branch, the only
/// one this writer used to code here) or real inter (`is_inter` coded
/// `true`), `NEARESTMV` only -- `NEWMV` at this size is this function's
/// caller's to never build (`Quadrant::Split`'s `BlockCoeffs.inter`) -- same
/// `single_ref`/mv-stack/DRL symbol chain as the whole-32x32 branch above,
/// just at this leaf's own 4x4-mi window (`bw4`/`bh4` of 4, not 8) and coded
/// through [`TxbSet::Luma16Inter`] rather than [`TxbSet::Luma32Inter`]. Its
/// own `PARTITION_NONE` symbol is already written, same contract as
/// [`write_block`].
#[allow(clippy::too_many_arguments)]
pub(super) fn write_inter_frame_leaf(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    grid: &mut MiGrid,
    mi_cols: u32,
    mi_rows: u32,
    block: &BlockCoeffs,
    at: (usize, usize),
    scan16: &Vec<u16>,
    scan8: &Vec<u16>,
    tx_select: bool,
    all_scans: &[Vec<u16>; 4],
) -> Result<()> {
    if block.inter.is_none() && usize::from(block.mode) >= INTRA_MODES {
        return Err(Error::unsupported(
            "AV1 tile",
            format!(
                "intra mode {} is not one of the thirteen this writer codes",
                block.mode
            ),
        ));
    }
    /// `Y_MODE`'s size group (spec `Size_Group`, `common_data.h`'s
    /// `size_group_lookup[BLOCK_16X16]`) for this leaf's own size.
    const SIZE_GROUP_16: usize = 2;

    let (r, c) = at;
    let (mi_r, mi_c) = (r * (SUB / MI), c * (SUB / MI));
    let skip_ctx =
        usize::from(neighbours.above_skip[mi_c]) + usize::from(neighbours.left_skip[mi_r]);
    enc.symbol(usize::from(block.skip), &mut cdfs.skip[skip_ctx]);
    write_cdef_idx(enc, (mi_r, mi_c), block.skip);
    write_delta_q(enc, cdfs, (mi_r, mi_c), false, block.skip);

    let (has_above, has_left) = (neighbours.has_above(mi_r), neighbours.has_left(mi_c));
    let (above_inter, left_inter) = (neighbours.above_inter[mi_c], neighbours.left_inter[mi_r]);
    let ii_ctx = intra_inter_ctx(has_above, has_left, above_inter, left_inter);
    let is_inter = block.inter.is_some();
    enc.symbol(usize::from(is_inter), &mut cdfs.intra_inter[ii_ctx]);

    let mode_for_tx;
    if let Some(info) = block.inter {
        write_comp_mode(
            enc,
            cdfs,
            neighbours,
            (mi_r, mi_c),
            (has_above, has_left),
            info.ref1.is_some(),
        );
        let (mi_row, mi_col) = (r * SUB_MI as usize, c * SUB_MI as usize);
        if info.ref1.is_some() {
            let n = SUB_MI as usize;
            let (mv, mv1, is_new_mv) = write_compound_block(
                enc,
                cdfs,
                neighbours,
                grid,
                (mi_row, mi_col),
                (n, n),
                (has_above, has_left),
                info,
                (mi_cols as usize, mi_rows as usize),
            )?;
            for dr in 0..n {
                for dc in 0..n {
                    grid.set(
                        mi_row + dr,
                        mi_col + dc,
                        MiInfo {
                            is_inter: true,
                            ref_frame: info.ref_frame,
                            ref_frame1: info.ref1.unwrap_or(NO_REF1),
                            mv1: mv16(mv1),
                            mv: mv16(mv),
                            is_new_mv,
                            size: n as u8,
                            size_h: n as u8,
                            is_global_mv0: false,
                            is_global_mv1: false,
                        },
                    );
                }
            }
            mode_for_tx = 0;
        } else {
            write_single_ref(
                enc,
                cdfs,
                info.ref_frame,
                neighbours.above_ref[mi_c],
                neighbours.above_ref1[mi_c],
                neighbours.left_ref[mi_r],
                neighbours.left_ref1[mi_r],
            );

            let stack = find_mv_stack(
                grid,
                mi_row,
                mi_col,
                SUB_MI as usize,
                SUB_MI as usize,
                info.ref_frame,
                mi_cols as usize,
                mi_rows as usize,
            );

            let (mv, is_new_mv) = write_inter_mode(enc, cdfs, info, &stack)?;
            for dr in 0..SUB_MI as usize {
                for dc in 0..SUB_MI as usize {
                    grid.set(
                        mi_row + dr,
                        mi_col + dc,
                        MiInfo {
                            is_inter: true,
                            ref_frame: info.ref_frame,
                            ref_frame1: NO_REF1,
                            mv1: (0, 0),
                            mv: mv16(mv),
                            is_new_mv,
                            size: SUB_MI as usize as u8,
                            size_h: SUB_MI as usize as u8,
                            is_global_mv0: false,
                            is_global_mv1: false,
                        },
                    );
                }
            }
            write_motion_mode(
                enc,
                cdfs,
                grid,
                (mi_row, mi_col),
                (SUB_MI as usize, SUB_MI as usize),
                (SUB, SUB),
                (mi_cols as usize, mi_rows as usize),
                info.ref_frame,
                block.motion_mode,
            )?;
            mode_for_tx = 0;
        }
    } else {
        let mode = usize::from(block.mode);
        enc.symbol(mode, &mut cdfs.y_mode[SIZE_GROUP_16]);
        if (V_PRED..=D67_PRED).contains(&mode) {
            enc.symbol(
                (ANGLE_DELTA_ZERO as i32 + i32::from(block.angle_delta_y)) as usize,
                &mut cdfs.angle_delta[mode - V_PRED],
            );
        }
        let uv_mode = usize::from(block.uv_mode);
        enc.symbol(uv_mode, &mut cdfs.uv_mode_cfl[mode]);
        if uv_mode == UV_CFL_PRED {
            write_cfl_alphas(enc, cdfs, block.cfl_alphas.expect("cfl"));
        }
        if (V_PRED..=D67_PRED).contains(&uv_mode) {
            enc.symbol(ANGLE_DELTA_ZERO, &mut cdfs.angle_delta[uv_mode - V_PRED]);
        }
        mode_for_tx = tx_row(mode, block.filter_intra);
        // Intra: no vote, but still a coded cell -- mvstack's extended-scan
        // coverage must see it (module doc).
        let (mi_row, mi_col) = (r * SUB_MI as usize, c * SUB_MI as usize);
        write_palette_syntax(
            enc,
            cdfs,
            neighbours,
            block.palette.as_ref(),
            block.palette_uv.as_ref(),
            block.filter_intra,
            mode,
            uv_mode,
            (mi_row, mi_col),
            SUB,
        );
        for dr in 0..SUB_MI as usize {
            for dc in 0..SUB_MI as usize {
                grid.set(
                    mi_row + dr,
                    mi_col + dc,
                    MiInfo {
                        is_inter: false,
                        ref_frame: -1,
                        ref_frame1: NO_REF1,
                        mv1: (0, 0),
                        mv: (0, 0),
                        is_new_mv: false,
                        size: SUB_MI as usize as u8,
                        size_h: SUB_MI as usize as u8,
                        is_global_mv0: false,
                        is_global_mv1: false,
                    },
                );
            }
        }
    }

    let at_mi = (at.0 * (SUB / MI), at.1 * (SUB / MI));
    let tx = if tx_select {
        write_tx_syntax_inter(
            enc,
            cdfs,
            neighbours,
            at_mi,
            SUB,
            is_inter,
            block.skip,
            usize::from(block.tx_depth),
        )
    } else {
        SUB
    };
    let split = tx < SUB;
    if block.skip {
        let zero_grids = [
            vec![0i32; TX16 * TX16],
            vec![0i32; TX8 * TX8],
            vec![0i32; TX8 * TX8],
        ];
        neighbours.record(at, SUB, mode_for_tx, &zero_grids);
    } else {
        let grids = [
            level_grid(&block.luma, TX16)?,
            level_grid(&block.u, TX8)?,
            level_grid(&block.v, TX8)?,
        ];
        if split {
            write_luma_tus(
                enc,
                cdfs,
                neighbours,
                at_mi,
                SUB,
                tx,
                &grids[0],
                mode_for_tx,
                all_scans,
                is_inter,
                &block.luma_tx_types,
            )?;
        }
        write_block_planes(
            enc,
            cdfs,
            if is_inter {
                &[TxbSet::Luma16Inter, TxbSet::Chroma8, TxbSet::Chroma8]
            } else {
                &[TxbSet::Luma16, TxbSet::Chroma8, TxbSet::Chroma8]
            },
            &grids,
            &[scan16, scan8, scan8],
            &neighbours.around(at, SUB),
            mode_for_tx,
            split,
            block_tx_type(block),
            is_inter,
        );
        neighbours.record_planes(at, SUB, mode_for_tx, &grids, !split);
    }
    neighbours.record_inter(at, SUB, block.skip, is_inter, block_ref(block));
    record_block_compound(neighbours, (mi_r, mi_c), SUB, block);
    Ok(())
}

/// Writes one 8x8 leaf of a straddling 16x16 inter-frame block
/// (lane-av1inter8): its own skip flag, intra/inter choice and (when inter)
/// `NEARESTMV`/`NEWMV` chain, or (when intra) `Y_MODE`, then its own luma and
/// two chroma transform blocks -- coded exactly like
/// [`write_inter_frame_leaf`] but through [`TxbSet::Luma8Inter`]/
/// [`TxbSet::Chroma4`] and at this leaf's own 2x2-mi mv-stack window, reading
/// its skip/intra-inter context from the *enclosing* 16x16 slot's
/// [`Neighbours`] arrays (`outer_at`, in [`SUB`]-grid units) unless
/// `prev_leaf` names this straddling block's own first leaf as the true
/// mi-adjacent neighbour -- the same override [`write_leaf8`] applies to its
/// mode context, needed for the same reason: `Neighbours`' above/left arrays
/// only resolve to [`SUB`] granularity, too coarse for the second leaf of a
/// straddling 16x16 to see the first. `Y_MODE`'s own context (`Size_Group`)
/// is not neighbour-dependent, unlike the key frame path's `kf_y_mode`, so no
/// override is needed there. Hands back this leaf's own skip flag and
/// intra/inter choice, which is what the next leaf (or the caller's final
/// write-back, mirroring `write_leaf8`'s caller) reads.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_inter_frame_leaf8(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    grid: &mut MiGrid,
    mi_cols: u32,
    mi_rows: u32,
    block: &BlockCoeffs,
    leaf_mi: (usize, usize),
    scan8: &Vec<u16>,
    scan4: &Vec<u16>,
    tx_select: bool,
    all_scans: &[Vec<u16>; 4],
) -> Result<(bool, bool)> {
    if block.inter.is_none() && usize::from(block.mode) >= INTRA_MODES {
        return Err(Error::unsupported(
            "AV1 tile",
            format!(
                "intra mode {} is not one of the thirteen this writer codes",
                block.mode
            ),
        ));
    }
    /// `Y_MODE`'s size group (spec `Size_Group`, `common_data.h`'s
    /// `size_group_lookup[BLOCK_8X8]`) for this leaf's own size.
    const SIZE_GROUP_8: usize = 1;

    // This leaf's own 4x4 mode-info cells, not the enclosing 16x16 slot's:
    // under a real PARTITION_SPLIT all four leaves are coded, so the
    // bottom-left leaf's true above neighbour is the FIRST leaf and the
    // bottom-right's above and left are two DIFFERENT ones (lane-av1rd2 --
    // reading the coarse slot desynced the stream, and so did an override
    // that remembered only the previous leaf).
    let above_skip = neighbours.above_skip[leaf_mi.1];
    let left_skip = neighbours.left_skip[leaf_mi.0];
    let above_inter = neighbours.above_inter[leaf_mi.1];
    let left_inter = neighbours.left_inter[leaf_mi.0];
    let skip_ctx = usize::from(above_skip) + usize::from(left_skip);
    enc.symbol(usize::from(block.skip), &mut cdfs.skip[skip_ctx]);
    write_cdef_idx(enc, leaf_mi, block.skip);
    write_delta_q(enc, cdfs, leaf_mi, false, block.skip);

    let (has_above, has_left) = (
        neighbours.has_above(leaf_mi.0),
        neighbours.has_left(leaf_mi.1),
    );
    let ii_ctx = intra_inter_ctx(has_above, has_left, above_inter, left_inter);
    let is_inter = block.inter.is_some();
    enc.symbol(usize::from(is_inter), &mut cdfs.intra_inter[ii_ctx]);

    let mode_for_tx;
    if let Some(info) = block.inter {
        write_comp_mode(
            enc,
            cdfs,
            neighbours,
            leaf_mi,
            (has_above, has_left),
            info.ref1.is_some(),
        );
        let (mi_row, mi_col) = leaf_mi;
        if info.ref1.is_some() {
            let (mv, mv1, is_new_mv) = write_compound_block(
                enc,
                cdfs,
                neighbours,
                grid,
                (mi_row, mi_col),
                (2, 2),
                (has_above, has_left),
                info,
                (mi_cols as usize, mi_rows as usize),
            )?;
            for dr in 0..2 {
                for dc in 0..2 {
                    grid.set(
                        mi_row + dr,
                        mi_col + dc,
                        MiInfo {
                            is_inter: true,
                            ref_frame: info.ref_frame,
                            ref_frame1: info.ref1.unwrap_or(NO_REF1),
                            mv1: mv16(mv1),
                            mv: mv16(mv),
                            is_new_mv,
                            size: 2,
                            size_h: 2,
                            is_global_mv0: false,
                            is_global_mv1: false,
                        },
                    );
                }
            }
            mode_for_tx = 0;
        } else {
            write_single_ref(
                enc,
                cdfs,
                info.ref_frame,
                neighbours.above_ref[leaf_mi.1],
                neighbours.above_ref1[leaf_mi.1],
                neighbours.left_ref[leaf_mi.0],
                neighbours.left_ref1[leaf_mi.0],
            );

            let stack = find_mv_stack(
                grid,
                mi_row,
                mi_col,
                2,
                2,
                info.ref_frame,
                mi_cols as usize,
                mi_rows as usize,
            );

            let (mv, is_new_mv) = write_inter_mode(enc, cdfs, info, &stack)?;
            for dr in 0..2 {
                for dc in 0..2 {
                    grid.set(
                        mi_row + dr,
                        mi_col + dc,
                        MiInfo {
                            is_inter: true,
                            ref_frame: info.ref_frame,
                            ref_frame1: NO_REF1,
                            mv1: (0, 0),
                            mv: mv16(mv),
                            is_new_mv,
                            size: 2,
                            size_h: 2,
                            is_global_mv0: false,
                            is_global_mv1: false,
                        },
                    );
                }
            }
            write_motion_mode(
                enc,
                cdfs,
                grid,
                (mi_row, mi_col),
                (2, 2),
                (8, 8),
                (mi_cols as usize, mi_rows as usize),
                info.ref_frame,
                block.motion_mode,
            )?;
            mode_for_tx = 0;
        }
    } else {
        let mode = usize::from(block.mode);
        enc.symbol(mode, &mut cdfs.y_mode[SIZE_GROUP_8]);
        if (V_PRED..=D67_PRED).contains(&mode) {
            enc.symbol(
                (ANGLE_DELTA_ZERO as i32 + i32::from(block.angle_delta_y)) as usize,
                &mut cdfs.angle_delta[mode - V_PRED],
            );
        }
        let uv_mode = usize::from(block.uv_mode);
        enc.symbol(uv_mode, &mut cdfs.uv_mode_cfl[mode]);
        if uv_mode == UV_CFL_PRED {
            write_cfl_alphas(enc, cdfs, block.cfl_alphas.expect("cfl"));
        }
        if (V_PRED..=D67_PRED).contains(&uv_mode) {
            enc.symbol(ANGLE_DELTA_ZERO, &mut cdfs.angle_delta[uv_mode - V_PRED]);
        }
        mode_for_tx = tx_row(mode, block.filter_intra);
        // Intra: no vote, but still a coded cell -- mvstack's extended-scan
        // coverage must see it (module doc).
        let (mi_row, mi_col) = leaf_mi;
        write_palette_syntax(
            enc,
            cdfs,
            neighbours,
            block.palette.as_ref(),
            block.palette_uv.as_ref(),
            block.filter_intra,
            mode,
            uv_mode,
            leaf_mi,
            8,
        );
        for dr in 0..2 {
            for dc in 0..2 {
                grid.set(
                    mi_row + dr,
                    mi_col + dc,
                    MiInfo {
                        is_inter: false,
                        ref_frame: -1,
                        ref_frame1: NO_REF1,
                        mv1: (0, 0),
                        mv: (0, 0),
                        is_new_mv: false,
                        size: 2,
                        size_h: 2,
                        is_global_mv0: false,
                        is_global_mv1: false,
                    },
                );
            }
        }
    }

    let planes = if is_inter {
        [TxbSet::Luma8Inter, TxbSet::Chroma4, TxbSet::Chroma4]
    } else {
        [TxbSet::Luma8, TxbSet::Chroma4, TxbSet::Chroma4]
    };
    let tx = if tx_select {
        write_tx_syntax_inter(
            enc,
            cdfs,
            neighbours,
            leaf_mi,
            8,
            is_inter,
            block.skip,
            usize::from(block.tx_depth),
        )
    } else {
        8
    };
    let split = tx < 8;
    if block.skip {
        let zero_grids = [
            vec![0i32; TX8 * TX8],
            vec![0i32; TX4 * TX4],
            vec![0i32; TX4 * TX4],
        ];
        neighbours.record_mi(leaf_mi, 8, &zero_grids);
        neighbours.record_inter_mi(
            leaf_mi,
            8,
            block.skip,
            block.inter.is_some(),
            block_ref(block),
        );
        record_block_compound(neighbours, leaf_mi, 8, block);
    } else {
        let grids = [
            level_grid(&block.luma, TX8)?,
            level_grid(&block.u, TX4)?,
            level_grid(&block.v, TX4)?,
        ];
        if split {
            write_luma_tus(
                enc,
                cdfs,
                neighbours,
                leaf_mi,
                8,
                tx,
                &grids[0],
                mode_for_tx,
                all_scans,
                is_inter,
                &block.luma_tx_types,
            )?;
        }
        write_block_planes(
            enc,
            cdfs,
            &planes,
            &grids,
            &[scan8, scan4, scan4],
            &neighbours.around_mi(leaf_mi, 8),
            mode_for_tx,
            split,
            block_tx_type(block),
            is_inter,
        );
        neighbours.record_mi_planes(leaf_mi, 8, &grids, !split);
        neighbours.record_inter_mi(leaf_mi, 8, block.skip, is_inter, block_ref(block));
        record_block_compound(neighbours, leaf_mi, 8, block);
    }
    Ok((block.skip, is_inter))
}


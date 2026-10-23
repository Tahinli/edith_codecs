use super::*;

/// The side of a chroma palette's shared colour-index map for a square luma
/// block of `side` samples: `av1_get_plane_block_size(bsize, 1, 1)` floored at
/// the 4-px transform, NOT a plain halving (decode.rs reads the map at
/// exactly this size).
pub(crate) fn palette_uv_side(side: usize) -> usize {
    (side / 2).max(4)
}

/// How many blocks took a palette, and of what size -- the fire count a gate
/// prints so that "palette is on" is a measurement rather than a claim
/// (class `gate-blind-to-feature`). Index 0 is the block count, 2..=8 the
/// per-size histogram.
pub(super) static PALETTE_HITS: [std::sync::atomic::AtomicUsize; 9] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 9];

/// lane-palw: colour-index maps written over the ON-SCREEN corner only,
/// i.e. palette blocks this writer cut at the frame's right/bottom mi edge.
/// A gate asserts it non-zero so it cannot pass on a stream that never cuts
/// one (class `gate-blind-to-feature`).
pub(super) static PALETTE_CUT_MAPS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Current value of [`PALETTE_CUT_MAPS`] (colour-index maps).
pub fn palette_cut_maps_written() -> usize {
    PALETTE_CUT_MAPS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Zeroes [`PALETTE_CUT_MAPS`] before a gate's own encode.
pub fn reset_palette_cut_maps_written() {
    PALETTE_CUT_MAPS.store(0, std::sync::atomic::Ordering::Relaxed);
}

pub(super) fn note_palette(n: usize) {
    PALETTE_HITS[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    PALETTE_HITS[n].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// [`PALETTE_HITS`] for the chroma palette -- the same shape, so a gate can
/// print the share of each plane's palette separately.
pub(super) static PALETTE_UV_HITS: [std::sync::atomic::AtomicUsize; 9] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 9];

pub(super) fn note_palette_uv(n: usize) {
    PALETTE_UV_HITS[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    PALETTE_UV_HITS[n].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// How many chroma base colours came out of the NEIGHBOUR CACHE rather than
/// the stream -- the half of `write_palette_colors_uv` a test cannot see from
/// the block counts alone, and the one that desyncs the tile if this writer's
/// cache ever disagrees with the decoder's (`gate-blind-to-feature`).
pub(super) static PALETTE_UV_CACHE_HITS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Reads [`PALETTE_UV_CACHE_HITS`] and zeroes it.
#[cfg(test)]
pub(crate) fn take_palette_uv_cache_hits() -> usize {
    PALETTE_UV_CACHE_HITS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// [`take_palette_hits`] for the chroma palette.
#[cfg(test)]
pub(crate) fn take_palette_uv_hits() -> [usize; 9] {
    std::array::from_fn(|i| PALETTE_UV_HITS[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// Reads [`PALETTE_HITS`] and zeroes it, so a gate can attribute the counts to
/// its own encode.
#[cfg(test)]
pub(crate) fn take_palette_hits() -> [usize; 9] {
    std::array::from_fn(|i| PALETTE_HITS[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// Writes one coded block: its mode, its three transform blocks, and what it
/// leaves behind for the blocks beside it. Its partition symbol is already
/// written, because only the caller knows what tree led here.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_block(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    block: &BlockCoeffs,
    at: (usize, usize),
    side: usize,
    planes: &[TxbSet; 3],
    grids: &[Vec<i32>; 3],
    scans: [&Vec<u16>; 3],
    cfl: bool,
    // The frame's `tx_mode == TxMode::Select`, and the block's own chosen
    // depth: with it the luma residual goes through [`write_luma_select`]
    // (one `tx_depth` symbol, then one transform unit per tile of the block)
    // instead of the single whole-block transform below.
    tx_select: bool,
    all_scans: &[Vec<u16>; 4],
) -> Result<()> {
    let (r, c) = at;
    let at_mi = (r * (SUB / MI), c * (SUB / MI));
    if let Some(dv) = block.dv {
        write_intrabc_block(enc, cdfs, neighbours, dv, at_mi, side)?;
        record_intrabc_mi(at_mi.0, at_mi.1, side / MI, Some(dv));
        return Ok(());
    }
    let (above_mode, left_mode) = (neighbours.above_mode[c], neighbours.left_mode[r]);
    let mode = write_intra_mode(
        enc, cdfs, neighbours, block, above_mode, left_mode, cfl, at_mi, side,
    );
    // The coefficient tables read `fimode_to_intradir[filter_intra_mode]`'s
    // row on a filter-intra block; the neighbour publication below still gets
    // the block's own `DC_PRED` ([`tx_row`]).
    let tx_mode = tx_row(mode, block.filter_intra);
    let split = if tx_select {
        let tx = write_luma_select(
            enc,
            cdfs,
            neighbours,
            at_mi,
            side,
            usize::from(block.tx_depth),
            &grids[0],
            tx_mode,
            all_scans,
            &block.luma_tx_types,
        )?;
        tx < side
    } else {
        false
    };
    write_block_planes(
        enc,
        cdfs,
        planes,
        grids,
        &scans,
        &neighbours.around(at, side),
        tx_mode,
        tx_select,
        block_tx_type(block),
        false,
    );
    neighbours.record_planes(at, side, mode, grids, !split);
    clear_skip_band(neighbours, at_mi, side);
    record_intrabc_mi(at_mi.0, at_mi.1, side / MI, None);
    Ok(())
}

/// An unskipped block's own `skip` publication: only an `allow_intrabc`
/// frame ever sets these bands, so a frame without one leaves them exactly
/// as they were before this lane (all false).
pub(super) fn clear_skip_band(neighbours: &mut Neighbours, mi: (usize, usize), side: usize) {
    if !intrabc_armed() {
        return;
    }
    for cell in 0..side / MI {
        neighbours.above_skip[mi.1 + cell] = false;
        neighbours.left_skip[mi.0 + cell] = false;
    }
}

/// Writes one 8x8 leaf of a straddling 16x16 block (lane-av1-rect): its own
/// luma transform and 4x4 chroma transforms, coded exactly like
/// [`write_block`] but reading its intra-mode context from the *enclosing*
/// 16x16 slot -- `outer_at`, in [`SUB`]-grid units -- rather than from its own
/// finer position, since [`Neighbours`]'s `above_mode`/`left_mode` arrays stay
/// at [`SUB`] (16-sample) granularity. `leaf_mi` is this leaf's own position
/// in 4x4 mode-info units, which is what its coefficient context (finer than
/// [`SUB`]) is kept and read at.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_leaf8(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    block: &BlockCoeffs,
    outer_at: (usize, usize),
    leaf_mi: (usize, usize),
    grids: &[Vec<i32>; 3],
    scans: [&Vec<u16>; 3],
    prev_leaf: Option<((usize, usize), usize)>,
    tx_select: bool,
    all_scans: &[Vec<u16>; 4],
) -> Result<usize> {
    let (r, c) = outer_at;
    if let Some(dv) = block.dv {
        write_intrabc_block(enc, cdfs, neighbours, dv, leaf_mi, 8)?;
        record_intrabc_mi(leaf_mi.0, leaf_mi.1, 2, Some(dv));
        return Ok(DC_PRED);
    }
    let mut above_mode = neighbours.above_mode[c];
    let mut left_mode = neighbours.left_mode[r];
    // The previous leaf sits directly above (same column, two mi rows up)
    // or directly to the left (same row, two mi cols over) of this one --
    // in either case its just-written mode, not the enclosing 16x16 slot's
    // stale neighbour, is what a decoder reads as this leaf's context.
    if let Some(((pr, pc), pmode)) = prev_leaf {
        if pc == leaf_mi.1 && leaf_mi.0 == pr + 2 {
            above_mode = pmode;
        } else if pr == leaf_mi.0 && leaf_mi.1 == pc + 2 {
            left_mode = pmode;
        }
    }
    // An 8x8 leaf is well within `is_cfl_allowed`'s <=32x32 bound (spec
    // 5.11.5), so it reads the CFL-allowed `uv_mode_cfl` CDF -- like every
    // other `write_block` caller at 16x16 and up -- not the narrower
    // no-CFL one: r12 lane-av1-rect, this leaf's own `cfl: false` was the
    // true first divergence (a differently-sized alphabet under the same
    // DC_PRED decision desyncs the coder even though the decoded mode is
    // unchanged).
    let mode = write_intra_mode(
        enc, cdfs, neighbours, block, above_mode, left_mode, true, leaf_mi, 8,
    );
    let planes = [TxbSet::Luma8, TxbSet::Chroma4, TxbSet::Chroma4];
    // As [`write_block`]: the coefficient row follows the filter-intra mode,
    // the mode this leaf publishes (and returns to the next leaf) does not.
    let tx_mode = tx_row(mode, block.filter_intra);
    let split = if tx_select {
        let tx = write_luma_select(
            enc,
            cdfs,
            neighbours,
            leaf_mi,
            8,
            usize::from(block.tx_depth),
            &grids[0],
            tx_mode,
            all_scans,
            &block.luma_tx_types,
        )?;
        tx < 8
    } else {
        false
    };
    write_block_planes(
        enc,
        cdfs,
        &planes,
        grids,
        &scans,
        &neighbours.around_mi(leaf_mi, 8),
        tx_mode,
        tx_select,
        block_tx_type(block),
        false,
    );
    neighbours.record_mi_planes(leaf_mi, 8, grids, !split);
    clear_skip_band(neighbours, leaf_mi, 8);
    record_intrabc_mi(leaf_mi.0, leaf_mi.1, 2, None);
    Ok(mode)
}

/// Writes the three transform blocks of one coded block, in the order a
/// decoder reads them.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_block_planes(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    planes: &[TxbSet; 3],
    grids: &[Vec<i32>; 3],
    scans: &[&Vec<u16>; 3],
    around: &[Around; 3],
    mode: usize,
    // A block whose luma was already written as several transform units
    // ([`write_luma_select`]) has only its chroma planes left here.
    skip_luma: bool,
    // This block's single luma transform type (lane-txset); chroma's is
    // derived from its mode by the decoder and codes no symbol.
    luma_tx_type: TxType,
    // lane-intertx: an INTER block's chroma transforms INHERIT the luma
    // type (reduced to the chroma set against this frame's own
    // `reduced_tx_set` bit); an intra block's comes off its UV mode and
    // rides `DCT_DCT`'s 2D class exactly as before.
    inter: bool,
) {
    // lane-intertx, desync (b): the writer used to code every chroma plane
    // `DCT_DCT`. Chroma codes no `tx_type` symbol, but the type names the
    // transform class, and the class picks the scan, the `eob_pt` row and
    // the context offsets (the lane-txw note in `write_coeffs`) -- so an
    // inherited 1-D luma type desynced the chroma exactly as luma itself
    // once did. The reduced bit read here is the frame's own header bit,
    // the ONE the writer's `Cdfs` is armed with (`Cdfs::reduced_tx_set`).
    let reduced = cdfs.reduced_tx_set;
    for (plane, (grid, scan)) in grids.iter().zip(scans.iter()).enumerate() {
        if plane == 0 && skip_luma {
            continue;
        }
        // Luma's transform covers its whole block, which fixes the all-zero
        // flag's context at zero; a chroma transform reads whether its
        // neighbours coded anything, on top of the offset the chroma tables
        // start at.
        let skip_ctx = if plane == 0 {
            0
        } else {
            usize::from(around[plane].above_coded) + usize::from(around[plane].left_coded)
        };
        #[cfg(test)]
        let before = enc.tell();
        #[cfg(test)]
        let q_ctx = cdfs.q_ctx;
        let mut coding = cdfs.txb(planes[plane], mode);
        let tx_type = if plane == 0 {
            luma_tx_type
        } else if inter {
            crate::decode::reduce_inherited_chroma_tx_type_flagged(
                luma_tx_type,
                coding.side.min(32),
                coding.side.min(32),
                reduced,
            )
        } else {
            TxType::DctDct
        };
        write_coeffs(
            enc,
            &mut coding,
            grid,
            scan,
            skip_ctx,
            dc_sign_ctx(around[plane].dc_vote),
            Some(plane),
            tx_type,
        );
        #[cfg(test)]
        census_record(
            planes[plane],
            q_ctx,
            grid,
            f64::from(enc.tell() - before),
            skip_ctx,
            dc_sign_ctx(around[plane].dc_vote),
        );
        ec_rng_trace(|| {
            format!(
                "EC_PLANE plane={plane} nz={} skip_ctx={skip_ctx} tell={}",
                grid.iter().filter(|&&l| l != 0).count(),
                enc.tell()
            )
        });
    }
}

/// The single luma transform type a block carries, `DCT_DCT` for one that
/// never searched one (lane-txset).
pub(super) fn block_tx_type(block: &BlockCoeffs) -> TxType {
    block
        .luma_tx_types
        .first()
        .copied()
        .unwrap_or(TxType::DctDct)
}

/// The `(side / tx)^2` transform units of one block's luma residual, raster
/// order, each with its own coefficient context and each published before the
/// next one reads it (decode.rs `decode_block`'s multi-transform-unit branch).
/// `grid` is the block's levels in BLOCK coordinates when the transform
/// splits, and the single transform's own coefficient grid when it does not.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_luma_tus(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    at_mi: (usize, usize),
    side: usize,
    tx: usize,
    grid: &[i32],
    mode: usize,
    scans: &[Vec<u16>; 4],
    // lane-av1tx2 r2: an INTER block's split units read the inter coefficient
    // tables, not the intra ones (`TxbSet::Luma*Inter`).
    inter: bool,
    // One transform type per unit in this same raster order (lane-txset).
    // Empty -- what every caller that never searched a type passes -- is
    // `DCT_DCT` throughout, the one type this writer used to code.
    tx_types: &[TxType],
) -> Result<()> {
    if tx < side && side > 32 && tx < 32 {
        // A 64x64 block's own levels reach the writer as the 32x32 corner a
        // 64-point transform codes unless the whole block was SPLIT into
        // TX_32X32 units (lane-b64b), so any finer split has nothing to slice.
        return Err(Error::unsupported(
            "AV1 tile",
            "a 64x64 block's luma transform splits no finer than TX_32X32",
        ));
    }
    let coeff_side = tx.min(32);
    let n = side / tx;
    let set = match (tx, inter) {
        (32, false) => TxbSet::Luma32,
        (16, false) => TxbSet::Luma16,
        (8, false) => TxbSet::Luma8,
        (4, false) => TxbSet::Luma4,
        (32, true) => TxbSet::Luma32Inter,
        (16, true) => TxbSet::Luma16Inter,
        (8, true) => TxbSet::Luma8Inter,
        (4, true) => TxbSet::Luma4Inter,
        _ => TxbSet::Luma64,
    };
    let scan = match coeff_side {
        32 => &scans[0],
        16 => &scans[1],
        8 => &scans[2],
        _ => &scans[3],
    };
    let (mi_r, mi_c) = at_mi;
    for tu_row in 0..n {
        for tu_col in 0..n {
            let tu_mi = (mi_r + tu_row * (tx / MI), mi_c + tu_col * (tx / MI));
            // lane-av1straddle: libaom clips the transform loop to
            // `max_blocks_wide/high` (`mb_to_right_edge`/`mb_to_bottom_edge`,
            // off the frame's true `mi_cols`/`mi_rows`), and decode.rs does
            // the same (`tu_px >= y.true_width` there): a unit whose TOP-LEFT
            // sample is outside the frame is NEVER coded. Writing one -- as
            // this loop did for the phantom right-hand column of a 32x32 at
            // x=192 in a 216-wide frame -- desyncs the tile at the very next
            // symbol, which is what made an odd-size GOP's last block
            // reconstruct differently and both ffmpeg decoders (libdav1d and
            // libaom) refuse the whole key frame.
            if tu_mi.0 >= neighbours.mi_rows || tu_mi.1 >= neighbours.mi_cols {
                continue;
            }
            let unit = if n == 1 {
                grid.to_vec()
            } else {
                (0..tx)
                    .flat_map(|row| grid[(tu_row * tx + row) * side + tu_col * tx..][..tx].to_vec())
                    .collect()
            };
            // spec `get_txb_ctx_general`: a lone transform unit covering its
            // whole block reads context 0, a smaller one the neighbour
            // magnitude table.
            let skip_ctx = if n == 1 {
                0
            } else {
                neighbours.luma_skip_ctx(tu_mi, tx / MI)
            };
            let around = neighbours.around_mi(tu_mi, tx)[0];
            #[cfg(test)]
            let before = enc.tell();
            #[cfg(test)]
            let q_ctx = cdfs.q_ctx;
            write_coeffs(
                enc,
                &mut cdfs.txb(set, mode),
                &unit,
                scan,
                skip_ctx,
                dc_sign_ctx(around.dc_vote),
                Some(0),
                tx_types
                    .get(tu_row * n + tu_col)
                    .copied()
                    .unwrap_or(TxType::DctDct),
            );
            #[cfg(test)]
            census_record(
                set,
                q_ctx,
                &unit,
                f64::from(enc.tell() - before),
                skip_ctx,
                dc_sign_ctx(around.dc_vote),
            );
            if n > 1 {
                neighbours.record_mi_luma(tu_mi, tx, &unit);
            }
        }
    }
    Ok(())
}

/// Writes one intra block's luma residual under `TxMode::Select`: the
/// `tx_depth` symbol (spec 5.11.16 `read_selected_tx_size`, off the size
/// category's own CDF at [`Neighbours::tx_size_ctx`]'s row), then the
/// `(side / tx)^2` transform units in raster order -- each predicted and
/// coded against what the units before it already left behind, which is why
/// every unit publishes its own coefficient context before the next one reads
/// it (decode.rs `decode_block`'s multi-transform-unit branch).
///
/// `grid` is the block's levels in BLOCK coordinates (`side` by `side`) when
/// the transform splits, and the single transform's own coefficient grid when
/// it does not. Hands back the resolved transform side in pixels.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_luma_select(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    at_mi: (usize, usize),
    side: usize,
    depth: usize,
    grid: &[i32],
    mode: usize,
    scans: &[Vec<u16>; 4],
    tx_types: &[TxType],
) -> Result<usize> {
    let max_tx = side.min(64);
    let ctx = neighbours.tx_size_ctx(at_mi, max_tx);
    match max_tx {
        8 => enc.symbol(depth, &mut cdfs.tx_size_cat0[ctx]),
        16 => enc.symbol(depth, &mut cdfs.tx_size_cat1[ctx]),
        32 => enc.symbol(depth, &mut cdfs.tx_size_cat2[ctx]),
        64 => enc.symbol(depth, &mut cdfs.tx_size_cat3[ctx]),
        _ => {
            return Err(Error::unsupported(
                "AV1 tile",
                "a tx_depth symbol is only coded at 8/16/32/64",
            ));
        }
    }
    let tx = max_tx >> depth;
    write_luma_tus(
        enc, cdfs, neighbours, at_mi, side, tx, grid, mode, scans, false, tx_types,
    )?;
    neighbours.record_tx(at_mi, side, tx);
    Ok(tx)
}

/// The transform syntax one block of an INTER frame coded with
/// `tx_mode == TxMode::Select` carries, in the place spec 5.11.16's
/// `read_block_tx_size` reads it -- after the modes and the motion vector,
/// before the residual:
///  * an inter block that codes a residual walks a var-tx tree; this writer
///    keeps it at its own largest transform, so that is one `txfm_split` flag
///    of zero, off `txfm_partition_ctx_rect`'s row, publishing the bands per
///    unit the way `read_var_tx_size` does.
///  * a skipped inter block codes nothing and records its own BLOCK size
///    (`set_txfm_ctxs`'s `skip_inter` term).
///  * an intra block codes `tx_depth` like a key frame's, but off the
///    `TXFM_CONTEXT` bands rather than the deblock grid.
///
/// Hands back the block's resolved luma transform side.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_tx_syntax_inter(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    at_mi: (usize, usize),
    side: usize,
    is_inter: bool,
    skip: bool,
    depth: usize,
) -> usize {
    let max_tx = side.min(64);
    let (mi_r, mi_c) = at_mi;
    if is_inter {
        if !skip {
            let ctx = txfm_partition_ctx_rect(
                neighbours.above_txfm[mi_c],
                neighbours.left_txfm[mi_r],
                side,
                max_tx,
                max_tx,
            );
            enc.symbol(usize::from(depth != 0), &mut cdfs.txfm_partition[ctx]);
            if depth == 0 {
                neighbours.record_txfm(at_mi, max_tx, max_tx, max_tx);
                return max_tx;
            }
            // lane-av1tx2 r2: one halving of the var-tx tree. Each child of a
            // split root codes its own `txfm_split` (decode.rs
            // `read_var_tx_size` only stops symbol-free at `MAX_VARTX_DEPTH`
            // or at a 4x4 sub-size), off the bands the children before it
            // published -- so the context read and the update interleave per
            // unit, exactly as the reader's recursion does.
            let tx = max_tx / 2;
            for row in (0..max_tx / MI).step_by(tx / MI) {
                for col in (0..max_tx / MI).step_by(tx / MI) {
                    let unit = (mi_r + row, mi_c + col);
                    // The encoder only picks depth 1 for a block wholly
                    // inside the frame; the reader skips a unit past the true
                    // edge, which would leave the residual units unpaired.
                    debug_assert!(
                        unit.0 < neighbours.mi_rows && unit.1 < neighbours.mi_cols,
                        "a split inter transform unit past the frame edge"
                    );
                    let ctx = txfm_partition_ctx_rect(
                        neighbours.above_txfm[unit.1],
                        neighbours.left_txfm[unit.0],
                        side,
                        tx,
                        tx,
                    );
                    enc.symbol(0, &mut cdfs.txfm_partition[ctx]);
                    neighbours.record_txfm(unit, tx, tx, tx);
                }
            }
            return tx;
        }
        neighbours.record_txfm(at_mi, side, side, side);
        return max_tx;
    }
    let ctx = neighbours.tx_size_ctx_txfm(at_mi, max_tx);
    match max_tx {
        8 => enc.symbol(depth, &mut cdfs.tx_size_cat0[ctx]),
        16 => enc.symbol(depth, &mut cdfs.tx_size_cat1[ctx]),
        32 => enc.symbol(depth, &mut cdfs.tx_size_cat2[ctx]),
        _ => enc.symbol(depth, &mut cdfs.tx_size_cat3[ctx]),
    }
    let tx = max_tx >> depth;
    neighbours.record_txfm(at_mi, tx, side, side);
    tx
}

/// What a coded transform block leaves behind for the blocks that read it as a
/// neighbour.
pub(super) fn neighbour_state(grid: &[i32]) -> Neighbour {
    Neighbour {
        coded: grid.iter().any(|&l| l != 0),
        level: grid.iter().map(|l| l.unsigned_abs()).sum::<u32>().min(7) as u8,
        dc: (grid[0] != 0).then_some(grid[0] < 0),
    }
}

/// Writes the payload of a one-tile key frame in which every superblock is
/// split into 32x32 blocks, each carrying the coefficients `blocks` gives it.
///
/// `blocks` gives one coefficient set per 32x32 block in raster order across
/// the frame. This is [`sb_coeff_key_frame_tile`] with every superblock split.
///
/// # Errors
/// As [`sb_coeff_key_frame_tile`], with `blocks` sized for the 32x32 grid.
pub fn split_coeff_key_frame_tile(
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    blocks: &[BlockCoeffs],
) -> Result<Vec<u8>> {
    check_blocks(mi_cols, mi_rows)?;
    let (cols, rows) = block_grid(mi_cols, mi_rows);
    if blocks.len() != (cols * rows) as usize {
        return Err(Error::unsupported(
            "AV1 tile",
            "a coefficient key frame needs one coefficient set per 32x32 block",
        ));
    }
    let (sb_cols, sb_rows) = (cols.div_ceil(2), rows.div_ceil(2));
    let superblocks: Vec<Superblock> = (0..sb_rows)
        .flat_map(|sb_r| (0..sb_cols).map(move |sb_c| (sb_r, sb_c)))
        .map(|(sb_r, sb_c)| {
            Superblock::Split(
                (0..4)
                    .map(|q| (sb_r * 2 + q / 2, sb_c * 2 + q % 2))
                    .filter(|&(r, c)| r < rows && c < cols)
                    .map(|(r, c)| Quadrant::Whole(blocks[(r * cols + c) as usize].clone()))
                    .collect(),
            )
        })
        .collect();
    sb_coeff_key_frame_tile(mi_cols, mi_rows, base_q_idx, &superblocks)
}

/// Lays one plane's coefficient list out over its transform, rejecting the
/// positions and levels the writer does not code.
pub(super) fn level_grid(coeffs: &[Coeff], side: usize) -> Result<Vec<i32>> {
    let mut grid = vec![0i32; side * side];
    for coeff in coeffs {
        if usize::from(coeff.row) >= side || usize::from(coeff.col) >= side {
            return Err(Error::unsupported(
                "AV1 tile",
                "a coefficient sits outside the transform of its plane",
            ));
        }
        if coeff.level == 0 || coeff.level.abs() > MAX_LEVEL {
            return Err(Error::unsupported(
                "AV1 tile",
                format!(
                    "coefficients are written for levels -{MAX_LEVEL}..={MAX_LEVEL} without zero"
                ),
            ));
        }
        let pos = usize::from(coeff.row) * side + usize::from(coeff.col);
        if grid[pos] != 0 {
            return Err(Error::unsupported(
                "AV1 tile",
                "two coefficients of one block share a position",
            ));
        }
        grid[pos] = coeff.level;
    }
    Ok(grid)
}

/// The default scan of a square transform (spec 8.4.2's `Default_Scan_NxN`),
/// as raster positions in the order they are coded.
///
/// The table is a rule rather than hundreds of pinned numbers: the scan walks
/// the anti-diagonals of the transform outwards from the origin, each diagonal
/// from its top-right end on odd diagonals and from its bottom-left end on
/// even ones.
pub(super) fn default_scan(side: usize) -> Vec<u16> {
    let mut scan = Vec::with_capacity(side * side);
    for d in 0..(2 * side - 1) {
        let lo = d.saturating_sub(side - 1);
        let hi = d.min(side - 1);
        let diagonal = (lo..=hi).map(|row| (row * side + (d - row)) as u16);
        if d % 2 == 0 {
            scan.extend(diagonal.rev());
        } else {
            scan.extend(diagonal);
        }
    }
    scan
}

/// What one 32x32 luma transform block's levels cost, in bits, priced through
/// the very CDFs [`write_coeffs`] will write them with -- end-of-block
/// position, base levels, base range, signs and the Golomb tail all included.
///
/// The mode search calls this before the block's neighbours exist, so the two
/// neighbour-derived contexts (the block-skip context and the DC sign context)
/// are taken as zero. Every other context inside the block is exact, because
/// those are read from the block's own levels.
///
/// The encoder codes 32x32 luma only -- every superblock it emits is split --
/// so one table is all the search needs; a second block size would need its
/// own entry point rather than a size argument, because the table and the scan
/// have to agree.
/// What the search paid for every coefficient of `blocks`, in bits: the same
/// [`coeff_bits`] price the mode search ranked those blocks by, summed over
/// the blocks that were actually kept.
///
/// The instrument behind `predicted_coeff_bits_track_the_tile_the_writer_wrote`
/// (lane-av1rd1): the writer codes these levels against a state that has
/// adapted away from the defaults `coeff_bits` prices with, and against real
/// neighbour contexts instead of the empty ones, so the sum here and the tile
/// the writer produced differ by exactly that drift plus the non-coefficient
/// syntax (partition/mode/skip/mv symbols) the sum leaves out.
#[cfg(test)]
pub(super) fn dense(coeffs: &[Coeff], side: usize) -> Vec<i32> {
    let mut grid = vec![0i32; side * side];
    for c in coeffs {
        grid[usize::from(c.row) * side + usize::from(c.col)] = c.level;
    }
    grid
}

#[cfg(test)]
pub(crate) fn predicted_coeff_bits_sb(superblocks: &[Superblock], base_q_idx: u8) -> f64 {
    let q_ctx = crate::decode::q_ctx_of(base_q_idx);
    superblocks
        .iter()
        .map(|sb| match sb {
            // A whole 64x64 codes its luma through `TxbSet::Luma64` (only the
            // top-left 32x32 carries coefficients) and each chroma plane at
            // 32x32; `predicted_coeff_bits` has no 64 arm, so price it here.
            Superblock::Whole(block) if !block.skip => {
                coeff_bits(&dense(&block.luma, 32), TxbSet::Luma64, q_ctx, 0, 0)
                    + coeff_bits(&dense(&block.u, 32), TxbSet::Chroma32, q_ctx, 0, 0)
                    + coeff_bits(&dense(&block.v, 32), TxbSet::Chroma32, q_ctx, 0, 0)
            }
            Superblock::Whole(_) => 0.0,
            Superblock::Split(quadrants) => predicted_coeff_bits(quadrants, base_q_idx),
        })
        .sum()
}

#[cfg(test)]
pub(crate) fn predicted_coeff_bits(blocks: &[Quadrant], base_q_idx: u8) -> f64 {
    let q_ctx = crate::decode::q_ctx_of(base_q_idx);
    fn block_bits(block: &BlockCoeffs, side: usize, q_ctx: usize) -> f64 {
        if let Some(leaves) = &block.eight {
            return leaves.iter().map(|l| block_bits(l, 8, q_ctx)).sum();
        }
        if block.skip {
            return 0.0;
        }
        let inter = block.inter.is_some();
        let (luma, chroma) = match (side, inter) {
            (32, false) => (TxbSet::Luma32, TxbSet::Chroma16),
            (32, true) => (TxbSet::Luma32Inter, TxbSet::Chroma16),
            (16, false) => (TxbSet::Luma16, TxbSet::Chroma8),
            (16, true) => (TxbSet::Luma16Inter, TxbSet::Chroma8),
            (_, false) => (TxbSet::Luma8, TxbSet::Chroma4),
            (_, true) => (TxbSet::Luma8Inter, TxbSet::Chroma4),
        };
        coeff_bits(&dense(&block.luma, side), luma, q_ctx, 0, 0)
            + coeff_bits(&dense(&block.u, side / 2), chroma, q_ctx, 0, 0)
            + coeff_bits(&dense(&block.v, side / 2), chroma, q_ctx, 0, 0)
    }
    blocks
        .iter()
        .map(|q| {
            let side = match q {
                // lane-b128: a 128x128 root is SKIP by construction (the
                // writer refuses a residual at that size), so it names no
                // coefficients on any plane.
                Quadrant::Whole128(_) | Quadrant::Rect128(..) | Quadrant::Ab128(..) => {
                    return 0.0;
                }
                Quadrant::Whole(_) => 32,
                Quadrant::Split(_) => 16,
                // lane-tx64: a 64x64 root that codes a residual pays for one
                // TX_64X64 luma transform (only the top-left 32x32 carries
                // coefficients) and one TX_32X32 per chroma plane -- the same
                // three prices `predicted_coeff_bits_sb` takes for a key
                // frame's whole superblock, and the same sets the writer's
                // `Whole64` arm codes. `block_bits`'s table has no 64 row, so
                // it is spelled out here.
                Quadrant::Whole64(block) => {
                    return match block.skip {
                        true => 0.0,
                        false => {
                            // lane-b64b: at `tx_depth = 1` the luma levels are
                            // the whole 64-sided block, written as four
                            // TX_32X32 units, so the pricer slices them the
                            // same way the writer does.
                            let luma = if block.tx_depth == 1 {
                                let g = dense(&block.luma, 64);
                                (0..4)
                                    .map(|tu| {
                                        let (r0, c0) = ((tu / 2) * 32, (tu % 2) * 32);
                                        let unit: Vec<i32> = (0..32)
                                            .flat_map(|row| {
                                                g[(r0 + row) * 64 + c0..][..32].to_vec()
                                            })
                                            .collect();
                                        coeff_bits(&unit, TxbSet::Luma32Inter, q_ctx, 0, 0)
                                    })
                                    .sum::<f64>()
                            } else {
                                coeff_bits(&dense(&block.luma, 32), TxbSet::Luma64, q_ctx, 0, 0)
                            };
                            luma + coeff_bits(&dense(&block.u, 32), TxbSet::Chroma32, q_ctx, 0, 0)
                                + coeff_bits(&dense(&block.v, 32), TxbSet::Chroma32, q_ctx, 0, 0)
                        }
                    };
                }
                Quadrant::Covered => return 0.0,
            };
            q.blocks()
                .iter()
                .map(|b| block_bits(b, side, q_ctx))
                .sum::<f64>()
        })
        .sum()
}

#[cfg(test)]
pub(crate) fn luma_32_coeff_bits(grid: &[i32]) -> f64 {
    coeff_bits(grid, TxbSet::Luma32, 2, 0, 0)
}

/// What one transform block's levels cost through the CDFs of `set`, in bits.
///
/// The search runs before the tile is written, so the adapted state the block
/// will really be coded against does not exist yet: the price is taken against
/// the defaults every tile starts from, at the frame's own coefficient
/// q-context (`q_ctx_of(base_q_idx)`, what `Cdfs::new` is given at
/// `sb_coeff_*_tile` -- lane-av1rd1: this used to be pinned at 2, so two of
/// the four points of the BD ladder were priced against another quantizer's
/// tables), and against the contexts a block whose neighbours coded nothing
/// reads.
///
/// lane-av1skipctx CLOSED that row: `skip_ctx`/`sign_ctx` are the caller's
/// now, and [`crate::encode::CoefCtxMap`] -- a per-plane, per-4x4-cell map
/// committed in coding order and undone with a losing partition trial's
/// pixels -- is the per-trial neighbour state the previous lane said the
/// search did not have. The census (6 frames of each clip at 640x384, q=100,
/// `pricer_error_census_on_clips`) moved:
///
/// | row | before | after |
/// |---|---|---|
/// | screen total | +10.3% | +9.5% |
/// | screen Chroma8 nz=0 | -73.0% | -1.9% |
/// | screen Chroma16 nz=0 | -48% | -23.4% |
/// | screen Chroma4 nz=0 | -74% | -6.7% |
/// | screen Chroma8 nz=1 / nz=2 | +32.2% / +26.2% | +4.9% / +7.9% |
/// | screen Luma4 nz=0 | +464% | +89.5% |
/// | film total | +5.3% | +1.5% |
///
/// What is left in the all-zero rows is table ADAPTATION, not context: the
/// price is taken against the frame's starting tables, and a frame whose
/// blocks mostly code nothing narrows `txb_skip` far below where it started.
/// A whole-block luma transform still reads context 0 -- that is what the
/// writer codes (`write_luma_tus`, `n == 1`), not an approximation.
/// The default scan of one transform size, built once per size: the search
/// prices thirteen modes for every block, and the scan is the same table
/// every time.
pub(super) fn scan_of(side: usize) -> &'static Vec<u16> {
    pub(super) static SCANS: LazyLock<[Vec<u16>; 4]> = LazyLock::new(|| {
        [
            default_scan(TX4),
            default_scan(TX8),
            default_scan(TX16),
            default_scan(TX32),
        ]
    });
    match side {
        TX4 => &SCANS[0],
        TX8 => &SCANS[1],
        TX16 => &SCANS[2],
        _ => &SCANS[3],
    }
}

/// [`scan_of`]'s `TX_CLASS_HORIZ`/`TX_CLASS_VERT` sibling (lane-txw): the
/// `Mrow_Scan`/`Mcol_Scan` a `V_DCT`/`H_DCT` unit is coded in, built off the
/// DECODER's own table so the two cannot drift, cached per size and class.
pub(super) fn class_scan_of(side: usize, class: crate::decode::TxClass) -> &'static [u16] {
    pub(super) static SCANS: LazyLock<[[Vec<u16>; 2]; 4]> = LazyLock::new(|| {
        use crate::decode::{TxClass, class_scan_table};
        [TX4, TX8, TX16, TX32].map(|side| {
            [
                class_scan_table(side, TxClass::Horiz),
                class_scan_table(side, TxClass::Vert),
            ]
        })
    });
    let size = match side {
        TX4 => 0,
        TX8 => 1,
        TX16 => 2,
        _ => 3,
    };
    let idx = usize::from(class == crate::decode::TxClass::Vert);
    &SCANS[size][idx]
}

/// [`coeff_bits_typed`] at `DCT_DCT`, the only type the test-only pricers
/// ([`predicted_coeff_bits`]) model.
#[cfg(test)]
pub(crate) fn coeff_bits(
    grid: &[i32],
    set: TxbSet,
    q_ctx: usize,
    skip_ctx: usize,
    sign_ctx: usize,
) -> f64 {
    coeff_bits_typed(grid, set, q_ctx, skip_ctx, sign_ctx, TxType::DctDct)
}

/// What levels a named transform type produced cost: the `tx_type`
/// symbol is part of what the block spends, so a search comparing types has
/// to see each one's own symbol priced (lane-txset).
pub(crate) fn coeff_bits_typed(
    grid: &[i32],
    set: TxbSet,
    q_ctx: usize,
    // lane-av1skipctx: the two neighbour-derived contexts, mirrored from the
    // writer's own (`write_block_planes`/`write_luma_tus`) off the search's
    // coding-order-committed map ([`crate::encode::CoefCtxMap`]). `(0, 0)` is
    // what every caller passed before the map existed -- exact for a luma
    // transform covering its whole block, and the census row that was the
    // largest remaining pricer error for every other block.
    skip_ctx: usize,
    sign_ctx: usize,
    tx_type: TxType,
) -> f64 {
    // `Cdfs::new` builds every table a tile writer adapts -- tens of
    // kilobytes -- and this function is called once per priced candidate:
    // together with the copy behind it that was 9% of the encoder's profile
    // (`__memmove` 6.0% + `Cdfs::new` 3.4%, both under this call). Only the
    // tables `write_coeffs` touches change, so keep one scratch `Cdfs` per
    // q-context per thread beside a pristine one and restore just those from
    // it. The state the price is taken against is identical to a fresh
    // `Cdfs::new(q_ctx)`, so every price is bit-identical.
    // lane-av1txbits: an inter frame's writer does NOT start from the
    // defaults -- it starts from the tables the previous frame stored (spec
    // 7.20, `start_cdfs` in `encode_inter_frame`) -- so pricing against the
    // defaults charged the search for a narrowing the writer no longer pays.
    // The census measured that at +31% on the one-coefficient inter blocks
    // the search lives on. `arm_pricing_cdfs` puts the frame's own starting
    // tables under the price; nothing armed keeps the old default behaviour.
    let price = |slot: &mut (Cdfs, Cdfs)| {
        let (pristine, scratch) = (&mut slot.0, &mut slot.1);
        let src = pristine.txb(set, DC_PRED);
        let mut coding = scratch.txb(set, DC_PRED);
        coding.txb_skip.copy_from_slice(src.txb_skip);
        coding.eob_pt.copy_from_slice(src.eob_pt);
        if let (Some(dst), Some(src)) = (coding.eob_pt_class1.as_mut(), src.eob_pt_class1) {
            dst.copy_from_slice(src);
        }
        *coding.eob_extra = *src.eob_extra;
        *coding.base = *src.base;
        *coding.base_eob = *src.base_eob;
        *coding.br = *src.br;
        *coding.dc_sign = *src.dc_sign;
        if let (Some(dst), Some(src)) = (coding.tx_type.as_mut(), src.tx_type) {
            dst.copy_from_slice(src);
        }
        let scan = scan_of(coding.side);
        let mut enc = SymbolEncoder::pricer();
        write_coeffs(
            &mut enc,
            &mut coding,
            grid,
            scan,
            skip_ctx,
            sign_ctx,
            None,
            tx_type,
        );
        enc.bits()
    };
    // lane-av1speed2: 60% of the transform units of an inter stream hold no
    // coefficient, and for those `write_coeffs` codes ONE symbol -- the
    // all-zero flag -- and returns. Restoring the eight tables it might have
    // touched (~800 bytes of `copy_from_slice`, the largest `memmove` in the
    // encoder's profile) buys nothing there, so price that symbol straight
    // off the pristine table on a copy of its own row. Identical bits: the
    // scratch's `txb_skip[skip_ctx]` was restored from exactly this row, and
    // nothing after the flag runs.
    let zero_price = |slot: &mut (Cdfs, Cdfs)| {
        let mut row = slot.0.txb(set, DC_PRED).txb_skip[skip_ctx];
        let mut enc = SymbolEncoder::pricer();
        enc.symbol(1, &mut row);
        enc.bits()
    };
    let all_zero = grid.iter().all(|&level| level == 0);
    PRICING_BASE.with_borrow_mut(|base| {
        if let Some(slot) = base.as_deref_mut().filter(|s| s.0.q_ctx == q_ctx) {
            return if all_zero {
                zero_price(slot)
            } else {
                price(slot)
            };
        }
        PRICING.with_borrow_mut(|slots| {
            let slot = slots[q_ctx.min(3)]
                .get_or_insert_with(|| Box::new((Cdfs::new(q_ctx), Cdfs::new(q_ctx))));
            if all_zero {
                zero_price(slot)
            } else {
                price(slot)
            }
        })
    })
}

/// The largest level [`rdoq`] offers a candidate for, and the most prices it
/// may spend on one block. `EC_AV1_RDOQ_MAXLEVEL` / `EC_AV1_RDOQ_BUDGET`
/// sweep them; see [`rdoq`] for why both exist.
pub(super) const RDOQ_MAX_LEVEL: u32 = 2;
pub(super) const RDOQ_BUDGET: u32 = 24;

pub(super) fn rdoq_max_level() -> u32 {
    pub(super) static ENV: LazyLock<u32> = LazyLock::new(|| {
        crate::envflags::var("EC_AV1_RDOQ_MAXLEVEL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(RDOQ_MAX_LEVEL)
    });
    *ENV
}

pub(super) fn rdoq_budget() -> u32 {
    pub(super) static ENV: LazyLock<u32> = LazyLock::new(|| {
        crate::envflags::var("EC_AV1_RDOQ_BUDGET")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(RDOQ_BUDGET)
    });
    *ENV
}

/// Rate-distortion optimised quantisation (lane-rdoq): the pass every
/// reference encoder runs after the quantiser and this one did not have.
///
/// The deadzone quantiser rounds each coefficient on its own, blind to what
/// the coded level costs. Every reference decides that per coefficient
/// against the entropy coder (libaom `av1_optimize_txb`, rav1e
/// `optimize_txb`): a level is lowered toward zero when the bits it gives
/// back are worth more than the squared error it adds, which also shortens
/// the eob when the tail levels go to zero.
///
/// Mirroring rav1e's shape without its incremental cost model: the candidate
/// set per coefficient is `{level, level - 1, .., 0}` walked one step at a
/// time in REVERSE scan order (the tail first, so a dropped tail is retried
/// against the shorter eob it leaves), and each candidate is priced by
/// re-coding the whole block through [`coeff_bits`] -- the same CDFs, the
/// same neighbour contexts and the same writer the search already prices
/// with, so no second cost model can drift from the writer (class
/// `table-and-reader-move-together`). That costs one `write_coeffs` per
/// candidate instead of rav1e's table lookup, which is why this is a preset
/// lever (`crate::speed::RDOQ`) and not unconditional.
///
/// `levels` is the side-strided grid the inverse transform and the writer
/// will read, `scaled` the pre-rounding `coeff / q` of each position
/// (`crate::transform::forward_and_quantize_scaled`). `lambda` is the rate
/// weight in units of ONE SQUARED AC QUANTISER STEP: the search's own
/// `sse + lambda * bits` is `step^2 * (sum (scaled - level)^2 + LAMBDA_SCALE
/// * bits)` once the pixel-domain error of a level is written through the
/// orthonormal transform (`(coeff - level * q) / 8`, `INVERSE_GAIN_RECIPROCAL`),
/// so `step^2` divides out of every comparison here and the DC's different
///   quantiser is the only per-position weight left.
///
/// Returns what the block's levels cost after the pass, so the caller prices
/// the tile it will actually write and never the pre-RDOQ grid.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rdoq(
    levels: &mut [i32],
    scaled: &[f32],
    side: usize,
    base_q_idx: u8,
    set: TxbSet,
    skip_ctx: usize,
    sign_ctx: usize,
    lambda: f64,
    tx_type: TxType,
) -> f64 {
    // A 64-point transform carries its top-left 32x32 alone, and that corner
    // is what the writer and the pricer take (`coded_corner`).
    let coded = side.min(TX32);
    let q_ctx = crate::decode::q_ctx_of(base_q_idx);
    let mut dense = if coded == side {
        Vec::new()
    } else {
        vec![0i32; coded * coded]
    };
    fn price(
        levels: &[i32],
        dense: &mut [i32],
        side: usize,
        coded: usize,
        set: TxbSet,
        q_ctx: usize,
        skip_ctx: usize,
        sign_ctx: usize,
        tx_type: TxType,
    ) -> f64 {
        if dense.is_empty() {
            return coeff_bits_typed(levels, set, q_ctx, skip_ctx, sign_ctx, tx_type);
        }
        for row in 0..coded {
            dense[row * coded..][..coded].copy_from_slice(&levels[row * side..][..coded]);
        }
        coeff_bits_typed(dense, set, q_ctx, skip_ctx, sign_ctx, tx_type)
    }
    let mut bits = price(
        levels, &mut dense, side, coded, set, q_ctx, skip_ctx, sign_ctx, tx_type,
    );
    // The DC has its own quantiser, so its squared error per unit of level is
    // `(dc_q / ac_q)^2` of an AC coefficient's.
    let dc = f64::from(crate::quant::dc_q(8, i32::from(base_q_idx)));
    let ac = f64::from(crate::quant::ac_q(8, i32::from(base_q_idx)));
    let dc_weight = (dc / ac) * (dc / ac);
    let census = crate::envflags::env_flag!("EC_AV1_RDOQ_CENSUS");
    if census {
        census_block(levels, scaled, side, coded, bits);
    }
    // Where the pass may spend its prices. Every candidate here costs a whole
    // `write_coeffs`, where rav1e's incremental model costs a table lookup,
    // so both bounds exist to keep that affordable -- and both cut where the
    // decisions do not pay anyway:
    //  * a level above [`RDOQ_MAX_LEVEL`] is never a candidate: one step off
    //    a big level gives back a fraction of a bit for a whole quantiser
    //    step of squared error. What pays is `1 -> 0` and `2 -> 1`.
    //  * at most [`RDOQ_BUDGET`] prices per block, spent from the TAIL
    //    backwards -- the eob end, where dropping a level also shortens the
    //    eob -- so a fine-quantizer block carrying two hundred coefficients
    //    cannot cost two hundred re-codings.
    let mut budget = rdoq_budget();
    let max_level = rdoq_max_level();
    // lane-txw: the tail this walks back from is the tail of the unit's OWN
    // scan -- a `V_DCT`/`H_DCT` unit is coded in `Mrow_Scan`/`Mcol_Scan`, so
    // the 2D zigzag's last positions are not the ones whose removal shortens
    // its eob. Every candidate is still priced through the class-aware writer
    // ([`write_coeffs`]), so this changes which positions the budget reaches,
    // never whether a decision is right.
    let class = crate::decode::TxClass::of(tx_type);
    let order: &[u16] = if class == crate::decode::TxClass::TwoD {
        scan_of(coded)
    } else {
        class_scan_of(coded, class)
    };
    for &position in order.iter().rev() {
        if budget == 0 {
            break;
        }
        let (row, col) = (usize::from(position) / coded, usize::from(position) % coded);
        let i = row * side + col;
        if levels[i] == 0 || levels[i].unsigned_abs() > max_level {
            continue;
        }
        let s = f64::from(scaled[i]);
        let weight = if i == 0 { dc_weight } else { 1.0 };
        while budget > 0 {
            let level = levels[i];
            let candidate = level - level.signum();
            levels[i] = candidate;
            budget -= 1;
            let after = price(
                levels, &mut dense, side, coded, set, q_ctx, skip_ctx, sign_ctx, tx_type,
            );
            let distortion =
                weight * ((s - f64::from(candidate)).powi(2) - (s - f64::from(level)).powi(2));
            if distortion + lambda * (after - bits) < 0.0 {
                bits = after;
                if census {
                    RDOQ_STATS[if candidate == 0 { 4 } else { 5 }].fetch_add(1, Ordering::Relaxed);
                }
                if candidate == 0 {
                    break;
                }
            } else {
                levels[i] = level;
                break;
            }
        }
    }
    if census {
        RDOQ_STATS[6].fetch_add((bits * 64.0) as u64, Ordering::Relaxed);
    }
    bits
}

/// blocks, coded coefficients, +-1 levels, trailing +-1 levels (a run of them
/// at the end of the scan), zeroed coefficients, lowered coefficients, bits
/// after the pass * 64, bits before the pass * 64.
pub(crate) static RDOQ_STATS: [AtomicU64; 8] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

/// The pre-pass half of [`RDOQ_STATS`] (`EC_AV1_RDOQ_CENSUS`): what the
/// quantiser handed the pass, which is the instrument for how much is at
/// stake before any decision is taken.
pub(super) fn census_block(levels: &[i32], _scaled: &[f32], side: usize, coded: usize, bits: f64) {
    let scan = scan_of(coded);
    let at = |p: u16| levels[usize::from(p) / coded * side + usize::from(p) % coded];
    let nonzero = scan.iter().filter(|&&p| at(p) != 0).count();
    let ones = scan.iter().filter(|&&p| at(p).abs() == 1).count();
    let trailing = scan
        .iter()
        .rev()
        .skip_while(|&&p| at(p) == 0)
        .take_while(|&&p| at(p).abs() == 1)
        .count();
    RDOQ_STATS[0].fetch_add(1, Ordering::Relaxed);
    RDOQ_STATS[1].fetch_add(nonzero as u64, Ordering::Relaxed);
    RDOQ_STATS[2].fetch_add(ones as u64, Ordering::Relaxed);
    RDOQ_STATS[3].fetch_add(trailing as u64, Ordering::Relaxed);
    RDOQ_STATS[7].fetch_add((bits * 64.0) as u64, Ordering::Relaxed);
}

/// Reads and clears [`RDOQ_STATS`].
pub fn take_rdoq_stats() -> [u64; 8] {
    std::array::from_fn(|i| RDOQ_STATS[i].swap(0, Ordering::Relaxed))
}

thread_local! {
    /// The default tables every price falls back to, one pristine/scratch
    /// pair per q-context (see [`coeff_bits`] for why the pair is kept).
    pub(super) static PRICING: RefCell<[Option<Box<(Cdfs, Cdfs)>>; 4]> =
        const { RefCell::new([None, None, None, None]) };
    /// The tables the frame being searched will really be written against,
    /// armed per frame by the encoder ([`arm_pricing_cdfs`]) on every thread
    /// that prices -- the search jobs re-arm their own worker, exactly like
    /// the other tile thread-locals.
    pub(super) static PRICING_BASE: RefCell<Option<Box<(Cdfs, Cdfs)>>> = const { RefCell::new(None) };
}

/// Arms the tables [`coeff_bits`] prices against on this thread: the state
/// this frame's tile writer starts from. `None` goes back to the per-q-context
/// defaults (a key frame, whose writer really does start there), and so does
/// any frame the screen detector said yes to (`screen`).
///
/// ON by default for non-screen frames since lane-av1price2;
/// `EC_AV1_PRICE_FRAME_CDFS` selects 0 = never (the pre-lane behaviour),
/// 1 = always, unset/2 = the screen-gated default.
///
/// Pricing against the frame's REAL starting tables is the more accurate
/// estimate by measurement -- the pricer census
/// (`pricer_error_census_by_block_class`) goes from +7.6% to +4.8% over every
/// transform block, and the one- and two-coefficient inter blocks the search
/// lives on from +31.4%/+22.3% to +3.1%/+3.6% -- but the accuracy only
/// converts into ladder bytes on camera content. Measured on ONE build
/// (lane-av1price2, mode 0 vs mode 2), BD-rate vs libaom / vs rav1e:
///
/// | clip | 640x384 base | 640x384 gated | native base | native gated |
/// |---|---|---|---|---|
/// | film 1080p | +79.9 / +37.3 | +78.8 / +36.1 | +18.7 / +1.4 | +18.0 / +0.7 |
/// | film 2160p | +94.6 / +55.7 | +95.1 / +56.4 | +48.1 / +19.8 | +47.3 / +19.2 |
/// | screen | +54.5 / +1.1 | +54.5 / +1.1 | +59.4 / -10.0 | +59.4 / -10.0 |
///
/// The screen rows are not merely close, they are byte-identical (the ladders
/// print 8832/13507/18772/24754 and 39312/52829/70719/92134 in both modes):
/// the gate is per frame and the detector fires on 48 of 48 screen frames, so
/// no screen frame is ever armed with anything but the defaults. At native
/// resolution -- the resolution his library is in -- both films come down
/// against both references and the screen capture is flat, which is this
/// lane's keep rule. At the downscaled 640x384 gate the 2160p film goes the
/// other way (+0.5/+0.7) while the 1080p one comes down further (-1.1/-1.2);
/// the downscale is the same recipe that exaggerated the frame pyramid's loss
/// sixfold, so the native table decides and the 640x384 2160p regression is
/// recorded here rather than argued away.
///
/// Why the screen capture needs the gate at all is in the census
/// (`pricer_error_census_on_clips`, 6 frames of each clip at 640x384, q=100).
/// Real tables take the film clip's total pricer error from +7.8% to +5.3%
/// and flatten its inter classes outright (Luma8Inter/Luma16Inter/Luma32Inter
/// 1-coefficient rows from +11.7%/+17.4%/+24.9% to -0.3%/-5.5%/+8.8%). On the
/// screen capture the total only moves +12.8% -> +10.3%, because the classes
/// that dominate a screen frame are not the ones the tables fix: its chroma
/// stays over-priced (Chroma8 nz=1 +32.2% -> +31.8%, nz=2 +26.2% -> +26.2%,
/// unmoved) and its all-zero blocks stay hugely UNDER-priced (Chroma8 nz=0
/// -73.0% -> -73.0%, Luma4 nz=0 +464% -> +467% -- both a pricer that assumes
/// `skip_ctx` 0, not a table set). So on screen the change makes the inter
/// luma price accurate while leaving the bigger chroma and all-zero errors
/// exactly where they were, which SKEWS the search's tradeoffs between them
/// instead of correcting them -- and the bytes get worse. The fix for that
/// row was the neighbour skip context, not another table set, and
/// lane-av1skipctx landed it (see [`coeff_bits`]): with the real contexts
/// under every price the screen capture comes down +59.4 -> +58.3 vs libaom
/// at native, still with every one of its frames armed with the defaults.
pub(crate) fn arm_pricing_cdfs(base: Option<&Cdfs>, screen: bool) {
    pub(super) static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    let mode = *MODE.get_or_init(
        || match std::env::var("EC_AV1_PRICE_FRAME_CDFS").as_deref() {
            Ok("0") => 0,
            Ok("1") => 1,
            _ => 2,
        },
    );
    let on = match mode {
        0 => false,
        1 => true,
        _ => !screen,
    };
    PRICING_HITS[usize::from(on && base.is_some())].fetch_add(1, Ordering::Relaxed);
    PRICING_BASE.with_borrow_mut(|slot| {
        *slot = base
            .filter(|_| on)
            .map(|c| Box::new((c.clone(), c.clone())));
    });
}

/// How many times a frame's search was armed with the DEFAULT tables and how
/// many with the frame's REAL starting tables ([`arm_pricing_cdfs`]), so a
/// gate can print which side of the screen gate every frame landed on. Counts
/// arming calls, not frames: a multi-tile frame arms one worker per tile.
pub(crate) static PRICING_HITS: [AtomicUsize; 2] = [AtomicUsize::new(0), AtomicUsize::new(0)];

/// Reads and clears [`PRICING_HITS`]: (default-table armings, real-table
/// armings).
#[cfg(test)]
pub(crate) fn take_pricing_hits() -> (usize, usize) {
    (
        PRICING_HITS[0].swap(0, Ordering::Relaxed),
        PRICING_HITS[1].swap(0, Ordering::Relaxed),
    )
}

/// The pricer census (`pricer_census_on`): for every transform block the tile
/// writer really codes, the bits it spent against the bits [`coeff_bits`]
/// prices those very levels at -- the search's rate term measured against the
/// rate it is meant to predict. Keyed by (table set, non-zero bucket) so a
/// class that is systematically mispriced (all-zero blocks' `txb_skip`, the
/// eob of a one-coefficient block, a whole size) shows up on its own row
/// rather than inside one average.
/// Thread-local, so a census cannot pick up the blocks of another test's
/// encode running beside it; a multi-tile, multi-threaded encode would
/// therefore only census the tiles this thread wrote, which is every tile of
/// the one-tile-one-thread encodes the gates print it from.
#[cfg(test)]
pub(super) type CensusRows = BTreeMap<(String, usize), (u64, f64, f64)>;
#[cfg(test)]
thread_local! {
    pub(super) static CENSUS: RefCell<Option<CensusRows>> = const { RefCell::new(None) };
}

/// Starts a census on this thread, discarding anything a previous one left.
#[cfg(test)]
pub(crate) fn pricer_census_on() {
    CENSUS.with_borrow_mut(|c| *c = Some(BTreeMap::new()));
}

/// Stops the census and hands back its rows: (set, non-zero bucket) ->
/// (blocks, priced bits, written bits).
#[cfg(test)]
pub(crate) fn take_pricer_census() -> CensusRows {
    CENSUS.with_borrow_mut(|c| c.take()).unwrap_or_default()
}

/// The label of a non-zero bucket, for the tables the gates print.
#[cfg(test)]
pub(crate) fn census_bucket_label(bucket: usize) -> &'static str {
    ["0", "1", "2", "3-4", "5-8", "9-16", "17+"][bucket]
}

#[cfg(test)]
pub(super) fn census_record(
    set: TxbSet,
    q_ctx: usize,
    grid: &[i32],
    written: f64,
    // The writer's own neighbour contexts for this very transform block: the
    // census prices the search's estimate the way the search now takes it
    // (lane-av1skipctx), so a row's error is the table/adaptation drift that
    // is left once the context is right.
    skip_ctx: usize,
    sign_ctx: usize,
) {
    if CENSUS.with_borrow(|c| c.is_none()) {
        return;
    }
    let nz = grid.iter().filter(|&&l| l != 0).count();
    let bucket = match nz {
        0 => 0,
        1 => 1,
        2 => 2,
        3..=4 => 3,
        5..=8 => 4,
        9..=16 => 5,
        _ => 6,
    };
    let priced = coeff_bits(grid, set, q_ctx, skip_ctx, sign_ctx);
    CENSUS.with_borrow_mut(|rows| {
        let rows = rows.as_mut().expect("the census is on");
        let row = rows.entry((format!("{set:?}"), bucket)).or_default();
        row.0 += 1;
        row.1 += priced;
        row.2 += written;
    });
}

/// What the partition symbol of a block of `side` samples costs, in bits, when
/// it says the block is split (or that it is not).
///
/// The price is taken at context zero — a block whose neighbours are no finer
/// than it is — because the search that reads it runs before the tile knows
/// what its neighbours will be.
pub(crate) fn partition_bits(side: usize, split: bool) -> f64 {
    // Read-only, and the same tables every call: built once (see
    // [`coeff_bits`] for why that matters here).
    pub(super) static CDFS: LazyLock<Cdfs> = LazyLock::new(|| Cdfs::new(2));
    let cdfs = &*CDFS;
    let cdf: &[u16] = match side {
        // lane-b128: the 128 superblock root's own eight-symbol alphabet.
        128 => &cdfs.partition_w128[0],
        SB => &cdfs.partition_w64[0],
        BLOCK => &cdfs.partition_w32[0],
        TX8 => &cdfs.partition_w8[0],
        _ => &cdfs.partition_w16[0],
    };
    let symbol = if split {
        PARTITION_SPLIT
    } else {
        PARTITION_NONE
    };
    crate::encode::symbol_bits(cdf, symbol)
}

/// lane-b128hv: [`partition_bits`] for any partition symbol, at the same
/// fixed (round-2) tables -- what the 128 root's HORZ/VERT candidate is priced
/// with (`PARTITION_HORZ` = 1, `PARTITION_VERT` = 2).
pub(crate) fn partition_bits_sym(side: usize, symbol: usize) -> f64 {
    pub(super) static CDFS: LazyLock<Cdfs> = LazyLock::new(|| Cdfs::new(2));
    let cdfs = &*CDFS;
    let cdf: &[u16] = match side {
        128 => &cdfs.partition_w128[0],
        SB => &cdfs.partition_w64[0],
        BLOCK => &cdfs.partition_w32[0],
        TX8 => &cdfs.partition_w8[0],
        _ => &cdfs.partition_w16[0],
    };
    crate::encode::symbol_bits(cdf, symbol)
}

/// coeffs() for one plane's transform block of any coefficients the base and
/// base-range syntax reach (spec 5.11.39).
///
/// A 16x16 luma transform codes which transform type it is, because its set
/// holds more than one; 32x32 and 64x64 are DCT-only by spec 5.11.40. The
/// levels the contexts below are read from are the levels of
/// coefficients later in the scan, which a decoder walking the scan backwards
/// already has.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_coeffs(
    enc: &mut SymbolEncoder,
    coding: &mut TxbTables,
    grid: &[i32],
    scan: &[u16],
    skip_ctx: usize,
    sign_ctx: usize,
    plane: Option<usize>,
    // Which transform type these levels were produced with (lane-txset).
    // Only a luma set whose alphabet holds more than one type codes it;
    // chroma never codes a `tx_type` symbol at all.
    tx_type: TxType,
) {
    if let Some(plane) = plane {
        ec_rng_trace(|| format!("EC_PLANE plane={plane} tell_before={}", enc.tell()));
    }
    let side = coding.side;
    // lane-txw ROOT CAUSE: this writer used to code every transform unit as
    // `TX_CLASS_2D` -- default zigzag scan, the 2D `eob_pt` table and the 2D
    // neighbour taps -- while the decoder resolves `V_DCT`/`H_DCT` to
    // `TX_CLASS_VERT`/`TX_CLASS_HORIZ` (`decode::TxClass::of`), which read
    // their own scan (`class_scan_table`), their own `eob_pt` row
    // (`eob_pt_class1`) and their own context offsets. So the first 1-D type
    // the search ever offered (the wider `reduced_tx_set == 0` alphabets)
    // desynced the tile from its own trial decode. The class is a property of
    // the type alone, so it is resolved here, once per unit, exactly as the
    // reader does it.
    let class = crate::decode::TxClass::of(tx_type);
    let scan: &[u16] = if class == crate::decode::TxClass::TwoD {
        scan
    } else {
        class_scan_of(side, class)
    };
    // The scan-order search reads the grid in scan order, one dependent load
    // per position, and an all-zero block makes it walk every one of them --
    // which the pricer does for candidate after candidate. A straight-line
    // pass settles that case first: it vectorises, and a block that codes
    // anything at all almost always codes its DC, so it stops at the first
    // element.
    let eob = if grid.iter().all(|&level| level == 0) {
        0
    } else {
        scan.iter()
            .rposition(|&pos| grid[pos as usize] != 0)
            .map_or(0, |i| i + 1)
    };
    enc.symbol(usize::from(eob == 0), &mut coding.txb_skip[skip_ctx]);
    if plane.is_some() {
        ec_rng_trace(|| {
            format!(
                "EC_TXBSKIP ctx={skip_ctx} eob0={} tell={} rng={}",
                usize::from(eob == 0),
                enc.tell(),
                enc.rng()
            )
        });
    }
    if eob == 0 {
        return;
    }
    // A luma transform whose type set holds more than one type codes which it
    // is, right after the all-zero flag (spec 5.11.39). The writer only ever
    // uses DCT_DCT, which is index one of `Tx_Type_Intra_Inv_Set2`.
    if let Some(cdf) = coding.tx_type.as_deref_mut() {
        // The symbol comes from the DECODER's own map, keyed by this set's
        // alphabet width, so a type this set cannot express is caught here
        // rather than desyncing the tile. The search only ever offers types
        // the set holds ([`crate::encode::tx_type_candidates`]).
        let symbol = crate::decode::tx_type_symbol(cdf.len(), tx_type);
        debug_assert!(
            symbol.is_some(),
            "{tx_type:?} is not in the {}-symbol tx_type set",
            cdf.len() - 1
        );
        enc.symbol(symbol.unwrap_or(TX_TYPE_DCT_DCT_SET2), cdf);
        if plane.is_some() {
            ec_rng_trace(|| format!("EC_TXTYPE tell={} rng={}", enc.tell(), enc.rng()));
        }
    }

    write_eob(enc, coding, eob, plane, class);

    // lane-av1speed3: `side` is 4, 8, 16 or 32, but it is a runtime value --
    // `pos / side` compiles to a real integer division, once per coefficient,
    // in the encoder's largest self-time symbol. Same row and column.
    debug_assert!(
        side.is_power_of_two(),
        "the scan splits a position by shifting"
    );
    let (shift, mask) = (side.trailing_zeros(), side - 1);
    for scan_idx in (0..eob).rev() {
        let pos = scan[scan_idx] as usize;
        let (row, col) = (pos >> shift, pos & mask);
        let level = grid[pos].abs();
        if scan_idx == eob - 1 {
            let ctx = eob_coeff_ctx(scan_idx, side * side);
            let sym = (level.min(NUM_BASE_LEVELS + 1) - 1) as usize;
            enc.symbol(sym, &mut coding.base_eob[ctx]);
            if plane.is_some() {
                ec_rng_trace(|| {
                    format!(
                        "EC_BASEEOB scan_idx={scan_idx} ctx={ctx} level={} tell={}",
                        sym + 1,
                        enc.tell()
                    )
                });
            }
        } else {
            let ctx = base_ctx(grid, side, row, col, class);
            let sym = level.min(NUM_BASE_LEVELS + 1) as usize;
            enc.symbol(sym, &mut coding.base[ctx]);
            if plane.is_some() {
                ec_rng_trace(|| {
                    format!(
                        "EC_BASE scan_idx={scan_idx} ctx={ctx} level={sym} tell={}",
                        enc.tell()
                    )
                });
            }
        }
        if level > NUM_BASE_LEVELS {
            let ctx = br_ctx(grid, side, row, col, class);
            let mut remaining = level - (NUM_BASE_LEVELS + 1);
            let mut sent = 0;
            while sent < COEFF_BASE_RANGE {
                let k = remaining.min(BR_STEP);
                enc.symbol(k as usize, &mut coding.br[ctx]);
                if plane.is_some() {
                    ec_rng_trace(|| {
                        format!(
                            "EC_BR scan_idx={scan_idx} ctx={ctx} k={k} tell={}",
                            enc.tell()
                        )
                    });
                }
                if k < BR_STEP {
                    break;
                }
                remaining -= k;
                sent += BR_STEP;
            }
        }
    }

    // The signs come after the levels, in scan order, the DC's from a CDF and
    // the rest as raw bits (spec 5.11.39).
    for &pos in &scan[..eob] {
        let level = grid[pos as usize];
        if level == 0 {
            continue;
        }
        if pos == 0 {
            enc.symbol(usize::from(level < 0), &mut coding.dc_sign[sign_ctx]);
            if plane.is_some() {
                ec_rng_trace(|| {
                    format!(
                        "EC_DCSIGN ctx={sign_ctx} neg={} tell={}",
                        usize::from(level < 0),
                        enc.tell()
                    )
                });
            }
        } else {
            enc.literal(u32::from(level < 0), 1);
        }
        // A level the base and base-range syntax cannot reach carries the rest
        // of itself here, after its own sign (spec 5.11.39).
        if level.abs() > MAX_BR_LEVEL {
            write_golomb(enc, (level.abs() - MAX_BR_LEVEL - 1) as u32);
            if plane.is_some() {
                ec_rng_trace(|| format!("EC_GOLOMB tell={}", enc.tell()));
            }
        }
    }
    if let Some(plane) = plane {
        ec_rng_trace(|| format!("EC_PLANE plane={plane} eob={eob} tell_after={}", enc.tell()));
    }
}

/// The end-of-block position (spec 5.11.39): which group of scan positions the
/// last coded coefficient falls in, then its offset inside that group — the
/// offset's top bit from a CDF and the rest as raw bits.
pub(super) fn write_eob(
    enc: &mut SymbolEncoder,
    coding: &mut TxbTables,
    eob: usize,
    plane: Option<usize>,
    class: crate::decode::TxClass,
) {
    /// `Eob_Group_Start` (spec 5.11.39): the first scan position each group of
    /// end-of-block positions covers, indexed by the group's own number.
    const GROUP_START: [usize; 12] = [0, 1, 2, 3, 5, 9, 17, 33, 65, 129, 257, 513];
    /// `Eob_Offset_Bits` (spec 5.11.39): how wide each group's offset is.
    const OFFSET_BITS: [u32; 12] = [0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9];

    let group = GROUP_START
        .iter()
        .rposition(|&start| start <= eob)
        .expect("the first group starts at zero");
    // The groups are numbered from one, and how many of them the transform
    // reaches is the size of its own end-of-block alphabet. A
    // `TX_CLASS_HORIZ`/`TX_CLASS_VERT` unit adapts a wholly separate row
    // (libaom's `eob_multi_ctx`), which is what `decode::read_eob` reads.
    let eob_pt: &mut [u16] = match (class, coding.eob_pt_class1.as_deref_mut()) {
        (crate::decode::TxClass::TwoD, _) | (_, None) => coding.eob_pt,
        (_, Some(class1)) => class1,
    };
    enc.symbol(group - 1, eob_pt);
    if let Some(plane) = plane {
        ec_rng_trace(|| {
            format!(
                "EC_EOBPT plane={plane} eob_pt={group} tell={} rng={}",
                enc.tell(),
                enc.rng()
            )
        });
    }

    let bits = OFFSET_BITS[group];
    if bits > 0 {
        let offset = (eob - GROUP_START[group]) as u32;
        let top = (offset >> (bits - 1)) & 1;
        enc.symbol(top as usize, &mut coding.eob_extra[group - 3]);
        if let Some(plane) = plane {
            ec_rng_trace(|| {
                format!(
                    "EC_EOBEXTRA plane={plane} eob_ctx={} top={top} tell={}",
                    group - 3,
                    enc.tell()
                )
            });
        }
        if bits > 1 {
            enc.literal(offset & ((1 << (bits - 1)) - 1), bits - 1);
            if let Some(plane) = plane {
                ec_rng_trace(|| {
                    format!(
                        "EC_EOBBITS plane={plane} eob_extra={offset} tell={}",
                        enc.tell()
                    )
                });
            }
        }
    }
}

/// Writes the remainder of a level the base and base-range syntax could not
/// reach (spec 5.11.40): the value plus one, as its bit length in unary
/// followed by the value's own bits, most significant first.
pub(crate) fn write_golomb(enc: &mut SymbolEncoder, value: u32) {
    let x = value + 1;
    let length = 32 - x.leading_zeros();
    for _ in 0..length - 1 {
        enc.literal(0, 1);
    }
    for i in (0..length).rev() {
        enc.literal((x >> i) & 1, 1);
    }
}

/// The context of the level of the last coded coefficient (spec 8.3.2): how
/// far into the scan it sits, in quarters and eighths of the transform.
pub(super) fn eob_coeff_ctx(scan_idx: usize, area: usize) -> usize {
    match scan_idx {
        0 => 0,
        i if i <= area / 8 => 1,
        i if i <= area / 4 => 2,
        _ => 3,
    }
}

/// The context of a coefficient's level (spec 8.3.2): the magnitudes of the
/// five neighbours below and to the right of it — the ones a decoder walking
/// the scan backwards already has — plus a term for where in the transform it
/// sits. The DC reads context 0 whatever its neighbours carry.
pub(super) fn base_ctx(
    grid: &[i32],
    side: usize,
    row: usize,
    col: usize,
    class: crate::decode::TxClass,
) -> usize {
    use crate::decode::TxClass;
    // Only `TX_CLASS_2D` short-circuits its DC to context 0
    // (`get_nz_map_ctx_from_stats`'s `(tx_class | coeff_idx) == 0`); a 1-D
    // class runs the DC through the same neighbour sum as every other
    // position.
    if class == TxClass::TwoD && row == 0 && col == 0 {
        return 0;
    }
    if class != TxClass::TwoD {
        let offsets: [(usize, usize); 5] = if class == TxClass::Horiz {
            [(1, 0), (0, 1), (0, 2), (0, 3), (0, 4)]
        } else {
            [(1, 0), (0, 1), (2, 0), (3, 0), (4, 0)]
        };
        let mag: i32 = offsets
            .iter()
            .map(|&(dr, dc)| neighbour(grid, side, row + dr, col + dc).abs().min(3))
            .sum();
        let along = if class == TxClass::Horiz { col } else { row };
        return (((mag + 1) >> 1).min(4) as usize)
            + crate::decode::nz_map_ctx_offset_1d(along.min(31));
    }
    // A coefficient two rows and two columns clear of the far edges reaches
    // all five neighbours, so the gather is five loads off one index with no
    // per-neighbour edge test -- which is where this writer spends a third of
    // its coefficient loop.
    let p = row * side + col;
    let mag: i32 = if row + 2 < side && col + 2 < side {
        [p + side, p + 1, p + side + 1, p + 2 * side, p + 2]
            .iter()
            .map(|&i| grid[i].abs().min(3))
            .sum()
    } else {
        [(1, 0), (0, 1), (1, 1), (2, 0), (0, 2)]
            .iter()
            .map(|&(dr, dc)| neighbour(grid, side, row + dr, col + dc).abs().min(3))
            .sum()
    };
    let offset = cdf::NZ_MAP_CTX_OFFSET_32[row.min(4)][col.min(4)] as usize;
    (((mag + 1) >> 1).min(4) as usize) + offset
}

/// The context of a coefficient's base-range tail (spec 8.3.2): the magnitudes
/// of its three closest neighbours below and to the right, uncapped, and a
/// term separating the DC, the corner of the transform, and the rest.
pub(super) fn br_ctx(
    grid: &[i32],
    side: usize,
    row: usize,
    col: usize,
    class: crate::decode::TxClass,
) -> usize {
    use crate::decode::TxClass;
    if class != TxClass::TwoD {
        let extra = if class == TxClass::Horiz {
            neighbour(grid, side, row, col + 2)
        } else {
            neighbour(grid, side, row + 2, col)
        };
        let mag = neighbour(grid, side, row + 1, col).abs()
            + neighbour(grid, side, row, col + 1).abs()
            + extra.abs();
        let mag = (((mag + 1) >> 1).min(6)) as usize;
        if row == 0 && col == 0 {
            return mag;
        }
        let near_origin = if class == TxClass::Horiz {
            col == 0
        } else {
            row == 0
        };
        return if near_origin { mag + 7 } else { mag + 14 };
    }
    let p = row * side + col;
    let mag: i32 = if row + 1 < side && col + 1 < side {
        grid[p + side].abs() + grid[p + 1].abs() + grid[p + side + 1].abs()
    } else {
        [(1, 0), (0, 1), (1, 1)]
            .iter()
            .map(|&(dr, dc)| neighbour(grid, side, row + dr, col + dc).abs())
            .sum()
    };
    let mag = (((mag + 1) >> 1).min(6)) as usize;
    if row == 0 && col == 0 {
        mag
    } else if row < 2 && col < 2 {
        mag + 7
    } else {
        mag + 14
    }
}

/// A level at a position that may fall off the transform, where the levels a
/// context reads are zero.
pub(super) fn neighbour(grid: &[i32], side: usize, row: usize, col: usize) -> i32 {
    if row >= side || col >= side {
        0
    } else {
        grid[row * side + col]
    }
}

/// Shared by both DC writers: one level per block, none of them zero.
pub(super) fn check_levels(levels: &[i32], blocks: usize) -> Result<()> {
    if levels.len() != blocks {
        return Err(Error::unsupported(
            "AV1 tile",
            "a DC-only key frame needs one level per coded block",
        ));
    }
    if levels.iter().any(|&l| l == 0 || l.abs() > MAX_BR_LEVEL) {
        return Err(Error::unsupported(
            "AV1 tile",
            "a DC-only key frame is written for levels -14..=14 without zero; \
             wider levels need the Golomb tail",
        ));
    }
    Ok(())
}

/// coeffs() for a luma transform block whose only coefficient is the DC (spec
/// 5.11.39). The block size is the transform size, so the all-zero flag's
/// context is 0; an end-of-block of one is token 0 of the position alphabet
/// with no extra bits; the transform sizes here are all DCT-only, so no
/// transform type is coded; and the DC's own neighbours are zero, so its
/// magnitude contexts are 0 throughout.
pub(super) fn write_dc_coeffs(
    enc: &mut SymbolEncoder,
    dc_level: i32,
    sign_ctx: usize,
    q_ctx: usize,
    txb_skip: &[u16],
    base_eob: &[u16],
) {
    use crate::cdf_state::pick;
    let level = dc_level.abs();
    let eob_pt = pick(
        q_ctx,
        cdf::EOB_PT_1024_LUMA_Q0,
        cdf::EOB_PT_1024_LUMA_Q1,
        cdf::EOB_PT_1024_LUMA,
        cdf::EOB_PT_1024_LUMA_Q3,
    );
    let br = pick(
        q_ctx,
        cdf::COEFF_BR_LUMA_32_Q0,
        cdf::COEFF_BR_LUMA_32_Q1,
        cdf::COEFF_BR_LUMA_32,
        cdf::COEFF_BR_LUMA_32_Q3,
    );
    enc.symbol_fixed(0, txb_skip);
    enc.symbol_fixed(0, &eob_pt);
    enc.symbol_fixed((level.min(NUM_BASE_LEVELS + 1) - 1) as usize, base_eob);
    if level > NUM_BASE_LEVELS {
        let mut remaining = level - (NUM_BASE_LEVELS + 1);
        let mut sent = 0;
        while sent < COEFF_BASE_RANGE {
            let k = remaining.min(BR_STEP);
            enc.symbol_fixed(k as usize, &br[0]);
            if k < BR_STEP {
                break;
            }
            remaining -= k;
            sent += BR_STEP;
        }
    }
    // The signs come after the levels, DC first (spec 5.11.39).
    enc.symbol_fixed(usize::from(dc_level < 0), &cdf::DC_SIGN_LUMA[sign_ctx]);
}

/// One 4x4 unit's vote in `Dc_Sign_Contexts` (spec 8.3.2): plus one for a
/// positive DC, minus one for a negative one, nothing for a unit whose block
/// carried no DC or that sits past the frame's true edge (spec
/// `av1_set_entropy_contexts`), which is why the vote is gathered per 4x4
/// unit and not per coded cell -- a unit past the edge does not vote even
/// when the rest of its cell does.
pub(super) fn dc_vote(dc: Option<bool>) -> i32 {
    match dc {
        None => 0,
        Some(true) => -1,
        Some(false) => 1,
    }
}

/// Which of the three DC sign contexts a gathered vote picks.
pub(super) fn dc_sign_ctx(vote: i32) -> usize {
    match vote.signum() {
        0 => 0,
        -1 => 1,
        _ => 2,
    }
}

/// The `is_inter` context (spec 5.11.16 via `av1_get_intra_inter_context`,
/// `pred_common.c`): both neighbours' intra/inter state when both are
/// available, one neighbour's when only one is, and zero at a tile's own
/// top-left corner.
pub(crate) fn intra_inter_ctx(
    has_above: bool,
    has_left: bool,
    above_inter: bool,
    left_inter: bool,
) -> usize {
    match (has_above, has_left) {
        (true, true) => {
            let (above_intra, left_intra) = (!above_inter, !left_inter);
            if above_intra && left_intra {
                3
            } else {
                usize::from(above_intra || left_intra)
            }
        }
        (true, false) => 2 * usize::from(!above_inter),
        (false, true) => 2 * usize::from(!left_inter),
        (false, false) => 0,
    }
}

/// `CLASS0_SIZE << (class + 2)` (spec 3), the magnitude an `MV_CLASS_n`
/// component's own bits start counting from; class zero starts at zero.
pub(super) fn mv_class_base(class: usize) -> i32 {
    if class == 0 { 0 } else { 2i32 << (class + 2) }
}

/// The class a pre-offset magnitude `z` (`|diff| - 1`) falls in — the inverse
/// of `mv_class_base`'s doubling ranges, ported from libaom's
/// `av1_get_mv_class` (`mv.h`).
pub(super) fn mv_class_of(z: i32) -> usize {
    let mut class = 0;
    while class < 10 && mv_class_base(class + 1) <= z {
        class += 1;
    }
    class
}

/// Writes one motion vector component's non-zero diff (spec 5.11.32
/// `read_mv_component`, run backwards): sign, class, then the class's own
/// bits, and (lane-hpmv) the eighth-pel bit when the frame armed through
/// [`arm_high_precision_mv`] carries `allow_high_precision_mv`. Without that
/// flag the eighth-pel bit is inferred as one rather than coded, so only
/// diffs whose eighth-pel bit really is one are representable; anything else
/// is refused rather than rounded, since rounding would silently code a
/// different vector than the caller asked for.
///
/// # Errors
/// Returns an error when `diff` needs eighth-pel precision in a frame coded
/// without `allow_high_precision_mv`.
pub(super) fn write_mv_component(enc: &mut SymbolEncoder, c: &mut MvComponentCdfs, diff: i32) -> Result<()> {
    debug_assert_ne!(diff, 0);
    let hp_frame = high_precision_mv();
    let sign = diff < 0;
    enc.symbol(usize::from(sign), &mut c.sign);
    let mag = diff.unsigned_abs() as i32;
    let z = mag - 1;
    if !hp_frame && z & 1 == 0 {
        return Err(Error::unsupported(
            "AV1 tile",
            "a motion vector component needs eighth-pel precision, which \
             allow_high_precision_mv off does not carry",
        ));
    }
    let class = mv_class_of(z);
    enc.symbol(class, &mut c.class);
    let local = z - mv_class_base(class);
    let hp = (local & 1) as usize;
    if class == 0 {
        let bit = (local >> 3) & 1;
        let fr = (local >> 1) & 3;
        enc.symbol(bit as usize, &mut c.class0_bit);
        enc.symbol(fr as usize, &mut c.class0_fr[bit as usize]);
        // spec 5.11.32: the eighth-pel bit is CODED when the frame allows
        // high-precision motion vectors, and inferred as one otherwise ("if
        // (allow_high_precision_mv) mv_class0_hp ... else mv_class0_hp = 1").
        if hp_frame {
            enc.symbol(hp, &mut c.class0_hp);
        }
    } else {
        let d = local >> 3;
        let fr = (local >> 1) & 3;
        for i in 0..class {
            enc.symbol(((d >> i) & 1) as usize, &mut c.bit[i]);
        }
        enc.symbol(fr as usize, &mut c.fr);
        if hp_frame {
            enc.symbol(hp, &mut c.hp);
        }
    }
    if hp_frame {
        HP_SYMBOLS[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if hp == 0 {
            HP_SYMBOLS[1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    Ok(())
}

/// Writes a motion vector as a residual against `pred` (spec 5.11.32
/// `read_mv`): the joint symbol naming which components differ, then each
/// differing component.
pub(super) fn write_mv(
    enc: &mut SymbolEncoder,
    mv_comp: &mut [MvComponentCdfs; 2],
    mv_joint: &mut [u16; 5],
    mv: (i32, i32),
    pred: (i32, i32),
) -> Result<()> {
    let diff = (mv.0 - pred.0, mv.1 - pred.1);
    let joint = match (diff.0 != 0, diff.1 != 0) {
        (false, false) => 0, // MV_JOINT_ZERO
        (false, true) => 1,  // MV_JOINT_HNZVZ: column only
        (true, false) => 2,  // MV_JOINT_HZVNZ: row only
        (true, true) => 3,   // MV_JOINT_HNZVNZ
    };
    enc.symbol(joint, mv_joint);
    if diff.0 != 0 {
        write_mv_component(enc, &mut mv_comp[0], diff.0)?;
    }
    if diff.1 != 0 {
        write_mv_component(enc, &mut mv_comp[1], diff.1)?;
    }
    Ok(())
}

/// What a block leaves in the neighbour reference bands: its own reference,
/// or `-1` when it is intra (decode.rs `Neighbours::above_ref`'s convention).
pub(super) fn block_ref(block: &BlockCoeffs) -> i8 {
    block.inter.map_or(-1, |i| i.ref_frame)
}

/// Writer-side counterpart of decode.rs `read_single_ref` (spec 5.11.25's
/// `single_ref_p1`..`p6` tree): codes `ref_frame` bit by bit, each symbol at
/// the context its own `single_ref_p*_ctx` derives from the immediate
/// above/left neighbours' references -- the same functions the decoder calls,
/// with `-1` standing for an intra or unavailable neighbour and no second
/// reference (this writer codes no compound block).
pub(super) fn write_single_ref(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    ref_frame: i8,
    above_ref: i8,
    above_ref1: Option<i8>,
    left_ref: i8,
    left_ref1: Option<i8>,
) {
    REF_HITS[(ref_frame.max(1) - 1) as usize % 7]
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    use crate::mvstack::{
        ALTREF_FRAME, ALTREF2_FRAME, BWDREF_FRAME, GOLDEN_FRAME, LAST2_FRAME, LAST3_FRAME,
        single_ref_p1_ctx, single_ref_p2_ctx, single_ref_p3_ctx, single_ref_p4_ctx,
        single_ref_p5_ctx, single_ref_p6_ctx,
    };
    let above = (above_ref > 0).then_some(above_ref);
    let left = (left_ref > 0).then_some(left_ref);
    // libaom `av1_collect_neighbors_ref_counts` counts BOTH of a compound
    // neighbour's references (decode.rs `read_single_ref`'s own
    // `above_ref1`/`left_ref1`): dropping the second one reads a different
    // context row for every block sitting under a compound neighbour, and so
    // decodes a different reference NAME.
    let backward = ref_frame >= BWDREF_FRAME;
    enc.symbol(
        usize::from(backward),
        &mut cdfs.single_ref[single_ref_p1_ctx(above, above_ref1, left, left_ref1)][0],
    );
    if backward {
        let is_altref = ref_frame == ALTREF_FRAME;
        enc.symbol(
            usize::from(is_altref),
            &mut cdfs.single_ref[single_ref_p2_ctx(above, above_ref1, left, left_ref1)][1],
        );
        if !is_altref {
            enc.symbol(
                usize::from(ref_frame == ALTREF2_FRAME),
                &mut cdfs.single_ref[single_ref_p6_ctx(above, above_ref1, left, left_ref1)][5],
            );
        }
        return;
    }
    let far = ref_frame == LAST3_FRAME || ref_frame == GOLDEN_FRAME;
    enc.symbol(
        usize::from(far),
        &mut cdfs.single_ref[single_ref_p3_ctx(above, above_ref1, left, left_ref1)][2],
    );
    if far {
        enc.symbol(
            usize::from(ref_frame == GOLDEN_FRAME),
            &mut cdfs.single_ref[single_ref_p5_ctx(above, above_ref1, left, left_ref1)][4],
        );
    } else {
        enc.symbol(
            usize::from(ref_frame == LAST2_FRAME),
            &mut cdfs.single_ref[single_ref_p4_ctx(above, above_ref1, left, left_ref1)][3],
        );
    }
}

/// Per-mode fire counts of [`write_inter_mode`], indexed
/// `NEARESTMV`/`NEARMV`/`GLOBALMV`/`NEWMV`, and the DRL index each block
/// coded (bucket 3 collects any index past 2). Read by the BD gate, which
/// is the only thing that proves a newly added mode ever fires
/// (gate-blind-to-feature).
pub(super) static INTER_MODE_HITS: [std::sync::atomic::AtomicUsize; 4] = [
    std::sync::atomic::AtomicUsize::new(0),
    std::sync::atomic::AtomicUsize::new(0),
    std::sync::atomic::AtomicUsize::new(0),
    std::sync::atomic::AtomicUsize::new(0),
];
pub(super) static DRL_HITS: [std::sync::atomic::AtomicUsize; 4] = [
    std::sync::atomic::AtomicUsize::new(0),
    std::sync::atomic::AtomicUsize::new(0),
    std::sync::atomic::AtomicUsize::new(0),
    std::sync::atomic::AtomicUsize::new(0),
];

/// One counter per reference name (`LAST`..`ALTREF`), for the gate's own
/// census of what the encoder actually picks.
pub(super) static REF_HITS: [std::sync::atomic::AtomicUsize; 7] = [
    std::sync::atomic::AtomicUsize::new(0),
    std::sync::atomic::AtomicUsize::new(0),
    std::sync::atomic::AtomicUsize::new(0),
    std::sync::atomic::AtomicUsize::new(0),
    std::sync::atomic::AtomicUsize::new(0),
    std::sync::atomic::AtomicUsize::new(0),
    std::sync::atomic::AtomicUsize::new(0),
];

/// Takes and clears the per-reference histogram.
#[cfg(test)]
pub(crate) fn take_ref_hits() -> [usize; 7] {
    std::array::from_fn(|i| REF_HITS[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// Bumps the two histograms for one coded inter block.
/// How many blocks this process wrote whose search-chosen `ref_mv_idx` the
/// write-time stack could not carry, so the writer signalled a SMALLER index
/// (lane-sb128b). Every one of these used to price its residual against a
/// predictor the decoder never derives.
pub(super) static DRL_CLAMP_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Counts one clamped DRL index whose predictor differs from the one the
/// search asked for -- the harmful case, and the only one a witness can see.
pub(super) fn note_drl_clamp(target: usize, signalled: usize, entries: usize) {
    DRL_CLAMP_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if crate::envflags::env_flag!("EC_AV1_TRACE") {
        eprintln!("DRL_CLAMP target={target} signalled={signalled} entries={entries}");
    }
}

/// Reads [`DRL_CLAMP_HITS`] -- the tripwire for the DRL owner's fix. It is
/// provably zero while `every_drl_index_the_new_mv_pricer_offers_is_one_the_writer_can_signal`
/// (encode.rs) holds: the pricer never offers an index this walk cannot signal,
/// so no clip can move it (lane-av1recipehunt, 153 attempts, plus a mutation
/// proof that widening the pricer's offer set does). The clip gate that used
/// to wait on it is deleted; this counter stays so that widening the pricer
/// is loud instead of silent. `EC_AV1_TRACE=1` prints each hit.
pub fn drl_clamp_hits() -> usize {
    DRL_CLAMP_HITS.load(std::sync::atomic::Ordering::Relaxed)
}

pub(super) fn note_inter_mode(mode: usize, drl: usize) {
    INTER_MODE_HITS[mode].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    DRL_HITS[drl.min(3)].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Takes and clears the inter-mode histogram (NEAREST/NEAR/GLOBAL/NEW).
#[cfg(test)]
pub(crate) fn take_inter_mode_hits() -> [usize; 4] {
    [0, 1, 2, 3].map(|i| INTER_MODE_HITS[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// Takes and clears the DRL-index histogram (0, 1, 2, 3+).
#[cfg(test)]
pub(crate) fn take_drl_hits() -> [usize; 4] {
    [0, 1, 2, 3].map(|i| DRL_HITS[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// Writes the DRL index of one block: the loop the decoder runs (spec
/// 5.11.24 `read_drl_idx`, decode.rs' two copies at ~26655 and ~26695), one
/// `drl_mode` bit per stack entry past `start`, `1` to advance and `0` to
/// stop, at most two bits. `drl_ctx[idx]` is the context between
/// `entries[idx]` and `entries[idx + 1]`, exactly the pair the bit chooses
/// between. Returns the index the symbols actually SIGNAL, which is `target`
/// clamped by the stack (`min(target, start + 2, entries.len() - 1)`): the
/// decoder derives its predictor from THAT index, so every caller must take
/// its own base MV from the returned value and not from `target` (lane-sb128b:
/// a search-chosen `ref_mv_idx` of 2 against a two-entry write-time stack
/// signalled index 1 and priced the residual against `pred_mv`, and the
/// decoder's MV silently drifted a whole frame before the drl symbols
/// desynced).
/// The DRL index [`write_drl_idx`]'s walk can actually SIGNAL against a stack
/// of `entries` candidates: at most two `drl_mode` symbols past `start`, and
/// each one needs a further entry to advance into (spec 5.11.24's
/// `for (idx = 0; idx < 2; idx++) if (NumMvFound > idx + 1)`).
///
/// A search that offers an index this clamps prices its residual against a
/// predictor the decoder never derives (lane-sb128b's silent MV drift); the
/// pricer's own offer set is enumerated against this rule by
/// `crate::encode::tests::every_drl_index_the_new_mv_pricer_offers_is_one_the_writer_can_signal`.
pub(crate) fn signalled_drl_idx(entries: usize, start: usize, target: usize) -> usize {
    target
        .min(start + 2)
        .min(entries.saturating_sub(1))
        .max(start)
}

pub(super) fn write_drl_idx(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    stack: &crate::mvstack::MvStack,
    start: usize,
    target: usize,
) -> usize {
    debug_assert_eq!(
        signalled_drl_idx(stack.entries.len(), start, target),
        {
            let mut i = start;
            while i < start + 2 && stack.entries.len() > i + 1 && i < target {
                i += 1;
            }
            i
        },
        "the drl clamp rule and the writer's own walk disagree",
    );
    let mut idx = start;
    while idx < start + 2 && stack.entries.len() > idx + 1 {
        let advance = idx < target;
        enc.symbol(usize::from(advance), &mut cdfs.drl_mode[stack.drl_ctx[idx]]);
        if !advance {
            return idx;
        }
        idx += 1;
    }
    idx
}

/// Writer-side counterpart of the decoder's single-reference
/// `read_inter_mode`/`assign_mv` chain (decode.rs ~26645): writes `info`'s
/// mode symbols against `stack`'s own contexts and, for `NEWMV`, the MV
/// residual against the DRL-selected predictor. Returns the motion vector
/// the decoder will derive for the block (what the MI grid must record) and
/// whether it is `NEWMV`.
///
/// `GLOBALMV` resolves to `(0, 0)` because this encoder writes no global
/// motion parameters at all, so every `gm_get_motion_vector` the decoder
/// runs is the identity model's zero vector; a lane that starts writing
/// real `gm_params` has to feed the same vector in here.
///
/// # Errors
/// As [`write_mv`]: a `NEWMV` residual needing eighth-pel precision.
pub(super) fn write_inter_mode(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    info: InterInfo,
    stack: &crate::mvstack::MvStack,
) -> Result<((i32, i32), bool)> {
    let idx = usize::from(info.ref_mv_idx);
    if info.mode.compound_index().is_some() {
        return Err(Error::unsupported(
            "AV1 tile",
            "a compound mode is written by `write_compound_block`, not here",
        ));
    }
    match info.mode {
        InterMode::NewMv => {
            enc.symbol(0, &mut cdfs.new_mv[stack.new_mv_ctx]);
            let signalled = write_drl_idx(enc, cdfs, stack, 0, idx);
            let base = stack.entries.get(signalled).map_or(stack.pred_mv, |e| e.mv);
            if signalled != idx && base != stack.entries.get(idx).map_or(stack.pred_mv, |e| e.mv) {
                note_drl_clamp(idx, signalled, stack.entries.len());
            }
            let idx = signalled;
            write_mv(enc, &mut cdfs.mv_comp, &mut cdfs.mv_joint, info.mv, base)?;
            note_inter_mode(3, idx);
            Ok((info.mv, true))
        }
        InterMode::GlobalMv => {
            enc.symbol(1, &mut cdfs.new_mv[stack.new_mv_ctx]); // not NEWMV
            enc.symbol(0, &mut cdfs.zero_mv[stack.zero_mv_ctx]); // GLOBALMV
            note_inter_mode(2, 0);
            Ok(((0, 0), false))
        }
        InterMode::NearestMv => {
            enc.symbol(1, &mut cdfs.new_mv[stack.new_mv_ctx]); // not NEWMV
            enc.symbol(1, &mut cdfs.zero_mv[stack.zero_mv_ctx]); // not GLOBALMV
            enc.symbol(0, &mut cdfs.ref_mv[stack.ref_mv_ctx]); // NEARESTMV
            note_inter_mode(0, 0);
            Ok((stack.nearest_mv, false))
        }
        InterMode::NearMv => {
            enc.symbol(1, &mut cdfs.new_mv[stack.new_mv_ctx]); // not NEWMV
            enc.symbol(1, &mut cdfs.zero_mv[stack.zero_mv_ctx]); // not GLOBALMV
            enc.symbol(1, &mut cdfs.ref_mv[stack.ref_mv_ctx]); // NEARMV
            // `RefMvIdx` starts at 1 for NEARMV (spec 5.11.24).
            let idx = idx.max(1);
            let signalled = write_drl_idx(enc, cdfs, stack, 1, idx);
            let mv = stack.entries.get(signalled).map_or(stack.near_mv, |e| e.mv);
            if signalled != idx && mv != stack.entries.get(idx).map_or(stack.near_mv, |e| e.mv) {
                note_drl_clamp(idx, signalled, stack.entries.len());
            }
            let idx = signalled;
            note_inter_mode(1, idx);
            Ok((mv, false))
        }
        _ => unreachable!("compound modes are refused above"),
    }
}

/// Writes the payload of a one-tile inter frame built from `blocks`, one
/// 32x32 block per entry in raster order across the frame — every superblock
/// is split into its four quadrants the way [`split_coeff_key_frame_tile`]
/// codes a key frame, because a mixed-size partition tree buys this writer's
/// gate nothing a flat grid does not already prove: the CDF state, the
/// neighbour contexts and the single-reference/MV-stack machinery below are
/// exactly as size-sensitive whether the tree recurses or not, and recursing
/// it would be undirected scaffolding — see `sb_coeff_key_frame_tile` for
/// where that tree already lives, ready to graft this block's mode reads
/// onto once a real partition search needs it.
///
/// A block whose [`BlockCoeffs::inter`] is `Some` is coded inter: `skip`,
/// then `is_inter`, then a single-reference chain that always names `LAST`
/// (spec 5.11.25 `single_ref_p1`/`p3`/`p4`, this crate's only reference so
/// far), then the two-mode `read_inter_mode` chain (spec 5.11.24) and, for
/// [`InterMode::NewMv`], a DRL index (always zero) and a coded motion vector
/// residual. A block whose `inter` is `None` codes intra, through the
/// *inter*-frame intra path (spec 5.11.16's `intra_frame_mode_info` is the
/// key frame writers' path; this is `inter_frame_mode_info`'s intra branch):
/// `Y_MODE` by size group rather than `KF_Y_MODE` by neighbour context, since
/// an inter frame's intra blocks do not read their neighbours' modes.
///
/// # Errors
/// Returns an error when the frame is not a whole number of 64x64
/// superblocks, when `blocks` does not carry exactly one entry per 32x32
/// block, when an intra block names a mode a key frame does not code, when a
/// coefficient sits outside its transform or is one the writer cannot code,
/// when a `NEWMV` block's motion vector needs eighth-pel precision.
pub fn sb_coeff_inter_frame_tile(
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    blocks: &[Quadrant],
) -> Result<Vec<u8>> {
    sb_coeff_inter_frame_tile_tx(mi_cols, mi_rows, base_q_idx, blocks, false)
}

/// [`sb_coeff_inter_frame_tile`] for a frame header carrying
/// `tx_mode == TxMode::Select`: every block then codes the transform syntax
/// [`write_tx_syntax_inter`] writes -- one `txfm_split` flag on an inter
/// block that carries a residual, a `tx_depth` symbol on an intra one -- and
/// an intra block whose depth is nonzero codes its luma as several transform
/// units. With `tx_select` false this writes exactly the stream it always did.
///
/// # Errors
/// As [`sb_coeff_inter_frame_tile`].
pub fn sb_coeff_inter_frame_tile_tx(
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    blocks: &[Quadrant],
    tx_select: bool,
) -> Result<Vec<u8>> {
    let mut cdfs = Cdfs::new(q_ctx_of(base_q_idx));
    sb_coeff_inter_frame_tile_cdfs(
        mi_cols,
        mi_rows,
        base_q_idx,
        blocks,
        tx_select,
        &mut cdfs,
        TileRect::whole(mi_cols, mi_rows),
        &mut MiGrid::new(mi_cols as usize, mi_rows as usize),
    )
}

/// [`sb_coeff_inter_frame_tile_tx`] starting from -- and leaving behind --
/// the caller's own CDF state; see [`sb_coeff_key_frame_tile_cdfs`].
///
/// # Errors
/// As [`sb_coeff_inter_frame_tile_tx`].
// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
pub(crate) fn sb_coeff_inter_frame_tile_cdfs(
    mi_cols: u32,
    mi_rows: u32,
    _base_q_idx: u8,
    blocks: &[Quadrant],
    tx_select: bool,
    cdfs: &mut Cdfs,
    // This tile's own span; see [`sb_coeff_key_frame_tile_cdfs`].
    tile: TileRect,
    // The frame's MV grid, carried across the frame's tiles by the caller
    // (see the comment at `set_tile_bounds` below).
    grid: &mut MiGrid,
) -> Result<Vec<u8>> {
    // Consumes whatever `arm_cdef_idx`/`arm_lr` armed, on every exit path.
    let _cdef_idx = CdefIdxGuard;
    check_blocks(mi_cols, mi_rows)?;
    // `block_grid`'s ceiling, not a plain division: a true frame size that is
    // not a whole number of 32x32 blocks (or of 64x64 superblocks) still has
    // one more block/superblock whose own origin is inside the true frame,
    // same as `sb_coeff_key_frame_tile`.
    let (cols, rows) = block_grid(mi_cols, mi_rows);
    if blocks.len() != (cols * rows) as usize {
        return Err(Error::unsupported(
            "AV1 tile",
            "an inter frame needs one entry per 32x32 block inside the true frame",
        ));
    }
    let coded: Vec<&BlockCoeffs> = blocks.iter().flat_map(Quadrant::blocks).collect();
    if let Some(bad) = coded
        .into_iter()
        .find(|b| b.inter.is_none() && usize::from(b.mode) >= INTRA_MODES)
    {
        return Err(Error::unsupported(
            "AV1 tile",
            format!(
                "intra mode {} is not one of the thirteen this writer codes",
                bad.mode
            ),
        ));
    }

    /// `Y_MODE`'s size group (spec `Size_Group`) for a 32x32 block, the only
    /// size this writer's inter branch codes.
    const SIZE_GROUP_32: usize = 3;

    let (sb_cols, sb_rows) = (cols.div_ceil(2), rows.div_ceil(2));
    let mut neighbours = Neighbours::new(
        cols as usize * 2,
        rows as usize * 2,
        mi_cols as usize,
        mi_rows as usize,
    );
    neighbours.set_tile_origin(tile.mi_row0 as usize, tile.mi_col0 as usize);
    // The MV grid spans the FRAME and carries every tile written before this
    // one, exactly as a decoder's does (`decode_inter_frame_tile_with_cdfs`
    // keeps one grid for the whole frame and narrows its read window per
    // tile): the tile bounds below are what stops a candidate scan reaching
    // across the boundary, and a grid that was merely empty out there would
    // not read the same as the decoder's wherever a scan reads the array
    // without asking `MiGrid::get` first.
    let grid = &mut *grid;
    grid.set_tile_bounds(
        tile.mi_row0 as usize,
        tile.mi_col0 as usize,
        tile.mi_row1 as usize,
        tile.mi_col1 as usize,
    );
    grid.set_sign_bias(SIGN_BIAS.with(std::cell::Cell::get));
    let mut enc = SymbolEncoder::new();
    // An `is_inter` block's 32x32 luma transform reads a different
    // `tx_type` set than an intra block's (`get_tx_set`, spec 5.11.48; see
    // `TxbSet::Luma32Inter`'s doc comment); chroma's transform type is
    // derived from luma's, not coded, so it never differs between the two.
    let intra_planes = [TxbSet::Luma32, TxbSet::Chroma16, TxbSet::Chroma16];
    let inter_planes = [TxbSet::Luma32Inter, TxbSet::Chroma16, TxbSet::Chroma16];
    // A 64x64 root's planes (lane-tx64): TX_64X64 luma -- DCT-only at this
    // size for an inter block too (spec `av1_get_ext_tx_set_type` returns
    // `TX_SET_DCTONLY` for `tx_size_sqr_up == TX_64X64`), so `Luma64` carries
    // no `tx_type` symbol and there is no `Luma64Inter` to write -- and one
    // TX_32X32 per chroma plane.
    let sb64_planes = [TxbSet::Luma64, TxbSet::Chroma32, TxbSet::Chroma32];
    let scan32 = default_scan(TX32);
    let scan16 = default_scan(TX16);
    let scan8 = default_scan(TX8);
    let scan4 = default_scan(TX4);
    // The same four tables again as one array, which is what the per-unit
    // luma writer indexes by transform side.
    let all_scans = [scan32.clone(), scan16.clone(), scan8.clone(), scan4.clone()];
    let zero_grids = [
        vec![0i32; TX32 * TX32],
        vec![0i32; TX16 * TX16],
        vec![0i32; TX16 * TX16],
    ];

    let sb128 = sb128_armed();
    // lane-b128: set at a root whose own `PARTITION_NONE` 128x128 block was
    // just written, so the root's remaining 64x64 cells code nothing.
    let mut sb128_none = false;
    // lane-sb128: at a 128 superblock the four 64x64 cells of each root are
    // written in libaom's own TL/TR/BL/BR order, the root's restoration units
    // and partition symbol first; at a 64 one this is plain raster and every
    // cell is its own superblock, i.e. byte-identical to the loop before.
    for (sb_r, sb_c, row_start, sb_start) in sb_write_order(
        tile.sb_row0,
        tile.sb_row1.min(sb_rows),
        tile.sb_col0,
        tile.sb_col1.min(sb_cols),
        sb128,
    ) {
        if row_start {
            neighbours.start_row();
        }
        if sb_start {
            sb128_none = false;
        }
        // lane-b128: the remaining three 64x64 cells of a root the search
        // left WHOLE carry no syntax at all -- the same skip the decoder's
        // own `sb128_none` runs.
        if sb128_none {
            continue;
        }
        if sb128 && sb_start {
            // The 128x128 block a `PARTITION_NONE` root carries, held by the
            // top-left 32x32 entry of the root's sixteen.
            let root_entry = ((sb_r & !1) * 2 < rows && (sb_c & !1) * 2 < cols)
                .then(|| &blocks[(((sb_r & !1) * 2) * cols + (sb_c & !1) * 2) as usize]);
            let root_block = root_entry.and_then(|q| match q {
                Quadrant::Whole128(block) => Some(block),
                _ => None,
            });
            // lane-b128hv: the two rectangular halves a HORZ/VERT root carries.
            let root_rect = root_entry.and_then(|q| match q {
                Quadrant::Rect128(horz, halves) => Some((*horz, halves)),
                _ => None,
            });
            // lane-ab128: the three pieces an AB-shape root carries, and the
            // partition symbol it is coded under.
            let root_ab = root_entry.and_then(|q| match q {
                Quadrant::Ab128(symbol, pieces) => Some((*symbol as usize, pieces)),
                _ => None,
            });
            write_sb128_root(
                &mut enc,
                cdfs,
                &neighbours,
                sb_r,
                sb_c,
                mi_cols,
                mi_rows,
                match (root_block.is_some(), root_rect, root_ab) {
                    (true, ..) => PARTITION_NONE,
                    (_, Some((true, _)), _) => PARTITION_HORZ,
                    (_, Some((false, _)), _) => PARTITION_VERT,
                    (_, _, Some((symbol, _))) => symbol,
                    _ => PARTITION_SPLIT,
                },
            );
            if let Some(block) = root_block {
                let at128 = ((sb_r & !1) as usize * 4, (sb_c & !1) as usize * 4);
                write_inter_block_128(
                    &mut enc,
                    cdfs,
                    &mut neighbours,
                    grid,
                    at128,
                    block,
                    mi_cols,
                    mi_rows,
                    tx_select,
                )?;
                sb128_none = true;
                continue;
            }
            if let Some((horz, halves)) = root_rect {
                // libaom `decode_partition` at BLOCK_128X128: `subsize` at
                // (mi_row, mi_col) and a second one at + hbs (16 mi) along the
                // cut axis, the second only when it is inside the frame -- the
                // search only offers a root wholly inside, so both are.
                let base_mi = (
                    (sb_r & !1) as usize * SB_MI as usize,
                    (sb_c & !1) as usize * SB_MI as usize,
                );
                let (w, h) = if horz { (128usize, 64usize) } else { (64, 128) };
                for (i, half) in halves.iter().enumerate() {
                    let at_mi = if horz {
                        (base_mi.0 + i * 16, base_mi.1)
                    } else {
                        (base_mi.0, base_mi.1 + i * 16)
                    };
                    write_inter_block_128_rect(
                        &mut enc,
                        cdfs,
                        &mut neighbours,
                        grid,
                        at_mi,
                        (w, h),
                        half,
                        mi_cols,
                        mi_rows,
                        tx_select,
                    )?;
                }
                sb128_none = true;
                continue;
            }
            if let Some((symbol, pieces)) = root_ab {
                // libaom `decode_partition` at BLOCK_128X128, AB arm: two
                // 64x64 blocks and one 128x64 / 64x128 half, in the order
                // decode.rs' `sb128_ab` reads them
                // ([`crate::encode::ab128_pieces`]). An AB shape only comes
                // from the full alphabet symbol (both halves in frame), so
                // there is no edge break to mirror -- the search offers one
                // only at a root wholly inside.
                let base_mi = (
                    (sb_r & !1) as usize * SB_MI as usize,
                    (sb_c & !1) as usize * SB_MI as usize,
                );
                for (((mi_r, mi_c), (w, h)), piece) in crate::encode::ab128_pieces(symbol, base_mi)
                    .iter()
                    .zip(pieces)
                {
                    write_inter_block_128_rect(
                        &mut enc,
                        cdfs,
                        &mut neighbours,
                        grid,
                        (*mi_r, *mi_c),
                        (*w, *h),
                        piece,
                        mi_cols,
                        mi_rows,
                        tx_select,
                    )?;
                }
                SB128_AB_HITS[symbol - 4].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                sb128_none = true;
                continue;
            }
        }
        {
            if !sb128 {
                write_lr(&mut enc, cdfs, sb_r * SB_MI_W, sb_c * SB_MI_W, SB_MI_W);
            }
            let sb_at = (sb_r as usize * 4, sb_c as usize * 4);
            let sb_ctx = neighbours.partition_ctx(sb_at, SB);
            // spec `decode_partition`'s hasRows/hasCols (5.11.4): a superblock
            // whose bottom or right half falls outside the true frame cannot
            // be left whole, so the only question there is which of the three
            // partition symbols the split takes — same three-way signaling as
            // `sb_coeff_key_frame_tile`'s superblock level.
            let (has_cols, has_rows) = (
                sb_c * SB_MI + SB_MI / 2 < mi_cols,
                sb_r * SB_MI + SB_MI / 2 < mi_rows,
            );
            // lane-b64: a superblock the search left WHOLE — one 64x64 block
            // at `PARTITION_NONE`, carried by its top-left quadrant's entry
            // ([`Quadrant::Whole64`]). Single-reference; skipped, or with a
            // real residual through `TxbSet::Luma64` + two `Chroma32`
            // (lane-tx64).
            let sb64 = ((sb_r * 2) < rows && (sb_c * 2) < cols)
                .then(|| &blocks[((sb_r * 2) * cols + sb_c * 2) as usize])
                .and_then(|q| match q {
                    Quadrant::Whole64(block) => Some(block),
                    _ => None,
                });
            if let Some(block) = sb64 {
                if !has_cols || !has_rows {
                    return Err(Error::unsupported(
                        "AV1 tile",
                        "a superblock that is half outside the true frame cannot be left whole",
                    ));
                }
                let info = block.inter.ok_or_else(|| {
                    Error::unsupported("AV1 tile", "a 64x64 root block is coded inter")
                })?;
                enc.symbol(PARTITION_NONE, &mut cdfs.partition_w64[sb_ctx]);
                let (mi_r, mi_c) = (sb_at.0 * (SUB / MI), sb_at.1 * (SUB / MI));
                let has_above = neighbours.has_above(mi_r);
                let has_left = neighbours.has_left(mi_c);
                let skip_ctx = usize::from(neighbours.above_skip[mi_c])
                    + usize::from(neighbours.left_skip[mi_r]);
                enc.symbol(usize::from(block.skip), &mut cdfs.skip[skip_ctx]);
                write_cdef_idx(&mut enc, (mi_r, mi_c), block.skip);
                write_delta_q(
                    &mut enc,
                    cdfs,
                    (mi_r, mi_c),
                    !sb128_armed(),
                    block.skip,
                );
                let ii_ctx = intra_inter_ctx(
                    has_above,
                    has_left,
                    neighbours.above_inter[mi_c],
                    neighbours.left_inter[mi_r],
                );
                enc.symbol(1, &mut cdfs.intra_inter[ii_ctx]);
                write_comp_mode(
                    &mut enc,
                    cdfs,
                    &neighbours,
                    (mi_r, mi_c),
                    (has_above, has_left),
                    info.ref1.is_some(),
                );
                // lane-b64b: a COMPOUND 64x64 root takes the same mode chain
                // every other compound block does, at this superblock's own
                // 16x16-mi window.
                let (mv, mv1, is_new_mv) = if info.ref1.is_some() {
                    write_compound_block(
                        &mut enc,
                        cdfs,
                        &neighbours,
                        grid,
                        (mi_r, mi_c),
                        (SB_MI as usize, SB_MI as usize),
                        (has_above, has_left),
                        info,
                        (mi_cols as usize, mi_rows as usize),
                    )?
                } else {
                    write_single_ref(
                        &mut enc,
                        cdfs,
                        info.ref_frame,
                        neighbours.above_ref[mi_c],
                        neighbours.above_ref1[mi_c],
                        neighbours.left_ref[mi_r],
                        neighbours.left_ref1[mi_r],
                    );
                    let stack = find_mv_stack(
                        grid,
                        mi_r,
                        mi_c,
                        SB_MI as usize,
                        SB_MI as usize,
                        info.ref_frame,
                        mi_cols as usize,
                        mi_rows as usize,
                    );
                    let (mv, is_new_mv) = write_inter_mode(&mut enc, cdfs, info, &stack)?;
                    if crate::msac::symtrace::dir().is_some() {
                        crate::msac::symtrace::note(&format!(
                            "  MODE mi=({mi_r},{mi_c}) mode={:?} idx={} mv={mv:?} info_mv={:?}",
                            info.mode, info.ref_mv_idx, info.mv
                        ));
                    }
                    (mv, (0, 0), is_new_mv)
                };
                for dr in 0..SB_MI as usize {
                    for dc in 0..SB_MI as usize {
                        grid.set(
                            mi_r + dr,
                            mi_c + dc,
                            MiInfo {
                                is_inter: true,
                                ref_frame: info.ref_frame,
                                ref_frame1: info.ref1.unwrap_or(NO_REF1),
                                mv1: mv16(mv1),
                                mv: mv16(mv),
                                is_new_mv,
                                size: SB_MI as u8,
                                size_h: SB_MI as u8,
                                is_global_mv0: false,
                                is_global_mv1: false,
                            },
                        );
                    }
                }
                if info.ref1.is_none() {
                    // libaom's `motion_mode_allowed` fails on a compound
                    // block, so the decoder reads no symbol for one.
                    write_motion_mode(
                        &mut enc,
                        cdfs,
                        grid,
                        (mi_r, mi_c),
                        (SB_MI as usize, SB_MI as usize),
                        (SB, SB),
                        (mi_cols as usize, mi_rows as usize),
                        info.ref_frame,
                        block.motion_mode,
                    )?;
                }
                if tx_select {
                    // A skipped inter block writes no `txfm_split` at all --
                    // that call only publishes the max transform size the
                    // reader infers for it; an unskipped one writes the
                    // `txfm_split` flag: 0 when the root's luma is one whole
                    // TX_64X64, 1 when it is the four TX_32X32 units
                    // lane-b64b's var-tx arm chose.
                    write_tx_syntax_inter(
                        &mut enc,
                        cdfs,
                        &mut neighbours,
                        (mi_r, mi_c),
                        SB,
                        true,
                        block.skip,
                        usize::from(block.tx_depth),
                    );
                }
                if block.skip {
                    neighbours.record(sb_at, SB, 0, &zero_grids);
                } else {
                    // lane-tx64: the 64x64 root's real residual. Luma is one
                    // TX_64X64 whose coded quarter is a 32x32 level grid
                    // (`TxbSet::Luma64`, `TX32`'s own scan -- the same pair
                    // `sb_coeff_key_frame_tile`'s `Superblock::Whole` writes),
                    // chroma one whole TX_32X32 per plane, and `split` is
                    // false because the luma transform covers the block.
                    // lane-b64b: at `txfm_split = 1` the luma levels arrive in
                    // 64x64 block coordinates and are written as the four
                    // TX_32X32 units `write_luma_tus` slices out, each
                    // publishing its own coefficient context before the next
                    // one reads it; chroma is one TX_32X32 per plane either
                    // way.
                    let split = block.tx_depth == 1;
                    let grids = [
                        level_grid(&block.luma, if split { SB } else { TX32 })?,
                        level_grid(&block.u, TX32)?,
                        level_grid(&block.v, TX32)?,
                    ];
                    if split {
                        write_luma_tus(
                            &mut enc,
                            cdfs,
                            &mut neighbours,
                            (mi_r, mi_c),
                            SB,
                            TX32,
                            &grids[0],
                            0,
                            &all_scans,
                            true,
                            &block.luma_tx_types,
                        )?;
                    }
                    write_block_planes(
                        &mut enc,
                        cdfs,
                        &sb64_planes,
                        &grids,
                        &[&scan32, &scan32, &scan32],
                        &neighbours.around(sb_at, SB),
                        0,
                        split,
                        block_tx_type(block),
                        true,
                    );
                    neighbours.record_planes(sb_at, SB, 0, &grids, !split);
                }
                neighbours.record_inter(sb_at, SB, block.skip, true, block_ref(block));
                record_block_compound(&mut neighbours, (mi_r, mi_c), SB, block);
                continue;
            }
            match (has_cols, has_rows) {
                (true, true) => enc.symbol(PARTITION_SPLIT, &mut cdfs.partition_w64[sb_ctx]),
                (true, false) => {
                    enc.symbol_fixed(1, &gather(&cdfs.partition_w64[sb_ctx], VERT_ALIKE));
                }
                (false, true) => {
                    enc.symbol_fixed(1, &gather(&cdfs.partition_w64[sb_ctx], HORZ_ALIKE));
                }
                (false, false) => {}
            }

            for quadrant in 0..4 {
                let (r32, c32) = (sb_r * 2 + quadrant / 2, sb_c * 2 + quadrant % 2);
                // Only a quadrant whose own mi origin is inside the true
                // frame is coded at all (spec `decode_partition`'s
                // `r >= MiRows || c >= MiCols` early return), same filter as
                // `sb_coeff_key_frame_tile`'s `quadrant_positions`.
                if r32 >= rows || c32 >= cols {
                    continue;
                }
                let site = &blocks[(r32 * cols + c32) as usize];
                let at = (r32 as usize * 2, c32 as usize * 2);
                let ctx32 = neighbours.partition_ctx(at, BLOCK);
                // spec `decode_partition`'s hasRows/hasCols recomputed at this
                // quadrant's own half: a whole 32x32 block cannot be left
                // whole once its own half straddles the true edge, mirroring
                // `sb_coeff_key_frame_tile`'s "cannot be left whole" refusal.
                let (has_cols32, has_rows32) = (
                    has_half(c32 * BLOCK_MI, BLOCK_MI, mi_cols),
                    has_half(r32 * BLOCK_MI, BLOCK_MI, mi_rows),
                );
                let block = match site {
                    // Already coded by the `Whole64` above -- but only inside
                    // a superblock that really carried one.
                    Quadrant::Whole64(_)
                    | Quadrant::Whole128(_)
                    | Quadrant::Rect128(..)
                    | Quadrant::Ab128(..)
                    | Quadrant::Covered => {
                        return Err(Error::unsupported(
                            "AV1 tile",
                            "a `Whole64`/`Covered` quadrant needs its superblock's \
                             top-left entry to be the `Whole64`",
                        ));
                    }
                    Quadrant::Whole(block) => {
                        if !has_cols32 || !has_rows32 {
                            return Err(Error::unsupported(
                                "AV1 tile",
                                "a 32x32 block that is half outside the true frame \
                                 cannot be left whole",
                            ));
                        }
                        enc.symbol(PARTITION_NONE, &mut cdfs.partition_w32[ctx32]);
                        block
                    }
                    Quadrant::Split(sub_blocks) => {
                        // Same filter as `sb_coeff_key_frame_tile`'s
                        // `sub_positions`: only the 16x16 leaves whose own mi
                        // origin is inside the true frame are coded.
                        let sub_positions: Vec<(usize, usize)> = (0..4)
                            .map(|i| (r32 as usize * 2 + i / 2, c32 as usize * 2 + i % 2))
                            .filter(|&(sr, sc)| {
                                (sr as u32) * SUB_MI < mi_rows && (sc as u32) * SUB_MI < mi_cols
                            })
                            .collect();
                        if sub_blocks.len() != sub_positions.len() {
                            return Err(Error::unsupported(
                                "AV1 tile",
                                "a split 32x32 inter-frame block needs one 16x16 entry \
                                 per sub-block inside the true frame",
                            ));
                        }
                        // Same three-way spec signaling as the superblock
                        // level above, recomputed at this quadrant's own half.
                        match (has_cols32, has_rows32) {
                            (true, true) => {
                                enc.symbol(PARTITION_SPLIT, &mut cdfs.partition_w32[ctx32]);
                            }
                            (true, false) => {
                                enc.symbol_fixed(
                                    1,
                                    &gather(&cdfs.partition_w32[ctx32], VERT_ALIKE),
                                );
                            }
                            (false, true) => {
                                enc.symbol_fixed(
                                    1,
                                    &gather(&cdfs.partition_w32[ctx32], HORZ_ALIKE),
                                );
                            }
                            (false, false) => {}
                        }
                        for (leaf, (sr, sc)) in sub_blocks.iter().zip(sub_positions) {
                            // A 16x16 leaf whose own half straddles the true
                            // frame edge splits into the 8x8 leaves that are
                            // inside it (lane-av1inter8), same split the key
                            // frame search takes at this geometry -- on both
                            // axes too, where `decode_partition` infers the
                            // split and reads no symbol at all
                            // (lane-av1rect).
                            let (has_cols16, has_rows16) = (
                                has_half(sc as u32 * SUB_MI, SUB_MI, mi_cols),
                                has_half(sr as u32 * SUB_MI, SUB_MI, mi_rows),
                            );
                            let at16 = (sr, sc);
                            if has_cols16 && has_rows16 {
                                let ctx16 = neighbours.partition_ctx(at16, SUB);
                                // A 16x16 leaf wholly inside the frame that
                                // carries `eight` split itself further, on
                                // cost rather than on geometry (lane-av1rd2):
                                // spec PARTITION_SPLIT at BLOCK_16X16, then
                                // the same four 8x8 leaves the straddling
                                // branch below writes -- all four inside, so
                                // the partition symbol is a real one rather
                                // than the edge's gathered flag.
                                if let Some(leaves) = &leaf.eight {
                                    if leaves.len() != 4 {
                                        return Err(Error::unsupported(
                                            "AV1 tile",
                                            "a 16x16 inter-frame block inside the frame that                                              splits needs all four 8x8 leaves",
                                        ));
                                    }
                                    enc.symbol(PARTITION_SPLIT, &mut cdfs.partition_w16[ctx16]);
                                    let (mi_row0, mi_col0) =
                                        (sr as u32 * SUB_MI, sc as u32 * SUB_MI);
                                    for (i, leaf8) in leaves.iter().enumerate() {
                                        let leaf_mi = (
                                            (mi_row0 + (i as u32 / 2) * 2) as usize,
                                            (mi_col0 + (i as u32 % 2) * 2) as usize,
                                        );
                                        let leaf_ctx = neighbours.partition_ctx_mi(leaf_mi, 8);
                                        enc.symbol(
                                            PARTITION_NONE,
                                            &mut cdfs.partition_w8[leaf_ctx],
                                        );
                                        let (skip, is_inter) = write_inter_frame_leaf8(
                                            &mut enc,
                                            cdfs,
                                            &mut neighbours,
                                            grid,
                                            mi_cols,
                                            mi_rows,
                                            leaf8,
                                            leaf_mi,
                                            &scan8,
                                            &scan4,
                                            tx_select,
                                            &all_scans,
                                        )?;
                                        let _ = (skip, is_inter);
                                    }
                                    continue;
                                }
                                enc.symbol(PARTITION_NONE, &mut cdfs.partition_w16[ctx16]);
                                write_inter_frame_leaf(
                                    &mut enc,
                                    cdfs,
                                    &mut neighbours,
                                    grid,
                                    mi_cols,
                                    mi_rows,
                                    leaf,
                                    at16,
                                    &scan16,
                                    &scan8,
                                    tx_select,
                                    &all_scans,
                                )?;
                            } else {
                                let ctx16 = neighbours.partition_ctx(at16, SUB);
                                match (has_cols16, has_rows16) {
                                    (true, false) => enc.symbol_fixed(
                                        1,
                                        &gather(&cdfs.partition_w16[ctx16], VERT_ALIKE),
                                    ),
                                    (false, true) => enc.symbol_fixed(
                                        1,
                                        &gather(&cdfs.partition_w16[ctx16], HORZ_ALIKE),
                                    ),
                                    // Both halves outside: SPLIT is inferred.
                                    _ => {}
                                }
                                let leaves = leaf.eight.as_ref().ok_or_else(|| {
                                    Error::unsupported(
                                        "AV1 tile",
                                        "a 16x16 inter-frame block the true frame edge cuts \
                                         through needs its `eight` leaves populated",
                                    )
                                })?;
                                let (mi_row0, mi_col0) = (sr as u32 * SUB_MI, sc as u32 * SUB_MI);
                                let leaf_positions: Vec<(u32, u32)> = (0..4)
                                    .map(|i| (mi_row0 + (i / 2) * 2, mi_col0 + (i % 2) * 2))
                                    .filter(|&(mr, mc)| mr < mi_rows && mc < mi_cols)
                                    .collect();
                                if leaves.len() != leaf_positions.len() {
                                    return Err(Error::unsupported(
                                        "AV1 tile",
                                        "a straddling 16x16 inter-frame block needs one \
                                         `eight` entry per 8x8 leaf inside the true frame",
                                    ));
                                }
                                for (leaf8, (mr, mc)) in leaves.iter().zip(leaf_positions) {
                                    let leaf_mi = (mr as usize, mc as usize);
                                    let leaf_ctx = neighbours.partition_ctx_mi(leaf_mi, 8);
                                    enc.symbol(PARTITION_NONE, &mut cdfs.partition_w8[leaf_ctx]);
                                    let (skip, is_inter) = write_inter_frame_leaf8(
                                        &mut enc,
                                        cdfs,
                                        &mut neighbours,
                                        grid,
                                        mi_cols,
                                        mi_rows,
                                        leaf8,
                                        leaf_mi,
                                        &scan8,
                                        &scan4,
                                        tx_select,
                                        &all_scans,
                                    )?;
                                    let _ = (skip, is_inter);
                                }
                                // Same write-back-once-from-the-last-leaf rule
                                // as the key frame search's `write_leaf8`
                                // caller (r15): the SUB-grid skip/inter arrays
                                // are otherwise left stale for the next
                                // 16x16 slot.
                            }
                        }
                        continue;
                    }
                };

                let (r, c) = at;
                let (mi_r, mi_c) = (r * (SUB / MI), c * (SUB / MI));
                let has_above = neighbours.has_above(mi_r);
                let has_left = neighbours.has_left(mi_c);
                let skip_ctx = usize::from(neighbours.above_skip[mi_c])
                    + usize::from(neighbours.left_skip[mi_r]);
                enc.symbol(usize::from(block.skip), &mut cdfs.skip[skip_ctx]);
                write_cdef_idx(&mut enc, (mi_r, mi_c), block.skip);
                write_delta_q(&mut enc, cdfs, (mi_r, mi_c), false, block.skip);

                let is_inter = block.inter.is_some();
                let (above_inter, left_inter) =
                    (neighbours.above_inter[mi_c], neighbours.left_inter[mi_r]);
                let ii_ctx = intra_inter_ctx(has_above, has_left, above_inter, left_inter);
                enc.symbol(usize::from(is_inter), &mut cdfs.intra_inter[ii_ctx]);

                let mode_for_tx;
                if let Some(info) = block.inter {
                    write_comp_mode(
                        &mut enc,
                        cdfs,
                        &neighbours,
                        (mi_r, mi_c),
                        (has_above, has_left),
                        info.ref1.is_some(),
                    );
                    let (mi_row, mi_col) = (r32 as usize * 8, c32 as usize * 8);
                    if info.ref1.is_some() {
                        let (mv, mv1, is_new_mv) = write_compound_block(
                            &mut enc,
                            cdfs,
                            &neighbours,
                            grid,
                            (mi_row, mi_col),
                            (8, 8),
                            (has_above, has_left),
                            info,
                            (mi_cols as usize, mi_rows as usize),
                        )?;
                        for dr in 0..8 {
                            for dc in 0..8 {
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
                                        size: 8,
                                        size_h: 8,
                                        is_global_mv0: false,
                                        is_global_mv1: false,
                                    },
                                );
                            }
                        }
                        mode_for_tx = 0;
                    } else {
                        write_single_ref(
                            &mut enc,
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
                            8,
                            8,
                            info.ref_frame,
                            mi_cols as usize,
                            mi_rows as usize,
                        );

                        let (mv, is_new_mv) = write_inter_mode(&mut enc, cdfs, info, &stack)?;
                        if crate::msac::symtrace::dir().is_some() {
                            crate::msac::symtrace::note(&format!(
                                "  MODE mi=({mi_r},{mi_c}) mode={:?} idx={} mv={mv:?} info_mv={:?}",
                                info.mode, info.ref_mv_idx, info.mv
                            ));
                        }
                        grid.set(
                            mi_row,
                            mi_col,
                            MiInfo {
                                is_inter: true,
                                ref_frame: info.ref_frame,
                                ref_frame1: NO_REF1,
                                mv1: (0, 0),
                                mv: mv16(mv),
                                is_new_mv,
                                size: 8,
                                size_h: 8,
                                is_global_mv0: false,
                                is_global_mv1: false,
                            },
                        );
                        for dr in 0..8 {
                            for dc in 0..8 {
                                if dr == 0 && dc == 0 {
                                    continue;
                                }
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
                                        size: 8,
                                        size_h: 8,
                                        is_global_mv0: false,
                                        is_global_mv1: false,
                                    },
                                );
                            }
                        }
                        write_motion_mode(
                            &mut enc,
                            cdfs,
                            grid,
                            (mi_row, mi_col),
                            (8, 8),
                            (BLOCK, BLOCK),
                            (mi_cols as usize, mi_rows as usize),
                            info.ref_frame,
                            block.motion_mode,
                        )?;
                        mode_for_tx = 0;
                    }
                } else {
                    let mode = usize::from(block.mode);
                    enc.symbol(mode, &mut cdfs.y_mode[SIZE_GROUP_32]);
                    if (V_PRED..=D67_PRED).contains(&mode) {
                        enc.symbol(
                            (ANGLE_DELTA_ZERO as i32 + i32::from(block.angle_delta_y)) as usize,
                            &mut cdfs.angle_delta[mode - V_PRED],
                        );
                    }
                    let uv_mode = usize::from(block.uv_mode);
                    enc.symbol(uv_mode, &mut cdfs.uv_mode_cfl[mode]);
                    if uv_mode == UV_CFL_PRED {
                        write_cfl_alphas(&mut enc, cdfs, block.cfl_alphas.expect("cfl"));
                    }
                    if (V_PRED..=D67_PRED).contains(&uv_mode) {
                        enc.symbol(ANGLE_DELTA_ZERO, &mut cdfs.angle_delta[uv_mode - V_PRED]);
                    }
                    mode_for_tx = tx_row(mode, block.filter_intra);
                    // Intra: no vote, but still a coded cell -- mvstack's
                    // extended-scan coverage must see it (module doc).
                    let (mi_row, mi_col) = (r32 as usize * 8, c32 as usize * 8);
                    write_palette_syntax(
                        &mut enc,
                        cdfs,
                        &mut neighbours,
                        block.palette.as_ref(),
                        block.palette_uv.as_ref(),
                        block.filter_intra,
                        mode,
                        uv_mode,
                        (mi_row, mi_col),
                        BLOCK,
                    );
                    for dr in 0..8 {
                        for dc in 0..8 {
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
                                    size: 8,
                                    size_h: 8,
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
                        &mut enc,
                        cdfs,
                        &mut neighbours,
                        at_mi,
                        BLOCK,
                        is_inter,
                        block.skip,
                        usize::from(block.tx_depth),
                    )
                } else {
                    BLOCK
                };
                let split = tx < BLOCK;
                if block.skip {
                    neighbours.record(at, BLOCK, mode_for_tx, &zero_grids);
                } else {
                    let grids = [
                        level_grid(&block.luma, TX32)?,
                        level_grid(&block.u, TX16)?,
                        level_grid(&block.v, TX16)?,
                    ];
                    if split {
                        write_luma_tus(
                            &mut enc,
                            cdfs,
                            &mut neighbours,
                            at_mi,
                            BLOCK,
                            tx,
                            &grids[0],
                            mode_for_tx,
                            &all_scans,
                            is_inter,
                            &block.luma_tx_types,
                        )?;
                    }
                    write_block_planes(
                        &mut enc,
                        cdfs,
                        if is_inter {
                            &inter_planes
                        } else {
                            &intra_planes
                        },
                        &grids,
                        &[&scan32, &scan16, &scan16],
                        &neighbours.around(at, BLOCK),
                        mode_for_tx,
                        split,
                        block_tx_type(block),
                        is_inter,
                    );
                    neighbours.record_planes(at, BLOCK, mode_for_tx, &grids, !split);
                }
                neighbours.record_inter(at, BLOCK, block.skip, is_inter, block_ref(block));
                record_block_compound(&mut neighbours, (mi_r, mi_c), BLOCK, block);
            }
        }
    }
    Ok(enc.finish())
}

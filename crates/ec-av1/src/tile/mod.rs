//! Tile payload writer (spec 5.11), for the one block coding the encoder can
//! produce today: a key frame whose every superblock is a single 64x64
//! DC-predicted block with no residual.
//!
//! That frame decodes to a flat mid-grey picture — every sample is the value a
//! DC prediction with no neighbours produces — which is what makes it a usable
//! gate: any desync between this writer and a real decoder shows up as a
//! decode failure or a sample that is not mid-grey, with no metric in the way.
//! It is the skeleton the block modes, transform sizes and coefficients hang
//! off as they arrive.

use std::cell::RefCell;
#[cfg(test)]
use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use ec_core::{Error, Result};

use crate::cdf;
use crate::cdf_state::{Cdfs, MvComponentCdfs, TxbSet, TxbTables};
use crate::decode::{TXFM_CTX_INIT, txfm_partition_ctx_rect};
use crate::msac::SymbolEncoder;
use crate::mvstack::{MiGrid, MiInfo, NO_REF1, find_mv_stack, mv16};
use crate::transform::TxType;

// ---------------------------------------------------------------------------
// lane-av1lr: per-64x64 `cdef_idx` (spec 5.11.56, `read_cdef`). The decoder
// reads ONE `cdef_idx` literal per 64x64 superblock, at that superblock's
// first non-skip block, and only when the frame header's `cdef_bits > 0`
// (`crate::decode::maybe_read_cdef_idx`). The writers below mirror that from
// a plan the filter search hands over, kept in a thread-local rather than
// threaded through every `write_*` signature -- the same shape the decoder's
// own `FrameCtx::cdef_transmitted` uses, and the encoder codes one tile at a
// time on one thread. The plan is CONSUMED by the tile writer that runs next
// ([`CdefIdxGuard`] clears it on every exit path, error included), so a frame
// that arms nothing writes no literals.

// ---------------------------------------------------------------------------
// lane-av1lr: per-restoration-unit loop restoration syntax (spec 5.11.57,
// `read_lr`), armed by the encoder's restoration search the same way
// [`arm_cdef_idx`] arms the CDEF indices. LUMA ONLY and Wiener only: the
// frame header this encoder writes sets `FrameRestorationType[0] = WIENER`
// and leaves both chroma planes at `RESTORE_NONE`, so a unit codes one
// `restore_wiener` symbol and, when it takes the filter, its two directions
// against the running per-tile reference (`crate::restoration::
// write_wiener_filter`, the exact inverse of the reader's own
// `read_wiener_filter`).

/// One frame's per-unit restoration plan (see [`arm_lr`]).
struct LrPlan {
    /// `LoopRestorationSize[0]`, in luma samples.
    unit_size: u32,
    /// `horz_units`/`vert_units` for the luma plane.
    horz_units: u32,
    vert_units: u32,
    /// One entry per unit, `rcol + rrow * horz_units`: the filter it takes,
    /// or `None` for `RESTORE_NONE`.
    units: Vec<Option<crate::restoration::WienerInfo>>,
    /// `ref_wiener_info`: the running reference the taps are coded against,
    /// reset to the midpoint filter at the start of each tile (this writer
    /// codes one tile per frame).
    reference: crate::restoration::WienerInfo,
}

thread_local! {
    static LR_PLAN: std::cell::RefCell<Option<LrPlan>> = const { std::cell::RefCell::new(None) };
}

/// Arms the next tile write with one Wiener decision per luma restoration
/// unit. An empty `units` disarms (the frame codes no restoration at all).
pub(crate) fn arm_lr(
    unit_size: u32,
    horz_units: u32,
    vert_units: u32,
    units: Vec<Option<crate::restoration::WienerInfo>>,
) {
    LR_PLAN.with(|c| {
        *c.borrow_mut() = (!units.is_empty()).then(|| LrPlan {
            unit_size,
            horz_units,
            vert_units,
            units,
            reference: crate::restoration::WienerInfo::default(),
        });
    });
}

// ---------------------------------------------------------------------------
// lane-sb128: the sequence's `use_128x128_superblock`. Armed per tile write
// like [`arm_lr`] (the encoder codes one tile at a time per thread) rather
// than threaded through every writer signature. When set, the two tile
// writers walk 128x128 superblocks -- loop restoration and one
// `partition_w128` symbol at the 128 root, then the four 64x64 quadrants in
// libaom `decode_partition`'s own TL, TR, BL, BR order -- which is exactly
// what `crate::decode::read_sb128_root` reads.

thread_local! {
    static SB128: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Arms the next tile write with the sequence's 128x128 superblock size.
pub(crate) fn arm_sb128(on: bool) {
    SB128.with(|c| c.set(on));
}

/// How many 128x128 superblock roots the writers have coded since the last
/// [`take_sb128_root_hits`] -- a gate reports how often the root fires rather
/// than assuming it does (class `gate-blind-to-feature`). Process-global
/// rather than thread-local because the tiles of one frame are written on
/// worker threads.
static SB128_ROOT_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The 128x128-root count since the last call, and zero it.
pub fn take_sb128_root_hits() -> usize {
    SB128_ROOT_HITS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// Whether the next tile write codes 128x128 superblocks ([`arm_sb128`]).
fn sb128_armed() -> bool {
    SB128.with(std::cell::Cell::get)
}

/// The order a tile's 64x64 superblock cells are written in, mirroring
/// `crate::decode::sb_visit_order` exactly: plain raster at a 64 superblock,
/// and TL/TR/BL/BR inside each 128x128 superblock (in raster order over the
/// 128 grid) at a 128 one. The two flags mark the first cell of a superblock
/// ROW (where the left neighbour context resets) and the first cell of a
/// superblock (where the 128 root's own restoration units and partition
/// symbol are written).
fn sb_write_order(r0: u32, r1: u32, c0: u32, c1: u32, sb128: bool) -> Vec<(u32, u32, bool, bool)> {
    let mut out = Vec::new();
    if !sb128 {
        for r in r0..r1 {
            for c in c0..c1 {
                out.push((r, c, c == c0, true));
            }
        }
        return out;
    }
    let mut r = r0;
    while r < r1 {
        let mut first_in_row = true;
        let mut c = c0;
        while c < c1 {
            let mut first_in_sb = true;
            for qr in 0..2 {
                for qc in 0..2 {
                    let (rr, cc) = (r + qr, c + qc);
                    if rr >= r1 || cc >= c1 {
                        continue;
                    }
                    out.push((rr, cc, first_in_row, first_in_sb));
                    first_in_row = false;
                    first_in_sb = false;
                }
            }
            c += 2;
        }
        r += 2;
    }
    out
}

/// The 128x128 superblock root ([`sb_write_order`]'s `sb_start` cells): the
/// units its own 128-sample span covers, then the partition symbol. Only
/// `PARTITION_SPLIT` is written -- the four 64x64 quadrants below are today's
/// superblocks -- and at a root the true frame edge cuts, the DECIDING
/// gathered bit the decoder reads instead (`read_sb128_root`).
// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
fn write_sb128_root(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    neighbours: &Neighbours,
    sb_r: u32,
    sb_c: u32,
    mi_cols: u32,
    mi_rows: u32,
    // lane-b128: the partition the root's own search chose --
    // `PARTITION_SPLIT` (the four 64x64 quadrants below, which is every root
    // this writer coded before) or `PARTITION_NONE` (one 128x128 block,
    // written by the caller right after this). Only offered at a root wholly
    // inside the frame, so the gathered edge arms below still code SPLIT.
    part: usize,
) {
    SB128_ROOT_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (mi_r128, mi_c128) = ((sb_r & !1) * SB_MI, (sb_c & !1) * SB_MI);
    write_lr(enc, cdfs, mi_r128, mi_c128, SB_MI * 2);
    let at128 = ((mi_r128 / 4) as usize, (mi_c128 / 4) as usize);
    let ctx128 = neighbours.partition_ctx(at128, 128);
    let (has_cols128, has_rows128) = (mi_c128 + SB_MI < mi_cols, mi_r128 + SB_MI < mi_rows);
    if crate::envflags::env_flag!("EC_AV1_TRACE") {
        eprintln!("WTRACE partition_w128 ctx={ctx128} cols={has_cols128} rows={has_rows128}");
    }
    match (has_cols128, has_rows128) {
        (true, true) => enc.symbol(part, &mut cdfs.partition_w128[ctx128]),
        // As at the 64 root: the gathered CDF is built for the read and
        // thrown away, so nothing adapts here. Value 1 is SPLIT.
        (true, false) => enc.symbol_fixed(
            1,
            &crate::decode::gather_of(&cdfs.partition_w128[ctx128], &crate::decode::VERT_ALIKE128),
        ),
        (false, true) => enc.symbol_fixed(
            1,
            &crate::decode::gather_of(&cdfs.partition_w128[ctx128], &crate::decode::HORZ_ALIKE128),
        ),
        // Both halves outside: the split is the only partition left and the
        // decoder reads nothing.
        (false, false) => {}
    }
}

/// lane-b128: one 128x128 inter block at a `PARTITION_NONE` 128 superblock
/// root -- the syntax `decode_inter_block` reads for `sb128_none` (decode.rs
/// ~35110): `skip`, `is_inter`, the reference/mode chain at the block's own
/// 32x32-mi window, the motion mode, and the transform syntax, with the mi
/// grid and every neighbour band published over all thirty-two mi cells.
///
/// lane-b128r: a block that carries a RESIDUAL codes it as the four 64x64 mu
/// chunks libaom's `decode_token_recon_block` walks -- per chunk one
/// TX_64X64 luma unit (its coded 32x32 corner, `TxbSet::Luma64`, DCT-only at
/// this size) then one TX_32X32 unit per chroma plane on the offset-10
/// `txb_skip` rows, each publishing its own coefficient context before the
/// next unit reads it. Its transform syntax is the var-tx tree's four
/// depth-0 `txfm_partition` symbols, one per chunk. The levels arrive packed
/// per plane into a 64-wide grid, chunk (cr, cc) at (cr * 32, cc * 32)
/// ([`crate::encode::search_root_128`]). A skipped block writes no
/// `cdef_idx` and no `delta_q`, exactly as the 64 root's skip arm does.
///
/// # Errors
/// A block that is intra, that is compound with a residual, or whose var-tx
/// depth is not 0, at this size.
#[allow(clippy::too_many_arguments)]
fn write_inter_block_128(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    grid: &mut MiGrid,
    // The root's top-left position in [`SUB`] (16x16) units.
    sb_at: (usize, usize),
    block: &BlockCoeffs,
    mi_cols: u32,
    mi_rows: u32,
    tx_select: bool,
) -> Result<()> {
    /// The block's side in samples and in 4x4 mode-info units.
    const SIDE: usize = 128;
    const SIDE_MI: usize = SIDE / MI;
    /// The four 64x64 "mu" chunks, in the order libaom's
    /// `decode_token_recon_block` walks them (raster).
    const CHUNKS: [(usize, usize); 4] = [(0, 0), (0, 1), (1, 0), (1, 1)];
    let info = block.inter.ok_or_else(|| {
        Error::unsupported("AV1 tile", "a 128x128 superblock root block is coded inter")
    })?;
    if !block.skip && block.tx_depth != 0 {
        return Err(Error::unsupported(
            "AV1 tile",
            "a 128x128 superblock root block splits its var-tx tree below TX_64X64",
        ));
    }
    let (mi_r, mi_c) = (sb_at.0 * (SUB / MI), sb_at.1 * (SUB / MI));
    let has_above = neighbours.has_above(mi_r);
    let has_left = neighbours.has_left(mi_c);
    let skip_ctx =
        usize::from(neighbours.above_skip[mi_c]) + usize::from(neighbours.left_skip[mi_r]);
    enc.symbol(usize::from(block.skip), &mut cdfs.skip[skip_ctx]);
    write_cdef_idx(enc, (mi_r, mi_c), block.skip);
    write_delta_q(enc, cdfs, (mi_r, mi_c), true, block.skip);
    let ii_ctx = intra_inter_ctx(
        has_above,
        has_left,
        neighbours.above_inter[mi_c],
        neighbours.left_inter[mi_r],
    );
    enc.symbol(1, &mut cdfs.intra_inter[ii_ctx]);
    write_comp_mode(
        enc,
        cdfs,
        neighbours,
        (mi_r, mi_c),
        (has_above, has_left),
        info.ref1.is_some(),
    );
    let (mv, mv1, is_new_mv) = if info.ref1.is_some() {
        write_compound_block(
            enc,
            cdfs,
            neighbours,
            grid,
            (mi_r, mi_c),
            (SIDE_MI, SIDE_MI),
            (has_above, has_left),
            info,
            (mi_cols as usize, mi_rows as usize),
        )?
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
            mi_r,
            mi_c,
            SIDE_MI,
            SIDE_MI,
            info.ref_frame,
            mi_cols as usize,
            mi_rows as usize,
        );
        let (mv, is_new_mv) = write_inter_mode(enc, cdfs, info, &stack)?;
        (mv, (0, 0), is_new_mv)
    };
    for dr in 0..SIDE_MI {
        for dc in 0..SIDE_MI {
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
                    size: SIDE_MI as u8,
                    size_h: SIDE_MI as u8,
                    is_global_mv0: false,
                    is_global_mv1: false,
                },
            );
        }
    }
    if info.ref1.is_none() {
        write_motion_mode(
            enc,
            cdfs,
            grid,
            (mi_r, mi_c),
            (SIDE_MI, SIDE_MI),
            (SIDE, SIDE),
            (mi_cols as usize, mi_rows as usize),
            info.ref_frame,
            block.motion_mode,
        )?;
    }
    if tx_select {
        if block.skip {
            write_tx_syntax_inter(enc, cdfs, neighbours, (mi_r, mi_c), SIDE, true, true, 0);
        } else {
            // The var-tx tree of a 128x128 block enters at TX_64X64 once per
            // mu chunk (`read_block_tx_size`'s `max_tx = side.min(64)` loop),
            // so it is four depth-0 `txfm_split` flags of zero, each read
            // against the bands the chunks before it published.
            for (cr, cc) in CHUNKS {
                let unit = (mi_r + cr * 16, mi_c + cc * 16);
                let ctx = txfm_partition_ctx_rect(
                    neighbours.above_txfm[unit.1],
                    neighbours.left_txfm[unit.0],
                    SIDE,
                    64,
                    64,
                );
                enc.symbol(0, &mut cdfs.txfm_partition[ctx]);
                neighbours.record_txfm(unit, 64, 64, 64);
            }
        }
    }
    let zero_grids = [
        vec![0i32; TX32 * TX32],
        vec![0i32; TX32 * TX32],
        vec![0i32; TX32 * TX32],
    ];
    if !block.skip {
        // The packed 64-wide level grids the search hands over, one plane
        // each (see this function's doc).
        let packed = [
            level_grid(&block.luma, 64)?,
            level_grid(&block.u, 64)?,
            level_grid(&block.v, 64)?,
        ];
        let scan32 = default_scan(TX32);
        let unit_of = |plane: usize, cr: usize, cc: usize| -> Vec<i32> {
            (0..32)
                .flat_map(|row| packed[plane][(cr * 32 + row) * 64 + cc * 32..][..32].to_vec())
                .collect()
        };
        let mut chroma_units: Vec<((usize, usize), [Vec<i32>; 2])> = Vec::new();
        for (cr, cc) in CHUNKS {
            let unit_mi = (mi_r + cr * 16, mi_c + cc * 16);
            // The luma unit: its own `txb_skip_ctx` is the neighbour
            // magnitude table (the unit is smaller than its block), and it
            // publishes its context before the next chunk reads it.
            let luma = unit_of(0, cr, cc);
            let skip_ctx = neighbours.luma_skip_ctx(unit_mi, 64 / MI);
            let around = neighbours.around_mi(unit_mi, 64);
            write_coeffs(
                enc,
                &mut cdfs.txb(TxbSet::Luma64, 0),
                &luma,
                &scan32,
                skip_ctx,
                dc_sign_ctx(around[0].dc_vote),
                Some(0),
                TxType::DctDct,
            );
            neighbours.record_mi_luma(unit_mi, 64, &luma);
            // Then this chunk's chroma pair, before the next chunk's luma:
            // libaom codes every plane of a mu chunk before moving on. The
            // chroma `txb_skip_ctx` takes `get_txb_ctx`'s offset-10 rows
            // (+3 here) because the uv plane block is bigger than the unit.
            // lane-av1-128rectspan: SQUARE per-axis geometry -- one TX_32X32
            // per 64x64 mu chunk, stamped at span 64 on BOTH axes. That is
            // 4:2:0/4:4:4 only: at 4:2:2 the chunk's chroma plane block is
            // 32x64 (two stacked units) with a (64, 32) luma footprint per
            // unit, so `unit_mi`, the `64`-spans and the whole `CHUNKS`
            // raster need the same per-axis treatment before 4:2:2 could be
            // written here. This is a PORT note, not a live defect, because
            // nothing feeds this writer 4:2:2: every non-test `ColorConfig`
            // the encoder names is (1,1), pinned by arm 2 of
            // `the_chroma_palette_map_side_is_the_420_plane_block_and_nothing_
            // else_reaches_it`. lane-av1422lift lifted the decoder's
            // sequence-header refusal, so the reason this is not live is the
            // writer's 4:2:0-only source, not a refusal.
            let mut pair = [Vec::new(), Vec::new()];
            for plane in 1..3usize {
                let grid = unit_of(plane, cr, cc);
                let around = neighbours.around_mi(unit_mi, 64)[plane];
                write_coeffs(
                    enc,
                    &mut cdfs.txb(TxbSet::Chroma32, 0),
                    &grid,
                    &scan32,
                    usize::from(around.above_coded) + usize::from(around.left_coded) + 3,
                    dc_sign_ctx(around.dc_vote),
                    Some(plane),
                    TxType::DctDct,
                );
                neighbours.record_mi_chroma(unit_mi, 64, plane, &grid);
                pair[plane - 1] = grid;
            }
            chroma_units.push((unit_mi, pair));
        }
        // decode.rs' own tail (`record_split_luma_rect_mi` then the
        // `mu_chroma_units` re-stamp): the block-level write off the WHOLE
        // assembled chroma plane grid, luma kept because each unit already
        // published its own, and then the per-unit chroma writes replayed
        // over it -- last writer wins, so the next block reads the units.
        let block_grids = [
            vec![0i32; TX32 * TX32],
            packed[1].clone(),
            packed[2].clone(),
        ];
        neighbours.record_planes(sb_at, SIDE, 0, &block_grids, false);
        for (unit_mi, pair) in &chroma_units {
            neighbours.record_mi_chroma(*unit_mi, 64, 1, &pair[0]);
            neighbours.record_mi_chroma(*unit_mi, 64, 2, &pair[1]);
        }
    } else {
        neighbours.record(sb_at, SIDE, 0, &zero_grids);
    }
    neighbours.record_inter(sb_at, SIDE, block.skip, true, block_ref(block));
    record_block_compound(neighbours, (mi_r, mi_c), SIDE, block);
    SB128_NONE_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if !block.skip {
        SB128_RESIDUAL_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    if info.ref1.is_some() {
        SB128_COMPOUND_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}

/// lane-b128hv: one 128x64 (HORZ) or 64x128 (VERT) inter block, the two
/// halves a `PARTITION_HORZ`/`PARTITION_VERT` 128 superblock root carries --
/// the syntax `decode_inter_block` reads for `sb128_rect` (decode.rs ~35343):
/// the same chain [`write_inter_block_128`] writes, at the block's own
/// rectangular window. Every CDF/context decision is taken at the enclosing
/// square `side` of 128 (what the decoder's `inter_piece!` passes), and the
/// block's true footprint governs only its mv-stack window, its mi/grid stamp
/// and its neighbour bands (decode.rs' own `write_w`/`write_h` split).
///
/// lane-ab128: also the two 64x64 squares an AB shape codes (the `w == h`
/// arm below) and its 128x64 / 64x128 half.
///
/// lane-rectres: a piece may CARRY A RESIDUAL -- the search only ever hands
/// one over under `EC_AV1_RECTRES` ([`crate::encode::rectres`]), and then it
/// is coded here exactly as `decode_inter_block` reads it: one TX_64X64 luma
/// unit plus a TX_32X32 chroma pair per 64x64 mu chunk (two chunks for a
/// half, one for a square), each depth-0 in the var-tx tree (a half codes
/// two `txfm_partition` flags under `TxMode::Select`, a square one).
///
/// # Errors
/// A block that is intra.
#[allow(clippy::too_many_arguments)]
fn write_inter_block_128_rect(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    grid: &mut MiGrid,
    // The half's own top-left position in mode-info (4x4) units.
    at_mi: (usize, usize),
    // Its true footprint in samples: (128, 64) at HORZ, (64, 128) at VERT.
    (w, h): (usize, usize),
    block: &BlockCoeffs,
    mi_cols: u32,
    mi_rows: u32,
    tx_select: bool,
) -> Result<()> {
    let info = block.inter.ok_or_else(|| {
        Error::unsupported("AV1 tile", "a 128-root rectangular half is coded inter")
    })?;
    let (mi_r, mi_c) = at_mi;
    let (w_mi, h_mi) = (w / MI, h / MI);
    let has_above = neighbours.has_above(mi_r);
    let has_left = neighbours.has_left(mi_c);
    let skip_ctx =
        usize::from(neighbours.above_skip[mi_c]) + usize::from(neighbours.left_skip[mi_r]);
    enc.symbol(usize::from(block.skip), &mut cdfs.skip[skip_ctx]);
    write_cdef_idx(enc, (mi_r, mi_c), block.skip);
    // NOT a whole superblock: decode.rs' `whole_sb` is `write_w == write_h ==
    // sb_px`, so a rect half reads its `delta_q` group even when skipped.
    write_delta_q(enc, cdfs, (mi_r, mi_c), false, block.skip);
    let ii_ctx = intra_inter_ctx(
        has_above,
        has_left,
        neighbours.above_inter[mi_c],
        neighbours.left_inter[mi_r],
    );
    enc.symbol(1, &mut cdfs.intra_inter[ii_ctx]);
    write_comp_mode(
        enc,
        cdfs,
        neighbours,
        (mi_r, mi_c),
        (has_above, has_left),
        info.ref1.is_some(),
    );
    let (mv, mv1, is_new_mv) = if info.ref1.is_some() {
        write_compound_block(
            enc,
            cdfs,
            neighbours,
            grid,
            (mi_r, mi_c),
            (w_mi, h_mi),
            (has_above, has_left),
            info,
            (mi_cols as usize, mi_rows as usize),
        )?
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
            mi_r,
            mi_c,
            w_mi,
            h_mi,
            info.ref_frame,
            mi_cols as usize,
            mi_rows as usize,
        );
        let (mv, is_new_mv) = write_inter_mode(enc, cdfs, info, &stack)?;
        (mv, (0, 0), is_new_mv)
    };
    for dr in 0..h_mi {
        for dc in 0..w_mi {
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
                    size: w_mi as u8,
                    size_h: h_mi as u8,
                    is_global_mv0: false,
                    is_global_mv1: false,
                },
            );
        }
    }
    if info.ref1.is_none() {
        write_motion_mode(
            enc,
            cdfs,
            grid,
            (mi_r, mi_c),
            (w_mi, h_mi),
            (w, h),
            (mi_cols as usize, mi_rows as usize),
            info.ref_frame,
            block.motion_mode,
        )?;
    }
    // The 64x64 mu chunks the residual is coded in, raster order -- the
    // decoder's own walk: two for a half, one for a square piece.
    let chunks: &[(usize, usize)] = match (w, h) {
        (128, 64) => &[(0, 0), (0, 1)],
        (64, 128) => &[(0, 0), (1, 0)],
        _ => &[(0, 0)],
    };
    // A half's uv plane block (64x32 / 32x64) is wider or taller than its
    // 32x32 unit, so the unit's `txb_skip_ctx` takes `get_txb_ctx`'s
    // offset-10 rows (+3 here); a 64x64 piece's uv block IS the unit and
    // takes none. The half is also the only shape whose chroma is re-stamped
    // per mu chunk at the end (decode.rs' `mu_chroma`).
    let is_half = w.max(h) > 64;
    if tx_select {
        if block.skip {
            // `read_block_tx_size_rect`'s skip arm: `txfm_partition_update_rect`
            // with the BLOCK's own size in both bands, no symbol.
            neighbours.record_txfm_rect((mi_r, mi_c), (w, h), (w, h));
        } else if is_half {
            // The var-tx tree of a 128x64 / 64x128 block enters at TX_64X64
            // once per mu chunk (`read_block_tx_size_rect`'s `max_tx = 64`
            // loop), so it is two depth-0 `txfm_split` flags of zero, each
            // read against the bands the chunks before it published.
            for &(cr, cc) in chunks {
                let unit = (mi_r + cr * 16, mi_c + cc * 16);
                let ctx = txfm_partition_ctx_rect(
                    neighbours.above_txfm[unit.1],
                    neighbours.left_txfm[unit.0],
                    w.max(h),
                    64,
                    64,
                );
                enc.symbol(0, &mut cdfs.txfm_partition[ctx]);
                neighbours.record_txfm(unit, 64, 64, 64);
            }
        } else {
            // A 64x64 piece is one whole TX_64X64: the single depth-0 flag
            // the 64 root's own writer codes (`write_tx_syntax_inter`).
            write_tx_syntax_inter(enc, cdfs, neighbours, (mi_r, mi_c), SB, true, false, 0);
        }
    }
    let zero_grids = [
        vec![0i32; TX32 * TX32],
        vec![0i32; TX32 * TX32],
        vec![0i32; TX32 * TX32],
    ];
    // A 128 root's half is always SUB (16 px) aligned, so the coarse mode /
    // side bands are named by the same SUB-grid origin every square block
    // uses.
    let at_sub = (mi_r / (SUB / MI), mi_c / (SUB / MI));
    if !block.skip {
        // lane-rectres: the packed 64-wide level grids the search hands over,
        // one plane each (the same layout `write_inter_block_128` slices for
        // the whole block, here with the piece's own chunk list).
        let packed = [
            level_grid(&block.luma, 64)?,
            level_grid(&block.u, 64)?,
            level_grid(&block.v, 64)?,
        ];
        let scan32 = default_scan(TX32);
        let unit_of = |plane: usize, cr: usize, cc: usize| -> Vec<i32> {
            (0..32)
                .flat_map(|row| packed[plane][(cr * 32 + row) * 64 + cc * 32..][..32].to_vec())
                .collect()
        };
        let mut chroma_units: Vec<((usize, usize), [Vec<i32>; 2])> = Vec::new();
        for &(cr, cc) in chunks {
            let unit_mi = (mi_r + cr * 16, mi_c + cc * 16);
            // The luma unit, and the context its all-zero flag reads: a
            // HALF's unit is smaller than its block (neighbour magnitude
            // table), a square's IS the whole plane block (fixed zero --
            // `read_inter_plane`'s `luma_skip_ctx.unwrap_or(0)`, the 64
            // root's own writer rule). It publishes its context before the
            // next chunk reads it.
            let luma = unit_of(0, cr, cc);
            let skip_ctx = if is_half {
                neighbours.luma_skip_ctx(unit_mi, 64 / MI)
            } else {
                0
            };
            let around = neighbours.around_mi(unit_mi, 64);
            write_coeffs(
                enc,
                &mut cdfs.txb(TxbSet::Luma64, 0),
                &luma,
                &scan32,
                skip_ctx,
                dc_sign_ctx(around[0].dc_vote),
                Some(0),
                TxType::DctDct,
            );
            neighbours.record_mi_luma(unit_mi, 64, &luma);
            // Then this chunk's chroma pair, before the next chunk's luma:
            // libaom codes every plane of a mu chunk before moving on.
            let mut pair = [Vec::new(), Vec::new()];
            for plane in 1..3usize {
                let grid = unit_of(plane, cr, cc);
                let around = neighbours.around_mi(unit_mi, 64)[plane];
                write_coeffs(
                    enc,
                    &mut cdfs.txb(TxbSet::Chroma32, 0),
                    &grid,
                    &scan32,
                    usize::from(around.above_coded)
                        + usize::from(around.left_coded)
                        + 3 * usize::from(is_half),
                    dc_sign_ctx(around.dc_vote),
                    Some(plane),
                    TxType::DctDct,
                );
                neighbours.record_mi_chroma(unit_mi, 64, plane, &grid);
                pair[plane - 1] = grid;
            }
            chroma_units.push((unit_mi, pair));
        }
        // decode.rs' own tail (`record_split_luma_rect_mi` then, on a half,
        // the per-chunk chroma re-stamp): the block-level write off the
        // whole assembled chroma plane grid, luma kept because each unit
        // already published its own, and then the per-unit chroma writes
        // replayed over it -- last writer wins, so the next block reads the
        // units.
        let block_grids = [
            vec![0i32; TX32 * TX32],
            packed[1].clone(),
            packed[2].clone(),
        ];
        neighbours.record_planes_rect(at_sub, (w, h), 0, &block_grids, false);
        if is_half {
            for (unit_mi, pair) in &chroma_units {
                neighbours.record_mi_chroma(*unit_mi, 64, 1, &pair[0]);
                neighbours.record_mi_chroma(*unit_mi, 64, 2, &pair[1]);
            }
        }
        SB128_RECTRES_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    } else {
        neighbours.record_planes_rect(at_sub, (w, h), 0, &zero_grids, true);
    }
    neighbours.record_inter_rect_mi((mi_r, mi_c), (w, h), block.skip, true, block_ref(block));
    if let Some(r1) = info.ref1 {
        neighbours.record_compound_rect_mi((mi_r, mi_c), (w, h), r1, 0, 1);
    }
    // lane-ab128: an AB shape codes its two 64x64 pieces through this same
    // function, and they are neither a 128x64 nor a 64x128 half -- the
    // witness reads these two counters for the HORZ/VERT shape alone.
    if w > h {
        SB128_HORZ_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    } else if h > w {
        SB128_VERT_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}

/// lane-b128hv: how many 128x64 / 64x128 halves the inter writer has coded
/// since the last [`take_sb128_horz_hits`] / [`take_sb128_vert_hits`] (class
/// `gate-blind-to-feature`).
static SB128_HORZ_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// The VERT twin of [`SB128_HORZ_HITS`].
static SB128_VERT_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The 128x64-half count since the last call, and zero it.
pub fn take_sb128_horz_hits() -> usize {
    SB128_HORZ_HITS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// The 64x128-half count since the last call, and zero it.
pub fn take_sb128_vert_hits() -> usize {
    SB128_VERT_HITS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// lane-ab128: how many AB-shape roots the inter writer has coded since the
/// last [`take_sb128_ab_hits`], in `PARTITION_HORZ_A` .. `_VERT_B` order
/// (class `gate-blind-to-feature`).
static SB128_AB_HITS: [std::sync::atomic::AtomicUsize; 4] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 4];

/// The four per-shape AB-root counts since the last call, and zero them.
pub fn take_sb128_ab_hits() -> [usize; 4] {
    std::array::from_fn(|i| SB128_AB_HITS[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// lane-b128r: how many of those blocks carried a REAL residual (four
/// TX_64X64 luma units plus their per-chunk chroma pair) rather than being
/// skipped, since the last [`take_sb128_residual_hits`].
static SB128_RESIDUAL_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The 128x128-with-residual count since the last call, and zero it.
pub fn take_sb128_residual_hits() -> usize {
    SB128_RESIDUAL_HITS.swap(0, std::sync::atomic::Ordering::Relaxed)
}
/// lane-rectres: how many RECT/AB pieces (the halves and 64x64 squares
/// [`write_inter_block_128_rect`] codes) carried a REAL residual rather than
/// being skipped, since the last [`take_sb128_rectres_hits`] -- the writer
/// side of [`crate::encode::take_b128_rectres_hits`]'s search count.
static SB128_RECTRES_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The rect/AB-piece-with-residual count since the last call, and zero it.
pub fn take_sb128_rectres_hits() -> usize {
    SB128_RECTRES_HITS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// lane-b128r: how many of those blocks named a SECOND reference (the
/// compound arm at the 128 root), since the last
/// [`take_sb128_compound_hits`].
static SB128_COMPOUND_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The compound-128 count since the last call, and zero it.
pub fn take_sb128_compound_hits() -> usize {
    SB128_COMPOUND_HITS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// How many 128x128 `PARTITION_NONE` blocks the inter writer has coded since
/// the last [`take_sb128_none_hits`] (class `gate-blind-to-feature`).
static SB128_NONE_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The 128x128-block count since the last call, and zero it.
pub fn take_sb128_none_hits() -> usize {
    SB128_NONE_HITS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// `read_lr`'s write side, called once at the top of every superblock,
/// before its partition symbol: writes the units this superblock's own
/// mi span covers (`av1_loop_restoration_corners_in_sb`; no superres, so the
/// column scaling is the plain mi one).
fn write_lr(enc: &mut SymbolEncoder, cdfs: &mut Cdfs, mi_row: u32, mi_col: u32, span: u32) {
    LR_PLAN.with(|c| {
        let mut plan = c.borrow_mut();
        let Some(plan) = plan.as_mut() else { return };
        let unit = plan.unit_size;
        let rcol0 = (mi_col * 4).div_ceil(unit);
        let rcol1 = ((mi_col + span) * 4).div_ceil(unit).min(plan.horz_units);
        let rrow0 = (mi_row * 4).div_ceil(unit);
        let rrow1 = ((mi_row + span) * 4).div_ceil(unit).min(plan.vert_units);
        for rrow in rrow0..rrow1 {
            for rcol in rcol0..rcol1 {
                let info = plan.units[(rcol + rrow * plan.horz_units) as usize];
                enc.symbol(usize::from(info.is_some()), &mut cdfs.restore_wiener);
                if let Some(info) = info {
                    crate::restoration::write_wiener_filter(enc, false, &mut plan.reference, &info);
                }
            }
        }
    });
}

/// A 64x64 superblock's width in 4x4 mode-info units (`SB_MI` in
/// `crate::decode`), the span [`write_lr`] resolves its units over.
const SB_MI_W: u32 = 16;

/// One tile's own superblock and mode-info span (spec 5.11.1's tile loop
/// bounds): `MiColStarts[i]..MiColStarts[i+1]` and the same down the rows,
/// with the superblock indices those mi bounds fall on. The writers walk
/// exactly this rect, and everything a block reads outside it -- the
/// neighbour bands, the MV grid -- is left at the "unavailable" state a
/// tile's own first row and column see, which is how the decoder clips them
/// (`MiGrid::set_tile_bounds`, `PlaneBuf::set_tile_origin`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TileRect {
    pub(crate) sb_col0: u32,
    pub(crate) sb_col1: u32,
    pub(crate) sb_row0: u32,
    pub(crate) sb_row1: u32,
    pub(crate) mi_col0: u32,
    pub(crate) mi_col1: u32,
    pub(crate) mi_row0: u32,
    pub(crate) mi_row1: u32,
}

impl TileRect {
    /// The whole frame as one tile -- what every single-tile caller writes.
    pub(crate) fn whole(mi_cols: u32, mi_rows: u32) -> Self {
        let (cols, rows) = block_grid(mi_cols, mi_rows);
        Self {
            sb_col0: 0,
            sb_col1: cols.div_ceil(2),
            sb_row0: 0,
            sb_row1: rows.div_ceil(2),
            mi_col0: 0,
            mi_col1: mi_cols,
            mi_row0: 0,
            mi_row1: mi_rows,
        }
    }
}

/// A frame's uniform tile grid (spec 5.9.15's `uniform_tile_spacing_flag`
/// arm, mirrored from `ec_av1_syntax::frame::read_tile_info` so that what
/// this encoder writes and what a decoder derives from the same two log2
/// fields are the same rects). 64x64 superblocks only, which is the only
/// size this encoder's sequence header signals.
#[derive(Clone, Debug)]
pub(crate) struct TileLayout {
    pub(crate) cols_log2: u32,
    pub(crate) rows_log2: u32,
    /// Tile column starts in superblocks, `cols + 1` entries.
    col_starts_sb: Vec<u32>,
    /// Tile row starts in superblocks, `rows + 1` entries.
    row_starts_sb: Vec<u32>,
    mi_cols: u32,
    mi_rows: u32,
}

impl TileLayout {
    /// The layout `cols_log2`/`rows_log2` name for this frame's mi grid.
    pub(crate) fn new(mi_cols: u32, mi_rows: u32, cols_log2: u32, rows_log2: u32) -> Self {
        let (cols32, rows32) = block_grid(mi_cols, mi_rows);
        let (sb_cols, sb_rows) = (cols32.div_ceil(2), rows32.div_ceil(2));
        // lane-sb128: a tile boundary is a SUPERBLOCK boundary, so under a
        // 128 superblock the starts are laid out on the 128 grid and then
        // doubled back into the 64-cell units the writers walk.
        let unit = u32::from(crate::encode::sb128_on()) + 1;
        let starts = |total: u32, log2: u32| {
            let total128 = total.div_ceil(unit);
            let step = total128.div_ceil(1 << log2).max(1);
            let mut v: Vec<u32> = (0..total128)
                .step_by(step as usize)
                .map(|s| s * unit)
                .collect();
            v.push(total);
            v
        };
        Self {
            cols_log2,
            rows_log2,
            col_starts_sb: starts(sb_cols, cols_log2),
            row_starts_sb: starts(sb_rows, rows_log2),
            mi_cols,
            mi_rows,
        }
    }

    pub(crate) fn cols(&self) -> u32 {
        self.col_starts_sb.len() as u32 - 1
    }

    pub(crate) fn rows(&self) -> u32 {
        self.row_starts_sb.len() as u32 - 1
    }

    pub(crate) fn count(&self) -> usize {
        (self.cols() * self.rows()) as usize
    }

    /// `MiColStarts`/`MiRowStarts` as the frame header carries them.
    pub(crate) fn mi_col_starts(&self) -> Vec<u32> {
        let mut v: Vec<u32> = self.col_starts_sb[..self.cols() as usize]
            .iter()
            .map(|sb| sb * SB_MI_W)
            .collect();
        v.push(self.mi_cols);
        v
    }

    pub(crate) fn mi_row_starts(&self) -> Vec<u32> {
        let mut v: Vec<u32> = self.row_starts_sb[..self.rows() as usize]
            .iter()
            .map(|sb| sb * SB_MI_W)
            .collect();
        v.push(self.mi_rows);
        v
    }

    /// Tile `index` (row major, `TileNum`) as a rect.
    pub(crate) fn rect(&self, index: usize) -> TileRect {
        let (r, c) = (index / self.cols() as usize, index % self.cols() as usize);
        let (sb_col0, sb_col1) = (self.col_starts_sb[c], self.col_starts_sb[c + 1]);
        let (sb_row0, sb_row1) = (self.row_starts_sb[r], self.row_starts_sb[r + 1]);
        TileRect {
            sb_col0,
            sb_col1,
            sb_row0,
            sb_row1,
            mi_col0: sb_col0 * SB_MI_W,
            mi_col1: (sb_col1 * SB_MI_W).min(self.mi_cols),
            mi_row0: sb_row0 * SB_MI_W,
            mi_row1: (sb_row1 * SB_MI_W).min(self.mi_rows),
        }
    }

    /// The largest tile by mi area, `context_update_tile_id`'s own rule
    /// (spec 6.8.14 leaves the choice to the encoder; libaom and rav1e both
    /// name the biggest tile, whose end-of-tile tables the frame stores).
    pub(crate) fn largest_tile(&self) -> usize {
        (0..self.count())
            .max_by_key(|&i| {
                let t = self.rect(i);
                ((t.mi_col1 - t.mi_col0) as u64) * ((t.mi_row1 - t.mi_row0) as u64)
            })
            .unwrap_or(0)
    }
}

/// lane-edge128: which 64x64 CDEF unit's `cdef_idx` literal covers each unit
/// of this frame, in the same raster order [`arm_cdef_idx`]'s grid is in.
///
/// `read_cdef` (spec 5.11.56) codes ONE literal for the block that first
/// reaches a unit and copies it over every unit that block SPANS, so a whole
/// 128x128 root block puts one literal in the stream for all four of its
/// units -- the encoder's CDEF search, which prices a preset per unit, may
/// not hand those four units different presets: only the first would be
/// coded, and every decoder (ours and ffmpeg's alike) would apply it to all
/// four while the encoder's own filter replay applied four different ones.
/// That is the shape this map exists to collapse
/// ([`crate::filter_search::pick_filters`]).
///
/// lane-rectres: the same holds for a Rect128 half or an Ab128 half that
/// carries a residual (`EC_AV1_RECTRES`) -- it codes one literal at its own
/// origin and the decoder stamps BOTH units it spans with it, so its pair is
/// collapsed onto the origin unit too. Its 64x64 squares cover exactly one
/// unit each, and a SKIP-ONLY rect/AB piece codes no literal at all (the
/// decoder's per-piece `cdef_transmitted` reset never fires), so neither
/// touches this map -- only pieces the writer really codes a residual for.
pub(crate) fn cdef_unit_owner(blocks: &[Quadrant], mi_cols: u32, mi_rows: u32) -> Vec<u32> {
    let (cols, rows) = block_grid(mi_cols, mi_rows);
    let (sb_cols, sb_rows) = (cols.div_ceil(2) as usize, rows.div_ceil(2) as usize);
    let mut owner: Vec<u32> = (0..(sb_cols * sb_rows) as u32).collect();
    // Collapses every 64x64 unit `(ur, uc) ..` a residual piece at unit
    // `(ur, uc)` with footprint `(w, h)` spans onto the piece's origin --
    // [`crate::encode::write_inter_block_128_rect`]'s one `write_cdef_idx`
    // is what the decoder copies over all of them.
    let collapse = |owner: &mut Vec<u32>, (ur, uc): (usize, usize), (w, h): (usize, usize)| {
        let origin = (ur * sb_cols + uc) as u32;
        for r in ur..(ur + (h / 64).max(1)).min(sb_rows) {
            for c in uc..(uc + (w / 64).max(1)).min(sb_cols) {
                owner[r * sb_cols + c] = origin;
            }
        }
    };
    for sb_r in (0..sb_rows).step_by(2) {
        for sb_c in (0..sb_cols).step_by(2) {
            let Some(root) = ((sb_r * 2) < rows as usize && (sb_c * 2) < cols as usize)
                .then(|| &blocks[(sb_r * 2) * cols as usize + sb_c * 2])
            else {
                continue;
            };
            match root {
                Quadrant::Whole128(_) => {
                    let origin = (sb_r * sb_cols + sb_c) as u32;
                    for r in sb_r..(sb_r + 2).min(sb_rows) {
                        for c in sb_c..(sb_c + 2).min(sb_cols) {
                            owner[r * sb_cols + c] = origin;
                        }
                    }
                }
                // lane-rectres: the two halves of a HORZ/VERT root sit in
                // different unit rows (HORZ) / columns (VERT); each
                // non-skip half collapses its own pair.
                Quadrant::Rect128(horz, halves) => {
                    for (i, half) in halves.iter().enumerate() {
                        if half.skip {
                            continue;
                        }
                        let (ur, uc) = if *horz {
                            (sb_r + i, sb_c)
                        } else {
                            (sb_r, sb_c + i)
                        };
                        let (w, h) = if *horz { (128, 64) } else { (64, 128) };
                        collapse(&mut owner, (ur, uc), (w, h));
                    }
                }
                // lane-rectres: the AB half is the only multi-unit piece
                // (its two squares are one unit each); only a non-skip one
                // codes a literal, so only it fires.
                Quadrant::Ab128(symbol, pieces) => {
                    for (piece, ((mi_r, mi_c), (w, h))) in pieces
                        .iter()
                        .zip(crate::encode::ab128_pieces(usize::from(*symbol), (0, 0)))
                    {
                        if piece.skip {
                            continue;
                        }
                        let (ur, uc) = (sb_r + mi_r / 16, sb_c + mi_c / 16);
                        collapse(&mut owner, (ur, uc), (w, h));
                    }
                }
                _ => {}
            }
        }
    }
    owner
}

/// One frame's per-superblock `cdef_idx` plan (see [`arm_cdef_idx`]).
struct CdefIdxPlan {
    /// The header's `cdef_bits`; never 0 while armed.
    bits: u8,
    /// Superblocks per row, the stride of `grid`.
    sb_cols: usize,
    /// One index per 64x64 superblock, in raster order.
    grid: Vec<u8>,
    /// The superblock whose literal was already written -- the decoder's
    /// `cdef_transmitted`, which resets at every superblock and which coding
    /// order (each superblock's blocks are contiguous) makes a single slot.
    last: Option<usize>,
}

thread_local! {
    static CDEF_IDX: std::cell::RefCell<Option<CdefIdxPlan>> =
        const { std::cell::RefCell::new(None) };
}

/// Arms the next tile write with one `cdef_idx` per 64x64 superblock.
/// `bits == 0` disarms (the header codes a single strength pair, so the tile
/// carries no literal at all).
pub(crate) fn arm_cdef_idx(bits: u8, sb_cols: usize, grid: Vec<u8>) {
    CDEF_IDX.with(|c| {
        *c.borrow_mut() = (bits > 0).then_some(CdefIdxPlan {
            bits,
            sb_cols,
            grid,
            last: None,
        });
    });
}

/// One frame's per-superblock quantizer plan (lane-deltaq, see
/// [`arm_delta_q`]): the writer side of decode.rs `maybe_read_delta_q`.
struct DeltaQPlan {
    /// `1 << delta_q_res`, the multiplier the coded symbol is scaled by --
    /// every `grid` entry is congruent to `base` modulo it, so the delta the
    /// writer codes is always a whole number of steps.
    res: i32,
    /// Superblocks per row, the stride of `grid`.
    sb_cols: usize,
    /// The absolute `qindex` each 64x64 superblock is coded at, raster order.
    grid: Vec<u8>,
    /// The spec's `CurrentQIndex`, carried across the superblocks of THIS
    /// tile -- armed at the frame's `base_q_idx` because a tile writer is
    /// armed once per tile (`encode_inter_frame`'s `code_tiles`), which is
    /// exactly the spec's own per-tile reset.
    cur: i32,
}

thread_local! {
    static DELTA_Q: std::cell::RefCell<Option<DeltaQPlan>> =
        const { std::cell::RefCell::new(None) };
}

/// Arms the next tile write with one absolute `qindex` per 64x64 superblock.
/// Disarmed (a plain no-op writer, byte for byte what this crate wrote before
/// lane-deltaq) when `res == 0`.
pub(crate) fn arm_delta_q(res: i32, sb_cols: usize, grid: Vec<u8>, base: u8) {
    DELTA_Q.with(|c| {
        *c.borrow_mut() = (res > 0).then(|| DeltaQPlan {
            res,
            sb_cols,
            grid,
            cur: i32::from(base),
        });
    });
}

thread_local! {
    static SIGN_BIAS: std::cell::Cell<crate::mvstack::SignBiasTable> =
        const { std::cell::Cell::new(crate::mvstack::NO_SIGN_BIAS) };
}

/// Arms the next tile write with this frame's `ref_frame_sign_bias` (spec
/// 5.9.2), the table its MV-stack scans run under. Armed the same way
/// [`arm_cdef_idx`]/[`arm_lr`] are -- and disarmed by the same guard -- so
/// that a frame with a backward reference needs no new parameter on the four
/// nested writer helpers between here and `find_mv_stack`. A frame that never
/// names a future reference leaves this alone: all-`false` is what every
/// stream this crate wrote before pyramids existed codes under.
pub(crate) fn arm_sign_bias(sign_bias: crate::mvstack::SignBiasTable) {
    SIGN_BIAS.with(|c| c.set(sign_bias));
}

thread_local! {
    static SCREEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Arms the next tile write with this frame's `allow_screen_content_tools`
/// (spec 5.9.2), which is what makes every intra block carry the
/// `palette_y_mode`/`palette_uv_mode` symbols spec 5.11.46
/// `read_palette_mode_info` reads. Armed the same way [`arm_cdef_idx`]/
/// [`arm_sign_bias`] are -- and disarmed by the same guard -- so a frame that
/// never detected screen content writes exactly the stream it wrote before
/// this lane.
pub(crate) fn arm_screen(on: bool) {
    SCREEN.with(|c| c.set(on));
}

thread_local! {
    /// This frame's `(mi_rows, mi_cols)` (spec `compute_image_size`), armed
    /// beside [`SCREEN`] by both frame encoders: a palette block cut by the
    /// frame's right/bottom mi edge codes its colour-index map over the
    /// ON-SCREEN part only, so the writer needs the same dims the decoder's
    /// `palette_onscreen` reads off its `FrameCtx` (lane-palw). `(0, 0)`
    /// -- an unarmed writer, i.e. a unit test coding one block -- clamps
    /// nothing, which is what every stream without a cut palette block codes.
    static FRAME_MI_DIMS: std::cell::Cell<(usize, usize)> =
        const { std::cell::Cell::new((0, 0)) };
}

/// Arms the next tile write with this frame's `(mi_rows, mi_cols)`.
pub(crate) fn arm_frame_mi_dims(dims: (usize, usize)) {
    FRAME_MI_DIMS.with(|c| c.set(dims));
}

/// The armed `(mi_rows, mi_cols)`, or a grid big enough to clamp nothing.
fn frame_mi_dims() -> (usize, usize) {
    match FRAME_MI_DIMS.with(std::cell::Cell::get) {
        (0, 0) => (usize::MAX / 8, usize::MAX / 8),
        dims => dims,
    }
}

fn screen_armed() -> bool {
    SCREEN.with(std::cell::Cell::get)
}

thread_local! {
    /// This frame header's `allow_intrabc` (spec 5.9.2), armed exactly like
    /// [`SCREEN`]: with it every intra block of the tile carries a
    /// `use_intrabc` symbol right after `skip`/`cdef`/`delta_q` (decode.rs
    /// `read_intra_mode`), so a tile written without it and read with it
    /// desyncs at the first block.
    static INTRABC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The mi grid the DV predictor is built off (decode.rs
    /// `INTRABC_MI_GRID`/`record_intrabc_mi`): every coded block publishes
    /// its own footprint into it, an intrabc one also its DV as an
    /// `INTRA_FRAME` candidate. `Some` only while an `allow_intrabc` tile is
    /// being written.
    static INTRABC_GRID: std::cell::RefCell<Option<(crate::mvstack::MiGrid, usize, usize)>> =
        const { std::cell::RefCell::new(None) };
}

/// Arms the frame's `allow_intrabc` bit for the next tile written on this
/// thread; cleared by [`CdefIdxGuard`] when that writer returns.
pub(crate) fn arm_intrabc(on: bool) {
    INTRABC.with(|c| c.set(on));
}

fn intrabc_armed() -> bool {
    INTRABC.with(std::cell::Cell::get)
}

thread_local! {
    /// This SEQUENCE header's `enable_filter_intra` (spec 5.5.2), armed
    /// exactly like [`SCREEN`]: with it every DC_PRED intra block of at most
    /// 32x32 that took no luma palette carries a `use_filter_intra` symbol
    /// after the palette colours and before the colour-index maps (decode.rs
    /// `read_intra_mode`), so a tile written without it and read with it
    /// desyncs at the first such block.
    static FILTER_INTRA: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Arms the sequence's `enable_filter_intra` bit for the next tile written on
/// this thread; cleared by [`CdefIdxGuard`] when that writer returns.
pub(crate) fn arm_filter_intra(on: bool) {
    FILTER_INTRA.with(|c| c.set(on));
}

fn filter_intra_armed() -> bool {
    FILTER_INTRA.with(std::cell::Cell::get)
}

/// How many blocks this writer coded `use_filter_intra == 1`, and the
/// histogram of the five `filter_intra_mode`s (index 1..=5) -- the fire count
/// a gate prints so that "filter intra is on" is a measurement rather than a
/// claim (class `gate-blind-to-feature`). Index 0 is the block count.
static FILTER_INTRA_HITS: [std::sync::atomic::AtomicUsize; 6] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 6];

/// Reads [`FILTER_INTRA_HITS`] and zeroes it, so a gate can attribute the
/// counts to its own encode.
#[cfg(test)]
pub(crate) fn take_filter_intra_hits() -> [usize; 6] {
    std::array::from_fn(|i| FILTER_INTRA_HITS[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// `filter_intra_mode_info` (spec 5.11.14) written out, mirrored symbol for
/// symbol from decode.rs `read_intra_mode`: a `use_filter_intra` flag on every
/// DC_PRED block whose sides `av1_filter_intra_allowed_bsize` admits (both <=
/// 32, [`crate::decode::filter_intra_size_class`]) and that took no luma
/// palette (`av1_filter_intra_allowed`, reconintra.h:77), then the chosen mode
/// of five. Sits between the palette COLOUR lists and the colour-index maps,
/// where libaom reads it (`av1_visit_palette` runs after
/// `read_filter_intra_mode_info`).
fn write_filter_intra(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    mode: usize,
    side: usize,
    has_palette_y: bool,
    filter_intra: Option<u8>,
) {
    if !filter_intra_armed() || mode != DC_PRED || has_palette_y {
        debug_assert!(
            filter_intra.is_none(),
            "no `use_filter_intra` symbol exists here"
        );
        return;
    }
    let Some(class) = crate::decode::filter_intra_size_class(side) else {
        debug_assert!(
            filter_intra.is_none(),
            "past av1_filter_intra_allowed_bsize"
        );
        return;
    };
    enc.symbol(
        usize::from(filter_intra.is_some()),
        &mut cdfs.filter_intra[class],
    );
    if let Some(fi) = filter_intra {
        enc.symbol(usize::from(fi), &mut cdfs.filter_intra_mode);
        FILTER_INTRA_HITS[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        FILTER_INTRA_HITS[usize::from(fi) + 1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The coefficient-table row a block's luma transform types are coded from:
/// `fimode_to_intradir[filter_intra_mode]` for a filter-intra block, its own
/// luma mode otherwise (class `filter-intra tx_type row`, decode.rs
/// [`crate::decode::fi_tx_row`]). The block still PUBLISHES `DC_PRED` as its
/// neighbour mode -- libaom's `mbmi->mode` is unchanged by filter intra.
fn tx_row(mode: usize, filter_intra: Option<u8>) -> usize {
    crate::decode::fi_tx_row(mode, filter_intra.map(usize::from))
}

/// How many blocks this writer coded `use_intrabc == 1`, and the histogram of
/// their DV magnitudes in full pels (index 1..=6 = 1, 2, 4, 8, 16, 32 pels
/// and up, by the larger component) -- the fire count a gate prints so that
/// "intrabc is on" is a measurement rather than a claim (class
/// `gate-blind-to-feature`). Index 0 is the block count.
static INTRABC_HITS: [std::sync::atomic::AtomicUsize; 7] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 7];

fn note_intrabc(dv: (i32, i32)) {
    INTRABC_HITS[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let px = (dv.0.abs() / 8).max(dv.1.abs() / 8).max(1) as usize;
    let bucket = (usize::BITS - px.leading_zeros()) as usize;
    INTRABC_HITS[bucket.clamp(1, 6)].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Reads [`INTRABC_HITS`] and zeroes it, so a gate can attribute the counts
/// to its own encode.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn take_intrabc_hits() -> [usize; 7] {
    std::array::from_fn(|i| INTRABC_HITS[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

thread_local! {
    /// This frame header's own `reference_select` bit (spec 5.9.22), armed
    /// the same way `arm_sign_bias` arms the sign-bias table rather than
    /// threaded through the five nested writer helpers between here and the
    /// per-block mode syntax. `true` makes every inter block of size at least
    /// 8x8 code a `comp_mode` symbol, exactly where decode.rs `read_comp_mode`
    /// reads one.
    static REFERENCE_SELECT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Arms the frame's `reference_select` bit for the next tile written on this
/// thread; cleared by [`CdefIdxGuard`] when that writer returns.
pub(crate) fn arm_reference_select(on: bool) {
    REFERENCE_SELECT.with(|c| c.set(on));
}

thread_local! {
    /// This frame header's own `is_motion_mode_switchable` bit (spec 5.9.2),
    /// armed exactly like [`REFERENCE_SELECT`]. `true` makes every
    /// single-reference inter block that libaom's `motion_mode_allowed`
    /// accepts carry a `motion_mode`/`obmc` symbol right after its MV syntax,
    /// exactly where decode.rs' `read_motion_mode` reads one.
    static MOTION_MODE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

thread_local! {
    /// This frame header's own `allow_warped_motion` bit (spec 5.9.2): with
    /// it set, every block libaom's `motion_mode_allowed` accepts AND that
    /// has at least one warp sample (`num_proj_ref >= 1`) reads the 3-symbol
    /// `motion_mode_cdf` alphabet instead of the 2-symbol `obmc_cdf` one.
    /// lane-av1obmc2.
    static WARPED_MOTION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The frame geometry the warp-sample walk needs (`num_proj_ref` ->
    /// `has_top_right`), which is only ever this writer's own superblock
    /// size. Every stream this crate writes uses a 64 superblock, which is
    /// `FrameCtx::new`'s default.
    /// corner-cut: ceiling is a 128-superblock WRITER -- it must arm the real
    /// frame context here (or take one as a parameter) or the warp-sample
    /// walk reads the wrong `has_top_right` geometry.
    static WRITER_FCTX: crate::decode::FrameCtx = crate::decode::FrameCtx::new();
}

/// Arms [`WARPED_MOTION`] for the next tile written on this thread.
pub(crate) fn arm_warped_motion(on: bool) {
    WARPED_MOTION.with(|c| c.set(on));
}

/// Arms the frame's `is_motion_mode_switchable` bit for the next tile written
/// on this thread; cleared by [`CdefIdxGuard`] when that writer returns.
pub(crate) fn arm_motion_mode(on: bool) {
    MOTION_MODE.with(|c| c.set(on));
}

/// How many single-reference inter blocks this writer coded a `motion_mode`
/// symbol for, split by the value it coded and by the block's own footprint
/// -- the histogram a gate prints so that "OBMC is on" is a measurement, not
/// a claim (class `gate-blind-to-feature`). Index: `0 + 2 * bucket` for
/// SIMPLE, `1 + 2 * bucket` for OBMC, bucket 0/1/2 = 32x32/16x16/8x8.
static MOTION_MODE_HITS: [std::sync::atomic::AtomicUsize; 9] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 9];

/// Reads [`MOTION_MODE_HITS`] and zeroes it, so a gate can attribute the
/// counts to its own encode.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn take_motion_mode_hits() -> [usize; 9] {
    std::array::from_fn(|i| MOTION_MODE_HITS[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// What the 3-symbol `motion_mode` symbol costs, in bits, against the same
/// frame tables [`obmc_symbol_bits`] prices the 2-symbol alphabet off --
/// which alphabet a block reads is `motion_mode_allowed`'s own choice
/// ([`write_motion_mode`]).
pub(crate) fn motion_mode_symbol_bits(row: usize, motion: u8) -> f64 {
    PRICING_BASE.with_borrow(|slot| match slot.as_deref() {
        Some(s) => crate::encode::symbol_bits(&s.0.motion_mode[row], usize::from(motion)),
        None => crate::encode::symbol_bits(&crate::cdf::MOTION_MODE[row], usize::from(motion)),
    })
}

/// What the `motion_mode`/`obmc` symbol costs, in bits, against the tables
/// this frame's writer really starts from ([`arm_pricing_cdfs`]) -- the same
/// tables the coefficients are priced off since lane-av1price2, rather than
/// the static defaults the symbol used to be estimated with. Falls back to
/// the default table whenever the search is armed with the defaults (a key
/// frame, or a screen frame the gate turned off).
pub(crate) fn obmc_symbol_bits(row: usize, obmc: bool) -> f64 {
    PRICING_BASE.with_borrow(|slot| match slot.as_deref() {
        Some(s) => crate::encode::symbol_bits(&s.0.obmc[row], usize::from(obmc)),
        None => crate::encode::symbol_bits(&crate::cdf::OBMC[row], usize::from(obmc)),
    })
}

/// `read_motion_mode`'s write side (spec 5.11.24), called on a
/// single-reference inter block right after its MV syntax and before the
/// (never switchable here) interpolation filter -- libaom's own sequential
/// order in `read_inter_block_mode_info`.
///
/// Eligibility is decode.rs' own `motion_mode_eligible`, term for term, with
/// the terms this writer settles statically dropped: no block it codes is
/// `skip_mode`, interintra or a non-IDENTITY-global `GLOBALMV` one, so what
/// is left is the footprint floor and the overlappable-neighbour walk
/// ([`crate::decode::has_overlappable_neighbour`], shared with the decoder so
/// the two cannot drift). The frame header keeps `allow_warped_motion == 0`,
/// so the alphabet is always the 2-symbol `obmc_cdf`, never the 3-symbol
/// `motion_mode_cdf`.
// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
fn write_motion_mode(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    grid: &MiGrid,
    (mi_row, mi_col): (usize, usize),
    (bw4, bh4): (usize, usize),
    (write_w, write_h): (usize, usize),
    (mi_cols, mi_rows): (usize, usize),
    // This block's own reference, for the warp-sample walk that picks the
    // alphabet (`num_proj_ref`).
    ref_frame: i8,
    // 0 SIMPLE_TRANSLATION, 1 OBMC_CAUSAL, 2 WARPED_CAUSAL -- what the
    // encoder's search committed for this block.
    motion: u8,
) -> Result<()> {
    let eligible = MOTION_MODE.with(std::cell::Cell::get)
        && write_w.min(write_h) >= 8
        && crate::decode::has_overlappable_neighbour(
            grid, mi_row, mi_col, bw4, bh4, mi_cols, mi_rows,
        );
    if crate::envflags::env_flag!("EC_TRACE_MODE_STEP") {
        let proj = WRITER_FCTX.with(|fctx| {
            crate::decode::num_proj_ref(
                grid, mi_row, mi_col, bw4, bh4, mi_cols, mi_rows, ref_frame, fctx,
            )
        });
        eprintln!(
            "EC_MMW mi_row={mi_row} mi_col={mi_col} w={write_w} h={write_h} elig={} proj={proj} warp_armed={} motion={motion}",
            eligible as u8,
            WARPED_MOTION.with(std::cell::Cell::get) as u8,
        );
    }
    if !eligible {
        // The encoder trialled an OBMC prediction for a block this writer
        // codes as SIMPLE_TRANSLATION: its reconstruction is not the
        // decoder's (class encoder-grid-drift), which is exactly what the
        // `EC_COMP_MISMATCH` rung exists to catch.
        if motion != 0 && crate::envflags::env_flag!("EC_COMP_MISMATCH") {
            eprintln!(
                "EC_COMP_MISMATCH motion_mode enc=OBMC wrote=SIMPLE                  mi=({mi_row},{mi_col}) wh=({write_w}x{write_h})"
            );
        }
        return Ok(());
    }
    let row = crate::decode::motion_mode_cdf_row(write_w, write_h).ok_or_else(|| {
        Error::unsupported(
            "AV1 tile",
            format!("a {write_w}x{write_h} block has no motion_mode CDF row"),
        )
    })?;
    // libaom `motion_mode_allowed`: the 3-symbol `motion_mode_cdf` alphabet
    // exactly when `allow_warped_motion` is on and the block has at least one
    // warp sample. The other two vetoes libaom applies there cannot fire for
    // a stream this encoder writes -- it codes no scaled reference and no
    // inter frame with `force_integer_mv` -- and the decoder checks all three
    // (`decode.rs`'s `warp_eligible`), so the two sides agree term for term.
    let warp_alphabet = WARPED_MOTION.with(std::cell::Cell::get)
        && WRITER_FCTX.with(|fctx| {
            crate::decode::num_proj_ref(
                grid, mi_row, mi_col, bw4, bh4, mi_cols, mi_rows, ref_frame, fctx,
            )
        }) >= 1;
    if warp_alphabet {
        enc.symbol(usize::from(motion), &mut cdfs.motion_mode[row]);
    } else {
        // A warp winner whose block the writer resolves to the 2-symbol
        // alphabet would reconstruct as SIMPLE at the decoder: the same
        // encoder-grid-drift rung the OBMC case above prints.
        if motion == 2 && crate::envflags::env_flag!("EC_COMP_MISMATCH") {
            eprintln!(
                "EC_COMP_MISMATCH motion_mode enc=WARP wrote=SIMPLE mi=({mi_row},{mi_col}) wh=({write_w}x{write_h})"
            );
        }
        enc.symbol(usize::from(motion == 1), &mut cdfs.obmc[row]);
    }
    let bucket = match write_w.min(write_h) {
        32.. => 0,
        16 => 1,
        _ => 2,
    };
    MOTION_MODE_HITS[usize::from(motion.min(2)) + 3 * bucket]
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// The compound half of a block's end-of-block neighbour record: nothing for
/// a single-reference or intra block, and `record_compound_mi`'s overwrite of
/// the ALTREF default for a compound one (this writer only codes
/// `comp_group_idx == 0`, `compound_idx == 1`).
fn record_block_compound(
    neighbours: &mut Neighbours,
    at_mi: (usize, usize),
    side: usize,
    block: &BlockCoeffs,
) {
    if let Some(r1) = block.inter.and_then(|i| i.ref1) {
        neighbours.record_compound_mi(at_mi, side, r1, 0, 1);
    }
}

/// decode.rs' own `is_uni_comp_ref`: a pair is `UNIDIR_COMP_REFERENCE`
/// exactly when both references sit on the same temporal side.
use crate::mvstack::is_uni_comp_ref;

thread_local! {
    /// `(order_hint_bits, this frame's order hint, each reference's order
    /// hint)` -- what `get_comp_index_context` (decode.rs) needs to context
    /// the `compound_idx` symbol, armed the same way the sign-bias table is.
    static ORDER_HINTS: std::cell::Cell<(u32, u32, [u32; 7])> =
        const { std::cell::Cell::new((7, 0, [0; 7])) };
}

/// Arms this frame's order hints for the next tile written on this thread.
pub(crate) fn arm_order_hints(bits: u32, order_hint: u32, hints: [u32; 7]) {
    ORDER_HINTS.with(|c| c.set((bits, order_hint, hints)));
}

/// What [`arm_order_hints`] last armed: `(order_hint_bits, order_hint,
/// ref_order_hints)`, for the encoder's own per-reference distance.
pub(crate) fn order_hints() -> (u32, u32, [u32; 7]) {
    ORDER_HINTS.with(std::cell::Cell::get)
}

thread_local! {
    /// lane-hpmv: this frame's `allow_high_precision_mv`, armed per worker
    /// beside [`arm_order_hints`] -- the writer's eighth-pel symbol, the
    /// pricer's mv bits and the motion search's finest step all read it.
    static HIGH_PRECISION_MV: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Arms this frame's `allow_high_precision_mv` for the next tile written --
/// or searched -- on this thread.
pub(crate) fn arm_high_precision_mv(on: bool) {
    HIGH_PRECISION_MV.with(|c| c.set(on));
}

/// What [`arm_high_precision_mv`] last armed on this thread.
pub(crate) fn high_precision_mv() -> bool {
    HIGH_PRECISION_MV.with(std::cell::Cell::get)
}

/// lane-hpmv's witness counters: `[0]` counts every `mv_hp`/`mv_class0_hp`
/// symbol this process's writers coded (zero without
/// `allow_high_precision_mv`, one per non-zero mv component with it), `[1]`
/// counts the ones that came out ZERO -- an eighth-pel vector, i.e. one a
/// frame without the flag cannot name at all.
static HP_SYMBOLS: [std::sync::atomic::AtomicU64; 2] = [
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
];

/// Reads and clears [`HP_SYMBOLS`]: `(hp symbols coded, eighth-pel ones)`.
#[allow(dead_code)] // read only from the `#[cfg(test)]` witness
pub(crate) fn take_hp_symbols() -> (u64, u64) {
    let all = HP_SYMBOLS[0].swap(0, std::sync::atomic::Ordering::Relaxed);
    (
        all,
        HP_SYMBOLS[1].swap(0, std::sync::atomic::Ordering::Relaxed),
    )
}

/// [`take_hp_symbols`] without clearing -- what the NEWMV census prints.
pub(crate) fn hp_symbols() -> (u64, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    (HP_SYMBOLS[0].load(Relaxed), HP_SYMBOLS[1].load(Relaxed))
}

/// Writer-side counterpart of decode.rs `read_compound_ref_frames` (spec
/// 5.11.25's `comp_reference_type`/`uni_comp_ref`/`comp_ref`/`comp_bwdref`
/// trees), symbol for symbol at the same contexts -- which are, as that
/// function's own doc records, `single_ref_p1_ctx`..`single_ref_p6_ctx`/
/// `uni_comp_ref_p1_ctx` under libaom's compound names.
///
/// # Errors
/// A pair this writer never forms (its `ref0` is not `LAST_FRAME`).
fn write_compound_ref_frames(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    above: Option<crate::mvstack::NeighbourRef>,
    left: Option<crate::mvstack::NeighbourRef>,
    above_ref: i8,
    left_ref: i8,
    (ref0, ref1): (i8, i8),
) -> Result<()> {
    use crate::mvstack::{
        ALTREF_FRAME, ALTREF2_FRAME, BWDREF_FRAME, GOLDEN_FRAME, LAST_FRAME, LAST2_FRAME,
        LAST3_FRAME, comp_reference_type_ctx, single_ref_p1_ctx, single_ref_p2_ctx,
        single_ref_p3_ctx, single_ref_p4_ctx, single_ref_p5_ctx, single_ref_p6_ctx,
        uni_comp_ref_p1_ctx,
    };
    let a = (above_ref > 0).then_some(above_ref);
    let l = (left_ref > 0).then_some(left_ref);
    let a1 = above.and_then(|n| n.ref1);
    let l1 = left.and_then(|n| n.ref1);
    let unidir = is_uni_comp_ref(ref0, ref1);
    enc.symbol(
        usize::from(!unidir),
        &mut cdfs.comp_ref_type[comp_reference_type_ctx(above, left)],
    );
    if unidir {
        let both_backward = (ref0, ref1) == (BWDREF_FRAME, ALTREF_FRAME);
        enc.symbol(
            usize::from(both_backward),
            &mut cdfs.uni_comp_ref[single_ref_p1_ctx(a, a1, l, l1)][0],
        );
        if both_backward {
            return Ok(());
        }
        if ref0 != LAST_FRAME {
            return Err(Error::unsupported(
                "AV1 tile",
                "a unidirectional compound pair this writer does not form",
            ));
        }
        let past_last2 = ref1 != LAST2_FRAME;
        enc.symbol(
            usize::from(past_last2),
            &mut cdfs.uni_comp_ref[uni_comp_ref_p1_ctx(a, a1, l, l1)][1],
        );
        if past_last2 {
            enc.symbol(
                usize::from(ref1 == GOLDEN_FRAME),
                &mut cdfs.uni_comp_ref[single_ref_p5_ctx(a, a1, l, l1)][2],
            );
        }
        return Ok(());
    }
    let far = ref0 == LAST3_FRAME || ref0 == GOLDEN_FRAME;
    enc.symbol(
        usize::from(far),
        &mut cdfs.comp_ref[single_ref_p3_ctx(a, a1, l, l1)][0],
    );
    if far {
        enc.symbol(
            usize::from(ref0 == GOLDEN_FRAME),
            &mut cdfs.comp_ref[single_ref_p5_ctx(a, a1, l, l1)][2],
        );
    } else {
        enc.symbol(
            usize::from(ref0 == LAST2_FRAME),
            &mut cdfs.comp_ref[single_ref_p4_ctx(a, a1, l, l1)][1],
        );
    }
    let is_altref = ref1 == ALTREF_FRAME;
    enc.symbol(
        usize::from(is_altref),
        &mut cdfs.comp_bwdref[single_ref_p2_ctx(a, a1, l, l1)][0],
    );
    if !is_altref {
        enc.symbol(
            usize::from(ref1 == ALTREF2_FRAME),
            &mut cdfs.comp_bwdref[single_ref_p6_ctx(a, a1, l, l1)][1],
        );
    }
    Ok(())
}

/// Per-mode and per-pair fire counts of [`write_compound_block`], for the
/// gate's own census (gate-blind-to-feature): modes indexed by the spec's
/// `INTER_COMPOUND_MODES`, pairs by `ref1` (`LAST`..`ALTREF`).
static COMPOUND_MODE_HITS: [std::sync::atomic::AtomicUsize; 8] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 8];
static COMPOUND_PAIR_HITS: [std::sync::atomic::AtomicUsize; 7] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 7];

/// Takes and clears the compound mode histogram.
#[cfg(test)]
pub(crate) fn take_compound_mode_hits() -> [usize; 8] {
    std::array::from_fn(|i| COMPOUND_MODE_HITS[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// Takes and clears the compound pair histogram, indexed by the SECOND
/// reference (`LAST` = 0 .. `ALTREF` = 6).
#[cfg(test)]
pub(crate) fn take_compound_pair_hits() -> [usize; 7] {
    std::array::from_fn(|i| COMPOUND_PAIR_HITS[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// Per-SIZE fire counts of [`write_compound_block`], indexed by
/// `log2(bw4)` (1 = 8x8, 2 = 16x16, 3 = 32x32 and up): compound started as a
/// 32x32-only tool, so the share by size is what says whether the leaf
/// candidates fire at all (gate-blind-to-feature).
static COMPOUND_SIZE_HITS: [std::sync::atomic::AtomicUsize; 4] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 4];

/// Takes and clears the compound size histogram.
#[cfg(test)]
pub(crate) fn take_compound_size_hits() -> [usize; 4] {
    std::array::from_fn(|i| COMPOUND_SIZE_HITS[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// The compound mode histogram of the LEAVES alone (`bw4 <= 4`, so 16x16 and
/// below). The frame-wide histogram above cannot say whether a mode reaches
/// the leaves at all, which is exactly the question every leaf compound step
/// asks (gate-blind-to-feature): before lane-av1comp4 gave a leaf its second
/// reference search, the two half-new modes were structurally impossible
/// there and screen capture's leaf compound was 100% `NEAREST_NEARESTMV`.
static COMPOUND_LEAF_MODE_HITS: [std::sync::atomic::AtomicUsize; 8] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 8];

/// Takes and clears the leaf-only compound mode histogram.
#[cfg(test)]
pub(crate) fn take_compound_leaf_mode_hits() -> [usize; 8] {
    std::array::from_fn(|i| {
        COMPOUND_LEAF_MODE_HITS[i].swap(0, std::sync::atomic::Ordering::Relaxed)
    })
}

/// Writes one COMPOUND_REFERENCE block's whole mode chain -- the writer side
/// of decode.rs' `read_compound_ref_frames` / `read_inter_compound_mode` /
/// `assign_compound_mv` / `comp_group_idx` / `compound_idx` sequence, in that
/// order. Returns the two motion vectors the decoder derives (what the MI
/// grid must record) and whether the mode was a `NEW*` one.
///
/// Only the modes [`InterMode::compound_index`] names are written, and
/// only the `comp_group_idx == 0`, `compound_idx == 1` blend (spec
/// `COMPOUND_AVERAGE`) -- masked compound and the distance-weighted blend are
/// not searched.
///
/// # Errors
/// As [`write_mv`], or a pair/mode this writer does not form.
#[allow(clippy::too_many_arguments)]
fn write_compound_block(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    neighbours: &Neighbours,
    grid: &crate::mvstack::MiGrid,
    (mi_row, mi_col): (usize, usize),
    (bw4, bh4): (usize, usize),
    (has_above, has_left): (bool, bool),
    info: InterInfo,
    (mi_cols, mi_rows): (usize, usize),
) -> Result<((i32, i32), (i32, i32), bool)> {
    let (Some(ref1), Some(mode)) = (info.ref1, info.mode.compound_index()) else {
        return Err(Error::unsupported(
            "AV1 tile",
            "a compound block needs a second reference and a compound mode",
        ));
    };
    let above = has_above.then(|| neighbours.above_nbr(mi_col));
    let left = has_left.then(|| neighbours.left_nbr(mi_row));
    write_compound_ref_frames(
        enc,
        cdfs,
        above,
        left,
        neighbours.above_ref[mi_col],
        neighbours.left_ref[mi_row],
        (info.ref_frame, ref1),
    )?;
    let stack = crate::mvstack::find_mv_stack_compound(
        grid,
        mi_row,
        mi_col,
        bw4,
        bh4,
        (info.ref_frame, ref1),
        mi_cols,
        mi_rows,
        grid.sign_bias_table(),
        &[(0, 0); 7],
        None,
    );
    let ctx = crate::cdf::COMPOUND_MODE_CTX_MAP[stack.ref_mv_ctx >> 1][stack.new_mv_ctx.min(4)];
    if crate::envflags::env_flag!("EC_TRACE_MODE") {
        eprintln!(
            "EC_WCOMP mi_row={mi_row} mi_col={mi_col} mode={mode} ref0={} ref1={ref1} ctx={ctx} new_mv_ctx={} ref_mv_ctx={} stack={}",
            info.ref_frame,
            stack.new_mv_ctx,
            stack.ref_mv_ctx,
            stack.entries.len()
        );
    }
    enc.symbol(mode, &mut cdfs.inter_compound_mode[ctx]);
    // `assign_compound_mv`'s own DRL half: `NEW_NEWMV` walks from index 0,
    // the two derived modes code no index at all.
    let mvs = match info.mode {
        InterMode::NearestNearestMv => stack.nearest_mv,
        InterMode::GlobalGlobalMv => ((0, 0), (0, 0)),
        InterMode::NearNearMv => {
            // decode.rs `assign_compound_mv`'s second DRL loop: the index
            // starts at 1 and the mode's own `ref_mv_idx` is `idx - 1`, so
            // this walk starts at 1 too and reads entry `ref_mv_idx + 1`.
            let idx = usize::from(info.ref_mv_idx) + 1;
            let mut walk = 1usize;
            while walk < 3 && stack.entries.len() > walk + 1 {
                let advance = walk < idx;
                enc.symbol(
                    usize::from(advance),
                    &mut cdfs.drl_mode[stack.drl_ctx[walk]],
                );
                if !advance {
                    break;
                }
                walk += 1;
            }
            let near = |i: usize| {
                stack
                    .entries
                    .get(i)
                    .map_or(stack.near_mv, |e| (e.mv0, e.mv1))
            };
            if walk != idx && near(walk) != near(idx) {
                note_drl_clamp(idx, walk, stack.entries.len());
            }
            near(walk)
        }
        InterMode::NearestNewMv | InterMode::NewNearestMv => {
            // Neither mode codes a DRL index (the decoder leaves `ref_mv_idx`
            // at 0): the NEAREST half is derived, the NEW half codes a
            // residual against stack entry 0.
            let base = stack
                .entries
                .first()
                .map_or(stack.nearest_mv, |e| (e.mv0, e.mv1));
            if matches!(info.mode, InterMode::NearestNewMv) {
                write_mv(enc, &mut cdfs.mv_comp, &mut cdfs.mv_joint, info.mv1, base.1)?;
                (stack.nearest_mv.0, info.mv1)
            } else {
                write_mv(enc, &mut cdfs.mv_comp, &mut cdfs.mv_joint, info.mv, base.0)?;
                (info.mv, stack.nearest_mv.1)
            }
        }
        _ => {
            let idx = usize::from(info.ref_mv_idx);
            let mut walk = 0usize;
            while walk < 2 && stack.entries.len() > walk + 1 {
                let advance = walk < idx;
                enc.symbol(
                    usize::from(advance),
                    &mut cdfs.drl_mode[stack.drl_ctx[walk]],
                );
                if !advance {
                    break;
                }
                walk += 1;
            }
            let nearest = |i: usize| {
                stack
                    .entries
                    .get(i)
                    .map_or(stack.nearest_mv, |e| (e.mv0, e.mv1))
            };
            if walk != idx && nearest(walk) != nearest(idx) {
                note_drl_clamp(idx, walk, stack.entries.len());
            }
            let base = nearest(walk);
            write_mv(enc, &mut cdfs.mv_comp, &mut cdfs.mv_joint, info.mv, base.0)?;
            write_mv(enc, &mut cdfs.mv_comp, &mut cdfs.mv_joint, info.mv1, base.1)?;
            (info.mv, info.mv1)
        }
    };
    // spec 5.11.25's `comp_group_idx`/`compound_idx` are NOT coded: this
    // crate's sequence header carries `enable_masked_compound = false` and
    // `enable_jnt_comp = false` (encode.rs), and decode.rs reads neither
    // symbol under those bits -- it infers `comp_group_idx = 0`,
    // `compound_idx = 1` (the simple average), which is the only blend this
    // writer forms anyway. A lane that turns either sequence bit on has to
    // write the matching symbol here, at
    // `get_comp_group_idx_context`/`get_comp_index_context`'s contexts (which
    // is what `Neighbours::above_comp_group_idx`/`above_compound_idx` and the
    // armed order hints below are already kept for).
    let _ = ORDER_HINTS.with(std::cell::Cell::get);
    COMPOUND_MODE_HITS[mode].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    COMPOUND_PAIR_HITS[(ref1.max(1) - 1) as usize % 7]
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    COMPOUND_SIZE_HITS[(bw4.max(1).trailing_zeros() as usize).min(3)]
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if bw4 <= 4 {
        COMPOUND_LEAF_MODE_HITS[mode].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    if crate::envflags::env_flag!("EC_COMP_MISMATCH") && (mvs.0 != info.mv || mvs.1 != info.mv1) {
        eprintln!(
            "EC_COMP_MISMATCH mode={mode} enc=({:?},{:?}) wrote=({:?},{:?})",
            info.mv, info.mv1, mvs.0, mvs.1
        );
    }
    // decode.rs' own `is_new_mv` for a compound block: modes 2, 3, 4, 5, 7.
    Ok((
        mvs.0,
        mvs.1,
        matches!(
            info.mode,
            InterMode::NewNewMv | InterMode::NearestNewMv | InterMode::NewNearestMv
        ),
    ))
}

/// Writer-side counterpart of decode.rs `read_comp_mode` (spec 5.11.25's
/// `comp_mode`): on a `reference_select` frame every inter block whose size
/// allows compound (`is_comp_ref_allowed`, always true here -- this writer's
/// smallest leaf is 8x8) names SINGLE or COMPOUND, at
/// `mvstack::reference_mode_ctx`'s own context off the same two neighbour
/// bands decode.rs builds its `NeighbourRef` pair from.
fn write_comp_mode(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    neighbours: &Neighbours,
    (mi_r, mi_c): (usize, usize),
    (has_above, has_left): (bool, bool),
    compound: bool,
) {
    if !REFERENCE_SELECT.with(std::cell::Cell::get) {
        return;
    }
    let above = has_above.then(|| neighbours.above_nbr(mi_c));
    let left = has_left.then(|| neighbours.left_nbr(mi_r));
    let ctx = crate::mvstack::reference_mode_ctx(above, left);
    if crate::envflags::env_flag!("EC_TRACE_MODE") {
        eprintln!(
            "EC_WCM mi=({mi_r},{mi_c}) ctx={ctx} val={} a={:?} l={:?}",
            usize::from(compound),
            above.map(|n| (n.is_inter, n.ref0, n.ref1)),
            left.map(|n| (n.is_inter, n.ref0, n.ref1))
        );
    }
    enc.symbol(usize::from(compound), &mut cdfs.comp_mode[ctx]);
}

/// Clears the armed plan when the tile writer that consumed it returns.
struct CdefIdxGuard;

impl Drop for CdefIdxGuard {
    fn drop(&mut self) {
        CDEF_IDX.with(|c| *c.borrow_mut() = None);
        DELTA_Q.with(|c| *c.borrow_mut() = None);
        LR_PLAN.with(|c| *c.borrow_mut() = None);
        SIGN_BIAS.with(|c| c.set(crate::mvstack::NO_SIGN_BIAS));
        SCREEN.with(|c| c.set(false));
        REFERENCE_SELECT.with(|c| c.set(false));
        MOTION_MODE.with(|c| c.set(false));
        INTRABC.with(|c| c.set(false));
        FILTER_INTRA.with(|c| c.set(false));
        INTRABC_GRID.with(|c| *c.borrow_mut() = None);
        ORDER_HINTS.with(|c| c.set((7, 0, [0; 7])));
    }
}

/// `read_cdef`'s write side, called right after every block's `skip` symbol:
/// writes this superblock's `cdef_idx` literal the first time a non-skip
/// block of it is coded. `mi` is the block's own position in 4x4 mode-info
/// units.
fn write_cdef_idx(enc: &mut SymbolEncoder, mi: (usize, usize), skip: bool) {
    if skip {
        return;
    }
    CDEF_IDX.with(|c| {
        let mut plan = c.borrow_mut();
        let Some(plan) = plan.as_mut() else { return };
        let sb = (mi.0 / 16) * plan.sb_cols + (mi.1 / 16);
        if plan.last == Some(sb) {
            return;
        }
        plan.last = Some(sb);
        enc.literal(
            u32::from(plan.grid.get(sb).copied().unwrap_or(0)),
            u32::from(plan.bits),
        );
    });
}

/// `read_delta_qindex`'s write side (spec 5.11.10), called right after every
/// block's [`write_cdef_idx`] -- the spec's own `skip -> cdef -> delta_q`
/// order, and decode.rs `maybe_read_delta_q` is the reader this mirrors
/// symbol for symbol. A no-op unless this block sits at its superblock's own
/// top-left mode-info position, and (the one case the reader skips too) when
/// that block IS the whole superblock and is skipped, in which case
/// `CurrentQIndex` carries over unchanged on both sides.
fn write_delta_q(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    mi: (usize, usize),
    is_whole_sb: bool,
    skip: bool,
) {
    DELTA_Q.with(|c| {
        let mut plan = c.borrow_mut();
        let Some(plan) = plan.as_mut() else { return };
        // lane-b128m: the delta_q unit is the SUPERBLOCK, so it is 128 wide
        // under `EC_AV1_SB128` -- decode.rs `maybe_read_delta_q` gates on
        // `sb_mi_cur`, and a writer gating on a hardcoded 64 codes three
        // symbol groups per 128 root the reader never reads.
        let sb_mi = if sb128_armed() {
            SB_MI as usize * 2
        } else {
            SB_MI as usize
        };
        if !mi.0.is_multiple_of(sb_mi) || !mi.1.is_multiple_of(sb_mi) {
            return;
        }
        if is_whole_sb && skip {
            return;
        }
        let sb = (mi.0 / SB_MI as usize) * plan.sb_cols + (mi.1 / SB_MI as usize);
        let target = i32::from(plan.grid.get(sb).copied().unwrap_or(0));
        // `reduced` is the spec's `delta_q_abs`/`delta_q_sign_bit` pair: the
        // step count, not the qindex difference.
        let reduced = (target - plan.cur) / plan.res;
        let abs = reduced.unsigned_abs() as usize;
        /// spec `DELTA_Q_SMALL`: the largest `delta_q_abs` symbol that is not
        /// an escape into the literal pair below.
        const DELTA_Q_SMALL: usize = 3;
        enc.symbol(abs.min(DELTA_Q_SMALL), &mut cdfs.delta_q);
        if abs >= DELTA_Q_SMALL {
            // libaom `write_delta_qindex`: `rem_bits = get_msb(abs - 1)`,
            // written one less in three bits, and the value offset by the
            // threshold `(1 << rem_bits) + 1` the reader adds back.
            let rem_bits = 31 - ((abs as u32) - 1).leading_zeros();
            let thr = (1u32 << rem_bits) + 1;
            enc.literal(rem_bits - 1, 3);
            enc.literal(abs as u32 - thr, rem_bits);
        }
        if abs != 0 {
            enc.literal(u32::from(reduced < 0), 1);
        }
        plan.cur = (plan.cur + reduced * plan.res).clamp(1, 255);
    });
}

/// round-4 av1-truesize debugging aid: prints `msg()` to stderr when the
/// `EC_RNG` environment variable is set, mirroring the `EC_PART`/`EC_TOK`
/// trace `/tmp/libaom-src`'s debug `aomdec` build already emits under the
/// same variable, so the two traces line up symbol for symbol. Checked once
/// per process, so unset (the default) costs one atomic load per call and no
/// allocation. Only the real tile writer below calls this -- the mode/rate
/// search's own trial encoders never do -- so the trace does not need the
/// throwaway-encoder gating a whole-encoder trace would.
fn ec_rng_trace(msg: impl FnOnce() -> String) {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2); // 2 = unknown, 1 = on, 0 = off
    let on = match ON.load(Ordering::Relaxed) {
        2 => {
            let on = u8::from(std::env::var_os("EC_RNG").is_some());
            ON.store(on, Ordering::Relaxed);
            on
        }
        v => v,
    };
    if on == 1 {
        eprintln!("{}", msg());
    }
}

/// `PARTITION_NONE` (spec 6.10.4): the whole block, undivided.
const PARTITION_NONE: usize = 0;
/// `PARTITION_HORZ` (spec 6.10.4): the block cut into a top and a bottom half.
const PARTITION_HORZ: usize = 1;
/// `PARTITION_VERT` (spec 6.10.4): the block cut into a left and a right half.
const PARTITION_VERT: usize = 2;
/// `PARTITION_SPLIT` (spec 6.10.4): the block cut into four quadrants.
const PARTITION_SPLIT: usize = 3;

/// The partition types whose probability mass the split-or-horizontal flag of
/// a superblock hanging off the bottom of the frame gathers (spec 9.3,
/// `partition_gather_vert_alike`): the flag says split, so everything vertical
/// or split lands on it.
const VERT_ALIKE: [usize; 6] = [2, PARTITION_SPLIT, 4, 6, 7, 9];

/// The same for a superblock hanging off the right-hand edge
/// (`partition_gather_horz_alike`).
const HORZ_ALIKE: [usize; 6] = [1, PARTITION_SPLIT, 4, 5, 6, 8];

/// Mode-info units across a 32x32 block.
pub(crate) const BLOCK_MI: u32 = SB_MI / 2;

/// Mode-info units across a 16x16 block, the smallest this crate's key-frame
/// writer codes.
pub(crate) const SUB_MI: u32 = BLOCK_MI / 2;

/// spec `decode_partition`'s `hasRows`/`hasCols` (5.11.4), recomputed at
/// whichever block size is asking: a block of `side_mi` mode-info units at mi
/// position `pos` may be coded unsplit (its own `PARTITION_NONE`) only when
/// this is true in both directions -- the frame's *true* size can put the
/// boundary inside a superblock, a 32x32 quadrant or (since this writer's
/// smallest block is 16x16) even a leaf, and each level must ask again with
/// its own half, not just once at the superblock.
pub(crate) fn has_half(pos: u32, side_mi: u32, bound: u32) -> bool {
    pos + side_mi / 2 < bound
}

/// The number of 32x32 block columns/rows a frame whose *true* (unpadded)
/// size gives `mi_cols`/`mi_rows` is coded over: every block whose mi origin
/// (`col * BLOCK_MI`) is inside the true bound is coded, so this is a
/// ceiling, not the exact division a block-aligned frame would give. Shared
/// by the tile writer's own iteration and by [`crate::encode`]'s block/
/// superblock generation, which must agree with it exactly.
pub(crate) fn block_grid(mi_cols: u32, mi_rows: u32) -> (u32, u32) {
    (mi_cols.div_ceil(BLOCK_MI), mi_rows.div_ceil(BLOCK_MI))
}
/// `DC_PRED` (spec 6.10.2), as both the luma and the chroma mode.
const DC_PRED: usize = 0;

/// `V_PRED` (spec 6.10.2), the first of the eight directional intra modes and
/// so the first mode that carries an angle delta.
const V_PRED: usize = 1;

/// `H_PRED`, which predicts every row from the column to the block's left.
#[cfg(test)]
const H_PRED: usize = 2;

/// The last directional mode, `D67_PRED`.
const D67_PRED: usize = 8;

/// The number of intra modes a key frame's luma block chooses from,
/// `INTRA_MODES` (spec 3).
const INTRA_MODES: usize = 13;

/// `Intra_Mode_Context` (spec 9.3): the five-way class each intra mode puts
/// its neighbours in when they pick the CDF for the next block's mode.
pub(crate) const INTRA_MODE_CTX: [usize; INTRA_MODES] = [0, 1, 2, 3, 4, 4, 4, 4, 3, 0, 1, 2, 0];

/// The symbol an angle delta of zero codes as: the alphabet runs from -3 to
/// +3, so `MAX_ANGLE_DELTA` is the middle of it.
pub(crate) const ANGLE_DELTA_ZERO: usize = 3;
/// Side of a superblock in 4x4 mode-info units when 128x128 superblocks are off.
pub(crate) const SB_MI: u32 = 16;

/// `NUM_BASE_LEVELS` (spec 3): levels above this carry a base-range tail.
const NUM_BASE_LEVELS: i32 = 2;
/// Where DCT_DCT sits in the spec's `Tx_Type_Intra_Inv_Set2`, the set a 16x16
/// intra luma transform picks its type from.
const TX_TYPE_DCT_DCT_SET2: usize = 1;
/// `COEFF_BASE_RANGE` (spec 3): how far the base-range tail reaches before the
/// Golomb tail takes over.
const COEFF_BASE_RANGE: i32 = 12;
/// `BR_CDF_SIZE - 1` (spec 3): the largest increment one base-range symbol
/// carries.
const BR_STEP: i32 = 3;
/// The largest level the base and base-range syntax carry between them.
/// Anything above it is written as a Golomb tail on top of this.
const MAX_BR_LEVEL: i32 = NUM_BASE_LEVELS + COEFF_BASE_RANGE;
/// The largest level this writer codes. The Golomb tail is a length in unary
/// followed by that many bits, and the spec's decoder reads at most twenty of
/// each, so a level a decoder cannot read back is refused rather than written.
const MAX_LEVEL: i32 = MAX_BR_LEVEL + (1 << 19);

/// The coefficient q-context (spec 8.3.2's `Get_Qctx`, `Default_..._Cdf`'s
/// leading index) a frame's `base_q_idx` picks its default CDFs from.
/// [`crate::cdf`] carries all four, one constant set per context.
pub(crate) fn q_ctx_of(base_q_idx: u8) -> usize {
    match base_q_idx {
        0..=20 => 0,
        21..=60 => 1,
        61..=120 => 2,
        _ => 3,
    }
}

/// Both writers here code whole superblocks only: a partial one forces the
/// partition syntax down the block tree, which they do not code yet.
/// The probability the CDF gives one symbol on its own.
fn element_prob(cdf: &[u16], element: usize) -> u16 {
    cdf[element] - if element > 0 { cdf[element - 1] } else { 0 }
}

/// The two-symbol CDF a partial superblock's partition flag is coded with: the
/// mass of the listed partition types becomes the probability of a split, and
/// the rest is the one partition the frame edge still allows.
fn gather(cdf: &[u16], elements: [usize; 6]) -> [u16; 3] {
    let split: u16 = elements.iter().map(|&e| element_prob(cdf, e)).sum();
    [32768 - split, 32768, 0]
}

fn check_superblocks(mi_cols: u32, mi_rows: u32) -> Result<()> {
    if mi_cols == 0
        || mi_rows == 0
        || !mi_cols.is_multiple_of(SB_MI)
        || !mi_rows.is_multiple_of(SB_MI)
    {
        return Err(Error::unsupported(
            "AV1 tile",
            "a key frame is written only for frames that are a whole number \
             of 64x64 superblocks",
        ));
    }
    Ok(())
}

/// Writes the payload of a one-tile key frame in which every superblock is a
/// skipped 64x64 DC-predicted block.
///
/// `mi_cols` and `mi_rows` are the frame's dimensions in 4x4 mode-info units,
/// as the frame header carries them.
///
/// # Errors
/// Returns an error when the frame is not a whole number of 64x64 superblocks:
/// a partial superblock forces the partition syntax down the block tree, which
/// this writer does not code yet.
pub fn flat_key_frame_tile(mi_cols: u32, mi_rows: u32) -> Result<Vec<u8>> {
    check_superblocks(mi_cols, mi_rows)?;
    let (sb_cols, sb_rows) = (mi_cols / SB_MI, mi_rows / SB_MI);

    let mut enc = SymbolEncoder::new();
    for r in 0..sb_rows {
        for c in 0..sb_cols {
            // decode_partition (spec 5.11.4). Every neighbour is a 64x64 block,
            // whose stored partition context has a zero bit at this block
            // size, so the context is 0 wherever the block sits.
            enc.symbol_fixed(PARTITION_NONE, &cdf::PARTITION_W64[0]);

            // intra_frame_mode_info (spec 5.11.16). Segmentation, delta q,
            // delta lf, palette, filter intra and intrabc are all off in the
            // frame header, and a skipped block codes no CDEF index, so the
            // block is three symbols: the skip flag and the two modes.
            let skip_ctx = usize::from(r > 0) + usize::from(c > 0);
            enc.symbol_fixed(1, &cdf::SKIP[skip_ctx]);
            // Both neighbours are DC-predicted, and an unavailable neighbour
            // counts as DC too, so both mode contexts are 0.
            enc.symbol_fixed(DC_PRED, &cdf::KF_Y_MODE[0][0]);
            // Chroma from luma is only offered up to 32x32, so the CFL-free
            // table is the one a 64x64 block reads.
            enc.symbol_fixed(DC_PRED, &cdf::UV_MODE_NO_CFL[DC_PRED]);

            // read_block_tx_size codes nothing while the frame's tx_mode is
            // TX_MODE_LARGEST, and a skipped block has no residual, so the
            // block ends here.
        }
    }
    Ok(enc.finish())
}

/// Writes the payload of a one-tile key frame in which every superblock is a
/// 64x64 DC-predicted block carrying one luma DC coefficient of `dc_level` and
/// no chroma residual.
///
/// `dc_level` is a quantised level, not a sample value: the decoder multiplies
/// it by the frame's DC quantiser and inverse-transforms it over the whole
/// block, so the picture it makes is a flat grey some distance either side of
/// the mid-grey a zero level gives. `base_q_idx` is the frame header's, and
/// picks the coefficient CDFs.
///
/// # Errors
/// As [`dc_key_frame_tile_levels`], which this is the one-level case of.
pub fn dc_key_frame_tile(
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    dc_level: i32,
) -> Result<Vec<u8>> {
    check_superblocks(mi_cols, mi_rows)?;
    let blocks = ((mi_cols / SB_MI) * (mi_rows / SB_MI)) as usize;
    dc_key_frame_tile_levels(mi_cols, mi_rows, base_q_idx, &vec![dc_level; blocks])
}

/// Writes the payload of a one-tile key frame carrying one luma DC coefficient
/// per superblock, `levels` giving them in the raster order the superblocks are
/// coded in.
///
/// Each superblock decodes to a flat block of its own grey, so a frame written
/// here is a grid of greys the caller chooses — which is what makes the sign
/// context observable: a block's sign context is read off its coded neighbours,
/// and a frame whose levels differ in sign exercises the three-way split that a
/// single-sign frame cannot reach.
///
/// # Errors
/// Returns an error when the frame is not a whole number of 64x64 superblocks,
/// when `levels` does not carry exactly one level per superblock, when a level
/// is outside the range the base and base-range syntax carry (`-14..=14`
/// without zero).
pub fn dc_key_frame_tile_levels(
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    levels: &[i32],
) -> Result<Vec<u8>> {
    check_superblocks(mi_cols, mi_rows)?;
    let (sb_cols, sb_rows) = (mi_cols / SB_MI, mi_rows / SB_MI);
    check_levels(levels, (sb_cols * sb_rows) as usize)?;
    let q_ctx = q_ctx_of(base_q_idx);
    let txb_skip_luma_64 = crate::cdf_state::pick(
        q_ctx,
        cdf::TXB_SKIP_LUMA_64_Q0,
        cdf::TXB_SKIP_LUMA_64_Q1,
        cdf::TXB_SKIP_LUMA_64,
        cdf::TXB_SKIP_LUMA_64_Q3,
    );
    let base_eob_luma_64_dc = crate::cdf_state::pick(
        q_ctx,
        cdf::COEFF_BASE_EOB_LUMA_64_Q0[0],
        cdf::COEFF_BASE_EOB_LUMA_64_Q1[0],
        cdf::COEFF_BASE_EOB_LUMA_64[0],
        cdf::COEFF_BASE_EOB_LUMA_64_Q3[0],
    );
    let txb_skip_chroma_32_none = crate::cdf_state::pick(
        q_ctx,
        cdf::TXB_SKIP_CHROMA_32_Q0[0],
        cdf::TXB_SKIP_CHROMA_32_Q1[0],
        cdf::TXB_SKIP_CHROMA_32[0],
        cdf::TXB_SKIP_CHROMA_32_Q3[0],
    );

    // The sign of the DC each coded block left behind, for the two neighbours
    // the sign context is read from: one row of them above, and the block to
    // the left, which is dropped at the start of every superblock row the way a
    // decoder clears its left context there.
    let mut above: Vec<Option<bool>> = vec![None; sb_cols as usize];
    let mut enc = SymbolEncoder::new();
    for r in 0..sb_rows {
        let mut left: Option<bool> = None;
        for c in 0..sb_cols {
            let dc_level = levels[(r * sb_cols + c) as usize];
            let negative = dc_level < 0;

            enc.symbol_fixed(PARTITION_NONE, &cdf::PARTITION_W64[0]);

            // Nothing is skipped now, so every neighbour's skip flag is 0 and
            // the skip context stays 0 across the frame.
            enc.symbol_fixed(0, &cdf::SKIP[0]);
            enc.symbol_fixed(DC_PRED, &cdf::KF_Y_MODE[0][0]);
            enc.symbol_fixed(DC_PRED, &cdf::UV_MODE_NO_CFL[DC_PRED]);

            write_dc_coeffs(
                &mut enc,
                dc_level,
                dc_sign_ctx(dc_vote(above[c as usize]) + dc_vote(left)),
                q_ctx,
                &txb_skip_luma_64,
                &base_eob_luma_64_dc,
            );

            // Both chroma transform blocks are all-zero. Their planes carry no
            // coded coefficient anywhere in the frame, so the neighbour halves
            // of their context stay 0 and only the offset for a transform block
            // that covers its whole plane block is left: context 7.
            enc.symbol_fixed(1, &txb_skip_chroma_32_none);
            enc.symbol_fixed(1, &txb_skip_chroma_32_none);

            above[c as usize] = Some(negative);
            left = Some(negative);
        }
    }
    Ok(enc.finish())
}

/// Writes the payload of a one-tile key frame in which every superblock is
/// split into four 32x32 DC-predicted blocks, each carrying one luma DC
/// coefficient and no chroma residual.
///
/// `levels` gives one level per 32x32 block in raster order across the frame,
/// so the grid is twice as wide and twice as tall as the superblock grid. The
/// blocks are coded in the z-order a superblock's split walks, but every
/// block's neighbours above and to its left are coded before it either way, so
/// the picture reads in raster order.
///
/// # Errors
/// As [`dc_key_frame_tile_levels`], with `levels` sized for the 32x32 grid.
pub fn split_dc_key_frame_tile(
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    levels: &[i32],
) -> Result<Vec<u8>> {
    check_superblocks(mi_cols, mi_rows)?;
    let (sb_cols, sb_rows) = (mi_cols / SB_MI, mi_rows / SB_MI);
    let (cols, rows) = (sb_cols * 2, sb_rows * 2);
    check_levels(levels, (cols * rows) as usize)?;
    let q_ctx = q_ctx_of(base_q_idx);
    let txb_skip_luma_32 = crate::cdf_state::pick(
        q_ctx,
        cdf::TXB_SKIP_LUMA_32_Q0,
        cdf::TXB_SKIP_LUMA_32_Q1,
        cdf::TXB_SKIP_LUMA_32,
        cdf::TXB_SKIP_LUMA_32_Q3,
    );
    let base_eob_luma_32 = crate::cdf_state::pick(
        q_ctx,
        cdf::COEFF_BASE_EOB_LUMA_32_Q0[0],
        cdf::COEFF_BASE_EOB_LUMA_32_Q1[0],
        cdf::COEFF_BASE_EOB_LUMA_32[0],
        cdf::COEFF_BASE_EOB_LUMA_32_Q3[0],
    );
    let txb_skip_chroma_16_0 = crate::cdf_state::pick(
        q_ctx,
        cdf::TXB_SKIP_CHROMA_16_Q0[0],
        cdf::TXB_SKIP_CHROMA_16_Q1[0],
        cdf::TXB_SKIP_CHROMA_16[0],
        cdf::TXB_SKIP_CHROMA_16_Q3[0],
    );

    let mut above: Vec<Option<bool>> = vec![None; cols as usize];
    let mut left: Vec<Option<bool>> = vec![None; rows as usize];

    let mut enc = SymbolEncoder::new();
    for sb_r in 0..sb_rows {
        // A decoder clears its left context at the start of every superblock
        // row, and so does the partition context the 64x64 symbol reads.
        left.iter_mut().for_each(|l| *l = None);
        for sb_c in 0..sb_cols {
            // The partition context of a 64x64 block reads the bit its
            // neighbours' block size sets at this depth: a 32x32 neighbour sets
            // it, and an uncoded one leaves it clear, so the context is just
            // which neighbours exist. The 32x32 blocks below read a bit their
            // own size leaves clear, so their context is 0 throughout.
            // Every superblock here is split, so an existing neighbour always
            // sets that bit.
            let ctx = 2 * usize::from(sb_c > 0) + usize::from(sb_r > 0);
            enc.symbol_fixed(PARTITION_SPLIT, &cdf::PARTITION_W64[ctx]);

            for quadrant in 0..4 {
                let (r, c) = (sb_r * 2 + quadrant / 2, sb_c * 2 + quadrant % 2);
                let dc_level = levels[(r * cols + c) as usize];
                let negative = dc_level < 0;

                enc.symbol_fixed(PARTITION_NONE, &cdf::PARTITION_W32[0]);
                enc.symbol_fixed(0, &cdf::SKIP[0]);
                enc.symbol_fixed(DC_PRED, &cdf::KF_Y_MODE[0][0]);
                // Chroma from luma is offered up to 32x32, so the block reads
                // the wider table even though it does not take the mode.
                enc.symbol_fixed(DC_PRED, &cdf::UV_MODE_CFL[DC_PRED]);

                write_dc_coeffs(
                    &mut enc,
                    dc_level,
                    dc_sign_ctx(dc_vote(above[c as usize]) + dc_vote(left[r as usize])),
                    q_ctx,
                    &txb_skip_luma_32,
                    &base_eob_luma_32,
                );
                enc.symbol_fixed(1, &txb_skip_chroma_16_0);
                enc.symbol_fixed(1, &txb_skip_chroma_16_0);

                above[c as usize] = Some(negative);
                left[r as usize] = Some(negative);
            }
        }
    }
    Ok(enc.finish())
}

mod syntax;
pub use self::syntax::*;


mod blocks;
pub use self::blocks::*;
mod leaf;
use self::leaf::*;

#[cfg(test)]
mod tests;

//! Inter block shape helpers: the block tx-size readers for rect and inter blocks, the rect residual scan tables and the edge-parameter body. Moved out of `decode/mod.rs` (av1split D13); the items are unchanged apart from visibility.

use super::*;

/// `read_block_tx_size` (libaom `decodeframe.c` `parse_decode_block`'s tx
/// branch): every block of a `TxMode::Select` inter frame codes its transform
/// size right after its own mode info -- an inter non-skip block through the
/// recursive var-tx tree ([`read_var_tx_size`], whose leaf list this returns),
/// every other block through the intra `tx_depth` symbol (a skipped inter
/// block codes no symbol at all) plus a `set_txfm_ctxs` write. Returns the
/// block's own resolved transform side and the leaf list, the latter only
/// when the tree resolved to more than one transform unit.
#[allow(clippy::too_many_arguments)]
pub(super) fn read_block_tx_size(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    n: &mut Neighbours,
    at_mi: (usize, usize),
    side: usize,
    (mi_cols, mi_rows): (usize, usize),
    is_inter: bool,
    skip: bool,
    fctx: &crate::decode::FrameCtx,
) -> Result<(usize, Option<Vec<(usize, usize, usize, usize)>>)> {
    // lane-lossless: TX_4X4 over the whole block, and NO var-tx tree -- libaom
    // `read_block_tx_size` (`decodeframe.c`) sends a lossless block down the
    // `read_tx_size` arm, which returns TX_4X4 before it reads anything.
    if lossless(fctx) {
        // lane-av1lm444loss: libaom CLIPS the transform-unit grid at the frame
        // edge -- `max_block_high`/`max_block_wide` (`av1_common_int.h:1565`,
        // `:1579`) fold `mb_to_bottom_edge`/`mb_to_right_edge` into
        // `block_size_high/wide` before `>> MI_SIZE_LOG2`, so a block that
        // hangs off the bottom/right codes only the units INSIDE the frame.
        // `decode_token_recon_block`'s mu walk then bounds every chunk with
        // `unit_height = ROUND_POWER_OF_TWO(AOMMIN(mu_blocks_high + row,
        // max_blocks_high), ss_y)`, so the out-of-frame units are never read.
        //
        // The unclipped `side / MI` grid is correct arithmetic for a block that
        // FITS, which is every block of a frame whose height and width are
        // multiples of the superblock -- so this reads identically on all of
        // those, and only differs on the partial edge block. Measured on the
        // 128x96 4:4:4 lossless pin (one 128 root per inter frame, frame 32
        // mi = 24 tx rows tall): the unclipped grid read 32 rows where the
        // oracle read 24 -- 256 extra luma units per inter frame (1024 vs
        // the oracle's 768), the `EC_SYMR` ladder's first fork landing exactly
        // at that row-24 boundary (`plane=0` luma where the oracle had moved
        // to `plane=1`), and the pixel diff growing 5596 -> 33561 wrong
        // samples over the six frames.
        let side_mi = side / MI;
        let max_w_mi = side_mi.min(mi_cols.saturating_sub(at_mi.1));
        let max_h_mi = side_mi.min(mi_rows.saturating_sub(at_mi.0));
        if max_w_mi != side_mi || max_h_mi != side_mi {
            LOSSLESS_EDGE_CLIPPED.with(|c| c.set(c.get() + 1));
            LOSSLESS_EDGE_CLIP_UNITS
                .with(|c| c.set(c.get() + (side_mi * side_mi - max_w_mi * max_h_mi)));
        }
        let mut leaves = Vec::with_capacity(max_w_mi * max_h_mi);
        for row in 0..max_h_mi {
            for col in 0..max_w_mi {
                leaves.push((row, col, 4, 4));
            }
        }
        // lane-av1txctxband2: a lossless block still runs libaom's
        // `set_txfm_ctxs` at the tail of `parse_decode_block`
        // (decodeframe.c:1260). Its `read_tx_size` returned `TX_4X4` on the
        // FIRST line (decodeframe.c:1214) -- the early return skips the symbol
        // read, not the PUBLISH -- so `tx_size_wide[TX_4X4] == 4` goes over the
        // block's own width/height, and a SKIPPED inter block instead records
        // its block size (the `skip && is_inter` term).
        //
        // This early return published nothing, so on a MIXED frame every
        // lossless square block left the band holding whatever an earlier,
        // lossy block wrote. Measured on the 640x480 mixed-lossless+altref
        // cell (`420_mixll_altref_640x480_5f.obu`, 87 537 B): the abutting
        // 8x8 INTRA lossless block at mi(78,110) left `above_txfm[110]` at
        // the 16 a skipped 16-px inter block at mi(36,108) had written 43 mi
        // rows earlier, and the 8x4 intra leaf with `use_filter_intra` at
        // mi(80,110) then read `tx_size_cat0[2]` where libaom reads row 1
        // (oracle `EC_TXCTXB` `abv=4 lft=8 above=0 left=1 ctx=1`). The tile
        // desynced there and the frame refused.
        //
        // The RECT twin [`read_block_tx_size_rect`] has published on this arm
        // since lane-av1lm444loss; only the square arm had the hole.
        let pub_above: u8 = if skip && is_inter {
            (side_mi * MI) as u8
        } else {
            4
        };
        // The gate's non-vacuity arm, on the DECISION, sampled BEFORE the
        // write: this publish CHANGES at least one of the above-band cells it
        // covers -- the cell did not already hold `pub_above`, so a later
        // `get_tx_size_context` / `txfm_partition_context` reader in the same
        // tile column reads a value the publish produced.
        //
        // CORRECTED 2026-09-30 (refutation pass `lanes/refute-av1txctxband2`):
        // an earlier version of this comment, and of the static's doc, claimed
        // the write counts only when it OVERWRITES "a size some earlier block
        // had left", and that a wholly-lossless all-non-skip stream leaves it
        // at zero. MEASURED FALSE: 420_lossless_tallinter_8x16.obu fires 48,
        // ALL with every covered column still at `TXFM_CTX_INIT` -- a first
        // write into a fresh tile column counts here too, because the test is
        // "the cell did not already hold this value", not "some block wrote a
        // different value earlier". Of the pin's 944 fires, 183 are that
        // first-touch case. The count is still a decision (no reader, no
        // intra-only frame, no unchanged column), so the gate keeps its teeth;
        // only the doc's stronger reading was wrong.
        let overwrote = !fctx.intra_only.with(std::cell::Cell::get)
            && (0..side_mi).any(|i| n.above_txfm.get(at_mi.1 + i) != Some(&pub_above));
        set_txfm_ctxs(n, at_mi, 4, side_mi, side_mi, skip && is_inter);
        if overwrote {
            hit!(LOSSLESS_SQ_TXFM_BAND_OVERWRITE_HITS);
        }
        return Ok((4, Some(leaves)));
    }
    if !fctx.tx_select_inter.with(std::cell::Cell::get) {
        // lane-sb128c r1: `max_txsize_rect_lookup[BLOCK_128X128]` is TX_64X64,
        // so a 128x128 inter block is FOUR luma units even with `tx_mode ==
        // TX_MODE_LARGEST` -- the same leaf shape the var-tx tree hands back.
        // lane-sb128c r3: intra too -- an INTRA 128x128 block under
        // `TX_MODE_LARGEST` also resolves to TX_64X64, i.e. four luma units.
        if side > 64 {
            let step = 64 / MI;
            let mut leaves = Vec::new();
            for row in (0..side / MI).step_by(step) {
                for col in (0..side / MI).step_by(step) {
                    leaves.push((row, col, 64, 64));
                }
            }
            return Ok((64, Some(leaves)));
        }
        return Ok((side, None));
    }
    let side_mi = side / MI;
    if is_inter && !skip {
        let max_tx = side.min(64);
        let max_w_mi = side_mi.min(mi_cols.saturating_sub(at_mi.1));
        let max_h_mi = side_mi.min(mi_rows.saturating_sub(at_mi.0));
        let mut leaves = Vec::new();
        let step = max_tx / MI;
        for row in (0..side_mi).step_by(step) {
            for col in (0..side_mi).step_by(step) {
                read_var_tx_size(
                    dec,
                    cdfs,
                    n,
                    at_mi,
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
        let single = leaves.len() == 1 && leaves[0].2 == side && leaves[0].3 == side;
        // lane-sb128c r6: below the 128 root the ceiling is main's TX_32X32
        // ([`inter_txbset_for`] has no 64-point *inter* table set); a 128x128
        // block's own var-tx tree legitimately bottoms out at TX_64X64 leaves,
        // which the mu-chunk reader above codes as their 32x32 coded corner.
        let leaf_ceiling = if side > 64 { 64 } else { 32 };
        if !single && leaves.iter().any(|leaf| leaf.2.max(leaf.3) > leaf_ceiling) {
            // Only a block wider than a superblock's own 64x64 max transform
            // reaches here; this crate has no 64-point *inter* coefficient
            // table set ([`inter_txbset_for`] stops at 32).
            return Err(unsupported(
                "an inter var-tx tree with a leaf transform larger than 32x32",
            ));
        }
        let resolved = leaves.last().map_or(side, |l| l.2);
        return Ok((resolved, (!single).then_some(leaves)));
    }
    let tx = if is_inter {
        side.min(64)
    } else {
        read_tx_size(dec, cdfs, n, at_mi, side, fctx)
    };
    set_txfm_ctxs(n, at_mi, tx, side_mi, side_mi, skip && is_inter);
    if !is_inter && tx != side {
        // lane-intrainter r1: an intra block inside an inter frame whose
        // `tx_depth` split its luma transform codes exactly the key-frame
        // split path's transform units (`decode_block`'s multi-TU loop),
        // raster order over the block. Handed back in the same leaf shape the
        // inter var-tx tree uses, so the caller's per-TU luma loop, the
        // `record_split_luma` neighbour write-back and the per-TU deblock
        // edges are the ones it already runs for a var-tx inter block. A
        // caller that has no per-TU intra loop (the 8x8 leaf) refuses on
        // seeing `Some` here.
        let step = tx / MI;
        let mut leaves = Vec::new();
        for row in (0..side_mi).step_by(step) {
            for col in (0..side_mi).step_by(step) {
                leaves.push((row, col, tx, tx));
            }
        }
        hit_do! {
            INTRA_IN_INTER_SPLIT_TX_HITS.with(|c| {
                let mut h = c.get();
                h[(side.trailing_zeros() as usize).saturating_sub(3).min(3)] += 1;
                c.set(h);
            });
        }
        return Ok((tx, Some(leaves)));
    }
    Ok((tx, None))
}

/// lane-inter4 r2: the shapes whose *rectangular inter* residual this
/// decoder codes -- a 2:1 strip whose own max rect transform
/// (`max_txsize_rect_lookup[BLOCK_32X16] == TX_32X16`) has both a
/// coefficient table set ([`TxbSet::LumaRect32x16Inter`], 512-coefficient
/// `eob_pt`) and a `tx_type` alphabet here. 64x32/64x16 strips would need a
/// 64-point rect transform, and 16x8/8x16 needs
/// `default_inter_ext_tx_cdf[2][TX_8X8]` (the 12-symbol
/// `DTT9_IDTX_1DDCT` row at `tx_size_sqr == TX_8X8`, only reachable through a
/// rect transform), neither of which exists yet -- both stay refused by name.
pub(crate) fn rect_inter_residual_supported(w: usize, h: usize) -> bool {
    matches!(
        (w, h),
        // 2:1 strips whose transform is coded in full (lane-inter4).
        (32, 16) | (16, 32) | (16, 8) | (8, 16)
            // lane-inter16ab r2: the 16x16-level 1:4 strips. TX_16X4/TX_4X16
            // codes all 64 of its positions, and its chroma pair is an 8x4 /
            // 4x8 unit (`ss_size_lookup[BLOCK_16X4]` = BLOCK_8X4).
            | (16, 4) | (4, 16)
            // lane-r14 r2: strips with a 64-px axis. `av1_get_max_eob` codes
            // only the low 32 coefficients of a 64-length axis, so these read
            // exactly the truncated corner the INTRA superblock-strip path
            // already reads (`read_intra_mode_rect`), through
            // `av1_get_adjusted_tx_size`'s CDF set. Their `tx_size_sqr_up` is
            // `TX_64X64`, so `av1_get_ext_tx_set_type` is `DCT_ONLY` and no
            // `tx_type` symbol is coded at all.
            | (64, 32) | (32, 64) | (64, 16) | (16, 64)
            // lane-inter16ab r4: `sub_tx_size_map[TX_16X4] == TX_8X4`, so a
            // split-transform 16x4/4x16 strip has RECTANGULAR 8x4/4x8 leaves.
            // Only reachable as a var-tx LEAF -- an 8x4/4x8 inter BLOCK is
            // refused earlier ("an inter partition below 8x8").
            | (8, 4) | (4, 8)
            // lane-sb128c r7: the 128 root's HORZ/VERT halves. These are NOT
            // one rectangular transform -- `max_txsize_rect_lookup
            // [BLOCK_128X64]` is TX_64X64, so the block tiles into two square
            // TX_64X64 units read through the mu-chunk loop, same as the
            // 128x128 NONE block's four.
            | (128, 64) | (64, 128)
            // lane-rectres r1: the 32x32-level 1:4 strips -- THE shape both
            // 10-bit film cuts and every measured 1080p offset stop on
            // (measured: 6/6 probes refuse at 32x8 or 8x32, nothing else).
            // `max_txsize_rect_lookup[BLOCK_32X8] == TX_32X8` codes all 256
            // positions, and its chroma pair is a 16x4 / 4x16 unit
            // (`ss_size_lookup[BLOCK_32X8]` = BLOCK_16X4).
            | (32, 8) | (8, 32)
    )
}

// lane-t900 r20: the `(side, write_w, write_h)` triples the inter block path
// actually PRESENTS, as a thread-local bitmask -- the measured domain behind
// the "rectangular ... has no ... here" refusals. `side` is the square context
// a block reads its CDFs at and `write_w`/`write_h` its true footprint;
// `inter_piece!` derives `reject_residual` from exactly that inequality, so
// these triples are what decides whether those refusals can fire at all. Bit
// 31 is "a triple this table does not list", which is what makes the gate a
// census rather than a restatement of its own table.
pub(crate) const INTER_BLOCK_SHAPES: [(usize, usize, usize); 18] = [
    (16, 16, 16),
    (32, 32, 32),
    (32, 16, 32),
    (32, 32, 16),
    (16, 8, 16),
    (16, 16, 8),
    (32, 8, 32),
    (64, 64, 64),
    (32, 32, 8),
    (64, 16, 64),
    (128, 128, 128),
    (64, 32, 64),
    (64, 64, 32),
    (16, 4, 16),
    (64, 64, 16),
    (16, 16, 4),
    (128, 128, 64),
    (128, 64, 128),
];

/// Bit 31 of [`inter_block_shape_mask`]: a `(side, write_w, write_h)` triple
/// outside [`INTER_BLOCK_SHAPES`].
pub const INTER_BLOCK_SHAPE_UNLISTED: u32 = 1 << 31;

thread_local! {
    pub(super) static INTER_BLOCK_SHAPE_MASK: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Which of [`INTER_BLOCK_SHAPES`] this THREAD has decoded (thread-local, so a
/// gate reading it sees only the streams it decoded itself).
pub fn inter_block_shape_mask() -> u32 {
    INTER_BLOCK_SHAPE_MASK.with(std::cell::Cell::get)
}

/// The luma coefficient/`tx_type` table set of a whole-block rectangular
/// INTER transform unit (lane-inter4 r3). `av1_get_ext_tx_set_type` splits
/// the two shapes: a 32x16/16x32 unit has `tx_size_sqr_up == TX_32X32`, so
/// it reads the 2-symbol `EXT_TX_SET_DCT_IDTX` set regardless of
/// `reduced_tx_set`; a 16x8/8x16 unit has `tx_size_sqr_up == TX_16X16` and
/// `tx_size_sqr == TX_8X8`, so it reads `EXT_TX_SET_DCT_IDTX` when reduced
/// and the 16-symbol `EXT_TX_SET_ALL16` otherwise -- both at the *8x8* CDF
/// row (`txsize_sqr_map`), never the 16x16 one.
pub(super) fn rect_inter_luma_set(
    w: usize,
    h: usize,
    fctx: &crate::decode::FrameCtx,
) -> Result<TxbSet> {
    Ok(match (w, h) {
        (32, 16) | (16, 32) => TxbSet::LumaRect32x16Inter,
        // lane-inter16ab r4: the 8x4/4x8 split leaf of a 16x4/4x16 strip.
        // `av1_get_ext_tx_set_type` (blockd.h:1097) at `tx_size_sqr_up ==
        // TX_8X8`, `tx_size_sqr == TX_4X4`, inter: DCT_IDTX when reduced,
        // ALL16 otherwise -- both at the TX_4X4 row.
        (8, 4) | (4, 8) => {
            if fctx.reduced_tx_set_inter.with(std::cell::Cell::get) {
                TxbSet::LumaRect8x4Inter
            } else {
                TxbSet::LumaRect8x4InterSet1
            }
        }
        (16, 4) | (4, 16) => {
            if fctx.reduced_tx_set_inter.with(std::cell::Cell::get) {
                TxbSet::LumaRect16x4Inter
            } else {
                TxbSet::LumaRect16x4Inter1
            }
        }
        (16, 8) | (8, 16) => {
            if fctx.reduced_tx_set_inter.with(std::cell::Cell::get) {
                TxbSet::LumaRect16x8Inter
            } else {
                TxbSet::LumaRect16x8InterSet1
            }
        }
        // lane-r14 r2: `av1_get_adjusted_tx_size` maps TX_64X32/TX_32X64 to
        // TX_32X32 and TX_64X16/TX_16X64 to TX_32X16/TX_16X32, and
        // `get_txsize_entropy_ctx`/`txsize_log2_minus4` agree with the
        // adjusted size, so the existing sets are bit-exact. Neither carries a
        // `tx_type` table (DCT_ONLY above), so the intra/inter split is moot.
        // lane-rectres r1: `tx_size_sqr_up[TX_32X8] == TX_32X32`, so
        // `av1_get_ext_tx_set_type` is `EXT_TX_SET_DCT_IDTX` for inter with or
        // without `reduced_tx_set` -- one set, no `REDUCED_TX_SET_INTER`
        // branch, read at the `txsize_sqr_map[TX_32X8] == TX_8X8` row.
        (32, 8) | (8, 32) => TxbSet::LumaRect32x8Inter,
        (64, 32) | (32, 64) => TxbSet::Luma64,
        (64, 16) | (16, 64) => TxbSet::LumaRect32x16,
        _ => {
            return Err(unsupported(
                "a rectangular inter luma transform unit whose shape has no coefficient table set here",
            ));
        }
    })
}

/// The chroma table set of the same block's single rectangular chroma unit
/// (`av1_get_max_uv_txsize`): 16x8/8x16 under a 32x16/16x32 strip, 8x4/4x8
/// under a 16x8/8x16 one. Neither codes a `tx_type` symbol -- both inherit
/// the luma unit's.
pub(super) fn rect_inter_chroma_set(w: usize, h: usize) -> Result<TxbSet> {
    Ok(match (w, h) {
        (16, 8) | (8, 16) => TxbSet::ChromaRect16x8,
        (8, 4) | (4, 8) => TxbSet::ChromaRect8x4,
        // lane-r14 r2: chroma of a 64-axis strip -- 32x16/16x32 under a
        // 64x32/32x64 one, 32x8/8x32 under a 64x16/16x64 one. Neither chroma
        // unit has a 64-px axis, so both are coded in full; `Chroma16` is the
        // exact set for TX_32X8/TX_8X32 (`get_txsize_entropy_ctx` = TX_16X16,
        // `txsize_log2_minus4` = 4), same as the intra strip path uses.
        (32, 16) | (16, 32) => TxbSet::ChromaRect32x16,
        (32, 8) | (8, 32) => TxbSet::Chroma16,
        // lane-rectres r1: chroma of a 32x8/8x32 strip. `Chroma8` is the exact
        // set for TX_16X4/TX_4X16 (`get_txsize_entropy_ctx` = TX_8X8,
        // `txsize_log2_minus4` = 2, 64-coefficient `eob_pt`).
        (16, 4) | (4, 16) => TxbSet::Chroma8,
        // lane-av1-444: one 32x32 unit of a 4:4:4 64x32/32x64 chroma plane
        // block (`av1_get_max_uv_txsize` adjusts `TX_64X32`/`TX_32X64` to
        // `TX_32X32`); `get_txsize_entropy_ctx(TX_32X32)` is the 32x32 set.
        (32, 32) => TxbSet::Chroma32,
        // lane-av1-422j: square chroma units of a 4:2:2 strip -- a 16x8
        // (8x16) luma strip's chroma plane block is 8x8 (16x16) square, and
        // the square-block routing above sends it here. The sets are the
        // exact `get_txsize_entropy_ctx` tables the square arms use.
        (16, 16) => TxbSet::Chroma16,
        (8, 8) => TxbSet::Chroma8,
        (4, 4) => TxbSet::Chroma4,
        _ => {
            // lane-av1-422l: the refusing (w, h) for the owning lane -- the
            // 4:2:2 frontier stops here at (4, 32), the plane residual of an
            // 8x32 luma block, which libaom itself marks BLOCK_INVALID
            // (`av1_ss_size_lookup[BLOCK_8X32][1][0]`, common_data.c:38) and
            // for which no TX_4X32 exists -- that chroma is coded as a
            // TX_8X32-chunked merge, not a 4-px unit.
            if crate::envflags::env_flag!("EC_AV1_TRACE_422") {
                eprintln!("EC_422 rect_inter_chroma_set missing (w,h)=({w},{h})");
            }
            return Err(unsupported(
                "a rectangular inter chroma transform unit whose shape has no coefficient table set here",
            ));
        }
    })
}

/// libaom `size_group_lookup[bsize]` for the block shapes this decoder's
/// inter path reaches, from the block's TRUE footprint rather than the
/// square `side` corner-cut: 8x8/8x16/16x8 -> 1, 16x16/16x32/32x16 -> 2,
/// everything 32 and above -> 3.
pub(super) fn size_group_wh(w: usize, h: usize) -> usize {
    // lane-inter4 r4: `size_group_lookup[BLOCK_SIZES_ALL]` (libaom
    // `common_data.h:60`) is 0 for every block with a 4-px side
    // (4x4/4x8/8x4/4x16/16x4), 1 for min side 8, 2 for 16, 3 above -- the old
    // `0..=8 => 1` row gave a sub-8 leaf group 1 where libaom says 0.
    match w.min(h) {
        0..=4 => 0,
        8 => 1,
        16 => 2,
        _ => 3,
    }
}

#[test]
fn size_group_wh_matches_libaom_size_group_lookup() {
    // `BLOCK_SIZES_ALL` in libaom's own order, with `size_group_lookup`'s
    // transcribed values (common_data.h:60-62).
    const TABLE: [((usize, usize), usize); 22] = [
        ((4, 4), 0),
        ((4, 8), 0),
        ((8, 4), 0),
        ((8, 8), 1),
        ((8, 16), 1),
        ((16, 8), 1),
        ((16, 16), 2),
        ((16, 32), 2),
        ((32, 16), 2),
        ((32, 32), 3),
        ((32, 64), 3),
        ((64, 32), 3),
        ((64, 64), 3),
        ((64, 128), 3),
        ((128, 64), 3),
        ((128, 128), 3),
        ((4, 16), 0),
        ((16, 4), 0),
        ((8, 32), 1),
        ((32, 8), 1),
        ((16, 64), 2),
        ((64, 16), 2),
    ];
    for ((w, h), want) in TABLE {
        assert_eq!(size_group_wh(w, h), want, "size_group_wh({w}, {h})");
    }
}

/// The default (2D) coefficient scan of a rectangular transform.
pub(super) fn rect_scan(w: usize, h: usize) -> Result<&'static [u16]> {
    Ok(match (w, h) {
        (32, 16) => &SCAN_32X16,
        (16, 32) => &SCAN_16X32,
        (16, 8) => &SCAN_16X8,
        (8, 16) => &SCAN_8X16,
        (16, 4) => &SCAN_16X4,
        (4, 16) => &SCAN_4X16,
        (8, 4) => &SCAN_8X4,
        (4, 8) => &SCAN_4X8,
        (32, 8) => &SCAN_32X8,
        (8, 32) => &SCAN_8X32,
        // lane-av1-422j: square units routed through the rect reader -- a
        // 4:2:2 strip's square chroma plane block (8x8 under a 16x8 strip,
        // 16x16 under an 8x16 one). The default 2D scan is exactly what the
        // square reader uses for the same side.
        (w, h) if w == h => default_scan(w),
        _ => {
            return Err(unsupported(
                "a rectangular transform unit whose shape has no coefficient scan table here",
            ));
        }
    })
}

/// lane-t900 r20, refusal D -- the three "a rectangular inter luma/chroma
/// transform unit whose shape has no coefficient table set here" / "a
/// rectangular transform unit whose shape has no coefficient scan table here"
/// arms, enumerated against the MEASURED domain of the inter block path
/// ([`INTER_BLOCK_SHAPES`], filled from three real streams by the gate
/// `a_block_shape_census_over_three_real_streams_leaves_the_rect_residual_refusal_unreachable`).
///
/// What each helper is actually handed:
///
/// * [`rect_inter_luma_set`] -- the block's own footprint (call sites at the
///   whole-block residual), and a var-tx LEAF shape. `sub_tx_size_map` is
///   square for every rect transform except TX_16X4/TX_4X16, whose leaves are
///   TX_8X4/TX_4X8, so the leaf domain adds exactly 8x4 and 4x8.
/// * [`rect_inter_chroma_set`] -- `(write_w / 2, write_h / 2)` for a 4:2:0
///   block, except a 16x4/4x16 strip, whose chroma is the PAIR's 8x4 / 4x8.
/// * [`rect_scan`] -- the `min(w, 32) x min(h, 32)` coded corner of a rect
///   unit, and only when that corner is not 32x32 (a square corner goes
///   through the square reader with `rect_shape = Some((w, h))`).
///
/// The 128-px halves take neither: `max_txsize_rect_lookup[BLOCK_128X64]` is
/// TX_64X64, so they tile into SQUARE 64x64 luma units through the mu-chunk
/// loop, which is why they are skipped below.
#[test]
fn every_rect_transform_shape_the_census_lists_has_a_coefficient_table_and_scan() {
    // lane-av1refusalspan: this gate is the PROOF for THREE refusals, so it
    // names all three and pins all three guard sites. The census below proves
    // the shape domain and, on its own, named NONE of the strings that domain
    // was about -- under a real function boundary rather than a window that
    // ran into the next test, all three rows read as unanchored.
    for refusal in [
        "a rectangular inter luma transform unit whose shape has no coefficient table set here",
        "a rectangular inter chroma transform unit whose shape has no coefficient table set here",
        "a rectangular transform unit whose shape has no coefficient scan table here",
    ] {
        assert!(
            crate::test_source::module_source("decode").contains(refusal),
            "this gate proves {refusal:?}, but that string is no longer in decode.rs"
        );
    }
    let fctx = &crate::decode::FrameCtx::new();
    let corner_scan = |w: usize, h: usize| {
        let (cw, ch) = (w.min(32), h.min(32));
        if (cw, ch) == (32, 32) {
            return; // the square coefficient reader, not `rect_scan`
        }
        assert!(
            rect_scan(cw, ch).is_ok(),
            "a {w}x{h} rect transform codes its {cw}x{ch} corner, which has no scan table"
        );
    };
    let mut checked = 0;
    for &(side, w, h) in &INTER_BLOCK_SHAPES {
        if (w, h) == (side, side) || w.max(h) > 64 {
            continue;
        }
        assert!(
            rect_inter_luma_set(w, h, fctx).is_ok(),
            "the inter path decodes {w}x{h} blocks but has no luma coefficient table set for one"
        );
        corner_scan(w, h);
        // 4:2:0 chroma: the pair's 8x4 / 4x8 for a 1:4 strip, the halved
        // footprint for every other shape.
        let (cw, ch) = match (w, h) {
            (16, 4) => (8, 4),
            (4, 16) => (4, 8),
            _ => (w / 2, h / 2),
        };
        assert!(
            rect_inter_chroma_set(cw, ch).is_ok(),
            "a {w}x{h} block's {cw}x{ch} chroma unit has no coefficient table set"
        );
        corner_scan(cw, ch);
        checked += 1;
    }
    assert_eq!(
        checked, 12,
        "the block-shape census changed: {checked} rect shapes reach the rect transform \
         tables, not the 12 this claim was measured over"
    );
    // `sub_tx_size_map[TX_16X4] == TX_8X4` -- the only RECTANGULAR var-tx leaf
    // shapes, which reach `rect_inter_luma_set` as `(tw, th)`.
    for (w, h) in [(8usize, 4usize), (4, 8)] {
        assert!(
            rect_inter_luma_set(w, h, fctx).is_ok(),
            "the {w}x{h} split leaf of a 16x4/4x16 strip has no luma coefficient table set"
        );
        corner_scan(w, h);
    }
}

/// [`read_block_tx_size`] for a rectangular inter block (lane-inter4 r2):
/// spec 5.11.17 / libaom `read_tx_size_vartx` starting from the block's own
/// `max_txsize_rect_lookup` entry, which for a 2:1 strip IS rectangular. The
/// depth-0 `txfm_partition` symbol reads its context off the rect transform's
/// own width/height; a `0` leaves one whole-block rect transform unit
/// (`None`), and a `1` splits it into `sub_tx_size_map`'s two SQUARE
/// sub-transforms (`sub_tx_size_map[TX_32X16] == TX_16X16`), from which the
/// ordinary square [`read_var_tx_size`] recursion continues at depth 1 -- so
/// every leaf of a split rect block is square and the existing square leaf
/// loop reconstructs it unchanged.
#[allow(clippy::too_many_arguments)]
pub(super) fn read_block_tx_size_rect(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    n: &mut Neighbours,
    at_mi: (usize, usize),
    (bw, bh): (usize, usize),
    (mi_cols, mi_rows): (usize, usize),
    skip: bool,
    fctx: &crate::decode::FrameCtx,
) -> Result<Option<Vec<(usize, usize, usize, usize)>>> {
    if lossless(fctx) {
        // lane-av1lm444loss: the same frame-edge clip as the square builder
        // above -- `max_block_high`/`max_block_wide` bound a rect block's
        // transform grid by the part of it inside the frame, so a 1:4 strip
        // hanging off the bottom codes only its in-frame rows. Identical
        // arithmetic for a block that fits.
        let max_w_mi = (bw / MI).min(mi_cols.saturating_sub(at_mi.1));
        let max_h_mi = (bh / MI).min(mi_rows.saturating_sub(at_mi.0));
        let full_w_mi = bw / MI;
        let full_h_mi = bh / MI;
        if max_w_mi != full_w_mi || max_h_mi != full_h_mi {
            LOSSLESS_EDGE_CLIPPED.with(|c| c.set(c.get() + 1));
            LOSSLESS_EDGE_CLIP_UNITS
                .with(|c| c.set(c.get() + (full_w_mi * full_h_mi - max_w_mi * max_h_mi)));
        }
        let mut leaves = Vec::with_capacity(max_w_mi * max_h_mi);
        for row in 0..max_h_mi {
            for col in 0..max_w_mi {
                leaves.push((row, col, 4, 4));
            }
        }
        // A lossless segment takes libaom's `else` branch (the var-tx
        // condition carries `!xd->lossless[mbmi->segment_id]`), so
        // `read_tx_size` returns `TX_4X4` and `set_txfm_ctxs` publishes it
        // over the block's own width/height. The grid's own values (4x4) are
        // identical either way, so this write is only what keeps the
        // caller's `fill_lf_grid_rect_after_vartx` suppression sound: a
        // mixed frame's lossy neighbour reads this block's bands.
        txfm_partition_update_rect(n, at_mi, (4, 4), (bw, bh));
        return Ok(Some(leaves));
    }
    if skip {
        // `set_txfm_ctxs(.., skip && is_inter)`: a skipped inter block records
        // its own BLOCK size -- width above, height left -- not a transform.
        txfm_partition_update_rect(n, at_mi, (bw, bh), (bw, bh));
        return Ok(None);
    }
    // `max_txsize_rect_lookup` is the block's own size, capped at 64 -- and
    // `rect_inter_residual_supported` has already excluded everything above 32.
    // lane-sb128c r7: except at the 128 root. `max_txsize_rect_lookup
    // [BLOCK_128X64] == TX_64X64` (libaom `common_data.h`), so a 128x64 block
    // is TWO square top-level transform units side by side and a 64x128 one is
    // two stacked -- exactly what libaom `read_block_tx_size`
    // (decodeframe.c:1140) codes with its `idy/idx` loop over
    // `tx_size_high_unit/tx_size_wide_unit[max_tx_size]`. Each unit reads its
    // own `txfm_partition` symbol at depth 0.
    if bw.max(bh) > 64 {
        let tx = 64usize;
        let (max_w_mi, max_h_mi) = (
            (bw / MI).min(mi_cols.saturating_sub(at_mi.1)),
            (bh / MI).min(mi_rows.saturating_sub(at_mi.0)),
        );
        let mut leaves = Vec::new();
        if !fctx.tx_select_frame.with(std::cell::Cell::get) {
            for row in (0..bh / MI).step_by(tx / MI) {
                for col in (0..bw / MI).step_by(tx / MI) {
                    leaves.push((row, col, tx, tx));
                }
            }
            for &(row, col, tw, th) in &leaves {
                txfm_partition_update_rect(n, (at_mi.0 + row, at_mi.1 + col), (tw, th), (tw, th));
            }
            return Ok(Some(leaves));
        }
        for row in (0..bh / MI).step_by(tx / MI) {
            for col in (0..bw / MI).step_by(tx / MI) {
                read_var_tx_size(
                    dec,
                    cdfs,
                    n,
                    at_mi,
                    bw.max(bh),
                    (max_w_mi, max_h_mi),
                    (tx, tx),
                    0,
                    row,
                    col,
                    &mut leaves,
                );
            }
        }
        if leaves.iter().any(|l| l.2.max(l.3) > 64) {
            return Err(unsupported(
                "an inter var-tx tree with a leaf transform larger than 64x64",
            ));
        }
        return Ok(Some(leaves));
    }
    let (tx_w, tx_h) = (bw, bh);
    if !fctx.tx_select_frame.with(std::cell::Cell::get) {
        // `TX_MODE_LARGEST`: no symbol at all, the whole block is one
        // transform, and `set_txfm_ctxs` records exactly what
        // `txfm_partition_update` would for that same unit.
        txfm_partition_update_rect(n, at_mi, (tx_w, tx_h), (bw, bh));
        hit!(RECT_INTER_TU_HITS);
        return Ok(None);
    }
    // lane-av1oddheightfork2: `above_px` / `left_px` are the two
    // `TXFM_CONTEXT` inputs of `txfm_partition_ctx_rect`, i.e. the exact
    // operands of libaom's `*above_ctx < txw` / `*left_ctx < txh`
    // (av1_common_int.h:1750-1753). They are printed so a ctx disagreement
    // with aomdec's `EC_VARTX` rung names WHICH neighbour is wrong instead of
    // only the arithmetic difference. Trace-only; reads no bits.
    let (above_px, left_px) = (n.above_txfm[at_mi.1], n.left_txfm[at_mi.0]);
    let ctx = txfm_partition_ctx_rect(above_px, left_px, bw.max(bh), tx_w, tx_h);
    let split = dec.symbol(&mut cdfs.txfm_partition[ctx]) == 1;
    hit!(TXFM_SPLIT_READS);
    if crate::envflags::env_flag!("EC_TRACE_MODE_STEP") {
        let (rng, _) = dec.debug_state();
        eprintln!(
            "EC_ISTEP mi_row={} mi_col={} name=txfm_split_rect val={} ctx={ctx} above={above_px} left={left_px} maxblk={} tx={tx_w}x{tx_h} rng={rng}",
            at_mi.0,
            at_mi.1,
            u8::from(split),
            bw.max(bh),
        );
    }
    if !split {
        txfm_partition_update_rect(n, at_mi, (tx_w, tx_h), (bw, bh));
        hit!(RECT_INTER_TU_HITS);
        return Ok(None);
    }
    hit!(TXFM_SPLIT_HITS);
    hit!(RECT_INTER_TXSPLIT_HITS);
    // lane-r14 r3: `sub_tx_size_map` (libaom `common_data.c`) stays
    // RECTANGULAR under a 1:4 strip -- `[TX_64X16] == TX_32X16`,
    // `[TX_16X64] == TX_16X32` -- so the recursion below is the general
    // rect one and its leaves carry their own width and height.
    let (sub_w, sub_h) = sub_tx_size_map(tx_w, tx_h);
    let max_w_mi = (bw / MI).min(mi_cols.saturating_sub(at_mi.1));
    let max_h_mi = (bh / MI).min(mi_rows.saturating_sub(at_mi.0));
    let mut leaves = Vec::new();
    for row in (0..bh / MI).step_by(sub_h / MI) {
        for col in (0..bw / MI).step_by(sub_w / MI) {
            read_var_tx_size(
                dec,
                cdfs,
                n,
                at_mi,
                bw.max(bh),
                (max_w_mi, max_h_mi),
                (sub_w, sub_h),
                1,
                row,
                col,
                &mut leaves,
            );
        }
    }
    Ok(Some(leaves))
}

/// The chroma half of a whole-block rectangular INTER transform unit (both
/// [`decode_inter_block`] rect readers): `av1_get_max_uv_txsize(bsize)` gives
/// the strip's own rect chroma size at 4:2:0 (one unit for the whole block),
/// but at 4:4:4 the chroma plane block keeps the luma block's 64-px axis,
/// which `av1_get_adjusted_tx_size` caps at 32 (`TX_64X16` -> `TX_32X16`),
/// so it codes TWO units plane-major (every U, then every V --
/// `decode_token_recon_block`'s inter branch loops `for plane` outside
/// `for blk_row/blk_col` inside the one mu chunk). No unit codes a `tx_type`
/// symbol: each inherits the block-wide luma one (`av1_get_tx_type`'s inter
/// UV lookup at ss 0 reads the tx_type_map at its own mi, which a single-TU
/// luma block fills uniformly).
#[allow(clippy::too_many_arguments)]
pub(super) fn read_inter_rect_chroma(
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    around: [(bool, bool, i32); 3],
    su: Pred<'_>,
    sv: Pred<'_>,
    at_mi: (usize, usize),
    (cpx, cpy): (usize, usize),
    (write_chroma_w, write_chroma_h): (usize, usize),
    chroma_side: usize,
    mode_for_tx: usize,
    // lane-av1chrtx: this block's coded luma transform leaves, `(row, col,
    // tw, th, tx_type)` -- the same list the 4:4:4 four-unit arms resolve
    // through `covering_leaf_tx_type`. This sibling used to hand EVERY unit
    // the block-level `luma_tx_type` (the top-left leaf's type), so a rect
    // inter block whose chroma plane splits (`nx*ny > 1`) over MIXED var-tx
    // leaves read the wrong type per unit.
    leaf_tx_types: &[(usize, usize, usize, usize, TxType)],
    luma_tx_type: TxType,
    fctx: &crate::decode::FrameCtx,
) -> Result<(Grid, Grid, Vec<((usize, usize), usize, Grid, usize, usize)>)> {
    let (uw, uh) = (write_chroma_w.min(32), write_chroma_h.min(32));
    // lane-av1-422m, CORRECTED lane-av1-422n: an 8x32 luma block HAS no
    // 4:2:2 plane residual block -- `av1_ss_size_lookup[BLOCK_8X32][1][0]`
    // is BLOCK_INVALID (= 255, blockd.h) -- so libaom never codes its chroma
    // as a (4, 32) unit, and no such coefficient set exists (no TX_4X32
    // either). The plane is still coded, CHUNKED: `decode_token_recon_block`
    // (`decodeframe.c:1022`) steps the plane in `get_vartx_max_txsize`
    // units, where the `BLOCK_INVALID` plane size indexes
    // `max_txsize_rect_lookup` OUT OF BOUNDS and the byte read there in the
    // pinned Release oracle build is TX_4X8 -- the committed
    // "adjacent `vtx_tab[0]` = TX_4X4" reading was wrong (reviewer FAIL on
    // e24dc84b). `decode_reconstruct_tx`'s
    // `if (tx_size == plane_tx_size || plane)` never recurses for chroma, so
    // every unit reads at TX_4X8: FOUR stacked 4x8 units per plane over the
    // 4x32 chroma plane. The units KEEP a tx type: `av1_get_tx_type`
    // (blockd.h) has no TX_4X4 early-out -- it derives from the block's
    // inherited luma type reduced into the TX_4X8 ext-tx set
    // (`reduce_inherited_chroma_tx_type`), exactly like every other rect
    // unit here, so the 4:2:2 drop-the-type special case is gone.
    let invalid_ss_plane =
        ss_x(fctx) == 1 && ss_y(fctx) == 0 && (write_chroma_w, write_chroma_h) == (4, 32);
    let (uw, uh) = if invalid_ss_plane { (4, 8) } else { (uw, uh) };
    // lane-av1chrtx: `inherit_tx_type` is resolved PER UNIT below, not once
    // for the block -- `av1_get_tx_type` (blockd.h:1287-1297) reads
    // `xd->tx_type_map` at the chroma unit's own position scaled back to
    // luma, so a unit sitting over a luma leaf that coded NOTHING takes that
    // leaf's `DCT_DCT` stamp (`decodetxb.c:199-203`), not the block-level
    // (top-left leaf) type.
    let (nx, ny) = (write_chroma_w / uw, write_chroma_h / uh);
    let multi = nx * ny > 1;
    let set = rect_inter_chroma_set(uw, uh)?;
    // lane-av1-422blockhalf: the assembled block grid is the PRED stride x
    // the plane's true height -- at 4:2:2 a square block's plane is
    // (side/2, side), so the old `chroma_side`-square sizing under-allocated
    // every row past `chroma_stride` once the caller began passing the
    // per-axis stride.
    let assembled_h = write_chroma_h.max(chroma_side);
    let mut assembled: [Vec<i32>; 2] = [Vec::new(), Vec::new()];
    // The multi-unit stamps below are rewritten by the caller's whole-block
    // record (`record_rect_mi`/`record_split_luma_rect_mi` write ONE
    // whole-block chroma state, class `override-slot-on-one-arm`). The
    // block tail's square-mu derivation cannot express a non-square strip's
    // units, so the TRUE units travel back for the tail's `mu_chroma_units`
    // replay (the c8 re-stamp, at the strip's own tiling).
    let mut units: Vec<((usize, usize), usize, Grid, usize, usize)> = Vec::new();
    if multi {
        hit!(CHROMA_SPLIT_TX_HITS);
        assembled = [
            vec![0i32; chroma_side * assembled_h],
            vec![0i32; chroma_side * assembled_h],
        ];
    }
    for plane_idx in 1..=2 {
        let src = if plane_idx == 1 { su } else { sv };
        for cu_row in 0..ny {
            for cu_col in 0..nx {
                let (span_x, span_y) = (uw << ss_x(fctx), uh << ss_y(fctx));
                let cu_mi = (
                    at_mi.0 + cu_row * (span_y / MI),
                    at_mi.1 + cu_col * (span_x / MI),
                );
                let unit_rel_mi = (cu_mi.0 - at_mi.0, cu_mi.1 - at_mi.1);
                let cu_around = if multi {
                    // lane-av1422ctxgather: at ss (1,0) the unit's above
                    // span covers `uw` chroma px = `2*uw` luma mi columns,
                    // so the plain per-mi gather counts every chroma
                    // column's whole-unit dc sign twice
                    // ([`Neighbours::around_mi_422_chroma`]; the left
                    // extent is 1:1 at ss_y 0). 4:2:0 keeps the per-mi
                    // gather (both extents duplicated there, measured
                    // elsewhere).
                    if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
                        neighbours.around_mi_422_chroma(cu_mi, span_x, span_y)
                    } else {
                        neighbours.around_mi_rect(cu_mi, span_x, span_y)
                    }
                } else {
                    around
                };
                let (cu_x, cu_y) = (cpx + cu_col * uw, cpy + cu_row * uh);
                let cu_pred = src.offset(cu_row * uh * chroma_side + cu_col * uw);
                let inherit_tx_type =
                    if let Some(t) = covering_leaf_tx_type(leaf_tx_types, unit_rel_mi) {
                        hit!(CHROMA_RECT_LEAF_TX_HITS);
                        if Some(t) != Some(luma_tx_type) {
                            hit!(CHROMA_RECT_LEAF_TX_DIFF_HITS);
                        }
                        Some(t)
                    } else {
                        // lane-av1ichromatx: the ONE remaining site in
                        // `decode_inter_block` that can hand a chroma unit a
                        // BLOCK-level type on a split plane. `covering_leaf_tx_
                        // type` found nothing, i.e. no luma leaf covers this
                        // unit's own MI cell -- which is exactly the shape a
                        // block that coded a SINGLE luma transform has (its
                        // leaf list is empty, so every lookup misses). For
                        // that block `luma_tx_type` IS the type of the one
                        // luma unit whose stamp covers the whole plane, so the
                        // fallback is correct there.
                        //
                        // What is NOT correct is a fallback on a plane with
                        // `leaves` NON-empty: then some luma leaf does exist,
                        // it simply does not cover this unit's cell, and the
                        // block-level value is a DIFFERENT leaf's type. The
                        // assertion below is what closes that shape, so the
                        // fallback can never again be a silent mistype: the
                        // class is "block-level where the unit's own cell
                        // says otherwise", and this is its last site.
                        hit!(CHROMA_RECT_BLOCK_TX_FALLBACK);
                        if multi {
                            hit!(CHROMA_RECT_BLOCK_TX_FALLBACK_MULTI);
                        }
                        debug_assert!(
                            leaf_tx_types.is_empty(),
                            "lane-av1ichromatx: a multi-unit rect chroma plane fell back to the \
                             block-level tx_type while {} luma leaves were coded -- the unit's own \
                             av1_get_tx_type cell is then some other leaf's stamp, not \
                             luma_tx_type",
                            leaf_tx_types.len()
                        );
                        Some(luma_tx_type)
                    };
                let cu_grid = read_inter_plane_rect(
                    dec,
                    cdfs,
                    set,
                    (uw, uh),
                    chroma_side,
                    plane_idx,
                    cu_around[plane_idx],
                    mode_for_tx,
                    y,
                    u,
                    v,
                    cu_x,
                    cu_y,
                    cu_pred,
                    inherit_tx_type,
                    multi.then_some(3),
                    fctx,
                )?
                .0;
                if invalid_ss_plane {
                    hit!(CHROMA422_CHUNK_HITS);
                }
                if multi {
                    // Immediately, so the next unit of this block reads this
                    // one's coefficient context (libaom updates the chroma
                    // above/left contexts per unit).
                    neighbours.record_mi_chroma(cu_mi, span_x, span_y, plane_idx, &cu_grid);
                    let dst = &mut assembled[plane_idx - 1];
                    for rr in 0..uh {
                        let start = (cu_row * uh + rr) * chroma_side + cu_col * uw;
                        dst[start..start + uw].copy_from_slice(&cu_grid[rr * uw..][..uw]);
                    }
                    units.push((cu_mi, plane_idx, cu_grid, span_x, span_y));
                } else {
                    let mut full = vec![0i32; chroma_side * assembled_h];
                    for rr in 0..uh {
                        full[rr * chroma_side..][..uw].copy_from_slice(&cu_grid[rr * uw..][..uw]);
                    }
                    assembled[plane_idx - 1] = full;
                }
            }
        }
    }
    let [ug, vg] = assembled;
    Ok((Grid::Own(ug), Grid::Own(vg), units))
}

/// [`read_inter_plane`] for a rectangular transform unit (lane-inter4 r2):
/// the coefficients are read with the rect coefficient machinery
/// ([`read_coeffs_rect`], whose `TxClass` handling covers the 1D `V_*`/`H_*`
/// types), inverted with the rect inverse transform
/// ([`dequant_and_inverse_typed_wh`], which carries the 2:1 `sqrt2` scale),
/// and added onto the top-left `w x h` of the block's own square-strided
/// motion-compensated prediction buffer. Returns the *rect* coefficient grid
/// and the coded `tx_type`.
///
/// `txb_skip_ctx` is 0 for luma: this unit covers the whole block, i.e.
/// `get_txb_ctx`'s `plane_bsize == txsize_to_bsize[tx_size]` branch.
#[allow(clippy::too_many_arguments)]
pub(super) fn read_inter_plane_rect(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    set: TxbSet,
    (w, h): (usize, usize),
    stride: usize,
    plane_idx: usize,
    around: (bool, bool, i32),
    tx_mode: usize,
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    x: usize,
    cy: usize,
    prediction: Pred<'_>,
    inherited_luma_tx_type: Option<TxType>,
    luma_skip_ctx: Option<usize>,
    fctx: &crate::decode::FrameCtx,
) -> Result<(Grid, TxType)> {
    // lane-av1mono: as [`read_plane`] -- a monochrome frame codes no chroma
    // coefficients (`plane < av1_num_planes == 1`).
    if plane_idx > 0 && mono(fctx) {
        return Ok((Grid::Zero(w * h), TxType::DctDct));
    }
    // lane-av1readcensus: the units read from here on are this plane's.
    set_census_plane(plane_idx);
    let skip_ctx = if plane_idx == 0 {
        // `get_txb_ctx_general`: 0 only when this unit IS the whole plane
        // block; a var-tx leaf passes its own neighbour-table context.
        luma_skip_ctx.unwrap_or(0)
    } else {
        // libaom `get_txb_ctx`: a chroma unit's context is
        // `combine_entropy_contexts(above, left)` plus 7, or plus 10 when the
        // plane block is bigger than the transform. Our chroma tables hold the
        // offset-7 rows first, so a multi-unit caller adds 3 through
        // `luma_skip_ctx` (same convention as [`read_plane`]).
        usize::from(around.0) + usize::from(around.1) + luma_skip_ctx.unwrap_or(0)
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
    // `av1_get_ext_tx_set_type`'s DCT-only clamp is on `tx_size_sqr_up`, i.e.
    // the *longer* side of this rect transform.
    let default_tx_type = match inherited_luma_tx_type {
        Some(t) => reduce_inherited_chroma_tx_type(t, w, h, fctx),
        None => TxType::DctDct,
    };
    // libaom `av1_get_tx_type` (`blockd.h`): a lossless block's type is
    // DCT_DCT on EVERY plane, so chroma never inherits or mode-derives one.
    let default_tx_type = if lossless(fctx) {
        TxType::DctDct
    } else {
        default_tx_type
    };
    // lane-r14 r2: a 64-length axis codes only its low 32 coefficients
    // (`av1_get_max_eob`), so the coded unit is the `min(w,32) x min(h,32)`
    // corner, zero-extended into the true `w x h` grid before the inverse
    // transform -- exactly what the intra superblock-strip path does. A
    // corner that comes out square (32x32, under a 64x32/32x64 strip) reads
    // through the SQUARE coefficient reader with `rect_shape = Some((w, h))`,
    // because `av1_nz_map_ctx_offset` is indexed by the real, un-adjusted
    // rectangular shape (lane-sbpart r8).
    let (cw, ch) = (w.min(32), h.min(32));
    if plane_idx == 0 && w.max(h) == 64 {
        hit!(RECT64_INTER_TU_HITS);
    }
    if plane_idx == 0 && (w, h) == (32, 8) || plane_idx == 0 && (w, h) == (8, 32) {
        hit_do! {
            RECT32X8_INTER_TU_HITS.with(|c| {
                let mut v = c.get();
                v[usize::from(w == 8)] += 1;
                c.set(v);
            });
        }
    }
    let (corner, tx_type) = if (cw, ch) == (32, 32) {
        let scan32 = default_scan(TX32);
        read_coeffs(
            dec,
            &mut coding,
            scan32,
            skip_ctx,
            dc_sign_ctx(around.2),
            default_tx_type,
            Some((w, h)),
        )?
    } else {
        read_coeffs_rect(
            dec,
            &mut coding,
            rect_scan(cw, ch)?,
            cw,
            ch,
            skip_ctx,
            dc_sign_ctx(around.2),
            default_tx_type,
            plane_idx,
        )?
    };
    let grid = extend_corner(corner, cw, ch, w, h);
    let (dc_delta, ac_delta) = plane_q_delta(plane_idx, fctx);
    if crate::envflags::env_flag!("EC_TRACE_TXTYPE") {
        eprintln!(
            "OUR_TXTYPE plane={plane_idx} x={x} y={cy} w={w} h={h} tx_type={tx_type:?} inh={inherited_luma_tx_type:?}"
        );
    }
    // `reconstruct_mc_rect` reads both buffers at the square prediction
    // stride, so the dense `w x h` residual is re-laid at that stride
    // (`TxParams::stride`) after the inverse transform.
    let tx = TxParams {
        plane: plane_idx,
        w,
        h,
        bit_depth: bit_depth(fctx),
        q_idx: block_q_idx(fctx),
        dc_delta,
        ac_delta,
        tx_type,
        qm: block_iqmatrix(fctx, plane_idx, w, h, tx_type),
        stride,
        lossless: lossless(fctx),
    };
    push_mc_rect_tx(
        y, u, v, plane_idx, x, cy, stride, w, h, prediction, tx, &grid, fctx,
    );
    Ok((grid, tx_type))
}

/// The coded `cw x ch` corner of a transform zero-extended into its true
/// `w x h` grid (a 64-length axis codes only its low 32 coefficients). A
/// corner that coded nothing stays the shared zero grid -- no allocation
/// (lane-coefgrid).
pub(super) fn extend_corner(corner: Grid, cw: usize, ch: usize, w: usize, h: usize) -> Grid {
    if (cw, ch) == (w, h) {
        return corner;
    }
    if let Grid::Zero(_) = corner {
        return Grid::Zero(w * h);
    }
    let mut full = vec![0i32; w * h];
    for row in 0..ch {
        full[row * w..][..cw].copy_from_slice(&corner[row * cw..][..cw]);
    }
    Grid::Own(full)
}

/// [`embed_rect_grid`] that keeps the shared zero marker zero: an all-zero
/// transform unit's grid IS `Grid::Zero` (whose deref is the shared
/// `ZERO_RESIDUAL`), so re-laying it would read a zero-length slice.
pub(super) fn embed_grid_stride(grid: &Grid, w: usize, h: usize, stride: usize) -> Grid {
    if is_zero_grid(grid) {
        return Grid::Zero(stride * stride);
    }
    Grid::Own(embed_rect_grid(grid, w, h, stride))
}

/// A rect coefficient grid re-laid into the square `stride x stride` grid the
/// neighbour/deblock stamps downstream of the residual expect.
pub(super) fn embed_rect_grid(grid: &[i32], w: usize, h: usize, stride: usize) -> Vec<i32> {
    let mut full = vec![0i32; stride * stride];
    for row in 0..h {
        full[row * stride..][..w].copy_from_slice(&grid[row * w..][..w]);
    }
    full
}

/// One 8x8 inter leaf's luma residual (lane-txselect): either the whole-8x8
/// transform, or -- when [`read_block_tx_size`]'s var-tx tree took the single
/// split a `BLOCK_8X8` can take (`split`) -- the four 4x4 transform units in
/// raster order, each reading its own coefficient context off the units
/// already decoded before it. Returns the block's luma grid (all-zero in the
/// split case, whose reconstruction has already happened unit by unit) and
/// the `tx_type` chroma inherits: `av1_get_tx_type`'s colocated luma lookup,
/// which for a split block lands on the top-left unit.
#[allow(clippy::too_many_arguments)]
pub(super) fn read_inter_luma8(
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    leaf_mi: (usize, usize),
    px: usize,
    py: usize,
    pred_y: Pred<'_>,
    mode_for_tx: usize,
    base_q_idx: u8,
    scan8: &[u16],
    scan4: &[u16],
    reduced_tx_set: bool,
    split: bool,
    fctx: &crate::decode::FrameCtx,
) -> Result<(Grid, TxType)> {
    // lane-lossless: an 8x8 inter block is FOUR 4x4 units (libaom
    // `read_block_tx_size`'s lossless TX_4X4), tree or no tree.
    let split = split || lossless(fctx);
    if !split {
        return read_inter_plane(
            dec,
            cdfs,
            if reduced_tx_set {
                TxbSet::Luma8Inter
            } else {
                TxbSet::Luma8InterSet1
            },
            scan8,
            0,
            neighbours.around_mi(leaf_mi, 8)[0],
            mode_for_tx,
            y,
            u,
            v,
            px,
            py,
            8,
            base_q_idx,
            pred_y,
            None,
            None,
            fctx,
        );
    }
    let mut first = TxType::DctDct;
    for tu_row in 0..2 {
        for tu_col in 0..2 {
            let tu_mi = (leaf_mi.0 + tu_row, leaf_mi.1 + tu_col);
            let tu_pred = pred_y.view(8).offset(tu_row * 4 * 8 + tu_col * 4);
            let tu_skip_ctx = neighbours.luma_skip_ctx(tu_mi, 1);
            let (tu_grid, tu_tx_type) = read_inter_plane(
                dec,
                cdfs,
                if reduced_tx_set {
                    TxbSet::Luma4Inter
                } else {
                    TxbSet::Luma4InterSet1
                },
                scan4,
                0,
                neighbours.around_mi(tu_mi, 4)[0],
                mode_for_tx,
                y,
                u,
                v,
                px + tu_col * 4,
                py + tu_row * 4,
                4,
                base_q_idx,
                tu_pred,
                None,
                Some(tu_skip_ctx),
                fctx,
            )?;
            if tu_row == 0 && tu_col == 0 {
                first = tu_tx_type;
            }
            neighbours.record_mi_luma(tu_mi, 4, &tu_grid);
        }
    }
    Ok((Grid::Zero(64), first))
}

/// The [`TxbSet`] one leaf of an inter block's var-tx tree reads its
/// coefficient tables from -- the inter counterpart of [`txbset_for`].
/// `av1_get_tx_type`'s chroma tail (libaom `av1/common/blockd.h:1291-1305`):
/// an inter block's chroma transform inherits the *colocated luma* unit's
/// coded `tx_type`, then drops back to `DCT_DCT` unless that type is a member
/// of the extended set the CHROMA transform size selects
/// (`av1_ext_tx_used[av1_get_ext_tx_set_type(uv_tx, 1, reduced)][tx_type]`,
/// `blockd.h:1036`/`1097`). lane-golomb r7: we used to force `DCT_DCT` at
/// every chroma side >= 32, but `av1_get_ext_tx_set_type`'s DCT-only rung is
/// `tx_size_sqr_up > TX_32X32`; at exactly TX_32X32 an inter block reads
/// `EXT_TX_SET_DCT_IDTX`, so a 64x64 inter block whose first luma leaf coded
/// `IDTX` inherits IDTX on its 32x32 chroma units. `IDTX` and `DCT_DCT` are
/// both `TX_CLASS_2D`, so that mistake left the entropy stream perfectly in
/// sync and only corrupted the inverse transform (one 64x64 superblock of
/// high-frequency chroma noise, luma bit-exact).
pub(crate) fn reduce_inherited_chroma_tx_type(
    t: TxType,
    w: usize,
    h: usize,
    fctx: &crate::decode::FrameCtx,
) -> TxType {
    reduce_inherited_chroma_tx_type_flagged(
        t,
        w,
        h,
        fctx.reduced_tx_set_inter.with(std::cell::Cell::get),
    )
}

/// [`reduce_inherited_chroma_tx_type`] against an explicit `reduced_tx_set`
/// bit (lane-intertx): the WRITER calls this with the frame's own header bit
/// ([`crate::cdf_state::Cdfs::reduced_tx_set`]) to derive the type an inter
/// block's chroma transform is coded with, the same derivation its reader
/// makes -- it has no `FrameCtx` in reach and must not silently read one
/// whose value can lag the frame being written (class `stale-context-bit`).
pub(crate) fn reduce_inherited_chroma_tx_type_flagged(
    t: TxType,
    w: usize,
    h: usize,
    reduced: bool,
) -> TxType {
    use TxType::*;
    let sqr_up = w.max(h);
    let sqr = w.min(h);
    // `av1_get_ext_tx_set_type(tx_size, is_inter = 1, reduced)`.
    let allowed: &[TxType] = if sqr_up > 32 {
        // EXT_TX_SET_DCTONLY
        &[DctDct]
    } else if sqr_up == 32 || reduced {
        // EXT_TX_SET_DCT_IDTX
        &[DctDct, Idtx]
    } else if sqr == 16 {
        // EXT_TX_SET_DTT9_IDTX_1DDCT -- TX_TYPES 0..=11, i.e. everything but
        // the four 1D ADST/FLIPADST types.
        &[
            DctDct,
            AdstDct,
            DctAdst,
            AdstAdst,
            FlipAdstDct,
            DctFlipAdst,
            FlipAdstFlipAdst,
            AdstFlipAdst,
            FlipAdstAdst,
            Idtx,
            VDct,
            HDct,
        ]
    } else {
        // EXT_TX_SET_ALL16
        return t;
    };
    if allowed.contains(&t) { t } else { DctDct }
}

/// The [`TxbSet`] a resolved luma transform side reads its coefficient tables
/// from for a block libaom's `is_inter_block` calls inter -- which an intrabc
/// block is (`blockd.h:373`), so its `tx_type` comes off the inter `ext_tx`
/// sets, never the mode-indexed intra ones ([`txbset_for`]).
///
/// 64 is only ever the 64x64-block, depth-0, corner-scanned case
/// ([`TxbSet::Luma64`]); every other resolved side maps to its own matching
/// set. `get_tx_set` (spec 5.11.48) only splits on `reduced_tx_set` at
/// `TX_8X8`/`TX_4X4`: an intra `TX_16X16` always reads `TX_SET_INTRA_2`
/// regardless of the flag (`av1_get_ext_tx_set_type`'s
/// `tx_size_sqr == TX_16X16` branch ignores `use_reduced_set` once it is past
/// the `TX_32X32`/reduced-set checks), so only the 8x8 and 4x4 cases have a
/// second table ([`TxbSet::Luma8Inter`]/[`TxbSet::Luma4Inter`]).
///
/// lane-av1txr: the intrabc leaf sites used to call a near-duplicate
/// `txbset_for_inter` whose `4` fell through to `Luma64` -- 32x32-class tables
/// (a 1024-entry grid) handed to a 4x4 dequant, which panicked at
/// `transform.rs`'s `inverse_transform_2d_typed_wh` the first time an intrabc
/// 4x4 TU coded a non-zero coefficient (`allintra0`/cpu-used=0). The
/// duplicate is gone; every inter-table site goes through this one mapping.
pub(super) fn inter_txbset_for(tx_px: usize, reduced_tx_set: bool) -> TxbSet {
    match tx_px {
        // lane-sb128c r1: a 128x128 inter block's own luma unit
        // (`max_txsize_rect_lookup[BLOCK_128X128] == TX_64X64`).
        // `av1_get_ext_tx_set_type` is `DCT_ONLY` at `tx_size_sqr_up ==
        // TX_64X64`, so no `tx_type` symbol is coded and the intra/inter sets
        // coincide -- exactly the set the whole-64x64 inter block already uses.
        64 => TxbSet::Luma64,
        32 => TxbSet::Luma32Inter,
        16 if reduced_tx_set => TxbSet::Luma16Inter,
        16 => TxbSet::Luma16InterSet1,
        8 if reduced_tx_set => TxbSet::Luma8Inter,
        8 => TxbSet::Luma8InterSet1,
        4 if reduced_tx_set => TxbSet::Luma4Inter,
        4 => TxbSet::Luma4InterSet1,
        _ => unreachable!("a var-tx leaf is never wider than a 64x64 block's 32x32 sub-transform"),
    }
}

/// The luma coefficient scan table for a resolved transform side -- 32/16/8/4,
/// the only four sizes a `TxMode::Select` block's coded coefficient grid
/// ([`read_tx_size`]'s resolved side, or 32 at the 64x64 corner case) can be.
pub(super) fn scan_for<'a>(
    tx_px: usize,
    scan32: &'a [u16],
    scan16: &'a [u16],
    scan8: &'a [u16],
    scan4: &'a [u16],
) -> &'a [u16] {
    match tx_px {
        32 => scan32,
        16 => scan16,
        8 => scan8,
        4 => scan4,
        _ => unreachable!("read_tx_size never resolves the coefficient grid to anything else"),
    }
}

pub(super) fn txbset_for(tx_px: usize, reduced_tx_set: bool) -> TxbSet {
    match tx_px {
        64 => TxbSet::Luma64,
        32 => TxbSet::Luma32,
        16 => TxbSet::Luma16,
        8 if reduced_tx_set => TxbSet::Luma8,
        8 => TxbSet::Luma8Set1,
        4 if reduced_tx_set => TxbSet::Luma4,
        4 => TxbSet::Luma4Set1,
        _ => unreachable!("read_tx_size never resolves a block's own side to anything else"),
    }
}

/// `tx_size_wide_unit_log2`/`tx_size_high_unit_log2`: `log2(px / 4)`, the
/// index [`tx_dim_to_filter_length`] and the chroma 4-vs-6 choice key on.
fn unit_log2(px: u32) -> i32 {
    if px == 0 {
        0
    } else {
        px.trailing_zeros() as i32 - 2
    }
}

/// spec 7.14.4's `get_filter_level`: `ref_deltas` is indexed by the block's
/// reference frame, `mode_deltas` by `mode_lf_lut[mode]` -- `0` for
/// `GLOBALMV`, `1` for `NEARESTMV`/`NEARMV`/`NEWMV` (intra gets no mode
/// delta). `ref_frame` here is [`Neighbours::ref_grid`]'s packed cell:
/// magnitude = reference (`0` = intra), sign = `GLOBALMV` (lane-av1golden2:
/// the flat `mode_deltas[1]` this used before was a `LAST_FRAME`-reachable
/// wrong delta on every `GLOBALMV` block, masked while any `GOLDEN_FRAME`
/// block anywhere aborted the whole stream's decode before comparison).
pub(super) fn lf_level(
    lf: &LoopFilterParams,
    plane_idx: usize,
    dir: usize,
    ref_frame: i8,
    delta_lf: i32,
    segment_id: u8,
    fctx: &crate::decode::FrameCtx,
) -> i32 {
    let base = i32::from(if plane_idx == 0 {
        lf.level[dir]
    } else if plane_idx == 1 {
        lf.level[2]
    } else {
        lf.level[3]
    });
    // spec 7.14.4: the per-superblock `DeltaLF` term applies before the
    // ref/mode delta scaling below, and regardless of `delta_enabled`
    // (that flag only gates `ref_deltas`/`mode_deltas`).
    let base = (base + delta_lf).clamp(0, 63);
    // lane-seg, spec 7.14.4 / libaom `av1_loop_filter_frame_init`: this
    // block's segment's own `SEG_LVL_ALT_LF_*` delta applies after the
    // per-superblock `DeltaLF` term and before the ref/mode deltas below.
    // Feature index: `SEG_LVL_ALT_LF_Y_V + dir` for luma, `_U`/`_V` (3/4)
    // for chroma.
    let feature = if plane_idx == 0 {
        SEG_LVL_ALT_LF_Y_V + dir
    } else {
        plane_idx + 2
    };
    // lane-perf9: `segment_id` is `0` and this lookup is skipped entirely
    // when the frame codes no segmentation (`seg_lf_active`), which is what
    // spares the deblock loop two thread-local reads per 4x4 edge.
    let base = match seg_feature(usize::from(segment_id), feature, fctx) {
        Some(data) => (base + data).clamp(0, 63),
        None => base,
    };
    if !lf.delta_enabled {
        return base;
    }
    let scale = 1i32 << (base >> 5);
    let is_inter = ref_frame != 0;
    let is_globalmv = ref_frame < 0;
    let ref_idx = ref_frame.unsigned_abs() as usize;
    let mut lvl = base + i32::from(lf.ref_deltas[ref_idx]) * scale;
    if is_inter {
        lvl += i32::from(lf.mode_deltas[usize::from(!is_globalmv)]) * scale;
    }
    lvl.clamp(0, 63)
}

thread_local! {
    /// [`EdgeCtx`]'s level LUT, memoised per thread. The table
    /// is a pure function of the frame's [`LoopFilterParams`], the plane, the
    /// pass and segment 0's own `SEG_LVL_ALT_LF_*` datum (the only thing
    /// [`lf_level`] reads out of the `FrameCtx` at `segment_id = 0`), so one
    /// build per plane and pass serves the whole frame.
    static LVL_CACHE: std::cell::RefCell<
        Vec<((usize, usize, LoopFilterParams, Option<i32>), [[u8; 15]; 64])>,
    > = const { std::cell::RefCell::new(Vec::new()) };
}

/// `lf_level` over the whole `(base level, ref frame)` domain -- see
/// [`LVL_CACHE`]. `lf_level`'s own first step is
/// `(base0 + delta_lf).clamp(0, 63)`, so passing `b - base0` pins its base at
/// `b` and the rest of the function is a pure function of `(b, ref_frame)`.
fn lvl_table(
    lf: &LoopFilterParams,
    plane_idx: usize,
    dir: usize,
    base0: i32,
    fctx: &crate::decode::FrameCtx,
) -> [[u8; 15]; 64] {
    let feature = if plane_idx == 0 {
        SEG_LVL_ALT_LF_Y_V + dir
    } else {
        plane_idx + 2
    };
    let key = (plane_idx, dir, *lf, seg_feature(0, feature, fctx));
    if let Some(t) = LVL_CACHE.with(|c| c.borrow().iter().find(|(k, _)| *k == key).map(|(_, t)| *t))
    {
        return t;
    }
    let mut lvl = [[0u8; 15]; 64];
    for (b, row) in lvl.iter_mut().enumerate() {
        for (r, cell) in row.iter_mut().enumerate() {
            *cell = lf_level(
                lf,
                plane_idx,
                dir,
                (r as i32 - 7) as i8,
                b as i32 - base0,
                0,
                fctx,
            ) as u8;
        }
    }
    LVL_CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= 8 {
            c.remove(0);
        }
        c.push((key, lvl));
    });
    lvl
}

/// Spec 7.14.2's `set_lpf_parameters`, restricted to the uniform-level,
/// no-segmentation, no-delta-LF path and to this decoder's own invariant
/// that a coded block's transform is always its own full size -- so a TU
/// edge is always also a PU edge, and the "both sides skipped, not a PU
/// edge" suppression (which only ever fires at a non-PU-edge TU boundary)
/// never applies here, letting this skip the `skip`/`skip_txfm` lookup
/// entirely. Returns `None` when this 4-pixel edge group is not filtered.
/// One plane pass's worth of [`edge_params`] constants (lane-deblock2): the
/// seven values that used to be re-passed per 4-pixel edge group -- half of
/// them on the stack, into an `inline(never)` body -- plus libaom's
/// `lfi->lfthr` idea in level form: `av1_loop_filter_frame_init` builds the
/// filter level for every `(base level, ref frame)` pair once per frame
/// instead of running `av1_get_filter_level`'s delta arithmetic per edge.
pub(super) struct EdgeCtx<'a> {
    pub(super) lf: &'a LoopFilterParams,
    pub(super) n: &'a Neighbours,
    pub(super) fctx: &'a crate::decode::FrameCtx,
    pub(super) plane_idx: usize,
    pub(super) dir: usize,
    pub(super) chroma: bool,
    pub(super) seg_lf_active: bool,
    /// `delta_lf_grid`'s column for this plane and direction.
    pub(super) delta_idx: usize,
    /// `mode_step` (`set_lpf_parameters`): one mi for luma, `1 << ss` for
    /// chroma.
    pub(super) step: usize,
    /// This pass's own `lf.level[..]`, the LUT's base.
    pub(super) base0: i32,
    /// `lf_level` at `[base level][ref frame + 7]`, with no segment
    /// override -- the whole domain, since a level is `0..=63` and a ref
    /// frame `-7..=7` (`ref_deltas` holds `TOTAL_REFS_PER_FRAME` = 8
    /// entries and is indexed by the ref frame's magnitude).
    pub(super) lvl: [[u8; 15]; 64],
}

impl<'a> EdgeCtx<'a> {
    pub(super) fn new(
        lf: &'a LoopFilterParams,
        n: &'a Neighbours,
        plane_idx: usize,
        dir: usize,
        chroma: bool,
        seg_lf_active: bool,
        fctx: &'a crate::decode::FrameCtx,
    ) -> Self {
        let base0 = i32::from(if plane_idx == 0 {
            lf.level[dir]
        } else if plane_idx == 1 {
            lf.level[2]
        } else {
            lf.level[3]
        });
        let lvl = lvl_table(lf, plane_idx, dir, base0, fctx);
        Self {
            lf,
            n,
            fctx,
            plane_idx,
            dir,
            chroma,
            seg_lf_active,
            delta_idx: if plane_idx == 0 { dir } else { plane_idx + 1 },
            step: if chroma {
                1 << (if dir == 0 { ss_x(fctx) } else { ss_y(fctx) })
            } else {
                1
            },
            base0,
            lvl,
        }
    }

    /// Spec 7.14.4's `get_filter_level`, off the table when no segment can
    /// override it (which is every frame that codes no segmentation).
    #[inline(always)]
    pub(super) fn level(&self, ref_frame: i8, delta_lf: i32, seg: u8) -> i32 {
        if self.seg_lf_active {
            return lf_level(
                self.lf,
                self.plane_idx,
                self.dir,
                ref_frame,
                delta_lf,
                seg,
                self.fctx,
            );
        }
        let b = (self.base0 + delta_lf).clamp(0, 63) as usize;
        debug_assert!((-7..=7).contains(&ref_frame));
        let r = (i32::from(ref_frame) + 7).clamp(0, 14) as usize;
        i32::from(self.lvl[b][r])
    }
}

/// Spec 7.14.2's `set_lpf_parameters`, restricted to the uniform-level,
/// no-segmentation, no-delta-LF path and to this decoder's own invariant
/// that a coded block's transform is always its own full size -- so a TU
/// edge is always also a PU edge, and the "both sides skipped, not a PU
/// edge" suppression (which only ever fires at a non-PU-edge TU boundary)
/// never applies here, letting this skip the `skip`/`skip_txfm` lookup
/// entirely. Returns `None` when this 4-pixel edge group is not filtered.
#[inline(always)]
pub(super) fn edge_params(c: &EdgeCtx, x0: usize, y0: usize) -> Option<(u8, i32)> {
    let (mi_r, mi_c) = (
        plane_to_mi(c.chroma, y0, if c.chroma { ss_y(c.fctx) } else { 0 }),
        plane_to_mi(c.chroma, x0, if c.chroma { ss_x(c.fctx) } else { 0 }),
    );
    let cur_tx = if c.dir == 0 {
        tx_px_at(c.n, c.chroma, mi_r, mi_c)
    } else {
        tx_h_px_at(c.n, c.chroma, mi_r, mi_c)
    };
    // lane-deblock: `cur_tx` is a power-of-two pixel size, so the alignment
    // test is a mask, not the hardware `divl` this used to emit (10.7% of
    // `edge_params`' own samples sat on that divide's result test).
    //
    // Most 4-pixel edge groups die right here (a 4-aligned coordinate that
    // the covering transform does not start at), so this head is inlined
    // into the deblock loops and only survivors pay the call and the
    // register-spilling prologue of the body below.
    if cur_tx == 0 || (if c.dir == 0 { x0 } else { y0 }) & (cur_tx as usize - 1) != 0 {
        return None;
    }
    edge_params_body(c, x0, y0, mi_r, mi_c, cur_tx)
}

/// [`edge_params`] past its transform-alignment gate.
#[inline(never)]
pub(super) fn edge_params_body(
    c: &EdgeCtx,
    x0: usize,
    y0: usize,
    mi_r: usize,
    mi_c: usize,
    cur_tx: u32,
) -> Option<(u8, i32)> {
    let n = c.n;
    let cur_ref = n.ref_grid[mi_r * n.skip_grid_cols_mi + mi_c];
    // The previous cell must keep the odd parity `plane_to_mi` just
    // established, i.e. the bottom/right mi of the PREVIOUS 8x8 luma group,
    // not the other half of this one.
    let (pv_mi_r, pv_mi_c) = if c.dir == 0 {
        (mi_r, mi_c - c.step)
    } else {
        (mi_r - c.step, mi_c)
    };
    let pv_tx = if c.dir == 0 {
        tx_px_at(n, c.chroma, pv_mi_r, pv_mi_c)
    } else {
        tx_h_px_at(n, c.chroma, pv_mi_r, pv_mi_c)
    };
    let pv_ref = n.ref_grid[pv_mi_r * n.skip_grid_cols_mi + pv_mi_c];
    let cur_delta_lf = i32::from(n.delta_lf_grid[mi_r * n.skip_grid_cols_mi + mi_c][c.delta_idx]);
    let pv_delta_lf =
        i32::from(n.delta_lf_grid[pv_mi_r * n.skip_grid_cols_mi + pv_mi_c][c.delta_idx]);

    // spec 7.14.2 / libaom `set_lpf_parameters`: a transform edge between two
    // SKIPPED INTER blocks is filtered only when it is also a prediction
    // (coded-block) edge -- `!pv_skip || !curr_skipped || pu_edge`. The
    // omission of this term (on the stale invariant "a coded block's transform
    // is always its own full size", which var-tx broke) had us filtering an
    // interior 8-px lattice the reference never touches: on the pinned 192x68
    // inter stream frame 1 went from 7244 wrong post-deblock luma px to 0.
    // libaom prices `pu_edge` in the PLANE's own pixels against the plane's
    // block size (`!(coord & (block_size_wide[get_plane_block_size(bsize)] -
    // 1))`), which for chroma bottoms out at 4 px: every 4-aligned chroma
    // edge of a sub-8x8 group is a prediction edge, even though the group's
    // bottom/right mi does not start there. Comparing mi origins instead is
    // the same test only while the block's own origin is aligned to its
    // size, which a 4x4 luma block read at the `| 1` mi is not.
    let cur_cell = n.blk_grid[mi_r * n.skip_grid_cols_mi + mi_c];
    let (cur_org, cur_dim) = (cur_cell.org, cur_cell.dim);
    let (org_px, dim_mi) = if c.dir == 0 {
        (usize::from(cur_org[1]), usize::from(cur_dim[0]))
    } else {
        (usize::from(cur_org[0]), usize::from(cur_dim[1]))
    };
    let pu_edge = if c.chroma {
        let ss = if c.dir == 0 {
            ss_x(c.fctx)
        } else {
            ss_y(c.fctx)
        };
        let plane_dim = ((dim_mi * MI) >> ss).max(4);
        (if c.dir == 0 { x0 } else { y0 }) % plane_dim == 0
    } else {
        plane_to_mi(false, if c.dir == 0 { x0 } else { y0 }, 0) == org_px
    };
    if !pu_edge
        && cur_ref != 0
        && pv_ref != 0
        && n.skip_at(mi_r, mi_c)
        && n.skip_at(pv_mi_r, pv_mi_c)
    {
        return None;
    }
    let (cur_seg, pv_seg) = if c.seg_lf_active {
        (
            segment_id_at(mi_r, mi_c, c.fctx),
            segment_id_at(pv_mi_r, pv_mi_c, c.fctx),
        )
    } else {
        (0, 0)
    };
    let cur_level = c.level(cur_ref, cur_delta_lf, cur_seg);
    let pv_level = c.level(pv_ref, pv_delta_lf, pv_seg);
    if cur_level == 0 && pv_level == 0 {
        return None;
    }
    let dim = unit_log2(cur_tx).min(unit_log2(pv_tx)).clamp(0, 4) as usize;
    let len: u8 = if c.chroma {
        if dim == 0 { 4 } else { 6 }
    } else {
        [4, 8, 14, 14, 14][dim]
    };
    let level = if cur_level != 0 { cur_level } else { pv_level };
    hit!(DEBLOCK_HITS);
    Some((len, level))
}
